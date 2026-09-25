---
title: Glossary
owner: mixed
freshness: check-on-touch
last_verified: 2026-09-25
verified_against:
  - crates/acp/src/lib.rs
  - docs/scoping/shared-acp-client.md
  - crates/engine/src/container_egress.rs
  - crates/engine/src/orchestrator/live_permissions.rs
  - crates/engine/src/acp_worker.rs
  - crates/engine/src/backend_acp.rs
  - crates/engine/src/reviewer_independence.rs
  - crates/engine/src/types.rs
  - crates/engine/src/orchestrator.rs
  - crates/engine/src/config.rs
  - crates/engine/src/planning.rs
  - crates/engine/src/control.rs
  - crates/engine/src/event_log.rs
  - crates/engine/src/paths.rs
  - crates/slack/src/format.rs
  - AGENTS.md
---

# Glossary

- **Shared ACP client** — `kranz-acp` owns bounded framing and session protocol; the engine retains permission authority, event/cost normalization and process containment. Client terminal/filesystem services and resume remain disabled. See [the boundary](../scoping/shared-acp-client.md).

Project vocabulary. Terms link to the note that explains them in depth.

- **Mission** — one unit of work driven through the
  [pipeline](architecture/mission-pipeline.md): draft → review plan → queue →
  run → deliver → gated merge → land. Identified `m-<hex>`.
- **Ticket** — a backlog item at `.kranz/tickets/<slug>.md`. Its additive
  `state:` frontmatter is lifecycle authority; the gitignored `.status`
  sidecar is a write-through compatibility cache. The ticket is planning input
  for a mission.
- **Plan** — the approved unit of intent: `plan.json` (durable, structured) plus
  a `plan.md` twin, committed on the mission branch. Contains milestones,
  features, and the validation contract.
- **Milestone / Feature** — plan structure. A milestone groups features; a
  worker implements one feature at a time.
- **Validation contract** — per-plan assertions the mission must satisfy. Each
  is a `Command` (a build/test/lint invocation), `AgentJudgement`
  (a validator/orchestrator verdict), or `PtyScript` (an interactive terminal check). See
  [gates](validation/gates.md).
- **Baseline/candidate pair** — opt-in advisory observations at two actual
  revisions, with approved expectations, separately identified checker overlay
  and source/environment configuration bindings. A recorded pair is not a
  current gate decision. See [critical assertion controls](../contract-controls.md).
- **Orchestrator** — the planning-and-judging agent role: drafts plans, proposes
  revisions, and renders final-gate verdicts. Claude uses a streaming session;
  non-Claude backends use fresh single-shot turns. Execution turns receive the
  current durable-state digest, including the effective repair allowance.
- **Worker** — the coding agent role; one run per feature, using the configured
  checkout or dedicated [worktree isolation](decisions/inviolable-invariants.md).
