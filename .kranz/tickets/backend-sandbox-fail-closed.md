---
state: done
state-note: Done at 103ff40: BackendKind::supports_sandbox_enforcement() capability seam (exhaustive match), config::validate rejects enforce!=off on codex/droid/kimi/local with role+backend+mode+remedy, readiness parks via the same validation. Edge flagged: executor-tier routing can flip worker to local post-create; readiness re-validation still parks at drain. Tests 2>&1 | grep -qE 'test result: ok. [1-9]'
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
