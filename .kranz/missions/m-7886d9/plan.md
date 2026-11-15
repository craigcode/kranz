# Mission plan — m-7886d9

**Goal:** Make the hint printed for a parked draft name a route that accepts it: add answers to the ticket, then `kranz abandon <id>` and `kranz draft <slug>`. Document that route in docs/tickets.md, and record in-place reshaping as an OPEN operator decision.

Branch `kranz/mission-m-7886d9` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Authority granted on approval

Approving this plan grants the following to the mission's sessions with no further prompt.

**Write paths** (7; glob patterns the mission may change):
- `crates/cli/src/backlog.rs`
- `crates/cli/tests/**`
- `crates/engine/tests/draft_test.rs`
- `docs/tickets.md`
- `docs/scoping/parked-draft-reshape.md`
- `docs/knowledge/validation/gates.md`
- `CHANGELOG.md`

## Cost estimate

Estimated **$3.88 – $19.40** (expected ~$8.94). Rough estimate — live usage is authoritative; based on 49 completed mission(s).

Context-fit check (corpus p90 over 56 plan(s): 4213 chars / 23 files):
- **f-1-1** (Correct the parked-draft hint, document the route in docs/tickets.md, and prove the route works) — spec is 5542 chars (corpus p90: 4213); spec points at 25 files (corpus p90: 23)

## Considered alternatives

**Chosen approach:** Correct the hint to name the abandon + redraft route that works today, prove the route with tests against a real parked draft, document it in docs/tickets.md, and record in-place reshaping as an OPEN operator decision. This is the smaller change the scoping answers prefer, and it needs no contract file change.

Rejected shapes:
- **Make `kranz plan --mission` accept a parked draft by returning an approved mission to Planning** — Needs a new reverse status transition in the reducer/event contract and pre-empts an operator decision the ticket explicitly reserves
- **Add a `kranz redraft <slug>` verb that abandons and redrafts in one step** — New CLI surface and tests across surfaces for something the operator has not chosen; recorded as option 2 in the decision note instead
- **Also add a reshape route to the dashboard ticket page and the Slack parked-draft message** — Neither carries the wrong wording; it widens the touch set to the dashboard bundle and Slack code for a gap that belongs to the open decision

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The hint printed for a parked draft names `kranz ticket approve <slug>` to queue it, and names the ticket file, `kranz abandon <id>` and `kranz draft <slug>` as the way to reshape it. It names no `kranz plan` command. Every backticked command in the hint, rendered with the placeholders `<slug>` and `<id>`, appears word for word in docs/tickets.md.
  `cargo test -p kranz --test backlog_test parked_draft_reshape 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a2]** The CLI draft code no longer offers `kranz plan --mission` as the way to reshape a parked draft.
  `! grep -n 'kranz plan --mission' crates/cli/src/backlog.rs`
- **[a3]** Running `kranz abandon <id>` against a real parked draft (mission approved, ticket in REVIEW) is not refused. It exits 0, the mission ends Abandoned, and the ticket ends FAILED.
  `cargo test -p kranz --test cli_test parked_draft_reshape 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a4]** Once a parked draft is abandoned, drafting the same ticket again is not refused. It parks a new draft under a different mission id, sets the ticket back to REVIEW, and records the new mission on the ticket.
  `cargo test -p kranz-engine --test draft_test parked_draft_reshape 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`
- **[a5]** docs/tickets.md has a section, with a heading that mentions reshaping, describing how to reshape a parked draft through `kranz abandon` and `kranz draft`.
  `grep -qiE '^#+ .*reshap' docs/tickets.md && grep -q 'kranz abandon <id>' docs/tickets.md && grep -q 'kranz draft <slug>' docs/tickets.md`
- **[a6]** Whether a parked draft should be reshapable in place is recorded as an OPEN, lettered operator decision in docs/scoping/parked-draft-reshape.md.
  `test -f docs/scoping/parked-draft-reshape.md && grep -qE '^#+ D-[A-Z]' docs/scoping/parked-draft-reshape.md && grep -q 'OPEN' docs/scoping/parked-draft-reshape.md`
- **[a7]** No contract file changed: the event, type and reducer definitions are unchanged.
  `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs crates/engine/src/reducer.rs`
- **[a8]** No file outside the declared surfaces changed, apart from Kranz's own .kranz/ bookkeeping.
  `test -z "$(git diff --name-only $KRANZ_BASE_SHA | grep -vE '^(\.kranz/|crates/cli/src/backlog\.rs$|crates/cli/tests/|crates/engine/tests/draft_test\.rs$|docs/tickets\.md$|docs/scoping/parked-draft-reshape\.md$|docs/knowledge/validation/gates\.md$|CHANGELOG\.md$)')"`
