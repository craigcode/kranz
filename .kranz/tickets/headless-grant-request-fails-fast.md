---
state: done
state-note: Config grantRequestTimeoutSecs (absent keeps one hour); kranz exec defaults to 300s with --grant-wait; one stderr line per request names the wait and the answering command; the denial reason states the wait. Outcome is still deny-default.
title: Deny a grant request quickly when no operator can answer it
priority: 2
schedule: once
---

## Goal

A mission that parks on a grant request does not wait an hour when it was started in a way that gives no operator a chance to answer. It denies by default after a short, configurable wait and says why.

## Context

Reported from a mission run on 2026-11-15. A fix worker raised a touch-path grant request. The mission had been started with `kranz run` with no terminal attached and no `kranz serve` host. It waited the full default before recording `grant.denied` with "grant request timed out with no operator decision (deny-default)". That hour was most of the mission's wall-clock time.

The default is `DEFAULT_GRANT_REQUEST_TIMEOUT` in `crates/engine/src/orchestrator.rs`, 3600 seconds, changeable only by tests through `set_grant_request_timeout`.

A grant can still be answered from another terminal with `kranz grant approve`, so "no terminal" does not prove that nobody is there.

## Scoping answers

- Make the wait configurable in mission config, with the current hour as the default for interactive and served runs.
- `kranz exec` is headless by definition. Give it a short default and a flag to change it.
- When the wait starts, print one line to stderr that names the request, the deadline and the exact command that answers it.
- The outcome stays deny-default. This ticket never approves anything automatically and does not change what a grant covers.
- Config changes are additive, with a default that preserves today's behaviour for existing configs.

## Acceptance hints

- A mission test with the wait set to a few milliseconds ends with the milestone blocked and a `grant.denied` reason that states the configured wait.
- `kranz exec` with no flag uses the short default; the test asserts the value, not the elapsed time.
- An existing config with no new key behaves as it does today.
- The stderr line appears once per request and names the answering command.
