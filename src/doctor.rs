use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use directories::BaseDirs;
use serde_json::Value;

use crate::config::load_config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
}

impl CheckStatus {
    pub fn label(self) -> &'static str {
        match self {
            CheckStatus::Ok => "OK",
            CheckStatus::Warn => "WARN",
            CheckStatus::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DoctorCheck {
    pub status: CheckStatus,
    pub name: String,
    pub detail: String,
}

pub fn run_doctor(config_path: &Path) -> Vec<DoctorCheck> {
    let mut checks = Vec::new();
    checks.push(check_path_install());
    checks.push(check_config(config_path));

    if let Some(dirs) = BaseDirs::new() {
        let home = dirs.home_dir();
        checks.push(check_json_client(
            "Claude Code config",
            &home.join(".claude.json"),
            "mcpServers",
        ));
        checks.push(check_codex_client(&home.join(".codex").join("config.toml")));
        checks.push(check_json_client(
            "opencode config",
            &home.join(".config").join("opencode").join("opencode.json"),
            "mcp",
        ));
        checks.push(check_json_client(
            "Cursor config",
            &home.join(".cursor").join("mcp.json"),
            "mcpServers",
        ));
    } else {
        checks.push(DoctorCheck {
            status: CheckStatus::Warn,
            name: "Client config scan".to_owned(),
            detail: "could not resolve home directory".to_owned(),
        });
    }

    checks
}

fn check_path_install() -> DoctorCheck {
    match find_in_path("mcpocket") {
        Some(path) => DoctorCheck {
            status: CheckStatus::Ok,
            name: "mcpocket command".to_owned(),
            detail: path.display().to_string(),
        },
        None => DoctorCheck {
            status: CheckStatus::Warn,
            name: "mcpocket command".to_owned(),
            detail: "not found in PATH".to_owned(),
        },
    }
}

fn check_config(config_path: &Path) -> DoctorCheck {
    match load_config(config_path) {
        Ok(config) => DoctorCheck {
            status: CheckStatus::Ok,
            name: "Gateway config".to_owned(),
            detail: format!(
                "{} server(s) in {}",
                config.servers.len(),
                config_path.display()
            ),
        },
        Err(error) => DoctorCheck {
            status: CheckStatus::Fail,
            name: "Gateway config".to_owned(),
            detail: format!("{error:#}"),
        },
    }
}

fn check_json_client(name: &str, path: &Path, field: &str) -> DoctorCheck {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DoctorCheck {
                status: CheckStatus::Warn,
                name: name.to_owned(),
                detail: format!("missing {}", path.display()),
            };
        }
        Err(error) => {
            return DoctorCheck {
                status: CheckStatus::Fail,
                name: name.to_owned(),
                detail: format!("read {}: {error}", path.display()),
            };
        }
    };

    let parsed = serde_json::from_str::<Value>(&raw)
        .with_context(|| format!("parse {}", path.display()))
        .and_then(|value| {
            value
                .as_object()
                .and_then(|root| root.get(field))
                .and_then(Value::as_object)
                .cloned()
                .context("MCP server map missing")
        });

    match parsed {
        Ok(servers) => {
            let has_gateway = servers.contains_key("mcpocket");
            let has_memory = servers.contains_key("memory-module");
            client_check_result(name, path, has_gateway, has_memory)
        }
        Err(error) => DoctorCheck {
            status: CheckStatus::Fail,
            name: name.to_owned(),
            detail: format!("{error:#}"),
        },
    }
}

fn check_codex_client(path: &Path) -> DoctorCheck {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return DoctorCheck {
                status: CheckStatus::Warn,
                name: "Codex config".to_owned(),
                detail: format!("missing {}", path.display()),
            };
        }
        Err(error) => {
            return DoctorCheck {
                status: CheckStatus::Fail,
                name: "Codex config".to_owned(),
                detail: format!("read {}: {error}", path.display()),
            };
        }
    };
    let has_gateway =
        raw.contains("[mcp_servers.mcpocket]") || raw.contains("[mcp_servers.\"mcpocket\"]");
    let has_memory = raw.contains("[mcp_servers.memory-module]")
        || raw.contains("[mcp_servers.\"memory-module\"]");
    client_check_result("Codex config", path, has_gateway, has_memory)
}

fn client_check_result(
    name: &str,
    path: &Path,
    has_gateway: bool,
    has_memory: bool,
) -> DoctorCheck {
    match (has_gateway, has_memory) {
        (true, false) => DoctorCheck {
            status: CheckStatus::Ok,
            name: name.to_owned(),
            detail: format!("uses mcpocket ({})", path.display()),
        },
        (true, true) => DoctorCheck {
            status: CheckStatus::Warn,
            name: name.to_owned(),
            detail: "uses mcpocket but still has direct memory-module".to_owned(),
        },
        (false, true) => DoctorCheck {
            status: CheckStatus::Warn,
            name: name.to_owned(),
            detail: "direct memory-module configured without mcpocket".to_owned(),
        },
        (false, false) => DoctorCheck {
            status: CheckStatus::Warn,
            name: name.to_owned(),
            detail: format!("mcpocket not configured ({})", path.display()),
        },
    }
}

fn find_in_path(command: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    for dir in env::split_paths(&path) {
        let candidate = dir.join(command);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
