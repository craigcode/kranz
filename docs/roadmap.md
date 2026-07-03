# Kranz roadmap

v1 (plan phases 1–3) is complete: headless engine, resumability, CLI + planning
TUI, dashboard + Tauri shell — hardened by live missions and an adversarial
review. What follows, in priority order. Each milestone states its "done when"
criteria, in keeping with the product's own contract-first ethos.

## M1 — Prove it at scale (confidence before features)

The plan's flagship acceptance was never run end-to-end: a 2-milestone /
5-feature mission with a deliberately seeded defect, default models, fully
unattended. Everything after this milestone builds on the confidence it buys.

- Run the §5 Phase 1 acceptance mission on a sample repo (REST endpoint with
  auth + tests, then a CLI client; seeded defect → fix-feature → green).
- **Estimate calibration**: today's pre-mission estimates ran ~10× above
  actuals on small missions. Derive `EstimateParams` from recorded per-run
  costs in completed missions (`kranz missions` data is already sufficient);
  show "based on N completed missions" alongside the range.
- **`report.md` at mission completion**: what shipped per feature (commits,
  criteria outcomes), validation history including waived findings with their
  justifications, final cost vs estimate, elapsed time. Committed beside
  plan.md; linked from missions/index.md.

Done when: the acceptance mission completes unattended with every §5 criterion
checked off; a fresh mission's estimate lands within 2× of its actual; every
completed mission ends with a committed report.md.

## M2 — Mission lifecycle completeness

Live use exposed the lifecycle edges v1 cut.

- **Mid-mission re-planning**: the orchestrator can already propose scope
  changes in conversation, but `approve_plan` is once-only. Support a revised
  plan for the *remaining* work (completed milestones immutable), with a fresh
  approval gate and a plan.md diff as the review artifact.
- **Mission hygiene**: `kranz clean` (archive or delete abandoned planning
  missions and failed husks, with confirmation), `kranz abandon <id>`
  (explicit terminal state, recorded as an event — not a deleted directory).
- **Environment preflight**: before the first worker spawns, run contract
  commands' obvious prerequisites (interpreter present, Full Disk Access for
  paths the plan touches, package manager reachable) and surface failures as
  a planning-time warning instead of a mid-mission finding.

Done when: a scope change mid-mission produces an approved revised plan
without losing completed work; `kranz missions` in a long-lived repo shows
only meaningful entries; the imsg2notion FDA failure mode is caught before
spend, not after.

## M2.5 — Full mission lifecycle from the web UI

The dashboard graduates from observer/steerer to complete surface: create,
plan, approve, and start missions from the browser (and therefore the Tauri
app), matching the Factory reference where mission creation lives in the UI.

- `kranz serve` becomes an optional mission host: it holds a MissionEngine
  per mission it created (the single-writer lock already arbitrates server
  vs CLI ownership; either can resume what the other started).
- Endpoints: create (goal+config), planning turn, request-plan
  (Ready→review / NotReady→chat), approve (same plan.json/plan.md/index
  commit), start (engine.run() as a server background task). Steering stays
  on the existing control inbox; the WS feed already carries everything the
  planning chat needs.
- UI: new-mission form on the picker, planning chat pane (the composer
  pattern exists), plan review + two-consent approve/start panel.
- Authority: mutating endpoints require a per-serve session token printed at
  startup — spending money from a browser needs more than CORS.

Done when: a mission goes goal → conversation → approved plan → COMPLETE
without a terminal ever opening, killing the server mid-mission loses
nothing, and a foreign browser origin cannot create or start anything.

## M3 — Parallel workers (plan Phase 4, flagged off by default)

The marquee deferred capability. Correctness groundwork exists (single-writer
log, engine-serialized appends); the work is orchestration.

- Orchestrator marks independent features per milestone; up to N workers
  concurrently, each in its own git worktree.
- Engine merges in declared order; conflicts synthesize a conflict-resolution
  worker task carrying both branches' reports.
- Scrutiny + functional validators run concurrently.
- `--sequential` remains the default until instrumentation (wall-clock and
  token cost per mission, recorded in report.md) proves parallel wins on real
  workloads — the plan's own bar.

Done when: a 2-milestone mission with independent features completes in
materially less wall-clock than sequential at comparable cost, with zero
event-log corruption across 20 repeated runs.

## M4 — Windows first-class + distribution

The code is path-safe and lock-file based per §9, but never proven on Windows,
and the project has no distribution story.

- Push to a remote so the existing CI matrix (ubuntu + windows) actually runs;
  fix what Windows breaks. Process-tree kill via Job Objects (the documented
  unix-process-group gap).
- The §4.3 kill/resume acceptance test on Windows (`taskkill /F`).
- Packaging: versioned releases, `cargo install kranz-cli` from crates.io
  and/or Homebrew tap; Tauri app bundles (`.dmg`/`.msi`).

Done when: CI is green on both platforms including the kill/resume test, and
a new machine goes from nothing to `kranz plan` without cloning the repo.

## M5 — Deeper validation & automation (plan Phase 5)

- Functional QA via browser/computer-use driven by the validator, for target
  repos with a scriptable run harness (same prerequisite Factory imposes).
- `kranz exec -f mission.md` — fully headless missions for CI (plan file in,
  exit code out; no interactive approval, contract gates only).
- Skill capture: orchestrator proposes `.claude/skills` entries from repeated
  worker patterns; human approves before write.
- OTEL export of engine events; real secret scanning replacing the regex scrub.

## Continuous UX backlog (no milestone, picked up opportunistically)

- `kranz run` status header / richer TUI (dashboard remains the primary
  steering surface — revisit only if demand shows up).
- Readline editing in the line-mode REPL (piped mode stays raw by design).
- Dashboard: live-mission visual verification pass, transcript search,
  mission picker archive filter.
- Engine: `tools` field on SessionSpec (tighter built-in tool restriction);
  per-turn JSON schema for streaming orchestrator turns (harden decision
  parsing at the source); mock backend "die after N messages" script knob.

## Explicitly still out of scope

Cloud/remote execution, multi-user/RBAC, and org policy remain non-goals
(plan §3); the architecture continues to not preclude them.
