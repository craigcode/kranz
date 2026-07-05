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

- **`packaging/gascity/pack.toml`** — the pack manifest (name "kranz", schema
  2); the file `gc lint` validates and the unit a future `gc pack` remote
  source would fetch.

- **`packaging/gascity/orders/kranz-dispatch.toml`** — the order config that
  wires `bin/kranz-dispatch` into the city (see below). `trigger =
  "cooldown"` with `interval = "5m"` runs `exec = "kranz-dispatch"` on that
  cadence; it sets no env itself, deferring routing knobs
  (`KRANZ_RIG_DIR`/`KRANZ_SPOOL`/`KRANZ_ALLOW_UNVALIDATED`) to the city's own
  environment.

- **`packaging/gascity/bin/kranz-dispatch`** — the fast-path dispatch order,
  run under `gc order run` on a cooldown. Since `gc order run` enforces an
  exec context deadline (docs/gascity.md lesson 1), this script does no mission
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

1. **Orders dispatch and return.** `gc order run` enforces an exec context
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
exists to keep kranz out of. Reject: violates constraint 5 (no named
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
rules out. Reject: violates constraint 5 (no named sessions) — there is no
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

### gc agent — adopt

`gc agent` "manage[s] agent configuration in city.toml"; runtime operations
(attach/list/peek/nudge/kill/start/stop/destroy) have moved to `gc session`
and `gc runtime`, leaving `gc agent` as config-only: `add` (scaffold),
`list`, `resume`, `suspend`. This is the registration step the pack's
`agents/kranz-worker/agent.toml` already targets — a `long_running`
`[agent]` block with `command = "kranz-city-worker"` and, deliberately, no
`[[named_session]]`, so "the City sees one opaque agent type, not a
resident mayor." Citizenship here is narrow and mechanical: `gc agent add`
(or hand-authoring the equivalent city.toml stanza) makes that scaffold
visible to the city at all, which is the prerequisite for `gc status`, the
dashboard, and (later) `gc hook`/`gc sling` targeting it. It adds nothing
resident and costs nothing while the spool is empty — a config-registration
step, not a behavioral one. Whether the *agent* should ever grow an LLM
cast (a `prompt_template`, hence `gc prime` applicability — see the next
subsection) is a distinct, undecided question, flagged for design decision
D1. Adopt: registering the existing non-LLM scaffold is pure config, adds
no named-session surface, and directly enables the status/dashboard
visibility this assessment treats as adopt below — independent of any of
the three futures, though it's also the floor requirement for all of them.

### gc prime — defer

`gc prime [agent-name]` "outputs the behavioral prompt for an agent," e.g.
`claude "$(gc prime mayor)"` — priming a CLI coding agent with city-aware
instructions from a configured `prompt_template`. It explicitly tolerates
"agents whose config intentionally lacks a prompt_template (a supported
minimal config)" as a legitimate quiet state, not an error. `kranz-worker`
is exactly that minimal config today: a `long_running` agent with no
`prompt_template` and no LLM cast, because `kranz exec` already carries its
own orchestrator/worker/validator prompting internally — there is no CLI
coding agent on the City side for `gc prime` to prime. So `gc prime
kranz-worker` today would emit only a default/empty worker prompt that
nothing consumes; `--strict` tolerates the intentionally-absent
`prompt_template` and errors on a missing or unloadable city config, a
missing or unknown agent name, or an unreadable `prompt_template`. Whether kranz
should ever grow a City-visible, primeable LLM session — for interactive
triage of a blocked mission, say — is exactly the question flagged as D1 in
the `gc agent` subsection above, and constraint 5 (no named sessions, kept
narrow specifically to stay out of the mayor's restart/tmux surface) is the
invariant any such move would need to clear. This subsection does not
decide that question. Defer, named trigger: D1 resolves in favor of a
City-visible, named LLM session for kranz-shaped work; until then, `gc
prime` has no `prompt_template` to render for a non-LLM worker.

### gc session — reject

`gc session` creates, resumes, suspends, and closes "persistent
conversations with agents" — `new`/`attach`/`nudge`/`submit`/`wake`/`pin`
and friends, all built around a named, controller-managed, interactive
chat session with continuity across suspend/resume. Kranz already has
three interactive surfaces of its own for steering a mission — web
dashboard, Slack thread, CLI — and docs/gascity.md is explicit that a
blocked mission is steered "from kranz's own surfaces," not from a second
channel. Declaring a `gc session` for kranz-worker would mean giving it
exactly the `[[named_session]]` block constraint 5 and the pack's own
`agent.toml` comment ("no resident mayor") deliberately omit, adding kranz
to the same controller-restart/tmux-lifecycle surface `gc init`'s mayor
occupies. Reject: violates constraint 5 (no named sessions) and duplicates
kranz's own interactive surfaces — the same capability-neutral overlap the
spike verdict already priced in.

### gc status — adopt

`gc status [path]` "shows a city-wide overview: controller state,
suspension, all agents with running status, rigs, and a summary count," in
text or `--json`. Once `kranz-worker` is registered (`gc agent`, above),
it appears in this overview for free — no kranz-side code, just the
registration step. That is exactly what a citizen should report upward:
whether the worker process is alive, without needing to reimplement
liveness reporting as a separate City-facing surface. This is read-only
observability, costs nothing beyond registration, and doesn't compete with
kranz's own dashboard, which reports mission-level detail (which bead,
which cycle, which validator) that `gc status`'s agent-level view was
never going to carry. Adopt: pure upward visibility of "is the worker
running," gated only on the `gc agent` registration step, valuable for the
single-operator case today and a prerequisite for all three futures'
observability needs.

