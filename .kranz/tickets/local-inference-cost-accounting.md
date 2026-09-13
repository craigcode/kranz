---
state: done
state-note: tag+exclude+trailer landed: MissionCostClass (Frontier/Local/Mixed) classifies from folded config+escalation count; calibrate() excludes non-frontier with excluded_non_frontier counter; report.md cost trailer notes local $0-marginal / mixed. Pin test: local+mixed never enter the frontier corpus. Cache-miss modeling split to cache-miss-tier-switch-pricing.md (pri 3)
title: Local-run cost accounting so calibration stays clean
priority: 3
schedule: once
---

## Goal
Account for local-inference runs distinctly from paid frontier runs: a local
turn is ~$0 marginal (fixed hardware + electricity, not per-token). Flag local
runs as "local: $0 marginal" (with optional amortized hardware/watt) so they
do not pollute the calibration corpus or the mission cost trailers when mixed
with paid missions.

## Context
From the review addendum §5 of docs/scoping/local-inference-executor-tier.md —
a gap the original workstream missed. cost.rs prices per-token per-model; a
local mission would show ~$0 next to a $30–160 frontier mission and distort
both the estimate calibration (the corpus is small and hard-won — see the
calibration history) and the per-mission cost receipts. Study: cost.rs pricing
table + how cost lands in events and report.md, the calibration corpus and
shape-aware estimator, the mission-cost-trailers work. Decide: is a local turn
priced $0, or amortized (hardware $ / expected-lifetime-tokens + watts)? Start
with a distinct "$0 marginal (local)" tag that the calibrator EXCLUDES from
frontier-cost calibration; amortized accounting is a later refinement.

Cache-miss discipline (from cursor.com/blog/router): switching models
mid-conversation forfeits the prompt cache, and cached-prefix tokens are
typically ~10x cheaper than uncached — so an escalation that looks free on
per-token price can cost more than staying put once the full prefix is
re-priced uncached at the new tier. Two consequences for this ticket: the
cost model must price a tier switch as a cache-miss event (full prefix at
uncached rate), not just diff per-token rates; and the router/escalation
policy should switch tiers ONLY at feature/milestone edges, where kranz's
fresh-context-per-feature design means there is no warm cache to lose —
mid-feature switches pay the miss for nothing.

## Acceptance hints
- A local-backed mission's cost is tagged local/marginal-zero and is excluded
  from (or bucketed separately in) the frontier-cost calibration corpus.
- Mission cost trailers / receipts distinguish local from paid spend rather
  than reporting a misleading $0.
- cargo test --workspace passes with a pin that a local run does not enter the
  frontier calibration set.
