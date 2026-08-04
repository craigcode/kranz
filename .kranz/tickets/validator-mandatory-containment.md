---
title: "Mandatory containment for validator sessions — snapshot alone is not isolation"
priority: 1
schedule: once
---

# Mandatory containment for validator sessions

Source: 13th-pass review (2026-08-03, P1) — `orchestrator.rs` ~4516. The
validator runs in a disposable worktree, but that worktree sits underneath
the real repository hierarchy and references the shared Git directory. With
the default `enforce: off`, hostile validator code (prompt-injected or
model-authored) can walk to the real checkout, modify tests, run them, and
restore the bytes — evading the after-fingerprint (modify → use → restore
leaves no drift). The snapshot is physical separation, not containment.

## Problem

The validator is the adversarial reader the whole gate rests on; its
isolation must not depend on the operator opting into enforcement. Today
`enforce: off` (the default) leaves validator sessions with the engine's
full filesystem authority.

## Design (direction)

Validators get MANDATORY containment that exposes only the snapshot,
regardless of `sandbox.enforce`: a validator-profile Seatbelt/bwrap wrap
(always on where the platform supports it) with the snapshot worktree as
the sole writable root, the real checkout and shared `.git` read-denied
(except the narrow refs the fold needs), and mission metadata
write-denied exactly as the gate wrap does. Where the platform cannot
contain (Windows, no bwrap), the readiness gate must say so loudly —
fail-closed is a product decision to make deliberately here, not
silently.

Open work named up front:

- The spawn-path wiring across all four backends (validator sessions go
  through backend spawn, not command_exec — the wrap composes with each
  backend's own sandbox posture).
- The read-only surface the functional validator legitimately needs
  (contract commands in the snapshot, git log/diff against shared refs).
- Whether fail-closed or degrade-with-alarm on uncontainable platforms —
  an operator decision, recorded.

## Test gate

- A validator-session fixture (enforce: off) provably cannot read the real
  checkout or the shared git dir, cannot write outside the snapshot, and
  still completes a validation round.
- The after-fingerprint remains as defense-in-depth, not the only net.
- Workspace gates green, bare exit codes, never piped.

## Out of scope

Engine-run gate wrapping (landed: engine-gates-sandbox-wrapped), the
validator prompt/content rules, the container tier.