### gc dashboard — adopt

`gc dashboard` "open[s] the static GC dashboard against the machine-wide
supervisor API"; scoped to a city directory (or `--city`), it enables
"city-specific panels and action forms." Citizenship means the same thing
here as for `gc status`, one level up in fidelity: once `kranz-worker` is a
registered agent, the City dashboard's per-agent panel shows it running
(or suspended, or dead) alongside every other agent in the city, which is
the aggregate, cross-agent view kranz's own dashboard is not positioned to
provide (kranz's dashboard knows about kranz missions; it has no notion of
sibling City agents). The two dashboards report at different altitudes and
don't compete: City's shows "is kranz's worker alive, among N agents";
kranz's own shows "what is this specific mission doing." Adopt: read-only,
gated on the same `gc agent` registration as `gc status`, and the natural
place a human watching the whole city — not just kranz — would look first,
which matters most once there's more than one agent or rig to watch
(heterogeneous fleets, cross-machine).

### gc doctor — defer

`gc doctor` runs diagnostic health checks — "city structure, config
validity, binary dependencies … controller status, agent sessions,
zombie/orphan sessions, bead stores, Dolt server health, event log
integrity," formula-compiler and v2-config deprecations, and "per-rig
health," with `--fix` for safe mechanical remediation. Registering
`kranz-worker` as an agent gets the generic checks (config validity,
zombie/orphan session detection, bead-store health) for free, the same way
`gc status`/`gc dashboard` visibility does. What `--help` does not show is
any extension point for a *pack-contributed* check — there's no documented
way for the kranz pack to register "probe spool depth" or "probe
mission-lock liveness" as an additional doctor check; every check listed is
generic to city/rig structure, not agent-specific health kranz would define
itself. Contributing that kranz-specific signal today would mean inventing
an undocumented hook, which this assessment won't do on spec alone. Defer,
named trigger: `gc doctor` documents (or is observed live to support) a
pack- or agent-contributed health-check extension point; until then, kranz
gets doctor's generic per-rig checks passively via registration and nothing
more.

### gc pack — defer

`gc pack` "manage[s] remote pack sources that provide agent
configurations" — git repositories containing `pack.toml`, cached locally
and pinnable to a ref, with `fetch`/`list`/`registry`/`release`
subcommands. This is precisely the distribution future the spike verdict
named: "a published `kranz` pack as the validated-mission rig type in the
gastown ecosystem." The kranz pack (`packaging/gascity/`, schema 2) is
already shaped like a pack — `pack.toml` plus `bin/` plus `agents/` — but
Current state is explicit that it is **stub-verified only**, never
validated against a live registered city supervisor. Publishing an
unvalidated pack to a remote registry (`gc pack registry`, `gc pack
release`) so other operators' cities can `gc pack fetch` it would export
that gap beyond this machine. Defer, named trigger: the pack is validated
against a live registered city supervisor (closing the Current-state gap
shared with `gc agent`/`gc hook`) — only then does publishing it as a
fetchable remote pack source make sense.

### gc lint — adopt

`gc lint <pack>` "validate[s] a pack before merge" — checks `pack.toml`,
reports non-fatal loader warnings, and parses prompt templates with
runtime's missing-key behavior; `gc lint .` recurses to find every
`pack.toml` below a path. This is the one exception in this mission's
read-only rules, because it is a validator by construction, not a
mutation. Run live against this pack:

```
$ gc lint packaging/gascity
gc lint: <operator-home>/Data/kranz/packaging/gascity: ok
```

The pack lints clean today. Adopt: it's free, non-mutating, and exactly the
gate a "pack" citizenship story needs before any future publication step
(`gc pack release`, above) — run it in CI on any change under
`packaging/gascity/` so pack drift is caught the same commit it's
introduced, independent of any of the three futures.

### gc mcp — defer

