# mcpocket TUI — Design

Date: 2026-05-28
Status: Approved (brainstorming)
Command: `mcpocket tui`

## Goal

Add a polished, brand-themed terminal UI to `mcpocket` that works as both a
**management dashboard** (status, tools, policy, enable/disable, allow/deny,
doctor) and a **live monitor** of gateway traffic across all running `serve`
processes. Ship it with strong test coverage, graceful failure handling, and
bounded machine-resource usage (the `serve` hot path must stay untouched in
practice).

## Constraints & Context

- Each MCP client (Claude, Codex, opencode) launches its **own** long-lived
  `mcpocket serve` stdio process. There is no single gateway process — there may
  be several at once, or none. The TUI is a **separate, operator-facing**
  process and must connect to / merge from many `serve` processes.
- The TUI must reuse existing machinery rather than duplicate it:
  - `GatewayRouter` (`src/router.rs`) for `status()` and `inspect_tools()`.
  - `config_edit` (`src/config_edit.rs`) for `set_server_enabled`, `allow_tool`,
    `deny_tool` — these already do a timestamped backup + write.
- Resource discipline: telemetry emission on the `serve` side must **never block
  or slow the tool-call hot path**. The TUI render loop must be tick-bounded.

## Architecture

```
┌─ serve process (launched by a client) ────────────────┐
│  router.call_tool ──emit──▶ telemetry::EventBus        │
│                              ├ ring buffer (last N)     │
│                              └ broadcast (bounded)      │
│                                   │                     │
│                          UnixListener  ~/.mcpocket/run/ │
│                          serve-<pid>.sock               │
└───────────────────────────────────────┬────────────────┘
                                         │ JSONL frames
┌─ mcpocket tui process (operator) ──────┴────────────────┐
│  discovery: scan *.sock ▶ connect all ▶ reconnect       │
│  app state ◀ merge(input events, telemetry events)      │
│  render (ratatui + crossterm) @ fixed tick              │
│  edit actions ──▶ config_edit (backup + atomic write)   │
└─────────────────────────────────────────────────────────┘
```

### Layer boundaries

| Unit | Does | Depends on | Tested via |
|---|---|---|---|
| `telemetry::EventBus` | hold ring buffer, fan out events without blocking | tokio broadcast | unit + backpressure stress |
| telemetry socket server | accept connections, send `hello`+replay, stream events | EventBus, tokio net | IPC integration |
| `tui::discovery` | find/connect/reconnect/reap serve sockets | tokio net | IPC integration |
| `tui::app` | hold UI state, derive metrics (req/s, p95) | events | unit |
| `tui::ui::*` | render each tab to a frame | app + theme | `TestBackend` render |
| `tui::theme` | brand palette + truecolor fallback | crossterm | unit |

## Telemetry layer (serve side)

New module `src/telemetry.rs`.

### Event schema (JSONL, one frame per line)

```jsonc
{"ts":1716800000123,"pid":4823,"client":"claude","kind":"tool_call",
 "server":"github","tool":"github__search_repos","duration_ms":180,"status":"ok"}
```

- `kind` is an enum to allow future event types (`tool_call`, `hello`, `list_tools`).
- A `hello` frame is sent first on connect: serve metadata (`pid`, `client`,
  `version`, start time) followed by a replay of the ring buffer. This resolves
  the "socket has no history" downside of the IPC approach.
- `client` is read from `MCPOCKET_CLIENT` (env injected by `sync`, future
  enhancement) with a fallback to the parent process name, then `"unknown"`.

### EventBus

- `tokio::sync::broadcast::Sender` with a **bounded** capacity.
- A `VecDeque` ring buffer (default 200) behind a `Mutex`/`RwLock` for replay.
- `emit(event)`: push to ring (pop front if full), then `broadcast::send`.
  `broadcast::send` **drops for lagging receivers** and returns `Err` when there
  are no receivers — both are ignored. **The hot path never awaits the TUI.**
- When no TUI is connected, cost ≈ one VecDeque push + one send that is dropped.

### Wiring

- `router.call_tool` (`src/router.rs:79`) already computes `server`, `tool`
  (exposed name), `duration_ms`, and `status`. Emit the event there, right
  alongside the existing `info!(...)`.
