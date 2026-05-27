use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::config::{ServerConfig, TransportConfig};
use crate::policy::{PolicyDecision, decide_tool};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const STATUS_TIMEOUT: Duration = Duration::from_secs(5);
const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpstreamStatus {
    Reachable,
    Unreachable,
    AuthMissing,
}

impl std::fmt::Display for UpstreamStatus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpstreamStatus::Reachable => write!(formatter, "reachable"),
            UpstreamStatus::Unreachable => write!(formatter, "unreachable"),
            UpstreamStatus::AuthMissing => write!(formatter, "auth-missing"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct StatusRow {
    pub name: String,
    pub transport: &'static str,
    pub status: UpstreamStatus,
    pub duration_ms: u128,
    pub exposed_tools: Option<usize>,
    pub upstream_tools: Option<usize>,
    pub details: String,
}

pub struct UpstreamHandle {
    config: ServerConfig,
    state: Mutex<Option<ConnectedUpstream>>,
}

enum ConnectedUpstream {
    Stdio(StdioClient),
    Http(HttpClient),
}

impl UpstreamHandle {
    pub fn new(config: ServerConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            state: Mutex::new(None),
        })
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    pub async fn list_tools(&self) -> anyhow::Result<Vec<Value>> {
        self.list_tools_with_timeout(REQUEST_TIMEOUT).await
    }

    pub async fn list_tools_with_timeout(
        &self,
        request_timeout: Duration,
    ) -> anyhow::Result<Vec<Value>> {
        let result = self
            .request("tools/list", json!({}), request_timeout)
            .await
            .context("tools/list failed")?;
        Ok(result
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    pub async fn call_tool(&self, name: &str, arguments: Option<Value>) -> anyhow::Result<Value> {
        self.request(
            "tools/call",
            json!({
                "name": name,
                "arguments": arguments.unwrap_or_else(|| json!({}))
            }),
            REQUEST_TIMEOUT,
        )
        .await
        .with_context(|| format!("tools/call {name} failed"))
    }

    pub async fn status(&self) -> StatusRow {
        self.status_with_timeout(STATUS_TIMEOUT).await
    }

    async fn status_with_timeout(&self, status_timeout: Duration) -> StatusRow {
        let started = std::time::Instant::now();
        let details = redacted_details(&self.config);
        let result = match timeout(
            status_timeout,
            self.request("tools/list", json!({}), status_timeout),
        )
        .await
        {
            Ok(result) => result,
            Err(error) => Err(anyhow!(
                "status timed out after {:?}: {}",
                status_timeout,
                error
            )),
        };
        let duration_ms = started.elapsed().as_millis();
        let tool_counts = result.as_ref().ok().map(|value| {
            let tools = value
                .get("tools")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let exposed = tools
                .iter()
                .filter(|tool| decide_tool(&self.config, tool) == PolicyDecision::Allow)
                .count();
            (exposed, tools.len())
        });
        let status = match result {
            Ok(_) => UpstreamStatus::Reachable,
            Err(error) if is_auth_error(&error) => UpstreamStatus::AuthMissing,
            Err(_) => UpstreamStatus::Unreachable,
        };
        StatusRow {
            name: self.config.name.clone(),
            transport: self.config.transport_name(),
            status,
            duration_ms,
            exposed_tools: tool_counts.map(|(exposed, _)| exposed),
            upstream_tools: tool_counts.map(|(_, total)| total),
            details,
        }
    }

    async fn request(
        &self,
        method: &str,
        params: Value,
        request_timeout: Duration,
    ) -> anyhow::Result<Value> {
        let mut state = self.state.lock().await;
        if state.is_none() {
            *state = Some(timeout(request_timeout, connect(&self.config)).await??);
        }

        let request_result = match state.as_mut().expect("state initialized") {
            ConnectedUpstream::Stdio(client) => {
                timeout(request_timeout, client.request(method, params)).await?
            }
            ConnectedUpstream::Http(client) => {
                timeout(request_timeout, client.request(method, params)).await?
            }
        };

        match request_result {
            Ok(value) => Ok(value),
            Err(error) => {
                if matches!(state.as_ref(), Some(ConnectedUpstream::Stdio(_))) {
                    *state = None;
                }
                Err(error)
            }
        }
    }
}

async fn connect(config: &ServerConfig) -> anyhow::Result<ConnectedUpstream> {
    match &config.transport {
        TransportConfig::Stdio { command, args, env } => {
            let mut client = StdioClient::spawn(command, args, env).await?;
            client.initialize().await?;
            Ok(ConnectedUpstream::Stdio(client))
        }
        TransportConfig::Http { url, headers } => Ok(ConnectedUpstream::Http(HttpClient::new(
            url.clone(),
            headers.clone(),
        )?)),
    }
}

struct StdioClient {
    child: Child,
    stdout: BufReader<tokio::process::ChildStdout>,
    stdin: ChildStdin,
    next_id: u64,
}

impl StdioClient {
    async fn spawn(
        command: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> anyhow::Result<Self> {
        let mut process = Command::new(command);
        process.args(args);
        process.envs(env);
        process.stdin(Stdio::piped());
        process.stdout(Stdio::piped());
        process.stderr(Stdio::null());
        process.kill_on_drop(true);

        let mut child = process
            .spawn()
            .with_context(|| format!("spawn upstream command {command}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("upstream stdout was not piped"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("upstream stdin was not piped"))?;

        Ok(Self {
            child,
            stdout: BufReader::new(stdout),
            stdin,
            next_id: 1,
        })
    }

    async fn initialize(&mut self) -> anyhow::Result<()> {
        let _ = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "mcpocket", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;
        self.notify("notifications/initialized", json!({})).await
    }

    async fn notify(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        self.write_message(&message).await
    }

    async fn request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let message = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_message(&message).await?;

        loop {
            let mut line = String::new();
            let bytes = self.stdout.read_line(&mut line).await?;
            if bytes == 0 {
                let status = self.child.try_wait()?;
                anyhow::bail!("upstream closed stdout; child status: {:?}", status);
            }
            let value: Value = serde_json::from_str(line.trim())
                .with_context(|| format!("parse upstream response to {method}"))?;
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = value.get("error") {
                anyhow::bail!("{}", format_json_rpc_error(error));
            }
            return Ok(value.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    async fn write_message(&mut self, message: &Value) -> anyhow::Result<()> {
        let mut bytes = serde_json::to_vec(message)?;
        bytes.push(b'\n');
        self.stdin.write_all(&bytes).await?;
        self.stdin.flush().await?;
        Ok(())
    }
}

struct HttpClient {
    client: reqwest::Client,
    url: String,
    headers: HeaderMap,
    session_id: Option<HeaderValue>,
    initialized: bool,
    next_id: u64,
}

impl HttpClient {
    fn new(url: String, configured_headers: BTreeMap<String, String>) -> anyhow::Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        for (key, value) in configured_headers {
            headers.insert(
                HeaderName::from_bytes(key.as_bytes())
                    .with_context(|| format!("invalid HTTP header name {key}"))?,
                HeaderValue::from_str(&value)
                    .with_context(|| format!("invalid HTTP header value for {key}"))?,
            );
        }

        Ok(Self {
            client: reqwest::Client::new(),
            url,
            headers,
            session_id: None,
            initialized: false,
            next_id: 1,
        })
    }

    async fn request(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        if !self.initialized && method != "initialize" {
            self.initialize().await?;
        }

        let id = self.next_id;
        self.next_id += 1;
        self.send_json_rpc(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await
    }

    async fn initialize(&mut self) -> anyhow::Result<()> {
        let id = self.next_id;
        self.next_id += 1;
        let result = self
            .send_json_rpc(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "initialize",
                "params": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "mcpocket", "version": env!("CARGO_PKG_VERSION") }
                }
            }))
            .await;

        match result {
            Ok(_) => {
                self.send_notification("notifications/initialized", json!({}))
                    .await?;
                self.initialized = true;
                Ok(())
            }
            Err(error) if error.to_string().contains("method not found") => {
                self.initialized = true;
                Ok(())
            }
            Err(error) => Err(error).context("HTTP MCP initialize failed"),
        }
    }

    async fn send_notification(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        let response = self
            .client
            .post(&self.url)
            .headers(self.request_headers())
            .json(&json!({
                "jsonrpc": "2.0",
                "method": method,
                "params": params
            }))
            .send()
            .await?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            anyhow::bail!("auth-missing: HTTP {status}");
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("HTTP {status}: {}", truncate(&body));
        }
        Ok(())
    }

    async fn send_json_rpc(&mut self, payload: Value) -> anyhow::Result<Value> {
        let response = self
            .client
            .post(&self.url)
            .headers(self.request_headers())
            .json(&payload)
            .send()
            .await?;

        let status = response.status();
        if let Some(session_id) = response.headers().get("mcp-session-id") {
            self.session_id = Some(session_id.clone());
        }
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let body = response.text().await?;
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            anyhow::bail!("auth-missing: HTTP {status}");
        }
        if !status.is_success() {
            anyhow::bail!("HTTP {status}: {}", truncate(&body));
        }

        let value = if content_type.contains("text/event-stream") || body.starts_with("event:") {
            parse_sse_json(&body)?
        } else {
            serde_json::from_str::<Value>(&body).context("parse HTTP MCP JSON response")?
        };
        if let Some(error) = value.get("error") {
            anyhow::bail!("{}", format_json_rpc_error(error));
        }
        Ok(value.get("result").cloned().unwrap_or(Value::Null))
    }

    fn request_headers(&self) -> HeaderMap {
        let mut headers = self.headers.clone();
        if let Some(session_id) = &self.session_id {
            headers.insert(
                HeaderName::from_static("mcp-session-id"),
                session_id.clone(),
            );
        }
        headers
    }
}

fn redacted_details(config: &ServerConfig) -> String {
    config.redacted_details()
}

fn parse_sse_json(body: &str) -> anyhow::Result<Value> {
    for line in body.lines() {
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        let data = data.trim();
        if data == "[DONE]" || data.is_empty() {
            continue;
        }
        return serde_json::from_str(data).context("parse SSE data JSON");
    }
    anyhow::bail!("SSE response did not contain JSON data")
}

fn format_json_rpc_error(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| error.to_string())
}

fn truncate(value: &str) -> String {
    const LIMIT: usize = 240;
    if value.len() <= LIMIT {
        value.to_owned()
    } else {
        format!("{}...", &value[..LIMIT])
    }
}

fn is_auth_error(error: &anyhow::Error) -> bool {
    error.to_string().contains("auth-missing")
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::config::GatewayConfig;

    #[tokio::test]
    async fn status_times_out_hanging_stdio_initialize() {
        let handle = UpstreamHandle::new(ServerConfig {
            name: "silent".to_owned(),
            enabled: true,
            targets: BTreeMap::new(),
            transport: TransportConfig::Stdio {
                command: "sh".to_owned(),
                args: vec![
                    "-c".to_owned(),
                    "while IFS= read -r _line; do sleep 1000; done".to_owned(),
                ],
                env: BTreeMap::new(),
            },
            gateway: GatewayConfig {
                enabled: true,
                allow_tools: Vec::new(),
                deny_tools: Vec::new(),
            },
        });

        let started = Instant::now();
        let row = handle.status_with_timeout(Duration::from_millis(100)).await;

        assert_eq!(row.status, UpstreamStatus::Unreachable);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "status should not wait for the upstream initialize timeout"
        );
    }
}
