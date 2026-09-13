---
state: done
state-note: shipped in 22df707 and hardened in 5abccde
title: Codex workspace-write sandbox must grant writes to kranz's worktree (temp-dir path)
priority: 3
schedule: once
---

## Goal
With SessionSpec.writable→--sandbox workspace-write, a Codex WORKER now
attempts file edits (apply_patch) but the writes are REFUSED by Codex's
sandbox, so the worktree stays clean → no feature commit → mission FAILED
(caught honestly by the empty-deliverable gate). Root cause hypothesis:
kranz runs workers in a git worktree under the SYSTEM TEMP DIR
(/var/folders/... which resolves to /private/var/... on macOS), and
Codex's workspace-write writable-roots don't cover that path (Seatbelt
/private symlink mismatch, or temp dir not a default writable root).
Configure Codex's writable roots to include the worktree cwd (e.g.
`-c sandbox_workspace_write.writable_roots=[<worktree>]` or the codex
equivalent), OR run codex workers in a worktree location codex's sandbox
allows. Verify the exact codex knob against `codex exec --help` / `-c`
config. Preserve never-network/never-push.

## Context
Third layer of the Codex-worker integration, found by live-verify
2026-07-07 (m-e96644). Layer 1 = config selection (model-per-role-config,
done). Layer 2 = write-mode flag (fix-nonclaude-backends-need-write-mode,
done — codex now attempts writes). Layer 3 = THIS: the sandbox refuses
writes to the temp-dir worktree. CodexBackend already sets current_dir to
spec.cwd (the worktree) — the gap is the sandbox writable-roots, not the
cwd. Likely applies to Droid too. Note: block-1's empty-deliverable gate
caught this FAILED-honestly even after the validators waived contradictory
findings — the deterministic backstop held.

## Acceptance hints
- A worker.backend=codex mission edits files IN the worktree and lands a
  real feature commit (live-verify: fix-final-gate-judgement-diff-base
  delivers).
- Writable roots cover the worktree only; no network, no push.
- cargo test --workspace green.
