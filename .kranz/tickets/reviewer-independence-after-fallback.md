---
state: done
state-note: Delivered in public v0.4.0; approved policy, actual model provenance, fallback and replay regressions verified. See docs/reviews/2026-09-22-post-v040-maintenance.md. Reconciled from released code; no Complete mission record fabricated.
title: Preserve approved reviewer independence after backend fallback
priority: 2
schedule: once
---

## Goal

An optional, operator-configured requirement makes scrutiny and/or functional
review use a different model family from every recorded worker attempt in the
mission. Pin the requirement in the approved plan; backend changes, fallback,
retries, confirmation sessions and restart must not weaken it. A different CLI
alone is not evidence of independence. Unknown provenance fails closed.

## Scope and contractChangeRequest

Approved by the operator on 2026-09-07 as a governance gate within the
positioning ADR. At that approval, the repository was private and publication
was disabled. Subsequent public releases supersede that historical posture;
[the release procedure](../../docs/releasing.md) governs current publication.
This remains a governance feature, not a routing or code-generation feature.

Add optional reviewerIndependence fields to mission config, plan and mission,
and optional resolved backend provenance to worker.spawned and WorkerRun.
Absent fields preserve old logs/configs. The engine owns the approval pin;
revisions cannot replace it. Requirements are seed-time configuration and
cannot be relaxed mid-mission. No automatic exception or containment downgrade.

## Acceptance

- Different CLIs serving the same family are refused; a known different
  family passes. Unknown/automatic model selection does not establish identity.
- Unavailable reviewer and worker backend fallbacks are checked against the
  effective model, not the requested backend. All retries and local PASS
  confirmations use the same gate before launching.
- Recorded worker attempts, including repairs, parallel and pool runs, govern
  the comparison even after config changes and replay; missing provenance or
  a skipped required reviewer blocks with a durable explanation.
- Approved policy survives plan revision and config changes. Default missions
  retain existing fallback behavior. Workspace tests, clippy, fmt and build pass.
