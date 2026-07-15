---
title: Use agent lifecycle hooks as non-authoritative status signals
priority: 2
schedule: once
---

## Goal
Add a hook-derived observability lane for supported CLI backends so kranz can
surface "running", "needs input", "interrupted", and "turn finished" signals
when the backend exposes them. These signals must help the dashboard, Slack,
and operator status views answer "whose move is it?" without becoming the
authoritative mission state. The event log, backend stream parser, reducer,
and validation outcomes remain the source of truth.

## Context
AgentSystemLabs Mission Control has the right pattern: install project-local
Claude/Codex/Cursor hooks, preserve user hooks, tag managed entries, map
`UserPromptSubmit`, `Stop`, `PermissionRequest`, and narrowed notification
events into task status, and fail soft when the local HTTP endpoint is down.

For kranz, the useful slice is not a terminal manager. It is a backend-neutral
status side channel for backends whose own stream format does not reliably say
when they are waiting on a human. This should be especially relevant to the
Cursor/ChatGPT CLI backend work and to Slack notifications that currently have
to infer too much from mission status alone.

The hook payload is untrusted input, even when it arrives over a local token.
Do not let hook-reported paths, transcript references, or status values mutate
mission state outside a narrow, typed event path.

## Acceptance hints
- A supported backend can opt into lifecycle hooks without clobbering existing
  project hooks; kranz-managed hook entries are replaceable and identifiable.
- Hook events append typed observability events, or update a derived status
  projection, without changing mission terminal state by themselves.
- Permission or approval prompts surface as "needs input" in dashboard and
  Slack quickly enough to be useful.
- Malformed hook payloads, stale task ids, and path traversal attempts are
  rejected or ignored with tests.
- Existing headless backends keep working with hooks disabled.
