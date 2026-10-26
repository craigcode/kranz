---
state: done
title: Signal intake 4: deterministic triage
priority: 2
schedule: once
blocked-by: [signal-intake-2-ledger]
---

## Goal

Triage normalised signals deterministically: dedupe by fingerprint within a window, map the source project to a repo, classify the outcome by declared rules, and list recently merged missions whose paths overlap the signal's frames.

## Context

Design of record: `docs/scoping/signal-intake.md`. Read it in full before planning and do not contradict a decided or proposed section. Slice 4, decisions D-E and D-F. No model reads signal content here.

The host catalogue (`crates/server/src/multi.rs`) lists repos; nothing maps a monitoring project to one. Merged missions and their diffs are recoverable from the mission logs and branches.

## Scoping answers

- Project-to-repo mapping and classification rules live in config; an unknown project parks.
- Outcomes: proposed, parked, not a code change. Each is a ledger record with the rule that decided it.
- Dedupe window configurable per source, default 24 hours; repeats increment a count and add a ledger record.

## Acceptance hints

- Ten duplicates within the window produce one triaged signal with count 10.
- An unknown project produces a parked signal; an infrastructure-only rule match produces "not a code change" with no ticket.
- Path-overlap candidates are listed for a seeded merged mission and empty when nothing overlaps.
