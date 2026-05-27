use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

#[test]
fn proxies_memory_module_style_stdio_upstream_end_to_end() {
    let temp = TempDir::new("mcpocket-e2e-stdio");
    let upstream = temp.path().join("fake_memory_module_mcp.js");
    fs::write(&upstream, fake_memory_module_server()).unwrap();

    let config = temp.path().join("config.json");
    fs::write(
        &config,
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "servers": {
                "memory-module": {
                    "enabled": true,
                    "transport": "stdio",
                    "command": "node",
                    "args": [upstream],
                    "gateway": { "enabled": true }
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let mut gateway = GatewayProcess::spawn(&config);
    let init = gateway.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "mcpocket-e2e", "version": "0" }
        }),
    );
    assert_eq!(init["serverInfo"]["name"], "mcpocket");
    gateway.notify("notifications/initialized", json!({}));

    let listed = gateway.request("tools/list", json!({}));
    let tools = listed["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .any(|tool| tool["name"] == "memory-module__search_memories"),
        "gateway did not expose the fake memory search tool: {listed}"
    );

    let called = gateway.request(
        "tools/call",
        json!({
            "name": "memory-module__search_memories",
            "arguments": { "query": "gateway smoke" }
        }),
    );
    let text = called["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("memory ok: gateway smoke"), "{called}");
}

#[test]
#[ignore = "requires network access and a valid memory-module entry in ~/.mcpocket/config.json"]
fn live_memory_module_is_reachable_through_gateway() {
    let home_config = home_dir().join(".mcpocket").join("config.json");
    let raw = fs::read_to_string(&home_config).unwrap_or_else(|error| {
        panic!("read {}: {error}", home_config.display());
    });
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    let memory = parsed["servers"]["memory-module"].clone();
    assert!(
        memory.is_object(),
        "missing servers.memory-module in {}",
        home_config.display()
    );

    let temp = TempDir::new("mcpocket-e2e-live-memory");
    let live_config = temp.path().join("config.json");
    fs::write(
        &live_config,
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "servers": {
                "memory-module": memory
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let output = Command::new(mcpocket_bin())
        .args(["status", "--config"])
        .arg(&live_config)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "status command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("memory-module") && stdout.contains("http") && stdout.contains("reachable"),
        "memory-module was not reachable through gateway status:\n{stdout}"
    );
}

#[test]
#[ignore = "requires network access and a valid memory-module entry in ~/.mcpocket/config.json"]
fn live_memory_module_tool_call_through_gateway() {
    let home_config = home_dir().join(".mcpocket").join("config.json");
    let raw = fs::read_to_string(&home_config).unwrap_or_else(|error| {
        panic!("read {}: {error}", home_config.display());
    });
    let parsed: Value = serde_json::from_str(&raw).unwrap();
    let mut memory = parsed["servers"]["memory-module"].clone();
    assert!(
        memory.is_object(),
        "missing servers.memory-module in {}",
        home_config.display()
    );

    memory["enabled"] = json!(true);
    memory["gateway"] = json!({
        "enabled": true,
        "allow_tools": ["memory-module__search_memories"],
        "deny_tools": []
    });

    let temp = TempDir::new("mcpocket-e2e-live-memory-call");
    let live_config = temp.path().join("config.json");
    fs::write(
        &live_config,
        serde_json::to_string_pretty(&json!({
            "version": 1,
            "servers": {
                "memory-module": memory
            }
        }))
        .unwrap(),
    )
    .unwrap();

    let mut gateway = GatewayProcess::spawn(&live_config);
    let init = gateway.request(
        "initialize",
        json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "mcpocket-live-e2e", "version": "0" }
        }),
    );
    assert_eq!(init["serverInfo"]["name"], "mcpocket");
    gateway.notify("notifications/initialized", json!({}));

    let listed = gateway.request("tools/list", json!({}));
    let tools = listed["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .any(|tool| tool["name"] == "memory-module__search_memories"),
        "gateway did not expose memory-module__search_memories: {listed}"
    );

    let called = gateway.request(
        "tools/call",
        json!({
            "name": "memory-module__search_memories",
            "arguments": {
                "query": "mcpocket gateway smoke test",
                "limit": 1
            }
        }),
    );
    let content = called["content"].as_array().unwrap_or_else(|| {
        panic!("memory-module search response did not include content: {called}")
    });
    assert!(
        !content.is_empty(),
        "memory-module search returned empty content through gateway: {called}"
    );
}

struct GatewayProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    next_id: u64,
}

impl GatewayProcess {
    fn spawn(config: &Path) -> Self {
        let mut child = Command::new(mcpocket_bin())
            .args(["serve", "--config"])
            .arg(config)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.write(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }));
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.write(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }));

        loop {
            let mut line = String::new();
            let bytes = self.stdout.read_line(&mut line).unwrap();
            assert_ne!(
                bytes, 0,
                "gateway closed stdout before responding to {method}"
            );
            let value: Value = serde_json::from_str(line.trim()).unwrap();
            if value["id"] != id {
                continue;
            }
            if let Some(error) = value.get("error") {
                panic!("gateway returned error for {method}: {error}");
            }
            return value["result"].clone();
        }
    }

    fn write(&mut self, value: Value) {
        writeln!(self.stdin, "{}", serde_json::to_string(&value).unwrap()).unwrap();
        self.stdin.flush().unwrap();
    }
}

impl Drop for GatewayProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
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

fn mcpocket_bin() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_mcpocket")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/debug/mcpocket"))
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .expect("HOME is required for the live memory-module e2e test")
}

fn fake_memory_module_server() -> &'static str {
    r#"
const readline = require("node:readline");

const rl = readline.createInterface({ input: process.stdin });

function send(value) {
  process.stdout.write(`${JSON.stringify(value)}\n`);
}

rl.on("line", (line) => {
  if (!line.trim()) return;
  const message = JSON.parse(line);
  if (!Object.prototype.hasOwnProperty.call(message, "id")) return;

  if (message.method === "initialize") {
    send({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        protocolVersion: "2025-06-18",
        capabilities: { tools: {} },
        serverInfo: { name: "fake-memory-module", version: "0.0.0" }
      }
    });
    return;
  }

  if (message.method === "tools/list") {
    send({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        tools: [{
          name: "search_memories",
          description: "Search stored memories",
          inputSchema: {
            type: "object",
            properties: { query: { type: "string" } },
            required: ["query"]
          },
          annotations: { readOnlyHint: true }
        }]
      }
    });
    return;
  }

  if (message.method === "tools/call") {
    const query = message.params?.arguments?.query || "";
    send({
      jsonrpc: "2.0",
      id: message.id,
      result: {
        content: [{ type: "text", text: `memory ok: ${query}` }],
        isError: false
      }
    });
    return;
  }

  send({
    jsonrpc: "2.0",
    id: message.id,
    error: { code: -32601, message: `method not found: ${message.method}` }
  });
});
"#
}
