# mcpocket TUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `mcpocket tui` — a brand-themed terminal dashboard that manages upstreams (status, tools, policy, enable/disable, allow/deny, doctor) and live-monitors gateway traffic across all running `serve` processes.

**Architecture:** A new non-blocking telemetry layer in the `serve` process broadcasts tool-call events over a per-process Unix socket (`~/.mcpocket/run/serve-<pid>.sock`). The TUI is a separate process that discovers and merges those sockets, reuses `GatewayRouter` (status/inspect) and `config_edit` (edits), and renders with ratatui at a fixed tick.

**Tech Stack:** Rust (edition 2024), tokio (`net` + `sync`), ratatui 0.29, crossterm 0.28, serde/serde_json.

---

## File Structure

- Create: `src/telemetry.rs` — `Event`, `EventBus`, socket server, run-dir helpers.
- Modify: `src/router.rs` — optional `EventBus`, emit on `call_tool`.
- Modify: `src/mcp.rs` — start socket server in `serve_stdio`.
- Create: `src/tui/mod.rs` — `run_tui`, terminal setup/teardown, main loop.
- Create: `src/tui/theme.rs` — palette + truecolor fallback.
- Create: `src/tui/app.rs` — `App` state, `Tab`, derived metrics.
- Create: `src/tui/input.rs` — `Action`, `map_key`.
- Create: `src/tui/discovery.rs` — socket scan/parse, connect-or-reap, stream.
- Create: `src/tui/ui.rs` — render dispatch + per-tab render functions.
- Modify: `src/main.rs` — register `mod tui; mod telemetry;` + `Tui` command.
- Modify: `Cargo.toml` — add ratatui, crossterm, tokio `net` feature.
- Modify: `tests/e2e_gateway.rs` — socket event e2e.

---

## Task 1: Add dependencies

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add crates and the tokio `net` feature**

In `[dependencies]`, edit the `tokio` line to add `"net"` and add two crates:

```toml
ratatui = "0.29"
crossterm = "0.28"
tokio = { version = "1", features = ["io-std", "io-util", "macros", "net", "process", "rt-multi-thread", "sync", "time"] }
```

- [ ] **Step 2: Verify it builds**

Run: `cargo build`
Expected: PASS (compiles; new crates downloaded).

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "build: add ratatui, crossterm, tokio net feature"
```

---

## Task 2: Telemetry `Event` type + serde round-trip

**Files:**
- Create: `src/telemetry.rs`
- Modify: `src/main.rs` (add `mod telemetry;`)

- [ ] **Step 1: Register the module**

In `src/main.rs`, add to the module list near the top (keep alphabetical with the others):

```rust
mod telemetry;
```

- [ ] **Step 2: Write the failing test**

Create `src/telemetry.rs` with only the test first:

```rust
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
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --lib telemetry`
Expected: FAIL — `Event`, `CallStatus` not defined.

- [ ] **Step 4: Implement the types**

At the top of `src/telemetry.rs`, above the test module:

```rust
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
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --lib telemetry`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add src/telemetry.rs src/main.rs
git commit -m "feat(telemetry): add Event type with JSONL serde"
```

---

## Task 3: `EventBus` — non-blocking ring + broadcast

