export const meta = {
  name: 'build-mcpocket-tui',
  description: 'Implement the mcpocket TUI from the committed TDD plan, task-by-task, with a final verification + review gate',
  phases: [
    { title: 'Deps' },
    { title: 'Telemetry' },
    { title: 'TUI Core' },
    { title: 'Rendering' },
    { title: 'Wiring' },
    { title: 'E2E + Docs' },
    { title: 'Verify' },
    { title: 'Review' },
  ],
}

const DIR = '/Users/lucasmeneses/mcpocket/mcpocket'
const PLAN = `${DIR}/docs/superpowers/plans/2026-05-28-mcpocket-tui.md`

const STATUS = {
  type: 'object',
  additionalProperties: false,
  required: ['ok', 'summary'],
  properties: {
    ok: { type: 'boolean', description: 'true only if all specified cargo commands passed and changes were committed' },
    summary: { type: 'string', description: 'what was implemented and the test result' },
    commits: { type: 'string', description: 'commit hashes/messages created' },
    failures: { type: 'string', description: 'any failing commands with their output, or empty if none' },
  },
}

function impl(tasks, detail) {
  return `You are implementing part of a Rust TUI for the \`mcpocket\` MCP gateway.

WORKING DIRECTORY: ${DIR} (a cargo project; always use absolute paths or cd there).
PLAN FILE: ${PLAN}

Read the plan file first. Then implement EXACTLY these tasks: ${tasks}.
${detail}

Rules:
- Follow the TDD steps in the plan verbatim: write the failing test, confirm it fails, implement minimal code, confirm it passes.
- Use the exact code, file paths, types, and function signatures given in the plan. Do NOT invent alternative APIs.
- Run the cargo commands the plan specifies for your tasks. Before committing each task, ensure \`cargo build\` succeeds.
- Commit with the exact commit messages from the plan (one commit per task).
- This is edition 2024. Reuse existing modules (GatewayRouter, config_edit, doctor, upstream) — do not duplicate their logic.
- Run \`cargo fmt\` before committing so formatting is clean.
- If a plan code block has a small bug that prevents compiling (e.g. a missing import or a clippy nit), fix it minimally and note it in failures, but keep the design identical.
- Do NOT proceed past a task whose tests fail; instead report ok=false with the failing output.

Return your status. ok=true only if every assigned task built, its tests passed, and you committed.`
}

phase('Deps')
const r1 = await agent(impl('Task 1 (add ratatui, crossterm, tokio net feature)', 'This is a quick dependency edit; run `cargo build` to confirm the new crates resolve.'), { label: 'task1:deps', phase: 'Deps', schema: STATUS })
if (!r1 || !r1.ok) return { stopped_at: 'Deps', detail: r1 }

phase('Telemetry')
const r2 = await agent(impl('Tasks 2, 3, and 4 (telemetry Event type, EventBus, run-dir helpers + Unix socket server)', 'Implement all three into src/telemetry.rs and register `mod telemetry;` in main.rs. After Task 4, `cargo test --lib telemetry` must be fully green.'), { label: 'task2-4:telemetry', phase: 'Telemetry', schema: STATUS })
if (!r2 || !r2.ok) return { stopped_at: 'Telemetry(2-4)', detail: r2 }

const r3 = await agent(impl('Task 5 (wire EventBus into router.call_tool and start the socket server in mcp::serve_stdio; update the Serve arm in main.rs)', 'After this, `cargo test` for the whole crate must still pass.'), { label: 'task5:wiring', phase: 'Telemetry', schema: STATUS })
if (!r3 || !r3.ok) return { stopped_at: 'Telemetry(5)', detail: r3 }

phase('TUI Core')
// Sequential: each adds a `pub mod` line to src/tui/mod.rs, so parallel edits would race.
const r6 = await agent(impl('Tasks 6 and 7 (src/tui/discovery.rs: parse_serve_pid, list_socket_paths, connect_or_reap, stream_socket, spawn_connection_manager) and create src/tui/mod.rs with `pub mod discovery;`, register `mod tui;` in main.rs', '`cargo test --lib discovery` must be green (4 tests).'), { label: 'task6-7:discovery', phase: 'TUI Core', schema: STATUS })
if (!r6 || !r6.ok) return { stopped_at: 'TUI Core(6-7)', detail: r6 }

const r8 = await agent(impl('Task 8 (src/tui/theme.rs brand theme with truecolor fallback; add `pub mod theme;` to src/tui/mod.rs)', '`cargo test --lib theme` must be green (3 tests).'), { label: 'task8:theme', phase: 'TUI Core', schema: STATUS })
if (!r8 || !r8.ok) return { stopped_at: 'TUI Core(8)', detail: r8 }

