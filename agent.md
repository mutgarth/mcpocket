# Agent Guide

This file is for coding agents working inside `mcpocket/mcpocket`.

## Project Purpose

`mcpocket` is a Rust MCP gateway. It exposes one local stdio MCP server to
clients like Codex, Claude Code, and opencode, then proxies configured upstream
MCP servers behind that single entry.

The current implementation focuses on tool proxying:

- downstream transport: stdio via `rmcp`
- upstream transports: stdio child process and HTTP JSON-RPC/Streamable-style
- config source: `~/.mcpocket/config.json`
- tool naming: `server__tool`
- default policy: expose read-only annotated tools only

The legacy Node sync prototype lives at `../mcpocket-sync`. Do not migrate or
delete it unless the user explicitly asks.

## Important Commands

From this directory:

```bash
cargo test
cargo run -- status --config ~/.mcpocket/config.json
cargo run -- serve --config ~/.mcpocket/config.json
cargo run -- sync --gateway --to claude,codex,opencode --dry-run
```

Live Memory Module e2e:

```bash
cargo test --test e2e_gateway live_memory_module_is_reachable_through_gateway -- --ignored --nocapture
```

The live e2e requires network access and a valid `memory-module` entry in
`~/.mcpocket/config.json`.

## Code Map

- `src/main.rs`: CLI entrypoint and command dispatch.
- `src/config.rs`: canonical config parsing, validation, path defaults, name
  mapping, and redaction helpers.
- `src/mcp.rs`: downstream stdio MCP server using `rmcp`.
- `src/router.rs`: aggregates upstream tools and routes `server__tool` calls.
- `src/upstream.rs`: stdio and HTTP upstream clients.
- `src/policy.rs`: tool exposure policy.
- `src/client_sync.rs`: writes one gateway MCP entry into client configs.
- `tests/e2e_gateway.rs`: end-to-end gateway tests.

## Engineering Rules

- Keep changes scoped to the Rust gateway unless the user asks for sync
  prototype changes.
- Do not log header values, env values, API keys, tokens, or full tool
  arguments.
- Preserve the `server__tool` naming contract. Server names containing `__`
  must remain invalid.
- Normal `cargo test` must stay deterministic and must not require network.
- Put live service checks behind `#[ignore]`.
- Prefer adding focused tests for config, policy, routing, and gateway behavior
  when changing those surfaces.

## Current Limitations

- Tools are proxied end to end.
- Resources, prompts, completions, progress, cancellation, sampling, roots, and
  elicitation are not fully proxied yet.
- OAuth is not implemented in the gateway. v1 uses configured headers/env
  credentials from `~/.mcpocket/config.json`.
