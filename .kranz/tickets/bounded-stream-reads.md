---
title: Bound backend stream reads and local backend timeouts (P2)
priority: 3
schedule: once
---

## Goal
All CLI backends accumulate unbounded stderr/stdout streams in memory and
the local backend has no request timeout or response cap. Bound them
(stream caps with truncation markers, request timeouts), so a noisy or
malicious CLI/endpoint cannot hang or exhaust the host.
