use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde_json::{Map, Value, json};

use crate::config::{
    CURRENT_VERSION, ServerConfig, load_config, redact_command_args, redact_value,
    split_exposed_name,
};

#[derive(Debug, Clone)]
pub struct ConfigListRow {
    pub name: String,
    pub enabled: bool,
    pub gateway_enabled: bool,
    pub transport: &'static str,
    pub details: String,
    pub allowed_tools: usize,
    pub denied_tools: usize,
}

#[derive(Debug, Clone)]
pub struct ServerProfileListRow {
    pub name: String,
    pub active_profile: Option<String>,
    pub default_fields: Vec<ServerProfileFieldRow>,
    pub profiles: Vec<String>,
    pub profile_fields: BTreeMap<String, Vec<ServerProfileFieldRow>>,
    pub profile_details: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerProfileFieldRow {
    pub field: String,
    pub value: String,
    pub raw_value: String,
}

pub fn list_config(path: &Path) -> anyhow::Result<Vec<ConfigListRow>> {
    let config = load_config(path)?;
    Ok(config.servers.values().map(row_from_server).collect())
}

pub fn list_server_profiles(path: &Path) -> anyhow::Result<Vec<ServerProfileListRow>> {
    let root = read_config_value(path)?;
    let servers = root
        .get("servers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    Ok(servers
        .iter()
        .map(|(name, server)| {
            let active_profile = server
                .get("active_profile")
                .and_then(Value::as_str)
                .map(str::to_owned);
            let profiles = server
                .get("profiles")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let profile_details = profiles
                .iter()
                .map(|(profile, value)| (profile.clone(), profile_detail(value)))
                .collect();
            let profile_fields = profiles
                .iter()
                .map(|(profile, value)| (profile.clone(), profile_fields(value)))
                .collect();
            ServerProfileListRow {
                name: name.clone(),
                active_profile,
                default_fields: default_profile_fields(server),
                profiles: profiles.keys().cloned().collect::<Vec<_>>(),
                profile_fields,
                profile_details,
            }
        })
        .collect())
}

pub fn set_server_enabled(path: &Path, name: &str, enabled: bool) -> anyhow::Result<()> {
    edit_config(path, |root| {
        let server = server_object_mut(root, name)?;
        server.insert("enabled".to_owned(), Value::Bool(enabled));
        if enabled {
            gateway_object_mut(server).insert("enabled".to_owned(), Value::Bool(true));
        }
        Ok(())
    })
}

pub fn set_server_active_profile(
    path: &Path,
    name: &str,
    profile: Option<&str>,
) -> anyhow::Result<()> {
    edit_config(path, |root| {
        let server = server_object_mut(root, name)?;
        match profile {
            Some(profile) => {
                let exists = server
                    .get("profiles")
                    .and_then(Value::as_object)
                    .is_some_and(|profiles| profiles.contains_key(profile));
                if !exists {
                    anyhow::bail!("unknown profile \"{profile}\" for MCP server \"{name}\"");
                }
                server.insert(
                    "active_profile".to_owned(),
                    Value::String(profile.to_owned()),
                );
            }
            None => {
                server.remove("active_profile");
            }
        }
        Ok(())
    })
}

pub fn create_server_profile(path: &Path, name: &str, profile: &str) -> anyhow::Result<()> {
    let profile = profile.trim();
    if profile.is_empty() {
        anyhow::bail!("profile name cannot be empty");
    }
    edit_config(path, |root| {
        let server = server_object_mut(root, name)?;
        let profiles = profiles_object_mut(server);
        profiles
            .entry(profile.to_owned())
            .or_insert_with(|| json!({}));
        Ok(())
    })
}

pub fn set_server_profile_field(
    path: &Path,
    name: &str,
    profile: &str,
    field: &str,
    value: &str,
) -> anyhow::Result<()> {
    edit_config(path, |root| {
        let server = server_object_mut(root, name)?;
        let profiles = profiles_object_mut(server);
        let Some(profile_value) = profiles.get_mut(profile) else {
            anyhow::bail!("unknown profile \"{profile}\" for MCP server \"{name}\"");
        };
        ensure_object(profile_value);
        let profile_object = profile_value.as_object_mut().expect("profile object");

        set_parameter_field(profile_object, field, value, false)?;

        Ok(())
    })
}

pub fn set_server_field(path: &Path, name: &str, field: &str, value: &str) -> anyhow::Result<()> {
    edit_config(path, |root| {
        let server = server_object_mut(root, name)?;
        set_parameter_field(server, field, value, true)
    })
}

pub fn set_server_bearer_token(path: &Path, name: &str, token: &str) -> anyhow::Result<()> {
    edit_config(path, |root| {
        let server = server_object_mut(root, name)?;
        object_field_mut(server, "headers").insert(
            "Authorization".to_owned(),
            Value::String(format!("Bearer {token}")),
        );
        Ok(())
    })
}

pub fn allow_tool(path: &Path, exposed_tool: &str) -> anyhow::Result<()> {
    edit_tool_policy(path, exposed_tool, "allow_tools", "deny_tools")
}

pub fn allow_tools(path: &Path, exposed_tools: &[String]) -> anyhow::Result<usize> {
    edit_tools_policy(path, exposed_tools, "allow_tools", "deny_tools")
}

pub fn deny_tool(path: &Path, exposed_tool: &str) -> anyhow::Result<()> {
    edit_tool_policy(path, exposed_tool, "deny_tools", "allow_tools")
}

fn edit_tool_policy(
    path: &Path,
    exposed_tool: &str,
    add_field: &str,
    remove_field: &str,
) -> anyhow::Result<()> {
    edit_tools_policy(path, &[exposed_tool.to_owned()], add_field, remove_field).map(|_| ())
}

fn edit_tools_policy(
    path: &Path,
    exposed_tools: &[String],
    add_field: &str,
    remove_field: &str,
) -> anyhow::Result<usize> {
    let mut by_server = BTreeMap::<&str, Vec<&str>>::new();
    for exposed_tool in exposed_tools {
        let (server_name, _) = split_exposed_name(exposed_tool)?;
        by_server
            .entry(server_name)
            .or_default()
            .push(exposed_tool.as_str());
    }

    let mut changed = 0usize;
    edit_config(path, |root| {
        for (server_name, tools) in by_server {
            let server = server_object_mut(root, server_name)?;
            let gateway = gateway_object_mut(server);
            for exposed_tool in tools {
                array_remove(
                    gateway.entry(remove_field).or_insert_with(|| json!([])),
                    exposed_tool,
                );
                if array_add(
                    gateway.entry(add_field).or_insert_with(|| json!([])),
                    exposed_tool,
                ) {
                    changed += 1;
                }
            }
        }
        Ok(())
    })?;
    Ok(changed)
}

fn row_from_server(server: &ServerConfig) -> ConfigListRow {
    ConfigListRow {
        name: server.name.clone(),
        enabled: server.enabled,
        gateway_enabled: server.gateway.enabled,
        transport: server.transport_name(),
        details: server.redacted_details(),
        allowed_tools: server.gateway.allow_tools.len(),
        denied_tools: server.gateway.deny_tools.len(),
    }
}

fn set_parameter_field(
    object: &mut Map<String, Value>,
    field: &str,
    value: &str,
    allow_transport: bool,
) -> anyhow::Result<()> {
    if field == "url" || field == "command" || (allow_transport && field == "transport") {
        object.insert(field.to_owned(), Value::String(value.to_owned()));
    } else if field == "args" {
        let args = value
            .split_whitespace()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        object.insert(field.to_owned(), json!(args));
    } else if let Some(header) = field.strip_prefix("header:") {
        if header.trim().is_empty() {
            anyhow::bail!("header field must be formatted as header:<name>");
        }
        object_field_mut(object, "headers")
            .insert(header.to_owned(), Value::String(value.to_owned()));
    } else if let Some(env) = field.strip_prefix("env:") {
        if env.trim().is_empty() {
            anyhow::bail!("env field must be formatted as env:<name>");
        }
        object_field_mut(object, "env").insert(env.to_owned(), Value::String(value.to_owned()));
    } else {
        let transport = if allow_transport { ", transport" } else { "" };
        anyhow::bail!(
            "unsupported profile field \"{field}\"; use url, command, args{transport}, header:<name>, or env:<name>"
        );
    }

    Ok(())
}

fn profile_detail(value: &Value) -> String {
    profile_fields(value)
        .into_iter()
        .map(|row| format!("{}={}", row.field, row.value))
        .collect::<Vec<_>>()
        .join(" ")
}

fn default_profile_fields(server: &Value) -> Vec<ServerProfileFieldRow> {
    let mut rows = Vec::new();
    if let Some(transport) = server.get("transport").and_then(Value::as_str) {
        rows.push(ServerProfileFieldRow {
            field: "transport".to_owned(),
            value: transport.to_owned(),
            raw_value: transport.to_owned(),
        });
    }
    rows.extend(profile_fields(server));
    rows
}

fn profile_fields(value: &Value) -> Vec<ServerProfileFieldRow> {
    let Some(profile) = value.as_object() else {
        return Vec::new();
    };

    let mut rows = Vec::new();
    if let Some(url) = profile.get("url").and_then(Value::as_str) {
        rows.push(ServerProfileFieldRow {
            field: "url".to_owned(),
            value: url.to_owned(),
            raw_value: url.to_owned(),
        });
    }
    if let Some(headers) = profile.get("headers").and_then(Value::as_object) {
        for (key, value) in headers {
            if let Some(value) = value.as_str() {
                rows.push(ServerProfileFieldRow {
                    field: format!("header:{key}"),
                    value: redact_value(value),
                    raw_value: value.to_owned(),
                });
            }
        }
    }
    if let Some(command) = profile.get("command").and_then(Value::as_str) {
        rows.push(ServerProfileFieldRow {
            field: "command".to_owned(),
            value: command.to_owned(),
            raw_value: command.to_owned(),
        });
    }
    if let Some(args) = profile.get("args").and_then(Value::as_array) {
        let args = args
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if !args.is_empty() {
            rows.push(ServerProfileFieldRow {
                field: "args".to_owned(),
                value: redact_command_args(&args).join(" "),
                raw_value: args.join(" "),
            });
        }
    }
    if let Some(env) = profile.get("env").and_then(Value::as_object) {
        for (key, value) in env {
            if let Some(value) = value.as_str() {
                rows.push(ServerProfileFieldRow {
                    field: format!("env:{key}"),
                    value: redact_value(value),
                    raw_value: value.to_owned(),
                });
            }
        }
    }

    rows
}

fn edit_config<F>(path: &Path, edit: F) -> anyhow::Result<()>
where
    F: FnOnce(&mut Value) -> anyhow::Result<()>,
{
    let original = read_config_value(path)?;
    let mut next = original.clone();
    edit(&mut next)?;

    if next == original {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        fs::write(
            backup_path(path),
            serde_json::to_string_pretty(&original)? + "\n",
        )
        .with_context(|| format!("write backup for {}", path.display()))?;
    }
    fs::write(path, serde_json::to_string_pretty(&next)? + "\n")
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

fn read_config_value(path: &Path) -> anyhow::Result<Value> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "version": CURRENT_VERSION,
                "servers": {}
            }));
        }
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };
    let mut value: Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    ensure_object(&mut value);
    ensure_servers_object(&mut value);
    Ok(value)
}