`gc mcp list` shows the "projected MCP catalog for a concrete target" —
`--agent <name>` for an agent with a single deterministic projection, or
`--session <id>` for a live session target. Projected MCP is how a City
agent's tool surface gets exposed to whatever provider consumes it.
Kranz's own REST surface (mission/task status, control endpoints) is a
plausible source to project as MCP config, so that other City-side agents
(codex/gemini sessions under the same router) could query or drive kranz
missions as MCP tools rather than needing City-specific glue. But `gc mcp
list` only *inspects* a projection that must already exist from config or a
live session — it does not build one, and kranz has no MCP server today
projecting its REST surface. Building that projection is a kranz-side
feature this assessment doesn't scope; this subsection only judges whether
citizenship via `gc mcp` is worth pursuing. It is, but only once there's a
second agent that would consume it — the heterogeneous-fleets future.
Defer, named trigger: a heterogeneous fleet is assembled (naming the same
trigger as `gc sling`, above) *and* kranz has built an MCP projection of
its REST surface for `gc mcp list` to report.

### gc skill — adopt

`gc skill list` shows "skills visible to the current city": city pack
skills (`skills/<name>/SKILL.md`), imported pack shared skills
(binding-qualified), compatibility bootstrap skills, and — with
`--agent`/`--session` — that agent's own skills catalog. It's explicitly a
diagnostic view of what's *available*, not a precedence resolver. Adding a
`skills/kranz-mission-brief/SKILL.md` to the pack — documenting constraint
2 (title is the prompt; description carries constraints; acceptance
carries testable outcomes) and the exit-code contract — would make that
guidance discoverable by any City agent authoring or reviewing a
kranz-labelled bead, not just a human who's read this doc. This is
documentation-only, costs nothing, doesn't touch runtime, and directly
mitigates the single sharpest failure mode this plan has already observed
live (docs/gascity.md lesson 2's goal-in-acceptance bead). Adopt: a pure
documentation addition to the pack that pays off as soon as any second
author — human or agent — writes a kranz-labelled bead, which matters most
once kranz is either shared across a fleet or distributed to other
operators' cities.

This feature assessed ten more mechanisms: 5 adopt (`gc agent`, `gc
status`, `gc dashboard`, `gc lint`, `gc skill`), 4 defer (`gc prime`, `gc
doctor`, `gc pack`, `gc mcp`), 1 reject (`gc session`). Counting every
`### gc ` subsection in this document, earlier and current, across all
twenty assessed mechanisms: **7 adopt** (`gc order`, `gc events`, `gc
agent`, `gc status`, `gc dashboard`, `gc lint`, `gc skill`), **7 defer**
(`gc hook`, `gc sling`, `gc formula`, `gc prime`, `gc doctor`, `gc pack`,
`gc mcp`), **6 reject** (`gc mail`, `gc handoff`, `gc nudge`, `gc convoy`,
`gc converge`, `gc session`). The pattern from the first ten mechanisms
holds for this second batch:
citizenship is cheapest and clearest for pure upward visibility (status,
dashboard, lint, skill, agent registration itself) and for extending the
documented exit-code contract; it stays deferred wherever it needs either a
live-validated supervisor (agent registration, doctor extension, pack
publication) or a second party to route to or compose with (mcp, sling,
formula); and it's rejected wherever the City mechanism would duplicate a
rail kranz already owns (session, mail, convoy) or add kranz to the
named-session surface constraint 5 exists to keep it out of (handoff,
nudge, session).

## Staged roadmap

Each stage names the assessment verdicts it lands (never a rejected one),
sizes work as kranz missions (history prices a single-feature brief at
roughly $30–50), and separates autonomously stub-verifiable verification from
anything requiring a human — the latter flagged on its own **Human-gated:**
line. Every stage carries a **Trigger:** line naming the condition under
which the stage is worth doing; several stages conclude that the trigger has
not fired and the stage is not worth doing yet. This is an assessment, not
advocacy, matching docs/gascity.md's reality-check ethos.

### Stage 0 — today: pack v2 parked, stub-verified

Scope: the current committed state. No adopted mechanism beyond what
docs/gascity.md already proved live (happy path, escalation plumbing, field
mapping) plus the exit-code contract documented in Current state. Nothing
from the Citizenship assessment is landed yet — `gc order`'s cooldown→event
move, `gc events` emission, `gc agent` registration, `gc status`/`gc
dashboard` visibility, and `gc skill` documentation are all still pending
(Stages 2–3 below).

Work items: none — this stage is the already-committed baseline
(`packaging/gascity/`, docs/gascity.md, this document's Current state and
Constraints sections). Recorded here only so the roadmap has a zero point to
measure forward from.

Verification: `gc lint packaging/gascity` (adopt, read-only) passes today,
per Current state — this is the one live, non-mutating check available
without a registered city. Everything else about this stage's correctness is
stub-level (docs/gascity.md's stubbed exit-2 run, the happy-path bead cited
there) — no live-supervisor check exists yet, which is exactly the gap
Stage 1 closes.

**Trigger:** none needed — this is the present state, not a proposal.

### Stage 1 — live-city validation of the existing pack

Scope: validate the Stage 0 pack (dispatch → spool → worker → run-bead →
exit-code mapping, and the `agent.toml` registration wiring) against an
actual `gc` supervisor for the first time. Lands no new mechanism from the
assessment — it closes the "never validated against a live supervisor" gap
that the `gc hook`, `gc agent`, and `gc pack` verdicts all name as their
defer/adopt precondition. This is entirely human-run; no kranz code changes.

Work items: none sized as a kranz mission — this stage is a human validation
session, not development work. If the session surfaces a real bug (e.g. the
lock-file trap misfiring, or the bd dialect assumptions being wrong against a
live city), *that* fix becomes a normal kranz mission (~$30–50) filed
afterward, scoped to the specific defect found.

**Human-gated:** the entire stage. `gc init`/`gc register`/`gc order run`
are state-mutating (constraint: kranz code paths never run them; a human
runs them by hand, once, in a disposable city). Concretely, a human would:

1. Set up (disposable city, isolated from any real one):
   `gc init --city /tmp/kranz-citizenship-test` (or the equivalent
   city-scoping flag `gc init --help` documents at the time), confirming
   first that this spawns a live, billed mayor session per constraint 5 —
   budget for that cost before running it.
2. Make the pack visible to the test city (hand-copy `packaging/gascity/`
   into its pack search path, or wire it via `gc import` per
   `gc import --help` at the time), then `gc register` the test city with
   the machine-wide supervisor; `gc agent add kranz-worker` (or hand-author
   the `agent.toml` stanza) to land the registration the `gc agent` verdict
   scoped as adopt.
3. Start `kranz-city-worker` under the test city's supervision (per the
   pack's intended wiring) and `gc bd create … --label kranz` a smoke bead,
   the same shape docs/gascity.md's happy path used.
4. Observe: `gc status`/`gc dashboard` show the worker; the bead reaches
   `bd close` on exit 0 (or the matching state for injected 1/2/3 exits);
   `gc lint packaging/gascity` still passes.
5. Teardown, in order, every time — this is the step docs/gascity.md lesson
   5 says is easy to skip: `gc stop` (or the city-scoped equivalent) to
   unregister the test city, then verify no orphaned tmux server remains
   (`tmux -L <test-city-name> ls` should error "no server running"); if it
   doesn't, manually kill it (`tmux -L <test-city-name> kill-server`) — do
   not leave a paid mayor session or orphaned tmux socket running past the
   test.

**Trigger:** a live `gc` city becomes available to test against (this
machine currently has none registered — see Current state) *and* someone is
willing to spend the setup/teardown time plus the live mayor-session cost
constraint 5 implies. Not worth doing to satisfy curiosity alone — do it
when Stage 2 or 3's work is about to be built and needs a live target to
validate against, not before.

### Stage 2 — event-driven dispatch and mail-based escalation (already-adopted mail, not backflow)

Scope: lands the `gc order` verdict (adopt) — move
`orders/kranz-dispatch.toml`'s trigger from `cooldown`/`interval = "5m"` to
`event`, matching `bead.created`/`bead.ready`-shaped events for the `kranz`
label, per the trigger kinds the assessment cites from `gc order --help`.
Confirms (does not newly land) the already-adopted outbound `gc mail send
human --notify` escalation path already wired in `kranz-run-bead`. Does
**not** land inbound mail-based guidance backflow — the assessment rejects
that (`gc mail` verdict: the inbound half would duplicate kranz's own
guidance rails) and D3 below holds that line.

Work items (kranz missions, ~$30–50 each):
- Change `orders/kranz-dispatch.toml`'s trigger block from cooldown to
  event, matching the `kranz` label; no change to `kranz-dispatch`'s body
  (constraint 1 — the order still just claims and spools within the exec
  deadline).
- Add a regression check (stub-level, no live city) that the changed
  `pack.toml`/`orders/kranz-dispatch.toml` still passes `gc lint
  packaging/gascity`.

Verification: `gc lint packaging/gascity` (adopt, read-only, run today)
covers the config's validity autonomously.

**Human-gated:** confirming the event trigger actually fires dispatch faster
than the old cooldown requires a live city — re-run the Stage 1 smoke-bead
flow against the event-triggered config in a disposable test city and
observe latency, not just correctness.

**Trigger:** Stage 1 has run at least once (so there's a validated baseline
to compare cooldown-vs-event behavior against) — otherwise this is a config
change with no way to confirm it does what it claims beyond `gc lint`.

### Stage 3 — City-visible progress and health

Scope: lands four adopt verdicts together, since they share the same
registration prerequisite: `gc agent` (register `kranz-worker` in
city.toml), `gc status`/`gc dashboard` (free once registered), and `gc
events` (emit `kranz.mission.{started,blocked,complete}` from
`kranz-run-bead` alongside its existing bd mutations, per the exit-code
contract extension the assessment specifies). Explicitly does **not** land
`gc doctor` (deferred — no documented pack-contributed health-check
extension point) as a second telemetry path: kranz already exports OTEL
(docs/otel.md, `kranz otel` — an opt-in sidecar tailing mission events to an
OTLP collector). Registering `kranz-worker` for `gc status`/`gc dashboard`
visibility is agent-liveness reporting, a different altitude than mission
telemetry, so it doesn't compete with OTEL; but a future pack-contributed
`gc doctor` check *would* be a second health-signal path alongside `kranz
otel`, which is exactly why `gc doctor` stays deferred here rather than
folded into this stage.

Work items (kranz missions, ~$30–50 each):
- Author the `agents/kranz-worker/agent.toml` registration stanza (already
  drafted per Current state — confirm it matches whatever `gc agent add`
  scaffolds, or hand-author to match).
- Add `gc event emit kranz.mission.started/blocked/complete` calls to
  `kranz-run-bead` at the three points its exit-code mapping already
  branches (0/2/3|1), per the `gc events` verdict.
- Unit/stub coverage for the emit calls firing at the right branch (mock
  `gc` binary, assert the right subcommand + event name per exit code).

Verification: the emit-call wiring and its exit-code branching is
stub-verifiable exactly like the existing bd-mutation tests (mock `gc`,
assert arguments) — no live city needed for correctness of *what* gets
called.

**Human-gated:** confirming the events actually land in a live `gc events
--follow` stream and that `gc status`/`gc dashboard` show the registered
agent requires the Stage 1 disposable-city setup/teardown procedure again
(same tmux-reaping care applies if a fresh test city is spun up rather than
reusing one still live from Stage 1/2).

**Trigger:** Stage 1 has validated agent registration works as documented
against a live supervisor (the `gc agent`/`gc status`/`gc dashboard` verdicts
all name this as their shared precondition) — building the emit/registration
code is stub-safe today, but calling this stage "done" needs that live
confirmation.

### Stage 4 — distribution: a lint-clean published pack

Scope: lands the `gc pack` verdict's trigger condition by attempting it —
publish `packaging/gascity/` via `gc pack registry`/`gc pack release` so
other operators' cities can `gc pack fetch` it, per the assessment's named
distribution future. Presupposes Stages 1 and 3 are done: the assessment is
explicit that publishing an unvalidated pack "would export that gap beyond
this machine." Also revisits `gc hook` (currently deferred) once a published
pack implies other operators' cities, since `gc hook`'s defer condition is
the same live-supervisor validation this stage's precondition already
requires.

Work items (kranz missions, ~$30–50 each):
- Write the pack's publish-facing metadata (registry description, versioning
  policy for `pack.toml`'s schema field) — documentation-shaped, no runtime
  change.
- Re-run `gc lint packaging/gascity` as a pre-publish gate (already adopt,
  already passing) and wire it into CI per the `gc lint` verdict's stated
  recommendation, so pack drift is caught the same commit it's introduced.
- Evaluate collapsing `kranz-dispatch` + `kranz-city-worker` into a single
  `gc hook`-driven claim (the simplification the `gc hook` verdict names),
  now that a live supervisor is available to validate `work_query` semantics
  against.

Verification: CI-wired `gc lint` is autonomously verifiable (it already
passes, per Current state) and stays that way on every change under
`packaging/gascity/`.

**Human-gated:** the actual `gc pack registry`/`gc pack release` publish
step is networked and state-mutating — this plan's invariants keep
publishing out of any autonomous mission or worker; a human runs it,
deliberately, once the pack is validated (Stage 1) and City-visible
(Stage 3).

**Trigger:** a second operator or city actually wants to consume the kranz
pack as a rig type. Nothing today creates that demand — this machine has one
operator and no other city to fetch from. Not worth doing until that demand
is concrete; publishing a pack nobody fetches only exports Stage-0/1 risk for
no benefit.

### Stage 5 — fleets and cross-machine execution (speculative)

Scope: the two remaining named futures from the spike verdict —
heterogeneous fleets (lands `gc sling` and re-evaluates `gc mcp`, both
currently deferred pending exactly this) and cross-machine execution (no
specific mechanism verdict names this as its direct trigger; it would mean
City k8s runtimes behind kranz's `AgentBackend` seam, which does not exist
today). Does not land any mechanism the assessment rejected — `gc handoff`,
`gc nudge`, and `gc session` stay rejected regardless of fleet size, since
they require a named interactive session constraint 5 rules out
independent of how many agents share the router.

Work items: none sized — this stage is explicitly speculative. If a
heterogeneous fleet is actually assembled, the first real work item would be
scoping `kranz-worker` as a `gc sling` target (a normal ~$30–50 kranz
mission at that point), not before.

Verification: not applicable — there is nothing to verify until the
precondition below is real.

**Human-gated:** by construction, since standing up a second agent type
under one City router and/or a k8s cross-machine runtime is itself a human
infrastructure decision, not something a kranz mission would do
autonomously.

**Trigger:** a heterogeneous fleet (kranz plus at least one other agent
type, e.g. codex/gemini) is actually assembled under one City router, for
`gc sling`/`gc mcp`; a City k8s cross-machine runtime is actually offered
behind an `AgentBackend` implementation, for cross-machine execution.
Neither condition holds today, and this document does not predict when
either would. Not until the trigger fires.

## Design decisions

### D1 — the opacity boundary

Options: (a) hold the one-opaque-agent stance — no resident LLM session
City-side, `kranz-worker` stays a `long_running` agent with no
`[[named_session]]`, City's richer machinery (formulas, convoys, sessions,
handoff, nudge) stays bypassed; (b) open kranz internals to City by
declaring a City-visible, primeable LLM session for `kranz-worker` (unlocks
`gc prime`, and would let `gc handoff`/`gc nudge`/`gc session` apply).

