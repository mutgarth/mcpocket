use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use directories::BaseDirs;
use serde_json::{Value, json};

#[derive(Debug)]
pub struct GatewaySyncOptions {
    pub config_path: PathBuf,
    pub targets: Vec<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub struct GatewaySyncResult {
    pub target: String,
    pub path: PathBuf,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
struct GatewayEntry {
    command: String,
    args: Vec<String>,
}

pub async fn sync_gateway(options: GatewaySyncOptions) -> anyhow::Result<Vec<GatewaySyncResult>> {
    let paths = target_paths()?;
    let entry = GatewayEntry {
        command: std::env::current_exe()
            .context("resolve current executable")?
            .display()
            .to_string(),
        args: vec![
            "serve".to_owned(),
            "--config".to_owned(),
            options.config_path.display().to_string(),
        ],
    };

    let mut results = Vec::new();
    for target in options.targets {
        let path = match target.as_str() {
            "claude" => paths.claude.clone(),
            "codex" => paths.codex.clone(),
            "opencode" => paths.opencode.clone(),
            _ => anyhow::bail!("unsupported target \"{}\"", target),
        };

        if !options.dry_run {
            let existing = read_optional(&path)?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            if !existing.is_empty() {
                write_backup(&path, &existing)?;
            }
            let next = match target.as_str() {
                "claude" => write_claude(&existing, &entry)?,
                "codex" => write_codex(&existing, &entry),
                "opencode" => write_opencode(&existing, &entry)?,
                _ => unreachable!(),
            };
            fs::write(&path, next)?;
        }

        results.push(GatewaySyncResult {
            target,
            path,
            dry_run: options.dry_run,
        });
    }
    Ok(results)
}

struct TargetPaths {
    claude: PathBuf,
    codex: PathBuf,
    opencode: PathBuf,
}

fn target_paths() -> anyhow::Result<TargetPaths> {
    let dirs = BaseDirs::new().context("could not find home directory")?;
    let home = dirs.home_dir();
    Ok(TargetPaths {
        claude: home.join(".claude.json"),
        codex: home.join(".codex").join("config.toml"),
        opencode: home.join(".config").join("opencode").join("opencode.json"),
    })
}

fn read_optional(path: &Path) -> anyhow::Result<String> {
    match fs::read_to_string(path) {
        Ok(value) => Ok(value),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn write_claude(existing: &str, entry: &GatewayEntry) -> anyhow::Result<String> {
    let mut parsed = parse_json_object(existing)?;
    let mcp_servers = parsed
        .as_object_mut()
        .expect("object")
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    if !mcp_servers.is_object() {
        *mcp_servers = json!({});
    }
    mcp_servers.as_object_mut().expect("object").insert(
        "mcpocket".to_owned(),
        json!({
            "type": "stdio",
            "command": entry.command,
            "args": entry.args,
        }),
    );
    Ok(format!("{}\n", serde_json::to_string_pretty(&parsed)?))
}

fn write_opencode(existing: &str, entry: &GatewayEntry) -> anyhow::Result<String> {
    let mut parsed = parse_json_object(&strip_json_comments(existing))?;
    let mcp = parsed
        .as_object_mut()
        .expect("object")
        .entry("mcp")
        .or_insert_with(|| json!({}));
    if !mcp.is_object() {
        *mcp = json!({});
    }
    let command: Vec<Value> = std::iter::once(Value::String(entry.command.clone()))
        .chain(entry.args.iter().cloned().map(Value::String))
        .collect();
    mcp.as_object_mut().expect("object").insert(
        "mcpocket".to_owned(),
        json!({
            "type": "local",
            "command": command,
            "enabled": true,
        }),
    );
    Ok(format!("{}\n", serde_json::to_string_pretty(&parsed)?))
}

fn write_codex(existing: &str, entry: &GatewayEntry) -> String {
    let preserved = strip_codex_mcpocket_block(existing).trim_end().to_owned();
    let block = [
        "# Managed by mcpocket gateway".to_owned(),
        "[mcp_servers.mcpocket]".to_owned(),
        format!("command = {}", toml_string(&entry.command)),
        format!("args = {}", toml_array(&entry.args)),
    ]
    .join("\n");

    if preserved.is_empty() {
        format!("{block}\n")
    } else {
        format!("{preserved}\n\n{block}\n")
    }
}

fn strip_codex_mcpocket_block(existing: &str) -> String {
    let mut kept = Vec::new();
    let mut skipping = false;
    for line in existing.lines() {
        if is_toml_table(line) {
            skipping = is_mcpocket_table(line);
        }
        if !skipping {
            kept.push(line);
        }
    }
    kept.join("\n")
}

fn is_toml_table(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with('[') && trimmed.ends_with(']')
}

fn is_mcpocket_table(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == "[mcp_servers.mcpocket]" || trimmed == "[mcp_servers.\"mcpocket\"]"
}

fn parse_json_object(raw: &str) -> anyhow::Result<Value> {
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    let value: Value = serde_json::from_str(raw)?;
    if value.is_object() {
        Ok(value)
    } else {
        Ok(json!({}))
    }
}

fn strip_json_comments(raw: &str) -> String {
    let mut output = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if escaped {
            escaped = false;
            output.push(ch);
            continue;
        }
        if ch == '\\' && in_string {
            escaped = true;
            output.push(ch);
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            output.push(ch);
            continue;
        }
        if !in_string && ch == '/' && chars.peek() == Some(&'/') {
            for next in chars.by_ref() {
                if next == '\n' {
                    output.push('\n');
                    break;
                }
            }
            continue;
        }
        output.push(ch);
    }
    output
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("string serialization cannot fail")
}

fn toml_array(values: &[String]) -> String {
    format!(
        "[ {} ]",
        values
            .iter()
            .map(|value| toml_string(value))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn write_backup(path: &Path, content: &str) -> anyhow::Result<()> {
    if content.is_empty() {
        return Ok(());
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    fs::write(path.with_extension(format!("{}.bak", stamp)), content)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_gateway_sync_preserves_direct_servers() {
        let existing = r#"model = "gpt-5"

[mcp_servers.github]
command = "gh"
args = []

[mcp_servers.mcpocket]
command = "old"
args = []
"#;
        let next = write_codex(
            existing,
            &GatewayEntry {
                command: "/bin/mcpocket".to_owned(),
                args: vec![
                    "serve".to_owned(),
                    "--config".to_owned(),
                    "/tmp/config.json".to_owned(),
                ],
            },
        );
        assert!(next.contains("[mcp_servers.github]"));
        assert!(next.contains("[mcp_servers.mcpocket]"));
        assert!(!next.contains("command = \"old\""));
        assert!(next.contains("serve"));
    }

    #[test]
    fn claude_gateway_sync_upserts_one_server() {
        let next = write_claude(
            r#"{"mcpServers":{"github":{"command":"gh"}}}"#,
            &GatewayEntry {
                command: "/bin/mcpocket".to_owned(),
                args: vec!["serve".to_owned()],
            },
        )
        .unwrap();
        assert!(next.contains("\"github\""));
        assert!(next.contains("\"mcpocket\""));
    }
}
