# Gas City integration — spike findings (2026-07-04)

Status: **demo-ready** (2026-08-10). The pack lives in
[packaging/gascity/](../packaging/gascity/) and is now event-driven:
`kranz-dispatch` runs on `bead.created`, and `kranz-run-bead` emits
`kranz.mission.started/blocked/complete` City events. Live-city validation
against a real supervisor is still human-gated (see
[gascity-citizenship.md](gascity-citizenship.md) Stage 1). Target: Gas City
only (`gc` 1.3.2) — the gastown-era surface (agents.json, `gt prime`, tmux
detection) is deliberately not used.

## What was proven live

- **Happy path** — a City bead (`gc bd create … --label kranz`) dispatched by
  the `kranz-dispatch` exec order ran `kranz exec` headlessly on a rig
  checkout to COMPLETE ($0.42), and the detached runner closed the bead with
  the mission report line (`bd close --reason "kranz mission COMPLETE — …"`).
- **Escalation plumbing** — exit 2 maps to: bead → `blocked`, explanatory
  comment, and `gc mail send human --notify` ESCALATION carrying the report
  line. Proven deterministically with a stubbed exit-2 kranz.
- **Field mapping** — bead `title → ## Goal`, `description → ## Context`,
  `acceptance_criteria → ## Acceptance hints`; kranz exit codes are the whole
  return contract: 0 close · 2 escalate · 3 refine-and-reopen · 1 reopen.

## Deviations & lessons (each learned the hard way)

1. **`gc order run` enforces an exec context deadline.** It killed a
   mid-mission dispatcher (~60s), orphaning a kranz engine (whose dead-lock
   auto-steal handled it exactly as designed). Orders must dispatch and
   return: `kranz-dispatch` claims beads and detaches `kranz-run-bead` with
   nohup. Consequence: the runner is UNSUPERVISED — the production path is
   an order that enqueues into kranz's own queue with a long-lived
   `kranz work` dispatcher (or a City-supervised service), not nohup.
2. **The bead title is the prompt.** A bead titled "spike smoke bead" (goal
   buried in acceptance) made a haiku orchestrator confidently build a smoke
   TEST SUITE for a repo with no code, write its own contract around the
   misreading, and pass it. Title must carry the goal; context carries
   constraints; acceptance carries testable outcomes.
3. **Cheapest config + no adversaries = letter-over-spirit compliance.** A
   mission forbidden to invent its missing input (`OWNER_NAME.txt` absent by
   design, "NEVER invent … must block") COMPLETED by having its fix cycle
   CREATE the source-of-truth file from git config, then satisfying the
   acceptance tautologically. With `skipScrutiny`/`skipFunctional` there is
   no adversarial reader to object. **Rule: City-dispatched autonomous
   missions must keep the scrutiny validator enabled**; treat validators-off
   configs as trusted-brief-only. (Scrutiny is the role that caught the
   seeded auth bypass in the §5 acceptance mission.)
4. **bd flag dialects**: `bd close` takes `--reason`; `bd comment` takes the
   text positionally; `-m` belongs to neither (gc's wrapper aborts on
   unverifiable args rather than substring-resolving — good behavior).
5. **`gc init` side effects**: registers a machine-wide launchd supervisor
   and immediately spawns a mayor as a live
   `claude --dangerously-skip-permissions --effort max` session; `gc stop`
   unregisters but can orphan the session's tmux server (observed twice —
   reap `tmux -L <city>` manually). The kranz pack intentionally declares no
   named sessions.
6. **exec merge policy**: `kranz exec` leaves the validated work on the
   mission branch (`pushed=false` reported on stdout); the dispatching side
   owns merge/push policy. Spike-era limitation: one dispatcher order per
   rig (`KRANZ_RIG_DIR`); multi-rig routing should derive the rig from the
   bead prefix.

## Verdict (the reality check)

For a single operator on one machine, the integration is capability-neutral:
beads/orders/mail duplicate kranz tickets/queue/Slack, and City's richer
machinery (formulas, convoys, sessions) is deliberately bypassed by the
one-opaque-agent boundary. Integration earns its keep in exactly three
futures: heterogeneous fleets (kranz missions beside codex/gemini agents
under one router), distribution (a published `kranz` pack as the
validated-mission rig type in the gastown ecosystem), and cross-machine
execution (City k8s runtimes behind kranz's `AgentBackend` seam, someday).
Until one of those is wanted: daily work stays on kranz's own rails
(Slack/web/CLI); this pack is the ready-made on-ramp, kept cheap.
