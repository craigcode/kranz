# Mission report — m-b397e4

**Goal:** Hoist the non-interactive draft loop into the engine/MissionHost so any surface can draft a ticket, expose the backlog over REST (list, show, draft, approve), and add a blocked-by dependency primitive with approve-gating, work-time recheck, and cycle detection.

Branch `kranz/mission-m-b397e4` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 1h 58m 04s
**Tokens:** 86495 in / 271208 out / 35513804 cache read / 1428827 cache write
**Cost:** $172.23 actual vs $15.30–$76.50 estimated (expected $30.60)

## What shipped

### Milestone 1 — Draft loop hoisted into the engine/MissionHost ✅

- ✅ **Extract the non-interactive draft core into kranz-engine** — 2 runs, 1 respawn
  - `d3313d2` [f-1-1] surface seed reply and approved plan from drive_draft
  - `c9e18ad` [f-1-1] checkpoint (engine commit)
- ✅ **Add MissionHost::draft entry point** — 1 run
  - `e0f887e` [f-1-2] add MissionHost::draft entry point
- ✅ **Rewire CLI kranz draft as a thin byte-compatible caller** — 1 run
  - `e5b1756` [f-1-3] fix stdout/checkout-restore ordering in the hoisted cmd_draft

### Milestone 2 — blocked-by dependency primitive ✅

- ✅ **Parse blocked-by frontmatter and add engine blocker/cycle checks** — 2 runs, 1 respawn
  - `441aea7` [f-2-1] add blocked-by engine tests (parsing, satisfaction, cycle detection)
- ✅ **Gate kranz ticket approve on blockers and cycles** — 1 run
  - `5fa28a4` [f-2-2] gate kranz ticket approve on blocked-by cycles and unsatisfied blockers
- ✅ **Re-check blockers in the kranz work dispatcher** — 1 run
  - `8552fde` [f-2-3] re-check blockers at work time; skip entries whose blocker failed
- ✅ **Format the ms-2 test code so cargo fmt --all --check passes** *(fix)* — 1 run
  - `ec9576e` [ms-2-fix-1-1] cargo fmt backlog_test.rs

### Milestone 3 — REST backlog surface ✅

- ✅ **GET /api/tickets and GET /api/tickets/:slug** — 1 run
  - `7f916f3` [f-3-1] add GET /api/tickets and GET /api/tickets/:slug REST routes
- ✅ **POST /api/tickets/:slug/draft and POST /api/tickets/:slug/approve** — 1 run
  - `55d6c0f` [f-3-2] checkpoint (engine commit)
- ✅ **Document the REST ticket surface and blocked-by** — 1 run
  - `62963b8` [f-3-3] document REST ticket endpoints and blocked-by dependency primitive

## Validation history

### ms-1 round 1 — Draft loop hoisted into the engine/MissionHost

- [critical] a3 (blocked_by_approve) — cargo test -p kranz --test backlog_test blocked_by_approve => 'running 0 tests' / 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 30 filtered out'. No test matching the substring 'blocked… [truncated]
- [critical] a4 (blocked_by_cycle) — cargo test -p kranz-engine --test ticket_queue_test blocked_by_cycle => 'running 0 tests' / 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 25 filtered out'. No matching test exists in cr… [truncated]
- [critical] a5 (blocked_by) — cargo test -p kranz-engine --test ticket_queue_test blocked_by => 'running 0 tests' / 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 25 filtered out'. No matching test exists.
- [critical] a6 (work_skips_failed_blocker) — cargo test -p kranz --test backlog_test work_skips_failed_blocker => 'running 0 tests' / 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 30 filtered out'. No matching test exists in backl… [truncated]
- [critical] a7 (tickets_rest GET endpoints) — cargo test -p kranz-server --test tickets_rest => 'error: no test target named `tickets_rest` in `kranz-server` package' / 'help: a target with a similar name exists: `tickets_host`'. The test binary … [truncated]
- [critical] a8 (rest_ticket_mutations) — cargo test -p kranz-server --test tickets_rest rest_ticket_mutations => 'error: no test target named `tickets_rest` in `kranz-server` package' / 'help: a target with a similar name exists: `tickets_ho… [truncated]

Disposition: waived.
- a3 (blocked_by_approve): Owned by pending feature f-2-2 (Gate kranz ticket approve on blockers and cycles) in ms-2; not part of ms-1's draft-hoist scope. Will be implemented and validated at the ms-2 gate.
- a4 (blocked_by_cycle): Owned by pending feature f-2-1 (engine cycle-detection tests) in ms-2; out of scope for ms-1. Validated at the ms-2 gate.
- a5 (blocked_by): Owned by pending feature f-2-1 (blocked-by frontmatter parsing + satisfaction) in ms-2; out of scope for ms-1. Validated at the ms-2 gate.
- a6 (work_skips_failed_blocker): Owned by pending feature f-2-3 (work-dispatcher blocked-by recheck) in ms-2; out of scope for ms-1. Validated at the ms-2 gate.
- a7 (tickets_rest GET endpoints): Owned by pending feature f-3-1 (GET /api/tickets endpoints + crates/server/tests/tickets_rest.rs) in ms-3; out of scope for ms-1. Validated at the ms-3 gate.
- a8 (rest_ticket_mutations): Owned by pending feature f-3-2 (POST draft/approve, token-gated) in ms-3; out of scope for ms-1. Validated at the ms-3 gate.

