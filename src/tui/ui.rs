use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Tabs};

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
        Tab::Servers if app.is_server_profile_open() => {
            render_server_profiles(frame, chunks[1], app, theme)
        }
        Tab::Servers => render_servers(frame, chunks[1], app, theme),
        Tab::Tools => render_tools(frame, chunks[1], app, theme),
        Tab::Live => render_live(frame, chunks[1], app, theme),
        Tab::Doctor => render_doctor(frame, chunks[1], app, theme),
    }
    render_footer(frame, chunks[2], app, theme);
    render_text_input_modal(frame, frame.area(), app, theme);
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

fn render_server_profiles(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    use crate::policy::PolicyDecision;

    let Some(server) = app.server_profile_server.as_deref() else {
        render_servers(frame, area, app, theme);
        return;
    };
    let Some(profile_row) = app.server_profiles.iter().find(|row| row.name == server) else {
        let message = Paragraph::new("No profile data loaded. Press [r] to refresh.")
            .style(Style::default().fg(theme.dim))
            .block(Block::default().borders(Borders::ALL).title(" Profiles "));
        frame.render_widget(message, area);
        return;
    };

    let active = profile_row.active_profile.as_deref();
    let mut lines = Vec::new();
    let mut selectable_idx = 0usize;
    let default_selected = app.selected == selectable_idx;
    let default_active = active.is_none();
    lines.push(Line::from(vec![
        Span::styled(
            if default_selected { "> " } else { "  " },
            selectable_style(theme, theme.fg, default_selected),
        ),
        Span::styled(
            if default_active { "* " } else { "  " },
            selectable_style(theme, theme.ok, default_selected),
        ),
        Span::styled(
            "default",
            selectable_style(theme, theme.fg, default_selected),
        ),
        Span::styled(
            "  base server parameters",
            selectable_style(theme, theme.dim, default_selected),
        ),
    ]));
    selectable_idx += 1;
    for field in &profile_row.default_fields {
        let selected = app.selected == selectable_idx;
        lines.push(parameter_line(field, theme, selected));
        selectable_idx += 1;
    }

    for profile in &profile_row.profiles {
        let selected = app.selected == selectable_idx;
        let active = active == Some(profile.as_str());
        lines.push(Line::from(vec![
            Span::styled(
                if selected { "> " } else { "  " },
                selectable_style(theme, theme.fg, selected),
            ),
            Span::styled(
                if active { "* " } else { "  " },
                selectable_style(theme, theme.ok, selected),
            ),
            Span::styled(profile.clone(), selectable_style(theme, theme.fg, selected)),
        ]));
        selectable_idx += 1;
        for field in profile_row
            .profile_fields
            .get(profile)
            .into_iter()
            .flatten()
        {
            let selected = app.selected == selectable_idx;
            lines.push(parameter_line(field, theme, selected));
            selectable_idx += 1;
        }
    }

    if profile_row.profiles.is_empty() {
        lines.push(Line::from(Span::styled(
            "No alternate profiles configured for this MCP.",
            Style::default().fg(theme.dim),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Tools",
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));
    match app.tools.iter().find(|row| row.name == server) {
        Some(tool_server) if tool_server.error.is_some() => {
            let err = tool_server
                .error
                .as_deref()
                .unwrap_or("failed to load tools");
            lines.push(Line::from(Span::styled(
                format!("  FAIL {}", err.lines().next().unwrap_or(err)),
                Style::default().fg(theme.fail),
            )));
        }
        Some(tool_server) if tool_server.tools.is_empty() => {
            lines.push(Line::from(Span::styled(
                "  No tools loaded for this MCP.",
                Style::default().fg(theme.dim),
            )));
        }
        Some(tool_server) => {
            for tool in &tool_server.tools {
                let selected = app.selected == selectable_idx;
                let (label, color) = match tool.decision {
                    PolicyDecision::Allow => ("ALLOW", theme.ok),
                    PolicyDecision::Deny => ("HIDE ", theme.warn),
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        if selected { "> " } else { "  " },
                        selectable_style(theme, theme.fg, selected),
                    ),
                    Span::styled(
                        format!("{label} "),
                        selectable_style(theme, color, selected),
                    ),
                    Span::styled(
                        tool.exposed_name.clone(),
                        selectable_style(theme, theme.fg, selected),
                    ),
                    Span::styled("  ", selectable_style(theme, theme.dim, selected)),
                    Span::styled(
                        tool.reason.label().to_owned(),
                        selectable_style(theme, theme.dim, selected),
                    ),
                ]));
                selectable_idx += 1;
            }
        }
        None => {
            lines.push(Line::from(Span::styled(
                "  Tools have not been loaded yet. Press [r] to refresh.",
                Style::default().fg(theme.dim),
            )));
        }
    }

    let visible_lines = area.height.saturating_sub(2) as usize;
    let scroll = selection_scroll_offset(app.selected, visible_lines) as u16;
    let title = format!(" Profiles: {server} ");
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((scroll, 0))
            .block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn parameter_line(
    field: &crate::config_edit::ServerProfileFieldRow,
    theme: &Theme,
    selected: bool,
) -> Line<'static> {
    Line::from(vec![
        Span::styled("     ", selectable_style(theme, theme.dim, selected)),
        Span::styled(
            format!("{:<18}", field.field),
            selectable_style(theme, theme.dim, selected),
        ),
        Span::styled(
            field.value.clone(),
            selectable_style(theme, theme.dim, selected),
        ),
    ])
}

