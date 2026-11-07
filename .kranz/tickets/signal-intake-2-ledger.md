---
state: done
title: Signal intake 2: repo-level signal ledger and raw store
priority: 2
schedule: once
---

## Goal

Add an append-only, repo-level signal ledger that records every intake decision and keeps each verified raw payload verbatim with its source, receipt time and SHA-256.

## Context

Design of record: `docs/scoping/signal-intake.md`. Read it in full before planning and do not contradict a decided or proposed section. Slice 2, decision D-A.

Mission event logs are per mission (`Event.mission_id` is required), so signals need their own store. Reuse the hash chain, authority-key MAC and single-writer lock primitives from `event_log.rs` for a new record type. Do not change `events.rs` or `types.rs`.

## Scoping answers

- Ledger at `.kranz/signals/ledger.jsonl` (serialised with single-file atomic append under `.kranz/signals/ledger.lock`), raw bodies under `.kranz/signals/raw/<sha256>`; both gitignored runtime (add to `paths.rs` and `init.rs` ignores). Enforce directory mode `0700` on `.kranz/signals` and `.kranz/signals/raw`, file mode `0600` on `ledger.jsonl` and raw files, and add `.kranz/signals/` to `sandbox::authority_read_deny_paths` so sandboxed workers, validators, and gates cannot read raw signal payloads.
- Apply `scrub::scrub_text` (and strip sensitive headers/cookies/DSNs) before persisting raw payloads to `.kranz/signals/raw/<sha256>`, enforce a 256 KiB per-payload cap before buffering, and enforce a per-source rate limit and bounded retention / total disk quota (e.g. max 1,000 raw files or 64 MiB, oldest-evicted or rejected when full).
- Record kinds at minimum: received, dropped (unverified, no body), triaged, proposed, consented, linked, written-back.
- Raw bodies are written once and never parsed by the store.

## Acceptance hints

- A signal recorded by hand appears in the ledger and, after a restart, reads back with the same hash; `.kranz/signals/` is mode `0700`, files are mode `0600`, and `.kranz/signals/` is present in `sandbox::authority_read_deny_paths`.
- Oversized payloads (> 256 KiB) and quota overflows are rejected cleanly, and secrets in raw payloads are scrubbed before hitting disk (test).
- Tampering with a ledger line or a raw body is detected on read (test).
- Two concurrent writers serialise through `.kranz/signals/ledger.lock` (test).