fn server_object_mut<'a>(
    root: &'a mut Value,
    name: &str,
) -> anyhow::Result<&'a mut Map<String, Value>> {
    let servers = ensure_servers_object(root);
    let Some(server) = servers.get_mut(name) else {
        anyhow::bail!("unknown MCP server \"{}\" in config", name);
    };
    ensure_object(server);
    Ok(server.as_object_mut().expect("server object"))
}

fn gateway_object_mut(server: &mut Map<String, Value>) -> &mut Map<String, Value> {
    let gateway = server.entry("gateway").or_insert_with(|| json!({}));
    ensure_object(gateway);
    gateway.as_object_mut().expect("gateway object")
}

fn profiles_object_mut(server: &mut Map<String, Value>) -> &mut Map<String, Value> {
    object_field_mut(server, "profiles")
}

fn object_field_mut<'a>(
    object: &'a mut Map<String, Value>,
    field: &str,
) -> &'a mut Map<String, Value> {
    let value = object.entry(field.to_owned()).or_insert_with(|| json!({}));
    ensure_object(value);
    value.as_object_mut().expect("object field")
}

fn ensure_servers_object(root: &mut Value) -> &mut Map<String, Value> {
    ensure_object(root);
    let object = root.as_object_mut().expect("root object");
    object
        .entry("version")
        .or_insert_with(|| json!(CURRENT_VERSION));
    let servers = object.entry("servers").or_insert_with(|| json!({}));
    ensure_object(servers);
    servers.as_object_mut().expect("servers object")
}

