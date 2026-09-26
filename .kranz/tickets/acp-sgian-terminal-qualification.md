---
state: open
title: Qualify Sgian panes over the contained terminal provider
priority: 3
schedule: once
blocked-by: [acp-resource-budget-qualification, acp-contained-terminal-provider]
---

## Goal

Present contained ACP terminal output in Sgian panes while preserving Kranz authority and owned cleanup.

## Context

The operator approved this staged stream on 2026-09-24 after v0.4.1. See
[shared ACP scope and decisions D1–D4](../../docs/scoping/shared-acp-client.md).
The review-effort pilot is independent. Follow the dependency-admission rules
linked by the scope; ticket completion alone is not a recorded Complete mission.

## Scoping answers

- Coordinate with Sgian source owners using a separate consumer change; do not make kranz-engine a Sgian dependency or implement desk UI in Kranz.
- Authenticate the provider connection narrowly and bind run, session, terminal and generation. A pane lease does not bypass Kranz consent.
- Define output retention, delayed exit status, release, daemon death, restart recovery and cleanup acknowledgement; preserve evidence when cleanup is unconfirmed.
- Observation and exact permission answers use authenticated Kranz controls first. Prompt ownership transfer and keyboard input remain separate design work.
- The first integration must execute inside the admitted namespace. Any host backend is a separate explicitly qualified profile, never a fallback.

## Acceptance hints

- Both consumer changes run common conformance fixtures and adversarial daemon/provider lifecycle tests.
- Prove visible pane output without changing credentials, egress or filesystem containment and document residual supervisor/platform limitations.
