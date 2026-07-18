# AGENTS.md — working in the kranz repo

kranz is a Rust harness that turns "run an AI agent on this task" into a
planned, human-approved, validated, audited **mission** over headless agent
CLIs. This file is the conventions anchor for any agent (or human) working
here — read it before making changes.

## Layout

- `crates/engine` — the mission engine: orchestrator, agent backends
  (`backend_claude`, `backend_codex`, `backend_droid`), runner, config,
  gates, append-only event log, worktree isolation.
- `crates/cli` — the `kranz` binary (draft/queue/work/serve/exec/abandon).
- `crates/server` — REST + WebSocket host (`kranz serve`), the MissionHost.
- `crates/slack` — the Slack bridge (socket-mode).
- `apps/dashboard` — React/Vite (+ Tauri) Mission Control UI.
- `docs/scoping/` — design docs; each carries flagged **D-X operator
  decisions**. `docs/roadmap.md` — milestones. `.kranz/tickets/` — backlog.

## Commands

```bash
# Rust — the FULL-GATE suite (run all four before declaring done):
cargo test --workspace          # NOT `-p kranz-engine` — gate the whole workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check         # (use `cargo fmt --all` to fix)
cargo build --workspace

cargo install --path crates/cli --force   # rebuild+install the kranz binary

# Dashboard (run all gates and sync the embedded bundle when apps/dashboard changed):
cd apps/dashboard && npx tsc -b && npm run test && npm run build && npm run sync-embedded && npm run check-embedded && npm run lint

# Operate:
kranz serve                     # dashboard + REST/WS on 127.0.0.1:4560
kranz draft <ticket-slug>       # draft a plan from a .kranz/tickets/<slug>.md
kranz ticket queue <slug>       # queue an approved plan
kranz work                      # drain the queue (run missions)
```

## Rules that have bitten — follow them

1. **Gate on `--workspace`, never `-p kranz-engine` alone** for the final
   regression check. A crate-scoped pass hides breakage in consumers.
2. **Bare exit codes after gates — never pipe gate commands.** `... | tail -1`
   once masked a build failure and shipped a broken push. Run the gate, read
   its raw exit code.
3. **Run `cargo fmt --all` before finishing.** A skipped fmt has cost full
   respawns; CI gates `cargo fmt --check`.
4. **kranz never pushes.** The engine only advances refs locally / merges
   `--no-ff`. A human runs `git push`. This is inviolable.
5. **Anti-vacuity in contract commands:** write test-gate checks as
   `cargo test --workspace <filter> 2>&1 | grep -qE 'test result: ok\. [1-9]'`.
   The `[1-9]` guards against a filter that matches **zero** tests passing
   vacuously ("0 passed").
6. **Contract files are additive-only:** `crates/engine/src/events.rs` and
   `types.rs` define the event schema and persisted state. Add fields
   (`#[serde(default)]`); never break old logs/configs. Schema changes are a
   deliberate `contractChangeRequest`.
7. **Worktree isolation:** in worktree mode, workers run in dedicated git
   worktrees; the **primary checkout must stay byte-untouched** across a
   mission. Never check out the mission branch in, or commit to, the primary
   tree from run()/approve/validation.
8. **A mission must deliver:** a run whose diff against the pinned `base_sha`
   has zero feature commits FAILS honestly — never COMPLETE on an empty
   deliverable. Don't defeat that gate.
9. **Match the surrounding code** — comment density, naming, idiom. New
   abstractions earn their keep only on the third use.

## Tracked vs runtime

Committed: `.kranz/merge-gates.json`, `.kranz/tickets/<slug>.md`, and each
mission's `plan.md` / `plan.json` / `report.md` (on the mission branch). Gitignored runtime
(never commit): `events.jsonl`, `state.json`, `runs/`, `control/`,
`.kranz/config.json`, `serve.token`, `.kranz/tickets/*.status`.

## Change discipline

Small, focused changes with tests. Every change gets reviewed against the
five axes (correctness, readability, architecture, security, performance)
before merge. Prefer the standard library and existing utilities over new
dependencies.