**Recommendation:** (a), hold the opacity boundary. Constraint 5 exists
specifically because `gc init`'s mayor and its restart/tmux lifecycle are a
real, observed operational cost (docs/gascity.md lesson 5: orphaned tmux
servers, observed twice) — adding a second named session multiplies that
surface for a benefit the assessment can't currently name (no mechanism
verdict needed `gc prime`/`gc handoff`/`gc nudge`/`gc session` badly enough
to accept reject on all four). This forecloses interactive, City-native
triage of a blocked kranz mission (no `gc session`/`gc handoff` for it) —
that steering stays on kranz's own web/Slack/CLI surfaces, per D3. Revisit
only if a concrete need for City-side interactive triage of kranz missions
specifically (not generic City agents) is identified — none is, today.

### D2 — the dispatch model

Options: (a) stay on cooldown-polling (`orders/kranz-dispatch.toml`,
`trigger = "cooldown"`, `interval = "5m"`, the Stage 0 baseline); (b) move to
event-driven order dispatch (`trigger = "event"` matching
`bead.created`/`bead.ready` for the `kranz` label, the `gc order` verdict's
adopt recommendation, landed in Stage 2); (c) native `gc hook` routing
(`kranz-worker` calls `gc hook kranz-worker --claim` directly, collapsing
dispatch and worker into one process, the `gc hook` verdict's deferred
simplification).

