---
state: done
title: Hosted drain must restore the primary checkout at drain exit
priority: 3
schedule: once
---

## Goal

The serve-hosted queue drain (POST /api/queue/drain) leaves the repo
checkout on the last mission's branch when the drain finishes. The CLI
dispatcher (`kranz work`) restores the dispatch-time checkout at
drain-exit and --once boundaries; the hosted drain must honor the same
contract so an operator's next terminal interaction doesn't land on a
kranz/mission-* branch unexpectedly.

## Context

Observed live 2026-07-06 on the first production use of the hosted
drain: after m-ba8d58 completed, `git branch --show-current` reported
kranz/mission-m-ba8d58. The restore rules from
fix-checkout-restore-on-completion apply verbatim: capture the branch
at drain start, restore at drain exit, skip restore if the captured
branch is itself a kranz/mission-* branch, abort restore on a dirty
tracked tree.

## Acceptance hints

- Hosted drain captures the checkout branch when the drain task starts
  and restores it when the drain task exits (queue empty or fatal).
- No restore when the captured branch is a kranz/mission-* branch or
  the tracked tree is dirty at exit (mirror the CLI dispatcher rules).
- Host-level test with a fake mission run asserting the branch at
  drain-exit equals the branch at drain-start.
