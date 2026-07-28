---
title: Dead-claim recovery: age only breaks PID ambiguity, never overrides confirmed liveness (P2)
priority: 3
schedule: once
---

## Goal
queue.rs:508 treats any claim older than one hour as dead even when its
PID is alive, so a second dispatcher can requeue a genuinely long mission
(duplicate attempts). Age must only resolve ambiguous PID reuse — never
override a live, verified PID.
