# M2 scoping — Mid-mission re-planning (the past is frozen, the remainder is negotiable)

Status: scoped 2026-07-06, DESIGN-FIRST — the six flagged decisions (D-A…D-F) are the operator's; each carries a
recommendation, none is DECIDED. Review before any mission is drafted against this. Roadmap home: M2
(docs/roadmap.md:29–48, "re-planning = safe subset"). Claims the "Re-planning UI (M2)" territory pipeline-view.md:160 reserved.

## Why

Live use exposed the lifecycle edges v1 cut (roadmap M2 preamble). The receipts:

- A tested safe subset ALREADY SHIPPED — `request_revised_plan` / `approve_revised_plan`
  (crates/engine/src/orchestrator.rs:828/871; handoff.md:17–18; tests mission_test.rs:2386,2529) — with **zero call
  sites** in CLI, server, Slack, or dashboard. Operators steering a live mission today have no scope-changing
  affordance: pause/resume/config/interrupt exist (types.rs:473–480; drain_control orchestrator.rs:1217–1246;
  control.rs:199–232), but nothing an operator can send changes the remaining work — every scope outcome, including
  `handle_blocked`'s skip-milestone option, is mediated by an orchestrator decision (orchestrator.rs:1277–1339).
- `approve_plan` is structurally once-only: it rejects any status but Planning (orchestrator.rs:697–702) and the
  reducer never returns there (reducer.rs:83). Re-emitting `plan.approved` would clobber — milestones rebuilt
  wholesale, every status reset to Pending (reducer.rs:50–84).
- The safe subset has **no human gate at all** — the caller passes the plan straight in. The roadmap's "fresh
  approval gate + plan.md diff" (docs/roadmap.md:33–37) is entirely unbuilt. It is also spend-blind:
  `render_revised_plan_markdown` has no cost section (orchestrator.rs:3630–3690), while first approval bakes an
  estimate into plan.md (orchestrator.rs:740–752).
- The one hint pointing at post-park re-planning is a dead door: backlog.rs:382 suggests `kranz plan --mission <id>`,
  which `cmd_plan` refuses because parked missions sit at Approved (commands.rs:508–513; draft.rs:262–305).

**The engine can already revise a running plan; no human can ask for it, and no gate fires when it does.** The contract note at orchestrator.rs:779–814 is this doc's preamble — it ends in an explicit contractChangeRequest: "add a `plan.revised { plan }` (or a narrower `milestone.added { milestone }`) event whose reducer semantics MERGE the revised remainder onto the existing milestones." This doc rejects the narrower `milestone.added` shape: it cannot express drops or reorders of later milestones, which the remainder-rebuild semantics below require.

| Capability | engine | CLI | web | Slack |
|---|---|---|---|---|
| Propose revised remainder | ✓ orchestrator.rs:828 | — | — | — |
| Apply revision, work preserved | ✓ subset, orchestrator.rs:871 | — | — | — |
| Human consent on the revision | — | — | — | — |
| Diff artifact + fresh estimate | revised-plan.md, estimate-less | — | — | — |

## What exists (reusable, honestly labeled)

- Safe-subset semantics: completed milestones reproduced first and unchanged (`completed_features_unchanged`,
  orchestrator.rs:3604–3617); delta via existing events only — `feature.skipped` + `fixfeature.created` with
  `<ms>-replan-<cycle>-<n>` ids (orchestrator.rs:1023–1046). Cannot add/reorder milestones, touch later ones, or change the contract (orchestrator.rs:804–814).
- The pre-approval consent kit: pending_plan cache (host.rs:430; volatile by design, host.rs:66–70), approve-pending
  routes (host.rs:846–898, lib.rs:152–159), Slack plan-review card (format.rs:698–746), dashboard PlanReview panel
  (PlanReview.tsx:109–121), mutation token (lib.rs:199–205), Slack spend allowlist (bridge.rs:1699–1702).
- The only channels to a running engine: control inbox in (types.rs:473–480; drained duplicate-tolerantly between
  worker runs, orchestrator.rs:1204–1239), event log out (WS tail ws.rs:1–56; Slack tail outbound.rs:10–17).
  Planning endpoints 409 while Running (host.rs:1021–1023). Everything below the surface exists; nothing above it does.

