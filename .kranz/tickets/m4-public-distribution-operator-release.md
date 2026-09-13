---
state: wontfix
state-note: "Closed 2026-08-13 while the repo stayed private. Superseded 2026-08-22 by owner-approved v0.2.0 cut — see ticket v0.2.0-public-cut."
title: Cut the first version-aligned public Kranz distribution
priority: 1
schedule: once
---

## Goal

After the repository owner authorizes public visibility, cut one coherent
0.2.0 release across GitHub source/binaries, the four reserved crates.io
packages, Tauri metadata, and a real Homebrew tap, then prove installation on
clean hosts without cloning the repository.

## Context

**Superseded:** active cut work is
[`v0.2.0-public-cut`](v0.2.0-public-cut.md) and
[docs/reviews/v0.2.0-cut-checklist.md](../../docs/reviews/v0.2.0-cut-checklist.md).

The clean-origin migration is complete: `craigcode/kranz` is the active private
repository and all superseded repositories are retained as private archived
origins. The public-tree/public-history audits pass at the latest 2026-08-14
clean-origin rotation boundary. The second rotation removed private author
metadata from a GitHub merge commit after the required post-merge audit caught
the recurrence; a disposable server-side merge then proved the corrected
no-reply identity before rotation. That receipt remains point-in-time evidence
rather than a continuing publication claim. This ticket closed while the owner
kept the repository private; the 2026-08-22 policy change opened the successor
ticket instead of reopening this one.
