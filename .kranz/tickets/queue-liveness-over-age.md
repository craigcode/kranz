---
state: done
state-note: Done at 5d6e9f6: three-way ClaimPidLiveness — Alive claim stands at any age, Dead requeues, Unknown (non-unix/EPERM) keeps the age backstop. Windows conservative posture documented + cfg(not(unix)) test. Tests 2>&1 | grep -qE 'test result: ok. [1-9]'
title: Dead-claim recovery: age only breaks PID ambiguity, never overrides confirmed liveness (P2)
priority: 3
schedule: once
---

## Goal
queue.rs:508 treats any claim older than one hour as dead even when its
PID is alive, so a second dispatcher can requeue a genuinely long mission
(duplicate attempts). Age must only resolve ambiguous PID reuse — never
override a live, verified PID.
