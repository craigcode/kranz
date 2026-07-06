# Mission plan — m-d309c0

**Goal:** Make the engine draft core detect a plan-shaped NotReady reply and either recover it through the plan channel or fail honestly — never filing plan JSON as questions — and bound the size of anything appended to a ticket's needs-context section.

Branch `kranz/mission-m-d309c0` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$6.23 – $31.17** (expected ~$12.47). Rough estimate — live usage is authoritative; based on 19 completed mission(s).

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** A plan-shaped NotReady reply (contains "validationContract" and "milestones") is never written into the ticket .md body: after driving a draft whose orchestrator emits the plan as prose on every turn, the ticket file contains no "validationContract" text. 
  `cargo test -p kranz-engine --test draft_test plan_as_prose`
- **[a2]** When a plan-shaped NotReady is followed by a parseable plan on the bounded plan-channel retry, the draft core approves the plan (outcome ParkedForReview / Enqueued), not NeedsContext. 
  `cargo test -p kranz-engine --test draft_test plan_as_prose`
- **[a3]** When the bounded retry still returns prose, drive_draft returns DraftOutcome::PlanAsProse, sets the ticket to NeedsContext via a short .status note, and writes nothing to the ticket .md body. 
  `cargo test -p kranz-engine --test draft_test plan_as_prose`
- **[a4]** Ticket::append_needs_context bounds its output: each question is truncated to at most 500 characters with a truncation marker, and at most 20 questions are written with an overflow marker line for the remainder — a multi-KB input never produces a multi-KB ticket section. 
  `cargo test -p kranz-engine --test ticket_queue_test append_needs_context`
- **[a5]** A genuine (non-plan-shaped) NotReady reply still becomes NeedsContext with the orchestrator's questions appended, unchanged from prior behaviour. 
  `cargo test -p kranz-engine --test draft_test`
- **[a6]** The whole workspace compiles and all tests pass, including every DraftOutcome consumer surface handling the new PlanAsProse variant. 
  `cargo test --workspace`
- **[a7]** All code is formatted. 
  `cargo fmt --all --check`
- **[a8]** The 'plan emitted as prose' message surfaced to the user (CLI, Slack, and REST/host) is honest and actionable — it states the orchestrator produced a plan but sent it as prose and tells the user to re-run the draft. *(agent judgement)*

## Milestone 1 — Needs-context appends are size-bounded

### 1.1 Cap the size of Ticket::append_needs_context

In crates/engine/src/ticket.rs, the associated function `Ticket::append_needs_context(repo_root, slug, questions: &[String])` (around line 429) currently writes EVERY question line verbatim and uncapped under the `## Needs context (from orchestrator)` heading, then flips the ticket to TicketState::NeedsContext. A multi-KB reply therefore writes a multi-KB ticket section. Bound it.

Add a pure, unit-testable free function in this module:

    fn bound_questions(questions: &[String]) -> Vec<String>

Behaviour:
- Per-question length cap of 500 CHARACTERS (not bytes). If a question is longer than 500 chars, truncate it to 500 chars and append the marker ` … (truncated)`. Truncation MUST be on a char boundary — use `.chars().take(500).collect::<String>()`, never byte slicing, so multibyte UTF-8 input never panics.
- Total count cap of 20 questions. If more than 20 are supplied, keep the first 20 (each still per-question capped) and then append ONE extra final line of the exact form `… (N more omitted)` where N is `questions.len() - 20`. That overflow line is a 21st entry in the returned Vec (it is written as its own `- …` bullet by the existing loop).
- 20 or fewer questions, all within 500 chars: returned unchanged (no markers added).

Wire `append_needs_context` to call `bound_questions(questions)` and write the bounded result through the existing `- `-prefixed loop, heading, atomic_write, and write_state(NeedsContext) logic. Do NOT change the heading text, the state transition, or the atomic-write mechanism.

Constraints: this is a defense-in-depth guard on ALL callers of append_needs_context, independent of the plan-as-prose work in the other milestone — do not add plan detection here. Keep the change confined to crates/engine/src/ticket.rs and its tests. This crate is `kranz-engine`.

Encode the criteria as tests FIRST, extending crates/engine/tests/ticket_queue_test.rs (which already has `append_needs_context_mutates_md_and_status` near line 205 you can mirror for git/tempdir fixture setup). Name at least one test with the substring `append_needs_context` so the contract command `cargo test -p kranz-engine --test ticket_queue_test append_needs_context` selects it. Assert against the on-disk ticket .md contents after the call.

