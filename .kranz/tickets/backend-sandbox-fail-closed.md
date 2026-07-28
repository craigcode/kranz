---
title: Reject sandbox-configured non-Claude backends instead of silently ignoring enforcement (P1)
priority: 1
schedule: once
---

## Goal
Codex/Droid/Kimi silently discard the resolved sandbox wrapper today —
Codex uses its own --sandbox (different semantics), Droid gets --auto
high, Kimi gets --yolo (backend.rs, backend_droid.rs:314, tier-3 build
note). A configured kranz sandbox must never be silently unenforced:
reject the backend/sandbox combination at config validation and readiness
(fail closed), or apply the resolved wrapper in a backend-independent
spawn layer.

## Context
From the review (P1 #2). v1: refuse loudly at approve/preflight when
sandbox.enforce != off and the role's backend cannot honor kranz's
fs/extraWrite/egress policy. The uniform spawn layer is the later,
larger fix.

## Acceptance hints
- sandbox.enforce set + codex/droid/kimi backend ⇒ clear config error at
  validation and a readiness failure naming the pair — never a silent run.
- Claude path unchanged.
- cargo test --workspace green.
