---
title: Lessons — curated index
owner: mixed
freshness: check-on-touch
last_verified: 2026-09-25
verified_against:
  - .kranz/lessons/index.md
  - crates/engine/src/lessons.rs
  - AGENTS.md
  - docs/knowledge/decisions/inviolable-invariants.md
---

# Lessons

Rechecked 2026-09-25 against the shared ACP invariants update. The isolated
crate check supplements the full-workspace gate, and shared transport does not
change process ownership, permission authority or the lesson-capture channel.

`.kranz/lessons/` is the append-only, mission-capture memory: at most one
reusable lesson per mission, injected tightly-capped into planning seeds. It
stays the source of truth for hard-won operator scars — this file does **not**
absorb it. Per the [scoping decision](../scoping/repo-knowledge-store.md)
(D-F), the vault only *summarizes and links*.

## Where the lessons live

- Index: [`.kranz/lessons/index.md`](../../.kranz/lessons/index.md) — appended in
  capture order, oldest first. Planning injection selects the newest
  provenance-clean lessons first.
- One file per mission: `.kranz/lessons/<mission>.md`.
- Injected into planning via the engine's lessons channel, with its own byte
  budget, separate from the shipped ranked knowledge-vault block.

## Durable rules already promoted to standing docs

Some lessons recur enough that they are now standing rules, enforced in
[AGENTS.md](../../AGENTS.md) and the
[inviolable invariants](decisions/inviolable-invariants.md) note rather than
left to per-mission capture:

- **Full-workspace gate.** The final gate is `cargo test --workspace` (never a
  single `-p <crate>`), plus `clippy --workspace --all-targets -D warnings` and
  `fmt --all --check`. Run `cargo fmt` before finishing.
- **Bare exit codes.** Judge gate commands by their exit status; do not pipe the
  final gate command (piping can mask a non-zero exit).
- **Anti-vacuity.** A contract command that asserts test coverage must guard
  against zero matched tests:
  `... 2>&1 | grep -qE 'test result: ok\. [1-9]'`.
- **Contract-file additivity.** A revision may not weaken or drop an existing
  validation assertion; completed milestones are frozen.
- **Separate human review from validator inputs.** Human packets require the
  existing read capability and stay out of committed reports and fresh
  validator inputs. Decision controls still recheck identity and freshness.
- **kranz never pushes by default.** The explicit cloud handoff can push only
  a completed `kranz/*` branch; see the invariants note.
- **Own preflight helpers.** A host CLI timeout does not remove its daemon
  container. Bound the guest lifetime, delete only owned full IDs, and require
  confirmed absence; retain recovery intent when creation or cleanup is uncertain.
  See mount preflight ownership in the invariants note.
- **Name the actual isolation boundary.** Read-only cache mounts protect guest
  writes, not cache confidentiality or host-side mutation. Default-bridge
  relay access assumes trusted Docker peers; see the invariants note.
- **Sandbox mount ordering.** Restoring a writable private workspace must
  preserve covered Git/cache write protections and later authority masks;
  see the authority-material rule in the invariants note.

When a new lesson turns out to be a durable rule (not just a one-mission scar),
promote it into AGENTS.md / the invariants note and link it here — keep the
`.kranz/lessons/` entry as the origin record.
