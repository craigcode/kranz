---
state: done
state-note: "Done: a declared pty-script that SKIPs is recorded and renders FAIL (declared pty-script did not execute) at the round; the final gate flags every declared assertion lacking a validation.pty.transcript verdict as a non-waivable command-assertion finding — it can block but can never vacuously complete. Undeclared/no-pty contracts byte-identical. declared_pty filter: 4 green (e2e block + e2e green-on-executed + unit pins); full workspace gates green."
title: A declared pty-script that SKIPs can still green the final gate
priority: 2
schedule: once
---

## Goal
Close the vacuous-green hole for declared pty scripts (6346ba2): when a plan declares a pty-script assertion but the script SKIPs (non-unix host, openpty failure), the run renders soft SKIP-UNDER-WRAP lines and emits no Fail transcript, and the final gate does not re-run the pty assertion — so the mission can green without the declared functional validation ever executing. Fail (or refuse plan approval) when a declared pty-script did not execute; keep the honest SKIP marker for platforms where pty was never declared. 14th-pass review finding.

## Context


## Scoping answers

## Acceptance hints
