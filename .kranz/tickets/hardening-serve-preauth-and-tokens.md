---
title: Stop kranz serve disclosing paths before authentication and tighten token handling
priority: 3
schedule: once
---

## Goal

Reduce what an unauthenticated caller can learn from `kranz serve`, and how its tokens are exposed on the host.

## Context

Reported by the 2026-11-10 code review as low-severity items and not yet addressed. Each item below is a report, not a confirmed defect: confirm it against current code before changing anything, and drop any that no longer apply.

- A request to a repository the host cannot mount answers with that repository's root path, before any token check.
- A configuration error is reported with a filesystem path before authentication.
- Serve tokens are printed to standard output at start and can be passed on the command line, where other local processes can read them.
- The actor recorded for a consent decision is taken from the request; it should be derived from the authenticated capability.
- The draft route checks ticket state and then acts in two steps, so two requests can both pass the check.
- Some request handlers do blocking file and git work on async worker threads.

## Scoping answers

- Fix each item on its own evidence; an item that cannot be reproduced is closed with a note, not patched speculatively.
- Contract files change additively only.
- Each fix carries a test that fails without it.

## Acceptance hints

- Pre-authentication responses carry no filesystem paths.
- Tokens are read from a file or the environment, and are shown once only when explicitly asked for.
- Two simultaneous draft requests for one ticket start one draft.
- Blocking work in the named handlers runs on the blocking pool.
