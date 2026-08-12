---
state: done
state-note: Done at fc93a23: escalation_metrics.rs aggregator (autonomy ratio, rubber-stamp p50/p90 + under-10s, false greens via traced-from-mission, escalation ledger), GET /api/escalation-metrics, kranz escalation-metrics [--json], FlightSurgeon dashboard panel, kranz draft --from-mission. Zero contract changes; 26 new tests incl. the ticket's exact anti-vacuity numbers. Tests 2>&1 | grep -qE 'test result: ok. [1-9]'
---

# Flight-surgeon console (escalation metrics + ledger)

## Problem

Kranz's differentiators are consent, contract validation, and audit — but
nothing MEASURES whether the consent boundary is healthy. Three questions
an operator should be able to answer from the event stream, and currently
cannot answer at all:

1. **How autonomous is the fleet, honestly?** Share of closed missions that
   completed with zero operator interventions.
2. **Is the operator rubber-stamping?** When a mission parks for a grant,
   how long between the park and the grant? A wall of sub-ten-second
   approvals is the tight-boundary failure made visible — the operator is
   not reading what they approve.
3. **What got through?** Missions that closed green (autonomously or not)
   and later produced a defect traced back to them — the number with teeth.

Plus the artifact both the audit trail and the local-inference flywheel
want: the **escalation ledger** — every park/grant with what the agent
proposed and what the operator decided (same table, two purposes).

## Design (locked)

All four metrics are queries over data Kranz already writes: per-mission
`events.jsonl` (grant parks/approvals, control steers, revisions,
milestone.unblocked with operator decisions) + mission status. The ONE new
data entry is the defect→mission link.

### Defect linkage (additive, the only schema touch)

`.kranz/tickets/<slug>.md` frontmatter gains an OPTIONAL
`traced-from-mission: m-xxxx` field. `kranz draft --from-mission m-xxxx
<slug>` (new flag) seeds it when drafting a defect ticket; operators may
also add it by hand. The aggregator joins defect tickets against missions.
Additive-only; absent means "not a traced defect" (no false positives).

### The four metrics

- **Autonomy ratio** — closed missions (COMPLETED or FAILED honestly) with
  zero operator events (grant approvals, control steers, revision
  requests, manual unblocks) / all closed missions. Also reported split by
  completion outcome.
- **Rubber-stamp signal** — distribution of park→grant latencies (p50/p90,
  and the count under 10s).
- **False greens** — missions that closed COMPLETED with ≥1 defect ticket
  tracing back; rate over all completed missions, split by whether the
  mission had operator interventions (the autonomy-quality test).
- **Escalation ledger** — every park/grant/steer event: mission, milestone,
  what was asked (grant detail), operator decision, latency. Tabular.

### Where it surfaces

1. **Engine** (`crates/engine/src/escalation_metrics.rs`, new): the
   aggregator over a mission list — pure functions + tests. Mirrors
   `outcomes.rs`/`contract_health.rs` idioms.
2. **REST**: `GET /api/escalation-metrics` (per-host aggregate across its
   missions). Read-gated like other GETs.
3. **Dashboard**: a "Flight Surgeon" panel — three number cards + the
   ledger table (new React component, follows ProgressLog patterns).
4. **CLI**: `kranz escalation-metrics` — prints the same numbers + ledger
   (text table) for the terminal. Slack is explicitly out of scope (the
   read token work makes it a trivial follow-up if wanted).

## Test gate

- `cargo test --workspace escalation_metrics 2>&1 | grep -qE 'test result: ok. [1-9]'`
  (fixtures: missions with/without operator events, park/grant pairs with
  known latencies, defect tickets with/without traced-from-mission —
  autonomy ratio, percentiles, false-green join, ledger rows all exact)
- Dashboard: component test for the panel (npx vitest); tsc/build/
  sync-embedded/check-embedded/lint green.
- Full gates: `cargo test --workspace`, `cargo clippy --workspace
  --all-targets -- -D warnings`, `cargo fmt --all --check`, `cargo build
  --workspace` — bare exit codes, never piped.

## Anti-vacuity

The aggregator tests must use counts that would differ if any of the four
metrics were unimplemented (e.g. 3 closed missions: 1 autonomous-clean, 1
with a grant, 1 with a steer → ratio exactly 1/3; a park→grant pair at 5s
and one at 900s → p50 distinguishable; 1 traced defect out of 2 completed
→ false-green rate exactly 1/2).
