# Kranz roadmap

v1 (plan phases 1–3) is complete: headless engine, resumability, CLI + planning
TUI, dashboard + Tauri shell — hardened by live missions and an adversarial
review. What follows, in priority order. Each milestone states its "done when"
criteria, in keeping with the product's own contract-first ethos.

## M1 — Prove it at scale ✅ (acceptance proven twice; report + calibrated estimates shipped)

The plan's flagship acceptance was run end-to-end twice (Opus and Fable
orchestrators): a 2-milestone / 5-feature mission with a deliberately seeded
defect, default models, fully unattended. The reproducible manual CI smoke
workflow now makes that proof repeatable after engine changes; each live
dispatch remains an explicit, token-spending operator action.

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

## M2 — Mission lifecycle completeness ✅ (shipped 2026-07-08: mid-mission re-planning + hygiene + preflight)

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

## M2.9 — Slack as a full control surface ◑ (shipped in slices: new/plan/approve, config, pause/resume/work, App Home, deep links; full goal→complete lifecycle live-validated 2026-07-12: mission m-6a20dc driven draft→approve→work→merge entirely from Slack — docs/knowledge/surfaces/slack-commands.md)

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

## M3 — Parallel workers ✅ (done-when met 2026-07-04: live A/B measured 1.92× faster at lower cost with a self-healed merge conflict — docs/m3-measurement.md; corruption soak 20/20 — scripts/soak.sh. `--sequential` stays the default pending more live runs)

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
- Soak harness (`scripts/soak.sh`, default 20 iterations) covers the
  corruption half of the done-when: repeated parallel missions cycling
  clean / crash+resume / real-merge-conflict variants, with contiguous-seq,
  fold, snapshot, worktree and branch invariants asserted after every run.
  The wall-clock/cost half was proven by the live A/B recorded in
  `docs/m3-measurement.md`.

Done when: a 2-milestone mission with independent features completes in
materially less wall-clock than sequential at comparable cost, with zero
event-log corruption across 20 repeated runs.

## M4 — Windows first-class + distribution ◑ (CI green on ubuntu+windows incl. kill/resume; v0.1.0 tagged 2026-07-04 with linux/macos/windows binaries attached; remaining: crates.io + Homebrew, gated on the repo going public)

The code is path-safe and lock-file based per §9 and is proven on Windows CI,
including kill/resume. Tagged cross-platform binaries exist; crates.io and
Homebrew publication remain gated on the repository going public.

- Push to a remote so the existing CI matrix (ubuntu + windows) actually runs;
  fix what Windows breaks. Process-tree kill via Job Objects (the documented
  unix-process-group gap).
- The §4.3 kill/resume acceptance test on Windows (`taskkill /F`).
- Packaging: versioned releases, `cargo install kranz` from crates.io
  and/or Homebrew tap; Tauri app bundles (`.dmg`/`.msi`).

Done when: CI is green on both platforms including the kill/resume test, and
a new machine goes from nothing to `kranz plan` without cloning the repo.

## M5 — Deeper validation & automation ◑ (exec, functional QA, OTEL, and secret scanning shipped; skill authoring remains)

- [x] Functional QA via browser/computer-use driven by the validator, for target
  repos with a scriptable run harness (same prerequisite Factory imposes).
- [x] `kranz exec -f mission.md` — fully headless missions for CI (plan file in,
  exit code out; no interactive approval, contract gates only).
- Skill capture: the lessons loop records reusable mission knowledge, but
  proposing human-approved `.claude/skills` entries from repeated worker
  patterns remains open.
- [x] OTEL export of engine events (opt-in `kranz otel` sidecar shipped).
- [x] Real secret scanning replacing the regex scrub — shipped 2026-07-07
  (`0b731d0`): curated + entropy detectors, redact-at-write ingest gate with
  `secret.redacted` audit events, merge pre-gate + CI job + `kranz scan`,
  fingerprint waivers via `.kranz/secret-allowlist`. Design of record:
  docs/scoping/secret-scanning.md (all D-A..D-D decided). Follow-up: verify
  the always-on entropy detector's false-positive rate on real logs.

