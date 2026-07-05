# Kranz × Gas City — citizenship plan of record

Date: 2026-07-05. Grounded against `gc 1.3.2` (the literal output of
`gc version` on this machine). This document is the plan of record for
making kranz a full citizen of Gas City; it is a companion to
[docs/gascity.md](gascity.md), which records the spike findings this plan
builds on.

## Table of contents

1. [Current state](#current-state)
2. [Constraints and invariants](#constraints-and-invariants)
3. [Citizenship assessment](#citizenship-assessment)
4. [Staged roadmap](#staged-roadmap)
5. [Design decisions](#design-decisions)
6. [Ticket-ready briefs](#ticket-ready-briefs)

## Current state

The pack lives at `packaging/gascity/` and is **stub-verified only**: the
happy path and escalation plumbing were proven live against stubbed/spike
conditions (docs/gascity.md), but the supervised-worker wiring below has
never run against a live City supervisor, and no city is registered on this
machine. Read-only, run for this document:

```
$ gc cities
No cities registered. Use 'gc register' to add a city.
```

That is the expected, current state — not an error to fix. This plan does
not register a city.

### Component inventory (pack v2)

- **`packaging/gascity/bin/kranz-dispatch`** — the fast-path dispatch order,
  run under `gc order run` on a cooldown. Since `gc order exec` enforces a
  context deadline (docs/gascity.md lesson 1), this script does no mission
  work itself: it drains READY beads carrying the `kranz` label
  (`gc bd ready --label kranz --json`), and for each one:
  1. Resolves a target rig checkout — `KRANZ_RIG_DIR` wins when set
     (single-rig city); otherwise the bead id's prefix is resolved against
     `gc rig list --json` (multi-rig routing).
  2. Refuses to spool for rigs whose `.kranz/config.json` sets
     `skipScrutiny: true` — the letter-over-spirit risk from
     docs/gascity.md lesson 3 — unless the operator has explicitly set
     `KRANZ_ALLOW_UNVALIDATED=1`. The bead is left open with an explanatory
     comment instead.
  3. Claims the bead (`gc bd update <id> --status in_progress`) and spools a
     ticket-shaped mission brief (`## Goal` / `## Context` /
     `## Scoping answers` / `## Acceptance hints`, built from the bead's
     `title`/`description`/`acceptance_criteria`) plus a sidecar `.env` file
     into the spool directory (`KRANZ_SPOOL`, default
     `$GC_CITY/.gc/kranz-spool`).
  No mission executes here — dispatch and get out of the way.

- **`packaging/gascity/bin/kranz-city-worker`** — the supervised,
  long-running execution side. Intended to run as a City-supervised named
  session (see `agents/kranz-worker/agent.toml` below) or under any process
  manager. It takes a single-instance lock (`mkdir` on
  `$SPOOL/.worker-lock`, released via an `EXIT INT TERM` trap) so a second
  worker exits quietly rather than racing the first, and drains the spool
  **strictly serially**: pick the oldest `.env` entry, hand it to
  `kranz-run-bead`, sweep the entry (brief, env, and the runner's
  `events.log` sidecar), repeat. `--once` drains at most one entry and exits
  (used for tests/cron); with no argument it loops forever, sleeping 15s
  when the spool is empty. This replaces the spike's unsupervised `nohup`
  runner (docs/gascity.md lesson 1) with a supervised drain loop.

- **`packaging/gascity/bin/kranz-run-bead`** — the per-bead mission runner,
  invoked by `kranz-city-worker` (previously invoked detached via `nohup` in
  the spike). Runs `kranz exec -f <mission.md> --max-cycles <N>` in the
  resolved rig checkout and owns the entire exit-code → City-state mapping
  (see the exit-code contract below), using `gc --city <city> bd ...` for
  bead mutations. It encodes the bd flag dialect learned live
  (docs/gascity.md lesson 4): `bd close` takes `--reason`, `bd comment`
  takes the text positionally, and neither accepts `-m`.

- **`packaging/gascity/agents/kranz-worker/agent.toml`** — declares
  `kranz-worker` (command `kranz-city-worker`) as a `long_running` City
  session with no LLM cast (`[agent]` block only — no `[[named_session]]`,
  since the pack's whole premise is that the City sees one opaque agent
  type, not a resident mayor). This is **intended wiring, validated against
  the gc 1.3.2 docs but never validated against a live supervisor** — the
  spike city ran unregistered, and this mission does not register one
  either.

### The exit-code contract (the integration API)

`kranz exec`'s exit code is the entire return contract handed back to City,
confirmed against `crates/cli/src/exec.rs` (`exit_code_for`,
`EXIT_UNDERSPECIFIED`) and docs/gascity.md:

| exit | meaning        | City effect (in `kranz-run-bead`)                                          |
|------|----------------|-----------------------------------------------------------------------------|
| 0    | complete       | `bd close --reason "kranz mission COMPLETE — <report line>"`                |
| 2    | blocked        | bead set to `blocked` + comment + `gc mail send human --notify` ESCALATION  |
| 3    | underspecified | bead reopened (`--status open`) + "refine the brief" comment                |
| 1    | failed         | bead reopened (`--status open`) + failure comment                           |

`exit_code_for` maps terminal `MissionStatus` values `Complete → 0`,
`Failed → 1`, `Blocked → 2`; any non-terminal status reaching that function
is treated as a failure (1). `EXIT_UNDERSPECIFIED = 3` is a distinct path
handled before a mission directory is even created, when the scrutiny gate
or ticket parse finds the plan un-runnable headlessly (`NotReady`) — it
never reaches `exit_code_for`. This matches the pack's mapping exactly:
`kranz-run-bead`'s `case $CODE in 0|2|3|*)` block reproduces this table
verbatim.

Field mapping on the way in (bead → mission brief, per `kranz-dispatch` and
confirmed by docs/gascity.md): bead `title → ## Goal`, `description → ##
Context`, `acceptance_criteria → ## Acceptance hints`.

### Spike verdict (cited, not re-litigated here)

Per docs/gascity.md's Verdict: for a single operator on one machine the
integration is **capability-neutral** — beads/orders/mail duplicate kranz's
own tickets/queue/Slack, and City's richer machinery (formulas, convoys,
sessions) is deliberately bypassed by the one-opaque-agent boundary. It
earns its keep in exactly three futures: **heterogeneous fleets** (kranz
missions beside codex/gemini agents under one router), **distribution** (a
published `kranz` pack as the validated-mission rig type in the gastown
ecosystem), and **cross-machine execution** (City k8s runtimes behind
kranz's `AgentBackend` seam, someday). Until one of those is wanted, daily
work stays on kranz's own rails.

## Constraints and invariants

Each of docs/gascity.md's six numbered lessons, distilled into a standing
constraint this plan must respect:

1. **Orders dispatch and return.** `gc order exec` enforces a context
   deadline; it killed a mid-mission dispatcher live. No City order may host
   a multi-minute mission. Dispatch-side code claims work and hands it to a
   supervised long-running process (or enqueues into kranz's own queue) —
   never runs `kranz exec` inline inside an order.
2. **The bead title is the prompt.** Field-mapping discipline is load-
   bearing, not cosmetic: the title must carry the goal, the description
   carries constraints, and acceptance carries testable outcomes. A
   goal-in-acceptance bead reliably produces a confidently wrong mission.
3. **Scrutiny stays ON for autonomous dispatch.** Cheapest config plus no
   adversaries produces letter-over-spirit compliance (proven live: a
   mission invented its own missing input and passed its own tautological
   acceptance). Rigs with `skipScrutiny`/`skipFunctional` set are
   trusted-brief-only; City-dispatched autonomous missions must refuse
   validators-off rigs by default (`KRANZ_ALLOW_UNVALIDATED=1` is an
   explicit, auditable opt-out, not a default).
4. **bd flag dialects are not interchangeable.** `bd close` takes
   `--reason`; `bd comment` takes text positionally; `-m` belongs to
   neither and gc's wrapper aborts on unverifiable args rather than
   substring-resolving. Any new bd-calling code must be written and tested
   against this dialect, not assumed from other CLIs' conventions.
5. **`gc init` is a mutating, machine-wide action and must never run in a
   mission or CI context.** It registers a machine-wide launchd supervisor
   and immediately spawns a live, paid `claude --dangerously-skip-permissions
   --effort max` mayor session; `gc stop` can orphan that session's tmux
   server (observed twice; requires manually reaping `tmux -L <city>`). The
   kranz pack intentionally declares no named sessions to avoid adding to
   this surface.
6. **Merge/push policy belongs to the dispatching side, not to kranz
   exec.** `kranz exec` leaves validated work on the mission branch
   (`pushed=false` reported on stdout) — it does not merge or push. Any
   City-side integration that wants merged/pushed output must implement
   that policy itself; kranz's contract stops at a validated branch plus a
   report line.

Invariants this plan holds fixed, independent of the lessons above:

- **Kranz remains fully usable standalone.** The Gas City pack is optional
  integration surface, never a dependency of core kranz behavior — kranz
  must work identically with the pack absent.
- **Missions get no network access.** Publishing (e.g. to gastownhall.ai)
  and any other network-facing action are human-gated, never performed
  autonomously by a mission or worker.
- **Kranz missions and workers never run state-mutating `gc` commands.**
  Read-only City inspection (`gc version`, `gc help`, `gc <cmd> --help`,
  `gc <cmd> --json-schema`, `gc cities`, `gc bd ready/show --json`, `gc rig
  list --json`) is fine; anything that registers, starts/stops, or mutates
  City state (`gc init`, `gc register`, `gc order run`, `gc sling`, `gc
  handoff`, `gc mail send`, bead/bd mutations outside the documented
  `kranz-run-bead` mapping) stays out of kranz's own code paths and out of
  anything this plan schedules to run unattended.
- **The exit-code contract is the stable API.** `0`/`1`/`2`/`3` (complete /
  failed / blocked / underspecified) is the entire integration surface
  between kranz and City; any future pack work builds on this contract
  rather than inventing a richer one.

## Citizenship assessment

*Populated by a later feature.*

## Staged roadmap

*Populated by a later feature.*

## Design decisions

*Populated by a later feature.*

## Ticket-ready briefs

*Populated by a later feature.*
