---
title: Tighten how the dashboard binds and presents pending decisions
priority: 3
schedule: once
---

## Goal

Make each decision the dashboard sends name exactly what was shown, and keep the panels stable while a decision is pending.

## Context

Reported by the 2026-11-10 code review as low-severity items and not yet addressed. Each item below is a report, not a confirmed defect: confirm it against current code before changing anything, and drop any that no longer apply.

- A grant decision carries the target string only. It should also carry the grant kind and the request's sequence number, so a decision cannot land on a later request with the same text. The Slack grant buttons have the same shape.
- Draft text typed into a panel (a revision request, an answer) survives switching to another mission and can be submitted there.
- Board polling requests queue up behind the token gate when the token is missing or rejected, instead of stopping.
- When a permission request expires, the layout shifts under the cursor, so a click aimed at one control can land on another.

## Scoping answers

- Fix each item on its own evidence; an item that cannot be reproduced is closed with a note, not patched speculatively.
- Contract files change additively only.
- Each fix carries a test that fails without it.

## Acceptance hints

- A grant decision for a superseded request is refused, on the dashboard and in Slack.
- Switching missions clears panel drafts.
- Polling stops, with a visible state, after an authentication failure.
- Expiry does not move the remaining controls.
