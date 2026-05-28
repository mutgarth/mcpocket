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

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::broadcast;

/// Max events retained for replay to a newly connected TUI.
pub const RING_CAPACITY: usize = 200;
/// Broadcast channel depth. Lagging receivers drop old events; the sender
/// (the tool-call hot path) never blocks.
pub const BROADCAST_CAPACITY: usize = 256;

/// Current wall-clock time in milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Clone-cheap fan-out of telemetry events. Holds a bounded replay ring and a
/// bounded broadcast channel. `emit` is non-blocking by construction.
#[derive(Clone)]
pub struct EventBus {
    inner: Arc<BusInner>,
}

struct BusInner {
    sender: broadcast::Sender<Event>,
    ring: Mutex<VecDeque<Event>>,
    pid: u32,
    client: String,
}

impl EventBus {
    pub fn new(client: String) -> Self {
        let (sender, _) = broadcast::channel(BROADCAST_CAPACITY);
        Self {
            inner: Arc::new(BusInner {
                sender,
                ring: Mutex::new(VecDeque::with_capacity(RING_CAPACITY)),
                pid: std::process::id(),
                client,
            }),
        }
    }

    pub fn pid(&self) -> u32 {
        self.inner.pid
    }

    pub fn client(&self) -> &str {
        &self.inner.client
    }

    /// Record an event. Pushes to the replay ring, then broadcasts. Both steps
    /// are non-blocking: a full ring evicts the oldest; a full/absent broadcast
    /// drops without ever awaiting the caller.
    pub fn emit(&self, event: Event) {
        {
            let mut ring = self.inner.ring.lock().expect("ring poisoned");
            if ring.len() == RING_CAPACITY {
                ring.pop_front();
            }
            ring.push_back(event.clone());
        }
        let _ = self.inner.sender.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.inner.sender.subscribe()
    }

    pub fn snapshot(&self) -> Vec<Event> {
        self.inner
            .ring
            .lock()
            .expect("ring poisoned")
            .iter()
            .cloned()
            .collect()
    }