- `GatewayRouter` gains an optional `EventBus` handle (clone-cheap `Arc`).
- `mcp::serve_stdio` (`src/mcp.rs:14`) spawns the socket-server task, creates the
  `~/.mcpocket/run/` dir (`0700`), binds `serve-<pid>.sock`, and removes it on
  shutdown (best-effort). Stale sockets (dead pid) are reaped by the TUI side.

## TUI process (consumer)

New module tree `src/tui/`:

- `mod.rs` — `run_tui(config_path)`: terminal setup/teardown, main loop.
- `app.rs` — state: active tab, selection, bounded event history, derived
  metrics (req/s over a window, p95 latency, error count).
- `event.rs` — merge keyboard/mouse input + telemetry stream into one channel.
- `discovery.rs` — scan `~/.mcpocket/run/*.sock`, manage connections (reconnect
  with backoff, reap orphan sockets whose pid is dead).
- `theme.rs` — brand purple palette (`#1D0245`–`#B898E8`), truecolor with ANSI
  fallback; neon-purple selection highlight.
- `ui/servers.rs` — status table + per-server latency sparkline; `e`/`d` enable/
  disable.
- `ui/tools.rs` — policy per server (ALLOW/HIDE); `a`/`x` allow/deny.
- `ui/live.rs` — live traffic feed + req/s, p95, error count.
- `ui/doctor.rs` — runs existing `doctor` checks.

### Render & resource discipline

- Fixed render tick (~8 fps) decoupled from event arrival → **bounded CPU** even
  under heavy traffic. Redraw only when a dirty flag is set.
- Status probes (which contact upstreams) run on demand / throttled, never on a
  tight loop.
- Edit actions call `config_edit` (atomic write + existing backup) and reload the
  `GatewayRouter`.

## Dependencies

- `ratatui` (TUI framework) + `crossterm` (backend, mouse, truecolor).
- `tokio`: add the `"net"` feature for `UnixListener`/`UnixStream`. `broadcast`
  is already covered by the existing `"sync"` feature.

## Error handling & robustness

- **Panic hook** restores the terminal (leave raw mode / alternate screen) so a
  crash never corrupts the user's terminal.
- **Graceful degradation**: no `serve` running → Live tab shows "no active
  gateways"; every other tab works.
- TUI is **read-only over `serve`**; the only writes are config edits via
  `config_edit`.
- Socket directory is `0700`; orphan sockets are reaped.

## Testing strategy

- **Unit**: `Event` serde round-trip; ring buffer capacity/eviction; theme color
  fallback; sparkline data reduction; socket-path parsing; key→action mapping.
- **IPC integration**: a fake `UnixListener` emits events → the TUI connection
  manager ingests them, **reconnects after drop**, and **reaps an orphan socket**.
- **Backpressure**: flood `EventBus` with no/slow receiver → assert the emitter
  **never blocks** and old events are dropped (bounded cost).
- **Headless render**: `ratatui::TestBackend` — render `App` with known state and
  assert on the resulting buffer cells (deterministic, no real terminal).
- **e2e**: extend `tests/e2e_gateway.rs` — run `serve`, connect a raw socket
  client, call a tool, assert an event frame arrives.

## Out of scope (YAGNI)

- Persisting telemetry to disk / long-term metrics history.
- Authentication on the socket (local-only, `0700` dir is sufficient for v1).
- Configuring upstreams from scratch in the TUI (add/remove servers) — edit of
  existing entries only.
- Resources/prompts proxying telemetry (gateway doesn't proxy those yet).

## Shipping plan

After this spec is approved, `writing-plans` produces the detailed plan, then we
execute via a multi-agent Workflow in phases (each TDD + verification):

- **Phase A** — Telemetry (`telemetry.rs` + router/mcp wiring).
- **Phase B** — TUI skeleton: terminal setup, theme, tabs, render loop.
- **Phase C** — Tabs Servers/Tools/Live/Doctor.
- **Phase D** — Discovery/reconnect/reaping.
- **Phase E** — Verification: `cargo test` + `clippy` + `fmt` + manual run +
  adversarial review.
