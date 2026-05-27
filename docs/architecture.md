# mcpocket Architecture

`mcpocket` is a local MCP gateway. Instead of configuring every AI client with
many MCP servers, each client gets one server named `mcpocket`. The gateway then
connects to the upstream MCP servers defined in `~/.mcpocket/config.json`.

## Flow

```text
Codex / Claude / opencode
        |
        | stdio MCP
        v
    mcpocket
        |
        | stdio child process or HTTP MCP
        v
  memory-module, context7, github, plane, stripe, ...
```

## Runtime Pieces

### Downstream MCP Server

`src/mcp.rs` exposes the gateway as a stdio MCP server using `rmcp`.

Currently implemented downstream methods:

- `initialize`
- `tools/list`
- `tools/call`

The downstream server advertises tools only. Other MCP surfaces are planned but
not currently proxied.

### Config Loader

`src/config.rs` loads `~/.mcpocket/config.json`, validates it, and normalizes
server entries.

Supported upstream server shapes:

```json
{
  "transport": "stdio",
  "command": "node",
  "args": ["server.js"],
  "env": { "TOKEN": "..." }
}
```

```json
{
  "transport": "http",
  "url": "https://example.test/mcp",
  "headers": { "Authorization": "Bearer ..." }
}
```

Gateway-specific fields are optional:

```json
{
  "gateway": {
    "enabled": true,
    "allow_tools": ["github__create_issue"],
    "deny_tools": ["github__delete_repo"]
  }
}
```

If `gateway.enabled` is missing, it follows the server-level `enabled` value.

### Router

`src/router.rs` asks each enabled upstream for tools, rewrites names, applies
policy, and routes calls.

Tool names are exposed as:

```text
server__tool
```

Examples:

```text
memory-module__search_memories
context7__resolve_library_id
github__search_repositories
```

Routing splits on the first `__`. Because of that, upstream server names cannot
contain `__`.

### Policy

`src/policy.rs` controls what tools are visible.

Default behavior:

- expose tools with `annotations.readOnlyHint = true`
- hide destructive tools
- hide tools with unknown risk

Overrides:

- `gateway.allow_tools`: always expose these gateway tool names unless also
  denied
- `gateway.deny_tools`: always hide these gateway tool names

Deny wins over allow.

### Upstream Clients

`src/upstream.rs` manages lazy upstream connections.

Stdio upstreams:

- spawned as child processes
- initialized before tool calls
- reconnected after request failures

HTTP upstreams:

- use configured headers from the mcpocket config
- perform an MCP `initialize` handshake
- preserve `Mcp-Session-Id` when returned by upstream
- parse JSON and simple SSE `data:` responses

Downstream client credentials are never forwarded. Only configured upstream
headers/env are used.

### Client Sync

`src/client_sync.rs` writes a single `mcpocket` MCP server entry to:

- Claude: `~/.claude.json`
- Codex: `~/.codex/config.toml`
- opencode: `~/.config/opencode/opencode.json`

It does not remove existing direct MCP server entries in v1.

## Testing Strategy

Normal tests must be local and deterministic:

```bash
cargo test
```

The deterministic e2e test starts the gateway, creates a fake
`memory-module`-style stdio upstream, lists `memory-module__search_memories`,
and calls it through the gateway.

Live service checks are ignored by default:

```bash
cargo test --test e2e_gateway live_memory_module_is_reachable_through_gateway -- --ignored --nocapture
```

The live test uses the configured `memory-module` entry in
`~/.mcpocket/config.json`.

## Roadmap

1. Keep tools stable: list, policy filtering, call routing, reconnects, status.
2. Add resource proxying: list, read, templates, URI rewriting.
3. Add prompt proxying: list and get with `server__prompt` names.
4. Add MCP utilities: completions, logging notifications, progress, cancellation.
5. Add advanced client features only after core proxying is stable: sampling,
   roots, elicitation.
