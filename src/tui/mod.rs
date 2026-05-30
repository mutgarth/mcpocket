pub mod app;
pub mod discovery;
pub mod input;
pub mod theme;
pub mod ui;

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context;
use crossterm::event::{Event as CtEvent, KeyCode, KeyEventKind};
use crossterm::{event, execute, terminal};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

use crate::config::load_config;
use crate::config_edit::{
    allow_tool, allow_tools, create_server_profile, deny_tool, list_server_profiles,
    set_server_active_profile, set_server_enabled, set_server_field, set_server_profile_field,
};
use crate::doctor::{DoctorCheck, run_doctor};
use crate::oauth::authenticate_http_server;
use crate::policy::{PolicyDecision, PolicyReason};
use crate::router::{GatewayRouter, ToolInspectServer};
use crate::telemetry::{Event, run_dir_for};
use crate::upstream::{StatusRow, UpstreamStatus};

use self::app::{App, Tab, TextInputMode};
use self::discovery::spawn_connection_manager;
use self::input::{Action, map_key};
use self::theme::Theme;

type AuthEvent = Result<crate::oauth::AuthResult, String>;
type DataEvent = Result<DataSnapshot, String>;

struct DataSnapshot {
    doctor: Vec<DoctorCheck>,
    server_profiles: Vec<crate::config_edit::ServerProfileListRow>,
    servers: Vec<StatusRow>,
    tools: Vec<ToolInspectServer>,
}

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
    let (key_tx, mut key_rx) = mpsc::channel::<KeyCode>(64);
    spawn_input_thread(key_tx);
    let (auth_tx, mut auth_rx) = mpsc::channel::<AuthEvent>(4);
    let (data_tx, mut data_rx) = mpsc::channel::<DataEvent>(4);

    match load_initial_data(&config_path) {
        Ok(snapshot) => apply_data_snapshot(&mut app, snapshot),
        Err(e) => app.set_status(e),
    }
    start_background_refresh(&mut app, config_path.clone(), data_tx.clone());
    terminal.draw(|f| ui::render(f, &app, &theme))?;
    app.dirty = false;

    let mut tick = tokio::time::interval(Duration::from_millis(125)); // ~8 fps
    let result = loop {
        tokio::select! {
            maybe_ev = event_rx.recv() => {
                if let Some(ev) = maybe_ev { app.ingest(ev); }
            }
            maybe_key = key_rx.recv() => {
                match maybe_key {
                    Some(key) => {
                        handle_key(
                            &mut app,
                            key,
                            &config_path,
                            Some(auth_tx.clone()),
                            Some(data_tx.clone()),
                        ).await
                    }
                    None => break Ok(()),
                }
            }
            maybe_auth = auth_rx.recv() => {
                if let Some(result) = maybe_auth {
                    if handle_auth_event(&mut app, result) {
                        spawn_data_refresh(config_path.clone(), data_tx.clone());
                    }
                }
            }
            maybe_data = data_rx.recv() => {
                if let Some(result) = maybe_data {
                    handle_data_event(&mut app, result);
                }
            }
            _ = tick.tick() => {
                app.clear_expired_status(Instant::now());
                if app.refreshing {
                    app.dirty = true;
                }
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

fn spawn_input_thread(tx: mpsc::Sender<KeyCode>) {
    std::thread::spawn(move || {
        loop {
            // Block until a terminal event is available.
            match event::read() {
                Ok(CtEvent::Key(key)) if key.kind == KeyEventKind::Press => {
                    if tx.blocking_send(key.code).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
}

async fn handle_key(
    app: &mut App,
    key: KeyCode,
    config_path: &Path,
    auth_tx: Option<mpsc::Sender<AuthEvent>>,
    data_tx: Option<mpsc::Sender<DataEvent>>,
) {
    if app.is_text_input_open() {
        handle_text_input_key(app, key, config_path).await;
    } else {
        handle_action_with_auth(app, map_key(key), config_path, auth_tx, data_tx).await;
    }
}

#[cfg(test)]
async fn handle_action(app: &mut App, action: Action, config_path: &Path) {
    handle_action_with_auth(app, action, config_path, None, None).await;
}

async fn handle_action_with_auth(
    app: &mut App,
    action: Action,
    config_path: &Path,
    auth_tx: Option<mpsc::Sender<AuthEvent>>,
    data_tx: Option<mpsc::Sender<DataEvent>>,
) {
    if app.is_server_profile_open() {
        handle_server_profile_action(app, action, config_path).await;
        return;
    }

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
        Action::Refresh => {
            if let Some(data_tx) = data_tx {
                start_background_refresh(app, config_path.to_owned(), data_tx);
            } else {
                refresh_data(app, config_path).await;
            }
        }
        Action::Auth if app.tab == Tab::Servers => {
            if let Some(row) = app.servers.get(app.selected) {
                let name = row.name.clone();
                if let Some(auth_tx) = auth_tx {
                    let config_path = config_path.to_owned();
                    let auth_name = name.clone();
                    app.set_status(format!("opening browser auth for {name}"));
                    tokio::spawn(async move {
                        let result = authenticate_http_server(&config_path, &auth_name)
                            .await
                            .map_err(|error| error.to_string());
                        let _ = auth_tx.send(result).await;
                    });
                } else {
                    match authenticate_http_server(config_path, &name).await {
                        Ok(result) => {
                            handle_auth_event(app, Ok(result));
                            refresh_data(app, config_path).await;
                        }
                        Err(e) => app.set_status(format!("auth error: {e}")),
                    }
                }
            }
        }
        Action::ToggleExpand if app.tab == Tab::Servers => {
            if let Some(row) = app.servers.get(app.selected) {
                let server = row.name.clone();
                let selected_profile = active_profile_selection(app, &server);
                app.open_server_profile(server, selected_profile);
            }
        }
        Action::ToggleExpand if app.tab == Tab::Tools => {
            if let Some(server) = selected_tool_server(app) {
                app.toggle_tools_expanded(&server);
                clamp_selection(app);
            }
        }
        Action::AllowAll if app.tab == Tab::Tools => {
            if let Some(server) = selected_tool_server(app) {
                allow_all_server_tools(app, config_path, &server);
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
                update_tool_policy(app, config_path, &tool, allow);
            }
        }
        _ => {}
    }
}

fn handle_auth_event(app: &mut App, result: AuthEvent) -> bool {
    match result {
        Ok(result) => {
            let scope_text = if result.scopes.is_empty() {
                "default scopes".to_owned()
            } else {
                format!("scopes {}", result.scopes.join(","))
            };
            app.set_status(format!(
                "authenticated {} ({scope_text}); refreshing",
                result.server
            ));
            true
        }
        Err(e) => {
            app.set_status(format!("auth error: {e}"));
            false
        }
    }
}

fn update_tool_policy(app: &mut App, config_path: &Path, tool: &str, allow: bool) {
    let res = if allow {
        allow_tool(config_path, tool)
    } else {
        deny_tool(config_path, tool)
    };
    match res {
        Ok(()) => {
            if let Some(server) = set_tool_policy_in_memory(app, tool, allow) {
                refresh_server_tool_count(app, &server);
            }
            app.set_status(format!("updated policy for {tool}"));
        }
        Err(e) => app.set_status(format!("error: {e}")),
    }
    app.dirty = true;
}

fn allow_all_server_tools(app: &mut App, config_path: &Path, server: &str) {
    let tools = server_tool_names(app, server);
    if tools.is_empty() {
        app.set_status(format!("no tools loaded for {server}"));
        return;
    }

    match allow_tools(config_path, &tools) {
        Ok(changed) => {
            set_server_tools_policy_in_memory(app, server, true);
            refresh_server_tool_count(app, server);
            app.set_status(format!(
                "allowed {changed}/{} tools for {server}",
                tools.len()
            ));
        }
        Err(e) => app.set_status(format!("error: {e}")),
    }
    app.dirty = true;
}

fn server_tool_names(app: &App, server: &str) -> Vec<String> {
    app.tools
        .iter()
        .find(|row| row.name == server)
        .map(|server| {
            server
                .tools
                .iter()
                .map(|tool| tool.exposed_name.clone())
                .collect()
        })
        .unwrap_or_default()
}

fn set_tool_policy_in_memory(app: &mut App, exposed_tool: &str, allow: bool) -> Option<String> {
    for server in &mut app.tools {
        if let Some(tool) = server
            .tools
            .iter_mut()
            .find(|tool| tool.exposed_name == exposed_tool)
        {
            tool.decision = if allow {
                PolicyDecision::Allow
            } else {
                PolicyDecision::Deny
            };
            tool.reason = if allow {
                PolicyReason::Allowlist
            } else {
                PolicyReason::Denylist
            };
            return Some(server.name.clone());
        }
    }
    None
}

fn set_server_tools_policy_in_memory(app: &mut App, server_name: &str, allow: bool) {
    if let Some(server) = app.tools.iter_mut().find(|row| row.name == server_name) {
        for tool in &mut server.tools {
            tool.decision = if allow {
                PolicyDecision::Allow
            } else {
                PolicyDecision::Deny
            };
            tool.reason = if allow {
                PolicyReason::Allowlist
            } else {
                PolicyReason::Denylist
            };
        }
    }
}

fn refresh_server_tool_count(app: &mut App, server_name: &str) {
    let Some(tool_server) = app.tools.iter().find(|row| row.name == server_name) else {
        return;
    };
    let exposed = tool_server
        .tools
        .iter()
        .filter(|tool| tool.decision == PolicyDecision::Allow)
        .count();
    let total = tool_server.tools.len();
    if let Some(server) = app.servers.iter_mut().find(|row| row.name == server_name) {
        server.exposed_tools = Some(exposed);
        server.upstream_tools = Some(total);
    }
}

async fn handle_server_profile_action(app: &mut App, action: Action, config_path: &Path) {
    match action {
        Action::Quit | Action::Back => app.close_server_profile(),
        Action::New => {
            let Some(server) = app.server_profile_server.clone() else {
                return;
            };
            app.open_text_input(
                TextInputMode::NewServerProfile { server },
                "new profile name",
                "",
            );
        }
        Action::Enable => {
            open_selected_parameter_input(app);
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
        Action::Allow => {
            if let Some(ServerProfileSelection::Tool { tool }) = selected_profile_row(app) {
                update_tool_policy(app, config_path, &tool, true);
            }
        }
        Action::Deny => {
            if let Some(ServerProfileSelection::Tool { tool }) = selected_profile_row(app) {
                update_tool_policy(app, config_path, &tool, false);
            }
        }
        Action::AllowAll => {
            if let Some(server) = app.server_profile_server.clone() {
                allow_all_server_tools(app, config_path, &server);
            }
        }
        Action::ToggleExpand => {
            let Some(server) = app.server_profile_server.clone() else {
                return;
            };
            match selected_profile_row(app) {
                Some(ServerProfileSelection::DefaultProfile) => {
                    match set_server_active_profile(config_path, &server, None) {
                        Ok(()) => {
                            if let Some(row) = app
                                .server_profiles
                                .iter_mut()
                                .find(|row| row.name == server)
                            {
                                row.active_profile = None;
                            }
                            app.set_status(format!("selected default profile for {server}"));
                        }
                        Err(e) => app.set_status(format!("error: {e}")),
                    }
                }
                Some(ServerProfileSelection::Profile { profile }) => {
                    match set_server_active_profile(config_path, &server, Some(&profile)) {
                        Ok(()) => {
                            if let Some(row) = app
                                .server_profiles
                                .iter_mut()
                                .find(|row| row.name == server)
                            {
                                row.active_profile = Some(profile.clone());
                            }
                            app.set_status(format!("selected profile {profile} for {server}"));
                        }
                        Err(e) => app.set_status(format!("error: {e}")),
                    }
                }
                Some(ServerProfileSelection::DefaultField { .. })
                | Some(ServerProfileSelection::ProfileField { .. }) => {
                    open_selected_parameter_input(app);
                }
                Some(ServerProfileSelection::Tool { tool }) => {
                    update_tool_policy(app, config_path, &tool, true);
                }
                None => {}
            }
            app.dirty = true;
        }
        _ => {}
    }
}

async fn handle_text_input_key(app: &mut App, key: KeyCode, config_path: &Path) {
    match key {
        KeyCode::Esc => app.close_text_input(),
        KeyCode::Backspace if app.text_input_prefix_len() >= app.text_input_value_len() => {}
        KeyCode::Backspace => {
            if let Some(input) = &mut app.text_input {
                input.value.pop();
                app.dirty = true;
            }
        }
        KeyCode::Enter => submit_text_input(app, config_path).await,
        KeyCode::Char(ch) => {
            if let Some(input) = &mut app.text_input {
                input.value.push(ch);
                app.dirty = true;
            }
        }
        _ => {}
    }
}

async fn submit_text_input(app: &mut App, config_path: &Path) {
    let Some(input) = app.text_input.clone() else {
        return;
    };
    match input.mode {
        TextInputMode::NewServerProfile { server } => {
            let profile = input.value.trim();
            match create_server_profile(config_path, &server, profile) {
                Ok(()) => {
                    refresh_profiles(app, config_path);
                    app.set_status(format!("created profile {profile} for {server}"));
                    let selected = selected_profile_header_position(app, &server, profile);
                    if let Some(selected) = selected {
                        app.selected = selected;
                    }
                    app.close_text_input();
                }
                Err(e) => app.set_status(format!("error: {e}")),
            }
        }
        TextInputMode::EditServerParameter {
            server,
            profile,
            field,
        } => {
            let value = input
                .value
                .strip_prefix(&(field.clone() + "="))
                .unwrap_or(input.value.as_str());
            let result = match &profile {
                Some(profile) => {
                    set_server_profile_field(config_path, &server, profile, &field, value)
                }
                None => set_server_field(config_path, &server, &field, value),
            };
            match result {
                Ok(()) => {
                    refresh_profiles(app, config_path);
                    app.set_status(match profile {
                        Some(profile) => format!("updated {field} for {server}/{profile}"),
                        None => format!("updated {field} for {server}"),
                    });
                    app.close_text_input();
                }
                Err(e) => app.set_status(format!("error: {e}")),
            };
        }
    }
}

fn clamp_selection(app: &mut App) {
    let len = current_len(app);
    app.selected = app.selected.min(len.saturating_sub(1));
}

fn current_len(app: &App) -> usize {
    if app.is_server_profile_open() {
        return selected_server_profiles(app)
            .map(|row| server_profile_row_count(app, row))
            .unwrap_or(0);
    }

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

fn active_profile_selection(app: &App, server: &str) -> usize {
    let Some(row) = selected_server_profiles_by_name(app, server) else {
        return 0;
    };
    let Some(active) = row.active_profile.as_ref() else {
        return 0;
    };

    let mut idx = 1 + row.default_fields.len();
    for profile in &row.profiles {
        if profile == active {
            return idx;
        }
        idx += 1 + row.profile_fields.get(profile).map(Vec::len).unwrap_or(0);
    }

    0
}

fn server_profile_row_count(app: &App, row: &crate::config_edit::ServerProfileListRow) -> usize {
    1 + row.default_fields.len()
        + row
            .profiles
            .iter()
            .map(|profile| 1 + row.profile_fields.get(profile).map(Vec::len).unwrap_or(0))
            .sum::<usize>()
        + app
            .tools
            .iter()
            .find(|tool_server| tool_server.name == row.name)
            .map(|tool_server| tool_server.tools.len())
            .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ServerProfileSelection {
    DefaultProfile,
    DefaultField {
        field: String,
        value: String,
    },
    Profile {
        profile: String,
    },
    ProfileField {
        profile: String,
        field: String,
        value: String,
    },
    Tool {
        tool: String,
    },
}

fn selected_profile_row(app: &App) -> Option<ServerProfileSelection> {
    let row = selected_server_profiles(app)?;
    let mut idx = 0usize;
    if app.selected == idx {
        return Some(ServerProfileSelection::DefaultProfile);
    }
    idx += 1;

    for field in &row.default_fields {
        if app.selected == idx {
            return Some(ServerProfileSelection::DefaultField {
                field: field.field.clone(),
                value: field.raw_value.clone(),
            });
        }
        idx += 1;
    }

    for profile in &row.profiles {
        if app.selected == idx {
            return Some(ServerProfileSelection::Profile {
                profile: profile.clone(),
            });
        }
        idx += 1;
        for field in row.profile_fields.get(profile).into_iter().flatten() {
            if app.selected == idx {
                return Some(ServerProfileSelection::ProfileField {
                    profile: profile.clone(),
                    field: field.field.clone(),
                    value: field.raw_value.clone(),
                });
            }
            idx += 1;
        }
    }

    for tool in server_tool_names(app, &row.name) {
        if app.selected == idx {
            return Some(ServerProfileSelection::Tool { tool });
        }
        idx += 1;
    }

    None
}

fn open_selected_parameter_input(app: &mut App) {
    let Some(server) = app.server_profile_server.clone() else {
        return;
    };
    match selected_profile_row(app) {
        Some(ServerProfileSelection::DefaultField { field, value }) => app.open_text_input(
            TextInputMode::EditServerParameter {
                server,
                profile: None,
                field: field.clone(),
            },
            "edit value (Enter save, Esc cancel)",
            format!("{field}={value}"),
        ),
        Some(ServerProfileSelection::ProfileField {
            profile,
            field,
            value,
        }) => app.open_text_input(
            TextInputMode::EditServerParameter {
                server,
                profile: Some(profile),
                field: field.clone(),
            },
            "edit value (Enter save, Esc cancel)",
            format!("{field}={value}"),
        ),
        Some(ServerProfileSelection::Profile { profile }) => app.open_text_input(
            TextInputMode::EditServerParameter {
                server,
                profile: Some(profile),
                field: "header:x-api-key".to_owned(),
            },
            "edit value (Enter save, Esc cancel)",
            "header:x-api-key=",
        ),
        Some(ServerProfileSelection::DefaultProfile) => app.open_text_input(
            TextInputMode::EditServerParameter {
                server,
                profile: None,
                field: "url".to_owned(),
            },
            "edit value (Enter save, Esc cancel)",
            "url=",
        ),
        Some(ServerProfileSelection::Tool { .. }) => {}
        None => {}
    }
}

fn selected_profile_header_position(app: &App, server: &str, profile: &str) -> Option<usize> {
    let row = selected_server_profiles_by_name(app, server)?;
    let mut idx = 1 + row.default_fields.len();
    for item in &row.profiles {
        if item == profile {
            return Some(idx);
        }
        idx += 1 + row.profile_fields.get(item).map(Vec::len).unwrap_or(0);
    }
    None
}

fn selected_server_profiles(app: &App) -> Option<&crate::config_edit::ServerProfileListRow> {
    let server = app.server_profile_server.as_deref()?;
    selected_server_profiles_by_name(app, server)
}

fn selected_server_profiles_by_name<'a>(
    app: &'a App,
    server: &str,
) -> Option<&'a crate::config_edit::ServerProfileListRow> {
    app.server_profiles.iter().find(|row| row.name == server)
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
    app.refreshing = true;
    match load_data(config_path).await {
        Ok(snapshot) => apply_data_snapshot(app, snapshot),
        Err(e) => app.set_status(e),
    }
    app.refreshing = false;
    app.dirty = true;
}

fn start_background_refresh(app: &mut App, config_path: PathBuf, data_tx: mpsc::Sender<DataEvent>) {
    app.refreshing = true;
    app.dirty = true;
    spawn_data_refresh(config_path, data_tx);
}

fn spawn_data_refresh(config_path: PathBuf, data_tx: mpsc::Sender<DataEvent>) {
    tokio::spawn(async move {
        let result = load_data(&config_path).await;
        let _ = data_tx.send(result).await;
    });
}

fn load_initial_data(config_path: &Path) -> DataEvent {
    let doctor = run_doctor(config_path);
    let server_profiles =
        list_server_profiles(config_path).map_err(|e| format!("profile error: {e}"))?;
    let config = load_config(config_path).map_err(|e| format!("config error: {e}"))?;
    let mut servers = Vec::new();
    let mut tools = Vec::new();
    for server in config.active_gateway_servers() {
        servers.push(StatusRow {
            name: server.name.clone(),
            transport: server.transport_name(),
            status: UpstreamStatus::Loading,
            duration_ms: 0,
            exposed_tools: None,
            upstream_tools: None,
            details: server.redacted_details(),
        });
        tools.push(ToolInspectServer {
            name: server.name.clone(),
            transport: server.transport_name(),
            tools: Vec::new(),
            error: None,
        });
    }
    Ok(DataSnapshot {
        doctor,
        server_profiles,
        servers,
        tools,
    })
}

async fn load_data(config_path: &Path) -> DataEvent {
    let doctor = run_doctor(config_path);
    let server_profiles =
        list_server_profiles(config_path).map_err(|e| format!("profile error: {e}"))?;
    let config = load_config(config_path).map_err(|e| format!("config error: {e}"))?;
    let router = GatewayRouter::new(config).map_err(|e| format!("router error: {e}"))?;
    let servers = router.status().await;
    let tools = router.inspect_tools(None).await;
    Ok(DataSnapshot {
        doctor,
        server_profiles,
        servers,
        tools,
    })
}

fn handle_data_event(app: &mut App, result: DataEvent) {
    match result {
        Ok(snapshot) => apply_data_snapshot(app, snapshot),
        Err(e) => app.set_status(e),
    }
    app.refreshing = false;
    app.dirty = true;
}

fn apply_data_snapshot(app: &mut App, snapshot: DataSnapshot) {
    app.doctor = snapshot.doctor;
    app.server_profiles = snapshot.server_profiles;
    app.servers = snapshot.servers;
    app.tools = snapshot.tools;
    if app.selected >= current_len(app) {
        app.selected = current_len(app).saturating_sub(1);
    }
    app.dirty = true;
}

fn refresh_profiles(app: &mut App, config_path: &Path) {
    match list_server_profiles(config_path) {
        Ok(profiles) => app.server_profiles = profiles,
        Err(e) => app.set_status(format!("profile error: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::Value;

    use super::*;
    use crate::config_edit::{ServerProfileFieldRow, ServerProfileListRow};
    use crate::policy::{PolicyDecision, PolicyReason};
    use crate::router::{ToolInspectRow, ToolInspectServer};
    use crate::upstream::UpstreamStatus;

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
    async fn server_profile_view_allows_selected_and_all_tools() {
        let temp = TempDir::new("mcpocket-tui-profile-tools");
        let config = temp.path().join("config.json");
        fs::write(
            &config,
            r#"{"version":1,"servers":{"github":{"enabled":true,"transport":"http","url":"https://example.test/mcp","gateway":{"allow_tools":[],"deny_tools":["github__create","github__delete"]}}}}"#,
        )
        .unwrap();

        let mut app = App::new();
        app.tab = Tab::Servers;
        app.server_profile_server = Some("github".to_owned());
        app.server_profiles = vec![ServerProfileListRow {
            name: "github".to_owned(),
            active_profile: None,
            default_fields: Vec::new(),
            profiles: Vec::new(),
            profile_fields: BTreeMap::new(),
            profile_details: BTreeMap::new(),
        }];
        app.servers = vec![StatusRow {
            name: "github".to_owned(),
            transport: "http",
            status: UpstreamStatus::Reachable,
            duration_ms: 10,
            exposed_tools: Some(0),
            upstream_tools: Some(2),
            details: "https://example.test/mcp".to_owned(),
        }];
        app.tools = vec![ToolInspectServer {
            name: "github".to_owned(),
            transport: "http",
            tools: vec![
                ToolInspectRow {
                    exposed_name: "github__create".to_owned(),
                    decision: PolicyDecision::Deny,
                    reason: PolicyReason::Denylist,
                },
                ToolInspectRow {
                    exposed_name: "github__delete".to_owned(),
                    decision: PolicyDecision::Deny,
                    reason: PolicyReason::Denylist,
                },
            ],
            error: None,
        }];

        app.selected = 1;
        handle_action(&mut app, Action::Allow, &config).await;
        assert_eq!(app.tools[0].tools[0].decision, PolicyDecision::Allow);
        assert_eq!(app.tools[0].tools[1].decision, PolicyDecision::Deny);
        assert_eq!(app.servers[0].exposed_tools, Some(1));
        assert_eq!(app.servers[0].upstream_tools, Some(2));

        handle_action(&mut app, Action::AllowAll, &config).await;
        assert_eq!(app.tools[0].tools[0].decision, PolicyDecision::Allow);
        assert_eq!(app.tools[0].tools[1].decision, PolicyDecision::Allow);
        assert_eq!(app.servers[0].exposed_tools, Some(2));
        assert_eq!(app.servers[0].upstream_tools, Some(2));

        app.selected = 2;
        handle_action(&mut app, Action::Deny, &config).await;
        assert_eq!(app.tools[0].tools[0].decision, PolicyDecision::Allow);
        assert_eq!(app.tools[0].tools[1].decision, PolicyDecision::Deny);
        assert_eq!(app.servers[0].exposed_tools, Some(1));
        assert_eq!(app.servers[0].upstream_tools, Some(2));

        let updated: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert!(
            updated["servers"]["github"]["gateway"]["deny_tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == "github__delete")
        );
        assert!(
            !updated["servers"]["github"]["gateway"]["allow_tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == "github__delete")
        );
        for tool in ["github__create"] {
            assert!(
                updated["servers"]["github"]["gateway"]["allow_tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item == tool)
            );
            assert!(
                !updated["servers"]["github"]["gateway"]["deny_tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|item| item == tool)
            );
        }
    }

    #[tokio::test]
    async fn server_profile_view_selects_active_profile() {
        let temp = TempDir::new("mcpocket-tui-profile-select");
        let config = temp.path().join("config.json");
        fs::write(
            &config,
            r#"{"version":1,"servers":{"memory":{"enabled":true,"transport":"http","url":"https://example.test/mcp","profiles":{"personal":{"headers":{"x-api-key":"one"}},"work":{"headers":{"x-api-key":"two"}}}}}}"#,
        )
        .unwrap();

        let mut app = App::new();
        app.tab = Tab::Servers;
        app.server_profile_server = Some("memory".to_owned());
        app.selected = 2;
        app.server_profiles = vec![ServerProfileListRow {
            name: "memory".to_owned(),
            active_profile: None,
            default_fields: Vec::new(),
            profiles: vec!["personal".to_owned(), "work".to_owned()],
            profile_fields: BTreeMap::<String, Vec<ServerProfileFieldRow>>::new(),
            profile_details: BTreeMap::new(),
        }];

        handle_action(&mut app, Action::ToggleExpand, &config).await;

        assert_eq!(
            app.server_profiles[0].active_profile.as_deref(),
            Some("work")
        );
        let selected: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            selected["servers"]["memory"]["active_profile"].as_str(),
            Some("work")
        );
    }

    #[tokio::test]
    async fn text_input_creates_profile_from_server_profile_view() {
        let temp = TempDir::new("mcpocket-tui-profile-create");
        let config = temp.path().join("config.json");
        fs::write(
            &config,
            r#"{"version":1,"servers":{"memory":{"enabled":true,"transport":"http","url":"https://example.test/mcp"}}}"#,
        )
        .unwrap();

        let mut app = App::new();
        app.tab = Tab::Servers;
        app.server_profile_server = Some("memory".to_owned());
        app.server_profiles = list_server_profiles(&config).unwrap();

        handle_action(&mut app, Action::New, &config).await;
        for ch in "work".chars() {
            handle_text_input_key(&mut app, KeyCode::Char(ch), &config).await;
        }
        handle_text_input_key(&mut app, KeyCode::Enter, &config).await;

        assert!(app.text_input.is_none());
        assert_eq!(app.server_profiles[0].profiles, ["work"]);
        let updated: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert!(updated["servers"]["memory"]["profiles"]["work"].is_object());
    }

    #[tokio::test]
    async fn text_input_updates_profile_field() {
        let temp = TempDir::new("mcpocket-tui-profile-field");
        let config = temp.path().join("config.json");
        fs::write(
            &config,
            r#"{"version":1,"servers":{"memory":{"enabled":true,"transport":"http","url":"https://example.test/mcp","profiles":{"work":{}}}}}"#,
        )
        .unwrap();

        let mut app = App::new();
        app.tab = Tab::Servers;
        app.server_profile_server = Some("memory".to_owned());
        app.server_profiles = list_server_profiles(&config).unwrap();
        app.selected = selected_profile_header_position(&app, "memory", "work").unwrap();

        handle_action(&mut app, Action::Enable, &config).await;
        for ch in "secret".chars() {
            handle_text_input_key(&mut app, KeyCode::Char(ch), &config).await;
        }
        handle_text_input_key(&mut app, KeyCode::Enter, &config).await;

        assert!(app.text_input.is_none());
        let updated: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            updated["servers"]["memory"]["profiles"]["work"]["headers"]["x-api-key"].as_str(),
            Some("secret")
        );
    }

    #[tokio::test]
    async fn text_input_updates_default_server_field() {
        let temp = TempDir::new("mcpocket-tui-default-field");
        let config = temp.path().join("config.json");
        fs::write(
            &config,
            r#"{"version":1,"servers":{"memory":{"enabled":true,"transport":"http","url":"https://example.test/mcp"}}}"#,
        )
        .unwrap();

        let mut app = App::new();
        app.tab = Tab::Servers;
        app.server_profile_server = Some("memory".to_owned());
        app.server_profiles = list_server_profiles(&config).unwrap();
        app.selected = 2; // url field: default row, transport field, url field

        handle_action(&mut app, Action::Enable, &config).await;
        while app.text_input_value_len() > app.text_input_prefix_len() {
            handle_text_input_key(&mut app, KeyCode::Backspace, &config).await;
        }
        for ch in "https://new.example/mcp".chars() {
            handle_text_input_key(&mut app, KeyCode::Char(ch), &config).await;
        }
        handle_text_input_key(&mut app, KeyCode::Enter, &config).await;

        let updated: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            updated["servers"]["memory"]["url"].as_str(),
            Some("https://new.example/mcp")
        );
    }

    #[tokio::test]
    async fn text_input_prefills_existing_secret_value_for_edit() {
        let temp = TempDir::new("mcpocket-tui-prefill-secret");
        let config = temp.path().join("config.json");
        fs::write(
            &config,
            r#"{"version":1,"servers":{"memory":{"enabled":true,"transport":"http","url":"https://example.test/mcp","profiles":{"work":{"headers":{"x-api-key":"existing-secret"}}}}}}"#,
        )
        .unwrap();

        let mut app = App::new();
        app.tab = Tab::Servers;
        app.server_profile_server = Some("memory".to_owned());
        app.server_profiles = list_server_profiles(&config).unwrap();
        app.selected = selected_profile_header_position(&app, "memory", "work").unwrap() + 1;

        handle_action(&mut app, Action::Enable, &config).await;

        assert_eq!(
            app.text_input.as_ref().map(|input| input.value.as_str()),
            Some("header:x-api-key=existing-secret")
        );
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
