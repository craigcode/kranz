# Research — m-dd7d22

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/escalation_metrics.rs
- crates/engine/src/outcomes.rs
- crates/engine/src/report_render.rs
- crates/engine/tests/report_test.rs
- crates/engine/src/events.rs
- crates/engine/src/mission_catalog.rs
- crates/cli/src/output.rs
- crates/cli/src/cli.rs
- crates/cli/Cargo.toml
- docs/metrics.md
- CHANGELOG.md
- .github/workflows/ci.yml
- .kranz/missions/m-caf631/events.jsonl
- .kranz/missions/m-882080/events.jsonl

## External sources

- repository read-only inspection at main 8ca52214

## Facts

- kranz outcomes' AutonomyRatio counts ABANDONED missions as closed. — `crates/engine/src/outcomes.rs:481-488 (is_closed includes MissionAbandoned)`
- outcomes.rs has its own intervention loop that omits operator unblocks and classifies steers by ts. — `crates/engine/src/outcomes.rs:495-513`
- escalation_metrics.rs excludes abandoned from the autonomy denominator and counts four intervention classes, but keeps only a total. — `crates/engine/src/escalation_metrics.rs:286-327, 439`
- docs/metrics.md publishes the escalation-metrics ratio. — `docs/metrics.md:34 'kranz escalation-metrics --json → .autonomy'`
- The mission report is rendered only at completion, so abandoned missions have no report. — `crates/engine/src/orchestrator.rs:7455, orchestrator/finalization.rs:1095`
- Both motivating missions completed features and took one steer and one operator unblock each before being abandoned. — `.kranz/missions/m-caf631/events.jsonl lines 159,218,336,350,506; m-882080 lines 382-1415`
- The motivating missions' branches no longer exist locally, so a derived merge link reads unknown. — `git branch --list 'kranz/mission-m-caf631' 'kranz/mission-m-882080' → empty`
- Outcomes is built literally in slack tests, so adding a field needs those literals updated. — `crates/slack/tests/formatting.rs:436,541`
- MissionAbandoned is built in about 14 places across engine, cli and server. — `grep 'MissionAbandoned {' crates (non-pattern)`

## Ambiguities & stale docs

- The brief says `kranz outcomes` reports the escalation_metrics ratio and that abandoned missions are outside its denominator. Neither is true for outcomes.rs. The plan leaves both ratios unchanged, pins today's outcomes numbers in a test, and files a divergence ticket. Operator did not answer Q1; default (a) applied.
- D-X was not answered. D-X1 (count only) is applied and the options are recorded in a ticket for the operator.
- 'Split by the existing classes' is read as the four classes in escalation_metrics.rs's doc comment (grant decisions, steers, revision rulings, operator unblocks).

## Candidate knowledge updates

- docs/knowledge glossary: 'operator intervention' has two implementations (escalation_metrics.rs canonical; outcomes.rs divergent) until the divergence ticket is resolved.
