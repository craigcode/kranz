# Research — m-87118c

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/outcomes.rs
- crates/engine/src/escalation_metrics.rs
- crates/engine/src/evidence_bundle.rs
- crates/cli/src/merged_costs.rs
- docs/metrics.md
- CHANGELOG.md
- .kranz/tickets/outcomes-autonomy-ratio-divergence.md
- AGENTS.md

## External sources

- .kranz/tickets/outcomes-autonomy-ratio-divergence.md
- docs/metrics.md §1

## Facts

- MissionOutcomes.is_closed includes MissionAbandoned and also feeds per-task-class closed_missions and the evidence bundle's Closed line — `crates/engine/src/outcomes.rs:488-495,1091; crates/engine/src/evidence_bundle.rs:628`
- outcomes.rs classifies steers by ts and omits milestone.unblocked from interventions — `crates/engine/src/outcomes.rs:497-520`
- escalation_metrics uses the first-terminal rule and the shared mission_interventions rule (seq-based, operator unblocks counted) — `crates/engine/src/escalation_metrics.rs:264-302,393-398,583-604`
- MissionOutcomes.interventions also feeds CostPerMergedChange.zero_intervention_share, which `kranz outcomes --all` prints as 'autonomy N%', with a denominator that includes abandoned missions — `crates/engine/src/outcomes.rs:1399-1417; crates/cli/src/merged_costs.rs:140-147`
- The existing test pins closed 2 / share 0.5; its escalation twin pins closed 1 / share 1.0 — `crates/engine/src/outcomes.rs:2387-2446; crates/engine/src/escalation_metrics.rs:1481-1543`

## Ambiguities & stale docs

- The three scope questions put to the operator went unanswered before the formal plan request, so this plan takes the orchestrator's stated recommendations: (1) is_closed keeps its any-terminal meaning and a separate flag drives the ratio; (2) the shared intervention rule replaces the inline loop everywhere, which also changes the evidence bundle's Interventions figure and the --all share's numerator; (3) the --all / cost-per-merged-change share keeps abandoned missions in its denominator, with only its stale doc comment corrected. Reject this plan if any of these should differ.
- The ticket asks to 'say so in the pull request' about the updated test; Kranz never pushes or opens PRs, so the change is recorded in a test comment and the changelog instead.

## Candidate knowledge updates

- docs/knowledge: note that `kranz outcomes --all` / cost-per-merged-change zeroInterventionShare still counts abandoned missions in its denominator, unlike the published autonomy ratio.