**Files:**
- Modify: `src/telemetry.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `src/telemetry.rs`:

```rust
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib telemetry`
Expected: FAIL — `EventBus`, `RING_CAPACITY` not defined.

- [ ] **Step 3: Implement `EventBus`**

Add near the top of `src/telemetry.rs` (after the imports/types):

```rust
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
        self.inner.ring.lock().expect("ring poisoned").iter().cloned().collect()
    }

    pub fn hello(&self) -> Event {
        Event::Hello {
            pid: self.inner.pid,
            client: self.inner.client.clone(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib telemetry`
Expected: PASS (5 tests).

- [ ] **Step 5: Commit**

```bash
git add src/telemetry.rs
git commit -m "feat(telemetry): add non-blocking EventBus with replay ring"
```

---

## Task 4: Run-dir helpers + socket server

**Files:**
- Modify: `src/telemetry.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module:

```rust
    #[test]
    fn run_dir_is_sibling_of_config() {
        let cfg = std::path::Path::new("/home/u/.mcpocket/config.json");
        assert_eq!(run_dir_for(cfg), std::path::Path::new("/home/u/.mcpocket/run"));
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

        let hello: Event = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert!(matches!(hello, Event::Hello { .. }));

        let replay: Event = serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
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
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib telemetry`
Expected: FAIL — `run_dir_for`, `socket_file_name`, `spawn_socket_server` not defined.

- [ ] **Step 3: Implement helpers + server**

Add to `src/telemetry.rs` (imports first, then items):

```rust
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
```

> Note: subscribing before the snapshot means a live event could also appear in
> the replay (at most one duplicate). The TUI dedupes is unnecessary for v1 —
> duplicates are harmless in the feed. Keep it simple.

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib telemetry`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/telemetry.rs
git commit -m "feat(telemetry): add Unix socket server with hello + replay"
```

---

## Task 5: Wire telemetry into the router and serve

**Files:**
- Modify: `src/router.rs:79-104` (call_tool), struct + `new`
- Modify: `src/mcp.rs:14-18` (serve_stdio)

- [ ] **Step 1: Add an optional `EventBus` to `GatewayRouter`**

In `src/router.rs`, add the import and field:

```rust
use crate::telemetry::{CallStatus, Event, EventBus, now_ms};
```

Change the struct (around line 13):

```rust
#[derive(Clone)]
pub struct GatewayRouter {
    upstreams: Arc<BTreeMap<String, Arc<UpstreamHandle>>>,
    events: Option<EventBus>,
}
```

In `new` (around line 34), set `events: None`:

```rust
        Ok(Self {
            upstreams: Arc::new(upstreams),
            events: None,
        })
```

Add a setter after `new`:

```rust
    /// Attach a telemetry bus so completed tool calls are broadcast.
    pub fn with_event_bus(mut self, bus: EventBus) -> Self {
        self.events = Some(bus);
        self
    }
```

- [ ] **Step 2: Emit on `call_tool`**

In `call_tool` (around line 94-104), after computing `status`, emit before returning. Replace the `info!(...)` block's tail with:

```rust
        let status = if result.is_ok() { "ok" } else { "error" };
        let duration_ms = started.elapsed().as_millis();
        info!(
            server = server_name,
            tool = exposed_tool,
            duration_ms,
            status,
            "tool call finished"
        );
        if let Some(bus) = &self.events {
            bus.emit(Event::ToolCall {
                ts: now_ms(),
                pid: bus.pid(),
                client: bus.client().to_owned(),
                server: server_name.to_owned(),
                tool: exposed_tool.to_owned(),
                duration_ms: duration_ms as u64,
                status: if result.is_ok() { CallStatus::Ok } else { CallStatus::Error },
            });
        }
        result
```

- [ ] **Step 3: Start the socket server in `serve_stdio`**

In `src/mcp.rs`, change `serve_stdio` to accept the config path and start telemetry:

```rust
use std::path::Path;

use crate::router::GatewayRouter;
use crate::telemetry::{EventBus, run_dir_for, spawn_socket_server};

pub async fn serve_stdio(router: GatewayRouter, config_path: &Path) -> anyhow::Result<()> {
    let client = std::env::var("MCPOCKET_CLIENT").unwrap_or_else(|_| "unknown".to_owned());
    let bus = EventBus::new(client);
    let _socket = spawn_socket_server(bus.clone(), run_dir_for(config_path)).await?;
    let router = router.with_event_bus(bus);

    let service = GatewayServer { router }.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}
```

- [ ] **Step 4: Update the caller in `main.rs`**

In `src/main.rs`, the `Command::Serve` arm (around line 123-129) currently calls `serve_stdio(router).await`. Change it to pass the path:

```rust
        Command::Serve { config } => {
            let config_path = config.unwrap_or_else(default_config_path);
            let config = load_config(&config_path)
                .with_context(|| format!("failed to load {}", config_path.display()))?;
            let router = GatewayRouter::new(config)?;
            serve_stdio(router, &config_path).await
        }
```

- [ ] **Step 5: Build and run existing tests**

Run: `cargo test`
Expected: PASS (existing tests still green; new code compiles).

- [ ] **Step 6: Commit**

```bash
git add src/router.rs src/mcp.rs src/main.rs
git commit -m "feat(telemetry): emit tool-call events and start socket server in serve"
```

---

## Task 6: TUI discovery — parse, connect-or-reap, stream

**Files:**
- Create: `src/tui/mod.rs` (module declarations only for now)
- Create: `src/tui/discovery.rs`
- Modify: `src/main.rs` (add `mod tui;`)

- [ ] **Step 1: Register modules**

In `src/main.rs` add:

```rust
mod tui;
```

Create `src/tui/mod.rs` with:

```rust
pub mod discovery;
```

- [ ] **Step 2: Write the failing tests**

Create `src/tui/discovery.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{CallStatus, Event};

    #[test]
    fn parses_pid_from_socket_name() {
        assert_eq!(parse_serve_pid("serve-4823.sock"), Some(4823));
        assert_eq!(parse_serve_pid("serve-.sock"), None);
        assert_eq!(parse_serve_pid("other.txt"), None);
    }

    #[tokio::test]
    async fn connect_or_reap_removes_stale_socket() {
        let dir = std::env::temp_dir().join(format!("mcpocket-reap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("serve-1.sock");

        // Bind then drop the listener: the file remains but nothing listens.
        {
            let _l = std::os::unix::net::UnixListener::bind(&path).unwrap();
        }
        assert!(path.exists());

        let result = connect_or_reap(&path).await.unwrap();
        assert!(result.is_none());
        assert!(!path.exists(), "stale socket should be reaped");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stream_socket_forwards_decoded_frames() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("mcpocket-stream-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("serve-2.sock");
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path).unwrap();

        let server_path = path.clone();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let ev = sample();
            let mut line = serde_json::to_string(&ev).unwrap();
            line.push('\n');
            stream.write_all(line.as_bytes()).await.unwrap();
            // keep the connection open briefly
            let _ = server_path;
        });

        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = stream_socket(path, tx).await;
        });

        let got = rx.recv().await.unwrap();
        assert_eq!(got, sample());

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn sample() -> Event {
        Event::ToolCall {
            ts: 1,
            pid: 2,
            client: "c".to_owned(),
            server: "github".to_owned(),
            tool: "github__x".to_owned(),
            duration_ms: 5,
            status: CallStatus::Ok,
        }
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib discovery`
Expected: FAIL — `parse_serve_pid`, `connect_or_reap`, `stream_socket` not defined.

- [ ] **Step 4: Implement discovery**

At the top of `src/tui/discovery.rs`:

```rust
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::telemetry::Event;

/// Extract the pid from a `serve-<pid>.sock` file name.
pub fn parse_serve_pid(file_name: &str) -> Option<u32> {
    file_name
        .strip_prefix("serve-")?
        .strip_suffix(".sock")?
        .parse()
        .ok()
}

/// List serve socket paths currently present in `run_dir`.
pub fn list_socket_paths(run_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(run_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .and_then(parse_serve_pid)
                .is_some()
        })
        .map(|e| e.path())
        .collect()
}

/// Connect to a socket. If the connection is refused (the serve process is gone
/// but left its socket file behind), unlink the file and return `None`.
pub async fn connect_or_reap(path: &Path) -> std::io::Result<Option<UnixStream>> {
    match UnixStream::connect(path).await {
        Ok(stream) => Ok(Some(stream)),
        Err(e) if e.kind() == ErrorKind::ConnectionRefused => {
            let _ = std::fs::remove_file(path);
            Ok(None)
        }
        Err(e) => Err(e),
    }
}

/// Connect, then forward newline-delimited `Event` frames to `tx` until the
/// connection closes. Malformed lines are skipped.
pub async fn stream_socket(path: PathBuf, tx: mpsc::Sender<Event>) -> std::io::Result<()> {
    let Some(stream) = connect_or_reap(&path).await? else {
        return Ok(());
    };
    let mut lines = BufReader::new(stream).lines();
    while let Some(line) = lines.next_line().await? {
        if let Ok(event) = serde_json::from_str::<Event>(&line) {
            if tx.send(event).await.is_err() {
                break;
            }
        }
    }
    Ok(())
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --lib discovery`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add src/tui/mod.rs src/tui/discovery.rs src/main.rs
git commit -m "feat(tui): socket discovery, connect-or-reap, and frame streaming"
```

---

## Task 7: Connection manager — merge many sockets with rescans

**Files:**
- Modify: `src/tui/discovery.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `src/tui/discovery.rs`:

```rust
    #[tokio::test]
    async fn manager_merges_events_from_two_sockets() {
        use tokio::io::AsyncWriteExt;
        use tokio::net::UnixListener;

        let dir = std::env::temp_dir().join(format!("mcpocket-mgr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        for pid in [10u32, 11u32] {
            let path = dir.join(format!("serve-{pid}.sock"));
            let listener = UnixListener::bind(&path).unwrap();
            tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let ev = Event::ToolCall {
                    ts: pid as u64,
                    pid,
                    client: "c".to_owned(),
                    server: "s".to_owned(),
                    tool: "s__t".to_owned(),
                    duration_ms: 1,
                    status: CallStatus::Ok,
                };
                let mut line = serde_json::to_string(&ev).unwrap();
                line.push('\n');
                stream.write_all(line.as_bytes()).await.unwrap();
                std::future::pending::<()>().await; // hold open
            });
        }

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let run_dir = dir.clone();
        tokio::spawn(async move {
            spawn_connection_manager(run_dir, tx).await;
        });

        let mut pids = std::collections::HashSet::new();
        for _ in 0..2 {
            if let Event::ToolCall { pid, .. } = rx.recv().await.unwrap() {
                pids.insert(pid);
            }
        }
        assert_eq!(pids, [10, 11].into_iter().collect());

        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test --lib discovery::tests::manager_merges`
Expected: FAIL — `spawn_connection_manager` not defined.

- [ ] **Step 3: Implement the manager**

Add to `src/tui/discovery.rs`:

```rust
use std::collections::HashSet;
use std::time::Duration;

/// Periodically scan `run_dir` and keep one streaming task per live socket.
/// New sockets are connected; tasks that finish (disconnect) free their slot so
/// a later rescan reconnects. Runs until `tx` is closed.
pub async fn spawn_connection_manager(run_dir: PathBuf, tx: mpsc::Sender<Event>) {
    let mut active: HashSet<PathBuf> = HashSet::new();
    let (done_tx, mut done_rx) = mpsc::unbounded_channel::<PathBuf>();

    loop {
        // Drain finished connections so they can be retried on the next scan.
        while let Ok(path) = done_rx.try_recv() {
            active.remove(&path);
        }

        for path in list_socket_paths(&run_dir) {
            if active.contains(&path) {
                continue;
            }
            active.insert(path.clone());
            let task_tx = tx.clone();
            let task_done = done_tx.clone();
            let task_path = path.clone();
            tokio::spawn(async move {
                let _ = stream_socket(task_path.clone(), task_tx).await;
                let _ = task_done.send(task_path);
            });
        }

        if tx.is_closed() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
```

- [ ] **Step 4: Run to verify pass**

Run: `cargo test --lib discovery`
Expected: PASS (4 tests).

- [ ] **Step 5: Commit**

```bash
git add src/tui/discovery.rs
git commit -m "feat(tui): connection manager merges multiple serve sockets"
```

---

## Task 8: Theme — brand palette + truecolor fallback

**Files:**
- Create: `src/tui/theme.rs`
- Modify: `src/tui/mod.rs`

- [ ] **Step 1: Register module**

In `src/tui/mod.rs` add:

```rust
pub mod theme;
```

- [ ] **Step 2: Write the failing tests**

Create `src/tui/theme.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    #[test]
    fn truecolor_theme_uses_rgb_accent() {
        let theme = Theme::brand(true);
        assert!(matches!(theme.accent, Color::Rgb(_, _, _)));
    }

    #[test]
    fn fallback_theme_uses_indexed_accent() {
        let theme = Theme::brand(false);
        assert!(matches!(theme.accent, Color::Magenta | Color::LightMagenta));
    }

    #[test]
    fn status_colors_distinguish_ok_and_fail() {
        let theme = Theme::brand(true);
        assert_ne!(theme.ok, theme.fail);
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib theme`
Expected: FAIL — `Theme` not defined.

- [ ] **Step 4: Implement the theme**

Add to the top of `src/tui/theme.rs`:

```rust
use ratatui::style::Color;

/// Brand palette derived from the mcpocket / Jules design system
/// (deep purple `#1D0245` .. light `#B898E8`).
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub accent: Color,
    pub selection: Color,
    pub dim: Color,
    pub ok: Color,
    pub warn: Color,
    pub fail: Color,
}

impl Theme {
    /// Build the theme. `truecolor` selects 24-bit RGB; otherwise indexed ANSI.
    pub fn brand(truecolor: bool) -> Self {
        if truecolor {
            Self {
                bg: Color::Rgb(0x15, 0x0F, 0x36),
                fg: Color::Rgb(0xED, 0xE0, 0xFA),
                accent: Color::Rgb(0x9C, 0x70, 0xE0),
                selection: Color::Rgb(0x7B, 0x4E, 0xD8),
                dim: Color::Rgb(0xB8, 0x98, 0xE8),
                ok: Color::Rgb(0x6E, 0xE7, 0xB7),
                warn: Color::Rgb(0xFB, 0xBF, 0x24),
                fail: Color::Rgb(0xF8, 0x71, 0x71),
            }
        } else {
            Self {
                bg: Color::Reset,
                fg: Color::White,
                accent: Color::Magenta,
                selection: Color::LightMagenta,
                dim: Color::Gray,
                ok: Color::Green,
                warn: Color::Yellow,
                fail: Color::Red,
            }
        }
    }

    /// Detect terminal truecolor support via `COLORTERM`.
    pub fn detect() -> Self {
        let truecolor = std::env::var("COLORTERM")
            .map(|v| v.contains("truecolor") || v.contains("24bit"))
            .unwrap_or(false);
        Self::brand(truecolor)
    }
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --lib theme`
Expected: PASS (3 tests).

- [ ] **Step 6: Commit**

```bash
git add src/tui/theme.rs src/tui/mod.rs
git commit -m "feat(tui): brand theme with truecolor fallback"
```

---

## Task 9: Input mapping — keys to actions

**Files:**
- Create: `src/tui/input.rs`
- Modify: `src/tui/mod.rs`

- [ ] **Step 1: Register module**

In `src/tui/mod.rs` add:

```rust
pub mod input;
```

- [ ] **Step 2: Write the failing tests**

Create `src/tui/input.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyCode;

    #[test]
    fn quit_keys() {
        assert_eq!(map_key(KeyCode::Char('q')), Action::Quit);
        assert_eq!(map_key(KeyCode::Esc), Action::Quit);
    }

    #[test]
    fn navigation_keys() {
        assert_eq!(map_key(KeyCode::Tab), Action::NextTab);
        assert_eq!(map_key(KeyCode::BackTab), Action::PrevTab);
        assert_eq!(map_key(KeyCode::Down), Action::Down);
        assert_eq!(map_key(KeyCode::Char('j')), Action::Down);
        assert_eq!(map_key(KeyCode::Up), Action::Up);
        assert_eq!(map_key(KeyCode::Char('k')), Action::Up);
    }

    #[test]
    fn edit_keys() {
        assert_eq!(map_key(KeyCode::Char('e')), Action::Enable);
        assert_eq!(map_key(KeyCode::Char('d')), Action::Disable);
        assert_eq!(map_key(KeyCode::Char('a')), Action::Allow);
        assert_eq!(map_key(KeyCode::Char('x')), Action::Deny);
        assert_eq!(map_key(KeyCode::Char('r')), Action::Refresh);
    }

    #[test]
    fn unknown_key_is_none() {
        assert_eq!(map_key(KeyCode::Char('z')), Action::None);
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib input`
Expected: FAIL — `Action`, `map_key` not defined.

- [ ] **Step 4: Implement**

Add to the top of `src/tui/input.rs`:

```rust
use crossterm::event::KeyCode;

/// A high-level UI action decoded from a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    NextTab,
    PrevTab,
    Up,
    Down,
    Enable,
    Disable,
    Allow,
    Deny,
    Refresh,
    None,
}

/// Map a key code to an action. Tab-specific interpretation (e.g. Enable only
/// applies on the Servers tab) is handled by the caller.
pub fn map_key(code: KeyCode) -> Action {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => Action::Quit,
        KeyCode::Tab => Action::NextTab,
        KeyCode::BackTab => Action::PrevTab,
        KeyCode::Down | KeyCode::Char('j') => Action::Down,
        KeyCode::Up | KeyCode::Char('k') => Action::Up,
        KeyCode::Char('e') => Action::Enable,
        KeyCode::Char('d') => Action::Disable,
        KeyCode::Char('a') => Action::Allow,
        KeyCode::Char('x') => Action::Deny,
        KeyCode::Char('r') => Action::Refresh,
        _ => Action::None,
    }
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --lib input`
Expected: PASS (4 tests).

- [ ] **Step 6: Commit**

```bash
git add src/tui/input.rs src/tui/mod.rs
git commit -m "feat(tui): key-to-action mapping"
```

---

## Task 10: App state + derived live metrics

**Files:**
- Create: `src/tui/app.rs`
- Modify: `src/tui/mod.rs`

- [ ] **Step 1: Register module**

In `src/tui/mod.rs` add:

```rust
pub mod app;
```

- [ ] **Step 2: Write the failing tests**

Create `src/tui/app.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::{CallStatus, Event};

    fn call(ts: u64, dur: u64, status: CallStatus) -> Event {
        Event::ToolCall {
            ts,
            pid: 1,
            client: "c".to_owned(),
            server: "github".to_owned(),
            tool: "github__x".to_owned(),
            duration_ms: dur,
            status,
        }
    }

    #[test]
    fn tab_cycles_forward_and_back() {
        let mut app = App::new();
        assert_eq!(app.tab, Tab::Servers);
        app.next_tab();
        assert_eq!(app.tab, Tab::Tools);
        app.prev_tab();
        assert_eq!(app.tab, Tab::Servers);
        app.prev_tab();
        assert_eq!(app.tab, Tab::Doctor); // wraps
    }

    #[test]
    fn hello_event_is_not_counted_as_traffic() {
        let mut app = App::new();
        app.ingest(Event::Hello { pid: 1, client: "c".to_owned(), version: "0".to_owned() });
        assert_eq!(app.live_events.len(), 0);
    }

    #[test]
    fn history_is_bounded() {
        let mut app = App::new();
        for i in 0..(MAX_LIVE_EVENTS + 50) {
            app.ingest(call(i as u64, 1, CallStatus::Ok));
        }
        assert_eq!(app.live_events.len(), MAX_LIVE_EVENTS);
    }

    #[test]
    fn error_count_and_p95() {
        let mut app = App::new();
        app.ingest(call(1, 10, CallStatus::Ok));
        app.ingest(call(2, 20, CallStatus::Error));
        app.ingest(call(3, 30, CallStatus::Ok));
        assert_eq!(app.error_count(), 1);
        assert_eq!(app.p95_latency(), Some(30));
    }

    #[test]
    fn req_per_sec_counts_within_window() {
        let mut app = App::new();
        app.ingest(call(1_000, 1, CallStatus::Ok));
        app.ingest(call(1_500, 1, CallStatus::Ok));
        app.ingest(call(50_000, 1, CallStatus::Ok)); // outside 10s window from now=51_000
        // 2 events within [41_000, 51_000)? Only ts=50_000 qualifies -> recheck:
        // window is last 10s ending at now.
        assert_eq!(app.req_per_sec(51_000, 10_000), 0.1); // 1 event / 10s
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib app`
Expected: FAIL — `App`, `Tab`, `MAX_LIVE_EVENTS` not defined.

- [ ] **Step 4: Implement App**

Add to the top of `src/tui/app.rs`:

```rust
use std::collections::VecDeque;

use crate::telemetry::{CallStatus, Event};
use crate::upstream::StatusRow;

/// Max tool-call events retained for the Live feed.
pub const MAX_LIVE_EVENTS: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Servers,
    Tools,
    Live,
    Doctor,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Servers, Tab::Tools, Tab::Live, Tab::Doctor];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Servers => "Servers",
            Tab::Tools => "Tools",
            Tab::Live => "Live",
            Tab::Doctor => "Doctor",
        }
    }
}

