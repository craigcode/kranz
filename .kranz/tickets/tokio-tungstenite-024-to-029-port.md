---
title: Port tokio-tungstenite 0.24 → 0.29 in the Slack socket client (dependabot PR #10)
priority: 3
schedule: once
---

## Goal
Port `crates/slack`'s hand-rolled socket client from tokio-tungstenite 0.24 to
0.29 and land the dependabot bump (PR #10, currently red on rust ubuntu +
windows + msrv + docker). Five major versions of API churn must be absorbed
in `bridge.rs` / `client.rs` connection machinery; the in-memory duplex
pump tests (`pump_connection` generic over Stream/Sink) are the port's
safety net and must stay green without being weakened.

## Context
Dependabot PR #10 (`tokio-tungstenite` 0.24.0 → 0.29.0) fails compilation
across all rust jobs (CI run 29658360088). The lockfile currently carries
two majors: slack pins 0.24 while server dev-deps already use 0.29 for WS
integration tests — the port unifies them. Read the 0.25–0.29 changelogs
before editing; known churn areas across those majors: `connect_async`
signature/config types, `Message` variants, handshake error types, and the
`WebSocketStream` API. Do NOT downgrade the bump or split the version
requirement — unify on 0.29.

## Acceptance hints
- `cargo build --workspace` and `cargo test --workspace` pass with
  tokio-tungstenite 0.29 as the only tungstenite in the lockfile.
- Slack pump/routing/health tests pass unchanged (fixtures intact).
- PR #10 merges after the port (close the loop with dependabot, or cherry-
  the manifest bump into the port branch and close #10).
- cargo clippy --workspace --all-targets -- -D warnings and fmt --check clean.
