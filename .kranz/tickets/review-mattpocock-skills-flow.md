---
state: done
state-note: docs/reviews/mattpocock-skills-flow.md: idea 1 ADOPT-partial → ticket plan-feature-context-fit-check; idea 2 validated already present (compression artifacts at every boundary, validator non-compression by design); idea 3 validated already present (fresh-context validators + both review axes); idea 4 ADOPT → ticket draft-wrong-plan-escalation. No code changed.
title: Review Matt Pocock's skills flow for ideas worth adopting in kranz
priority: 3
schedule: once
---

## Goal

Evaluate the ideas from Matt Pocock's skills-repo workflow (grill →
spec → tickets → implement-per-session → fresh-context review) against
kranz's ticket pipeline, and record an adopt/reject decision per idea
with follow-up tickets filed for anything adopted.

## Context

Source: Matt Pocock's skills tutorial video (transcript reviewed
2026-07-16). His flow is a human manually doing what kranz automates —
he is the scheduler deciding when to clear context, which ticket runs
next, and whether budget remains for one more slice. The mapping is
direct: spec = mission plan, tickets = work units, clear-between-tickets
= session isolation, review agents = merge gates / final judgement.
Four ideas stand out:

1. **Context-window-sized work unit as the scheduling primitive.** His
   tickets are sized not by effort but by "fits in one context window
   before attention degrades" (his folk number: ~140k tokens, the
   "smart zone"). Kranz tickets are currently sized by the author's
   judgement; the draft/plan stage could estimate or enforce a
   context-budget per work unit, and the orchestrator could split or
   warn when a plan won't fit one worker session.

2. **Compress-to-artifact-before-clearing as the handoff contract.**
   His to-spec step distills ~46k tokens of interview dialogue into a
   durable spec *before* the context is cleared, so shared understanding
   survives session resets. Kranz's analogs are plan.md and report.md —
   worth checking whether every session boundary in the pipeline
   (draft → review → run → report) has an explicit compression artifact,
   or whether any handoff still leans on raw transcript/conversation
   state.

3. **Fresh-context review on two axes.** His implement step ends with
   subagent reviewers in clean contexts checking (a) work against the
   original spec and (b) work against repo standards docs, on the
   argument that an agent reviewing code it just wrote grades its own
   homework. Kranz's final gate judges the diff; confirm it always runs
   in a fresh context and explicitly compares against the drafted plan
   (spec axis), not just general quality (standards axis).

4. **The gap his loop doesn't close, and ours may not either: a wrong
   spec.** His review catches drift *from* the spec; nothing catches a
   spec that is itself wrong, because the same model asks the grilling
   questions and writes the spec. Kranz's NEEDS-CONTEXT flow is our
   grilling analog — consider whether the draft stage should ever
   escalate "this plan is confidently wrong" separately from "this
   ticket is underspecified".

## Acceptance hints

- A written review (docs/notes or ADR) with an explicit adopt / reject /
  defer verdict and one-line rationale for each of the four ideas.
- For each adopted idea, a follow-up ticket filed in .kranz/tickets/
  scoped to one implementable change.
- No code changes required by this ticket itself.
