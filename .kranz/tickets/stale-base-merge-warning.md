---
title: Warn when merging a mission whose base is stale relative to sibling merges
priority: 3
schedule: once
---

## Goal

When several missions are drafted from the same base and merged
sequentially, later merges carry cross-branch SEMANTIC conflicts git
merges silently — a required struct field added on branch A is missing
from a new initializer added on branch B; both merge clean, the build
breaks. The full gate suite catches these post-merge, but the operator
gets no warning at merge time that the mission's base is stale. Add a
merge-time signal: when a mission's pinned base_sha is far behind the
live base (N sibling merges have landed since), the gated merge emits a
loud "stale base — cross-branch semantic conflicts likely; full gates
will run" notice, and optionally offers/points to a rebase.

## Context

Receipt 2026-07-06: the M7 batch (sandbox-2/3, droid, two p3s) was
drafted from one base and merged sequentially; four of five merges hit
the missing-`sandbox`-field class plus fmt drift — every one caught by
bare-exit-code gates, but each cost a manual fixup. The durable
prevention is the sequential draft-after-merge discipline (draft N+1
only after N lands) already stated for the blocked chain; this ticket is
the SAFETY NET for when batching happens anyway: make staleness visible
at merge time rather than discovered by a broken build. Consider whether
the drain should optionally re-fork/rebase each queued mission onto
current main before running (bigger change — flag as a design option,
don't assume).

## Acceptance hints

- The gated merge computes how many merges have landed on the live base
  since the mission's base_sha and warns when that exceeds a threshold.
- The warning is informational (never blocks); full gates still run and
  remain the real guarantee.
- Test: a mission whose base_sha trails the live base by >=N surfaces
  the stale-base warning; a fresh-based mission does not.
