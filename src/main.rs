use std::io::{self, IsTerminal};
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use mcpocket::client_sync::{GatewaySyncOptions, sync_gateway};
use mcpocket::config::{default_config_path, load_config};
use mcpocket::config_edit::{allow_tool, deny_tool, list_config, set_server_enabled};
use mcpocket::doctor::{CheckStatus, run_doctor};
use mcpocket::mcp::serve_stdio;
use mcpocket::policy::PolicyDecision;
use mcpocket::router::{GatewayRouter, ToolInspectServer};
use mcpocket::upstream::{StatusRow, UpstreamStatus};

#[derive(Debug, Parser)]
#[command(name = "mcpocket")]
#[command(about = "MCP gateway and client config manager")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the stdio MCP gateway.
    Serve {
        /// Path to ~/.mcpocket/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Sync client configs to point at the mcpocket gateway.
    Sync {
        /// Write a single mcpocket gateway entry instead of direct upstream entries.
        #[arg(long)]
        gateway: bool,
        /// Comma-separated targets: claude,codex,opencode.
        #[arg(long, default_value = "claude,codex,opencode")]
        to: String,
        /// Show changes without writing files.
        #[arg(long)]
        dry_run: bool,
        /// Path to ~/.mcpocket/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Check configured gateway upstreams.
    Status {
        /// Path to ~/.mcpocket/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// List configured MCP upstreams without contacting them.
    List {
        /// Path to ~/.mcpocket/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Enable an MCP upstream in the gateway config.
    Enable {
        /// Server name from ~/.mcpocket/config.json.
        name: String,
        /// Path to ~/.mcpocket/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Disable an MCP upstream in the gateway config.
    Disable {
        /// Server name from ~/.mcpocket/config.json.
        name: String,
        /// Path to ~/.mcpocket/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Always expose a hidden gateway tool.
    AllowTool {
        /// Gateway tool name formatted as server__tool.
        tool: String,
        /// Path to ~/.mcpocket/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Always hide a gateway tool.
    DenyTool {
        /// Gateway tool name formatted as server__tool.
        tool: String,
        /// Path to ~/.mcpocket/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Show tools exposed or hidden by gateway policy.
    Tools {
        /// Optional server name to inspect.
        name: Option<String>,
        /// Path to ~/.mcpocket/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Check local mcpocket installation and client config health.
    Doctor {
        /// Path to ~/.mcpocket/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Launch the interactive terminal dashboard.
    Tui {
        /// Path to ~/.mcpocket/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let cli = Cli::parse();
    match cli.command.unwrap_or(Command::Tui { config: None }) {
        Command::Serve { config } => {
            let config_path = config.unwrap_or_else(default_config_path);
            let config = load_config(&config_path)
                .with_context(|| format!("failed to load {}", config_path.display()))?;
            let router = GatewayRouter::new(config)?;
            serve_stdio(router, &config_path).await
        }
        Command::Sync {
            gateway,
            to,
            dry_run,
            config,
        } => {
            if !gateway {
                anyhow::bail!("Rust mcpocket sync currently supports only --gateway");
            }
            let config_path = config.unwrap_or_else(default_config_path);
            let targets = parse_targets(&to)?;
            let results = sync_gateway(GatewaySyncOptions {
                config_path,
                targets,
                dry_run,
            })
            .await?;
            for result in results {
                let verb = if result.dry_run {
                    "would write"
                } else {
                    "wrote"
                };
                println!(
                    "{}: {} mcpocket -> {}",
                    result.target,
                    verb,
                    result.path.display()
                );
            }
            Ok(())
        }
        Command::Status { config } => {
            let config_path = config.unwrap_or_else(default_config_path);
            let config = load_config(&config_path)
                .with_context(|| format!("failed to load {}", config_path.display()))?;
            let router = GatewayRouter::new(config)?;
            let rows = router.status().await;
            print_status(&config_path, &rows);
            Ok(())
        }
        Command::List { config } => {
            let config_path = config.unwrap_or_else(default_config_path);
            let rows = list_config(&config_path)
                .with_context(|| format!("failed to load {}", config_path.display()))?;
            if rows.is_empty() {
                println!("No MCP upstreams in {}.", config_path.display());
                return Ok(());
            }

            println!(
                "{:<20} {:<8} {:<8} {:<8} {:<12} DETAILS",
                "NAME", "ON", "GATEWAY", "TYPE", "POLICY"
            );
            for row in rows {
                println!(
                    "{:<20} {:<8} {:<8} {:<8} +{:<2} / -{:<2}  {}",
                    row.name,
                    yes_no(row.enabled),
                    yes_no(row.gateway_enabled),
                    row.transport,
                    row.allowed_tools,
                    row.denied_tools,
                    row.details
                );
            }
            Ok(())
        }
        Command::Enable { name, config } => {
            let config_path = config.unwrap_or_else(default_config_path);
            set_server_enabled(&config_path, &name, true)?;
            println!("Enabled {name} in {}", config_path.display());
            Ok(())
        }
        Command::Disable { name, config } => {
            let config_path = config.unwrap_or_else(default_config_path);
            set_server_enabled(&config_path, &name, false)?;
            println!("Disabled {name} in {}", config_path.display());
            Ok(())
        }
        Command::AllowTool { tool, config } => {
            let config_path = config.unwrap_or_else(default_config_path);
            allow_tool(&config_path, &tool)?;
            println!("Allowed {tool} in {}", config_path.display());
            Ok(())
        }
        Command::DenyTool { tool, config } => {
            let config_path = config.unwrap_or_else(default_config_path);
            deny_tool(&config_path, &tool)?;
            println!("Denied {tool} in {}", config_path.display());
            Ok(())
        }
        Command::Tools { name, config } => {
            let config_path = config.unwrap_or_else(default_config_path);
            let config = load_config(&config_path)
                .with_context(|| format!("failed to load {}", config_path.display()))?;
            let router = GatewayRouter::new(config)?;
            let rows = router.inspect_tools(name.as_deref()).await;
            print_tools(&config_path, name.as_deref(), &rows);
            Ok(())
        }
        Command::Doctor { config } => {
            let config_path = config.unwrap_or_else(default_config_path);
            print_doctor(&config_path);
            Ok(())
        }
        Command::Tui { config } => {
            let config_path = config.unwrap_or_else(default_config_path);
            mcpocket::tui::run_tui(config_path).await
        }
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}

fn parse_targets(value: &str) -> anyhow::Result<Vec<String>> {
    const TARGETS: &[&str] = &["claude", "codex", "opencode"];
    let mut targets = Vec::new();
    for raw in value.split(',') {
        let target = raw.trim();
        if target.is_empty() {
            continue;
        }
        if !TARGETS.contains(&target) {
            anyhow::bail!(
                "unsupported target \"{}\"; use claude,codex,opencode",
                target
            );
        }
        targets.push(target.to_owned());
    }
    if targets.is_empty() {
        anyhow::bail!("at least one target is required");
    }
    Ok(targets)
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn print_status(config_path: &std::path::Path, rows: &[StatusRow]) {
    println!("Gateway: {}", config_path.display());
    if rows.is_empty() {
        println!("No enabled gateway upstreams.");
        return;
    }

    let healthy = rows
        .iter()
        .filter(|row| row.status == UpstreamStatus::Reachable)
        .collect::<Vec<_>>();
    let attention = rows
        .iter()
        .filter(|row| row.status != UpstreamStatus::Reachable)
        .collect::<Vec<_>>();

    if !healthy.is_empty() {
        println!();
        println!("{}", style("Healthy", Style::Bold));
        print_status_rows(&healthy);
    }
    if !attention.is_empty() {
        println!();
        println!("{}", style("Needs attention", Style::Bold));
        print_status_rows(&attention);
    }
}

fn print_status_rows(rows: &[&StatusRow]) {
    println!(
        "{:<6} {:<20} {:<8} {:<11} {:<12} DETAILS",
        "STATE", "NAME", "TYPE", "TOOLS", "LATENCY"
    );
    for row in rows {
        let state = match row.status {
            UpstreamStatus::Loading => style("LOAD", Style::Yellow),
            UpstreamStatus::Reachable => style("OK", Style::Green),
            UpstreamStatus::AuthMissing => style("AUTH", Style::Yellow),
            UpstreamStatus::Unreachable => style("FAIL", Style::Red),
        };
        let tools = match (row.exposed_tools, row.upstream_tools) {
            (Some(exposed), Some(total)) => format!("{exposed}/{total}"),
            _ => "-".to_owned(),
        };
        println!(
            "{:<6} {:<20} {:<8} {:<11} {:<12} {}",
            state,
            row.name,
            row.transport,
            tools,
            format!("{}ms", row.duration_ms),
            row.details
        );
    }
}

fn print_tools(config_path: &std::path::Path, filter: Option<&str>, rows: &[ToolInspectServer]) {
    println!("Gateway: {}", config_path.display());
    if rows.is_empty() {
        if let Some(filter) = filter {
            println!("No enabled upstream named {filter}.");
        } else {
            println!("No enabled gateway upstreams.");
        }
        return;
    }

    for server in rows {
        println!();
        println!(
            "{} {} ({})",
            style("MCP", Style::Bold),
            server.name,
            server.transport
        );
        if let Some(error) = &server.error {
            println!(
                "  {} {}",
                style("FAIL", Style::Red),
                first_error_line(error)
            );
            continue;
        }
        if server.tools.is_empty() {
            println!("  No tools returned by upstream.");
            continue;
        }
        println!("{:<8} {:<36} REASON", "POLICY", "TOOL");
        for tool in &server.tools {
            let policy = match tool.decision {
                PolicyDecision::Allow => style("ALLOW", Style::Green),
                PolicyDecision::Deny => style("HIDE", Style::Yellow),
            };
            println!(
                "{:<8} {:<36} {}",
                policy,
                tool.exposed_name,
                tool.reason.label()
            );
        }
    }
}

fn print_doctor(config_path: &std::path::Path) {
    println!("Gateway: {}", config_path.display());
    println!();
    println!("{}", style("Doctor", Style::Bold));
    for check in run_doctor(config_path) {
        let label = match check.status {
            CheckStatus::Ok => style(check.status.label(), Style::Green),
            CheckStatus::Warn => style(check.status.label(), Style::Yellow),
            CheckStatus::Fail => style(check.status.label(), Style::Red),
        };
        println!("{:<6} {:<22} {}", label, check.name, check.detail);
    }
}

fn first_error_line(error: &str) -> &str {
    error.lines().next().unwrap_or(error)
}

enum Style {
    Bold,
    Green,
    Yellow,
    Red,
}

fn style(text: &str, style: Style) -> String {
    if !io::stdout().is_terminal() {
        return text.to_owned();
    }
    let code = match style {
        Style::Bold => "1",
        Style::Green => "32",
        Style::Yellow => "33",
        Style::Red => "31",
    };
    format!("\x1b[{code}m{text}\x1b[0m")
}
