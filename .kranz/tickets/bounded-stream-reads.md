---
state: done
state-note: Done at 0d59945: stream_bounds.rs (TailWindow/BoundedLines/drain_to_tail); all four CLI backends capped (8MiB line, 64KiB stderr tail at EOF); backend_local 600s timeout + 8MiB body cap; truncation marker survives tailing. Tests 2>&1 | grep -qE 'test result: ok. [1-9]'
title: Bound backend stream reads and local backend timeouts (P2)
priority: 3
schedule: once
---

## Goal
All CLI backends accumulate unbounded stderr/stdout streams in memory and
the local backend has no request timeout or response cap. Bound them
(stream caps with truncation markers, request timeouts), so a noisy or
malicious CLI/endpoint cannot hang or exhaust the host.
