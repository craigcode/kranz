---
state: done
title: Probe Cursor CLI / Grok 4.5 as a kranz AgentBackend
priority: 2
schedule: once
---

## Goal

Run the Cursor CLI backend probe and decide whether `backend_cursor` should be
implemented as a direct `agent --print --output-format stream-json` parser, an
ACP-backed adapter, or deferred until Cursor's headless auth/model surface is
usable enough.

## Context

Cursor now overlaps unattended agent work, but kranz should absorb it as a
runtime rather than chase the IDE lane. The probe opened on 2026-07-09 found
the local CLI installed (`agent` version `2026.04.13-a9d7fb5`) and confirmed
the relevant flags (`--print`, `--output-format json|stream-json`, `--model`,
`--workspace`, `--worktree`, `--sandbox`, `--force`). Live prompt capture is
currently blocked because headless commands report `Authentication required`
even though `agent status` partially succeeds; `agent models` reports no
models available for the account.

Design note: do NOT build the full backend in this ticket. Produce evidence,
fixtures if auth works, and a go/no-go recommendation. See
`docs/scoping/cursor-cli-backend.md`.

## Acceptance hints

- Auth/model preflight is understood: document whether `agent login` or
  `CURSOR_API_KEY` is required, and how failures present.
- Capture and commit a small output fixture only if a live prompt succeeds
  without secrets; otherwise document the exact blocker and leave no fixture.
- Test default, Grok 4.5, and invalid model selection; record the actual CLI
  model id for Grok 4.5 if available.
- Decide direct parser vs ACP route based on observed structure, not guesswork.
- End with a scoped backend implementation brief if the probe is green.
