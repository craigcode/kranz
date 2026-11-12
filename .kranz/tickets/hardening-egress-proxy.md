---
title: Bound and authenticate the filtering egress proxy
priority: 3
schedule: once
---

## Goal

Finish hardening the `fs+net` egress proxy.

## Context

Reported by the 2026-11-10 code review as low-severity items and not yet addressed. Each item below is a report, not a confirmed defect: confirm it against current code before changing anything, and drop any that no longer apply.

- The list of recorded denials grows without bound for the life of a session.
- Open since the 2026-09-01 review: under Seatbelt the proxy accepts any local client without authentication, and the `fs+net` profile allows all of loopback rather than the proxy's port alone.

## Scoping answers

- The loopback restriction must be probed with `sandbox-exec`, since Seatbelt rejects some rule shapes; record what was probed.
- Each fix carries a test that fails without it.

## Acceptance hints

- Denials are capped, with an omitted count, as the event already allows.
- A local process outside the session cannot use the session's proxy.
- The profile allows the proxy's address and port only; a probe test shows another loopback port is refused.