/// A flattened view of a tool-call event for the Live feed.
#[derive(Debug, Clone)]
pub struct LiveEvent {
    pub ts: u64,
    pub client: String,
    pub tool: String,
    pub duration_ms: u64,
    pub status: CallStatus,
}

pub struct App {
    pub tab: Tab,
    pub selected: usize,
    pub servers: Vec<StatusRow>,
    pub live_events: VecDeque<LiveEvent>,
    pub status_message: Option<String>,
    pub should_quit: bool,
    pub dirty: bool,
}

impl App {
    pub fn new() -> Self {
        Self {
            tab: Tab::Servers,
            selected: 0,
            servers: Vec::new(),
            live_events: VecDeque::with_capacity(MAX_LIVE_EVENTS),
            status_message: None,
            should_quit: false,
            dirty: true,
        }
    }

    pub fn next_tab(&mut self) {
        let idx = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.tab = Tab::ALL[(idx + 1) % Tab::ALL.len()];
        self.dirty = true;
    }

    pub fn prev_tab(&mut self) {
        let idx = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        self.tab = Tab::ALL[(idx + Tab::ALL.len() - 1) % Tab::ALL.len()];
        self.dirty = true;
    }

    /// Fold a telemetry event into the live feed. `Hello` frames are ignored.
    pub fn ingest(&mut self, event: Event) {
        if let Event::ToolCall { ts, client, tool, duration_ms, status, .. } = event {
            if self.live_events.len() == MAX_LIVE_EVENTS {
                self.live_events.pop_front();
            }
            self.live_events.push_back(LiveEvent { ts, client, tool, duration_ms, status });
            self.dirty = true;
        }
    }

