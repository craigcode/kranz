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

Citizenship, for kranz, means participating in Gas City's own machinery
natively — being triggered by City events, feeding City's observability, and
(where it earns its keep) being a routable target for City's own routing
primitives — instead of sitting behind a cooldown-polling order and a
private spool directory that City can't see into. Every verdict below is
grounded in `gc <cmd> --help` output captured live against `gc 1.3.2` on
this machine (quoted sparingly, never extrapolated to flags or subcommands
not shown); nothing here was tested against a live supervisor, since no city
is registered (see Current state). The method is verdict-first: each
mechanism gets adopt, defer, or reject, with the reason stated against the
Constraints and invariants section above and the three futures from the
spike verdict (heterogeneous fleets, distribution, cross-machine execution).
Rejecting a mechanism is as informative as adopting one — it means kranz
already owns that rail, or that owning it would cost an invariant.

### gc order — adopt

`gc order` pairs a trigger — "cooldown, cron, condition, event, or manual"
— with an action (a formula or an exec script); `kranz-dispatch` is
currently wired as a cooldown-triggered exec order. Moving the trigger from
cooldown to `event` (matching on `bead.created` or `bead.ready`-shaped
events for the `kranz` label, per the trigger kinds `order --help` lists)
would let dispatch fire the moment a labelled bead is ready instead of
waiting out a fixed sleep — lower latency, fewer wasted evaluations, no
change to the exec-deadline constraint since the order still just claims
and spools. This is a config change to `orders/kranz-dispatch.toml`, not a
new invariant surface: dispatch still claims and returns within the order's
context deadline (constraint 1), still refuses `skipScrutiny` rigs
(constraint 3), and still leaves merge policy alone (constraint 6). Its
value doesn't depend on fleets, distribution, or cross-machine — it's a
strict improvement for the single-operator case today, which is why it's
adopt rather than defer.

### gc hook — defer

`gc hook [agent]` "finds routed work using the agent's `work_query` config"
and, with `--claim`, "atomically claim[s] one routed work item for the
current session." This is the same job `kranz-dispatch` + the spool
directory do by hand: spool-and-drain is a private, kranz-specific
re-implementation of routed-work claiming. If `kranz-city-worker` were
registered as a City agent with a `work_query` matching the `kranz` label,
it could call `gc hook kranz-worker --claim --json` directly instead of
draining `.env` entries a separate dispatch order wrote — collapsing the
current two-script split (dispatch claims-and-spools, worker drains) into
one worker that claims its own work. That's a real simplification, but it
requires validating agent registration and `work_query` semantics against a
live supervisor, and Current state is explicit that this pack has **never**
run against one — no city is registered on this machine and this plan does
not register one. Defer, named trigger: once the pack is validated against
a live registered city supervisor (the same gap flagged in Current state
for the `agent.toml` wiring), re-evaluate collapsing dispatch into a
hook-driven claim.

### gc sling — defer

`gc sling [target] <bead-or-formula-or-text>` routes a bead (or ad hoc text,
or a formula instantiation) to a named session or agent target, with
`default_sling_target` as the rig-level fallback. Citizenship would mean
`kranz-worker` becoming a legitimate sling target — `gc sling
kranz-worker BL-42` — so a human or another agent can hand kranz one bead
on demand instead of only going through the label-drain dispatch path.
For a single operator, this adds little over calling `kranz exec` directly
or filing a labelled bead for the existing dispatch order to pick up. Its
differential value shows up specifically when there's more than one
plausible target to choose among — i.e. the heterogeneous-fleets future
(kranz missions routed alongside codex/gemini agents under one router).
Defer, named trigger: a heterogeneous fleet (kranz plus at least one other
agent type) is actually assembled under one City router.

### gc mail — reject

