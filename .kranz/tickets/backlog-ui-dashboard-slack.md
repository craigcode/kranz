---
state: done
title: Backlog surfaces: dashboard panel and Slack ticket verbs
priority: 2
schedule: once
blocked-by: [backlog-host-draft-deps]
---

## Goal
Give the web dashboard a backlog panel (ticket list with states, ticket detail, Draft with live progress, Approve on REVIEW tickets, blocked-by shown and enforced in the UI) and give Slack /kranz ticket list, /kranz ticket show <slug>, /kranz draft <slug> (spend-gated), and approve-by-slug — all over the REST/host surface from the prerequisite ticket.

## Context
Rides entirely on the REST/host surface from backlog-host-draft-deps (see
that ticket; this one is blocked-by it and must not begin until it lands).
DASHBOARD (apps/dashboard): a backlog panel reachable from the picker — list
(slug, priority, state chip, title, blocked-by badges), ticket detail view
rendering the markdown sections incl. needs-context questions, a Draft button
that fires POST draft and shows live progress via the existing SSE feed
(pattern: the planning chat pane), an Approve button on REVIEW tickets that
disables itself with the blocker named when blocked-by is unsatisfied
(server refusal stays authoritative — the UI mirror is a courtesy). Reuse the
management-row button styling; npx tsc --noEmit and npm run build must pass.
SLACK (crates/slack): verbs `/kranz ticket list` and `/kranz ticket show
<slug>` (read-only, no allowlist gate, reply ephemeral), `/kranz draft <slug>`
(SPEND — allowlist-gated exactly like /kranz new, slow-action spawned off the
socket loop, immediate hourglass ack, NEEDS-CONTEXT questions posted back to
the invoker), and `/kranz approve <slug>` extended to accept a ticket slug
(resolves to the drafted mission; blocked-by refusals surface the server's
message verbatim). Slash-command platform rules apply: single-line commands,
no dispatch in threads. Update docs/tickets.md's listing/Slack sections and
docs/slack-management.md's slice ledger.

## Scoping answers

## Acceptance hints
- Dashboard: backlog panel lists the live .kranz/tickets/ state; drafting a ticket from the browser parks a reviewable plan without a terminal; Approve on a blocked ticket is disabled showing the blocker, and the server refusal path is covered by a handler test.
- Slack: /kranz ticket list and show reply ephemerally; /kranz draft is allowlist-gated (unauthorized users get the standard refusal, and no engine spawns); approve-by-slug queues a REVIEW ticket end-to-end in the fake-host tests.
- npx tsc --noEmit and npm run build pass in apps/dashboard; cargo test --workspace passes piped through grep -qE 'result: ok\. [1-9][0-9]* passed'; clippy and fmt clean.