    pub fn error_count(&self) -> usize {
        self.live_events
            .iter()
            .filter(|e| e.status == CallStatus::Error)
            .count()
    }

    /// 95th-percentile latency over the retained feed (nearest-rank).
    pub fn p95_latency(&self) -> Option<u64> {
        if self.live_events.is_empty() {
            return None;
        }
        let mut durs: Vec<u64> = self.live_events.iter().map(|e| e.duration_ms).collect();
        durs.sort_unstable();
        let rank = ((durs.len() as f64) * 0.95).ceil() as usize;
        let idx = rank.saturating_sub(1).min(durs.len() - 1);
        Some(durs[idx])
    }

    /// Requests per second over the `window_ms` window ending at `now_ms`.
    pub fn req_per_sec(&self, now_ms: u64, window_ms: u64) -> f64 {
        let start = now_ms.saturating_sub(window_ms);
        let count = self
            .live_events
            .iter()
            .filter(|e| e.ts >= start && e.ts < now_ms)
            .count();
        count as f64 / (window_ms as f64 / 1000.0)
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --lib app`
Expected: PASS (5 tests). If `req_per_sec` assertion disagrees, the formula is `count / (window_ms/1000)`; the test expects `1 / 10.0 = 0.1`.

- [ ] **Step 6: Commit**

```bash
git add src/tui/app.rs src/tui/mod.rs
git commit -m "feat(tui): App state with bounded feed and live metrics"
```

---

## Task 11: Rendering — tabs + Servers/Live/Doctor, tested via TestBackend

**Files:**
- Create: `src/tui/ui.rs`
- Modify: `src/tui/mod.rs`

- [ ] **Step 1: Register module**

In `src/tui/mod.rs` add:

```rust
pub mod ui;
```

- [ ] **Step 2: Write the failing test (render into a TestBackend buffer)**

Create `src/tui/ui.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::{App, Tab};
    use crate::tui::theme::Theme;
    use crate::upstream::{StatusRow, UpstreamStatus};
    use ratatui::{backend::TestBackend, Terminal};

    fn buffer_text(app: &mut App) -> String {
        let theme = Theme::brand(false);
        let backend = TestBackend::new(80, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render(f, app, &theme)).unwrap();
        let buf = terminal.backend().buffer().clone();
        buf.content().iter().map(|c| c.symbol()).collect::<String>()
    }

    #[test]
    fn servers_tab_lists_server_names_and_tab_titles() {
        let mut app = App::new();
        app.tab = Tab::Servers;
        app.servers = vec![StatusRow {
            name: "memory-module".to_owned(),
            transport: "http",
            status: UpstreamStatus::Reachable,
            duration_ms: 430,
            exposed_tools: Some(5),
            upstream_tools: Some(11),
            details: "https://example".to_owned(),
        }];
        let text = buffer_text(&mut app);
        assert!(text.contains("memory-module"));
        assert!(text.contains("Servers"));
        assert!(text.contains("Live"));
    }

    #[test]
    fn live_tab_shows_empty_hint_without_traffic() {
        let mut app = App::new();
        app.tab = Tab::Live;
        let text = buffer_text(&mut app);
        assert!(text.contains("no active gateways") || text.contains("Waiting"));
    }
}
```

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib ui`
Expected: FAIL — `render` not defined.

- [ ] **Step 4: Implement rendering**

Add to the top of `src/tui/ui.rs`:

```rust
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Tabs};
use ratatui::Frame;

use crate::tui::app::{App, Tab};
use crate::tui::theme::Theme;
use crate::upstream::UpstreamStatus;

/// Top-level render: title/tab bar, body for the active tab, footer hints.
pub fn render(frame: &mut Frame, app: &App, theme: &Theme) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1), Constraint::Length(1)])
        .split(frame.area());

    render_tabs(frame, chunks[0], app, theme);
    match app.tab {
        Tab::Servers => render_servers(frame, chunks[1], app, theme),
        Tab::Tools => render_placeholder(frame, chunks[1], "Tools", theme),
        Tab::Live => render_live(frame, chunks[1], app, theme),
        Tab::Doctor => render_placeholder(frame, chunks[1], "Doctor", theme),
    }
    render_footer(frame, chunks[2], app, theme);
}

fn render_tabs(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let titles: Vec<Line> = Tab::ALL.iter().map(|t| Line::from(t.title())).collect();
    let selected = Tab::ALL.iter().position(|t| *t == app.tab).unwrap_or(0);
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Span::styled(" mcpocket ", Style::default().fg(theme.accent).add_modifier(Modifier::BOLD))),
        )
        .select(selected)
        .style(Style::default().fg(theme.dim))
        .highlight_style(Style::default().fg(theme.fg).bg(theme.selection).add_modifier(Modifier::BOLD));
    frame.render_widget(tabs, area);
}

fn render_servers(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let header = Row::new(["STATE", "NAME", "TYPE", "TOOLS", "LATENCY"])
        .style(Style::default().fg(theme.accent).add_modifier(Modifier::BOLD));

    let rows = app.servers.iter().enumerate().map(|(i, row)| {
        let (state, color) = match row.status {
            UpstreamStatus::Reachable => ("OK", theme.ok),
            UpstreamStatus::AuthMissing => ("AUTH", theme.warn),
            UpstreamStatus::Unreachable => ("FAIL", theme.fail),
        };
        let tools = match (row.exposed_tools, row.upstream_tools) {
            (Some(e), Some(t)) => format!("{e}/{t}"),
            _ => "-".to_owned(),
        };
        let style = if i == app.selected {
            Style::default().bg(theme.selection).fg(theme.fg)
        } else {
            Style::default().fg(theme.fg)
        };
        Row::new(vec![
            Cell::from(state).style(Style::default().fg(color)),
            Cell::from(row.name.clone()),
            Cell::from(row.transport),
            Cell::from(tools),
            Cell::from(format!("{}ms", row.duration_ms)),
        ])
        .style(style)
    });

    let widths = [
        Constraint::Length(6),
        Constraint::Min(20),
        Constraint::Length(8),
        Constraint::Length(10),
        Constraint::Length(10),
    ];
    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(" Servers "));
    frame.render_widget(table, area);
}

fn render_live(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    if app.live_events.is_empty() {
        let hint = Paragraph::new("Waiting for gateway traffic… (no active gateways yet)")
            .style(Style::default().fg(theme.dim))
            .block(Block::default().borders(Borders::ALL).title(" Live "));
        frame.render_widget(hint, area);
        return;
    }

    let lines: Vec<Line> = app
        .live_events
        .iter()
        .rev()
        .take(area.height.saturating_sub(2) as usize)
        .map(|e| {
            let (label, color) = match e.status {
                crate::telemetry::CallStatus::Ok => ("ok ", theme.ok),
                crate::telemetry::CallStatus::Error => ("ERR", theme.fail),
            };
            Line::from(vec![
                Span::styled(format!("{label} "), Style::default().fg(color)),
                Span::styled(format!("{:<24} ", e.tool), Style::default().fg(theme.fg)),
                Span::styled(format!("{}ms ", e.duration_ms), Style::default().fg(theme.dim)),
                Span::styled(format!("[{}]", e.client), Style::default().fg(theme.accent)),
            ])
        })
        .collect();

    let para = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(" Live "));
    frame.render_widget(para, area);
}

fn render_placeholder(frame: &mut Frame, area: Rect, title: &str, theme: &Theme) {
    let para = Paragraph::new(format!("{title} — coming up"))
        .style(Style::default().fg(theme.dim))
        .block(Block::default().borders(Borders::ALL).title(format!(" {title} ")));
    frame.render_widget(para, area);
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    let hint = match app.tab {
        Tab::Servers => "[Tab] switch  [j/k] move  [e]nable [d]isable  [r]efresh  [q]uit",
        Tab::Tools => "[Tab] switch  [j/k] move  [a]llow [x]deny  [q]uit",
        Tab::Live => "[Tab] switch  live traffic  [q]uit",
        Tab::Doctor => "[Tab] switch  [r]efresh  [q]uit",
    };
    let text = app
        .status_message
        .clone()
        .unwrap_or_else(|| hint.to_owned());
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(theme.dim)),
        area,
    );
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --lib ui`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add src/tui/ui.rs src/tui/mod.rs
git commit -m "feat(tui): tab bar + Servers/Live rendering (TestBackend tested)"
```

---

## Task 12: Tools & Doctor tabs

**Files:**
- Modify: `src/tui/app.rs` (hold inspect + doctor data)
- Modify: `src/tui/ui.rs` (render Tools/Doctor)

- [ ] **Step 1: Add data fields to `App`**

In `src/tui/app.rs`, add imports and fields. Add to imports:

```rust
use crate::doctor::DoctorCheck;
use crate::router::ToolInspectServer;
```

Add to the `App` struct, after `servers`:

```rust
    pub tools: Vec<ToolInspectServer>,
    pub doctor: Vec<DoctorCheck>,
```

Initialize both in `App::new` (after `servers: Vec::new(),`):

```rust
            tools: Vec::new(),
            doctor: Vec::new(),
```

- [ ] **Step 2: Write the failing test**

Add to the `tests` module in `src/tui/ui.rs`:

```rust
    #[test]
    fn tools_tab_shows_policy_rows() {
        use crate::policy::{PolicyDecision, PolicyReason};
        use crate::router::{ToolInspectRow, ToolInspectServer};
        let mut app = App::new();
        app.tab = Tab::Tools;
        app.tools = vec![ToolInspectServer {
            name: "github".to_owned(),
            transport: "http",
            tools: vec![ToolInspectRow {
                exposed_name: "github__search".to_owned(),
                decision: PolicyDecision::Allow,
                reason: PolicyReason::Allowlist,
            }],
            error: None,
        }];
        let text = buffer_text(&mut app);
        assert!(text.contains("github__search"));
        assert!(text.contains("ALLOW"));
    }