    pub fn hello(&self) -> Event {
        Event::Hello {
            pid: self.inner.pid,
            client: self.inner.client.clone(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}

use std::path::{Path, PathBuf};

use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;

/// The directory where serve processes place their sockets: sibling `run/` next
/// to the config file's parent (i.e. `~/.mcpocket/run`).
pub fn run_dir_for(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("run")
}

pub fn socket_file_name(pid: u32) -> String {
    format!("serve-{pid}.sock")
}

/// Owns this serve process's socket file; removes it on drop (best-effort).
pub struct SocketGuard {
    path: PathBuf,
}

impl SocketGuard {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Create the run dir (0700 on unix), bind this process's socket, and spawn an
/// accept loop. Each connection gets a `hello` frame, a replay of the ring, and
/// then a live subscription. Returns a guard that unlinks the socket on drop.
pub async fn spawn_socket_server(bus: EventBus, run_dir: PathBuf) -> anyhow::Result<SocketGuard> {
    std::fs::create_dir_all(&run_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o700));
    }

    let path = run_dir.join(socket_file_name(bus.pid()));
    let _ = std::fs::remove_file(&path); // clear a stale socket from a crashed run
    let listener = UnixListener::bind(&path)?;

    let accept_bus = bus.clone();
    tokio::spawn(async move {
        while let Ok((stream, _addr)) = listener.accept().await {
            let conn_bus = accept_bus.clone();
            tokio::spawn(async move {
                let _ = serve_connection(stream, conn_bus).await;
            });
        }
    });

    Ok(SocketGuard { path })
}

async fn serve_connection(
    mut stream: tokio::net::UnixStream,
    bus: EventBus,
) -> std::io::Result<()> {
    // Subscribe BEFORE snapshotting so no event is lost in the gap.
    let mut rx = bus.subscribe();
    write_frame(&mut stream, &bus.hello()).await?;
    for event in bus.snapshot() {
        write_frame(&mut stream, &event).await?;
    }
    loop {
        match rx.recv().await {
            Ok(event) => write_frame(&mut stream, &event).await?,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
        }
    }
    Ok(())
}

async fn write_frame(stream: &mut tokio::net::UnixStream, event: &Event) -> std::io::Result<()> {
    let mut line = serde_json::to_string(event).unwrap_or_else(|_| "{}".to_owned());
    line.push('\n');
    stream.write_all(line.as_bytes()).await
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

    #[test]
    fn ring_buffer_caps_and_evicts_oldest() {
        let bus = EventBus::new("test".to_owned());
        for i in 0..(RING_CAPACITY + 5) {
            bus.emit(sample_call(i as u64));
        }
        let snapshot = bus.snapshot();
        assert_eq!(snapshot.len(), RING_CAPACITY);
        // Oldest 5 evicted; first retained ts is 5.
        if let Event::ToolCall { ts, .. } = snapshot[0] {
            assert_eq!(ts, 5);
        } else {
            panic!("expected ToolCall");
        }
    }

    #[tokio::test]
    async fn emit_never_blocks_without_receivers() {
        let bus = EventBus::new("test".to_owned());
        // No subscribers: emit must return immediately and not error out the caller.
        for i in 0..10_000 {
            bus.emit(sample_call(i));
        }
        assert_eq!(bus.snapshot().len(), RING_CAPACITY);
    }

    #[tokio::test]
    async fn subscriber_receives_emitted_events() {
        let bus = EventBus::new("test".to_owned());
        let mut rx = bus.subscribe();
        bus.emit(sample_call(42));
        let got = rx.recv().await.unwrap();
        assert_eq!(got, sample_call(42));
    }

    fn sample_call(ts: u64) -> Event {
        Event::ToolCall {
            ts,
            pid: 1,
            client: "test".to_owned(),
            server: "github".to_owned(),
            tool: "github__x".to_owned(),
            duration_ms: 1,
            status: CallStatus::Ok,
        }
    }

    #[test]
    fn run_dir_is_sibling_of_config() {
        let cfg = std::path::Path::new("/home/u/.mcpocket/config.json");
        assert_eq!(
            run_dir_for(cfg),
            std::path::Path::new("/home/u/.mcpocket/run")
        );
    }

    #[test]
    fn socket_file_name_includes_pid() {
        assert_eq!(socket_file_name(4823), "serve-4823.sock");
    }

    #[tokio::test]
    async fn server_sends_hello_then_replay_then_live() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::net::UnixStream;

        let dir = tempdir_unique();
        let bus = EventBus::new("test".to_owned());
        bus.emit(sample_call(1)); // already in ring -> should be replayed

        let guard = spawn_socket_server(bus.clone(), dir.clone()).await.unwrap();

        let stream = UnixStream::connect(guard.path()).await.unwrap();
        let mut lines = BufReader::new(stream).lines();

        let hello: Event =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert!(matches!(hello, Event::Hello { .. }));

        let replay: Event =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(replay, sample_call(1));

        bus.emit(sample_call(2)); // live after connect
        let live: Event = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(live, sample_call(2));

        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn tempdir_unique() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("mcpocket-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&p);
        p
    }

    #[tokio::test]
    async fn raw_client_receives_tool_call_after_hello() {
        use tokio::io::{AsyncBufReadExt, BufReader};
        use tokio::net::UnixStream;

        let dir = std::env::temp_dir().join(format!("mcpocket-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let bus = EventBus::new("e2e".to_owned());
        let guard = spawn_socket_server(bus.clone(), dir.clone()).await.unwrap();

        let stream = UnixStream::connect(guard.path()).await.unwrap();
        let mut lines = BufReader::new(stream).lines();
        let _hello = lines.next_line().await.unwrap().unwrap();

        bus.emit(sample_call(99));
        let line = lines.next_line().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Event>(&line).unwrap(),
            sample_call(99)
        );

        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