- **[a9]** The workspace is formatted and passes clippy with warnings denied.
  `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`
- **[a10]** The full workspace test suite passes.
  `out=$(cargo test --workspace 2>&1); rc=$?; printf '%s' "$out" | grep -qE 'result: ok\. [1-9][0-9]* passed' && [ $rc -eq 0 ]`
- **[a11]** The parked-draft hint, the docs/tickets.md section and the decision note agree on the route, with the same commands in the same order. The hint is actionable: it names the ticket file, says to add answers before redrafting, and does not suggest the old draft can be edited in place. The docs note that a draft made through `kranz serve` holds the mission lock while serve is running. *(agent judgement)*
## Contract lint

Each `check: command` assertion above was run once against the untouched base tree at approval time. Suspects are assertions that already pass (or could not reach a verdict) before this plan's work lands — a possible polarity/vacuity bug in the assertion itself. This never blocks approval.

author-bug suspects (already pass / no verdict on the untouched base): [a7] git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs crates/engine/src/reducer.rs; [a8] test -z "$(git diff --name-only $KRANZ_BASE_SHA | grep -vE '^(\.kranz/|crates/cli/src/backlog\.rs$|crates/cli/tests/|crates/engine/tests/draft_test\.rs$|docs/tickets\.md$|docs/scoping/parked-draft-reshape\.md$|docs/knowledge/validation/gates\.md$|CHANGELOG\.md$)')"; [a9] cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
base-expected-to-fail (benign): [a1] cargo test -p kranz --test backlog_test parked_draft_reshape 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'; [a2] ! grep -n 'kranz plan --mission' crates/cli/src/backlog.rs; [a3] cargo test -p kranz --test cli_test parked_draft_reshape 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'; [a4] cargo test -p kranz-engine --test draft_test parked_draft_reshape 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'; [a5] grep -qiE '^#+ .*reshap' docs/tickets.md && grep -q 'kranz abandon <id>' docs/tickets.md && grep -q 'kranz draft <slug>' docs/tickets.md; [a6] test -f docs/scoping/parked-draft-reshape.md && grep -qE '^#+ D-[A-Z]' docs/scoping/parked-draft-reshape.md && grep -q 'OPEN' docs/scoping/parked-draft-reshape.md; [a10] out=$(cargo test --workspace 2>&1); rc=$?; printf '%s' "$out" | grep -qE 'result: ok\. [1-9][0-9]* passed' && [ $rc -eq 0 ]

named contract gates (defect classes):
- vacuous-filter: PASS
- wrong-polarity: PASS
- passes-on-base: FAIL
  [a7] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs crates/engine/src/reducer.rs`
  [a8] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `test -z "$(git diff --name-only $KRANZ_BASE_SHA | grep -vE '^(\.kranz/|crates/cli/src/backlog\.rs$|crates/cli/tests/|crates/engine/tests/draft_test\.rs$|docs/tickets\.md$|docs/scoping/parked-draft-reshape\.md$|docs/knowledge/validation/gates\.md$|CHANGELOG\.md$)')"`
  [a9] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`
- env-sensitive: PASS


## Milestone 1 — A parked draft points at a route that works, and the route is documented

### 1.1 Correct the parked-draft hint, document the route in docs/tickets.md, and prove the route works

Context: `kranz draft <slug>` (cmd_draft in crates/cli/src/backlog.rs, around lines 507-630) drives a ticket through `kranz_engine::draft::drive_draft`. When the plan is ready and --yes is not passed, the mission is APPROVED (engine.approve_plan) and the ticket is set to REVIEW. This is a 'parked draft'. The `DraftOutcome::ParkedForReview` arm (around backlog.rs:618-627) currently prints:
  "plan committed on {mission_branch} for review; mission {mission_id} parked. Review it, then run `kranz ticket approve {slug}` to queue it (or `kranz plan --mission {mission_id}` to reshape)."
The `kranz plan --mission` route is wrong. cmd_plan (crates/cli/src/commands.rs around line 1141) refuses any mission not in Planning status. The route that DOES work: add the operator's answers to the ticket file, run `kranz abandon <id>`, then run `kranz draft <slug>`. Why it works: mission_catalog::abandon_mission (crates/engine/src/mission_catalog.rs:134) accepts any mission that is not terminal and reconciles the linked ticket to FAILED. drive_draft (crates/engine/src/draft.rs:169) does not check ticket state, and it calls Ticket::record_mission with the new mission id.

