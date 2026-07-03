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

## M2.5 — Full mission lifecycle from the web UI ✅ (shipped 2026-07-03)

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

## M2.75 — Mission backlog + Slack bridge ✅ (shipped 2026-07-03)

Tickets as markdown in the repo → async plan drafting → review/approve (web,
CLI, or Slack) → per-repo execution queue → Slack notifications with
threaded steering. Full design: docs/backlog-and-slack.md. Turns Kranz from
a sit-and-watch tool into scheduled work; multiplies with M6 (a hosted
backlog is a team's autonomous dev queue).

Done when: see the design doc's done-when list (merged ticket → drafted
plan → Slack approve → queued run → blocked-to-unblocked entirely in a
thread; underspecified tickets bounce back with the orchestrator's actual
questions; per-repo serialization holds).

## M2.9 — Slack as a full control surface

Extend the Slack bridge from notifications+light-steering to FULL management:
create, plan (thread conversation), review, approve, start, steer, configure,
and run the backlog from Slack; deep transcript forensics hand off to the web
UI via a deep link. Rides on M2.5's hosted-engine registry — Slack becomes a
second client of MissionHost alongside the browser. Full design:
docs/slack-management.md. Ship in slices (lifecycle → control/status → App
Home). Requires a Slack-user spend allowlist and an always-on `serve --slack`.

Done when: a mission goes goal → planning → approve → start → complete entirely
in Slack; `/kranz` fully operates the backlog; unauthorized users cannot spend;
transcripts are one tap from the thread.

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

## M4 — Windows first-class + distribution ◑ (built; user-gated on git remote)

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
- [x] `kranz exec -f mission.md` — fully headless missions for CI (plan file in,
  exit code out; no interactive approval, contract gates only).
- Skill capture: orchestrator proposes `.claude/skills` entries from repeated
  worker patterns; human approves before write.
- OTEL export of engine events; real secret scanning replacing the regex scrub.

## M6 — Cloud missions (the plan's "v3 idea", unblocked by M2.5)

Run missions on rented compute; the event-sourced core and the M2.5 HTTP
lifecycle already make Kranz location-independent. Two shapes, in order:

1. **Ephemeral (CI-shaped)**: container per mission — clone repo → `kranz
   exec -f mission.md` (M5 headless mode) → push the mission branch → exit.
   Fits GitHub Actions, Railway cron jobs, any container runner. Smallest
   new surface.
2. **Persistent host**: `kranz serve` on Railway/Fly/VPS with a volume for
   `.kranz`; full M2.5 lifecycle from the browser against a remote URL.

Design changes required (eyes open):
- **Scoped push**: amend §4.4's "never pushes" to "pushes `kranz/*` refs
  only" via a deploy key/GitHub App — never main, never merges; the human
  still reviews. Locally the rule stands unchanged.
- **Auth grows up**: token required on reads too (transcripts are source
  code), TLS via platform, `ANTHROPIC_API_KEY` instead of local OAuth.
- **Workspace provisioning**: clone-on-create (mission carries repo URL +
  ref); toolchain via `devcontainer.json` when present, one fat default
  image otherwise. This is the messy part — timebox it.
- Platform notes: Railway/Fly/VPS are the right shape; RunPod CPU pods only
  (API-bound workload, no GPU); Lambda is a non-fit (hours-long stateful
  processes vs 15-minute stateless invocations).

Done when: a mission file pushed to a repo runs unattended in a throwaway
container and delivers a reviewable `kranz/*` branch; a Railway-hosted
`kranz serve` takes a mission from browser conversation to COMPLETE with no
local Kranz install; a leaked dashboard URL without the token reveals
nothing and mutates nothing.

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