## Design principles (restating standing rules — not new decisions)

1. **The past is frozen.** Completed milestones, their ids, tags, events, and the pinned base_sha are immutable. A revision negotiates only the remainder.
2. **A revision IS a plan approval.** Same sacred Approve ceremony (pipeline-view.md D-A), same three surfaces, same token/allowlist gates. No cheaper consent path exists.
3. **The loop never changes owners.** The run task keeps the engine and the single-writer lock through the gate; consent rides the control inbox; the append-only log records everything.

## D-A — the verb (OPERATOR DECISION)

Approve, Queue, Start are spoken for (pipeline-view.md D-A); Reshape is the pre-approval sibling at Reviewable. This is its post-approval sibling on Running/Paused/Blocked rows.

**Proposal: Revise.** The noun is "revision", the event family `plan.revised` is already named by the standing
contractChangeRequest, the artifact is already revised-plan.md. Rejected: *Replan* reads as start-over, contradicting
the immutability promise in its own name; *Amend* is git-loaded (history rewriting — exactly what this must never
imply). The consent button on the card stays **Approve** — per D-A discipline it is a plan approval, not a new verb.

## D-B — trigger paths (OPERATOR DECISION)

1. **Operator-initiated**: Revise on a Running/Paused/Blocked row (secondary-action slot, pipelineStage.ts:113
   pattern) takes a one-line "what changed" → `POST /api/missions/:id/revise` → control inbox → in-loop
   `request_revised_plan` turn. CLI: `kranz revise <id> "<text>"`. Slack trigger wiring: verify at build time (thread
   replies today land only as advisory `user.message`).
2. **Orchestrator-initiated**: `handle_blocked` gains a propose-revision action beside skip-milestone
   (orchestrator.rs:1277–1339), and the consult prompt already says "you may adjust remaining work"
   (orchestrator.rs:1262). Proposals land in the same gate; the orchestrator NEVER self-approves.

**Proposal: build both, operator-initiated first** — it exercises the whole gate with a human at both ends. Headless
`kranz exec` auto-approves plans (exec.rs:180–183) and would silently swallow this gate: in exec mode,
orchestrator-initiated revisions are REFUSED — the mission proceeds or blocks honestly — mirroring the
floor-for-autonomous posture (worker-sandboxing.md:37–38).

## D-C — the in-flight milestone (OPERATOR DECISION)

Completed milestones are frozen — that part is roadmap contract. The milestone in flight:

1. **Milestone boundary** — revision applies from the next milestone; the current one completes or is skipped
   first. Predictable, but keeps spending on scope the operator just rejected.
2. **Feature boundary** — the active worker finishes its feature (the inbox is drained between worker runs anyway,
   control.rs:72–89); completed and active features in the in-flight milestone are frozen, still-Pending ones
   revisable. Exactly the shipped safe-subset semantics (orchestrator.rs:916–968).
3. **Abort the active worker** via `Msg{interrupt:true}` plumbing (control.rs:199–232) — fastest stop, but leaves a half-written feature to clean up.

**Proposal: (2), feature boundary.** No new abort machinery, matches the shipped merge semantics, bounds wasted spend to one feature. (3) is explicitly deferred, not rejected.

## D-D — gate mechanics (OPERATOR DECISION)

**Proposal**, end to end:

1. Triggers drain as `ControlCommand::RequestRevision{instructions}` — idempotent by revision number to satisfy
   duplicate-tolerant drain (orchestrator.rs:1204–1209). The loop runs the existing `request_revised_plan` turn;
   NotReady stays conversational, never an error (PlanRequest, orchestrator.rs:161–170). The three new
   `ControlCommand` variants (RequestRevision/ApproveRevision/RejectRevision) land in the SAME contractChangeRequest
   as the `plan.revision.*` events below — types.rs is a contract file exactly like events.rs.
