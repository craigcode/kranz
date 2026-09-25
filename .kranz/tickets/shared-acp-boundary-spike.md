---
state-note: Implemented and independently reviewed in PR #82; merged at 7943324aa7accbf81661ae278cf7f42034f84be7. Out-of-mission delivery; dependency admission still follows docs/tickets.md.
state: done
title: Shared ACP boundary and threaded consumer spike
priority: 1
schedule: once
---

## Goal

Fix the protocol, authority and process ownership boundary before extracting the client, and prove a minimal second consumer without kranz-engine.

## Context

The operator approved this staged stream on 2026-09-24 after v0.4.1. See
[shared ACP scope and decisions D1–D4](../../docs/scoping/shared-acp-client.md).
The review-effort pilot is independent. Follow the dependency-admission rules
linked by the scope; ticket completion alone is not a recorded Complete mission.

## Scoping answers

- Inventory the ACP module and current Sgian source; record exact source revisions and who owns child startup, cancellation, hard stop, pipe closure and reap.
- Keep credentials, containment, durable consent and AgentEvent normalization in Kranz; expose raw protocol identities without shared process ownership.
- Build a provider-free second consumer on a standard thread with a small runtime. Shared fixtures must prove initialization, fragmented updates, permission cancellation, two prompt turns and bounded shutdown.

## Acceptance hints

- An isolated dependency graph contains no kranz-engine, cap-std or process-spawn dependency from kranz-acp.
- Review the ownership matrix against correctness, readability, architecture, security and performance; distinguish the source/runtime spike from actual Sgian daemon integration.
