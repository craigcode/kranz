---
state: done
state-note: "7f0ef53 + d8d86de shipped the M7 Tier-3 container provider with runtime detection, read-only write boundary, fail-closed resolution, and a Docker smoke receipt on Ubuntu. Support update 2026-08-24: the release-supported host is Linux only because hosted macOS cannot renew its operator Colima proof as a CI gate; macOS uses native Seatbelt and enforced container configuration fails closed."
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

> Support update (2026-08-24): the release-supported container sandbox host is
> Linux. The macOS operator receipt is retained as historical evidence, but
> hosted CI cannot provision Colima; macOS uses the native Seatbelt process
> provider and enforced container configuration fails closed.

## Context
M7 shipped tiers 1–2 (worktree isolation; Seatbelt fs on macOS; bwrap
fs/fs+net on Linux) — but macOS has NO network boundary (Seatbelt can't
express hostname egress) and Windows has no sandbox at all. At AI Tinkerers
SF (2026-07), Chunk demonstrated lightweight microVMs running CI-grade
validation inside the agent's inner loop — independent confirmation that
isolated-execution-in-the-loop is where the industry is going. A container
provider originally aimed to close both gaps with one mechanism and give the
macOS fs+net story an honest answer. Scope for the plan: engine-side provider
interface first (spawn session in container vs process), one runtime
(container/microVM of the worker's available tooling), no orchestration
platform.

## Acceptance hints
- A role can request `sandbox.provider: "container"` on Linux; sessions then
  run inside it with the declared write/egress policy. Unsupported hosts fail
  closed and macOS uses the native Seatbelt process provider.
- Unsupported/refused modes fail closed (SandboxDecision::UnsupportedWarn
  must be refused, same as tiers 1–2).
- Workspace gates (fmt/clippy/test) pass; a smoke test runs a trivial worker
  inside the provider and asserts the isolation boundary (write outside
  policy denied).
