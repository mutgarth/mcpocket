use serde::{Deserialize, Serialize};

/// A telemetry frame emitted by a `serve` process over its Unix socket.
/// Serialized as newline-delimited JSON (one frame per line).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    /// First frame sent on connect: identifies the serve process.
    Hello {
        pid: u32,
        client: String,
        version: String,
    },
    /// A completed tool call routed through the gateway.
    ToolCall {
        ts: u64,
        pid: u32,
        client: String,
        server: String,
        tool: String,
        duration_ms: u64,
        status: CallStatus,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallStatus {
    Ok,
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_call_event_round_trips_as_jsonl() {
        let event = Event::ToolCall {
            ts: 1_716_800_000_123,
            pid: 4823,
            client: "claude".to_owned(),
            server: "github".to_owned(),
            tool: "github__search_repos".to_owned(),
            duration_ms: 180,
            status: CallStatus::Ok,
        };
        let line = serde_json::to_string(&event).unwrap();
        assert!(line.contains("\"kind\":\"tool_call\""));
        assert!(!line.contains('\n'));
        let decoded: Event = serde_json::from_str(&line).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn hello_event_round_trips() {
        let event = Event::Hello {
            pid: 1,
            client: "codex".to_owned(),
            version: "0.1.0".to_owned(),
        };
        let decoded: Event = serde_json::from_str(&serde_json::to_string(&event).unwrap()).unwrap();
        assert_eq!(decoded, event);
    }
}
