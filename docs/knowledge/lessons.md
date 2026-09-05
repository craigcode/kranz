---
title: Lessons — curated index
owner: mixed
freshness: check-on-touch
last_verified: 2026-09-05
verified_against:
  - .kranz/lessons/index.md
  - AGENTS.md
  - docs/knowledge/decisions/inviolable-invariants.md
---

# Lessons

`.kranz/lessons/` is the append-only, mission-capture memory: at most one
reusable lesson per mission, injected tightly-capped into planning seeds. It
stays the source of truth for hard-won operator scars — this file does **not**
absorb it. Per the [scoping decision](../scoping/repo-knowledge-store.md)
(D-F), the vault only *summarizes and links*.

## Where the lessons live

- Index: [`.kranz/lessons/index.md`](../../.kranz/lessons/index.md) — newest-first.
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
- **kranz never pushes by default.** The explicit cloud handoff can push only
  a completed `kranz/*` branch; see the invariants note.

When a new lesson turns out to be a durable rule (not just a one-mission scar),
promote it into AGENTS.md / the invariants note and link it here — keep the
`.kranz/lessons/` entry as the origin record.
