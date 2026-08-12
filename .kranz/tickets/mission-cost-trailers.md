---
state: done
state-note: landed in this polish commit; reviewed + gates green
title: Mission + cost trailers on kranz-authored merge and report commits
priority: 3
schedule: once
---

## Goal

kranz-authored commits that land durable history carry structured git
trailers so mission attribution and cost survive in the git metadata
channel itself: the gated merge commit gets `Kranz-Mission: <id>` and
`Kranz-Cost-USD: <actual>` (plus token totals), and the mission report
commit gets the same. `git log --grep` / `git interpret-trailers` can
then answer "what did this merge cost" forever, even if .kranz runtime
state is lost, without any kranz tooling installed.

## Context

Cost today lives in the event log and report.md — rich but kranz-shaped
and repo-runtime-local. Trailers piggyback git's own metadata channel:
greppable in any git UI, travel with clones/mirrors, zero schema
maintenance. Sites: the gated merge commit construction
(crates/engine/src/merge.rs) and write_mission_report's commit
(orchestrator.rs); actual cost is already computed for report.md at
completion time. Keep trailers append-only facts (no estimates — only
actuals known at commit time).

## Acceptance hints

- A gated merge commit message ends with Kranz-Mission and
  Kranz-Cost-USD trailers parseable by `git interpret-trailers`.
- Report commits carry the same pair.
- Trailer formatting covered by tests (exact key names, one value per
  line); cargo test --workspace green.
