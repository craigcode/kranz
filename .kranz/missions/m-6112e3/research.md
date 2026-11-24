# Research — m-6112e3

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- docs/knowledge/decisions/positioning-governance-evidence-layer.md
- docs/knowledge/lessons.md
- crates/engine/src/lessons.rs
- crates/engine/src/judgement.rs
- crates/engine/src/orchestrator/finalization.rs
- crates/engine/src/contract_sweep.rs
- crates/engine/src/review_memory.rs
- crates/engine/prompts/orchestrator.md
- crates/engine/tests/mission_test.rs
- crates/cli/src/cli.rs
- crates/cli/src/commands.rs
- crates/engine/src/queue.rs
- crates/engine/src/event_log.rs
- crates/cli/Cargo.toml
- crates/engine/Cargo.toml
- .kranz/lessons/index.md
- .kranz/tickets/lesson-to-check-promotion.md
- .gitattributes

## External sources

- repository read-only inspection at mission base

## Facts

- The planning renderer inlines full bodies of the newest provenance-clean lessons and reads only .md names listed in index.md — `crates/engine/src/lessons.rs:200-305, parse_manifest_line requires .md`
- Lesson provenance requires the add-commit to be a [kranz] mission report and the working tree to match HEAD — `crates/engine/src/judgement.rs:498-533 lesson_provenance_clean`
- The capture-turn prompt lives in judgement.rs prepare_lesson, not prompts.rs/knowledge.rs — `crates/engine/src/judgement.rs:305-319`
- Lesson paths returned by write_lesson are folded into the same report commit — `crates/engine/src/orchestrator/finalization.rs:1075-1095`
- No .kranz/lessons path is sweep-exempt — `crates/engine/src/contract_sweep.rs:156-170`
- The existing happy-path test asserts a four-file report commit with a lesson reply lacking any CHECK line — `crates/engine/tests/mission_test.rs:1348,1519-1532`
- Engine locks are events.jsonl.lock per mission and .mutate/.repo.busy/.repo.merge locks under .kranz/queue — `crates/engine/src/event_log.rs:495-516, crates/engine/src/queue.rs:144-548`
- CLI package is `kranz`, engine package is `kranz-engine` — `crates/cli/Cargo.toml, crates/engine/Cargo.toml`
- No test function named lesson_check_* exists at base — `grep 'fn lesson_check' crates → 0 matches`
- No merge driver exists for .kranz/lessons, so a shared append file would conflict across concurrent missions — `.gitattributes`

## Ambiguities & stale docs

- D-X (who classifies) was asked but not answered by the operator; this plan assumes 'both, operator wins'. Reject at approval to choose orchestrator-only or operator-only.
- Whether the capture-turn CHECK question counts as inside the prompt freeze; the plan treats it as capture-side record-keeping because prompts.rs, knowledge.rs and orchestrator.md are untouched and the record is never injected.
- Backfill of the 43 existing lessons is out of scope; they read as unclassified.

## Candidate knowledge updates

- docs/knowledge/lessons.md: document the .check companion record, precedence, listing and plain-link rationale (feature in this plan)
- docs/knowledge/decisions/positioning-governance-evidence-layer.md: recheck line for the new read-only record surface (feature in this plan)