## M6 — Cloud missions ◑ (scoped push, Dockerfile, deploy docs, exec --push shipped; live deploy user-gated)

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
- **Workspace contract + provider seam**: a tracked, base-branch-owned
  workspace contract describes bootstrap, services, health checks, dynamic
  ports, data clone/reset, previews, and secret *names* (never values).
  Provisioning is separate from `AgentBackend`: start with the existing local
  worktree provider, then add container and remote/Coder-style providers. Pin
  the effective provider and template/image version in the mission record.
  Proposed decisions and impact-ordered tickets:
  [`docs/scoping/workspace-contract.md`](scoping/workspace-contract.md).
- **Runnable data plane**: worktrees isolate source, not ports, Docker state,
  caches, or databases. A cloud/parallel workspace must be able to receive an
  isolated application runtime and, when configured, a de-identified golden
  data clone with readiness and reset hooks.
- Platform notes: Railway/Fly/VPS are the right shape; RunPod CPU pods only
  (API-bound workload, no GPU); Lambda is a non-fit (hours-long stateful
  processes vs 15-minute stateless invocations).

Done when: a mission file pushed to a repo runs unattended in a throwaway
workspace, proves its declared services/data are ready, and delivers a
reviewable `kranz/*` branch; a Railway-hosted `kranz serve` takes a mission
from browser conversation to COMPLETE with no local Kranz install; a human
can open declared previews or take over the same workspace; and a leaked
dashboard URL without the token reveals nothing and mutates nothing.

## M7 — Worker sandboxing ◑ (tiers 1–2 shipped; container provider and live cross-platform proof remain)

Containment now includes dedicated worktrees, out-of-contract write auditing,
environment hygiene, macOS Seatbelt `enforce: "fs"`, and Linux bubblewrap
`enforce: "fs" | "fs+net"` with fail-closed platform/preflight behavior.
macOS hostname egress cannot be honestly enforced by Seatbelt, Windows parity
and the container workspace provider remain open. The sandbox still
complements scrutiny: it bounds what CAN happen; validators judge what DID.

Done when: a deliberately hostile brief under `enforce: "fs+net"` leaves zero
writes outside its worktree + mission dir with blocked attempts surfaced as
findings; a normal mission's contract commands still pass under the sandbox
at <~10% wall-clock overhead; and the primary checkout never changes branch
during any mission, sequential included.

## M8 — Multi-repo operation ◑ (implementation shipped; cross-language end-to-end proof remains)

Kranz is per-repo by construction (`.kranz/` state, tickets, missions,
calibration, lessons all live in the repo) — but the operator surfaces
assume exactly one repo. Make "point kranz at any repo" true end to end,
in any language.

- [x] **Per-repo merge gates**: the gated Merge reads the live base branch's
  tracked `.kranz/merge-gates.json`; gates declare a command, cwd, and optional
  changed-path prefixes. Missing/invalid/conditional-only suites fail closed,
  and a mission cannot weaken the gate file that judges its own diff. This
  repo carries the Rust/dashboard suite explicitly; other languages carry
  their own commands.
- [x] **Multi-root host design**: one serve process composes one existing
  `MissionHost` per operator-configured repo; API and mission identity become
  repo-scoped, queues remain local under a fair bounded scheduler, one process
  token protects the local host, and Slack routes explicitly or fails closed.
  See [the accepted M8 decisions](scoping/m8-multi-root-host.md).
- [x] **One Slack bridge, many repos**: per-repo bridges can't work — Slack
  socket mode load-balances events across connections from one app, so N
  bridges each see 1/N of commands. Instead the single bridge routes:
  channel→repo default mapping in config plus an explicit repo tag to
  override in shared channels; mission threads already carry affinity
  (slack-threads.json). Spend-adjacent verbs keep the allowlist gate
  per repo.
- [x] **Serve story**: implement the accepted one-serve-many-repos host catalog,
  repo-scoped API, and fair queue scheduler. The dashboard repo picker follows
  as its own ticket after those routing boundaries exist.
- [x] **Dashboard project picker**: groups, pins, search, repository-scoped
  routes, unavailable-root errors, and queued/running/needs-input/unmerged/
  failed activity counts on top of the host catalog.
