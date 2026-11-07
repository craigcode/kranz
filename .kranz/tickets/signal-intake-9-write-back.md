---
state: done
title: Signal intake 9: idempotent write-back to the source
priority: 2
schedule: once
blocked-by: [signal-intake-7-first-source]
---

## Goal

Write the outcome of a signal's proposal and mission back to the source issue once: proposal created, completed, merged, failed or declined, with status and links only.

## Context

Design of record: `docs/scoping/signal-intake.md`. Read it in full before planning and do not contradict a decided or proposed section. Slice 9, decision D-G.

Precedent: the `EnqueueSource { producer, external_ref }` join (`queue.rs`) and the Gas City pack's pending receipt plus rename to `enqueue-source.returned.json` after a confirmed write (`packaging/gascity/`). Reuse the join and receipt discipline; never the `kranz exec --enqueue` auto-approval.

## Scoping answers

- Each write is preceded by a pending ledger record and followed by a confirmed one; resume retries pending writes only.
- Outbound egress to the vendor API is explicit configuration; credentials come from the sanctioned secret channel. Write-back URLs are constructed strictly from the operator-configured base URL plus a validated `[a-zA-Z0-9_-]` external issue ID (never following a raw URL from the incoming payload), using `reqwest::redirect::Policy::none()` and `.no_proxy()`, and validating the destination host against the configured vendor host.
- No diff, report or transcript text is posted. A failure or decline never resolves the source issue.

## Acceptance hints

- A completed mission posts once; a failed or declined one posts without resolving the issue.
- Payload-supplied callback URLs, external IDs failing `[a-zA-Z0-9_-]`, host mismatches, and HTTP redirects (`3xx`) are refused by the write-back client (test).
- Killing the process between the vendor write and the confirmation, then resuming, does not double-post (test against a fake vendor).
- Write-back errors are recorded and retried without blocking the mission.