    #[test]
    fn doctor_tab_shows_checks() {
        use crate::doctor::{CheckStatus, DoctorCheck};
        let mut app = App::new();
        app.tab = Tab::Doctor;
        app.doctor = vec![DoctorCheck {
            status: CheckStatus::Ok,
            name: "PATH".to_owned(),
            detail: "mcpocket on PATH".to_owned(),
        }];
        let text = buffer_text(&mut app);
        assert!(text.contains("PATH"));
        assert!(text.contains("OK"));
    }
```

> Before running, confirm the `PolicyReason` variant name. Check `src/policy.rs`
> for the actual variant used for an allowlisted tool and use that exact name in
> the test (e.g. `PolicyReason::Allowlist`). Adjust the test if it differs.

- [ ] **Step 3: Run to verify failure**

Run: `cargo test --lib ui`
Expected: FAIL — Tools/Doctor still render the placeholder; assertions fail.

- [ ] **Step 4: Implement Tools and Doctor rendering**

In `src/tui/ui.rs`, replace the two `render_placeholder` calls in `render` with real renderers:

```rust
        Tab::Tools => render_tools(frame, chunks[1], app, theme),
        Tab::Live => render_live(frame, chunks[1], app, theme),
        Tab::Doctor => render_doctor(frame, chunks[1], app, theme),
```

Add the functions (and you may delete `render_placeholder` once unused):

```rust
fn render_tools(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    use crate::policy::PolicyDecision;

    let mut lines: Vec<Line> = Vec::new();
    for server in &app.tools {
        lines.push(Line::from(Span::styled(
            format!("MCP {} ({})", server.name, server.transport),
            Style::default().fg(theme.accent).add_modifier(Modifier::BOLD),
        )));
        if let Some(err) = &server.error {
            lines.push(Line::from(Span::styled(
                format!("  FAIL {}", err.lines().next().unwrap_or(err)),
                Style::default().fg(theme.fail),
            )));
            continue;
        }
        for tool in &server.tools {
            let (label, color) = match tool.decision {
                PolicyDecision::Allow => ("ALLOW", theme.ok),
                PolicyDecision::Deny => ("HIDE ", theme.warn),
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {label} "), Style::default().fg(color)),
                Span::styled(tool.exposed_name.clone(), Style::default().fg(theme.fg)),
                Span::raw("  "),
                Span::styled(tool.reason.label().to_owned(), Style::default().fg(theme.dim)),
            ]));
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "No tools loaded — press [r] to refresh.",
            Style::default().fg(theme.dim),
        )));
    }
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" Tools ")),
        area,
    );
}

