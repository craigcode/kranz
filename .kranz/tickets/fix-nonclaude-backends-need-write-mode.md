---
state: done
state-note: landed in 670b896 (codex-built, reviewed, gates green)
title: Non-Claude backends (Codex, Droid) run read-only — unusable as builder/worker roles
priority: 2
schedule: once
---

## Goal
model-per-role-config lets any role select backend=codex/droid, but those
backends were built scrutiny-only and hardcode a NON-WRITING permission
level: CodexBackend passes `--sandbox read-only` (backend_codex.rs ~184),
so a codex WORKER can read but not edit/commit → empty deliverable. (Droid
likely the same — `--auto low`.) Make the backend's sandbox/permission
level ROLE-AWARE: read-only for scrutiny/functional validation, a WRITING
mode (codex `--sandbox workspace-write`; the droid equivalent) for worker/
builder roles. Preserve the never-push guarantee (write to workspace, no
network/push).

## Context
Found by a live-verify 2026-07-07: set worker.backend=codex, ran a small
ticket (fix-final-gate-judgement-diff-base, m-6c3518). The config/selection
layer worked and the safety machinery worked PERFECTLY — the empty codex
worker produced no commits, three validation rounds found the milestone
unimplemented, the fix-cycle cap tripped, and the mission BLOCKED honestly
(no false COMPLETE — block-1's safety net caught a failure mode it wasn't
even built for). The ONLY missing piece is write-mode: the non-claude
backends can't act as builders until their sandbox is role-aware. This is
the last gap before Codex/Droid/Fable workers are real through kranz.

## Acceptance hints
- CodexBackend (and DroidBackend) run in a WRITING sandbox for worker/
  builder roles and read-only for scrutiny; role drives the flag.
- A worker.backend=codex mission edits files and lands a real commit
  (live-verify: the same fix-final-gate ticket delivers).
- Never-push preserved; cargo test --workspace green.