`gc mail` implements messaging "as beads with type=\"message\"", with
`send`/`inbox`/`reply`/`check --inject` for agent-hook delivery. The
outbound half is already adopted: `kranz-run-bead`'s exit-2 path calls `gc
mail send human --notify` for escalations, per the exit-code contract.
The inbound half — feeding a human's mail reply back into a blocked
mission's guidance — would duplicate a channel kranz already owns:
docs/gascity.md is explicit that a blocked mission is steered "from kranz's
own surfaces (web dashboard, Slack thread, CLI)," with the escalation mail
only pointing at where to go. Building a second, inbound-mail guidance path
for the same blocked mission creates two sources of truth for one decision.
Reject: duplicates kranz's own guidance rails, which is exactly the
capability-neutral overlap (beads/orders/mail vs. kranz's own
tickets/queue/Slack) the spike verdict already priced in.

### gc handoff — reject

`gc handoff` sends self- or remote-context-handoff mail and, "for
controller-restartable sessions," requests a controller restart of that
named session (or supports `--auto` for provider PreCompact hooks). Its
entire model is a named, controller-managed interactive session with a
context window to hand off. `kranz-city-worker` is neither: constraint 5
and the pack's own `agent.toml` deliberately declare a `long_running`
agent with **no** `[[named_session]]` — "the City sees one opaque agent
type, not a resident mayor" — specifically to avoid adding kranz to the
named-session surface that `gc init`'s mayor and its restart/tmux lifecycle
occupy. Adopting handoff would mean declaring kranz-worker as a
controller-restartable named session, which is the surface constraint 5
exists to keep kranz out of. Reject: violates invariant 5 (no named
sessions).

### gc nudge — reject

`gc nudge status` "inspect[s] queued and dead-letter nudges for a session"
— deferred reminders queued because "the target agent was asleep or was
not at a safe interactive boundary yet." That model presumes an
interactive, named session with a notion of a safe boundary to defer
delivery until. `kranz-city-worker` has no such boundary: it is a headless
serial drain loop with a fixed poll sleep, not a named/interactive session
gc's controller tracks for wakefulness. Using nudge for it would require
the same named-session declaration handoff would, which constraint 5
rules out. Reject: violates invariant 5 (no named sessions) — there is no
session for a nudge to target.

### gc events — adopt

`gc events` reads the city/supervisor event log (`bead.created`,
`convoy.closed`, and similar DTOs over `list`/`--watch`/`--follow`); the
sibling `gc event emit` command writes to it. Bead-lifecycle events already
happen for free today — `kranz-run-bead`'s `bd update`/`bd close`/`bd
comment` calls (the documented exit-code mapping) naturally produce
`bead.*` events as a side effect. What's missing is a `kranz.mission.*`
signal distinct from bead state: "started" (worker picked up the spool
entry), "blocked", "complete" with the report line, emitted via `gc event
emit` at the same points `kranz-run-bead` already maps exit codes. This is
a small, explicit extension to the documented exit-code contract (not a new
ad hoc mutation) and it pays off today for anyone watching `gc events
--follow` or `gc analyze`/`gc dashboard`, independent of the three futures
— though it matters most once there's more than one kranz instance or rig
to watch (distribution, cross-machine). Adopt: extend the documented
contract in `kranz-run-bead` to emit `kranz.mission.{started,blocked,complete}`
alongside the existing bd mutations.

### gc formula — defer

A formula is "a reusable TOML method for how multi-step work should be
done (a bead is the work itself)," instantiated via `gc formula cook` and
routed via `gc sling --formula`. Exposing "validated kranz mission" as a
composable formula step would let other City workflows chain off a kranz
run — e.g. sling a bead through a formula step that hands it to kranz,
waits for the branch, then triggers a review or merge step elsewhere in the
same v2 workflow. That only pays off once kranz is meant to be composed
with steps other participants own, which is precisely the distribution
future the spike verdict named: "a published `kranz` pack as the
validated-mission rig type in the gastown ecosystem." Until kranz is
published/distributed as a reusable rig type for other formulas to build
on, there is no second party to compose with. Defer, named trigger: the
kranz pack is published/distributed as a rig type consumable by other
formulas.

### gc convoy — reject

A convoy is "a named graph of beads with dependencies" used to group
related issues via tracks-dependencies, with status/land/stranded
lifecycle commands — explicitly distinct from v2-formula workflow DAGs.
Tracking multi-bead kranz work (a feature broken into several dispatched
beads) as a convoy would duplicate a decomposition kranz already owns and
is running right now: this very mission is a plan decomposed into
features, each with its own scoped task and validation criteria, tracked
by kranz's own mission/feature/task rails, not by beads-with-dependencies.
Layering a convoy graph over the same multi-part kranz work would produce
two competing dependency graphs describing one decomposition. Reject:
duplicates kranz's own mission/feature decomposition rails.

### gc converge — reject

`gc converge` runs a "root bead + formula + gate = repeat until the gate
passes or max iterations," driven automatically off `wisp_closed` events —
a bounded, City-level iterative refinement loop. Kranz already runs a
bounded refinement loop at a finer grain, inside a single mission: adversarial
validators plus fix cycles bounded by `KRANZ_MAX_CYCLES`, entirely within
one `kranz exec` invocation, before the exit code (and hence the bead) ever
changes state. Wrapping that in a second, coarser converge loop at the City
level would double-bound the same retry behavior in two places with two
different iteration limits and no clear precedence between them. Reject:
kranz's own fix cycles subsume converge's role for the scope kranz owns
(one bead, one validated branch).

## Staged roadmap

*Populated by a later feature.*

## Design decisions

*Populated by a later feature.*

## Ticket-ready briefs

*Populated by a later feature.*
