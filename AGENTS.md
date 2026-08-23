# AGENTS.md — working in the kranz repo

kranz is a Rust harness that turns "run an AI agent on this task" into a
planned, human-approved, validated, audited **mission** over headless agent
CLIs. This file is the conventions anchor for any agent (or human) working
here — read it before making changes.

## Layout

- `crates/engine` — the mission engine: orchestrator, agent backends
  (`backend_claude`, `backend_codex`, `backend_droid`), runner, config,
  gates, append-only event log, worktree isolation, filtering egress proxy
  (`egress_proxy`, the `fs+net` boundary + denial signal).
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
# check-embedded compares your bundle against CI's clean `npm ci` build — if
# node_modules may be stale, run `npm ci` BEFORE `npm run build`, or the local
# hash can pass here and still fail in CI.

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
4. **kranz never pushes by default.** Mission execution and gated merge only
   advance local refs / merge `--no-ff`; a human pushes reviewed work. The sole
   exception is the human-invoked cloud handoff `kranz exec --push <REMOTE>`:
   it may push one completed `kranz/*` branch to an already-configured remote,
   never a base branch, tag, refspec, force, or merge. No other push path is
   permitted.
5. **When committing alongside in-flight subagents, stage by exact file
   list.** Three times in one week a scoped fix swept a subagent's
   uncommitted edits into a commit WITHOUT the rest of its feature
   (4115a9b, 475ecb8, 5c30396) — CI red on every platform until the
   completing commit. Before `git add`, check `git status` for files the
   agent also touched, and either include its whole change set or leave
   its files alone. Never `git add` a file an in-flight agent has also
   edited on its own.
5. **Anti-vacuity in contract commands:** write test-gate checks as
   `cargo test --workspace <filter> 2>&1 | grep -qE 'test result: ok\. [1-9]'`.
   The `[1-9]` guards against a filter that matches **zero** tests passing
   vacuously ("0 passed"). A nonzero count can ALSO be vacuous: a filter
   substring that collides with a pre-existing passing test (m-66aff8's a3 —
   `refus` matched `dirty_tracked_tree_is_refused_...`, gating green with
   zero implementation). Grep existing test names for your substring before
   shipping the contract; match ONLY the not-yet-written test (or use
   `--exact`).
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
10. **Children are hostile by default (2026-07-28 hardening):** every
    prompt-injectable child spawns `env_clear`d (`agent_env.rs`) — never
    reintroduce ambient inheritance; the sanctioned credential channels are
    `contractEnvPassthrough` (contract commands) and the workspace contract's
    `secrets` (bootstrap/data hooks). Enforced sandboxes are honored only by
    the claude backend (validation rejects other pairs — don't widen without
    a real spawn wrapper). `serve.token` is mutation authority, `serve.read.token`
    reads only; both must stay unreadable inside sandboxes
    (`authority_read_deny_paths`). Engine-run gates (validation-round,
    final-gate, and merge-gate commands) also execute worker-authored code:
    they run WRAPPED in the resolved worker sandbox profile when
    `sandbox.enforce != off` (`command_exec::GateSandbox`,
    `run_bounded_gate_command_sandboxed`); `off` keeps the env-only posture
    byte-for-byte. The gate wrap's supervision policy is gate-SPECIFIC
    (ticket gate-sandbox-supervision-dogfood, `gate_profile_extras`):
    `(allow signal (target same-sandbox))` lets a wrapped gate signal its
    OWN descendant tree (never host processes) so `cargo test --workspace`
    runs green as a wrapped contract command — the session profile
    generator stays untouched, and what no sandbox can host (setuid
    `/bin/ps` exec, nested `sandbox_apply`) skips with the detectable
    `SKIP-UNDER-WRAP` marker, held green by the `rust-macos-wrapped-suite`
    CI job. Validators never see the real checkout:
    each session runs in a throwaway snapshot (`validator_snapshot.rs`,
    warmed `target/` copy included) and only its verdict crosses back — and
    the snapshot is separation, not containment, so validator sessions are
    ADDITIONALLY wrapped regardless of `sandbox.enforce`
    (`sandbox::resolve_validator_containment`): the snapshot is the sole
    writable root, the real checkout's source tree is read-denied, and the
    shared `.git` is readable but write-denied; uncontainable
    platforms/backends FAIL CLOSED by default (14th-pass reversal of the
    224fa73 loud-degrade decision — the degrade reopens the
    modify→use→restore path; `validatorAllowUncontainedDegrade` in
    `.kranz/config.json` is the explicit per-repo opt-in back to the loud
    per-round degrade). The `validator.tamper` fingerprint on the real checkout is
    the tripwire (defense-in-depth) whose drift means the isolation itself
    failed.

## Tracked vs runtime

Committed: `.kranz/merge-gates.json`, `.kranz/workspace.json` (workspace
contract, validated at draft/approve; `crates/engine/src/workspace_contract.rs`),
`.kranz/routing-rules.json` (routing rules, base-branch-owned, validated at
draft/approve; `crates/engine/src/routing_rules.rs`, docs/routing-rules.md),
`.kranz/tickets/<slug>.md`, `.kranz/tickets/<slug>.notes.jsonl` (append-only
discussion sidecar, D-BW-3; `crates/engine/src/ticket_notes.rs`),
`.kranz/domain-denylist.json` + `.kranz/domain-allowlist` (clean-room lint
policy — salted hashes and reviewed waiver fingerprints only, never readable
terms; `crates/engine/src/domain_lint.rs`, docs/domain-lint.md), and each
mission's `plan.md` / `plan.json` / `report.md` (on the mission branch). Gitignored runtime
(never commit): `events.jsonl`, `state.json`, `runs/`, `control/`,
`missions/*/workspace/` (container-provider compose files),
`.kranz/config.json`, `serve.token`, `serve.read.token`,
`.kranz/domain-terms.local` (plaintext lint vocabulary), `.kranz/tickets/*.status`,
`.kranz/hook-status/` (ephemeral hook-signal projection — registrations +
latest signal per run; `crates/engine/src/hook_status.rs`).
Ticket lifecycle state is committed in the ticket .md itself: the additive
`state:` frontmatter key (`open` default; terminal `done`/`superseded`/
`wontfix`, optional `state-note:`) is the single source of truth — the
gitignored `.status` sidecar is a write-through cache of it (frontmatter
wins on conflict, logged; absent key = sidecar governs as before). Reads
resolve via `Ticket::read_state`; lifecycle writes go through
`Ticket::write_lifecycle` (both files); `kranz ticket migrate-state`
(dry-run, `--yes` to apply) folds existing terminal sidecars into
frontmatter, skipping git-dirty tickets by name
(`crates/engine/src/migrate_state.rs`).

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