fn ensure_object(value: &mut Value) {
    if !value.is_object() {
        *value = json!({});
    }
}

fn array_add(value: &mut Value, item: &str) -> bool {
    if !value.is_array() {
        *value = json!([]);
    }
    let array = value.as_array_mut().expect("array");
    if !array.iter().any(|existing| existing.as_str() == Some(item)) {
        array.push(Value::String(item.to_owned()));
        true
    } else {
        false
    }
}

fn array_remove(value: &mut Value, item: &str) {
    if let Some(array) = value.as_array_mut() {
        array.retain(|existing| existing.as_str() != Some(item));
    }
}

fn backup_path(path: &Path) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    path.with_extension(format!("{}.bak", stamp))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn enables_and_disables_server() {
        let temp = TempDir::new("mcpocket-config-edit-enable");
        let config = temp.path().join("config.json");
        fs::write(
            &config,
            r#"{"version":1,"servers":{"memory-module":{"enabled":false,"transport":"http","url":"https://example.test/mcp","gateway":{"enabled":false}}}}"#,
        )
        .unwrap();

        set_server_enabled(&config, "memory-module", true).unwrap();
        let enabled: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(enabled["servers"]["memory-module"]["enabled"], true);
        assert_eq!(
            enabled["servers"]["memory-module"]["gateway"]["enabled"],
            true
        );

        set_server_enabled(&config, "memory-module", false).unwrap();
        let disabled: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(disabled["servers"]["memory-module"]["enabled"], false);
    }

    #[test]
    fn allow_and_deny_tool_update_policy_arrays() {
        let temp = TempDir::new("mcpocket-config-edit-policy");
        let config = temp.path().join("config.json");
        fs::write(
            &config,
            r#"{"version":1,"servers":{"github":{"enabled":true,"transport":"http","url":"https://example.test/mcp","gateway":{"allow_tools":["github__old"],"deny_tools":["github__create_issue"]}}}}"#,
        )
        .unwrap();

        allow_tool(&config, "github__create_issue").unwrap();
        let allowed: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert!(
            allowed["servers"]["github"]["gateway"]["allow_tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == "github__create_issue")
        );
        assert!(
            !allowed["servers"]["github"]["gateway"]["deny_tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == "github__create_issue")
        );

        deny_tool(&config, "github__create_issue").unwrap();
        let denied: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert!(
            denied["servers"]["github"]["gateway"]["deny_tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == "github__create_issue")
        );
        assert!(
            !denied["servers"]["github"]["gateway"]["allow_tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item == "github__create_issue")
        );
    }

    #[test]
    fn allow_tools_updates_policy_arrays_in_bulk() {
        let temp = TempDir::new("mcpocket-config-edit-bulk-policy");
        let config = temp.path().join("config.json");
        fs::write(
            &config,
            r#"{"version":1,"servers":{"github":{"enabled":true,"transport":"http","url":"https://example.test/mcp","gateway":{"allow_tools":["github__existing"],"deny_tools":["github__create_issue","github__merge_pr"]}}}}"#,
        )
        .unwrap();

        let changed = allow_tools(
            &config,
            &[
                "github__create_issue".to_owned(),
                "github__merge_pr".to_owned(),
                "github__existing".to_owned(),
            ],
        )
        .unwrap();

        let updated: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(changed, 2);
        for tool in [
            "github__create_issue",
            "github__merge_pr",
            "github__existing",
        ] {
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

    #[test]
    fn lists_and_selects_server_profiles() {
        let temp = TempDir::new("mcpocket-config-edit-profiles");
        let config = temp.path().join("config.json");
        fs::write(
            &config,
            r#"{"version":1,"servers":{"memory":{"enabled":true,"transport":"http","url":"https://example.test/mcp","active_profile":"work","profiles":{"personal":{"headers":{"x-api-key":"one"}},"work":{"headers":{"x-api-key":"two"}}}}}}"#,
        )
        .unwrap();

        let rows = list_server_profiles(&config).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "memory");
        assert_eq!(rows[0].active_profile.as_deref(), Some("work"));
        assert_eq!(rows[0].profiles, ["personal", "work"]);
        assert_eq!(rows[0].profile_details["work"], "header:x-api-key=***");
        assert!(
            rows[0]
                .default_fields
                .iter()
                .any(|row| row.field == "url" && row.value == "https://example.test/mcp")
        );
        assert_eq!(
            rows[0].profile_fields["work"],
            [ServerProfileFieldRow {
                field: "header:x-api-key".to_owned(),
                value: "***".to_owned(),
                raw_value: "two".to_owned()
            }]
        );

        set_server_active_profile(&config, "memory", Some("personal")).unwrap();
        let selected: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            selected["servers"]["memory"]["active_profile"].as_str(),
            Some("personal")
        );

        set_server_active_profile(&config, "memory", None).unwrap();
        let base: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert!(base["servers"]["memory"].get("active_profile").is_none());
    }

    #[test]
    fn creates_profile_and_sets_supported_fields() {
        let temp = TempDir::new("mcpocket-config-edit-profile-fields");
        let config = temp.path().join("config.json");
        fs::write(
            &config,
            r#"{"version":1,"servers":{"memory":{"enabled":true,"transport":"http","url":"https://example.test/mcp"}}}"#,
        )
        .unwrap();

        create_server_profile(&config, "memory", "work").unwrap();
        set_server_profile_field(&config, "memory", "work", "header:x-api-key", "secret").unwrap();
        set_server_profile_field(&config, "memory", "work", "url", "https://work.example/mcp")
            .unwrap();
        set_server_field(&config, "memory", "transport", "http").unwrap();

        let updated: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            updated["servers"]["memory"]["transport"].as_str(),
            Some("http")
        );
        assert_eq!(
            updated["servers"]["memory"]["profiles"]["work"]["headers"]["x-api-key"].as_str(),
            Some("secret")
        );
        assert_eq!(
            updated["servers"]["memory"]["profiles"]["work"]["url"].as_str(),
            Some("https://work.example/mcp")
        );
    }

    #[test]
    fn stores_bearer_token_in_http_headers() {
        let temp = TempDir::new("mcpocket-config-edit-bearer-token");
        let config = temp.path().join("config.json");
        fs::write(
            &config,
            r#"{"version":1,"servers":{"plane":{"enabled":true,"transport":"http","url":"https://example.test/mcp"}}}"#,
        )
        .unwrap();

        set_server_bearer_token(&config, "plane", "access-token").unwrap();

        let updated: Value = serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            updated["servers"]["plane"]["headers"]["Authorization"].as_str(),
            Some("Bearer access-token")
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