Do this:
1. In crates/cli/src/backlog.rs add a pure, pub function, e.g. `pub fn parked_for_review_hint(ticket_path: &Path, slug: &str, mission_id: &str, mission_branch: &str) -> String` (exact signature is your choice, but keep it pure: no I/O). It returns the parked-draft message. Use it from the ParkedForReview arm of cmd_draft (the ticket path is `Ticket::tickets_dir(&repo).join(format!("{slug}.md"))`, as the neighbouring NeedsContext arm already builds). Wording, matching the neighbouring arms' style: "plan committed on {branch} for review; mission {id} parked. Review it, then run `kranz ticket approve {slug}` to queue it. To reshape it instead, add your answers to {ticket path}, then run `kranz abandon {id}` and `kranz draft {slug}`." The string `kranz plan --mission` must no longer appear anywhere in backlog.rs.
2. In docs/tickets.md add a section whose heading contains the word 'Reshaping' (e.g. `### Reshaping a parked draft`). Put it near 'The pipeline: draft → review → approve → drain' section (around lines 349-385). It explains that a parked draft's mission is already approved, so `kranz plan --mission` and `kranz revise` cannot reshape it. The route is: add answers under `## Scoping answers` in the ticket, then `kranz abandon <id>` (the ticket goes to FAILED), then `kranz draft <slug>` (a new mission and a new parked plan; `kranz ticket approve <slug>` then picks the new mission). The section must contain these exact literals: `kranz abandon <id>`, `kranz draft <slug>` and `kranz ticket approve <slug>`. Add a short note: a draft made through `kranz serve` (dashboard, Slack or REST) stays attached to serve and holds the mission lock until approval, so `kranz abandon` from a terminal is refused while serve holds it. Keep the doc's existing tone. Change nothing else in the file.
3. Tests. Every test name must start with `parked_draft_reshape`; no existing test uses that prefix.
  a. crates/cli/tests/backlog_test.rs: test(s) that call the hint function and assert all of the following. It contains `kranz ticket approve <slug>`, `kranz abandon <id>`, `kranz draft <slug>` and the ticket path (with real values, e.g. slug 'my-fix' and id 'm-abc123'). It does NOT contain 'kranz plan'. Rendered with slug "<slug>" and mission id "<id>", every backtick-delimited span in the hint appears verbatim in docs/tickets.md, read via `concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/tickets.md")` or the equivalent. Note: the CLI crate's package name is `kranz` (lib `kranz_cli`), so run with `cargo test -p kranz --test backlog_test parked_draft_reshape`.
  b. crates/cli/tests/cli_test.rs: a test that creates a REAL parked draft in a tempdir git repo with a ticket file. Use kranz_engine's MockBackend + MissionEngine::create + kranz_engine::draft::drive_draft(…, false), following the pattern in crates/server/tests/tickets_host.rs and crates/engine/tests/draft_test.rs. Assert the outcome is ParkedForReview and the ticket state is Review. Then run the actual `kranz` binary's `abandon <id>` against that repo, using the existing binary harness in cli_test.rs (around line 25, env!("CARGO_BIN_EXE_kranz")) and whatever repo/cwd convention it uses. Assert exit success, the mission's folded status is Abandoned, and the ticket state is Failed. If MockBackend is not reachable from the cli crate's dev-dependencies, add it as a dev-dependency feature only if that needs no Cargo.toml change outside the touch set; otherwise construct the approved mission through the engine's public API and report what you did.
  c. crates/engine/tests/draft_test.rs: a test that drafts a ticket to ParkedForReview with MockBackend (reuse the file's existing scripting helpers). It drops the engine, calls kranz_engine::mission_catalog::abandon_mission, and asserts Ok with the ticket Failed. Then it drafts the SAME ticket again with a fresh engine and asserts ParkedForReview with a different mission id, the ticket back in Review, and the ticket's recorded mission equal to the new id.
Constraints: do not change crates/engine/src/** or any contract file (events.rs, types.rs, reducer.rs). Do not change Slack, server or dashboard code. Only touch crates/cli/src/backlog.rs, crates/cli/tests/**, crates/engine/tests/draft_test.rs and docs/tickets.md. Before reporting, run `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings`.

Done when:
- `cargo test -p kranz --test backlog_test parked_draft_reshape` runs at least one test and passes; the tests assert the hint contains `kranz ticket approve <slug>`, `kranz abandon <id>`, `kranz draft <slug>` and the ticket path, and does not contain 'kranz plan'
- A backlog_test test asserts every backticked command in the placeholder-rendered hint appears verbatim in docs/tickets.md
- `grep -n 'kranz plan --mission' crates/cli/src/backlog.rs` finds nothing
- `cargo test -p kranz --test cli_test parked_draft_reshape` runs at least one test and passes: the kranz binary's abandon on a real parked draft exits 0, the mission is Abandoned and the ticket is Failed
- `cargo test -p kranz-engine --test draft_test parked_draft_reshape` runs at least one test and passes: after the abandon, redrafting the same ticket parks a new mission id, the ticket is Review, and the recorded mission is the new id
- docs/tickets.md has a heading containing 'Reshaping' and the literals `kranz abandon <id>`, `kranz draft <slug>` and `kranz ticket approve <slug>`, plus the serve-lock note
- `cargo fmt --all --check` exits 0
- `cargo clippy --workspace --all-targets -- -D warnings` exits 0

### 1.2 Record the in-place reshape decision, recheck the knowledge note, and add a changelog entry

Context: another feature in this mission corrects the parked-draft hint in crates/cli/src/backlog.rs. The hint now tells the operator to add answers to the ticket, then run `kranz abandon <id>` and `kranz draft <slug>`. That feature also adds a 'Reshaping a parked draft' section to docs/tickets.md. A parked draft is a mission already APPROVED (plan committed on its mission branch) whose ticket is in REVIEW. The operator wants the question 'should a parked draft be reshapable in place?' recorded as an open decision, not decided.

Do this (docs only, no code):
1. Create docs/scoping/parked-draft-reshape.md. Follow the style of docs/scoping/mid-mission-replanning.md: a 'Status:' line, a short 'Why', then a decision section headed exactly `## D-A — reshape a parked draft in place? (OPEN)`, with the literal word OPEN. Content:
  - Today's route: answers in the ticket → `kranz abandon <id>` → `kranz draft <slug>`. This is what the hint and docs/tickets.md now say.
  - Why `kranz plan --mission` refuses: crates/cli/src/commands.rs cmd_plan accepts only Planning-status missions. approve_plan is once-only and the reducer never returns a mission to Planning.
  - Why `kranz revise` does not fit: it targets running missions, is extend-only, and cannot change existing assertions (docs/scoping/mid-mission-replanning.md D-E).
  - Options, each with one-line trade-offs: (1) keep abandon + redraft (no code; leaves an abandoned mission directory and branch); (2) a convenience verb, e.g. `kranz redraft <slug>`, that abandons the parked mission and drafts again in one step (no contract change); (3) true in-place reshape, a feedback line → planning turn → re-parked plan, as docs/scoping/pipeline-view.md's 'Reshape' verb describes (needs a new reverse transition from Approved back to Planning or a plan-replaced event, i.e. an additive contract change to the event/reducer files).
  - Note the related dashboard gap: the Reviewable row's secondary 'Reshape' action (apps/dashboard/src/lib/pipelineStage.ts:122, apps/dashboard/src/components/PipelineView.tsx renderSecondary) only links to the ticket detail page, which for a REVIEW ticket offers Approve and no reshape action. It is left unchanged pending this decision.
  - Note that the Slack parked-draft message (crates/slack/src/commands.rs run_draft) names no reshape route and is left unchanged.
  - Give no recommendation that pre-empts the operator. Listing a 'proposal' is fine, but the status stays OPEN.
  Do not invent file paths: check every path you cite exists.
2. docs/knowledge/validation/gates.md lists docs/tickets.md in verified_against and follows a check-on-touch convention. Set front-matter `last_verified` to today's UTC date. Add one new 'Rechecked YYYY-MM-DD (UTC) …' paragraph directly under the front matter, above the existing Rechecked lines, in the same style. Say that the docs/tickets.md change adds a section on reshaping a parked draft (abandon + redraft) and that no gate changed. Change nothing else in that file.
3. CHANGELOG.md: add one entry in the existing unreleased/top section, following the file's existing format. Say that the hint printed for a parked draft now names the route that works (answers in the ticket, `kranz abandon <id>`, `kranz draft <slug>`) instead of `kranz plan --mission`, which refuses an approved mission.
Constraints: only touch docs/scoping/parked-draft-reshape.md, docs/knowledge/validation/gates.md and CHANGELOG.md. No code changes.

Done when:
- docs/scoping/parked-draft-reshape.md exists and has a heading matching `^#+ D-[A-Z]` with the word OPEN
- The decision note lists the three options (keep abandon + redraft, a redraft convenience verb, true in-place reshape) with trade-offs, and mentions the dashboard Reshape button and Slack message as left unchanged
- Every file path cited in the decision note exists in the repo (check with `test -e` for each)
- docs/knowledge/validation/gates.md has last_verified set to today's UTC date and a new top Rechecked paragraph naming the docs/tickets.md reshape section; no other line in it changed (`git diff` shows only those additions and the date)
- CHANGELOG.md has one new entry describing the corrected parked-draft hint, in the file's existing format
- `git diff --name-only` for this feature shows only the three declared files
