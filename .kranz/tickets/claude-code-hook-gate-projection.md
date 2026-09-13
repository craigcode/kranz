---
state: done
state-note: Done: hook_gates.rs projection + kranz hook-guard (exit 2 blocks with model feedback); out-of-contract rule enforced in-process; records fold to additive hook.gate.fired before worker.completed; sweep stays authoritative (bypass-tested); non-claude backends unchanged. hook_gate_projection filter: 17 green; full gates green.
title: Project kranz gates onto Claude Code lifecycle hooks
priority: 1
schedule: once
blocked-by: [gate-plugin-interface]
---

## Goal
Deterministic kranz gates are projected onto Claude Code lifecycle hooks so
failures are enforced in-process during the worker session instead of
discovered afterwards; the full transcript and tool-call record land in the
event log.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-302). This is an
extension of backend_claude — the mission→Claude Code session mapping and
transcript capture already exist. NOT the engine's hooks.rs (that module is
D-F webhook triggers; distinct concept, keep the names apart). The source
plan blocked this on the ACP worker; relaxed — backend_claude is the session
seam and ACP is not required. Design rule: in-process enforcement is
defense-in-depth, never a replacement — the engine-side gate ladder remains
authoritative (same philosophy as sandbox-vs-scrutiny: hooks bound what the
session agrees to; engine gates judge what actually happened).

## Acceptance hints
- Per-session hook config is generated; a gate-failing action inside the
  session triggers the hook and surfaces as a structured event before
  session end.
- A hook-bypassed failure is still caught by the engine-side gate
  afterwards (test proving the authoritative layer is unchanged).
- Sessions on backends without hook support behave exactly as today
  (regression).
- Anti-vacuity grep on a named filter unique to this work.
