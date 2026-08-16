---
state: open
state-note: Accepted plan plus phase-1 refusal and phase-2 native capability probe implemented: Windows process/container enforcement remains fail-closed; Windows CI records the real OS build and System32-only processmodel.dll/export result, then runs exact non-vacuous session/gate refusal tests. Native AppContainer launch and hostile Windows 11 receipt remain open.
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

## Phase 2 — native capability probe

- `kranz sandbox-probe [--json]` records the Windows version and checks
  `processmodel.dll` for Microsoft's experimental process-sandbox export.
- The DLL is loaded from System32 only. The probe neither calls the export nor
  creates a profile, changes an ACL, or starts a child.
- API presence is reported as capability evidence and never enables production
  containment or relaxes the phase-1 fail-closed behavior.
- The Windows CI leg prints the real-host report before running exact,
  anti-vacuity-checked refusal tests.

This closes the discovery step in the accepted delivery sequence; it does not
prove a stable serialization contract, containment, or availability.

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
