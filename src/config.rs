use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

pub const CURRENT_VERSION: u64 = 1;

#[derive(Debug, Clone, Deserialize)]
pub struct RawPocketConfig {
    #[allow(dead_code)]
    pub version: Option<u64>,
    #[serde(default)]
    pub servers: BTreeMap<String, RawServerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawServerConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub targets: BTreeMap<String, bool>,
    pub active_profile: Option<String>,
    pub profiles: Option<BTreeMap<String, RawServerProfile>>,
    pub transport: Option<String>,
    pub url: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    pub env: Option<BTreeMap<String, String>>,
    pub gateway: Option<RawGatewayConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawServerProfile {
    pub url: Option<String>,
    pub headers: Option<BTreeMap<String, String>>,
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RawGatewayConfig {
    pub enabled: Option<bool>,
    #[serde(default)]
    pub allow_tools: Vec<String>,
    #[serde(default)]
    pub deny_tools: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PocketConfig {
    #[allow(dead_code)]
    pub version: u64,
    pub servers: BTreeMap<String, ServerConfig>,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    pub name: String,
    pub enabled: bool,
    #[allow(dead_code)]
    pub targets: BTreeMap<String, bool>,
    pub transport: TransportConfig,
    pub gateway: GatewayConfig,
}

#[derive(Debug, Clone)]
pub enum TransportConfig {
    Http {
        url: String,
        headers: BTreeMap<String, String>,
    },
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct GatewayConfig {
    pub enabled: bool,
    pub allow_tools: Vec<String>,
    pub deny_tools: Vec<String>,
}

impl PocketConfig {
    pub fn active_gateway_servers(&self) -> impl Iterator<Item = &ServerConfig> {
        self.servers
            .values()
            .filter(|server| server.enabled && server.gateway.enabled)
    }
}

impl ServerConfig {
    pub fn transport_name(&self) -> &'static str {
        match self.transport {
            TransportConfig::Http { .. } => "http",
            TransportConfig::Stdio { .. } => "stdio",
        }
    }

    pub fn redacted_details(&self) -> String {
        match &self.transport {
            TransportConfig::Http { url, headers } => {
                let mut parts = vec![url.clone()];
                if !headers.is_empty() {
                    let header_list = headers
                        .iter()
                        .map(|(key, value)| format!("{key}={}", redact_value(value)))
                        .collect::<Vec<_>>()
                        .join(",");
                    parts.push(format!("headers:{header_list}"));
                }
                parts.join(" ")
            }
            TransportConfig::Stdio { command, args, env } => {
                let mut command_parts = vec![command.clone()];
                command_parts.extend(redact_command_args(args));
                let mut parts = vec![command_parts.join(" ")];
                if !env.is_empty() {
                    let env_list = env
                        .iter()
                        .map(|(key, value)| format!("{key}={}", redact_value(value)))
                        .collect::<Vec<_>>()
                        .join(",");
                    parts.push(format!("env:{env_list}"));
                }
                parts.join(" ")
            }
        }
    }
}

pub fn default_config_path() -> PathBuf {
    BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".mcpocket").join("config.json"))
        .unwrap_or_else(|| PathBuf::from(".mcpocket/config.json"))
}

pub fn load_config(path: &Path) -> anyhow::Result<PocketConfig> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return normalize_config(RawPocketConfig {
                version: Some(CURRENT_VERSION),
                servers: BTreeMap::new(),
            });
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let parsed: RawPocketConfig =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    normalize_config(parsed)
}

