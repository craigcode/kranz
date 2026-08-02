# The trust metrics, defined

*The vocabulary for measuring whether trust was correctly granted — each
metric with its definition, its denominator, the command that reproduces
every figure, and this repo's own numbers as the worked example.*

Everything in the industry's current governance wave measures usage,
access, spend, and attribution: which agent wrote the code, what it cost,
how much ran. None of it answers the question that decides whether the
autonomy was warranted: **should you have let it?** These four metrics are
kranz's answer. They are computed from the append-only mission event log —
the same log the missions themselves write — so the measurement and the
work share one source of truth, and every number below is reproducible by
anyone with the repo.

Companion reads: `docs/the-trust-interval.md` (the why),
`docs/pack-contract.md` (the gate machinery the metrics observe).

## 1. Autonomy ratio

**Definition.** The share of closed missions that completed with **zero
operator interventions** — no grant decisions, no steers, no unblocks, no
revision rulings. An intervention is a first-class event in the log, so the
ratio is counted, not estimated.

**Denominator.** Closed missions (`completed` + `failed`). Abandoned
missions are excluded: an abandonment is itself an operator act, and
counting it would punish the metric for being honest.

**Read it as:** how much of the work ran untouched. A falling ratio over
time is the system asking for more judgment than it should; a rising one
is trust being earned back by evidence.

**Reproduce:** `kranz escalation-metrics --json` → `.autonomy`.

**This repo, 2026-08-02:** 49 closed missions, 44 zero-intervention —
**89.8%**.

## 2. Block-to-grant timing (the rubber-stamp signal)

**Definition.** For every grant decision (a mission parked for operator
consent), the wall-clock latency from request to decision, reported as
p50/p90 plus the share decided in **under ten seconds**.

**Why under-ten-seconds is the bucket that matters.** A grant is a
go/no-go call on agent-proposed action. A human cannot read the evidence
and decide in seconds; a wall of sub-ten-second approvals is not trust, it
is habituation — the exact failure Claude Code's creator described solving
by removing the human from the loop. This metric is the measurement of the
thing he chose to eliminate: it exists so a system can prove the human is
still *deciding*.

**The honest bimodality.** kranz grants deny by default on timeout, so the
distribution has two clean modes: operator decisions at minutes-to-hours
(reading, verifying, occasionally reproducing the agent's claim first) and
deny-default timeouts at the grant ceiling. Both are the boundary working;
the pathological mode is the fast one.

**Reproduce:** `kranz escalation-metrics --json` → `.rubberStamp` (and the
per-grant rows in `.ledger`).

**This repo, 2026-08-02:** 32 decided grants, **0 under ten seconds**.
Approved-grant latencies run 10 minutes to 13 hours (a human read them);
the rest are one-hour deny-default timeouts (a human was away, and the
default did its job).

## 3. False green

**Definition.** A mission that closed clean — contract green, gates
passed — and later had a **defect traced back to it**. The linkage is data
entry on the defect, not inference: a defect ticket names the mission it
was traced from, and the join is computed over the log.

**Why it is the number with teeth.** Autonomy ratio measures process;
false greens measure *outcome*. A system can run 100% untouched and be
quietly wrong — the false-green rate is the only figure that separates
"autonomous" from "unsupervised". Split by intervention class (missions
with interventions vs. zero-intervention), it also answers whether the
interventions were doing work.

**Reproduce:** `kranz escalation-metrics --json` → `.falseGreens`.

**This repo, 2026-08-02:** **0 false greens** across 49 completed missions
— 0 of 5 with interventions, 0 of 44 zero-intervention.

## 4. The escalation ledger

**Definition.** Every block, grant, steer, and revision in chronological
order, each with what the agent proposed and what the operator decided —
ask, decision, latency, event seq. Not a metric but the substrate the
other three are computed from, and an artefact in its own right: labeled
human judgment over agent proposals, which is precisely the corpus
fine-tuning consent behavior requires (see
`.kranz/tickets/training-corpus-export.md`).

**Reproduce:** `kranz escalation-metrics --json` → `.ledger`; per mission,
`kranz provenance <mission-id>` reconstructs the same chain — gates in
order, sessions with backend/model and prompt hash, human decisions with
their event seq — from the log alone.

**This repo, 2026-08-02:** the ledger for this repo's own development
includes, verbatim, the go/no-go calls behind every claim in
`docs/the-trust-interval.md` — including the ones that *denied* the agent
by default and the ones that told it to stop and rework.

## The one-line version

Everyone in the field is building the viewing gallery — dashboards for
watching agents work. Nobody else audits the go/no-go calls. These four
numbers are the audit.
