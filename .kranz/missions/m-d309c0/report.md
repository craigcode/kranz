# Mission report — m-d309c0

**Goal:** Make the engine draft core detect a plan-shaped NotReady reply and either recover it through the plan channel or fail honestly — never filing plan JSON as questions — and bound the size of anything appended to a ticket's needs-context section.

Branch `kranz/mission-m-d309c0` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 2h 16m 39s
**Tokens:** 25872 in / 82731 out / 7593071 cache read / 479121 cache write
**Cost:** $20.28 actual vs $7.12–$35.62 estimated (expected $14.25)

## What shipped

### Milestone 1 — Needs-context appends are size-bounded ✅

- ✅ **Cap the size of Ticket::append_needs_context** — 1 run
  - `2a02bcf` [f-1-1] cap size of Ticket::append_needs_context

### Milestone 2 — The draft core never files a plan-as-prose reply as questions ✅

- ✅ **Detect plan-as-prose in drive_draft: bounded plan-channel retry, else honest PlanAsProse outcome** — 1 run
  - `d7ac98f` [f-2-1] detect plan-as-prose in drive_draft, bounded retry then honest PlanAsProse
- ✅ **Wire DraftOutcome::PlanAsProse through all consumer surfaces** — 2 runs, 1 respawn
  - `5a11991` [f-2-2] cargo fmt --all to clear a7 fmt drift

## Validation history

### ms-1 round 1 — Needs-context appends are size-bounded

- [critical] a1/a2/a3 (plan_as_prose contract items) — cargo test -p kranz-engine --test draft_test plan_as_prose ran 0 matching tests (3 filtered out); `cargo test -p kranz-engine --test draft_test -- --list` shows only: not_ready_reply_appends_needs_con… [truncated]
- [critical] a8 (honest/actionable plan-as-prose user message) — Cannot be judged: no plan-as-prose surfaced message exists anywhere in CLI, Slack, or REST/host code (grep for PlanAsProse/plan_as_prose across crates/server, crates/slack, crates/cli returned nothing… [truncated]
- [minor] f-1-1 / a4 (Ticket::append_needs_context bounding) — cargo test -p kranz-engine --test ticket_queue_test append_needs_context: 5 passed (append_needs_context_truncates_multibyte_on_char_boundary, append_needs_context_truncates_long_question, append_need… [truncated]

Disposition: waived.
- a1/a2/a3 (plan_as_prose contract items): Out of ms-1 scope: these are ms-2's bar, already planned as pending features f-2-1 (detection/retry/PlanAsProse outcome) and f-2-2 (surface wiring) — not yet executed, so their absence is expected, not a defect.
- a8 (honest/actionable plan-as-prose message): Out of ms-1 scope: depends on the PlanAsProse outcome built in f-2-1 and surfaced in f-2-2 (both pending); a8 becomes judgeable after ms-2 runs.
- f-1-1 / a4 (append_needs_context bounding): No fix needed — finding confirms a4 is fully implemented and passing (5 tests, workspace green, fmt clean); this is exactly ms-1's deliverable.

### ms-2 round 1 — The draft core never files a plan-as-prose reply as questions

No findings.

## Contract outcomes

- ✅ **[a1]** A plan-shaped NotReady reply (contains "validationContract" and "milestones") is never written into the ticket .md body: after driving a draft whose orchestrator emits the plan as prose on every turn, the ticket file contains no "validationContract" text. *(command: `cargo test -p kranz-engine --test draft_test plan_as_prose`)*
- ✅ **[a2]** When a plan-shaped NotReady is followed by a parseable plan on the bounded plan-channel retry, the draft core approves the plan (outcome ParkedForReview / Enqueued), not NeedsContext. *(command: `cargo test -p kranz-engine --test draft_test plan_as_prose`)*
- ✅ **[a3]** When the bounded retry still returns prose, drive_draft returns DraftOutcome::PlanAsProse, sets the ticket to NeedsContext via a short .status note, and writes nothing to the ticket .md body. *(command: `cargo test -p kranz-engine --test draft_test plan_as_prose`)*
- ✅ **[a4]** Ticket::append_needs_context bounds its output: each question is truncated to at most 500 characters with a truncation marker, and at most 20 questions are written with an overflow marker line for the remainder — a multi-KB input never produces a multi-KB ticket section. *(command: `cargo test -p kranz-engine --test ticket_queue_test append_needs_context`)*
- ✅ **[a5]** A genuine (non-plan-shaped) NotReady reply still becomes NeedsContext with the orchestrator's questions appended, unchanged from prior behaviour. *(command: `cargo test -p kranz-engine --test draft_test`)*
- ✅ **[a6]** The whole workspace compiles and all tests pass, including every DraftOutcome consumer surface handling the new PlanAsProse variant. *(command: `cargo test --workspace`)*
- ✅ **[a7]** All code is formatted. *(command: `cargo fmt --all --check`)*
- ✅ **[a8]** The 'plan emitted as prose' message surfaced to the user (CLI, Slack, and REST/host) is honest and actionable — it states the orchestrator produced a plan but sent it as prose and tells the user to re-run the draft. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
