---
state: open
title: Signal intake 5: undrafted proposals behind human consent
priority: 2
schedule: once
blocked-by: [signal-intake-1-untrusted-fence, signal-intake-3-source-contract, signal-intake-4-triage]
---

## Goal

Turn a proposed signal into an undrafted ticket with `signal:` provenance, a goal composed from normalised fields, and fenced evidence. Only a human Draft action moves it forward, and a test proves no other path exists.

## Context

Design of record: `docs/scoping/signal-intake.md`. Read it in full before planning and do not contradict a decided or proposed section. Slice 5, invariant 1.

The GitHub trigger drafts on receipt because an allow-listed user's labelled comment is the consent (D-D keeps that unless the operator changes it). A monitoring signal has no such actor. Drafting spends, so the proposal must stop before it.

## Scoping answers

- A new `signal` frontmatter key (additive, in `KNOWN_FRONTMATTER_KEYS`) carries the ledger reference.
- Title and goal are composed from source, fingerprint, severity and links; free text appears only inside the fence.
- Pre-consent labels and `queue` shortcuts never apply to signal tickets. Consent is recorded in the ledger with the actor.

## Acceptance hints

- No path from a received signal to a draft, queue or run without a recorded human actor, asserted by a test that walks every entry point (REST, CLI, Slack, scheduler).
- Drafting a signal ticket through the existing Draft action works and records consent.
- A signal whose free text contains instructions yields a ticket whose title and goal do not contain them.