pub fn normalize_config(raw: RawPocketConfig) -> anyhow::Result<PocketConfig> {
    let mut servers = BTreeMap::new();
    for (name, raw_server) in raw.servers {
        validate_server_name(&name)?;
        let raw_server = apply_active_profile(&name, raw_server)?;
        let transport_name = raw_server.transport.clone().unwrap_or_else(|| {
            if raw_server.url.is_some() {
                "http".to_owned()
            } else {
                "stdio".to_owned()
            }
        });

        let transport = match transport_name.as_str() {
            "http" => TransportConfig::Http {
                url: require_non_empty(raw_server.url.as_deref(), &name, "url")?.to_owned(),
                headers: raw_server.headers.unwrap_or_default(),
            },
            "stdio" => TransportConfig::Stdio {
                command: require_non_empty(raw_server.command.as_deref(), &name, "command")?
                    .to_owned(),
                args: raw_server.args,
                env: raw_server.env.unwrap_or_default(),
            },
            other => anyhow::bail!(
                "server \"{}\" has unsupported transport \"{}\"",
                name,
                other
            ),
        };

        let raw_gateway = raw_server.gateway.unwrap_or_default();
        let gateway = GatewayConfig {
            enabled: raw_gateway.enabled.unwrap_or(raw_server.enabled),
            allow_tools: raw_gateway.allow_tools,
            deny_tools: raw_gateway.deny_tools,
        };

        servers.insert(
            name.clone(),
            ServerConfig {
                name,
                enabled: raw_server.enabled,
                targets: with_default_targets(raw_server.targets),
                transport,
                gateway,
            },
        );
    }

    Ok(PocketConfig {
        version: raw.version.unwrap_or(CURRENT_VERSION),
        servers,
    })
}

fn apply_active_profile(
    server_name: &str,
    mut raw_server: RawServerConfig,
) -> anyhow::Result<RawServerConfig> {
    let Some(active_profile) = raw_server.active_profile.clone() else {
        return Ok(raw_server);
    };
    let Some(profile) = raw_server
        .profiles
        .as_ref()
        .and_then(|profiles| profiles.get(&active_profile))
    else {
        anyhow::bail!(
            "server \"{}\" active_profile \"{}\" does not exist",
            server_name,
            active_profile
        );
    };

    if let Some(url) = &profile.url {
        raw_server.url = Some(url.clone());
    }
    if let Some(headers) = &profile.headers {
        raw_server
            .headers
            .get_or_insert_with(BTreeMap::new)
            .extend(headers.clone());
    }
    if let Some(command) = &profile.command {
        raw_server.command = Some(command.clone());
    }
    if let Some(args) = &profile.args {
        raw_server.args = args.clone();
    }
    if let Some(env) = &profile.env {
        raw_server
            .env
            .get_or_insert_with(BTreeMap::new)
            .extend(env.clone());
    }

    Ok(raw_server)
}

pub fn exposed_name(server: &str, item: &str) -> String {
    format!("{server}__{item}")
}

pub fn split_exposed_name(name: &str) -> anyhow::Result<(&str, &str)> {
    let Some((server, item)) = name.split_once("__") else {
        anyhow::bail!(
            "gateway name \"{}\" must be formatted as server__name",
            name
        );
    };
    if server.is_empty() || item.is_empty() {
        anyhow::bail!(
            "gateway name \"{}\" must be formatted as server__name",
            name
        );
    }
    Ok((server, item))
}

pub fn redact_value(value: &str) -> String {
    if value.is_empty() {
        String::new()
    } else {
        "***".to_owned()
    }
}

pub fn redact_command_args(args: &[String]) -> Vec<String> {
    let mut redacted = Vec::with_capacity(args.len());
    let mut redact_next = false;
    for arg in args {
        if redact_next {
            redacted.push("***".to_owned());
            redact_next = false;
            continue;
        }
        let lower = arg.to_ascii_lowercase();
        if matches!(
            lower.as_str(),
            "--api-key"
                | "-api-key"
                | "--apikey"
                | "-apikey"
                | "--key"
                | "-key"
                | "--token"
                | "-token"
                | "--secret"
                | "-secret"
                | "--password"
                | "-password"
        ) {
            redacted.push(arg.clone());
            redact_next = true;
        } else if lower.contains("api-key=")
            || lower.contains("apikey=")
            || lower.contains("token=")
            || lower.contains("secret=")
            || lower.contains("password=")
        {
            let prefix = arg.split_once('=').map(|(prefix, _)| prefix).unwrap_or(arg);
            redacted.push(format!("{prefix}=***"));
        } else {
            redacted.push(arg.clone());
        }
    }
    redacted
}

fn validate_server_name(name: &str) -> anyhow::Result<()> {
    if name.trim().is_empty() {
        anyhow::bail!("server names cannot be empty");
    }
    if name.contains("__") {
        anyhow::bail!(
            "server \"{}\" is invalid: names cannot contain \"__\" because gateway tools use server__tool",
            name
        );
    }
    Ok(())
}

fn require_non_empty<'a>(
    value: Option<&'a str>,
    server: &str,
    field: &str,
) -> anyhow::Result<&'a str> {
    match value {
        Some(value) if !value.trim().is_empty() => Ok(value),
        _ => anyhow::bail!(
            "server \"{}\" is missing required field \"{}\"",
            server,
            field
        ),
    }
}

