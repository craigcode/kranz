---
title: Label container fs+net advisory until the sidecar boundary exists (P2)
priority: 3
schedule: once
---

## Goal
Container fs+net is documented-but-bypassable: the default bridge plus
proxy env vars means a process can open a direct socket. Either reject
this mode until the internal-network/sidecar boundary exists, or label it
"advisory" everywhere it is presented (config docs, module docs, preflight
output), never "enforce".

## Context
From the review (P2). The egress proxy covers env-respecting processes;
deterministic bypass needs the network boundary (P3.3 follow-up).

## Acceptance hints
- User-facing copy no longer claims enforcement for container fs+net with
  proxy-env-only coverage, or the mode refuses with a pointer to the
  boundary work.
