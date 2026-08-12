---
title: Flight Rules resolution, approval pinning, and policy-drift refusal
priority: 1
schedule: once
blocked-by: [flight-rules-pack-contract]
state: done
state-note: Implemented — pack/resolution.rs (deterministic resolver, approval_pin, merge_drift), additive standardsManifest contract, standards.resolved/standards.drifted events, final-validation park + merge refusal. 22 flight_rules_pin_* tests; workspace gates green.
---

## Goal
Resolve applicable Flight Rules deterministically, show and pin the normalized
manifest in plan approval, consume only that snapshot during the mission, and
refuse final/merge processing when actual scope or live-base policy changes the
applicable enforced set.

## Context
KRZ-342; design D-D/D-E in
`docs/scoping/flight-rules-engineering-standards.md`. Follow the existing
merge-gates/routing-rules ownership idiom: repo-relative standards are read
from tracked blobs in the trusted base Git tree, never the mission worktree;
that includes referenced pack checker declarations. An external pack is
capability-read once and its normalized bytes/digest are the approved advisory
authority, but cannot activate enforced rules without a verifiable tracked or
future signed/versioned source. This is a consent artifact, so add
`standardsManifest` to the plan contract additively and follow
`events.rs`/`types.rs` compatibility rules.

## Acceptance hints
- Same pack + task class + stage + approved touch set always selects the same stable-sorted rules; domains never invoke model selection.
- Initial candidate selection → proposed touch-set re-resolution reaches a tested fixed point; a newly selected rule triggers a bounded plan revision before review.
- Draft/plan review includes source digest, IDs, revisions, effective statuses, statements, scopes, and checker bindings; approval rejects a stale manifest.
- A mission-branch pack edit is ignored and surfaced for the current mission; an external pack edit after approval cannot change a run.
- An enforced rule from an untracked external pack is refused at approval; approved advisory rules from that pinned snapshot remain supported.
- Final validation compares actual changed paths with the approved envelope; a newly applicable enforced rule parks for revision/reapproval.
- Merge resolves current live-base policy against the exact scratch integration diff; applicable enforced-set drift emits `standards.drifted` and refuses.
- Missing pack remains today's byte-identical path; malformed configured pack fails before child spawn or other run side effects.
- Anti-vacuity: unique filter `flight_rules_pin_` reports a nonzero pass count.

## Wrong plan (from orchestrator)
KRZ-342's entire foundation is absent from the base this mission would branch from: main is at 90a4bdc and `git cat-file -e main:crates/engine/src/pack/standards.rs` fails — there is no standards module, no StandardsManifest, no StandardsTrust and no normalized digest anywhere in the tracked tree, and crates/engine/src/pack.rs still refuses `schema = 4` outright (supported versions are 2 and 3). All of that is KRZ-341, which this ticket explicitly declares `blocked-by: [flight-rules-pack-contrac … (truncated)
