use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs};

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
        Tab::Tools => render_placeholder(frame, chunks[1], "Tools", theme),
        Tab::Live => render_live(frame, chunks[1], app, theme),
        Tab::Doctor => render_placeholder(frame, chunks[1], "Doctor", theme),
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
    frame.render_widget(table, area);
}

fn render_live(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if app.live_events.is_empty() {
        let hint = Paragraph::new("Waiting for gateway traffic… (no active gateways yet)")
            .style(Style::default().fg(theme.dim))
            .block(Block::default().borders(Borders::ALL).title(" Live "));
        frame.render_widget(hint, area);
        return;
    }

    let lines: Vec<Line> = app
        .live_events
        .iter()
        .rev()
        .take(area.height.saturating_sub(2) as usize)
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

    let para = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Live "));
    frame.render_widget(para, area);
}

fn render_placeholder(frame: &mut Frame, area: Rect, title: &str, theme: &Theme) {
    let para = Paragraph::new(format!("{title} — coming up"))
        .style(Style::default().fg(theme.dim))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" {title} ")),
        );
    frame.render_widget(para, area);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let hint = match app.tab {
        Tab::Servers => "[Tab] switch  [j/k] move  [e]nable [d]isable  [r]efresh  [q]uit",
        Tab::Tools => "[Tab] switch  [j/k] move  [a]llow [x]deny  [q]uit",
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
    use ratatui::{Terminal, backend::TestBackend};

    fn buffer_text(app: &mut App) -> String {
        let theme = Theme::brand(false);
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, app, &theme)).unwrap();
        let buf = terminal.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    #[test]
    fn servers_tab_lists_server_names_and_tab_titles() {
        let mut app = App::new();
        app.tab = Tab::Servers;
        app.servers = vec![StatusRow {
            name: "memory-module".to_owned(),
            transport: "http",
            status: UpstreamStatus::Reachable,
            duration_ms: 430,
            exposed_tools: Some(5),
            upstream_tools: Some(11),
            details: "https://example".to_owned(),
        }];
        let text = buffer_text(&mut app);
        assert!(text.contains("memory-module"));
        assert!(text.contains("Servers"));
        assert!(text.contains("Live"));
    }

    #[test]
    fn live_tab_shows_empty_hint_without_traffic() {
        let mut app = App::new();
        app.tab = Tab::Live;
        let text = buffer_text(&mut app);
        assert!(text.contains("no active gateways") || text.contains("Waiting"));
    }
}
