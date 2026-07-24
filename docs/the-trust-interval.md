# The few seconds between action and trust

*What kranz is, why it exists, and the receipts to prove it — with the demo
runbook at the end.*

## The thesis

The hard part of letting an AI agent do real work is not the work. It is the
few seconds between the agent wanting to do something and you deciding
whether to trust it. Everything about kranz is built for that interval.

kranz turns "run an AI agent on this task" into a **mission**: a planned,
human-approved, validated, audited execution over headless agent CLIs
(Claude, Codex, Droid, Kimi, or a local endpoint). An orchestrator plans the
work contract-first; fresh-context workers implement one feature at a time;
independent validators grade each milestone; a deterministic gate decides
what passes; and the operator — a human — decides the moments that matter:
the plan, the exceptions, the merge.

Most tools make the agent faster. kranz makes the interval between action
and trust legible, measurable, and safe.

## Why now (the field, mid-2026)

The industry has converged on the same claim from every direction: the model
is no longer the bottleneck — the harness is. Etsy measured a 20-point
benchmark swing from the harness alone with the model held fixed; OpenAI's
own keynote ended on attention as the limit. Three independent talks at the
same conference converged on the exact rule kranz runs on: "enforce, don't
instruct" — instructions are probabilistic, permission is deterministic. And
the role split this repo is built on was named there in one sentence: use
code for determinism, agents for judgment, humans for authority. The counter-
evidence for why the validation layer is not optional: 31% more PRs merged
unreviewed, 242% more incidents per PR, 6x bugs per developer with
AI-assisted code. The harness is the unit that turns "predicts the next
token" into "does the work" — this repo is that unit with a consent spine.

## The differentiators, with receipts

**Consent inside the loop, not around it.** Missions pause at the moments of
judgment: a validator wants a command outside its allow-set, a worker writes
outside its declared touch-set, a deny rule fires. The run parks, the
operator gets the evidence, and the decision lands in the event log as a
first-class auditable event — approved or denied, with the reason. In one
recent week of dogfooding this repo's own development, the operator (me)
decided eleven of these: a `Cargo.lock` write for a new HTTP dependency, a
test-file extension for queue routing, a renderer case for a brand-new
`TierEscalated` event. Each was small, evidence-backed, and recorded. None
was a rubber stamp, and we can prove that — see the measurement section.

**Contract validation — plans mean something.** Every mission has a
validation contract written before any code: command assertions that must
exit green, and judgement assertions verdicted independently. The gate is
not advisory. A mission whose diff against its pinned base has zero feature
commits FAILS honestly (that net exists because of mission m-66aff8, which
once passed a green contract on an empty tree — the post-mortem is in
`docs/notes/`). And the contract itself is treated as untrusted input: in
July 2026 three missions died to authoring bugs in their own contract
commands — an inverted grep, a BSD-vs-GNU `grep -L` exit inversion, a
two-filter `cargo test` — each abandoned honestly, each producing a fix:
the contract linter that now smoke-tests assertion commands at plan
approval.

**Honest failure over green theater.** Blocked missions stay blocked until a
human unblocks them. Abandoned missions record why, in full. Unauthenticated
workers that would have produced empty diffs get caught by a real auth probe
(mission m-165b6f paid for that lesson). The empty-deliverable net runs
before any contract assertion. The system would rather say FAIL than say
nothing.

## The dogfood proof

This repository was substantially built by its own missions.

- 40+ completed missions with committed plans, reports, and full event logs
  under `.kranz/missions/` — the multi-repo host, the Slack control surface,
  the dashboard pipeline view, three of the four agent backends, the secret
  scanner, the merge gate, and most of the CI/CD hardening were executed as
  kranz missions.
- Missions drive themselves end-to-end: mission m-6a20dc was drafted,
  approved, run, and merged entirely from Slack — the bridge validating its
  own control loop.
- The calibration corpus is real money, recorded: each mission's estimated
  vs actual cost is kept, and the misses are kept too (a 9x underestimate on
  m-d341a7 is in the calibration docs, not deleted).
- The adversarial review culture runs in both directions: the event log got
  an independent adversarial review with five findings, each tracked to
  remediation or rebuttal in `docs/reviews/`.

## The measurement: the flight surgeon's console

If the trust interval is the product, it needs a metric. kranz computes it
from the same event logs that drive everything else:

- **Autonomy ratio** — operator interventions per closed mission, and the
  share of missions that ran untouched.
- **Grant latency** — the time from a grant request to its decision,
  bucketed. This *is* the trust interval, measured. A wall of sub-ten-second
  approvals would be rubber-stamping made visible; slow, evidence-backed
  decisions are what judgment looks like as data.
- **The escalation ledger** — every block, grant, and revision with what was
  proposed and what was decided. It doubles as the consent corpus for
  fine-tuning work: labeled human judgment, not just execution traces.

(Status note, 2026-07-23: the console SHIPPED as mission m-d1e3c3 —
`kranz outcomes`, the dashboard panel, and the `/kranz outcomes` Slack
card all fold the same engine module. Live numbers on this repo: 0.60
interventions per closed mission, 85% of 53 closed missions
zero-intervention, and 18 of 18 grant decisions ≥10m — slow,
evidence-backed judgment made visible, zero rubber-stamp buckets.)

## Demo runbook

A 3–4 minute capture of one real mission. Everything below is runnable
today; each beat notes whether it is delivered or pending.

```bash
# 1. Draft — the plan and the calibrated cost estimate (DELIVERED)
kranz draft <demo-ticket>

# 2. Approve and run — workers spawn, features commit (DELIVERED)
kranz ticket queue <demo-ticket>
kranz work        # or the Run queue button in the dashboard

# 3. The consent moment — a grant parks the run (DELIVERED)
#    Dashboard: GrantRequestPanel. CLI: `kranz grant approve <id> <target>`.
#    Show: the parked request, the evidence, the decision,
#    and the grant.approved event it writes to the log.

# 4. Validation and merge (DELIVERED)
#    Validation round -> final gate -> report.md -> gated merge:
#    secret scan, scratch-worktree gate suite, fast-forward of exactly
#    the tested commit.

# 5. The metric (DELIVERED — m-d1e3c3: CLI + dashboard panel + Slack card)
kranz outcomes    # autonomy ratio, grant-latency buckets, escalation ledger
#    Dashboard: OutcomesPanel. Slack: `/kranz outcomes` summary card.
```

Capture: asciinema + `agg` for the CLI beats; screen capture of the
dashboard for the grant panel and merge. The demo ticket should be small and
predictable — a one-milestone, two-feature change — with the grant beat
either accepted as live variability (most cross-cutting missions park at
least once) or replayed from a recorded mission's events.jsonl.

## What to show a skeptic

- `.kranz/missions/` — 40+ missions with plans, reports, and logs. Pick one.
- `docs/notes/empty-deliverable-safety-net.md` — the post-mortem culture.
- `.kranz/lessons/` — machine-written lessons from completed missions.
- `docs/reviews/event-log-review.md` — an adversarial review of the core
  component, with its own verification pass.
- The grant events in any recent `events.jsonl` — the trust interval,
  recorded.
