# Replaying past missions to check the gates (scoping)

Status: scoped 2026-11-23. D-A to D-D are OPEN. This doc records the idea and
the boundary question without deciding either. Nothing here is built.

## Why

Kranz's claim is that a mission which reaches COMPLETE passed its contract,
its validators and its final gate. The repo has little direct evidence of how
often those gates catch a defect that is really there. The false-green figure
in docs/metrics.md counts completed missions that later had a defect ticket
traced to them; it measures escapes after the fact, on whatever work happened
to run.

A replay turns the question around. Take a past change whose outcome is
known, run the gates against it, and see whether they say what is now known
to be true. Two kinds of past change are useful:

- **A known escape.** A mission that completed and later had a defect traced
  to it (`traced-from-mission` in the defect ticket). Would today's contract
  lint, validators and final gate still pass it?
- **A known good change.** A mission or pull request that merged and held.
  Do today's gates pass it, or do they now block good work?

The idea of using a team's own history as the test set is common in writing
about agent-run software pipelines. There it is usually proposed for
comparing models and agent setups on cost and quality. That use is the
boundary question below.

## What exists already

- `kranz provenance <mission>` rebuilds from the event log why a unit passed:
  gates in order, artefacts, backend and model, prompt hash, human decisions.
- `crates/engine/src/review_memory.rs` holds a versioned, human-reviewed
  replay suite for review rules, with required and prohibited findings, and
  will not promote a rule until it passes.
- The event log is append-only and replays to the same state.
- Mission branches and plans are tracked, so a past mission's base commit,
  contract and final tip can be recovered while its branch exists.

None of these reruns a gate against an old change.

## D-A. Is a replay inside the positioning boundary? (OPEN)

docs/knowledge/decisions/positioning-governance-evidence-layer.md freezes new
in-harness primitives whose purpose is to make an agent write better code.

- A replay that scores **the gates** (did the contract and validators catch a
  known defect; did they block a known good change) measures the evidence
  layer. That looks inside the boundary.
- A replay that scores **agents** (rerun the ticket with another model or
  setup and compare cost and quality) is an execution benchmark. That looks
  outside it, or at least needs the same three-property test the
  heterogeneous dispatch pool had to pass.

The operator decides whether the first is in scope, and whether the second is
ruled out or parked.

## D-B. What is replayed? (OPEN)

- **Gates only, on the recorded tip.** Check out the past mission's final
  commit and run today's contract lint and the mission's own command
  assertions. Deterministic and cheap. It cannot rerun agent-judgement
  assertions or validators without spending money.
- **Gates and validators.** Also rerun the scrutiny and functional
  validators. This costs per replay and is not deterministic, so one run is
  weak evidence.
- **The whole mission.** Rerun planning and workers from the ticket. This is
  the agent benchmark of D-A and the most expensive option.

## D-C. Where does the known outcome come from? (OPEN)

A replay needs a label that the gates did not produce. Candidates: a defect
ticket with `traced-from-mission`; a revert of the mission's merge; an
operator-written expectation, in the style of the review-memory suite's
required and prohibited findings. Whichever is chosen is human-reviewed
source, tracked in the repo, and small. A suite of five labelled cases says
little, and the doc that reports results should say so with raw counts.

## D-D. What is done with a result? (OPEN)

- Report only: a read-only command prints which labelled cases today's gates
  would pass or block.
- A regression check for gate changes: a change to the contract lint,
  preflight or final gate runs the suite, and a known escape that used to be
  caught and no longer is fails the check.

The second option makes the suite a gate on Kranz's own development. It
should not become a gate on user missions.

## Constraints that apply whatever is decided

- Replays never write to the original mission's event log, branch or runs
  directory. A replay is a new record.
- Replays run under the same containment as a mission: wrapped commands,
  cleared environment, no ambient credentials
  (docs/knowledge/validation/child-containment.md).
- Old mission branches are deleted over time. A suite that depends on them
  must pin commits that still exist, or store what it needs.
- Results are evidence about the gates on this repo's history. They do not
  generalise, and public docs should not present them as a quality score.

## Not decided here

Whether to build any of this. The cheapest useful step, if D-A allows it, is
D-B's first option against missions that have a traced defect. docs/metrics.md
recorded none on 2026-08-02, so the first suite may have to start from
operator-written cases (D-C).