2. Ready → the engine emits `plan.revision.proposed {revision, plan}`; the reducer folds it to
   `state.pending_revision`. No new MissionStatus — the mission stays Running/Blocked, so nothing new must be kept
   event-reachable (events.rs:11–13). WS transports the new event unmodified (ws.rs:1–56); the Slack tail needs a
   `classify()` arm before it renders anything (slice 4) — every reader must carry the slice-1 reducer/event
   definitions first, or it simply drops the new kind.
   events.rs/types.rs are CONTRACT FILES (events.rs:3–4): this lands as a contractChangeRequest extending the one on
   file at orchestrator.rs:809–814.
3. **The review artifact is the plan.md diff** (roadmap contract): the server renders the revised plan through
   `render_plan_markdown` and serves a unified diff against the committed plan.md (new GET beside lib.rs:134's
   plan.md route). Nothing is committed until approval. **The estimate is re-run, calibration-aware**: project a
   remainder-Plan (incomplete milestones/features only) into the pure count-based `estimate` (cost.rs:168–199) with
   `calibrate` fresh per call (cost.rs:303–347) and the same `apply_shape` widening. Shown beside the diff, every surface.
4. **Fresh approval on all three surfaces**: reuse the pending_plan cache + approve-pending route pattern
   (host.rs:430, 846–898), with the proposed EVENT as the authoritative copy so a serve restart cannot orphan the
   gate (the pre-approval cache is volatile, host.rs:66–70). Dashboard: single-consent PlanReview variant — Start is
   meaningless mid-run. Slack: a `build_plan_review` variant with Approve/Reject, allowlist-gated. CLI: `kranz
   missions` shows REVISION PENDING. All mutating routes token-gated (lib.rs:199–205).
5. Consent returns as `ControlCommand::ApproveRevision{revision}` / `RejectRevision{revision}`. On approve, the
   engine rewrites plan.json + plan.md (fresh "## Cost estimate"), upserts index.md, commits
   `[kranz] revised plan for <id> (rev N)` — plan.md was built "diffable across re-plans" (orchestrator.rs:728–745) —
   and emits `plan.revised {plan, revision}` with MERGE reducer semantics: completed milestones and ids preserved,
   remainder rebuilt, coexisting with fix/replan id minting and the duplicate-id guard (reducer.rs:218–223). Reject
   emits `plan.revision.rejected {revision}`. The append-only log records propose → approve/reject, always.

## D-E — contract implications (OPERATOR DECISION)

**base_sha is never re-pinned.** Resolved once at first approval (orchestrator.rs:717–721), it feeds every contract
command as KRANZ_BASE_SHA (runner.rs:495–501) and every validator round (orchestrator.rs:2203/2245/2276). Completed
work sits on the same branch; moving the base would shift the ground under already-validated milestones. Latent
inconsistency worth its own fix ticket: the final gate's judgement diff instead diffs the MOVING `base_branch`
(`judge_contract_assertions`, orchestrator.rs:2632–2633) — the doc's own "never re-resolve" rule (approve_plan
comment, orchestrator.rs:724–728) says this shouldn't exist, since a moved base branch would silently change the
final judgement's diff stat. The validation contract is per-mission, folded wholesale from `plan.approved`
(types.rs:42; reducer.rs:53), consumed by every validator round and the final gate (orchestrator.rs:2459–2565).
Replace, version, or extend?

**Proposal: extend-only, first build.** A revision may ADD assertions (ids continue through `assign_assertion_ids`,
orchestrator.rs:4137) and ADD `command_grants` (types.rs:54–57) — both rendered loudly in the review diff, since new
grants are new blast radius — but may remove or weaken NOTHING: loosening a mission's own acceptance gate after spend
has started is spend-adjacent in trust terms. If remaining scope genuinely invalidates an assertion, the honest move
is `kranz abandon` plus a new mission. Preflight re-runs against newly added contract commands before workers resume
(orchestrator.rs:497–555). Versioning is deferred, not rejected.

## D-F — run/queue interaction (OPERATOR DECISION)

- **Does re-planning pause the mission? Proposal: the proposal parks the loop.** After emitting
  `plan.revision.proposed`, the engine idles in the same inbox-poll Paused uses (orchestrator.rs:1141–1145) until
  consent arrives. No worker spend while a human decides; the alternative — keep executing scope that may be about
  to die — is rejected.
