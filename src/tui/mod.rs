pub mod app;
pub mod discovery;
pub mod input;
pub mod theme;
pub mod ui;

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{Event as CtEvent, KeyEventKind};
use crossterm::{event, execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::config::load_config;
use crate::config_edit::{allow_tool, deny_tool, set_server_enabled};
use crate::doctor::run_doctor;
use crate::router::GatewayRouter;
use crate::telemetry::{Event, run_dir_for};

use self::app::{App, Tab};
use self::discovery::spawn_connection_manager;
use self::input::{Action, map_key};
use self::theme::Theme;

/// Entry point for `mcpocket tui`.
pub async fn run_tui(config_path: PathBuf) -> anyhow::Result<()> {
    let mut terminal = setup_terminal().context("failed to enter TUI mode")?;
    install_panic_hook();

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

    restore_terminal(&mut terminal).ok();
    result
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn setup_terminal() -> anyhow::Result<Tui> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        terminal::EnterAlternateScreen,
        event::EnableMouseCapture
    )?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Tui) -> anyhow::Result<()> {
    terminal::disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        terminal::LeaveAlternateScreen,
        event::DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
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
        Action::NextTab => app.next_tab(),
        Action::PrevTab => app.prev_tab(),
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
        Action::Enable | Action::Disable if app.tab == Tab::Servers => {
            if let Some(row) = app.servers.get(app.selected) {
                let name = row.name.clone();
                let enable = matches!(action, Action::Enable);
                match set_server_enabled(config_path, &name, enable) {
                    Ok(()) => {
                        app.status_message = Some(format!(
                            "{} {name}",
                            if enable { "Enabled" } else { "Disabled" }
                        ))
                    }
                    Err(e) => app.status_message = Some(format!("error: {e}")),
                }
                refresh_data(app, config_path).await;
            }
        }
        Action::Allow | Action::Deny if app.tab == Tab::Tools => {
            if let Some(tool) = selected_tool(app) {
                let res = if matches!(action, Action::Allow) {
                    allow_tool(config_path, &tool)
                } else {
                    deny_tool(config_path, &tool)
                };
                app.status_message = Some(match res {
                    Ok(()) => format!("updated policy for {tool}"),
                    Err(e) => format!("error: {e}"),
                });
                refresh_data(app, config_path).await;
            }
        }
        _ => {}
    }
}

fn current_len(app: &App) -> usize {
    match app.tab {
        Tab::Servers => app.servers.len(),
        Tab::Tools => app.tools.iter().map(|s| s.tools.len()).sum(),
        _ => 0,
    }
}

fn selected_tool(app: &App) -> Option<String> {
    let mut idx = app.selected;
    for server in &app.tools {
        if idx < server.tools.len() {
            return Some(server.tools[idx].exposed_name.clone());
        }
        idx -= server.tools.len();
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
            Err(e) => app.status_message = Some(format!("router error: {e}")),
        },
        Err(e) => app.status_message = Some(format!("config error: {e}")),
    }
    if app.selected >= current_len(app) {
        app.selected = current_len(app).saturating_sub(1);
    }
    app.dirty = true;
}
