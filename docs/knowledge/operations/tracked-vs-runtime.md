---
title: Tracked vs runtime files
owner: human
freshness: check-on-touch
last_verified: 2026-11-06
verified_against:
  - AGENTS.md
  - crates/engine/src/workspace_contract.rs
  - crates/engine/src/routing_rules.rs
  - crates/engine/src/ticket_notes.rs
  - crates/engine/src/domain_lint.rs
  - crates/engine/src/hook_status.rs
  - crates/engine/src/migrate_state.rs
---

# Tracked vs runtime

Moved verbatim from AGENTS.md on 2026-11-06.


Committed: `.kranz/merge-gates.json`, `.kranz/workspace.json` (workspace
contract, validated at draft/approve; `crates/engine/src/workspace_contract.rs`),
`.kranz/routing-rules.json` (routing rules, base-branch-owned, validated at
draft/approve; `crates/engine/src/routing_rules.rs`, docs/routing-rules.md),
`.kranz/tickets/<slug>.md`, `.kranz/tickets/<slug>.notes.jsonl` (append-only
discussion sidecar, D-BW-3; `crates/engine/src/ticket_notes.rs`),
`.kranz/domain-denylist.json` + `.kranz/domain-allowlist` (clean-room lint
policy — salted hashes and reviewed waiver fingerprints only, never readable
terms; `crates/engine/src/domain_lint.rs`, docs/domain-lint.md), and each
mission's `plan.md` / `plan.json` / `report.md` (on the mission branch). Gitignored runtime
(never commit): `events.jsonl`, `state.json`, `runs/`, `control/`,
`missions/*/workspace/` (container-provider compose files),
`.kranz/config.json`, `serve.token`, `serve.read.token`,
`.kranz/missions/*/enqueue-source*.json` (external-producer ownership and
returned receipts),
`.kranz/domain-terms.local` (plaintext lint vocabulary), `.kranz/tickets/*.status`,
`.kranz/hook-status/` (ephemeral hook-signal projection — registrations +
latest signal per run; `crates/engine/src/hook_status.rs`).
Ticket lifecycle state is committed in the ticket .md itself: the additive
`state:` frontmatter key (`open` default; terminal `done`/`superseded`/
`wontfix`, optional `state-note:`) is the single source of truth — the
gitignored `.status` sidecar is a write-through cache of it (frontmatter
wins on conflict, logged; absent key = sidecar governs as before). Reads
resolve via `Ticket::read_state`; lifecycle writes go through
`Ticket::write_lifecycle` (both files); `kranz ticket migrate-state`
(dry-run, `--yes` to apply) folds existing terminal sidecars into
frontmatter, skipping git-dirty tickets by name
(`crates/engine/src/migrate_state.rs`).
