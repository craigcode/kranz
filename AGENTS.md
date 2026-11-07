# AGENTS.md — working in the kranz repo

kranz is a Rust harness that turns "run an AI agent on this task" into a
planned, human-approved, validated, audited **mission** over headless agent
CLIs. This file is the conventions anchor for any agent (or human) working
here — read it before making changes.

## Layout

Start at `docs/knowledge/index.md` for the reviewed knowledge map, and
`docs/README.md` for the rest of `docs/`.

- `crates/engine` — the mission engine: orchestrator, agent backends
  (`backend_claude`, `backend_codex`, `backend_droid`), runner, config,
  gates, append-only event log, worktree isolation, filtering egress proxy
  (`egress_proxy`, the `fs+net` boundary + denial signal).
- `crates/cli` — the `kranz` binary (draft/queue/work/serve/exec/abandon).
- `crates/server` — REST + WebSocket host (`kranz serve`), the MissionHost.
- `crates/slack` — the Slack bridge (socket-mode).
- `crates/acp` — ACP client crate (see its README).
- `apps/dashboard` — React/Vite (+ Tauri) Mission Control UI.
- `apps/landing` — landing site. `apps/even-g2` — experimental glasses client.
- `packaging/` — homebrew, release and gascity pack. `scripts/` — CI and audit helpers.
- `.kranz/lessons/` — per-mission lessons.
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
# check-embedded compares your bundle against CI's clean `npm ci` build — if
# node_modules may be stale, run `npm ci` BEFORE `npm run build`, or the local
# hash can pass here and still fail in CI.

# Experimental Even G2 client (when apps/even-g2 changed):
cd apps/even-g2 && npm ci && npm audit --audit-level=high && npm run test && npm run pack && npm run lint

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
4. **kranz never pushes by default.** A human pushes reviewed work; the only
   exception is the human-invoked `kranz exec --push`. See docs/knowledge/decisions/inviolable-invariants.md.
5. **Anti-vacuity in contract commands:** write test-gate checks as
   `cargo test --workspace <filter> 2>&1 | grep -qE 'test result: ok\. [1-9]'`.
   The `[1-9]` guards against a filter that matches **zero** tests passing
   vacuously ("0 passed"). A nonzero count can ALSO be vacuous: a filter
   substring that collides with a pre-existing passing test (m-66aff8's a3 —
   `refus` matched `dirty_tracked_tree_is_refused_...`, gating green with
   zero implementation). Grep existing test names for your substring before
   shipping the contract; match ONLY the not-yet-written test (or use
   `--exact`).
6. **Contract files are additive-only:** `events.rs` and `types.rs` only gain
   `#[serde(default)]` fields; schema changes are a `contractChangeRequest`.
   See docs/knowledge/decisions/inviolable-invariants.md.
7. **Worktree isolation:** the primary checkout stays byte-untouched across a
   mission. See docs/knowledge/decisions/inviolable-invariants.md.
8. **A mission must deliver:** a zero-commit diff against `base_sha` FAILS;
   never defeat that gate. See docs/knowledge/decisions/inviolable-invariants.md.
9. **Match the surrounding code** — comment density, naming, idiom. New
   abstractions earn their keep only on the third use.
10. **Children are hostile by default (2026-07-28 hardening):** every
    prompt-injectable child spawns `env_clear`d (`agent_env.rs`); never
    reintroduce ambient inheritance. Credentials reach children only via
    `contractEnvPassthrough` and workspace `secrets`. `serve.token` and
    `serve.read.token` must stay unreadable inside sandboxes. Gates and
    validators run wrapped and fail closed. Full detail, including
    `SKIP-UNDER-WRAP` and the gate signal policy:
    [docs/knowledge/validation/child-containment.md](docs/knowledge/validation/child-containment.md).
    Read it and docs/acp-containment.md before widening anything.
11. **When committing alongside in-flight subagents, stage by exact file
    list.** Before `git add`, check `git status` for files an agent also
    touched; include its whole change set or leave its files alone. Never use
    `git add -u` or `-A` unless `git status` shows only your own changes: the
    operator keeps live uncommitted doc edits in this tree.

## Tracked vs runtime

Never commit `events.jsonl`, `state.json`, `runs/`, `control/`,
`.kranz/config.json` or the serve tokens. Full tracked vs runtime list and
ticket lifecycle state: [docs/knowledge/operations/tracked-vs-runtime.md](docs/knowledge/operations/tracked-vs-runtime.md).

## Public documentation boundary

Tickets, discussion sidecars, roadmap entries and design notes in this repo
are public source artifacts. Keep implementation scope, constraints, acceptance
criteria and necessary technical attribution here. Keep competitive strategy,
market comparisons and private business analysis in an access-controlled notes
store outside the checkout. Public tickets must remain self-contained; do not
make a private note a prerequisite for implementing or reviewing them.

## Change discipline

Small, focused changes with tests. Every change gets reviewed against the
five axes (correctness, readability, architecture, security, performance)
before merge. Prefer the standard library and existing utilities over new
dependencies.

The positioning ADR's freeze applies at the point of temptation: new
in-harness execution primitives — worker pools beyond the shipped M3
machinery, prompt routing sophistication, context-management features,
anything whose purpose is to make an agent write better code — are NOT
built (docs/knowledge/decisions/positioning-governance-evidence-layer.md;
the heterogeneous dispatch pool is the one carve-out, an evidence
primitive with three properties). If a change drifts toward a frozen
surface, stop and check the boundary first; `docs/what-is-kranz.md` and
the frozen modules' doc headers (`prompts.rs`, `knowledge.rs`) carry the
same pointer.
