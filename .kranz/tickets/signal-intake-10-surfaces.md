---
state: done
title: Signal intake 10: dashboard and Slack surfaces
priority: 3
schedule: once
blocked-by: [signal-intake-5-proposal, signal-intake-8-linkage]
---

## Goal

Show signals in the dashboard (received, parked, not a code change, proposed, consented, closed) and notify Slack when a proposal is waiting, linking to the existing Draft action.

## Context

Design of record: `docs/scoping/signal-intake.md`. Read it in full before planning and do not contradict a decided or proposed section. Slice 10, stretch.

The pipeline view's lenses are filtered views of one data source (`docs/scoping/pipeline-view.md`); a signal proposal is a ticket and already appears there. This slice adds the pre-ticket states and the notification.

## Scoping answers

- Read-only views over the ledger through a new GET endpoint behind read authority; no new mutation route.
- Slack uses the existing Draft action and spend gate (`allow_users`); no new consent path.

## Acceptance hints

- The dashboard lists signals by triage outcome with their ledger evidence.
- A waiting proposal posts once to the repo's Slack channel and can be drafted from there by an allowed user.
- Tokenless and read-only requests cannot reach any signal mutation.