### ms-2 round 1 — blocked-by dependency primitive

- [critical] a11 — Formatting is clean (cargo fmt --all --check) — `cargo fmt --all --check` exits 1. The milestone's new test file crates/cli/tests/backlog_test.rs is not rustfmt-clean at two sites: the `TicketCommand::Approve { slug, mission, force }` match arm aro… [truncated]

Disposition: 1 fix feature(s) created.

### ms-2 round 2 — blocked-by dependency primitive

No findings.

### ms-3 round 1 — REST backlog surface

No findings.

## Contract outcomes

- ✅ **[a1]** kranz draft behaves exactly as before with the core hoisted: the existing CLI draft/backlog test binary stays green (state transitions, NEEDS-CONTEXT append, draft_decision, --yes park-vs-enqueue). *(command: `cargo test -p kranz --test backlog_test 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a2]** A host-level test drafts a ticket end-to-end against the mock backend through the server-callable draft entry point (MissionHost::draft), producing the parked-for-review outcome and recorded ticket->mission link without spawning a real claude binary. *(command: `cargo test -p kranz-server --test tickets_host 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a3]** kranz ticket approve refuses a ticket whose blocked-by is unsatisfied with an honest message naming the unsatisfied blocker, --force overrides the unsatisfied-blocker gate, and approve succeeds once the blocker's mission is Complete. *(command: `cargo test -p kranz --test backlog_test blocked_by_approve 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a4]** Approve-time cycle detection (DFS over ticket files) rejects a blocked-by cycle with the cycle path included in the message, and a cycle is rejected even when --force is set. *(command: `cargo test -p kranz-engine --test ticket_queue_test blocked_by_cycle 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a5]** blocked-by frontmatter parses as a list of slugs, existing tickets (no blocked-by key) are unaffected, and a blocker is satisfied only when its recorded mission reached MissionStatus::Complete (approved/queued/failed/missing blockers all count as unsatisfied). *(command: `cargo test -p kranz-engine --test ticket_queue_test blocked_by 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a6]** The kranz work dispatcher re-checks a claimed entry's ticket blockers before running and skips-with-recorded-warning (finish_claim + a Failed ticket state carrying a skip note) when a blocker's mission Failed mid-drain, via a pure decision helper. *(command: `cargo test -p kranz --test backlog_test work_skips_failed_blocker 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a7]** GET /api/tickets lists the real backlog with slug, priority, state, title, and blocked-by; GET /api/tickets/:slug returns the full parsed ticket plus needs-context; a traversal/invalid slug on a ticket route returns 400. *(command: `cargo test -p kranz-server --test tickets_rest 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a8]** POST /api/tickets/:slug/draft and POST /api/tickets/:slug/approve are mutation-token gated (401 without a valid x-kranz-token), and REST approve enforces the same blocked-by gate as the CLI with {"force":true} overriding it. *(command: `cargo test -p kranz-server --test tickets_rest rest_ticket_mutations 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a9]** The full workspace test suite passes. *(command: `cargo test --workspace 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a10]** Clippy is clean across the workspace with warnings denied. *(command: `cargo clippy --workspace --all-targets -- -D warnings`)*
- ✅ **[a11]** Formatting is clean. *(command: `cargo fmt --all --check`)*
- ✅ **[a12]** The blocked-by work-time recheck lives entirely in the kranz work dispatcher; crates/engine/src/queue.rs's claim machinery (claim_front/finish_claim/release_claim) and its (priority,seq) insertion-order scheduling are not modified to add eligibility/ordering logic (verify the diff against $KRANZ_BASE_SHA). *(agent judgement)*
- ✅ **[a13]** kranz draft remains byte-compatible with the pre-hoist CLI: identical stdout for park/needs-context/enqueue paths, identical ticket state transitions, identical '## Needs context (from orchestrator)' append format, and spend still bounded by the orchestrator budget cap; the CLI command is a thin caller of the hoisted core. *(agent judgement)*
- ✅ **[a14]** docs/protocol.md documents the four new ticket endpoints (correctly describing draft progress as observable over the existing WebSocket feed, not SSE) and docs/tickets.md documents blocked-by frontmatter usage, the approve-gate, --force, and the REST surface. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
