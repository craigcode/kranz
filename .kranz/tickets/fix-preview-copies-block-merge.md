---
title: Worktree-mode deliverable preview copies block the gated merge of their canonical files
priority: 2
schedule: once
---

## Goal

In worktree mode the engine writes untracked human-readable plan.md /
report.md copies into the PRIMARY .kranz/missions/<id>/ dir for
operator visibility (sandbox-1 M2.3). The gated merge then brings the
canonical TRACKED files at the same paths and git refuses: "untracked
working tree files would be overwritten by merge" — failing before
MERGE_HEAD exists, which also made merge_mission's abort path report a
confusing secondary error. Fix both: (1) relocate the preview copies to
a non-colliding path (e.g. .kranz/missions/<id>/preview/plan.md, still
gitignored via the runtime template) OR teach merge_mission to remove
untracked files that are byte-identical to the incoming canonical
version before merging; (2) make merge_mission tolerate a merge that
failed pre-MERGE_HEAD without attempting git merge --abort.

## Context

Hit live 2026-07-06 on the FIRST worktree-mode merge (m-671b04):
operator verified both untracked copies byte-identical to canonical,
removed them by hand, merge then succeeded (2e5b3b0). Every future
worktree-mode mission reproduces this at merge time until fixed —
p2 because it breaks the primary happy path of the new mode.

## Acceptance hints

- A worktree-mode mission's gated merge succeeds with preview copies
  present (test: temp repo, preview files in place, merge_mission
  lands the canonical files).
- A merge that conflicts pre-MERGE_HEAD surfaces the git refusal
  verbatim without the "abort also failed" wrapper.
- Preview copies remain readable post-merge wherever they land;
  cargo test --workspace green.
