---
state: done
state-note: split complete to the honest end-state across waves 1-4c (11 commits): orchestrator.rs 11,399 → 7,749 (-32%) via command_exec, report_render, preflight, judgement, mission_catalog, planning, findings; bridge.rs 7,549 → 4,615 (-39%) via approve_flow, dispatch, outbound_engine, commands. Every extraction pure code motion with byte-identity checks, four gates green per commit, CI green per push. What remains in orchestrator.rs is the run-loop core (run/run_loop/run_feature/validation_round/final_ga … (truncated)
title: Split the orchestrator.rs and slack bridge.rs monoliths along existing seams
priority: 3
schedule: once
---

## Goal
Break `crates/engine/src/orchestrator.rs` (~10,090 LOC) and
`crates/slack/src/bridge.rs` (~7,473 LOC) into cohesive modules via pure,
behavior-preserving code motion: natural extraction candidates are report/
index rendering, gate-command running, planning/judgement turn helpers, and
preflight out of orchestrator.rs; dispatch, the approve flow, outbound cursor
management, and pipeline rendering out of bridge.rs. No contract changes
(events.rs/types.rs untouched), no behavior changes, tests move with their
code.

## Context
Repo review 2026-07-18 flagged both files as the workspace's review and
onboarding bottlenecks and the most likely merge-pain sites (orchestrator.rs
alone carries the engine struct + run loop + planning + judgement + report
rendering + ~56 inline tests). AGENTS.md's "new abstractions earn their keep
on the third use" applies: this is not speculative generality — the seams
already exist as section comments. Do it incrementally (one extraction per
commit) so `git log --follow` stays useful and review stays possible.

## Acceptance hints
- Each extraction is a separate commit; the final diff is overwhelmingly
  moves, not edits.
- No public API or event-schema change; `crates/engine` external behavior
  identical (mission_test.rs suite unchanged in meaning).
- cargo test --workspace, clippy -D warnings, fmt --check all green after
  every commit.