Done when:
- A single 5000-character question is written truncated to 500 chars plus the ` … (truncated)` marker (total well under the input length), and the ticket .md no longer contains the full 5000-char string.
- Supplying 30 questions writes exactly 20 question bullets plus one `… (10 more omitted)` bullet (21 lines under the heading).
- Supplying 3 short (<500 char) questions writes them verbatim with no truncation or overflow markers.
- A question containing multibyte UTF-8 characters longer than 500 chars is truncated on a char boundary without panicking.
- cargo test -p kranz-engine --test ticket_queue_test append_needs_context passes.


## Milestone 2 — The draft core never files a plan-as-prose reply as questions

### 2.1 Detect plan-as-prose in drive_draft: bounded plan-channel retry, else honest PlanAsProse outcome

Context: crates/engine/src/orchestrator.rs `request_plan` (around line 603) already does one internal JSON retry; when neither the plan turn nor that retry parses, it returns `PlanRequest::NotReady(text)` carrying the raw reply. crates/engine/src/draft.rs `draft_decision` (line 41) blindly maps every NotReady to `DraftDecision::NeedsContext { split_questions(text) }`, and `drive_draft` (line 145) then calls `Ticket::append_needs_context`. For a plan emitted as PROSE, `text` is a multi-KB plan JSON blob, so the blob gets split line-by-line and filed as 'questions'. Fix this in the draft core.

All changes in crates/engine/src/draft.rs plus its tests. Do NOT edit orchestrator.rs, and do NOT touch crates/cli, crates/slack, or crates/server in this feature (the new enum variant will intentionally break their exhaustive matches — that is handled by the sibling feature 'Wire DraftOutcome::PlanAsProse through all consumer surfaces'; validate THIS feature with `cargo test -p kranz-engine` only, which builds the engine crate alone).

1. Add a private heuristic mirroring the slack bridge's reference implementation (crates/slack/src/bridge.rs:1980) — keep it engine-private, do NOT try to import from kranz-slack (slack depends on engine, not vice-versa):

    /// Does a NotReady reply look like a COMPLETE plan the orchestrator chatted
    /// out as prose instead of returning through the plan channel? Matches the
    /// plan schema's two distinctive top-level keys.
    fn looks_like_plan_json(reply: &str) -> bool {
        reply.contains("\"validationContract\"") && reply.contains("\"milestones\"")
    }

2. Add a new variant to `DraftOutcome`:

    /// The orchestrator produced a plan but emitted it as prose instead of
    /// through the plan channel, and a bounded retry did not recover it. No
    /// plan JSON is filed to the ticket body; the ticket is parked in
    /// NeedsContext with a short .status note and the user re-runs draft.
    PlanAsProse { mission_id: String },

3. Change `drive_draft`'s handling of the plan request. Keep the existing Approve path unchanged. Replace the current unconditional NeedsContext handling with: when `request` is `PlanRequest::NotReady(text)` AND `looks_like_plan_json(&text)` is true, do NOT append questions. Instead perform ONE bounded plan-channel retry by calling `engine.request_plan().await` exactly once more:
   - If that retry returns `Ok(PlanRequest::Ready(plan))`, follow the SAME approval logic the existing Approve branch uses (respect `then_enqueue`: approve_plan, then either enqueue+write_state(Queued) returning DraftOutcome::Enqueued, or write_state(Review) returning DraftOutcome::ParkedForReview) — factor the shared approval logic into a helper if that avoids duplication, but behaviour must match the current Approve arm exactly, including seed_reply/plan population in DraftDrive.
   - If the retry returns `Ok(PlanRequest::NotReady(_))` (still prose, whether or not plan-shaped) OR errors, DO NOT propagate as Err and DO NOT roll the ticket back to New. Instead: set the ticket to NeedsContext carrying a SHORT honest note via `Ticket::write_state(repo, slug, TicketState::NeedsContext, Some("The orchestrator produced a plan but emitted it as prose instead of through the plan channel — re-run `kranz draft` for this ticket.".to_string()))`. Do NOT call append_needs_context on this path (nothing goes into the .md body). Return `DraftDrive { outcome: DraftOutcome::PlanAsProse { mission_id }, seed_reply, plan: None }`.
   For a NotReady reply that is NOT plan-shaped, keep the existing behaviour exactly: split_questions + append_needs_context + DraftOutcome::NeedsContext.

   Note `request_plan` is `&mut self`; `drive_draft` holds `&mut engine`, so the extra call is available. 'One bounded retry' means EXACTLY one additional `request_plan()` invocation — never loop.

