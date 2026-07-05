# Mission plan — m-b397e4

**Goal:** Hoist the non-interactive draft loop into the engine/MissionHost so any surface can draft a ticket, expose the backlog over REST (list, show, draft, approve), and add a blocked-by dependency primitive with approve-gating, work-time recheck, and cycle detection.

Branch `kranz/mission-m-b397e4` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** kranz draft behaves exactly as before with the core hoisted: the existing CLI draft/backlog test binary stays green (state transitions, NEEDS-CONTEXT append, draft_decision, --yes park-vs-enqueue). 
  `cargo test -p kranz --test backlog_test 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a2]** A host-level test drafts a ticket end-to-end against the mock backend through the server-callable draft entry point (MissionHost::draft), producing the parked-for-review outcome and recorded ticket->mission link without spawning a real claude binary. 
  `cargo test -p kranz-server --test tickets_host 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a3]** kranz ticket approve refuses a ticket whose blocked-by is unsatisfied with an honest message naming the unsatisfied blocker, --force overrides the unsatisfied-blocker gate, and approve succeeds once the blocker's mission is Complete. 
  `cargo test -p kranz --test backlog_test blocked_by_approve 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a4]** Approve-time cycle detection (DFS over ticket files) rejects a blocked-by cycle with the cycle path included in the message, and a cycle is rejected even when --force is set. 
  `cargo test -p kranz-engine --test ticket_queue_test blocked_by_cycle 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a5]** blocked-by frontmatter parses as a list of slugs, existing tickets (no blocked-by key) are unaffected, and a blocker is satisfied only when its recorded mission reached MissionStatus::Complete (approved/queued/failed/missing blockers all count as unsatisfied). 
  `cargo test -p kranz-engine --test ticket_queue_test blocked_by 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a6]** The kranz work dispatcher re-checks a claimed entry's ticket blockers before running and skips-with-recorded-warning (finish_claim + a Failed ticket state carrying a skip note) when a blocker's mission Failed mid-drain, via a pure decision helper. 
  `cargo test -p kranz --test backlog_test work_skips_failed_blocker 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a7]** GET /api/tickets lists the real backlog with slug, priority, state, title, and blocked-by; GET /api/tickets/:slug returns the full parsed ticket plus needs-context; a traversal/invalid slug on a ticket route returns 400. 
  `cargo test -p kranz-server --test tickets_rest 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a8]** POST /api/tickets/:slug/draft and POST /api/tickets/:slug/approve are mutation-token gated (401 without a valid x-kranz-token), and REST approve enforces the same blocked-by gate as the CLI with {"force":true} overriding it. 
  `cargo test -p kranz-server --test tickets_rest rest_ticket_mutations 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a9]** The full workspace test suite passes. 
  `cargo test --workspace 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a10]** Clippy is clean across the workspace with warnings denied. 
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[a11]** Formatting is clean. 
  `cargo fmt --all --check`
- **[a12]** The blocked-by work-time recheck lives entirely in the kranz work dispatcher; crates/engine/src/queue.rs's claim machinery (claim_front/finish_claim/release_claim) and its (priority,seq) insertion-order scheduling are not modified to add eligibility/ordering logic (verify the diff against $KRANZ_BASE_SHA). *(agent judgement)*
- **[a13]** kranz draft remains byte-compatible with the pre-hoist CLI: identical stdout for park/needs-context/enqueue paths, identical ticket state transitions, identical '## Needs context (from orchestrator)' append format, and spend still bounded by the orchestrator budget cap; the CLI command is a thin caller of the hoisted core. *(agent judgement)*
- **[a14]** docs/protocol.md documents the four new ticket endpoints (correctly describing draft progress as observable over the existing WebSocket feed, not SSE) and docs/tickets.md documents blocked-by frontmatter usage, the approve-gate, --force, and the REST surface. *(agent judgement)*

## Milestone 1 — Draft loop hoisted into the engine/MissionHost

### 1.1 Extract the non-interactive draft core into kranz-engine

Move the sequencing core of the CLI's non-interactive draft loop out of crates/cli/src/backlog.rs (cmd_draft, lines ~438-539) into a backend-agnostic, testable core in kranz-engine. Do NOT change the ticket state machine, the NEEDS-CONTEXT append format, or the budget mechanism — only relocate the glue.

Create (in kranz-engine, e.g. a new module crates/engine/src/draft.rs or on MissionEngine): a `DraftOutcome` type capturing the terminal result of a draft — an enum with variants roughly `ParkedForReview { mission_id, mission_branch }`, `Enqueued { mission_id }`, and `NeedsContext { mission_id, questions: Vec<String> }`. Add a function/method that, given an already-constructed `MissionEngine` (holding a backend) plus the ticket slug and a `then_enqueue: bool`, drives the existing engine primitives in the existing order: `write_state(Drafting)` -> `record_mission` -> `planning_turn(ticket.mission_goal())` -> capture seed reply -> `request_plan()` -> branch on the result. On `PlanRequest::NotReady(text)` split the prose into questions with the existing `split_questions`/`strip_bullet` logic (move these pure helpers, and the `draft_decision` decision helper and `DraftDecision` type, from crates/cli/src/backlog.rs into the engine) and call `Ticket::append_needs_context`, returning `NeedsContext`. On `PlanRequest::Ready(plan)` call `engine.approve_plan(plan)`, then set ticket state to `Queued` (+ `queue::enqueue`) when `then_enqueue`, else `Review`, returning `Enqueued`/`ParkedForReview`. On a seed or plan-request error, reset the ticket to `New` and propagate the error (preserving today's rollback behavior). The core must NOT print anything and must NOT do operator-checkout restoration — those stay in the CLI caller. Use kranz-engine's own error type (kranz_engine::error::{EngineError, Result}); the CLI adapts at its boundary.

The core must be constructible with an injected backend so it can be tested without a real claude binary. Ensure `kranz_engine::backend_mock::MockBackend` is publicly exported from the engine crate (make it `pub` / re-export at the crate root) so both engine and server tests can inject it.

Add engine-level tests (in crates/engine/tests/, e.g. a new draft_test.rs or extending ticket_queue_test.rs) that drive the core against `MockBackend` with scripted orchestrator turns: (1) a Ready plan parks the ticket in Review with the mission recorded; (2) a Ready plan with then_enqueue=true sets Queued and enqueues; (3) a NotReady reply appends '## Needs context (from orchestrator)' with the split questions and flips state to NeedsContext. Follow the existing MockBackend wiring in crates/engine/tests/mission_test.rs (orch_script/MockBackend::with_scripts, MissionEngine::create).

Done when:
- An engine test drives the draft core against MockBackend and asserts a Ready plan parks the ticket in Review with the ticket->mission link recorded (via Ticket::mission_for)
- An engine test asserts a NotReady orchestrator reply appends the exact '## Needs context (from orchestrator)' heading + '- ' bulleted questions to the ticket .md and sets TicketState::NeedsContext
- An engine test asserts then_enqueue=true yields TicketState::Queued plus a queue entry, while then_enqueue=false yields TicketState::Review with no queue entry
- MockBackend is publicly reachable from outside the engine crate (used by the test without a real claude binary)
- draft_decision, split_questions, strip_bullet and DraftDecision are relocated into kranz-engine and the pure-helper behavior is unchanged

### 1.2 Add MissionHost::draft entry point

Give kranz_server::MissionHost (crates/server/src/host.rs) a draft entry point that any surface can call, wrapping the kranz-engine draft core from the previous feature so the planning mission it creates is registered in the host and observable over that mission's existing WebSocket feed (GET /api/missions/:id/ws), which tails the mission's events.jsonl.

Add `pub async fn draft(&self, slug: &str, then_enqueue: bool) -> Result<DraftOutcome, ApiError>` on MissionHost. It must: validate the slug with `kranz_engine::ticket::Ticket::ensure_valid_slug` (map failure to ApiError::bad_request -> 400); load the ticket from `Ticket::tickets_dir(self.repo_root())` (missing ticket -> ApiError::not_found -> 404); build/obtain the backend via the host's existing lazy `backend()` accessor; create the mission through the host so it lands in the `missions` registry (reuse the create path / MissionEngine::create with the ticket's mission_goal and per-ticket budget override analogous to config_for_ticket) so its lifecycle events stream over the WS; then run the engine draft core to completion and return the DraftOutcome. Map engine errors to ApiError via the existing `From<EngineError>` impl. Do not perform any operator-checkout restoration (headless server has no operator checkout). Ensure the hosted engine/lock is dropped/released appropriately when the draft finishes (mirror how create/run_to_end manage the registry entry) so the mission remains resumable/observable afterward.

Expose DraftOutcome to the server crate (serialize it to JSON where needed for a later REST handler; if DraftOutcome lacks Serialize, add a small JSON projection helper following the estimate_json pattern in host.rs).

Add a server-crate integration test file crates/server/tests/tickets_host.rs that constructs a MissionHost via `MissionHost::with_backend(repo_root, Arc::new(MockBackend::with_scripts(...)))` over a tempdir repo containing a ticket .md, calls `host.draft(slug, false).await`, and asserts the returned DraftOutcome is ParkedForReview, the ticket state is Review, and the ticket->mission link is recorded. Use the same MockBackend scripting approach as the engine tests.

Done when:
- MissionHost exposes `pub async fn draft(&self, slug, then_enqueue) -> Result<DraftOutcome, ApiError>`
- An invalid/traversal slug passed to host.draft returns an ApiError mapping to HTTP 400; a missing ticket returns 404
- crates/server/tests/tickets_host.rs drafts a ticket end-to-end against an injected MockBackend and asserts ParkedForReview + TicketState::Review + recorded mission link, with no real claude binary spawned
- The planning mission created by host.draft is registered in the host so its events are observable over GET /api/missions/:id/ws

### 1.3 Rewire CLI kranz draft as a thin byte-compatible caller

Rewrite crates/cli/src/backlog.rs `cmd_draft` so it is a thin caller of the hoisted kranz-engine draft core (from the first feature of this milestone) while producing byte-identical user-facing behavior to today. The CLI keeps ownership of all presentation and interactive/operator concerns: constructing the backend via crate::commands::build_backend, loading config via load_config, applying config_for_ticket, printing the same stdout lines it prints today (the 'drafting…' progress line, seed reply, plan render via crate::output::render_plan, and the park/enqueue/needs-context messages), performing restore_draft_checkout after approve to restore the operator's checkout, mapping errors with augment_limit_hint, and returning the same process exit codes. The --yes flag maps to then_enqueue.

Concretely: cmd_draft should build the backend + config, construct a MissionEngine (as today), then delegate the seed->plan->decision->side-effects sequence to the engine core, receive a DraftOutcome, and render the existing stdout for each outcome variant from that structured result. The '## Needs context (from orchestrator)' append and all ticket state transitions now happen inside the core — the CLI must not duplicate them. Preserve the New-rollback-on-error behavior surfaced to the user identically.

The existing tests in crates/cli/tests/backlog_test.rs must stay green unchanged (draft_ready_without_yes_parks_for_review, draft_ready_with_yes_enqueues, draft_not_ready_needs_context_splits_questions, draft_not_ready_single_line_becomes_one_question, state_transitions_*, parses_draft_and_draft_yes, etc.). If draft_decision/split_questions moved to the engine, update the test imports to the new path but keep the assertions identical. Do not weaken or delete any existing assertion.

Done when:
- cargo test -p kranz --test backlog_test passes with all pre-existing draft/backlog assertions intact (imports may be re-pointed to the engine, assertions unchanged)
- kranz draft <slug> stdout for park-for-review, --yes enqueue, and needs-context is byte-identical to the pre-hoist output
- cmd_draft no longer contains the draft sequencing/state-transition logic (it delegates to the engine core) and still performs operator-checkout restoration and exit-code mapping CLI-side
- The NEEDS-CONTEXT append and all ticket state writes occur exactly once, inside the hoisted core, not duplicated in the CLI


## Milestone 2 — blocked-by dependency primitive

### 2.1 Parse blocked-by frontmatter and add engine blocker/cycle checks

Add the blocked-by dependency primitive to kranz-engine's ticket model and provide the shared checks that both the CLI and REST approve paths will call.

In crates/engine/src/ticket.rs: add a `pub blocked_by: Vec<String>` field to the `Ticket` struct and populate it in `Ticket::parse` by adding a match arm for the normalized key `blocked-by` (also accept `blockedby`) that reads `value.list()`. The existing hand-rolled frontmatter parser already parses `[a, b, c]` bracket lists into FrontValue::List and ignores unknown keys, so tickets without the key must remain byte-for-byte unaffected (blocked_by defaults to empty). Validate each referenced slug with `Ticket::ensure_valid_slug` when consuming blockers (never join an unvalidated slug to a path).

Add two engine functions (e.g. in ticket.rs or a new deps.rs):
1. `unsatisfied_blockers(repo_root, slug) -> Result<Vec<String>>`: loads the ticket, and for each blocker slug returns it as unsatisfied unless the blocker's recorded mission reached Complete. Satisfaction is authoritative on mission status: resolve `Ticket::mission_for(repo_root, blocker)` -> load that mission's state and require `MissionStatus::Complete`. A blocker with no ticket file, no recorded mission, or any non-Complete status (Approved/Queued/Running/Blocked/Failed/Abandoned) counts as UNSATISFIED. (If loading mission state requires a helper currently only in the CLI, add an engine-level equivalent or move the minimal loader into the engine.)
2. `detect_cycle(repo_root, slug) -> Result<Option<Vec<String>>>`: DFS over the blocked-by edges across ticket files starting from slug; if a cycle is reachable, return Some(path) where the path lists the slugs forming the cycle in order (e.g. [a, b, a]); else None. Missing ticket files terminate that branch (they cannot extend a cycle).

Add engine tests in crates/engine/tests/ticket_queue_test.rs: (name them so a `blocked_by` filter and a `blocked_by_cycle` filter each match at least one) — parsing a blocked-by list; existing ticket without the key still parses unchanged; unsatisfied_blockers returns a blocker whose mission is not Complete and returns empty once it is Complete; detect_cycle returns the cycle path for a<->b and a->b->c->a, and None for an acyclic chain.

Done when:
- Ticket gains a blocked_by: Vec<String> field parsed from `blocked-by: [..]` frontmatter; tickets without the key parse identically to before (blocked_by empty)
- unsatisfied_blockers returns a blocker as unsatisfied for every non-Complete/missing state and returns empty only when the blocker's recorded mission is MissionStatus::Complete
- detect_cycle returns Some(path) naming the slugs in the cycle for both a direct a<->b cycle and a longer a->b->c->a cycle, and None for an acyclic chain
- Every referenced blocker slug is validated with ensure_valid_slug before any filesystem join
- Engine tests named to match `blocked_by` and `blocked_by_cycle` filters cover parsing, satisfaction, and cycle detection

### 2.2 Gate kranz ticket approve on blockers and cycles

Wire the shared engine checks (unsatisfied_blockers, detect_cycle from the previous feature) into the CLI approve path `cmd_ticket_approve` in crates/cli/src/backlog.rs so approval respects blocked-by, and expose a `--force` flag.

Behavior: on `kranz ticket approve <slug>`, before enqueuing/approving: (1) run detect_cycle; if a cycle is found, refuse with an honest error message that includes the cycle path (e.g. 'blocked-by cycle: a -> b -> a'), and refuse EVEN when --force is set (a cycle is a structural data error, not an overridable gate). (2) run unsatisfied_blockers; if non-empty and --force is NOT set, refuse with a message naming the unsatisfied blocker(s) (e.g. 'cannot approve <slug>: blocked by <blocker> (its mission is not Complete)'); when --force IS set, proceed despite unsatisfied blockers. (3) once no cycle and (no unsatisfied blockers or --force), approve exactly as today (Review -> Queued + queue entry).

Add the `--force` flag to the `kranz ticket approve` clap command (crates/cli/src/cli.rs) and thread it through commands.rs dispatch into cmd_ticket_approve. Keep the existing REVIEW-state precondition and all current approve behavior otherwise.

Add CLI tests in crates/cli/tests/backlog_test.rs, named so a `blocked_by_approve` filter matches, using tempdir repos with ticket .md files and forged .status files (and, where needed, a minimal folded mission state) to simulate blocker mission states: approve refuses and names the blocker when the blocker's mission is not Complete; approve with --force succeeds despite the unsatisfied blocker; approve succeeds without --force once the blocker's mission is Complete; approve refuses on a cycle with the cycle path in the message even under --force. Mirror the tempdir/state approach used by the existing ticket_approve_* tests.

Done when:
- kranz ticket approve refuses an unsatisfied-blocked ticket with a message naming the unsatisfied blocker(s)
- kranz ticket approve --force overrides an unsatisfied-blocker refusal and approves
- kranz ticket approve succeeds without --force once the blocker's mission reaches Complete
- A blocked-by cycle is rejected at approve time with the cycle path in the message, and is rejected even with --force
- Tests matching a `blocked_by_approve` filter cover refuse/force/complete/cycle and pass

### 2.3 Re-check blockers in the kranz work dispatcher

Add a work-time blocker re-check to the `cmd_work` dispatcher in crates/cli/src/backlog.rs WITHOUT modifying crates/engine/src/queue.rs's claim machinery or its (priority,seq) insertion-order scheduling.

Insertion point: in the WorkAction::Run arm (around backlog.rs:703-724), AFTER `queue::claim_front` succeeds and the claimed entry's `ticket_slug` is known, but BEFORE `Ticket::write_state(Running)` / `drive_mission`. For a claimed entry that carries a ticket_slug, re-evaluate the ticket's blockers using the engine's unsatisfied_blockers check. If a blocker is unsatisfied specifically because its mission FAILED (i.e. reached a terminal non-Complete state such as Failed/Abandoned/Blocked) mid-drain after batch-approval, SKIP this entry: retire the claim with `queue::finish_claim(claim)` (do NOT release_claim — that would re-claim the same blocked entry forever and hot-loop), record the warning durably by writing the ticket state to Failed with an explanatory note via `Ticket::write_state(&repo, slug, TicketState::Failed, Some("skipped: blocked-by <blocker> failed".into()))`, print a warning line to stderr in the existing `eprintln!("warning: ...")` style naming the blocker, then `continue` the drain loop. Do not skip for blockers that are merely not-yet-Complete-but-still-viable if that contradicts existing behavior — the required, tested behavior is the failed-blocker skip; keep the scope to what the acceptance names (a blocker's mission Failed after batch-approval).

Factor the skip decision into a PURE helper alongside next_work_action (e.g. `fn work_skip_for_failed_blocker(...) -> Option<String>` returning the offending blocker slug) so it is unit-testable without running the async dispatcher, mirroring the existing pure-helper test pattern (next_work_action / work_action_* tests). queue.rs must not gain any eligibility/ordering logic.

Add a CLI test in crates/cli/tests/backlog_test.rs named so a `work_skips_failed_blocker` filter matches: stage a tempdir repo with a queued ticket whose blocker ticket's .status is Failed, drive the pure skip-decision helper (and/or a thin seam), and assert it reports the failed blocker so the dispatcher would finish_claim + mark the ticket Failed-with-note + warn.

Done when:
- The blocker re-check lives in cmd_work's WorkAction::Run arm between claim_front and drive_mission; crates/engine/src/queue.rs is unchanged in its claim/ordering logic
- A queued entry whose blocker's mission Failed is skipped: the claim is retired via finish_claim, the ticket is marked Failed with a skip note naming the blocker, and a warning is printed to stderr
- The skip decision is a pure, unit-tested helper (no async dispatcher needed to test it)
- A test matching `work_skips_failed_blocker` passes and asserts the failed-blocker skip is detected


## Milestone 3 — REST backlog surface

### 3.1 GET /api/tickets and GET /api/tickets/:slug

Add read endpoints for the backlog to kranz-server. Register two routes in `router_with_shared_host` (crates/server/src/lib.rs:124-169): `GET /api/tickets` and `GET /api/tickets/:slug` (use the crate's axum brace syntax, `/api/tickets/{slug}`). GETs are tokenless by design.

Handlers (place alongside existing read handlers, e.g. in rest.rs or a new tickets.rs, taking `State(server): State<Arc<ServerState>>`):
- `GET /api/tickets`: call `kranz_engine::ticket::Ticket::list(server.host.repo_root())`, and for each ticket build a JSON object with camelCase fields: slug, priority, state (from `Ticket::read_state`, serialized kebab-case like elsewhere), title, and blockedBy (the parsed blocked_by list). Return a JSON array. Ticket has no Serialize derive — build the JSON explicitly (follow the estimate_json manual-projection pattern in host.rs), or add a dedicated projection; do not leak internal-only fields.
- `GET /api/tickets/:slug`: validate the slug at the route boundary with `Ticket::ensure_valid_slug` -> on failure return `ApiError::bad_request` (400). Load the ticket (missing -> `ApiError::not_found` -> 404) and return the full parsed ticket as JSON (slug, title, priority, schedule, blockedBy, goal, context, scopingAnswers, acceptanceHints, state) plus a needsContext field containing the orchestrator questions currently appended to the ticket (reuse/port the needs_context_block extraction logic from crates/cli/src/backlog.rs:188-215).

Add a server integration test crates/server/tests/tickets_rest.rs (using the tokenless router wrapper over a tempdir repo with a couple of ticket .md + .status files) asserting: GET /api/tickets returns the tickets with slug/priority/state/title/blockedBy; GET /api/tickets/:slug returns the full ticket incl. needsContext; and a traversal/invalid slug (e.g. `..%2f` decoded, or an invalid slug value) on the :slug route returns 400. Follow the existing server test harness patterns (router construction, request building) used by the other server tests.

Done when:
- GET /api/tickets returns a JSON array with slug, priority, state, title, and blockedBy for each real backlog ticket
- GET /api/tickets/:slug returns the full parsed ticket plus a needsContext field, and 404 for a missing ticket
- An invalid/traversal slug on GET /api/tickets/:slug returns HTTP 400 via Ticket::ensure_valid_slug at the boundary
- cargo test -p kranz-server --test tickets_rest passes and includes the traversal-400 assertion

### 3.2 POST /api/tickets/:slug/draft and POST /api/tickets/:slug/approve

Add the two mutating ticket endpoints to kranz-server, registered in `router_with_shared_host` (they are automatically token-gated because the middleware protects every POST /api/... by path prefix — no extra wiring). Validate the slug with `Ticket::ensure_valid_slug` at the boundary (400 on failure) in both handlers.

- `POST /api/tickets/:slug/draft`: long-running. Mirror `POST /api/missions/:id/start` (host.rs start_mission): spawn the draft as a background task that calls `server.host.draft(slug, false)` (park-for-review; --yes is CLI-only) and return `202 {"missionId":"m-…"}` immediately using the mission id of the planning mission the draft creates, so progress is observable over that mission's existing WebSocket feed (GET /api/missions/:id/ws) and the final outcome (Review vs NeedsContext) is read back via GET /api/tickets/:slug. Ensure the mission id is available to return before/at spawn time (create the hosted mission synchronously, then spawn the planning turns), so the 202 body carries a real missionId. On a bad slug or missing ticket, return 400/404 synchronously (do not spawn).
- `POST /api/tickets/:slug/approve`: parse an optional JSON body `{"force": bool}` (empty body -> force=false; use the existing parse_body helper). Call the SAME shared approve logic the CLI uses — the engine's detect_cycle + unsatisfied_blockers checks plus the approve/enqueue side effects — factoring the approve core so CLI and REST share it (avoid duplicating the gate). Refuse with `ApiError::conflict` (409) and an honest message naming the unsatisfied blocker when blocked and force is false; refuse with 409 and the cycle path on a cycle even when force is true; on success approve+enqueue and return `200 {"approved":true}` (or the queued mission id). Map the not-in-Review precondition and engine errors through the existing ApiError/From<EngineError> mapping.

Extend crates/server/tests/tickets_rest.rs with tests named so a `rest_ticket_mutations` filter matches: POST draft and POST approve without a valid x-kranz-token return 401 (token-gated); POST approve on a ticket with an unsatisfied blocker returns 409 naming the blocker; POST approve with {"force":true} overrides the unsatisfied blocker; POST approve on a cycle returns 409 with the cycle path even with force. Use an injected MockBackend where a draft would otherwise need a real backend, and the token-carrying router wrapper for the auth assertions.

Done when:
- POST /api/tickets/:slug/draft spawns the draft and returns 202 with the real planning missionId, observable over that mission's WebSocket feed; bad slug -> 400, missing ticket -> 404 synchronously
- POST /api/tickets/:slug/approve enforces the same blocked-by gate as the CLI, with {"force":true} overriding an unsatisfied blocker but never a cycle
- Both POST ticket routes require a valid x-kranz-token (401 without it); GET ticket routes remain tokenless
- The CLI and REST approve paths share one approve-gate implementation (no duplicated blocker/cycle logic)
- Tests matching `rest_ticket_mutations` cover token-gating, blocker-409, force-override, and cycle-409 and pass

### 3.3 Document the REST ticket surface and blocked-by

Update the docs to reflect the new behavior. This is a documentation-only feature — do not change code.

In docs/protocol.md: add the four new ticket endpoints to the REST section — `GET /api/tickets` (list: slug, priority, state, title, blockedBy), `GET /api/tickets/:slug` (full parsed ticket + needsContext), `POST /api/tickets/:slug/draft` (long-running; returns 202 {missionId}; note progress is observable over the existing per-mission WebSocket feed GET /api/missions/:id/ws — explicitly NOT an SSE feed, since the server has no SSE), and `POST /api/tickets/:slug/approve` (body {"force":bool}; 409 naming the unsatisfied blocker, or the cycle path; force overrides an unsatisfied blocker but not a cycle). State that the two POSTs are mutation-token gated like every other POST and that slugs are validated at the route boundary (bad slug -> 400).

In docs/tickets.md: document the `blocked-by: [slug, ...]` frontmatter key (semantics: a blocker is satisfied only when its mission reaches Complete; approved/queued/failed/missing blockers all block), the approve-gate and its honest blocker-naming message, `kranz ticket approve --force` (overrides unsatisfied blockers, never a cycle), approve-time cycle detection with the cycle path, the `kranz work` skip-with-warning when a blocker's mission Failed mid-drain, and the new REST backlog surface as an alternative to the CLI pipeline. Keep the doc style consistent with the surrounding sections.

Done when:
- docs/protocol.md documents all four ticket endpoints with methods, paths, request/response shapes, token-gating, and the 400/404/409 behaviors
- docs/protocol.md describes draft progress as observable over the existing WebSocket feed and does not claim an SSE feed exists
- docs/tickets.md documents blocked-by frontmatter semantics, the approve-gate message, --force, cycle detection with path, and the work-time skip-with-warning
- docs/tickets.md documents the REST backlog surface alongside the existing CLI pipeline