const r9 = await agent(impl('Task 9 (src/tui/input.rs Action enum + map_key; add `pub mod input;` to src/tui/mod.rs)', '`cargo test --lib input` must be green (4 tests).'), { label: 'task9:input', phase: 'TUI Core', schema: STATUS })
if (!r9 || !r9.ok) return { stopped_at: 'TUI Core(9)', detail: r9 }

const r10 = await agent(impl('Task 10 (src/tui/app.rs App state, Tab, LiveEvent, metrics; add `pub mod app;` to src/tui/mod.rs)', '`cargo test --lib app` must be green (5 tests).'), { label: 'task10:app', phase: 'TUI Core', schema: STATUS })
if (!r10 || !r10.ok) return { stopped_at: 'TUI Core(10)', detail: r10 }

phase('Rendering')
const r11 = await agent(impl('Tasks 11 and 12 (src/tui/ui.rs: tab bar + Servers/Live/Tools/Doctor rendering, tested via ratatui TestBackend; add `pub mod ui;` to src/tui/mod.rs and the App data fields from Task 12)', 'Verify the PolicyReason variant name against src/policy.rs before running the Tools test, and adjust the test to the real variant if needed. `cargo test --lib ui` must be green (4 tests).'), { label: 'task11-12:rendering', phase: 'Rendering', schema: STATUS })
if (!r11 || !r11.ok) return { stopped_at: 'Rendering', detail: r11 }

phase('Wiring')
const r13 = await agent(impl('Task 13 (src/tui/mod.rs run_tui main loop, panic-safe terminal setup/teardown, input thread, handle_action, refresh_data; add the Tui command + match arm in main.rs)', 'After this, `cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --check` must ALL pass. Fix any clippy warnings minimally (e.g. prefer &Path over &PathBuf). Do the manual smoke test described in the plan if feasible (create /tmp/mcpocket-empty.json and run `cargo run -- tui --config /tmp/mcpocket-empty.json` with a 2s timeout, or skip if no TTY — note which).'), { label: 'task13:mainloop', phase: 'Wiring', schema: STATUS })
if (!r13 || !r13.ok) return { stopped_at: 'Wiring', detail: r13 }

phase('E2E + Docs')
const r14 = await agent(impl('Task 14 (add the in-module e2e telemetry test raw_client_receives_tool_call_after_hello in src/telemetry.rs) and Task 15 (README TUI section + status bullet)', 'Use the in-module form preferred by the plan (avoid adding a lib target). `cargo test` must pass.'), { label: 'task14-15:e2e+docs', phase: 'E2E + Docs', schema: STATUS })
if (!r14 || !r14.ok) return { stopped_at: 'E2E + Docs', detail: r14 }

phase('Verify')
const verify = await agent(`In ${DIR}, run the full verification gate and report exact results:
- \`cargo test\` (all tests)
- \`cargo clippy --all-targets -- -D warnings\`
- \`cargo fmt --check\`
- \`cargo build --release\`
Report the pass/fail of each command with the relevant tail of output. Do not change code; only verify. Also report the total test count.`, { label: 'verify:gate', phase: 'Verify', schema: {
  type: 'object', additionalProperties: false, required: ['all_passed', 'report'],
  properties: {
    all_passed: { type: 'boolean' },
    report: { type: 'string', description: 'per-command pass/fail with output tails and total test count' },
  },
}})

phase('Review')
const review = await agent(`Adversarially review the mcpocket TUI implementation just built in ${DIR}. Inspect the git diff of branch feat/tui against main (\`git diff main...feat/tui\`) and the new files under src/telemetry.rs and src/tui/.
Focus on REAL defects in these areas:
1. RESOURCE MANAGEMENT: does telemetry emission ever block the tool-call hot path? Are channels bounded? Is the render loop tick-bounded? Any unbounded growth (event history, latency buffers, spawned tasks per rescan that never exit)?
2. RELIABILITY: is the terminal always restored on panic AND on normal/error exit? Are stale sockets reaped? Does the TUI degrade gracefully with zero serve processes? Any \`.unwrap()\`/\`.expect()\` on I/O that could crash the loop?
3. CORRECTNESS: does the connection manager leak tasks or busy-loop? Off-by-one in p95/req_per_sec? Socket file permission/cleanup issues?
Report concrete findings with file:line and a suggested fix for each. Rank by severity. If something is clean, say so plainly — do not invent issues.`, { label: 'review:adversarial', phase: 'Review', schema: {
  type: 'object', additionalProperties: false, required: ['findings', 'verdict'],
  properties: {
    findings: { type: 'array', items: { type: 'object', additionalProperties: false, required: ['severity', 'location', 'issue', 'fix'], properties: { severity: { type: 'string' }, location: { type: 'string' }, issue: { type: 'string' }, fix: { type: 'string' } } } },
    verdict: { type: 'string', description: 'overall assessment: ship-ready or what must change first' },
  },
}})

return {
  tasks_completed: 'all (1-15)',
  verify,
  review,
}
