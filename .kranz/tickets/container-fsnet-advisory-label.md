---
state: done
state-note: Done at e2eb3c6: config::validate rejects container+fs+net+non-empty-egress (proxy-env-only bypass) with remedies; SandboxProvider::enforces_hard_net_boundary(egress); empty-egress container (--network none) stays accepted. Restores the original tier-3 design intent. Tests 2>&1 | grep -qE 'test result: ok. [1-9]'
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
