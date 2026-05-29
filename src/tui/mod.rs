pub mod app;
pub mod discovery;
pub mod input;
pub mod theme;
pub mod ui;

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use crossterm::event::{Event as CtEvent, KeyEventKind};
use crossterm::{event, execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::config::load_config;
use crate::config_edit::{allow_tool, deny_tool, set_server_enabled};
use crate::doctor::run_doctor;
use crate::policy::{PolicyDecision, PolicyReason};
use crate::router::GatewayRouter;
use crate::telemetry::{Event, run_dir_for};

use self::app::{App, Tab};
use self::discovery::spawn_connection_manager;
use self::input::{Action, map_key};
use self::theme::Theme;

/// Entry point for `mcpocket tui`.
pub async fn run_tui(config_path: PathBuf) -> anyhow::Result<()> {
    // Install the panic hook first so a panic during setup also restores the terminal.
    install_panic_hook();
    let mut terminal = setup_terminal().context("failed to enter TUI mode")?;

    let mut app = App::new();
    let theme = Theme::detect();

    // Telemetry stream from all serve sockets.
    let (event_tx, mut event_rx) = mpsc::channel::<Event>(1024);
    let run_dir = run_dir_for(&config_path);
    tokio::spawn(spawn_connection_manager(run_dir, event_tx));

    // Keyboard input on a blocking thread -> async channel.
    let (key_tx, mut key_rx) = mpsc::channel::<Action>(64);
    spawn_input_thread(key_tx);

    refresh_data(&mut app, &config_path).await;

    let mut tick = tokio::time::interval(Duration::from_millis(125)); // ~8 fps
    let result = loop {
        tokio::select! {
            maybe_ev = event_rx.recv() => {
                if let Some(ev) = maybe_ev { app.ingest(ev); }
            }
            maybe_action = key_rx.recv() => {
                match maybe_action {
                    Some(action) => handle_action(&mut app, action, &config_path).await,
                    None => break Ok(()),
                }
            }
            _ = tick.tick() => {
                app.clear_expired_status(Instant::now());
                if app.dirty {
                    if let Err(e) = terminal.draw(|f| ui::render(f, &app, &theme)) {
                        break Err(anyhow::Error::from(e));
                    }
                    app.dirty = false;
                }
            }
        }
        if app.should_quit {
            break Ok(());
        }
    };

    restore_terminal(&mut terminal);
    result
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn setup_terminal() -> anyhow::Result<Tui> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    // If any later setup step fails, undo what we already enabled so the user's
    // shell is never left in raw mode / the alternate screen.
    if let Err(e) = execute!(
        stdout,
        terminal::EnterAlternateScreen,
        event::EnableMouseCapture
    ) {
        let _ = terminal::disable_raw_mode();
        return Err(e.into());
    }
    match Terminal::new(CrosstermBackend::new(stdout)) {
        Ok(terminal) => Ok(terminal),
        Err(e) => {
            let _ = execute!(
                io::stdout(),
                terminal::LeaveAlternateScreen,
                event::DisableMouseCapture
            );
            let _ = terminal::disable_raw_mode();
            Err(e.into())
        }
    }
}

/// Restore the terminal best-effort: every step runs even if an earlier one
/// fails, so a `disable_raw_mode` error can never skip leaving the alternate
/// screen or showing the cursor.
fn restore_terminal(terminal: &mut Tui) {
    let _ = terminal::disable_raw_mode();
    let _ = execute!(
        terminal.backend_mut(),
        terminal::LeaveAlternateScreen,
        event::DisableMouseCapture
    );
    let _ = terminal.show_cursor();
}

/// Restore the terminal even if a panic unwinds through the render loop.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            terminal::LeaveAlternateScreen,
            event::DisableMouseCapture
        );
        original(info);
    }));
}

