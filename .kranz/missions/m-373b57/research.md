# Research — m-373b57

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- crates/engine/src/types.rs
- crates/engine/src/reducer.rs
- crates/engine/src/orchestrator.rs
- crates/engine/src/orchestrator/live_permissions.rs
- crates/server/src/rest.rs
- crates/cli/src/commands.rs
- crates/slack/src/format.rs
- crates/slack/src/inbound.rs
- crates/slack/src/dispatch.rs
- crates/slack/src/bridge.rs
- crates/slack/src/outbound.rs
- apps/dashboard/src/App.tsx
- apps/dashboard/src/lib/api.ts
- apps/dashboard/src/lib/store.ts
- apps/dashboard/src/lib/pendingAsks.ts
- apps/dashboard/src/lib/types.ts
- apps/dashboard/src/components/GrantRequestPanel.tsx
- apps/dashboard/src/components/RevisionPanel.tsx
- apps/dashboard/src/components/QuestionRequestPanel.tsx
- apps/dashboard/src/components/BoardLens.tsx
- apps/dashboard/src/components/PermissionRequestPanel.tsx
- AGENTS.md
- .kranz/lessons/*.md
- .kranz/missions/m-fcde79/plan.md
- .kranz/missions/m-fcde79/research.md

## External sources

- read-only inspection of the repository at HEAD f6770281
- three read-only Explore investigations (Slack grant path; dashboard drafts and polling; permission expiry layout)
- git diff --stat 632b4f57 HEAD -- crates apps (no mission-relevant file changed since the m-fcde79 base)

## Facts

- Grant decisions match by target text only on every surface; PendingGrantRequest has no sequence number — `crates/server/src/rest.rs:1346-1363; crates/engine/src/orchestrator.rs:1955,2019; crates/slack/src/bridge.rs:2436-2460; crates/engine/src/types.rs:1044`
- The engine copies the current pending kind into grant.approved, so the reducer's kind cross-check always passes for a stale decision — `crates/engine/src/orchestrator.rs:1965-1968; crates/engine/src/reducer.rs:921-943`
- ControlCommand is internally tagged on 'kind', so a new variant field named kind would collide — `crates/engine/src/types.rs:1920`
- The Slack grant button value is mission:command with no seq or kind; refusals are already sent ephemerally — `crates/slack/src/format.rs:898; crates/slack/src/inbound.rs:647-654; crates/slack/src/bridge.rs:2423-2431`
- The dashboard posts { command } only for grant decisions — `apps/dashboard/src/lib/api.ts:339-353; apps/dashboard/src/components/GrantRequestPanel.tsx:44-46`
- A mission-to-mission hash change keeps unkeyed right-column panels mounted, so drafts survive in RevisionPanel, QuestionRequestPanel, the OrchestratorView composer and StandardsPanel — `apps/dashboard/src/App.tsx:74-77,138-151; apps/dashboard/src/lib/store.ts:468-480; apps/dashboard/src/components/RevisionPanel.tsx:11,22-42`
- Board polls use the default token gate and park a new waiter on every tick after a 401 — `apps/dashboard/src/components/BoardLens.tsx:161-252; apps/dashboard/src/lib/api.ts:153-158`
- Board files may reference only api.missionState among api methods (existing test) — `apps/dashboard/src/components/BoardLens.test.tsx:982`
- An expired permission card is removed from the DOM by the per-second deadline filter — `apps/dashboard/src/lib/pendingAsks.ts:50-52; apps/dashboard/src/components/PermissionRequestPanel.tsx:62-64`
- PendingGrantRequest has exactly one struct-literal site in the workspace — `grep 'PendingGrantRequest {' crates → crates/engine/src/reducer.rs:181 only`

## Ambiguities & stale docs

- HookStatusPanel polls api.hookStatus through the default token gate, the same defect class as the board; it was not among the reported items and is left as a follow-up.
- The Board ask-list entry for an expiring permission also drops out when the fleet model recomputes; the report concerns the permission panel controls, so the ask list is out of scope.
- The draft defect only triggers on a direct mission-to-mission hash change; in-app navigation goes through the pipeline and unmounts the panels, and the Board act column is already keyed by mission id.
- Engine drain-time refusals are recorded only as an orchestrator.decision and do not reach Slack; the REST and Slack pre-checks are what surface refusals to the operator.
- The Even G2 client and the CLI still send command-only grant decisions; they keep working because the new fields are optional.

## Candidate knowledge updates

- docs/knowledge/surfaces/slack-commands.md: document the grant button value format mission:seq:kind:command and the refusal of legacy card values.
- docs/knowledge/decisions/inviolable-invariants.md: note that ControlCommand is serde-tagged on 'kind', so new variant fields must not be named kind (use grant_kind).
