---
state: open
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

- Ledger at `.kranz/signals/ledger.jsonl`, raw bodies under `.kranz/signals/raw/<sha256>`; both gitignored runtime (add to `paths.rs` and `init.rs` ignores).
- Record kinds at minimum: received, dropped (unverified, no body), triaged, proposed, consented, linked, written-back.
- Raw bodies are written once and never parsed by the store.

## Acceptance hints

- A signal recorded by hand appears in the ledger and, after a restart, reads back with the same hash.
- Tampering with a ledger line or a raw body is detected on read (test).
- Two concurrent writers serialise through the lock (test).