**Recommendation:** (b) now, with (c) as the Stage 4 re-evaluation. Event
dispatch is a pure config change within the already-adopted `gc order`
mechanism — no new invariant surface, still respects constraint 1 (order
claims and returns within the exec deadline) — so it's strictly better than
cooldown-polling with no live-validation prerequisite beyond `gc lint`. (c)
is real but requires validating `work_query`/agent-registration semantics
against a live supervisor first (the `gc hook` verdict's stated defer
reason), which this machine cannot do until Stage 1 runs. This forecloses,
for now, collapsing the two-script split into one — that stays two
processes (dispatch order + supervised worker) until Stage 1/4 validate the
simpler alternative.

### D3 — guidance backflow

Options: (a) City mail feeding kranz's guidance inbox for blocked missions —
build an inbound path where `gc mail inbox`/`reply` responses steer a
blocked mission's fix cycle; (b) keep steering exclusively on kranz-native
surfaces (web dashboard, Slack thread, CLI), with outbound `gc mail send
human --notify` staying a pointer to those surfaces, not a channel itself.

**Recommendation:** (b), per the `gc mail` verdict (reject on the inbound
half) and docs/gascity.md's explicit statement that a blocked mission is
steered "from kranz's own surfaces." Building (a) creates two sources of
truth for one steering decision — a human could reply via mail *or* via
Slack/web/CLI, and now the mission needs a merge policy between them that
doesn't exist and isn't scoped anywhere. This forecloses City mail as a
guidance channel entirely, not just today: the reject verdict isn't
trigger-gated (unlike defers elsewhere in this plan) because the duplication
problem doesn't resolve with more validation or a bigger fleet — it's
structural. The outbound escalation notification (already adopted, Stage 0)
is unaffected.

