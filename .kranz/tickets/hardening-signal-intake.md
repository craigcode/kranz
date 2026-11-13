---
title: Authenticate replay protection and pin configuration for signal intake
priority: 3
schedule: once
---

## Goal

Close two gaps in the production-signal intake path before write-back is wired to it.

## Context

Reported by the 2026-11-10 code review as low-severity items and not yet addressed. Each item below is a report, not a confirmed defect: confirm it against current code before changing anything, and drop any that no longer apply.

- The replay key and timestamp for a Sentry delivery are read from headers that the signature does not cover.
- A repository's own config file can replace the `signals.sentry` block. That is harmless today and would let a repository redirect or read environment-derived settings once write-back exists.

## Scoping answers

- Fix each item on its own evidence; an item that cannot be reproduced is closed with a note, not patched speculatively.
- Contract files change additively only.
- Each fix carries a test that fails without it.
- Operator decision, 2026-11-13: a delivery that was accepted once stays refused as a replay for at least 24 hours after first receipt, measured on the server's clock and bounded by the existing cache capacity. The 300 second timestamp window remains a freshness check only and does not set how long a replay is remembered. Document that a restart or a full cache can shorten the window.

## Acceptance hints

- Replay protection uses only values the signature covers.
- The parts of `signals.sentry` that name an endpoint or a credential source are settable from the global config layer only, with a test alongside the other project-layer refusals.
