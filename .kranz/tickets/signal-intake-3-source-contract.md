---
state: done
title: Signal intake 3: signal source contract, with GitHub as its first adapter
priority: 2
schedule: once
blocked-by: [signal-intake-2-ledger]
---

## Goal

Define an in-process `SignalSource` contract (verify, normalise, write back) in engine core, port the GitHub webhook trigger onto it without changing its behavior, and add a stub adapter that passes a conformance suite.

## Context

Design of record: `docs/scoping/signal-intake.md`. Read it in full before planning and do not contradict a decided or proposed section. Slice 3, decision D-B.

The GitHub trigger already verifies (`verify_signature`), normalises (`parse_trigger` to `Trigger`) and dedupes. The contract generalises those seams. Verification runs before any untrusted byte reaches the ledger, triage or a ticket.

## Scoping answers

- Unverified or replayed requests are dropped with a one-line decision log that has neither secret nor body; an unconfigured secret refuses everything, as today. Verification enforces a 256 KiB HTTP body limit before buffering, checks the timestamp header against a 5-minute skew window, and consults a bounded replay cache of `(source, delivery_id_or_sig_hash, timestamp)`.
- Every non-fenced normalised field is validated against strict character-class allowlists and length caps: `source` (`[a-z0-9_-]{1,32}`), `fingerprint` (`[a-zA-Z0-9._:-]{1,128}`), `severity` (`critical | high | medium | low | info`), `project` (`[a-zA-Z0-9._/-]{1,128}`, no `..`), `environment` (`[a-zA-Z0-9._-]{1,64}`), and `evidence_links` (parsed `https://` URLs only, host matching the configured vendor host allowlist, no whitespace/control chars/markdown syntax `[`, `]`, `(`, `)`, max 4 links × 512 chars). Any field failing validation is rejected or moved into the fenced untrusted block.
- Write back is part of the trait but may be a no-op for GitHub in this slice.

## Acceptance hints

- The existing GitHub webhook tests pass unchanged through the new contract.
- A stub adapter passes the conformance suite: bad signature rejected, stale timestamp or replayed delivery rejected, missing secret refuses, oversized body (> 256 KiB) refused before buffering, and normalised fields failing character-class allowlists or URL host rules are rejected or demoted into the untrusted fence.
- Verified GitHub receipts now also land in the ledger.
