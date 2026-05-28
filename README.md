```text
███╗   ███╗  ██████╗ ██████╗   ██████╗   ██████╗ ██╗  ██╗ ███████╗ ████████╗
████╗ ████║ ██╔════╝ ██╔══██╗ ██╔═══██╗ ██╔════╝ ██║ ██╔╝ ██╔════╝ ╚══██╔══╝
██╔████╔██║ ██║      ██████╔╝ ██║   ██║ ██║      █████╔╝  █████╗      ██║
██║╚██╔╝██║ ██║      ██╔═══╝  ██║   ██║ ██║      ██╔═██╗  ██╔══╝      ██║
██║ ╚═╝ ██║ ╚██████╗ ██║      ╚██████╔╝ ╚██████╗ ██║  ██╗ ███████╗    ██║
╚═╝     ╚═╝  ╚═════╝ ╚═╝       ╚═════╝   ╚═════╝ ╚═╝  ╚═╝ ╚══════╝    ╚═╝
```

# mcpocket

[![CI](https://github.com/mutgarth/mcpocket/actions/workflows/ci.yml/badge.svg)](https://github.com/mutgarth/mcpocket/actions/workflows/ci.yml)
[![Release](https://github.com/mutgarth/mcpocket/actions/workflows/release.yml/badge.svg)](https://github.com/mutgarth/mcpocket/actions/workflows/release.yml)

`mcpocket` is a Rust MCP gateway. It gives AI clients one local MCP server named
`mcpocket`, then routes tool calls to the upstream MCP servers configured in
`~/.mcpocket/config.json`.

The goal is to centralize MCP routing, credentials, safety policy, and client
config sync instead of maintaining duplicate MCP entries in Claude Code, Codex,
and opencode.

## Status

Implemented:

- stdio MCP gateway for downstream clients
- upstream stdio child-process MCP clients
- upstream HTTP MCP clients
- `tools/list` and `tools/call` proxying
- namespaced tool names: `server__tool`
- read-only-by-default tool exposure policy
- `status` command
- `list`, `enable`, `disable`, `allow-tool`, and `deny-tool` management commands
- `tools` command for policy inspection
- `doctor` command for local setup checks
- `tui` interactive dashboard with live traffic monitor
- gateway sync for Claude, Codex, and opencode
- deterministic e2e test plus optional live Memory Module check

Not implemented yet:

- resources proxying
- prompts proxying
- completions, progress, cancellation, logging forwarding
- sampling, roots, elicitation
- OAuth/token vault management

## Repository Layout

```text
.
├── agent.md                 # instructions for future coding agents
├── docs/
│   └── architecture.md      # gateway architecture and roadmap
├── src/
│   ├── client_sync.rs       # writes one mcpocket entry into client configs
│   ├── config.rs            # config parsing, validation, redaction helpers
│   ├── main.rs              # CLI
│   ├── mcp.rs               # downstream rmcp stdio server
│   ├── policy.rs            # tool visibility policy
│   ├── router.rs            # tool aggregation and call routing
│   └── upstream.rs          # stdio and HTTP upstream clients
└── tests/
    └── e2e_gateway.rs       # gateway e2e tests
```

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/mutgarth/mcpocket/main/scripts/install.sh | bash
```

The installer downloads the latest GitHub Release for your platform and places
`mcpocket` in `~/.local/bin`. It currently supports:

```text
macOS arm64
macOS x86_64
Linux x86_64
```

Run the same command again to update to the latest release.

To install somewhere else:

```bash
MCPOCKET_INSTALL_DIR=/usr/local/bin bash -c "$(curl -fsSL https://raw.githubusercontent.com/mutgarth/mcpocket/main/scripts/install.sh)"
```

Verify:

```bash
mcpocket --help
mcpocket status
```

## Build From Source

```bash
git clone https://github.com/mutgarth/mcpocket.git
cd mcpocket
cargo build --release
```

The binary will be:

```text
./target/release/mcpocket
```

## Configure Upstreams

The gateway reads:

```text
~/.mcpocket/config.json
```

Example:

```json
{
  "version": 1,
  "servers": {
    "memory-module": {
      "enabled": true,
      "transport": "http",
      "url": "https://api.memorymodule.io/mcp",
      "headers": {
        "x-api-key": "..."
      },
      "gateway": {
        "enabled": true,
        "allow_tools": [],
        "deny_tools": []
      }
    }
  }
}
```

You can manage existing upstreams with the Rust CLI:

```bash
mcpocket list
mcpocket enable memory-module
mcpocket disable memory-module
mcpocket allow-tool memory-module__search_memories
mcpocket deny-tool github__delete_repo
```

The edit commands update `~/.mcpocket/config.json` and create a timestamped
backup next to it before writing.

## Use The Gateway

List configured upstreams without contacting them:

```bash
mcpocket list
```

Check upstream status:

```bash
mcpocket status
```

The status output groups healthy and failing upstreams, shows request latency,
and reports `exposed/upstream` tool counts:

```text
Gateway: /Users/lucasmeneses/.mcpocket/config.json

Healthy
STATE  NAME                 TYPE     TOOLS       LATENCY      DETAILS
OK     memory-module        http     5/11        430ms        https://api.memorymodule.io/mcp headers:x-api-key=***

Needs attention
STATE  NAME                 TYPE     TOOLS       LATENCY      DETAILS
FAIL   plane                http     -           5001ms       https://mcp.plane.so/http/mcp
```

Inspect tools exposed or hidden by policy:

```bash
mcpocket tools
mcpocket tools memory-module
```

Example:

```text
MCP memory-module (http)
POLICY   TOOL                                 REASON
ALLOW    memory-module__search_memories      allowlist
ALLOW    memory-module__list_memories        allowlist
HIDE     memory-module__delete_memory        destructive
```

Check local setup:

```bash
mcpocket doctor
```

`doctor` checks whether `mcpocket` is on `PATH`, whether the gateway config
loads, and whether common client configs point to the gateway without keeping a
direct `memory-module` MCP entry.

Sync one `mcpocket` MCP entry into clients:

```bash
mcpocket sync --gateway --to claude,codex,opencode --dry-run
mcpocket sync --gateway --to claude,codex,opencode
```

Restart the client after syncing. The client will launch the gateway
automatically when it needs MCP tools.

You usually do not run `serve` manually. It is the command clients execute from
their MCP config:

```bash
mcpocket serve --config ~/.mcpocket/config.json
```

## Interactive Dashboard (TUI)

Launch the terminal dashboard:

```bash
mcpocket tui
```

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

## Tool Names

Every upstream tool is exposed as:

```text
server__tool
```

Examples:

```text
memory-module__search_memories
context7__resolve_library_id
github__search_repositories
```

Server names must be unique and cannot contain `__`.

## Tool Policy

By default, mcpocket exposes only tools that are clearly annotated read-only.
Unknown-risk or destructive tools are hidden.

To expose a hidden tool:

```bash
mcpocket allow-tool github__create_issue
```

This adds its gateway name to `gateway.allow_tools`:

```json
{
  "gateway": {
    "enabled": true,
    "allow_tools": ["github__create_issue"],
    "deny_tools": []
  }
}
```

To always hide a tool:

```bash
mcpocket deny-tool github__delete_repo
```

This adds it to `gateway.deny_tools`. Deny wins over allow.

## Development Workflow

Run formatting:

```bash
cargo fmt
```

Run all deterministic tests:

```bash
cargo test
```

Run only the local gateway e2e:

```bash
cargo test --test e2e_gateway proxies_memory_module_style_stdio_upstream_end_to_end
```

Run the live Memory Module reachability test:

```bash
cargo test --test e2e_gateway live_memory_module_is_reachable_through_gateway -- --ignored --nocapture
```

The live test requires network access and a valid `memory-module` entry in
`~/.mcpocket/config.json`.

## Release Workflow

Releases are built by GitHub Actions when a version tag is pushed:

```bash
git tag v0.1.0
git push origin v0.1.0
```

The release workflow runs tests, builds macOS and Linux binaries, creates a
GitHub Release, and uploads downloadable assets for the installer.

## Useful Debug Commands

Check CLI help:

```bash
./target/release/mcpocket --help
./target/release/mcpocket sync --help
```

Run with logs:

```bash
RUST_LOG=info ./target/release/mcpocket serve --config ~/.mcpocket/config.json
```

Smoke-test an empty config:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"smoke","version":"0"}}}' \
  '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' \
  '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}' \
  | ./target/release/mcpocket serve --config /tmp/mcpocket-empty.json
```

## More Detail

Read [docs/architecture.md](docs/architecture.md) for the gateway design and
roadmap.