### D4 — supervision and health

Options: (a) keep `kranz-city-worker` under the supervisor's health patrol
as a registered `long_running` agent (current pack design, extended by
Stage 3's `gc agent`/`gc status`/`gc dashboard` adoption); (b) make `kranz
serve` itself the supervised City service, with a lock-probe health check
replacing or supplementing the worker-lock-file liveness signal.

**Recommendation:** (a). The pack's entire `kranz-city-worker` design
already exists to be the supervised long-running process constraint 1
requires (replacing the spike's unsupervised `nohup` runner, docs/gascity.md
lesson 1) — it takes the single-instance lock, drains serially, and is
exactly the shape `gc agent`'s adopt verdict registers. Making `kranz serve`
itself the City-supervised unit would mean the always-on kranz web/API
server becomes City-coupled, which contradicts the standing invariant that
"kranz remains fully usable standalone" and the Gas City pack stays optional
integration surface, never a dependency of core kranz behavior. This
forecloses folding kranz's own service lifecycle into City's supervisor —
`kranz serve` keeps running (or not) independent of whether any city has
`kranz-worker` registered at all.

### D5 — the private spool vs kranz-native queue

Options: (a) keep the current private spool directory (`KRANZ_SPOOL`,
`.env` entries written by `kranz-dispatch` and drained serially by
`kranz-city-worker`) as the production path; (b) build the long-lived
`kranz work` dispatcher docs/gascity.md deviation 1 names as the production
path, replacing the private spool with kranz's own native queue mechanism.

**Recommendation:** (b) is the better long-term target, but (a) is what's
committed today and nothing in this assessment forces an immediate swap —
this decision is a flag for Stage 2+ work, not a stage in itself. The
spool-file mechanism is a private, kranz-specific re-implementation of
routed-work claiming that the `gc hook` verdict already identifies as
collapsible once `work_query` semantics are validated (Stage 4). Building
`kranz work` as kranz's own native queue is orthogonal to that City-side
collapse: it would replace the *dispatch-order-writes-spool-file* half with
a kranz-owned mechanism, independent of whether the worker later claims via
`gc hook` or drains a `kranz work` queue directly. This forecloses treating
the current `.env`-file spool as a permanent design — it is understood, per
docs/gascity.md, as the spike-era stand-in, and `kranz work` should absorb
its function whenever kranz-side queue work is next scoped (not scheduled by
this document).

### D6 — verification strategy for city-coupled behavior

Options: (a) design a disposable test-city fixture — scripted `gc init`/`gc
register`/teardown against an isolated city directory, reused across Stage
1/2/3/4 validation runs; (b) stay stub-only (mock `gc` binary/CLI calls in
unit tests, as `kranz-run-bead`'s bd-mutation tests already do) and never
automate live-city checks.

**Recommendation:** (b) for anything a kranz mission or CI job runs, with
(a) specified as a **human-run** procedure (Stage 1) rather than an
automated fixture. Constraint 5's costs are real and per-invocation (a live,
billed mayor session; a tmux server that must be manually reaped on
teardown) — scripting (a) as something CI or a mission could invoke
unattended would mean an autonomous process potentially spawning billed
sessions and leaving orphaned tmux servers with nobody watching to reap
them, which is exactly the failure mode docs/gascity.md lesson 5 already
observed under a human's attention. This forecloses ever fully automating
live-city verification: the disposable-city procedure stays a documented,
human-run checklist (Stage 1) permanently, not a fixture kranz's own test
suite or CI grows to own. Stub-level mocking (mock `gc`, assert
argument-shape) remains the ceiling for what kranz's own automated tests
verify about City integration.

## Ticket-ready briefs

These briefs paste directly into kranz's own backlog (`kranz ticket`) or into
a Gas City bead via the pack's own field mapping (`title → Goal`, `description
→ Context`, `acceptance_criteria → Acceptance hints`, per the exit-code
contract section above). Briefs 1 and 3 are each scoped to the earliest
**not** **Human-gated:** work item their roadmap stage (Stage 2 and Stage 3
respectively) names. Brief 2 is different: design decision D5 deliberately
leaves the kranz-native queue unscheduled — "a flag for Stage 2+ work, not a
stage in itself" and "not scheduled by this document" — so no Stage 0-5 work
item names it. Brief 2 is instead the ready-made scoping D5 defers to
"whenever kranz-side queue work is next scoped"; filing it *is* that scoping
decision, left to the operator rather than scheduled by this document. Either
way, a fresh worker with no memory of this document's discussion can pick any
brief up and run it headlessly.

#### Brief 1: Switch kranz-dispatch's order trigger from cooldown to event

**Goal:** Change `packaging/gascity/orders/kranz-dispatch.toml`'s trigger
from `cooldown`/`interval = "5m"` to an `event` trigger matching
`bead.created`/`bead.ready`-shaped events for the `kranz` label, so dispatch
fires the moment a labelled bead is ready instead of waiting out a fixed
sleep.

**Context:** This lands the `gc order` verdict (adopt) and design decision
D2, Stage 2 of docs/gascity-citizenship.md's staged roadmap. Read-only `gc`
use only — inspect the exact trigger-kind config keys via `gc order --help`
(and `gc <cmd> --json-schema` if it documents the order schema); never run
any state-mutating `gc` command (no `gc order run`, `gc register`, etc.) and
no network access. Do not change `packaging/gascity/bin/kranz-dispatch`'s
body — constraint 1 (docs/gascity-citizenship.md's Constraints and
invariants) requires the order to still only claim-and-spool within the
exec context deadline; this brief is a config-only change to the TOML
trigger block. Do not touch `packaging/gascity/bin/kranz-city-worker` or
`packaging/gascity/bin/kranz-run-bead`. No new dependencies.

**Acceptance:** `packaging/gascity/orders/kranz-dispatch.toml` has an
`event` trigger (not `cooldown`) scoped to the `kranz` label, using only
trigger-kind keys documented by a live `gc order --help`/`--json-schema`
run captured in the ticket's own report; `gc lint packaging/gascity`
exits 0 with its `ok` line; `git diff` against the prior commit touches only
`packaging/gascity/orders/kranz-dispatch.toml` (and, if a regression test is
added, a test file alongside it — no changes to `kranz-dispatch`'s body,
`kranz-city-worker`, or `kranz-run-bead`).

#### Brief 2: Build a kranz-native queue to replace the pack's private spool

**Goal:** Implement a `kranz work` queue dispatcher inside kranz itself —
enqueue, dequeue-oldest-first, and single-consumer drain semantics
equivalent to the pack's current `.env`-file spool directory — as a
kranz-owned mechanism, without yet wiring the pack to use it.

**Context:** This lands design decision D5's recommended direction
(docs/gascity-citizenship.md), flagged there as work to scope "whenever
kranz-side queue work is next scoped" following Stage 2. The target
behavior to match is the private spool described in this repo's Current
state section: `kranz-dispatch` writes one `.env` entry per claimed bead
into `KRANZ_SPOOL`; `kranz-city-worker` drains strictly serially, oldest
entry first, one at a time, under a single-instance lock. This brief is
kranz-internal only — it must not modify anything under
`packaging/gascity/` (that pack keeps using its existing spool until a
later, separate brief swaps it over); it needs no `gc` interaction at all,
read-only or otherwise, and no network access. No new dependencies unless
the crate already vendors an equivalent; if one is truly required, name it
explicitly in the ticket.

**Acceptance:** A new `kranz work` command (or equivalent library entry
point, whichever fits the existing crate's command conventions) provides
enqueue, oldest-first dequeue, and serial single-consumer drain; a new
automated test suite covers oldest-first ordering, empty-queue behavior,
and that a second concurrent drain attempt does not double-process an
entry. Running that suite's own runner command piped through
`grep -qE 'result: ok\. [1-9][0-9]* passed'` (never a bare test-name
filter, which exits 0 on zero matches) confirms at least one test passed;
the existing `packaging/gascity/` spool mechanism and its scripts are
untouched by this brief's diff.

#### Brief 3: Emit `kranz.mission.*` events from kranz-run-bead's exit-code mapping

**Goal:** Extend `packaging/gascity/bin/kranz-run-bead` to call
`gc event emit kranz.mission.started`, `kranz.mission.blocked`, or
`kranz.mission.complete` (with the report line) at the same points its
existing exit-code mapping already branches on 0/2/1/3, alongside its
current `bd update`/`bd close`/`bd comment` calls.

**Context:** This lands the `gc events` verdict (adopt) and Stage 3 of
docs/gascity-citizenship.md's staged roadmap. It is a small, explicit
extension to the documented exit-code contract in this repo's Current
state section (the table mapping exit 0/1/2/3 to bead effects) — do not
invent any other bd or gc mutation beyond the three named
`kranz.mission.*` events, and do not add a `started` emission anywhere
except where the worker first picks up a spool entry, matching the
existing bd dialect rules (constraint 4: `bd close` takes `--reason`,
`bd comment` takes text positionally, neither takes `-m`). No changes to
`packaging/gascity/bin/kranz-dispatch` or `agents/kranz-worker/agent.toml`.
Verification is stub-only: mock the `gc`/`bd` binaries on `PATH` the same
way this repo's existing bd-mutation coverage does; this brief does not
require, and must not attempt, a live registered city (no `gc init`, `gc
register`, or any other state-mutating `gc` command). No new dependencies.

**Acceptance:** A stub/mock-`gc` test suite proves: on exit 0,
`kranz.mission.complete` is emitted after the `bd close` call; on exit 2,
`kranz.mission.blocked` is emitted alongside the existing `bd
update`/`comment`/`gc mail send human --notify` calls; on worker pickup of
a spool entry (before the mission runs), `kranz.mission.started` is
emitted; on exit 1 or 3, no `kranz.mission.complete` event is emitted. If
the suite is a cargo test, its invocation is piped through
`grep -qE 'result: ok\. [1-9][0-9]* passed'` (never a bare `cargo test
<name>` filter, which exits 0 on zero matches); `gc lint
packaging/gascity` still passes after the change.