fn with_default_targets(mut targets: BTreeMap<String, bool>) -> BTreeMap<String, bool> {
    for target in ["claude", "codex", "opencode"] {
        targets.entry(target.to_owned()).or_insert(true);
    }
    targets
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_existing_sync_config_and_gateway_fields() {
        let raw: RawPocketConfig = serde_json::from_str(
            r#"{
              "version": 1,
              "servers": {
                "github": {
                  "enabled": true,
                  "transport": "http",
                  "url": "https://example.test/mcp",
                  "headers": { "Authorization": "Bearer secret" },
                  "gateway": {
                    "allow_tools": ["github__create_issue"],
                    "deny_tools": ["github__delete_repo"]
                  }
                },
                "local": {
                  "enabled": false,
                  "command": "node",
                  "args": ["server.js"]
                }
              }
            }"#,
        )
        .unwrap();
        let config = normalize_config(raw).unwrap();
        assert_eq!(config.servers["github"].transport_name(), "http");
        assert!(config.servers["github"].gateway.enabled);
        assert!(!config.servers["local"].gateway.enabled);
    }

    #[test]
    fn rejects_names_with_gateway_separator() {
        let raw: RawPocketConfig = serde_json::from_str(
            r#"{"servers":{"bad__name":{"command":"node","args":["server.js"]}}}"#,
        )
        .unwrap();
        let error = normalize_config(raw).unwrap_err().to_string();
        assert!(error.contains("cannot contain"));
    }

    #[test]
    fn active_http_profile_overrides_headers() {
        let raw: RawPocketConfig = serde_json::from_str(
            r#"{
              "servers": {
                "memory": {
                  "transport": "http",
                  "url": "https://api.example/mcp",
                  "headers": { "x-api-key": "base" },
                  "active_profile": "work",
                  "profiles": {
                    "work": {
                      "headers": { "x-api-key": "work-key", "x-account": "work" }
                    }
                  }
                }
              }
            }"#,
        )
        .unwrap();

        let config = normalize_config(raw).unwrap();
        let crate::config::TransportConfig::Http { headers, .. } =
            &config.servers["memory"].transport
        else {
            panic!("expected http transport");
        };
        assert_eq!(headers["x-api-key"], "work-key");
        assert_eq!(headers["x-account"], "work");
    }

    #[test]
    fn active_stdio_profile_overrides_env_and_args() {
        let raw: RawPocketConfig = serde_json::from_str(
            r#"{
              "servers": {
                "github": {
                  "transport": "stdio",
                  "command": "github-mcp",
                  "args": ["--account", "base"],
                  "env": { "GITHUB_TOKEN": "base" },
                  "active_profile": "personal",
                  "profiles": {
                    "personal": {
                      "args": ["--account", "personal"],
                      "env": { "GITHUB_TOKEN": "personal-token" }
                    }
                  }
                }
              }
            }"#,
        )
        .unwrap();

        let config = normalize_config(raw).unwrap();
        let crate::config::TransportConfig::Stdio { args, env, .. } =
            &config.servers["github"].transport
        else {
            panic!("expected stdio transport");
        };
        assert_eq!(args, &["--account", "personal"]);
        assert_eq!(env["GITHUB_TOKEN"], "personal-token");
    }

    #[test]
    fn active_profile_must_exist() {
        let raw: RawPocketConfig = serde_json::from_str(
            r#"{"servers":{"github":{"command":"node","args":["server.js"],"active_profile":"missing","profiles":{}}}}"#,
        )
        .unwrap();
        let error = normalize_config(raw).unwrap_err().to_string();
        assert!(error.contains("active_profile"));
    }

    #[test]
    fn maps_gateway_names_by_first_separator() {
        assert_eq!(
            exposed_name("github", "create_issue"),
            "github__create_issue"
        );
        assert_eq!(
            split_exposed_name("github__create__issue").unwrap(),
            ("github", "create__issue")
        );
    }

    #[test]
    fn redacts_secret_args() {
        let args = vec![
            "--api-key=secret".to_owned(),
            "--token".to_owned(),
            "abc".to_owned(),
        ];
        assert_eq!(
            redact_command_args(&args),
            ["--api-key=***", "--token", "***"]
        );
    }
}
