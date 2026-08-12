---
state: open
title: Prove the persistent Kranz lifecycle on Railway
priority: 1
schedule: once
---

## Goal

Deploy a digest-pinned Railway host that takes one bounded disposable mission
from browser conversation through COMPLETE, persists across a restart, keeps
read and mutation authority separate, and can push only its `kranz/*` result
branch for human review.

## Context

The serve bind/auth primitives, scoped `kranz exec --push` path, and base
Docker image exist, but no cloud lifecycle has run. The base image is not a
mission-runner image: it lacks the Claude CLI, target toolchain, and Linux
bubblewrap. A real deployment also creates spend, a public endpoint, durable
external state, and credentials, so it cannot proceed until the operator
supplies the Railway/provider decisions listed in
[the readiness packet](../../docs/reviews/m4-m6-operator-readiness.md).

## Acceptance hints

- A derived image includes and pins Kranz, Claude CLI, bubblewrap, and the
  target repository toolchain; its digest is recorded.
- Railway HTTPS is the only public path to
  `serve --host 0.0.0.0 --insecure-lan --port "$PORT" --read-auth`.
- `/work` persists the Git checkout and `.kranz` runtime across restart.
- Tokenless access sees only health; read authority opens GET/WS but never
  POST; mutation authority is retained only by the operator.
- A low-budget mission reaches COMPLETE and emits a reviewable `kranz/*`
  branch; repository credentials reject main updates and force pushes at the
  provider layer.
- The receipt includes restart, auth-negative, containment, branch-scope,
  service/data-readiness, cost, and secret/log-scrub evidence.
