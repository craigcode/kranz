---
title: Positioning — adopt "the few seconds between action and trust" as the consent-gate story
priority: 4
schedule: once
---

## Goal
Update kranz's positioning language (README intro, docs/what-is-kranz.md,
dashboard/about surfaces where present) around the articulation that the
hard product decision is "the few seconds between an action and the moment
someone trusts it" — the consent surface (plan approval, grants, merge) is
kranz's answer to that interval, and grant latency (flight surgeon console)
is how we measure it. Keep the honest-register: measured, no hype.

## Context
AI Tinkerers SF GTM review (2026-07) ended on the line that agents get more
useful when validation, context, and approval happen inside the workflow,
and that the hardest decision is the seconds between action and trust.
That is kranz's architecture verbatim (contract gates, grants, merge gate)
and the property the Factory AMM whitepaper can't score. Current README
opens with "Git-native mission control for headless coding agents" — accurate
but quieter than the thesis. This ticket is wordsmithing with a thesis:
position kranz as the system that owns the action→trust interval, and link
the grant-latency metric as the measurement.

## Acceptance hints
- README intro and docs/what-is-kranz.md carry the interval framing in one
  or two sentences each, in the repo's existing voice (dry, precise).
- The flight-surgeon console ticket is cross-referenced as the measurement
  of that interval (grant latency buckets).
- No hype adjectives; a docs reviewer would not flag it as marketing.
