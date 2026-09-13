---
state: done
title: Multi-repo project picker with groups, pins, search, and activity counts
priority: 3
schedule: once
blocked-by: [m8-multi-root-host-design]
---

## Goal
Build the M8 operator home surface for choosing and monitoring many repos
from one kranz serve: pinned repos, optional groups, search, and at-a-glance
counts for queued, running, needs-input, complete-unmerged, and failed work.
Route into each repo's missions/tickets without weakening per-repo gates,
auth, or queue serialization.

## Context
Mission Control's project grid is the orientation reference — not a terminal
launcher. This ticket is UI/API on top of the accepted multi-root host design
(`m8-multi-root-host-design`). Do not invent a second token or queue model
here.

## Acceptance hints
- Lists configured repos with status counts using the host's scoped reads
  only (no cross-repo state bleed).
- Pins/groups/search live in operator config as decided by the host design.
- Selecting a repo scopes subsequent UI/API actions per that design's auth
  and routing rules (tested against the design's fixtures).
- Distinguishes complete-unmerged from landed.
- Missing/moved repos show an error row; picker remains usable.
- Anti-vacuity grep on the named filter.