- [x] **Fresh-repo onboarding**: `kranz init` first run (additive scaffold,
  gitignore template, gate/registration answers), cold-start calibration honesty
  ("based on 0 missions" must read as the warning it is), and promoting
  the proven workerIsolation=worktree default so new repos start isolated.

Done when: a TypeScript repo goes ticket → draft → queue → worktree run →
gated merge (its own gates) without touching this repo's config; two
repos operate from one Slack workspace with unambiguous routing; and a
brand-new repo's first mission runs with no hand-editing beyond
`kranz init` answers.

## Product pattern notes from Warp/Oz/Factory (2026-07-08), Cursor (2026-07-09), Monaco (2026-07-10), and Mission Control (2026-07-13)

External scan: Warp Agent/Oz and Factory's Droid/AutoWiki surfaces are useful
as UX/product benchmarks, not architecture targets. Cursor is now a stronger
direct overlap on unattended agent work (Agents Window, cloud agents,
worktrees, automations, hooks, Agent Review, and Grok 4.5 in its first-party
model pool), so "agent orchestrator" is no longer a useful differentiator.
The broad "agentic IDE" lane (terminal replacement, built-in editor/LSP,
voice, general local coding environment) still belongs to the sgian side
product, not kranz. The kranz-compatible lessons are narrower and should
reinforce the mission/audit/gate model:

AgentSystemLabs Mission Control reinforces the same boundary from the other
side: it is a polished desktop PTY/session manager, not a mission-validation
engine. Borrow sensing and operator ergonomics, not the IDE shell. The
follow-up backlog (rewritten after adversarial review) is sequenced as:

- **Near-term P2 slices shipped:** `repo-knowledge-ranked-brief-injection`,
  `post-complete-pr-handoff-no-push`, `backend-readiness-quota-preflight`,
  `workspace-sandbox-visibility` (local visibility only),
  `m8-multi-root-host-design` (prerequisite for the picker).
- **Later / gated (P3):** `structured-human-question-events` (unify with
  grants/NeedsContext), `agent-hooks-status-signals` (blocked on Cursor
  backend lane), `multi-repo-project-picker` (blocked on multi-root host
  design).

- **A worktree is not a workspace.** Monaco's failed local-worktree phase
  exposed the shared runtime problems source isolation does not solve: port
  collisions, Docker contention, dependency setup, disk pruning, and awkward
  human handoff. Keep worktrees as the local source-isolation provider, but
  make a complete runnable application environment the unit M6 provisions.
- **Buy the substrate; keep the policy plane.** Do not build a VM scheduler or
  cloud IDE. Integrate with Coder/container/vendor providers behind a small
  workspace seam while kranz continues to own the plan, consent, audit,
  validation, and delivery contract.
- **Seeded data is validation infrastructure.** Treat a scoped, de-identified
  golden data clone and its migration/reset lifecycle as first-class workspace
  inputs. This is likely to improve functional validation more than another
  model integration for database-backed repositories.
- **Human takeover and feedback are workspace artifacts.** Record workspace
  and preview links, readiness, provider/template identity, and lifecycle in
  the event trail. GitHub PR-comment/CI-failure triggers should create audited
  fix-features or follow-up missions rather than hide polling loops in prompts.

### M6 workspace backlog (impact order, 2026-07-25)

Scoping: [`docs/scoping/workspace-contract.md`](scoping/workspace-contract.md)
(proposed D-A…D-H). Priority `1` = highest impact / do first.

| Order | Pri | Ticket | Why this order |
|------:|----:|--------|----------------|
| 1 | 1 | `workspace-contract-design` | Accept D-A…D-H; unblock honest drafts |
| 2 | 1 | `workspace-contract-schema` | Contract artifact without needing cloud |
| 3 | 1 | `workspace-bootstrap-preflight` | **Best local ROI** — flimsy worktree fix |
| 4 | 1 | `workspace-provider-seam` | Trait + local-worktree; parallelizable after design |
| 5 | 2 | `local-container-workspace` | Ports/Docker isolation (≠ Tier-3 sandbox) |
| 6 | 2 | `workspace-provider-pin-at-approval` | Consent-time pin + artifact surfacing |
| 7 | 2 | `golden-data-hooks` | Seeded data for functional validation |
| 8 | 3 | `trigger-ci-pr-fix-mission` | AFK CI/PR loop as audited missions |
| 9 | 3 | `workspace-remote-coder-provider` | Thin buy-substrate adapter |
| 10 | 3 | `workspace-idle-hibernate` | Remote cost lifecycle |

