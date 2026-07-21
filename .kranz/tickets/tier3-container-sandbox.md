---
title: Tier-3 container sandbox provider for validation and execution (microVM isolation)
priority: 3
schedule: once
---

## Goal
Add the M7 Tier-3 sandbox provider: run worker/validator sessions (and
optionally gate suites) inside a container/microVM with a defined fs+net
policy, instead of relying on Seatbelt/bwrap process sandboxing alone. The
provider must preserve the current fail-closed contract: an unsupportable
requested mode refuses the run loudly, never degrades silently to
unsandboxed.

## Context
M7 shipped tiers 1–2 (worktree isolation; Seatbelt fs on macOS; bwrap
fs/fs+net on Linux) — but macOS has NO network boundary (Seatbelt can't
express hostname egress) and Windows has no sandbox at all. At AI Tinkerers
SF (2026-07), Chunk demonstrated lightweight microVMs running CI-grade
validation inside the agent's inner loop — independent confirmation that
isolated-execution-in-the-loop is where the industry is going. A container
provider closes both gaps with one mechanism and gives the macOS fs+net
story an honest answer. Scope for the plan: engine-side provider interface
first (spawn session in container vs process), one runtime (container/
microVM of the worker's available tooling), no orchestration platform.

## Acceptance hints
- A role can request `sandbox.provider: "container"`; sessions then run
  inside it with the declared write/egress policy; fs+net egress policy is
  enforcible on macOS hosts for the first time (documented how).
- Unsupported/refused modes fail closed (SandboxDecision::UnsupportedWarn
  must be refused, same as tiers 1–2).
- Workspace gates (fmt/clippy/test) pass; a smoke test runs a trivial worker
  inside the provider and asserts the isolation boundary (write outside
  policy denied).
