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
kranz-worker` today would fall back to (or, under `--strict`, refuse to
run without) a default worker prompt that nothing consumes. Whether kranz
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
occupies. Reject: violates invariant 5 (no named sessions) and duplicates
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

*Populated by a later feature.*

## Design decisions

*Populated by a later feature.*

## Ticket-ready briefs

*Populated by a later feature.*