- **Who owns the engine?** The run task, unchanged, start to finish — the gate is asynchronous to the loop. The
  single-writer lock (event_log.rs:1–10, `EventLog::acquire`) is held throughout; serve never adopts the engine (contrast
  approve-then-queue's release dance, host.rs:623–636).
- **Queue**: the repo slot stays busy — busy-detection is the same lock probe (queue.rs:362–396); one running
  mission per repo unchanged (queue.rs:9–13). A parked mission is still live.
- **Rejection = resume.** `plan.revision.rejected`, then the loop picks up the ORIGINAL plan exactly where it
  parked. Rejection is not failure — iterate is always on offer (pipeline-view.md design principle 2).

## Build slicing (each a single mission brief)

1. **Contract + reducer merge semantics** — proposed/revised/rejected events via contractChangeRequest; reducer
   MERGE preserving completed milestones/ids/statuses; coexistence with fix/replan id minting and status derivation.
   Pure-Rust engine work; gate `--workspace` per the M5 meta-lesson (worker-sandboxing.md:104–105). Acceptance
   includes the `ControlCommand` JSON backcompat probe across engine/serve version skew (control.rs:33–56) — a
   version-skewed reader fails to DESERIALIZE the new event kind entirely (EventKind is a tagged serde enum,
   events.rs:35), so this is load-bearing, not optional.
2. **In-loop gate choreography** — ControlCommand variants + idempotent drain, park/resume, proposal-turn wiring
   (operator + handle_blocked paths), remainder estimate projection, approve-commit (plan.json/plan.md/index.md),
   added-assertion preflight, exec-mode refusal. Consumes slice 1. Gate `--workspace`.
3. **Server + CLI + dashboard** — revise/approve/reject routes (token-gated), plan.md-diff GET, pending revision in
   WS state, `kranz revise`, Revise row action + single-consent panel; fix the stale backlog.rs:382 hint in passing.
   Consumes slice 2.
4. **Slack** — proposal card with diff + remaining estimate, Approve/Reject buttons (allowlist-gated), tail
   classification of the new events. Consumes slice 2; independent of slice 3.

Sequencing: 1 → 2 → {3, 4}. Slices 3 and 4 land in either order; every-surface parity means M2 is not done until BOTH have.

## Out of scope

Aborting an active worker to revise faster (D-C option 3), contract removal/weakening or versioning (D-E deferral),
re-pinning base_sha, auto-approved revisions in `kranz exec` (a future `--allow-replan` is M5+ territory), reshaping
parked/Reviewable missions (pre-approval, pipeline-view territory), any change to the approval spend gate's
semantics, out-of-contract write detection (.kranz/tickets/sandbox-2-write-audit.md owns it).

## Done when

A mission with milestones 1–2 Complete and milestone 3 mid-flight takes a one-line Revise from the dashboard row;
spend stops within one feature; all three surfaces show the same plan.md diff with a calibrated remaining-work
estimate; the operator approves from Slack alone (dashboard closed) and the mission finishes the revised remainder,
final-gating against the extended contract. Across the revision, milestones 1–2's ids, statuses, tags, and base_sha
are byte-identical (asserted in a test, not eyeballed), and the event log reads proposed → revised with no gap.
Reject instead resumes the original plan unchanged. Roadmap M2 verbatim: "a scope change mid-mission produces an
approved revised plan without losing completed work" (docs/roadmap.md:45–46).

## Open questions (probe before build)

1. Does `parseEstimateFromPlanMd` (pipelineStage.ts:127–144) tolerate the rewritten plan.md's revised estimate
   section? Verify, don't assume — the dashboard has no other estimate source.
2. Can a Slack card carry a unified diff within block limits, or does it link out to the dashboard diff view?
   Probe with a real revision before slice 4.
3. Should the deterministic state digest (digest.rs:30–102) render `pending_revision` so a reseeded orchestrator
   session knows a gate is open? Probably yes — verify against the reseed path (digest.rs:107–113).
4. Revision numbering vs the existing replan `cycle` counter (orchestrator.rs:996–1008) — unify or keep separate?
   Decide in slice 1.