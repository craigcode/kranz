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

- Signal proposal tickets always carry `schedule: once`, a `signal` frontmatter key with the ledger reference, and an explicit `signal-proposal: unconsented` marker (additive, in `KNOWN_FRONTMATTER_KEYS`) that causes `kranz work`, `kranz ticket queue`, and `Scheduler::find_due_tickets` to refuse queueing or running until an explicit human consent command (`kranz signal consent <id>` or `kranz draft --consent-signal` / authenticated operator UI action) records the `consented` ledger event and clears the unconsented marker.
- Title and goal are composed solely from strictly validated normalised fields (`source`, `fingerprint`, `severity`, `project`, `environment`, and host-allowlisted `https://` `evidence_links`); any field failing validation or free text appears only inside the nonced `<<<BEGIN KRANZ UNTRUSTED SIGNAL EVIDENCE [<nonce>]>>>` fence, and `Ticket::parse_sections` ignores `## ` headings inside the fence.
- Pre-consent labels and `queue` shortcuts never apply to signal tickets. Consent is recorded in the ledger with the actor.

## Acceptance hints

- No path from a received signal to a draft, queue or run without a recorded human consent event: `kranz work`, `kranz ticket queue`, and `Scheduler::find_due_tickets` all refuse unconsented signal proposals, asserted by a test that walks every entry point (REST, CLI, Slack, scheduler).
- Consenting and drafting a signal ticket through the explicit operator consent action works and records `consented` in the ledger.
- A signal whose free text or malformed fields contain instructions or `## Goal` / `## Review artifact` headings yields a ticket whose title, parsed `Goal`, and parsed sections do not contain them.
