---
title: Flight Rules plan review, mission coverage, drift, and waiver UI
priority: 2
schedule: once
blocked-by: [flight-rules-workflow-projection, flight-rules-enforcement-binding, flight-rules-waiver-decisions]
---

## Goal
Make the exact standards consent and outcome legible in the dashboard and
human report: applicable rules during plan review, live checker state during a
mission, merge-time drift, and narrow human waiver decisions.

## Context
KRZ-347; design D-G through D-I. The UI consumes typed API/event data and
must not infer policy from prompt or decision prose. This is the P2 operator
surface after the P1 CLI/evidence path is trustworthy.

## Acceptance hints
- Plan review groups applicable rules by RFC and shows ID/revision, status, level, statement, source, checker, waiver posture, and pack digest.
- Mission/report coverage distinguishes passed, failed, advisory, waived, not-evaluated, and not-applicable; status is never color-only.
- A policy-drift refusal compares approved/current rules and offers normal revalidation/reapproval rather than a bypass.
- Authorized waiver UI requires reason/expiry, repeats the exact rule and evidence, and disappears for non-waivable rules.
- Old/no-pack missions render without empty warning panels or API breakage.
- Dashboard gates: typecheck, tests, build, embedded sync/check, and lint.
- Anti-vacuity: unique test name/filter `flight_rules_dashboard_` cannot match a pre-existing test.
