---
title: Harden Slack bridge routing, escaping and endpoints
priority: 3
schedule: once
---

## Goal

Tidy the remaining places where the Slack bridge routes, renders or posts on weaker evidence than the rest of the bridge uses.

## Context

Reported by the 2026-11-10 code review as low-severity items and not yet addressed. Each item below is a report, not a confirmed defect: confirm it against current code before changing anything, and drop any that no longer apply.

- Buttons on an ephemeral `/kranz repo:<id> todo` reply are routed by channel, not by the repository the reply was for.
- Six handlers interpolate engine or host error text into messages without the escaping used elsewhere.
- Fields of a mission report rendered into a card can contain line breaks that read as separate sections.
- The websocket URL and the `response_url` a payload supplies are used without checking they are Slack hosts.
- A reply sent to a mission that has reached a terminal state is dropped with no notice to the sender.
- Each reply from an unauthorized user produces a public bot message.
- A signal proposal card is posted again after a posting error.

## Scoping answers

- Fix each item on its own evidence; an item that cannot be reproduced is closed with a note, not patched speculatively.
- Contract files change additively only.
- Each fix carries a test that fails without it.

## Acceptance hints

- A button on a repository-scoped reply acts on that repository.
- Every interpolated error passes through the escape helper; a test enumerates the handlers.
- Report fields render on one line each.
- Non-Slack hosts are refused for both URLs.
- Terminal-mission replies and unauthorized replies get one quiet, non-public response.
- A proposal card posts at most once per proposal.