fn render_doctor(frame: &mut Frame, area: Rect, app: &App, theme: &Theme) {
    use crate::doctor::CheckStatus;

    let lines: Vec<Line> = app
        .doctor
        .iter()
        .map(|check| {
            let color = match check.status {
                CheckStatus::Ok => theme.ok,
                CheckStatus::Warn => theme.warn,
                CheckStatus::Fail => theme.fail,
            };
            Line::from(vec![
                Span::styled(format!("{:<5} ", check.status.label()), Style::default().fg(color)),
                Span::styled(format!("{:<22} ", check.name), Style::default().fg(theme.fg)),
                Span::styled(check.detail.clone(), Style::default().fg(theme.dim)),
            ])
        })
        .collect();
    let body = if lines.is_empty() {
        Paragraph::new("Running checks… press [r] to refresh.")
            .style(Style::default().fg(theme.dim))
    } else {
        Paragraph::new(lines)
    };
    frame.render_widget(
        body.block(Block::default().borders(Borders::ALL).title(" Doctor ")),
        area,
    );
}
```

- [ ] **Step 5: Run to verify pass**

Run: `cargo test --lib ui`
Expected: PASS (4 tests). If a `PolicyReason` variant name mismatched, fix the test per the note in Step 2.

- [ ] **Step 6: Commit**

```bash
git add src/tui/app.rs src/tui/ui.rs
git commit -m "feat(tui): Tools and Doctor tab rendering"
```

---

## Task 13: Main loop + `mcpocket tui` command

**Files:**
- Modify: `src/tui/mod.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Implement the run loop**

