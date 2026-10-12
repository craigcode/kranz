---
state: open
title: Signal intake 7: first monitoring source adapter
priority: 2
schedule: once
blocked-by: [signal-intake-3-source-contract, signal-intake-6-injection-suite]
---

## Goal

Implement the first monitoring source adapter on the signal source contract, for the vendor the operator chose under D-C.

## Context

Design of record: `docs/scoping/signal-intake.md`. Read it in full before planning and do not contradict a decided or proposed section. Slice 7, decision D-C (Sentry issue alerts or New Relic alert webhooks).

Build against the vendor's current documentation for payload shape, signature header and algorithm, and replay protection, checked at the time of the work. Record the documentation version or date in the adapter's module doc.

## Scoping answers

- The adapter is domain-free: vendor format only, no project names in core.
- Receipt URL is per repo like the GitHub hook, mounted outside the token gate and authenticated by signature alone.
- Fingerprint is the vendor's issue or incident identity, not message text.

## Acceptance hints

- A real test alert from the vendor lands as a normalised, triaged signal.
- A payload with a bad or missing signature is rejected and leaves only a dropped record.
- Recorded vendor fixtures cover each event type the adapter accepts.
