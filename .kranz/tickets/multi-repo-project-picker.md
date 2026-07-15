---
title: Multi-repo project picker with groups, pins, search, and activity counts
priority: 2
schedule: once
---

## Goal
Build the M8 operator surface for choosing and monitoring many repos from one
kranz serve: a project picker with pinned repos, optional groups, search, and
at-a-glance counts for queued, running, needs-input, complete-unmerged, and
failed work. The surface should route into each repo's missions and tickets
without weakening per-repo gates, auth, or queue serialization.

## Context
Mission Control's home grid is the best direct inspiration: every project is a
card with path, branch, status counts, grouping, pinning, density, and search.
Kranz needs the same orientation layer once one Slack bridge or one server can
host several repos.

Keep the kranz boundary clear. This is not a terminal launcher and not an IDE
workspace. It is the repo selection and work-state overview needed for M8:
one bridge, many repos; one dashboard, many repo-local mission logs.

## Acceptance hints
- A single serve instance can list multiple configured repos and show status
  counts without reading or mutating another repo's runtime state incorrectly.
- Pins, groups, and search are persisted in operator config, not inside the
  target repos unless deliberately chosen.
- Selecting a repo scopes tickets, missions, merge gates, Slack routing, and
  mutation token behavior to that repo.
- The UI distinguishes complete-but-unmerged from landed work.
- Missing, moved, or temporarily unavailable repos degrade to a visible error
  row instead of breaking the whole picker.
