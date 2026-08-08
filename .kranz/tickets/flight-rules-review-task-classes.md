---
title: Reuse Flight Rules for spec and incident review task classes
priority: 3
schedule: once
blocked-by: [flight-rules-workflow-projection, flight-rules-enforcement-binding, flight-rules-dashboard-report]
---

## Goal
Apply the same standards resolver, lifecycle, checker, and evidence contracts
to explicit spec-review and incident-review artifacts without adding another
standards corpus or turning Kranz into a document/incident management system.

## Context
KRZ-349; design D-G/D-K. Cloudflare's strongest workflow lesson is that one
governed corpus informs code, spec, and incident review. Kranz should reuse
task classes and packs: an input artifact is reviewed, findings cite stable
rule revisions, and the review output/evidence is the deliverable. Integrations
with external spec stores or incident trackers stay outside core.

## Acceptance hints
- Add explicit `spec-review` and `incident-review` consumers, or one generic review-artifact consumer with those task classes, using the pinned resolver and stages.
- A synthetic spec rule is invisible to implementation review and selected for spec review; the inverse holds for an incident-only rule.
- Findings, waivers, checker outcomes, replay, and evidence bundles use the same types as code missions—no parallel review result schema.
- The run delivers a review artifact and honest outcome; it neither mutates the source artifact nor passes vacuously on an empty deliverable.
- No Jira/Linear/PagerDuty/Google Docs client, hosted standards service, or general semantic-search subsystem enters core.
- Anti-vacuity: unique filter `flight_rules_review_class_` reports a nonzero pass count.
