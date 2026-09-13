---
state: done
state-note: Fixed on codex/stabilization-proof-sprint after M7 mission m-ed91b6 showed every sandboxed Apple Git invocation attempting a denied xcrun_db refresh.
title: Prewarm Apple command-line-tool shims before Seatbelt sessions
priority: 1
schedule: once
traced-from-mission: m-ed91b6
---

## Goal

Refresh the Apple command-line-tool shim cache outside every resolved macOS
Seatbelt session so ordinary Git commands do not attempt to write the shared
`xcrun_db*` cache from inside the sandbox.

## Context

Engine-run gate wrappers already used the bounded prewarm-plus-deny posture.
Normal worker and mandatory-contained validator sessions generated the same
deny-default profile without the prewarm. Live M7 proof `m-ed91b6` therefore
printed `couldn't create cache file ... xcrun_db ... Operation not permitted`
on every Git invocation. Git happened to continue on this host, but a stale
shim cache is documented to fail loudly and would prevent normal inspection.

## Acceptance hints

- Worker/orchestrator session resolution and mandatory validator containment
  invoke the existing bounded host-side prewarm once per resolved Seatbelt
  session, never per wrapped command.
- No write allowance for the shared `xcrun_db*` surface is added to SBPL.
- A live macOS test resolves a profile and runs Apple Git successfully without
  an `xcrun_db` cache-write diagnostic.