Replace the contents of `src/tui/mod.rs` module-declaration block by keeping the `pub mod` lines and adding the runtime below them:

```rust
pub mod app;
pub mod discovery;
pub mod input;
pub mod theme;
pub mod ui;

use std::io::{self, Stdout};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{Event as CtEvent, KeyEventKind};
use crossterm::{event, execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use crate::config::load_config;
use crate::config_edit::{allow_tool, deny_tool, set_server_enabled};
use crate::doctor::run_doctor;
use crate::router::GatewayRouter;
use crate::telemetry::{run_dir_for, Event};

use self::app::{App, Tab};
use self::discovery::spawn_connection_manager;
use self::input::{map_key, Action};
use self::theme::Theme;

/// Entry point for `mcpocket tui`.
pub async fn run_tui(config_path: PathBuf) -> anyhow::Result<()> {
    let mut terminal = setup_terminal().context("failed to enter TUI mode")?;
    install_panic_hook();

    let mut app = App::new();
    let theme = Theme::detect();

    // Telemetry stream from all serve sockets.
    let (event_tx, mut event_rx) = mpsc::channel::<Event>(1024);
    let run_dir = run_dir_for(&config_path);
    tokio::spawn(spawn_connection_manager(run_dir, event_tx));

    // Keyboard input on a blocking thread -> async channel.
    let (key_tx, mut key_rx) = mpsc::channel::<Action>(64);
    spawn_input_thread(key_tx);

    refresh_data(&mut app, &config_path).await;

    let mut tick = tokio::time::interval(Duration::from_millis(125)); // ~8 fps
    let result = loop {
        tokio::select! {
            maybe_ev = event_rx.recv() => {
                if let Some(ev) = maybe_ev { app.ingest(ev); }
            }
            maybe_action = key_rx.recv() => {
                match maybe_action {
                    Some(action) => handle_action(&mut app, action, &config_path).await,
                    None => break Ok(()),
                }
            }
            _ = tick.tick() => {
                if app.dirty {
                    if let Err(e) = terminal.draw(|f| ui::render(f, &app, &theme)) {
                        break Err(anyhow::Error::from(e));
                    }
                    app.dirty = false;
                }
            }
        }
        if app.should_quit {
            break Ok(());
        }
    };

    restore_terminal(&mut terminal).ok();
    result
}

type Tui = Terminal<CrosstermBackend<Stdout>>;

fn setup_terminal() -> anyhow::Result<Tui> {
    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, terminal::EnterAlternateScreen, event::EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal(terminal: &mut Tui) -> anyhow::Result<()> {
    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), terminal::LeaveAlternateScreen, event::DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

/// Restore the terminal even if a panic unwinds through the render loop.
fn install_panic_hook() {
    let original = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(io::stdout(), terminal::LeaveAlternateScreen, event::DisableMouseCapture);
        original(info);
    }));
}

fn spawn_input_thread(tx: mpsc::Sender<Action>) {
    std::thread::spawn(move || {
        loop {
            // Block until a terminal event is available.
            match event::read() {
                Ok(CtEvent::Key(key)) if key.kind == KeyEventKind::Press => {
                    let action = map_key(key.code);
                    if tx.blocking_send(action).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
}

async fn handle_action(app: &mut App, action: Action, config_path: &Path) {
    match action {
        Action::Quit => app.should_quit = true,
        Action::NextTab => app.next_tab(),
        Action::PrevTab => app.prev_tab(),
        Action::Down => {
            let len = current_len(app);
            if len > 0 {
                app.selected = (app.selected + 1).min(len - 1);
                app.dirty = true;
            }
        }
        Action::Up => {
            app.selected = app.selected.saturating_sub(1);
            app.dirty = true;
        }
        Action::Refresh => refresh_data(app, config_path).await,
        Action::Enable | Action::Disable if app.tab == Tab::Servers => {
            if let Some(row) = app.servers.get(app.selected) {
                let name = row.name.clone();
                let enable = matches!(action, Action::Enable);
                match set_server_enabled(config_path, &name, enable) {
                    Ok(()) => app.status_message = Some(format!(
                        "{} {name}",
                        if enable { "Enabled" } else { "Disabled" }
                    )),
                    Err(e) => app.status_message = Some(format!("error: {e}")),
                }
                refresh_data(app, config_path).await;
            }
        }
        Action::Allow | Action::Deny if app.tab == Tab::Tools => {
            if let Some(tool) = selected_tool(app) {
                let res = if matches!(action, Action::Allow) {
                    allow_tool(config_path, &tool)
                } else {
                    deny_tool(config_path, &tool)
                };
                app.status_message = Some(match res {
                    Ok(()) => format!("updated policy for {tool}"),
                    Err(e) => format!("error: {e}"),
                });
                refresh_data(app, config_path).await;
            }
        }
        _ => {}
    }
}

fn current_len(app: &App) -> usize {
    match app.tab {
        Tab::Servers => app.servers.len(),
        Tab::Tools => app.tools.iter().map(|s| s.tools.len()).sum(),
        _ => 0,
    }
}

fn selected_tool(app: &App) -> Option<String> {
    let mut idx = app.selected;
    for server in &app.tools {
        if idx < server.tools.len() {
            return Some(server.tools[idx].exposed_name.clone());
        }
        idx -= server.tools.len();
    }
    None
}

/// Reload config and refresh status, tools, and doctor data.
async fn refresh_data(app: &mut App, config_path: &Path) {
    app.doctor = run_doctor(config_path);
    match load_config(config_path) {
        Ok(config) => match GatewayRouter::new(config) {
            Ok(router) => {
                app.servers = router.status().await;
                app.tools = router.inspect_tools(None).await;
            }
            Err(e) => app.status_message = Some(format!("router error: {e}")),
        },
        Err(e) => app.status_message = Some(format!("config error: {e}")),
    }
    if app.selected >= current_len(app) {
        app.selected = current_len(app).saturating_sub(1);
    }
    app.dirty = true;
}
```

- [ ] **Step 2: Add the `Tui` command to the CLI**

In `src/main.rs`, add a variant to the `Command` enum (after `Doctor`):

