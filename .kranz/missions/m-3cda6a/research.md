# Research — m-3cda6a

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- docs/scoping/local-inference-executor-tier.md
- .kranz/tickets/local-inference-router-escalation.md
- .kranz/tickets/local-inference-backend-local.md
- .kranz/tickets/local-inference-validator-guarded.md
- .kranz/tickets/local-inference-cost-accounting.md
- docs/knowledge/architecture/mission-pipeline.md
- docs/knowledge/validation/gates.md
- docs/knowledge/decisions/inviolable-invariants.md
- docs/knowledge/glossary.md
- docs/tickets.md
- crates/engine/src/ticket.rs
- crates/engine/src/types.rs
- crates/engine/src/config.rs
- crates/engine/src/orchestrator.rs
- crates/engine/src/events.rs
- crates/engine/src/reducer.rs
- crates/engine/src/backend_local.rs
- apps/dashboard/src/components/StatusStrip.tsx
- apps/dashboard/src/components/TopBar.tsx
- apps/dashboard/src/lib/store.ts
- apps/dashboard/src/lib/types.ts
- crates/server/src/rest.rs
- apps/dashboard/package.json

## External sources

- docs/scoping/local-inference-executor-tier.md §2/§4 addendum (KRZ-206a scope; validator-local split out)
- .kranz/tickets/local-inference-backend-local.status (state:done, m-c6882a) — BackendKind::Local already shipped

## Facts

- `BackendKind::Local` and a full `LocalBackend` HTTP-in-engine impl already exist and are wired into `Engine::select_backend`; this mission builds on them, it does not add the backend. — `crates/engine/src/types.rs:498-518 (BackendKind::Local), crates/engine/src/backend_local.rs (LocalBackend), orchestrator.rs:1043-1071 (select_backend constructs LocalBackend); .kranz/tickets/local-inference-backend-local.status state:done`
- The executor is the `Role::Worker`; validators are the separate `ValidatorScrutiny`/`ValidatorFunctional` roles, each with its own RoleConfig — so leaving validators frontier is a matter of not touching those two role configs. — `crates/engine/src/types.rs:196-203 (Role enum), types.rs:535-539 (MissionConfig per-role fields)`
- 'Two failed validations' maps exactly to the existing fix-cycle cap `max_fix_cycles_per_milestone` (default 2); the block decision is `if self.fix_cycle_exhausted(mi) { emit(MilestoneBlocked) }` in validation_round, mirrored twice in final_gate. — `orchestrator.rs:3894-3911 (validation_round), orchestrator.rs:4345 & 4372 (final_gate mirrors), orchestrator.rs:3922-3924 (fix_cycle_exhausted), types.rs:632 (default 2)`
- There is no escalation/tier event or tier state today; `types.rs`/`events.rs` are additive-only CONTRACT files (add fields with `#[serde(default)]`, new enum variants allowed), so a `TierEscalated` variant and `executor_tier`/counter fields must be additive with defaults. — `events.rs:41-325 (EventKind, no tier variant), docs/knowledge/decisions/inviolable-invariants.md 'Contract files are additive-only', AGENTS.md rule 6`
- MissionState is served verbatim to the dashboard via `GET /api/missions/:id/state` (reducer::fold) and WS frames, then mirrored in `types.ts` and rendered from the Zustand store; a new scalar/counter on MissionState reaches the UI with no server change, exactly like `total_cost_usd`. — `crates/server/src/rest.rs:96-107 (mission_state), reducer.rs:250-265 (total_cost_usd fold), apps/dashboard/src/lib/store.ts:259-267 (applyFrame), StatusStrip.tsx:64-66 / TopBar.tsx:41 (render)`
- The dashboard test suite is `vitest run` (npm run test) with colocated `*.test.tsx`, store-seeded via `useKranzStore.setState`; node_modules and the vitest bin are present in-tree, so a targeted `npx vitest run -t escalation` contract command is safe in the final-gate environment. — `apps/dashboard/package.json:13 ("test":"vitest run"), StatusStrip.test.tsx (factory/setState pattern), verified apps/dashboard/node_modules/.bin/vitest present`

## Ambiguities & stale docs

- 'Escalation rate per mission' is under-specified for a one-way mission-level tier flip. Chosen definition: escalatedMilestones / localExecutorMilestones (milestones begun under the Local tier), 0.0 when the denominator is 0 — a genuine per-mission fraction that also aggregates across missions as the 'local-model competence' metric.
- Routing to local requires a base_url/context (a configured local endpoint), which a fresh mission's default config does not have. Chosen resolution: routing sets executor_tier=Local and the Worker local backend only when a local endpoint is resolvable from the seed config; otherwise it fail-safes to Frontier and records the reason, keeping the applied tier (and thus the escalation rate) honest. If the operator wants a different endpoint source (host/env default), flag at execution.
- The exact ticket→mission config seeding call site was not pinned during planning; feature 1.2 directs the worker to locate it (engine draft/create path and/or crates/cli/src/backlog.rs) and apply routing before `mission.created` is emitted.

## Candidate knowledge updates

- Add a docs/knowledge note on the executor-tier routing/escalation model: task-class tag → ExecutorTier at seed, one-way local→frontier escalation on fix-cycle-cap exhaustion via `tier.escalated`, and the per-mission escalationRate = escalatedMilestones/localExecutorMilestones surfaced on the dashboard.
- Update docs/tickets.md and the ticket frontmatter schema section to document the new `task-class` frontmatter key and its execution-class→local routing semantics.