Encode criteria as tests FIRST in crates/engine/tests/draft_test.rs, using its existing MockBackend/MockScript/orch_script conventions (see the file header and the existing NeedsContext test near line 254). You will script the mock orchestrator turns: seed planning_turn, then the request_plan turns (each request_plan consumes a demand turn plus its internal JSON-retry turn), then — on the plan-shaped path — a second request_plan (two more turns). Give the plan-as-prose tests names containing the substring `plan_as_prose` so `cargo test -p kranz-engine --test draft_test plan_as_prose` selects them. A convenient plan-shaped prose payload is a fenced or prose-wrapped copy of a valid plan JSON (so it contains both \"validationContract\" and \"milestones\" yet fails strict parse); the recovering retry should return clean parseable plan JSON.

Done when:
- looks_like_plan_json returns true for text containing both "validationContract" and "milestones" and false otherwise (mirrors the slack bridge unit test).
- Orchestrator emits plan-as-prose, then the bounded retry returns clean plan JSON: drive_draft approves it (ParkedForReview when then_enqueue is false, Enqueued when true), and the ticket .md contains no "validationContract" text.
- Orchestrator emits plan-as-prose on the initial request AND again on the bounded retry: drive_draft returns DraftOutcome::PlanAsProse, the ticket status is NeedsContext with the short honest note, and the ticket .md body was not appended to (no `## Needs context` JSON blob).
- Exactly one additional request_plan call is made on the plan-shaped path (the mock script is exhausted correctly — no extra or missing turns).
- A non-plan-shaped NotReady reply still produces DraftOutcome::NeedsContext with the orchestrator's questions appended (existing behaviour preserved).
- cargo test -p kranz-engine passes (engine crate in isolation).

### 2.2 Wire DraftOutcome::PlanAsProse through all consumer surfaces

The sibling feature adds `DraftOutcome::PlanAsProse { mission_id }` to crates/engine/src/draft.rs. Rust exhaustive matches mean every consumer of DraftOutcome must now handle it or the workspace will not compile. Enumerate and fix EVERY site (this is the scattered-sibling-site trap that has cost respawns before — do not stop at the first two). Handle each by rendering an HONEST, actionable message: the orchestrator produced a plan but emitted it as prose instead of through the plan channel, so nothing was queued; re-run the draft for this ticket.

Sites to update (search the tree for `DraftOutcome::` to confirm you have them all before finishing):
1. crates/cli/src/backlog.rs — the match on the drive outcome around line 347 (alongside the NeedsContext / Enqueued / ParkedForReview arms). Print a clear one-line message to the user in the same style as the neighbouring arms (this CLI crate's package name is `kranz`, not `kranz-cli`).
2. crates/slack/src/bridge.rs — the match around line 701 (NeedsContext / Enqueued / ParkedForReview arms building Slack blocks). Produce an honest message block consistent with how the other arms build their blocks (e.g. via the existing error/info block helper used nearby).
3. crates/server/src/host.rs — the match around line 1176 that builds a json!({...}) response for each outcome. Add a PlanAsProse arm returning an analogous json object (include the mission_id and an honest status/message field consistent with the sibling arms' shape).
4. crates/slack/tests/tickets.rs — the test double `DraftOutcomeKind` enum and its mapping to DraftOutcome (around lines 352-356). Add a PlanAsProse kind and mapping so the test helper stays exhaustive and compiles.
Also check crates/server/tests/tickets_host.rs and crates/slack/tests for any other exhaustive match or fixture that constructs/inspects DraftOutcome and update as needed.

Do NOT change the engine-side semantics or the message the engine writes to the ticket .status note; this feature is purely the presentation/wiring at each surface. Keep each surface's message consistent in meaning with a8's requirement.

After wiring, the full workspace must build and all tests pass, and formatting must be clean. Add or extend a surface-level test if a surface already has outcome-rendering tests (e.g. slack tickets tests); otherwise the compile-plus-existing-suite is the gate.

Done when:
- Every match on DraftOutcome in crates/cli, crates/slack, and crates/server has an explicit PlanAsProse arm; `grep -rn "DraftOutcome::PlanAsProse" crates` lists the cli, slack, and server sites.
- Each surface's PlanAsProse rendering conveys that the orchestrator emitted a plan as prose and that the user should re-run the draft — it does not claim success or that a mission was queued.
- The slack tests' DraftOutcomeKind test double handles PlanAsProse and the slack test suite compiles and passes.
- cargo test --workspace passes.
- cargo fmt --all --check reports no changes.