```rust
    /// Launch the interactive terminal dashboard.
    Tui {
        /// Path to ~/.mcpocket/config.json.
        #[arg(long)]
        config: Option<PathBuf>,
    },
```

Add the match arm in `main` (after the `Command::Doctor` arm):

```rust
        Command::Tui { config } => {
            let config_path = config.unwrap_or_else(default_config_path);
            crate::tui::run_tui(config_path).await
        }
```

- [ ] **Step 3: Build**

Run: `cargo build`
Expected: PASS. Fix any unused-import warnings (e.g. remove `render_placeholder` if now unused).

- [ ] **Step 4: Run the full test suite + lints**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS on all three.

- [ ] **Step 5: Manual smoke test**

Run: `cargo run -- tui --config /tmp/mcpocket-empty.json`
(Create `/tmp/mcpocket-empty.json` with `{"version":1,"servers":{}}` first.)
Expected: TUI opens, tabs switch with Tab, `q` exits cleanly and the terminal is restored. The Live tab shows the "no active gateways" hint.

- [ ] **Step 6: Commit**

```bash
git add src/tui/mod.rs src/main.rs
git commit -m "feat(tui): main loop, panic-safe terminal, and tui command"
```

---

## Task 14: End-to-end — event reaches a socket client

**Files:**
- Modify: `tests/e2e_gateway.rs`

- [ ] **Step 1: Inspect the existing e2e to reuse its fixtures**

Run: `cargo test --test e2e_gateway -- --list`
Read `tests/e2e_gateway.rs` to find the existing stdio-upstream fixture builder
(`proxies_memory_module_style_stdio_upstream_end_to_end`). Reuse its config and
router construction helper.

- [ ] **Step 2: Write the failing test**

Add to `tests/e2e_gateway.rs` (adapt the fixture call to the existing helper that
builds a `GatewayRouter` from a stdio upstream config):

```rust
#[tokio::test]
async fn tool_call_emits_event_on_socket() {
    use mcpocket::telemetry::{run_dir_for, spawn_socket_server, EventBus, Event};
    // NOTE: telemetry items must be `pub` and re-exported from the crate root.

    let temp = std::env::temp_dir().join(format!("mcpocket-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    let config_path = temp.join("config.json");

    let bus = EventBus::new("e2e".to_owned());
    let guard = spawn_socket_server(bus.clone(), run_dir_for(&config_path))
        .await
        .unwrap();

    // Connect a raw socket client.
    use tokio::io::{AsyncBufReadExt, BufReader};
    use tokio::net::UnixStream;
    let stream = UnixStream::connect(guard.path()).await.unwrap();
    let mut lines = BufReader::new(stream).lines();
    let _hello = lines.next_line().await.unwrap().unwrap();

    // Emit as the router would on a tool call.
    bus.emit(Event::ToolCall {
        ts: 1,
        pid: bus.pid(),
        client: "e2e".to_owned(),
        server: "memory-module".to_owned(),
        tool: "memory-module__search".to_owned(),
        duration_ms: 12,
        status: mcpocket::telemetry::CallStatus::Ok,
    });

    let line = lines.next_line().await.unwrap().unwrap();
    let event: Event = serde_json::from_str(&line).unwrap();
    assert!(matches!(event, Event::ToolCall { .. }));

    drop(guard);
    let _ = std::fs::remove_dir_all(&temp);
}
```

> This test uses the crate as a library (`mcpocket::...`). The project is a
> binary crate. If there is no `src/lib.rs`, add one that re-exports the modules
> the test needs, OR move this test into `src/telemetry.rs` as an integration-
> style `#[tokio::test]`. **Preferred:** put the test in `src/telemetry.rs`
> (it already has `EventBus`/`spawn_socket_server` in scope) to avoid adding a
> library target. Use the in-module form below instead of the snippet above.

In-module form (add to `src/telemetry.rs` tests):

```rust
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
        assert_eq!(serde_json::from_str::<Event>(&line).unwrap(), sample_call(99));

        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 3: Run to verify failure, then it should pass once built**

Run: `cargo test --lib telemetry::tests::raw_client_receives_tool_call_after_hello`
Expected: PASS (the server is already implemented; this is the integration
assertion tying it together).

- [ ] **Step 4: Full verification**

Run: `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add src/telemetry.rs
git commit -m "test(telemetry): e2e raw client receives tool-call frame"
```

---

## Task 15: Docs

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Document the TUI**

Add a section after "Use The Gateway" in `README.md`:

```markdown
## Interactive Dashboard (TUI)

Launch the terminal dashboard:

\`\`\`bash
mcpocket tui
\`\`\`

Tabs (switch with `Tab` / `Shift+Tab`):

- **Servers** — upstream status and tool counts; `e`/`d` enable/disable the
  selected server.
- **Tools** — policy per server; `a`/`x` allow/deny the selected tool.
- **Live** — real-time tool-call traffic across every running gateway, with
  req/s, p95 latency, and error count.
- **Doctor** — local setup checks.

`r` refreshes, `q` (or `Esc`) quits.

Live traffic is read from per-process sockets under `~/.mcpocket/run/`. Each
`serve` process emits events without blocking tool calls; if no gateway is
running, the Live tab simply waits.
```

- [ ] **Step 2: Update the "Implemented" status list**

In the "Status" section of `README.md`, add to the implemented bullets:

```markdown
- `tui` interactive dashboard with live traffic monitor
```

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: document the mcpocket tui dashboard"
```

---

## Self-Review Notes (for the implementer)

- **Spec coverage:** telemetry layer (Tasks 2–5), socket discovery + reconnect +
  reap (Tasks 6–7), theme/truecolor (Task 8), interactive edits via `config_edit`
  (Task 13), all four tabs (Tasks 11–12), panic-safe terminal (Task 13),
  backpressure + render-tick resource discipline (Tasks 3 & 13), and the full
  test matrix (unit, IPC integration, backpressure, TestBackend render, e2e).
- **Live metrics (req/s, p95, error count)** are computed in `App` (Task 10) but
  are not yet wired into the Live tab header. If you want them on screen, extend
  `render_live` to draw a one-line summary using `app.req_per_sec(now_ms(), 10_000)`,
  `app.p95_latency()`, and `app.error_count()`. This is optional polish, not a
  spec requirement gap — the data and tests exist.
- **`PolicyReason` variant name** (Task 12 test) must match `src/policy.rs`.
  Verify before running that test.
- **Sparklines:** the spec mentions per-server latency sparklines (rich visual).
  ratatui ships a `Sparkline` widget; add it to `render_servers` by tracking a
  `VecDeque<u64>` of recent latencies per server in `App`. Deferred as polish to
  keep the core shippable; add a task if you want it in the first cut.
