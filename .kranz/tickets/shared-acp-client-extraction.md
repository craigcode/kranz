---
state-note: Implemented and independently reviewed in PR #82; merged at 7943324aa7accbf81661ae278cf7f42034f84be7. Out-of-mission delivery; dependency admission still follows docs/tickets.md.
state: done
title: Extract the ACP client without widening worker capabilities
priority: 1
schedule: once
blocked-by: [shared-acp-boundary-spike]
---

## Goal

Move bounded wire framing and session protocol into kranz-acp, retaining mission-specific normalization, permissions and process supervision in the engine.

## Context

The operator approved this staged stream on 2026-09-24 after v0.4.1. See
[shared ACP scope and decisions D1–D4](../../docs/scoping/shared-acp-client.md).
The review-effort pilot is independent. Follow the dependency-admission rules
linked by the scope; ticket completion alone is not a recorded Complete mission.

## Scoping answers

- Preserve current JSON-RPC handling, peer identity, exact raw permission arguments, event ordering, stop-reason handling and unknown/missing cost semantics.
- Keep filesystem and terminal capability false and resume refused; do not modify profile admission, credential channels, process cleanup or permission decisions.
- Run shared conformance fixtures through both consumers; retain the existing worker malformed/fragmented input, output-limit, permission, cancellation, death and report checks.
- Include package license/version/notice checks and bottom-up publishing order for the added crate; this change does not publish or tag a release.

## Acceptance hints

- Run workspace tests, clippy with warnings denied, formatting and build; run strict docs and shared-crate tests in isolation.
- Review the extraction diff for ownership and behavior changes and record exact validation and any unexercised platforms.
