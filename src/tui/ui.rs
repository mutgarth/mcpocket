use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Tabs};

use crate::tui::app::{App, Tab};
use crate::tui::theme::Theme;
use crate::upstream::UpstreamStatus;

/// Top-level render: title/tab bar, body for the active tab, footer hints.
pub fn render(frame: &mut Frame, app: &App, theme: &Theme) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_tabs(frame, chunks[0], app, theme);
    match app.tab {
        Tab::Servers => render_servers(frame, chunks[1], app, theme),
        Tab::Tools => render_tools(frame, chunks[1], app, theme),
        Tab::Live => render_live(frame, chunks[1], app, theme),
        Tab::Doctor => render_doctor(frame, chunks[1], app, theme),
    }
    render_footer(frame, chunks[2], app, theme);
}

fn render_tabs(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let titles: Vec<Line> = Tab::ALL.iter().map(|t| Line::from(t.title())).collect();
    let selected = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .block(
            Block::default().borders(Borders::ALL).title(Span::styled(
                " mcpocket ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            )),
        )
        .select(selected)
        .style(Style::default().fg(theme.dim))
        .highlight_style(
            Style::default()
                .fg(theme.fg)
                .bg(theme.selection)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(tabs, area);
}

fn render_servers(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let header = Row::new(["STATE", "NAME", "TYPE", "TOOLS", "LATENCY"]).style(
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    );

    let rows = app.servers.iter().enumerate().map(|(i, row)| {
        let (state, color) = match row.status {
            UpstreamStatus::Reachable => ("OK", theme.ok),
            UpstreamStatus::AuthMissing => ("AUTH", theme.warn),
            UpstreamStatus::Unreachable => ("FAIL", theme.fail),
        };
        let tools = match (row.exposed_tools, row.upstream_tools) {
            (Some(e), Some(t)) => format!("{e}/{t}"),
            _ => "-".to_owned(),
        };
        let style = if i == app.selected {
            Style::default().bg(theme.selection).fg(theme.fg)
        } else {
            Style::default().fg(theme.fg)
        };
        Row::new(vec![
            Cell::from(state).style(Style::default().fg(color)),
            Cell::from(row.name.clone()),
            Cell::from(row.transport),
            Cell::from(tools),
            Cell::from(format!("{}ms", row.duration_ms)),
        ])
        .style(style)
    });

    let widths = [
        Constraint::Length(6),
        Constraint::Min(20),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(10),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" Servers "));
    let selected = if app.servers.is_empty() {
        None
    } else {
        Some(app.selected)
    };
    let mut state = TableState::new().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_live(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if app.live_events.is_empty() {
        let hint = Paragraph::new("Waiting for gateway traffic… (no active gateways yet)")
            .style(Style::default().fg(theme.dim))
            .block(Block::default().borders(Borders::ALL).title(" Live "));
        frame.render_widget(hint, area);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1)])
        .split(area);

    // Headline metrics computed from the retained feed.
    let p95 = app
        .p95_latency()
        .map(|v| format!("{v}ms"))
        .unwrap_or_else(|| "-".to_owned());
    let rps = app.req_per_sec(crate::telemetry::now_ms(), 10_000);
    let errors = app.error_count();
    let error_color = if errors > 0 { theme.fail } else { theme.dim };
    let stats = Line::from(vec![
        Span::styled("p95 ", Style::default().fg(theme.dim)),
        Span::styled(format!("{p95}   "), Style::default().fg(theme.fg)),
        Span::styled("rate ", Style::default().fg(theme.dim)),
        Span::styled(format!("{rps:.1} req/s   "), Style::default().fg(theme.fg)),
        Span::styled("errors ", Style::default().fg(theme.dim)),
        Span::styled(format!("{errors}"), Style::default().fg(error_color)),
    ]);
    frame.render_widget(
        Paragraph::new(stats).block(Block::default().borders(Borders::ALL).title(" Live ")),
        chunks[0],
    );

    let lines: Vec<Line> = app
        .live_events
        .iter()
        .rev()
        .take(chunks[1].height.saturating_sub(2) as usize)
        .map(|e| {
            let (label, color) = match e.status {
                crate::telemetry::CallStatus::Ok => ("ok ", theme.ok),
                crate::telemetry::CallStatus::Error => ("ERR", theme.fail),
            };
            Line::from(vec![
                Span::styled(format!("{label} "), Style::default().fg(color)),
                Span::styled(format!("{:<24} ", e.tool), Style::default().fg(theme.fg)),
                Span::styled(
                    format!("{}ms ", e.duration_ms),
                    Style::default().fg(theme.dim),
                ),
                Span::styled(format!("[{}]", e.client), Style::default().fg(theme.accent)),
            ])
        })
        .collect();

    let para =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Traffic "));
    frame.render_widget(para, chunks[1]);
}

fn render_tools(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    use crate::policy::PolicyDecision;

    let mut lines: Vec<Line> = Vec::new();
    let mut selected_line = None;
    let mut selectable_idx = 0usize;
    for server in &app.tools {
        let expanded = app.is_tools_expanded(&server.name);
        let selected = selectable_idx == app.selected;
        if selected {
            selected_line = Some(lines.len());
        }
        let header_style =
            selectable_style(theme, theme.accent, selected).add_modifier(Modifier::BOLD);
        let meta_style = selectable_style(theme, theme.dim, selected);
        let marker = if expanded { "[-]" } else { "[+]" };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} MCP {}", server.name), header_style),
            Span::styled(
                format!(" ({})  {} tools", server.transport, server.tools.len()),
                meta_style,
            ),
        ]));
        selectable_idx += 1;
        if !expanded {
            continue;
        }
        if let Some(err) = &server.error {
            lines.push(Line::from(Span::styled(
                format!("  FAIL {}", err.lines().next().unwrap_or(err)),
                Style::default().fg(theme.fail),
            )));
            continue;
        }
        for tool in &server.tools {
            let (label, color) = match tool.decision {
                PolicyDecision::Allow => ("ALLOW", theme.ok),
                PolicyDecision::Deny => ("HIDE ", theme.warn),
            };
            let selected = selectable_idx == app.selected;
            if selected {
                selected_line = Some(lines.len());
            }
            let label_style = selectable_style(theme, color, selected);
            let name_style = selectable_style(theme, theme.fg, selected);
            let gap_style = selectable_style(theme, theme.fg, selected);
            let reason_style = selectable_style(theme, theme.dim, selected);
            lines.push(Line::from(vec![
                Span::styled(format!("  {label} "), label_style),
                Span::styled(tool.exposed_name.clone(), name_style),
                Span::styled("  ", gap_style),
                Span::styled(tool.reason.label().to_owned(), reason_style),
            ]));
            selectable_idx += 1;
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No tools loaded — press [r] to refresh.",
            Style::default().fg(theme.dim),
        )));
    }
    let visible_lines = area.height.saturating_sub(2) as usize;
    let scroll = selected_line
        .map(|line| selection_scroll_offset(line, visible_lines))
        .unwrap_or(0) as u16;
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll, 0))
            .block(Block::default().borders(Borders::ALL).title(" Tools ")),
        area,
    );
}

