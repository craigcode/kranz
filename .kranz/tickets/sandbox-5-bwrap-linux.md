---
title: Linux bubblewrap parity for enforce=fs and fs+net
priority: 2
schedule: once
blocked-by: [sandbox-3-seatbelt-fs]
---

## Goal
M7 tier 2 Linux parity: implement the same fs and fs+net enforcement via bubblewrap (landlock considered if bwrap unavailable), sharing the allowlist-construction code with the Seatbelt path so the two platforms cannot drift. CI-validated on ubuntu-latest; degrade loudly (preflight issue + refusal under a floor) when bwrap is absent.

## Context
Design of record: docs/scoping/worker-sandboxing.md (threat model with
receipts, three tiers, sequencing, open questions) — read fully before
planning. Fresh incident receipts strengthening tier 1, all from
2026-07-05/06: an operator commit landed on a live mission's branch
(twice); sequential drafts stacked mission branches; a checkout
crossfire between branches tracking and main ignoring runtime files
deleted six ticket sidecars. Every one is impossible once workers live
in worktrees and the primary checkout never moves. Engine seams: M3's
GitRepo::add_worktree/remove_worktree/prune_worktrees + the parallel
path in orchestrator.rs run_parallel_batch_inner; permissions.rs;
backend_claude.rs spawn. Missions must gate --workspace, not -p
kranz-engine (repo meta-lesson).


## Scoping answers

## Acceptance hints

## Implementation notes

The Linux path resolves `enforce: "fs"` and `"fs+net"` to bubblewrap when
`bwrap` is available, shares the same filesystem write allowlist construction
as Seatbelt, and adds `--unshare-net` for `fs+net` so network access fails
closed instead of pretending to offer hostname egress. If `bwrap` is missing,
requested enforcement now refuses before launch rather than running
unsandboxed.

Regression coverage locks the generated argv (`--ro-bind / /`, allowlisted
`--bind` write roots, `--chdir`, and `--unshare-net`) and the missing-bwrap
resolution path. A live ubuntu runner remains the external proof to add once
CI has bubblewrap/user namespaces available.
