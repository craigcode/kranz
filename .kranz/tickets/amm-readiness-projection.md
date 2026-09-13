---
state: done
state-note: implemented operator-driven in d0fa348: contract_health fold (engine), amm.rs projection (LADDER v1, ordinal labels), ready --all org view, readiness-axes.md note; dogfooded L2 on this repo, gitignore finding ticketed
title: AMM-compatible readiness projection over kranz-native signals, plus a contract-health axis and org metric
priority: 2
schedule: once
---

## Goal
Extend `kranz ready` from a scorecard into a two-axis readiness view whose
source of truth stays kranz-native: (1) an AMM-compatible projection —
level L1–L5 plus missing signals — derived from the native signals, with
Factory's 5/19/36/60/100 thresholds living in ONE mapping table as ordinal
labels (never gospel; the paper will revise them); (2) a second axis the
whitepaper cannot see — contract/consent health: contract-lint pass rate at
approval, secret-scan FP friction (waivers per mission), blocked-cause
histogram (grant vs contract-bug vs scan vs gate); (3) the org metric across
the M8 catalog: "N of M repos at L3+", computed by running the same probe
per catalog repo (CLI view first; REST surface only if cheap).

## Context
factory-autonomy-maturity-model-whitepaper.pdf (~/downloads) + the
meta-review quibbles this ticket encodes: map to AMM, do NOT adopt it (their
thresholds are marketing numbers); the moat is not on their map — the last
three bites came from the contract/consent axis the paper doesn't measure;
and the dogfood score is already done: this repo is L3− (P6 file size,
test/validation speed, and remote exec are the named gaps). ready.rs already
probes most foundation signals (merge gates, gitignore hygiene, backend
lanes, test runner detection); the AMM Eight Pillars map closely onto its
dimensions. Event logs carry the contract-health data (grant.requested,
milestone.blocked, validation.finding, waiver events).

## Acceptance hints
- `kranz ready --json` emits: native signals (unchanged), `amm` projection
  (level + missing signals + the mapping table version), `contractHealth`
  (lint pass rate, FP waivers/mission, blocked-cause counts) for repos with
  mission history (absent otherwise).
- Threshold mapping lives in one table/function with a comment: ordinal
  labels, not sacred integers.
- Org view: `kranz ready --all` (or serve) reports per-repo level and the
  N-of-M-at-L3+ headline across the host catalog; unavailable repos degrade
  with a reason, never silently.
- Docs: knowledge/validation/gates.md or a new vault note describes the two
  axes and why the second exists.
- cargo test --workspace green, incl. a fixture repo scored at each level
  boundary and a contract-health fixture computed from a seeded events.jsonl.