fn spawn_input_thread(tx: mpsc::Sender<Action>) {
    std::thread::spawn(move || {
        loop {
            // Block until a terminal event is available.
            match event::read() {
                Ok(CtEvent::Key(key)) if key.kind == KeyEventKind::Press => {
                    let action = map_key(key.code);
                    if tx.blocking_send(action).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
}

async fn handle_action(app: &mut App, action: Action, config_path: &Path) {
    match action {
        Action::Quit => app.should_quit = true,
        Action::NextTab => {
            app.next_tab();
            clamp_selection(app);
        }
        Action::PrevTab => {
            app.prev_tab();
            clamp_selection(app);
        }
        Action::Down => {
            let len = current_len(app);
            if len > 0 {
                app.selected = (app.selected + 1).min(len - 1);
                app.dirty = true;
            }
        }
        Action::Up => {
            app.selected = app.selected.saturating_sub(1);
            app.dirty = true;
        }
        Action::Refresh => refresh_data(app, config_path).await,
        Action::ToggleExpand if app.tab == Tab::Tools => {
            if let Some(server) = selected_tool_server(app) {
                app.toggle_tools_expanded(&server);
                clamp_selection(app);
            }
        }
        Action::Enable | Action::Disable if app.tab == Tab::Servers => {
            if let Some(row) = app.servers.get(app.selected) {
                let name = row.name.clone();
                let enable = matches!(action, Action::Enable);
                match set_server_enabled(config_path, &name, enable) {
                    Ok(()) => app.set_status(format!(
                        "{} {name}",
                        if enable { "Enabled" } else { "Disabled" }
                    )),
                    Err(e) => app.set_status(format!("error: {e}")),
                }
                refresh_data(app, config_path).await;
            }
        }
        Action::Allow | Action::Deny if app.tab == Tab::Tools => {
            if let Some(tool) = selected_tool(app) {
                let allow = matches!(action, Action::Allow);
                let res = if allow {
                    allow_tool(config_path, &tool)
                } else {
                    deny_tool(config_path, &tool)
                };
                match res {
                    Ok(()) => {
                        if let Some(row) = selected_tool_mut(app) {
                            row.decision = if allow {
                                PolicyDecision::Allow
                            } else {
                                PolicyDecision::Deny
                            };
                            row.reason = if allow {
                                PolicyReason::Allowlist
                            } else {
                                PolicyReason::Denylist
                            };
                        }
                        app.set_status(format!("updated policy for {tool}"));
                    }
                    Err(e) => app.set_status(format!("error: {e}")),
                }
                app.dirty = true;
            }
        }
        _ => {}
    }
}

fn clamp_selection(app: &mut App) {
    let len = current_len(app);
    app.selected = app.selected.min(len.saturating_sub(1));
}

fn current_len(app: &App) -> usize {
    match app.tab {
        Tab::Servers => app.servers.len(),
        Tab::Tools => app
            .tools
            .iter()
            .map(|server| {
                1 + if app.is_tools_expanded(&server.name) {
                    server.tools.len()
                } else {
                    0
                }
            })
            .sum(),
        _ => 0,
    }
}

fn selected_tool(app: &App) -> Option<String> {
    let mut row = 0usize;
    for server in &app.tools {
        if app.selected == row {
            return None;
        }
        row += 1;
        if !app.is_tools_expanded(&server.name) {
            continue;
        }
        for tool in &server.tools {
            if app.selected == row {
                return Some(tool.exposed_name.clone());
            }
            row += 1;
        }
    }
    None
}

fn selected_tool_mut(app: &mut App) -> Option<&mut crate::router::ToolInspectRow> {
    let mut row = 0usize;
    for server_idx in 0..app.tools.len() {
        if app.selected == row {
            return None;
        }
        row += 1;

        let expanded = app.is_tools_expanded(&app.tools[server_idx].name);
        if !expanded {
            continue;
        }

        let tools_len = app.tools[server_idx].tools.len();
        for tool_idx in 0..tools_len {
            if app.selected == row {
                return Some(&mut app.tools[server_idx].tools[tool_idx]);
            }
            row += 1;
        }
    }
    None
}

fn selected_tool_server(app: &App) -> Option<String> {
    let mut row = 0usize;
    for server in &app.tools {
        if app.selected == row {
            return Some(server.name.clone());
        }
        row += 1;
        if app.is_tools_expanded(&server.name) {
            row += server.tools.len();
        }
    }
    None
}

/// Reload config and refresh status, tools, and doctor data.
async fn refresh_data(app: &mut App, config_path: &Path) {
    app.doctor = run_doctor(config_path);
    match load_config(config_path) {
        Ok(config) => match GatewayRouter::new(config) {
            Ok(router) => {
                app.servers = router.status().await;
                app.tools = router.inspect_tools(None).await;
            }
            Err(e) => app.set_status(format!("router error: {e}")),
        },
        Err(e) => app.set_status(format!("config error: {e}")),
    }
    if app.selected >= current_len(app) {
        app.selected = current_len(app).saturating_sub(1);
    }
    app.dirty = true;
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::Value;

    use super::*;
    use crate::policy::{PolicyDecision, PolicyReason};
    use crate::router::{ToolInspectRow, ToolInspectServer};

    #[tokio::test]
    async fn toggle_expand_changes_visible_tool_rows() {
        let mut app = App::new();
        app.tab = Tab::Tools;
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

        assert_eq!(current_len(&app), 1);
        handle_action(
            &mut app,
            Action::ToggleExpand,
            Path::new("/unused/config.json"),
        )
        .await;

        assert!(app.is_tools_expanded("github"));
        assert_eq!(current_len(&app), 2);
    }

    #[tokio::test]
    async fn allow_tool_updates_selected_row_without_full_refresh() {
        let temp = TempDir::new("mcpocket-tui-allow-tool");
        let config = temp.path().join("config.json");
        fs::write(
            &config,
            r#"{"version":1,"servers":{"github":{"enabled":false,"transport":"http","url":"https://example.test/mcp","gateway":{"enabled":false,"deny_tools":["github__create_issue"]}}}}"#,
        )
        .unwrap();

        let mut app = App::new();
        app.tab = Tab::Tools;
        app.selected = 1;
        app.tools_expanded.insert("github".to_owned());
        app.tools = vec![ToolInspectServer {
            name: "github".to_owned(),
            transport: "http",
            tools: vec![ToolInspectRow {
                exposed_name: "github__create_issue".to_owned(),
                decision: PolicyDecision::Deny,
                reason: PolicyReason::Denylist,
            }],
            error: None,
        }];

        handle_action(&mut app, Action::Allow, &config).await;

        assert_eq!(app.tools.len(), 1, "tool list should not be refreshed");
        assert_eq!(app.tools[0].tools[0].decision, PolicyDecision::Allow);
        assert_eq!(app.tools[0].tools[0].reason, PolicyReason::Allowlist);
        assert!(app.dirty);

        let updated: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert!(
            updated["servers"]["github"]["gateway"]["allow_tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == "github__create_issue")
        );
        assert!(
            !updated["servers"]["github"]["gateway"]["deny_tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == "github__create_issue")
        );
    }

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(prefix: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