Related but separate: `tier3-container-sandbox` remains the **sandbox**
containment ticket; share runtime code with `local-container-workspace` when
useful, keep product APIs distinct.

- **Cursor CLI / Grok 4.5 backend.** Treat Cursor as a runtime/backend to
  absorb, not an IDE lane to chase: add a `backend_cursor` (or ACP-backed
  equivalent if the CLI print mode lacks enough event structure) so workers
  and validators can use Cursor first-party models such as Grok 4.5 and
  Composer under the existing kranz mission contract. The backend must prove
  terminal report text parsing, model/cost capture, worktree cwd discipline,
  permission mapping, and the no-push/no-main-write invariants before it is a
  default option.
- **Positioning correction.** Cursor now credibly owns much of the polished
  agentic development-platform story. Kranz should describe itself as the
  git-native mission recorder, consent gate, and validation harness around
  headless agents, not as a generalized agentic IDE or model router.
- **Automation triggers without losing the gate.** Cursor's automations make
  event-triggered background agents feel normal. Add kranz-side triggers only
  where they preserve explicit mission semantics: GitHub/Slack/Linear/webhook
  events should create or draft tickets, queue approved plans, or request
  human approval, never silently merge or push.
- **Cloud/network policy as first-class mission config.** Cursor's cloud-agent
  docs make egress policy, secret visibility modes, artifact hosting, and
  managed/self-hosted execution part of the operator story. Fold the same
  concerns into M6/M7 as mission/profile fields, with consent diffs before
  approval and event-log records of the effective policy.
- **Artifact evidence.** Cursor attaches screenshots, videos, and log
  references to PRs. Kranz's `report.md`/mission record should grow a simple
  artifact index for validator screenshots, browser/computer-use recordings,
  command logs, and demo URLs, all referenced from the append-only event trail.
- **Plan UX as an audited artifact.** Keep `plan.json`/`plan.md` as the source
  of truth, but make the dashboard feel more like a plan workspace: revision
  timeline, compare/restore prior revisions, clearer mid-mission plan diffs,
  and "execute this milestone/section" controls that still go through normal
  mission events and approval gates.
- **Human diff review loop.** Add an interactive review surface for delivered
  mission diffs: inline comments anchored to files/lines, batch submit feedback,
  side-by-side previews for mission artifacts, and route the batch into a
  fix-feature, revision, or validation pass. This is a better fit for kranz
  than agent-on-agent code review because the human stays the consent boundary.
- **Repo knowledge with freshness.** Advance the repo-knowledge-store lane:
  committed `docs/knowledge/` notes, generated `.kranz/knowledge/` cache,
  visible indexing/freshness status, worktree-aware selection, and injection
  into initial planning plus M2 revised-planning. Prefer reviewable Markdown
  over opaque embeddings as the durable layer. Borrow Factory AutoWiki's
  two-pass repo survey, incremental refresh by source commit, and multi-surface
  presentation, but keep third-party wiki output as seed/benchmark material
  rather than trusted mission input.
- **Mission profiles and permissions.** Package backend choice, model/effort,
  sandbox policy, command grants, MCP/tools, ask-question behavior, and autonomy
  floors into named profiles (`Safe`, `Autonomous`, `Cloud`, `Local review`).
  Profiles should render as a consent diff before plan approval and live in repo
  config when project-specific.
- **Run record completeness.** Treat every mission, local or cloud, as a
  shareable audit record: trigger/source, owner, profile, model/backend,
  estimate, approvals, plan revisions, commands/logs/transcripts, findings,
  costs, artifacts, and "whose move is it". This strengthens M6/M8 without
  turning kranz into an IDE.

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

For the current local v1 defaults, automatic pushes to main,
multi-user/RBAC, and org policy remain non-goals. M6 cloud/remote execution is
an explicit future, operator-gated milestone rather than a current default.
Also out of scope for kranz: terminal replacement, a general-purpose code
editor/LSP shell, voice-first coding, and the broader agentic-IDE product shape
now parked for sgian.
