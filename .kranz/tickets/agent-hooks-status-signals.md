---
title: Use agent lifecycle hooks as non-authoritative status signals
priority: 3
schedule: once
blocked-by: [cursor-cli-live-capture-route-decision]
---

## Goal
Add an optional hook-derived observability lane for CLI backends that expose
lifecycle hooks, so dashboard/Slack can surface "running", "needs input",
"interrupted", and "turn finished" when the backend stream is silent.
Hooks must never become authoritative mission state.

## Context
Useful mainly for Cursor/ChatGPT CLI backends (and similar interactive CLIs).
Headless Claude path already owns status via the engine event fold — do not
require hooks there.

Blocked on the Cursor backend route decision / implementation lane so this
does not ship as an orphan hook installer with no consumer.

## Persistence choice (accepted)
Prefer an **ephemeral derived projection** keyed by `run_id` / mission id
(in-memory or gitignored runtime), updated from hook POSTs.
Do **not** add reducer-driving EventKinds for hook status.
If durable audit of hook receipts is later required, that is a separate
additive observability event that the reducer ignores for transitions.

## Install / hygiene rules (non-negotiable)
- Do **not** rewrite tracked project hook files on the primary checkout as a
  side effect of `kranz serve`.
- Prefer env-injected / worktree-local / `~/.kranz/` managed hook dirs for
  headless runs.
- If a repo-local install is ever offered, it must be an explicit operator
  action that produces a reviewable diff — not silent mutation.
- Hook payloads are untrusted even on loopback+token: reject path traversal,
  stale ids, oversized bodies; never let hooks emit FeatureFailed / Blocked /
  Complete / grant mutations.

## Acceptance hints
- Opt-in per backend; headless backends work with hooks disabled.
- "Needs input" appears in dashboard/Slack from the projection without
  changing folded mission terminal state.
- Malformed payloads ignored with tests.
- No test or code path writes hooks into the primary tracked tree implicitly.
- Anti-vacuity grep on the named filter.
