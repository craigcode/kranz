---
state: done
state-note: mission m-84e7ea complete, merged 2026-07-05 — sidecar restored after tracked-on-branch/ignored-on-main checkout crossfire deleted it
title: Persist the serve mutation token to a 0600 file
priority: 2
schedule: once
---

## Goal
kranz serve should write its mutation token to .kranz/serve.token (mode 0600, removed on clean shutdown) in addition to printing it. CLI commands that need the token (kranz release, future start/stop) read it from there automatically when targeting a local repo, ending the stdout-scrollback scavenger hunt. Also accept bodyless POSTs on mutation endpoints (the '{}' Content-Type dance).

## Context
Token today: generated in crates/cli/src/commands.rs (~line 1116), printed once to stdout, held in memory. kranz release (crates/cli) demands --token/$KRANZ_TOKEN. Write to .kranz/serve.token mode 0600 on startup, remove on clean shutdown; document that filesystem access == authority (same trust boundary as .kranz itself). Local CLI commands targeting the same repo read it automatically, with the env var/flag still winning. Bodyless-POST acceptance: the axum JSON extractor currently 415s on missing Content-Type — accept empty bodies on mutation routes that take no payload (start, release, abandon).

## Scoping answers

## Acceptance hints
- kranz serve writes .kranz/serve.token with mode 0600 (asserted in a test) and removes it on graceful shutdown.
- kranz release --mission <id> works with no --token/$KRANZ_TOKEN when the file exists; env/flag override the file.
- curl -X POST .../start with no body and no Content-Type succeeds (integration test).
- cargo test --workspace passes, piped through grep -qE 'result: ok\. [1-9][0-9]* passed' for the new suites.
