# Review: Matt Pocock's skills flow vs kranz's pipeline (2026-07-24)

Verdicts on the four ideas from `review-mattpocock-skills-flow.md`, checked
against the current pipeline (validator-repair era). One line each, then
the follow-ups.

## 1. Context-window-sized work unit as the scheduling primitive — **ADOPT (partial)**

Kranz already sizes work by context, not effort: the *feature* is the atom,
and every feature runs in a **fresh worker session** with an explicit turn
budget — that IS "fits in one context window," enforced by construction
rather than by the author's feel. What his framing adds that we lack: a
plan-time *fit check*. Nothing today warns when a feature spec is shaped
like three sessions of work, so the split happens late (respawns, partial
results) instead of at planning. Follow-up ticket filed:
`plan-feature-context-fit-check`.

## 2. Compress-to-artifact before clearing — **ADOPTED ALREADY (validated)**

Every session boundary in the pipeline already has an explicit compression
artifact: draft → plan.md (+ research.md); run → per-feature WorkerReport
(structured, not transcript); validate → contract + criteria (task text,
not worker summaries); close → report.md (+ lessons). The one boundary
that deliberately does NOT compress — validators see the raw diff, never
the worker's account of it — is a feature ("do not trust summaries"), not
a gap. No change; this review is the record of the audit.

## 3. Fresh-context review on two axes — **ADOPTED ALREADY (validated)**

His argument — an agent reviewing code it just wrote grades its own
homework — is why kranz's validators are fresh sessions by construction,
not a persona of the worker's session. Both of his axes exist: the spec
axis (the validation contract + feature criteria, frozen at approval) and
the standards axis (scrutiny's "regressions outside the diff's intent"
plus the repo conventions the session inherits). The validator-repair
week is the case study: the failures were tooling, not judgment — when
the scrutiny validator ran, it caught what it was built to catch. No
change.

## 4. The wrong-spec gap — **ADOPT**

His loop catches drift *from* the spec, not a spec that's wrong, because
the same model grills and writes. Kranz has the underspecification path
(NeedsContext) but no distinct "this plan is confidently wrong" signal —
a wrong plan is currently discovered mid-mission (blocked, revision) at
mission prices. Cheap version worth building: a draft-stage escalation
distinct from NeedsContext — the planner can say "I can plan this, but
the plan is likely wrong / the goal is misframed" and park for the
operator before anything is approved. Follow-up ticket filed:
`draft-wrong-plan-escalation`.

## Summary

Two of four adopted with follow-ups, two validated as already present
(kranz's fresh-context-per-role design predates the video by months —
convergent evolution or the same folk theorem). The durable takeaway is
his sharpest point: review must be fresh-context and the spec must be a
checkpoint artifact, both of which kranz already enforces as architecture
rather than habit.