fn selection_scroll_offset(selected: usize, visible_len: usize) -> usize {
    if visible_len == 0 {
        0
    } else {
        selected.saturating_add(1).saturating_sub(visible_len)
    }
}

fn selectable_style(theme: &Theme, fg: ratatui::style::Color, selected: bool) -> Style {
    let style = Style::default().fg(fg);
    if selected {
        style.bg(theme.selection)
    } else {
        style
    }
}

fn render_doctor(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    use crate::doctor::CheckStatus;

    let lines: Vec<Line> = app
        .doctor
        .iter()
        .map(|check| {
            let color = match check.status {
                CheckStatus::Ok => theme.ok,
                CheckStatus::Warn => theme.warn,
                CheckStatus::Fail => theme.fail,
            };
            Line::from(vec![
                Span::styled(
                    format!("{:<5} ", check.status.label()),
                    Style::default().fg(color),
                ),
                Span::styled(
                    format!("{:<22} ", check.name),
                    Style::default().fg(theme.fg),
                ),
                Span::styled(check.detail.clone(), Style::default().fg(theme.dim)),
            ])
        })
        .collect();
    let body = if lines.is_empty() {
        Paragraph::new("Running checks… press [r] to refresh.")
            .style(Style::default().fg(theme.dim))
    } else {
        Paragraph::new(lines)
    };
    frame.render_widget(
        body.block(Block::default().borders(Borders::ALL).title(" Doctor ")),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let hint = match app.tab {
        Tab::Servers => "[Tab] switch  [j/k] move  [e]nable [d]isable  [r]efresh  [q]uit",
        Tab::Tools => "[Tab] switch  [j/k] move  [Enter] expand  [a]llow [x]deny  [q]uit",
        Tab::Live => "[Tab] switch  live traffic  [q]uit",
        Tab::Doctor => "[Tab] switch  [r]efresh  [q]uit",
    };
    let text = app
        .status_message
        .clone()
        .unwrap_or_else(|| hint.to_owned());
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(theme.dim)),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, Tab};
    use crate::tui::theme::Theme;
    use crate::upstream::{StatusRow, UpstreamStatus};
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

    fn render_buffer_with_size(app: &mut App, width: u16, height: u16) -> Buffer {
        let theme = Theme::brand(false);
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, app, &theme)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn render_buffer(app: &mut App) -> Buffer {
        render_buffer_with_size(app, 80, 20)
    }

    fn buffer_text(app: &mut App) -> String {
        let buf = render_buffer(app);
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    fn find_text_position(buffer: &Buffer, needle: &str) -> Option<(u16, u16)> {
        for y in 0..buffer.area.height {
            let row = (0..buffer.area.width)
                .map(|x| buffer.cell((x, y)).unwrap().symbol())
                .collect::<String>();
            if let Some(byte_idx) = row.find(needle) {
                let x = row[..byte_idx].chars().count() as u16;
                return Some((x, y));
            }
        }
        None
    }

    fn status_row(name: String) -> StatusRow {
        StatusRow {
            name,
            transport: "http",
            status: UpstreamStatus::Reachable,
            duration_ms: 10,
            exposed_tools: Some(1),
            upstream_tools: Some(1),
            details: "https://example".to_owned(),
        }
    }

    #[test]
    fn servers_tab_lists_server_names_and_tab_titles() {
        let mut app = App::new();
        app.tab = Tab::Servers;
        app.servers = vec![status_row("memory-module".to_owned())];
        let text = buffer_text(&mut app);
        assert!(text.contains("memory-module"));
        assert!(text.contains("Servers"));
        assert!(text.contains("Live"));
    }

    #[test]
    fn servers_tab_scrolls_selected_row_into_view() {
        let mut app = App::new();
        app.tab = Tab::Servers;
        app.selected = 8;
        app.servers = (0..12)
            .map(|i| status_row(format!("server-{i:02}")))
            .collect();

        let buffer = render_buffer_with_size(&mut app, 80, 8);
        let text = buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();

        assert!(text.contains("server-08"));
        assert!(!text.contains("server-00"));
    }

    #[test]
    fn live_tab_shows_empty_hint_without_traffic() {
        let mut app = App::new();
        app.tab = Tab::Live;
        let text = buffer_text(&mut app);
        assert!(text.contains("no active gateways") || text.contains("Waiting"));
    }

    #[test]
    fn live_tab_renders_metrics_with_traffic() {
        use crate::telemetry::{CallStatus, Event};
        let mut app = App::new();
        app.tab = Tab::Live;
        app.ingest(Event::ToolCall {
            ts: 1,
            pid: 1,
            client: "claude".to_owned(),
            server: "github".to_owned(),
            tool: "github__search".to_owned(),
            duration_ms: 42,
            status: CallStatus::Ok,
        });
        let text = buffer_text(&mut app);
        assert!(text.contains("p95"), "p95 metric should render");
        assert!(text.contains("req/s"), "throughput metric should render");
        assert!(text.contains("errors"), "error count should render");
        assert!(
            text.contains("github__search"),
            "the event feed should render"
        );
    }

    #[test]
    fn tools_tab_shows_policy_rows() {
        use crate::policy::{PolicyDecision, PolicyReason};
        use crate::router::{ToolInspectRow, ToolInspectServer};
        let mut app = App::new();
        app.tab = Tab::Tools;
        app.tools_expanded.insert("github".to_owned());
        app.tools = vec![ToolInspectServer {
            name: "github".to_owned(),
            transport: "http",
            tools: vec![ToolInspectRow {
                exposed_name: "github__search".to_owned(),
                decision: PolicyDecision::Allow,
                reason: PolicyReason::Allowlist,
            }],
            error: None,
        }];
        let text = buffer_text(&mut app);
        assert!(text.contains("github__search"));
        assert!(text.contains("ALLOW"));
    }

    #[test]
    fn tools_tab_can_show_collapsed_server_headers() {
        use crate::policy::{PolicyDecision, PolicyReason};
        use crate::router::{ToolInspectRow, ToolInspectServer};
        let mut app = App::new();
        app.tab = Tab::Tools;
        app.tools = vec![
            ToolInspectServer {
                name: "github".to_owned(),
                transport: "http",
                tools: vec![ToolInspectRow {
                    exposed_name: "github__search".to_owned(),
                    decision: PolicyDecision::Allow,
                    reason: PolicyReason::Allowlist,
                }],
                error: None,
            },
            ToolInspectServer {
                name: "memory".to_owned(),
                transport: "stdio",
                tools: Vec::new(),
                error: None,
            },
        ];

        let text = buffer_text(&mut app);
        assert!(text.contains("[+] MCP github"));
        assert!(text.contains("[+] MCP memory"));
        assert!(!text.contains("github__search"));
    }

    #[test]
    fn tools_tab_highlights_selected_tool_row() {
        use crate::policy::{PolicyDecision, PolicyReason};
        use crate::router::{ToolInspectRow, ToolInspectServer};
        let mut app = App::new();
        app.tab = Tab::Tools;
        app.selected = 2;
        app.tools_expanded.insert("github".to_owned());
        app.tools = vec![ToolInspectServer {
            name: "github".to_owned(),
            transport: "http",
            tools: vec![
                ToolInspectRow {
                    exposed_name: "github__search".to_owned(),
                    decision: PolicyDecision::Allow,
                    reason: PolicyReason::Allowlist,
                },
                ToolInspectRow {
                    exposed_name: "github__create".to_owned(),
                    decision: PolicyDecision::Deny,
                    reason: PolicyReason::Denylist,
                },
            ],
            error: None,
        }];

        let theme = Theme::brand(false);
        let buffer = render_buffer(&mut app);
        let (selected_x, selected_y) = find_text_position(&buffer, "github__create").unwrap();
        let (unselected_x, unselected_y) = find_text_position(&buffer, "github__search").unwrap();

        assert_eq!(
            buffer.cell((selected_x, selected_y)).unwrap().bg,
            theme.selection
        );
        assert_ne!(
            buffer.cell((unselected_x, unselected_y)).unwrap().bg,
            theme.selection
        );
    }

    #[test]
    fn tools_tab_scrolls_selected_tool_into_view() {
        use crate::policy::{PolicyDecision, PolicyReason};
        use crate::router::{ToolInspectRow, ToolInspectServer};
        let mut app = App::new();
        app.tab = Tab::Tools;
        app.selected = 10;
        app.tools_expanded.insert("github".to_owned());
        app.tools = vec![ToolInspectServer {
            name: "github".to_owned(),
            transport: "http",
            tools: (0..12)
                .map(|i| ToolInspectRow {
                    exposed_name: format!("github__tool_{i:02}"),
                    decision: PolicyDecision::Allow,
                    reason: PolicyReason::Allowlist,
                })
                .collect(),
            error: None,
        }];

        let buffer = render_buffer_with_size(&mut app, 80, 8);
        let text = buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();

        assert!(text.contains("github__tool_09"));
        assert!(!text.contains("github__tool_00"));
    }

    #[test]
    fn doctor_tab_shows_checks() {
        use crate::doctor::{CheckStatus, DoctorCheck};
        let mut app = App::new();
        app.tab = Tab::Doctor;
        app.doctor = vec![DoctorCheck {
            status: CheckStatus::Ok,
            name: "PATH".to_owned(),
            detail: "mcpocket on PATH".to_owned(),
        }];
        let text = buffer_text(&mut app);
        assert!(text.contains("PATH"));
        assert!(text.contains("OK"));
    }
}
