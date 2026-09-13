---
state: done
state-note: Already shipped: kranz ready --all (ready.rs assess_all/render_org) — org table + per-repo drill-down + AMM projection; unavailable roots reported with a reason (never zero-scored), malformed catalogs explained, single-repo unchanged. Verified green 2026-08-05: org_view_scores_catalog_and_counts_l3_plus, org_view_degrades_unavailable_repos_with_a_reason, org_view_explains_empty_and_malformed_catalogs + full ready filters.
title: kranz ready across the multi-root host catalog
priority: 3
schedule: once
---

## Goal
`kranz ready` aggregates across the M8 host catalog: one org-level
readiness scorecard over all configured repos, with per-repo drill-down and
the AMM-compatible projection preserved.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-328). Mostly shipped
per-repo: crates/cli/src/ready.rs (read-only deterministic scorecard) and
amm.rs (AMM projection, mapped never adopted) exist, as does the M8
multi-root host catalog. This is the aggregation view only — it stays
read-only and deterministic; it runs no test suites and starts no model
turns (ready.rs house rule).

## Acceptance hints
- A two-repo fixture catalog yields an org table plus per-repo detail.
- An unavailable root is reported as unavailable, never zero-scored (no
  fabricated numbers).
- Single-repo behavior unchanged (regression).
- Anti-vacuity grep on a named filter unique to this work.