- **Validator** — the checking roles: **scrutiny** (adversarial review) and
  **functional** (runs the contract's command gates). Configurable floors apply
  to autonomous runs.
- **Reviewer independence** — optional approval-pinned requirement that a reviewer
  use a known model family different from every recorded worker attempt; backend
  fallback, retries and restart must preserve it. Completion requires successful
  compatible review evidence from the latest relevant validation round. See
  [config composition](../config-composition.md#reviewer-independence-reviewerindependence).
- **base_sha** — the base branch tip pinned at approval. All contract/final-gate
  diffs are taken against it; it is never re-resolved later.
- **Plan identity** — the full sha256 over a plan's canonical JSON, shared by
  dashboard previews and Slack cards. Approval submits the displayed identity,
  so a stale preview cannot commit a replacement plan. The shared implementation
  is `planning::plan_identity`; missing or stale preview identities require a
  refreshed review. See
  [slack-commands](surfaces/slack-commands.md).
- **Event log** — `events.jsonl`, the append-only single-writer source of truth.
  `fold(events)` == `MissionState`; `state.json` is only a cache. Every line a
  keyed writer produces carries `h` (a sha256 chain over the previous `h` and
  the event's canonical bytes) and `m` (an HMAC of `h` under the authority
  key).
- **Authority key** — a per-repository 32-byte key kept OUTSIDE the repo, at
  `<global kranz dir>/keys/<repo fingerprint>.key`. It signs control files and
  MACs event-log lines. The sandbox denies sessions both read and write of the
  key directory, so an agent can neither compute nor replace a signature.
- **Global kranz dir** — `$KRANZ_HOME` when set, else `~/.kranz`
  (`%USERPROFILE%\.kranz`). Holds the operator config layer plus the authority
  keys, seal floors, and high-water marks. Read from the engine's own
  environment; agent sessions spawn env-cleared and cannot redirect it.
- **Seal floor / high-water mark** — the two out-of-repo witnesses recorded
  beside the authority key. The seal floor is the first `events.jsonl` seq that
  must carry `h` and `m`, so stripping integrity off every line is a forgery
  rather than a legacy log. The high-water mark is the highest seq durably
  written, so truncating the log is caught at resume.
- **Control inbox** — `.kranz/missions/<id>/control/*.json`, the filesystem
  channel the run loop drains for grant approvals, revision decisions, answers,
  and config changes. Every file carries a `sig` HMAC under the authority key;
  unsigned or wrongly signed files are quarantined to `.bad` and never applied.
- **Operator-only config keys** — keys the project layer
  (`<repo>/.kranz/config.json`) may not set because each names a program the
  engine runs, an endpoint it talks to outside the sandbox, a credential, or a
  containment escape: `claudeBinary`, `packDir`, `contractEnvPassthrough`,
  `slack`, `hookStatus`, `<role>.baseUrl`, `<role>.acpProfile`, the `<role>.sandbox.*` widening
  keys, and the `dangerouslyAllowAll` family. They are settable from the global
  layer only. A repository may RAISE `sandbox.enforce`, never lower it.
  Runtime `config-change` patches carry the same split by source: the
  authenticated control inbox may re-tune bounded mission knobs, while
  consent-bearing keys need an operator surface.
- **Deliver vs Land** — a mission is **Delivered** when it Completes but its
  branch is unmerged, and **Landed** once merged.
- **Drain** — running the per-repo execution queue (`kranz work`); serialized
  per repo.
- **Gate** — a typed check that can be deterministic or model-judged. The
  existing `Gate` interface reports pass/fail; the external protocol separates
  evaluation, engine disposition and operator consent. See
  [gates](validation/gates.md).
- **Empty-deliverable net** — a mission with zero non-meta feature commits vs
  its pinned `base_sha` FAILs rather than falsely Completing.
- **Anti-vacuity** — the contract rule that a filtered test run matching zero
  tests is a vacuous pass, not a real one
  (`grep -qE 'test result: ok\. [1-9]'`).
- **Lessons** — append-only cross-mission memory in `.kranz/lessons/`, injected
  tightly-capped into planning. See [lessons.md](lessons.md).
- **Backend** — the dispatch target a role runs on: `claude`, `codex`, `droid`,
  `kimi`, `cursor`, an OpenAI-compatible `local` endpoint, or a worker-only
  `acp` agent executable. Selection and sandbox support are validated per role.
- **ACP worker profile** — operator-selected `worker.acpProfile` fixing a
  qualified adapter/image, private file credential, startup policy and configured
  egress. It requires contained worktree workers; generic ACP enforcement remains
  refused. Mission egress grants cannot widen its fixed allowlist. Cache and
  relay access assume a trusted host/daemon; a version prerequisite alone is
  not profile qualification. See [profile setup](../acp-containment.md#qualified-ordinary-workers).
- **Mutation authority** — a validated, nonempty token required by the server's
  mutation-capable constructors. Convenience routers mint an undisclosed token,
  so reads work and unauthenticated mutations are refused.
- **`kranz serve`** — the REST/WS mission host. It defaults to loopback and can
  compose an operator-configured catalog of repositories for the dashboard and
  the single Slack bridge.
- **Respawn / fix-cycle / fix-feature** — a respawn re-runs a failed worker; a
  fix-cycle is counted when a validation round emits repair features; a
  fix-feature is a feature created to resolve a finding. Waiving findings
  does not consume a repair round.

- **One-call permission** — a short-lived ACP request bound to one action, offered
  options, peer/session/run, workspace and approved plan/policy. Its durable
  resolution precedes the response; delivery and tool outcome remain separate.
  Expiry ends the affected session without cancelling sibling candidates.
  It never becomes a mission-wide command grant. See
  [live consent](../acp-live-permissions.md).
