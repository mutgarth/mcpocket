use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde_json::{Map, Value, json};

use crate::config::{CURRENT_VERSION, ServerConfig, load_config, split_exposed_name};

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

pub fn list_config(path: &Path) -> anyhow::Result<Vec<ConfigListRow>> {
    let config = load_config(path)?;
    Ok(config.servers.values().map(row_from_server).collect())
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

pub fn allow_tool(path: &Path, exposed_tool: &str) -> anyhow::Result<()> {
    edit_tool_policy(path, exposed_tool, "allow_tools", "deny_tools")
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
    let (server_name, _) = split_exposed_name(exposed_tool)?;
    edit_config(path, |root| {
        let server = server_object_mut(root, server_name)?;
        let gateway = gateway_object_mut(server);
        array_remove(
            gateway.entry(remove_field).or_insert_with(|| json!([])),
            exposed_tool,
        );
        array_add(
            gateway.entry(add_field).or_insert_with(|| json!([])),
            exposed_tool,
        );
        Ok(())
    })
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

fn array_add(value: &mut Value, item: &str) {
    if !value.is_array() {
        *value = json!([]);
    }
    let array = value.as_array_mut().expect("array");
    if !array.iter().any(|existing| existing.as_str() == Some(item)) {
        array.push(Value::String(item.to_owned()));
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
