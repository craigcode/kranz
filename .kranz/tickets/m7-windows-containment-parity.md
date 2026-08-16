---
state: open
state-note: Accepted plan and phase-1 safety proof implemented on codex/roadmap-m8-m7-proof: Windows process enforcement remains fail-closed; the previously target-agnostic container resolver now also refuses Windows even when docker.exe is detected; exact session/gate refusal tests run in the Windows CI leg. Native AppContainer implementation and hostile Windows 11 receipt remain open.
title: Define and prove a fail-closed Windows containment path for enforced sessions
priority: 1
schedule: once
---

## Goal

Close the Windows gap in M7 without pretending that Job Objects or restricted
tokens provide filesystem isolation. Choose and implement the supported Windows
boundary for `sandbox.enforce: fs | fs+net`, then prove on a Windows host that a
hostile child cannot escape it.

## Decision direction

The process provider stays fail-closed on Windows unless a native primitive can
enforce the same declared writable roots. AppContainer is the primary native
candidate because it can express filesystem and network capabilities; the
existing OCI provider also stays refused until Windows guest paths, authority
masks, and container mode have a concrete hostile-host proof. Restricted-token
or runtime-discovery claims are not accepted from API-level inference alone.

Accepted design: [`docs/scoping/m7-windows-containment.md`](../../docs/scoping/m7-windows-containment.md).

## Phase 1 — fail-closed proof

- Session resolution rejects both Windows process enforcement and the unproved
  Windows container mount contract before spawn.
- Engine-run validation/final/merge gates apply the same refusal.
- The Windows CI leg runs exact named session and gate tests before the full
  workspace suite.
- `enforce: off` is unchanged and remains explicitly unsandboxed.

This closes the unsafe inference gap; it does not close the availability ticket.

## Acceptance hints

- `provider: process` plus non-off enforcement refuses before any child starts
  until a proven native boundary exists.
- The selected provider supports `fs`; `fs+net` is either hard-offline or uses
  a non-bypassable egress boundary. Proxy environment variables alone do not
  count.
- Windows CI covers resolution, path/mount encoding, authority masks, and the
  fail-closed negative cases.
- A live Windows receipt runs a hostile brief that attempts an out-of-root
  write and network egress, records both denials, and verifies the allowed
  worktree change still passes its contract gate.
- No completion claim is made from cross-compilation or mocked Win32 calls.