fn render_servers(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let header = Row::new(["STATE", "NAME", "TYPE", "TOOLS", "LATENCY"]).style(
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    );

    let rows = app.servers.iter().enumerate().map(|(i, row)| {
        let (state, color) = match row.status {
            UpstreamStatus::Loading => (loading_label("LOAD"), theme.accent),
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
            Cell::from(if row.status == UpstreamStatus::Loading {
                "-".to_owned()
            } else {
                format!("{}ms", row.duration_ms)
            }),
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
        let tool_count = if app.refreshing && server.tools.is_empty() && server.error.is_none() {
            "loading".to_owned()
        } else {
            format!("{} tools", server.tools.len())
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker} MCP {}", server.name), header_style),
            Span::styled(format!(" ({})  {tool_count}", server.transport), meta_style),
        ]));
        selectable_idx += 1;
        if !expanded {
            continue;
        }
        if app.refreshing && server.tools.is_empty() && server.error.is_none() {
            lines.push(Line::from(Span::styled(
                format!("  {} loading tools...", loading_label("")),
                Style::default().fg(theme.dim),
            )));
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

fn loading_label(prefix: &'static str) -> &'static str {
    const FRAMES: [&str; 4] = ["|", "/", "-", "\\"];
    let index = ((crate::telemetry::now_ms() / 125) % FRAMES.len() as u64) as usize;
    match prefix {
        "" => FRAMES[index],
        _ => match index {
            0 => "LOAD |",
            1 => "LOAD /",
            2 => "LOAD -",
            _ => "LOAD \\",
        },
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
    let hint = if app.is_server_profile_open() {
        "[j/k] move  [Enter] select/edit  [a]llow [x]deny [A]allow all  [n]ew profile  [b/q] back"
    } else {
        match app.tab {
            Tab::Servers => {
                "[Tab] switch  [j/k] move  [e]nable [d]isable  [o]auth  [r]efresh  [q]uit"
            }
            Tab::Tools => {
                "[Tab] switch  [j/k] move  [Enter] expand  [a]llow [x]deny [A]allow all  [q]uit"
            }
            Tab::Live => "[Tab] switch  live traffic  [q]uit",
            Tab::Doctor => "[Tab] switch  [r]efresh  [q]uit",
        }
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

fn render_text_input_modal(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let Some(input) = &app.text_input else {
        return;
    };

    let modal = centered_rect(area, 76, 9);
    let lines = vec![
        Line::from(Span::styled(
            input.prompt.clone(),
            Style::default().fg(theme.dim),
        )),
        Line::from(""),
        Line::from(Span::styled(
            text_input_display_value(&input.value, modal.width.saturating_sub(2) as usize),
            Style::default().fg(theme.fg),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Enter", Style::default().fg(theme.accent)),
            Span::styled(" save   ", Style::default().fg(theme.dim)),
            Span::styled("Esc", Style::default().fg(theme.accent)),
            Span::styled(" cancel   ", Style::default().fg(theme.dim)),
            Span::styled("Backspace", Style::default().fg(theme.accent)),
            Span::styled(" edit", Style::default().fg(theme.dim)),
        ]),
    ];

    frame.render_widget(Clear, modal);
    frame.render_widget(
        Paragraph::new(lines)
            .style(Style::default().fg(theme.fg))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Edit MCP parameter ")
                    .style(Style::default().bg(theme.bg).fg(theme.fg)),
            ),
        modal,
    );
}

fn centered_rect(area: Rect, preferred_width: u16, preferred_height: u16) -> Rect {
    let max_width = area.width.saturating_sub(2).max(1);
    let max_height = area.height.saturating_sub(2).max(1);
    let width = preferred_width.min(max_width);
    let height = preferred_height.min(max_height);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn text_input_display_value(value: &str, width: usize) -> String {
    let cursor = "_";
    if width == 0 {
        return String::new();
    }
    if width <= cursor.len() {
        return cursor[..width].to_owned();
    }

    let mut chars = value.chars().collect::<Vec<_>>();
    let available = width - cursor.len();
    if chars.len() <= available {
        return format!("{value}{cursor}");
    }

    let marker = "...";
    if available <= marker.len() {
        return format!(
            "{}{cursor}",
            marker.chars().take(available).collect::<String>()
        );
    }
    let tail_len = available.saturating_sub(marker.len());
    let tail = chars.split_off(chars.len().saturating_sub(tail_len));
    format!("{marker}{}{}", tail.into_iter().collect::<String>(), cursor)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::config_edit::{ServerProfileFieldRow, ServerProfileListRow};
    use crate::policy::{PolicyDecision, PolicyReason};
    use crate::router::{ToolInspectRow, ToolInspectServer};
    use crate::tui::app::{App, Tab, TextInputMode};
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
    fn server_profile_view_shows_profiles_and_active_marker() {
        let mut app = App::new();
        app.tab = Tab::Servers;
        app.server_profile_server = Some("memory".to_owned());
        app.selected = 2;
        app.server_profiles = vec![ServerProfileListRow {
            name: "memory".to_owned(),
            active_profile: Some("work".to_owned()),
            default_fields: vec![
                ServerProfileFieldRow {
                    field: "url".to_owned(),
                    value: "https://example.test/mcp".to_owned(),
                    raw_value: "https://example.test/mcp".to_owned(),
                },
                ServerProfileFieldRow {
                    field: "header:x-api-key".to_owned(),
                    value: "***".to_owned(),
                    raw_value: "base-key".to_owned(),
                },
            ],
            profiles: vec!["personal".to_owned(), "work".to_owned()],
            profile_fields: BTreeMap::from([
                (
                    "personal".to_owned(),
                    vec![ServerProfileFieldRow {
                        field: "header:x-api-key".to_owned(),
                        value: "***".to_owned(),
                        raw_value: "personal-key".to_owned(),
                    }],
                ),
                (
                    "work".to_owned(),
                    vec![ServerProfileFieldRow {
                        field: "header:x-api-key".to_owned(),
                        value: "***".to_owned(),
                        raw_value: "work-key".to_owned(),
                    }],
                ),
            ]),
            profile_details: BTreeMap::from([
                ("personal".to_owned(), "header:x-api-key=***".to_owned()),
                ("work".to_owned(), "header:x-api-key=***".to_owned()),
            ]),
        }];
        app.tools = vec![ToolInspectServer {
            name: "memory".to_owned(),
            transport: "http",
            tools: vec![
                ToolInspectRow {
                    exposed_name: "memory__search".to_owned(),
                    decision: PolicyDecision::Allow,
                    reason: PolicyReason::Allowlist,
                },
                ToolInspectRow {
                    exposed_name: "memory__delete".to_owned(),
                    decision: PolicyDecision::Deny,
                    reason: PolicyReason::Destructive,
                },
            ],
            error: None,
        }];

        let theme = Theme::brand(false);
        let buffer = render_buffer(&mut app);
        let text = buffer
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("Profiles: memory"));
        assert!(text.contains("default"));
        assert!(text.contains("personal"));
        assert!(text.contains("* work"));
        assert!(text.contains("https://example.test/mcp"));
        assert!(text.contains("header:x-api-key"));
        assert!(text.contains("***"));
        assert!(text.contains("[Enter] select"));
        assert!(text.contains("[n]ew profile"));
        assert!(text.contains("Tools"));
        assert!(text.contains("memory__search"));
        assert!(text.contains("memory__delete"));
        assert!(text.contains("ALLOW"));
        assert!(text.contains("HIDE"));

        let (field_x, field_y) = find_text_position(&buffer, "https://example.test/mcp").unwrap();
        assert_ne!(buffer.cell((field_x, field_y)).unwrap().bg, theme.selection);
    }

    #[test]
    fn text_input_modal_renders_bounded_edit_value() {
        let mut app = App::new();
        app.tab = Tab::Servers;
        app.server_profile_server = Some("memory".to_owned());
        let long_value = format!("header:x-api-key={}", "s".repeat(200));

        app.open_text_input(
            TextInputMode::EditServerParameter {
                server: "memory".to_owned(),
                profile: Some("work".to_owned()),
                field: "header:x-api-key".to_owned(),
            },
            "edit value (Enter save, Esc cancel)",
            long_value.clone(),
        );

        let text = buffer_text(&mut app);
        assert!(text.contains("Edit MCP parameter"));
        assert!(text.contains("..."));
        assert!(text.contains("ssssssss_"));
        assert!(!text.contains(&long_value));
        assert!(text.contains("[j/k] move"));
    }

    #[test]
    fn text_input_display_value_fits_available_width() {
        assert_eq!(text_input_display_value("abc", 4), "abc_");
        assert_eq!(text_input_display_value("abcdef", 5), "...f_");
        assert_eq!(text_input_display_value("abcdef", 2), "._");
        assert_eq!(text_input_display_value("abcdef", 0), "");
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
