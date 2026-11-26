# Mission report — m-e36de6

**Goal:** Offer a validator a command grant only when a denial is what stopped its session; when an earlier denial did not stop an untrusted validator, retry within the existing budget or block with a reason that says how the session actually ended.

Branch `kranz/mission-m-e36de6` (from `main`). Plan of record: [plan.md](plan.md).

## The plan

1 milestone, 4 features, gated by 14 contract assertions (13 command, 1 judgement).

1. **A validator is offered a command grant only for the denial that stopped it**
   - Runner records whether the last denial stopped the session — Repo: kranz (Rust workspace).
   - Orchestrator offers a validator grant only for the stopping denial and names how the session ended — Repo: kranz (Rust workspace).
   - Mission tests: no grant for a denial the validator carried on past; stopping denial still parks — Repo: kranz (Rust workspace).
   - Record the D-X decisions, open questions and changelog entry — Repo: kranz.

**Chosen approach:** Record where the last denial sits in the session (tool calls after it, stopping command, last tool call) in the runner, gate the existing v…
**Elapsed:** 41m 50s
**Tokens:** 372 in / 105983 out / 12020747 cache read / 615446 cache write
**Cost:** $7.23 actual vs $5.40–$29.31 estimated (expected $13.43)
**Interventions:** 0 (0 steers, 0 grant decisions, 0 revision rulings, 0 operator unblocks)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-e36de6-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Provider:** local-worktree (source isolation) · template: worktree · no workspace contract
- **Workspace contract:** no workspace contract (source isolation only)
- **Preflight:** preflight: 1 issue(s): [warn] command assertion [a8] uses '!', which was not found on PATH

## What shipped

### Milestone 1 — A validator is offered a command grant only for the denial that stopped it ✅

- ✅ **Runner records whether the last denial stopped the session** — 1 run
  Repo: kranz (Rust workspace).
  - `1afb974` [f-1-1] record whether the last denial stopped the session
  - ✓ An early denied Bash command followed by three successful tool calls and a partial end yields tool_calls_after_last_denial == Some(3) and stopping_denied_command == None
  - ✓ A denied Bash command with no later tool call yields tool_calls_after_last_denial == Some(0) and stopping_denied_command naming exactly that command
  - ✓ In a parallel batch (two ToolUse events, then a denied result, then a successful result) the denial counts as stopping
  - ✓ A session with no denial yields tool_calls_after_last_denial == None and stopping_denied_command == None
  - ✓ A last denial on a non-shell tool yields stopping_denied_command == None even when an earlier shell command was denied
  - ✓ last_tool_call renders the last ToolUse as `<tool>: <summary>`
  - ✓ denied_commands and denied_count behaviour is unchanged (existing denied_commands runner tests pass)
  - ✓ All tests are named with the prefix session_end_denial_ and pass under `cargo test -p kranz-engine --test runner_test session_end_denial`
- ✅ **Orchestrator offers a validator grant only for the stopping denial and names how the session ended** — 1 run
  Repo: kranz (Rust workspace).
  - `f66fe9a` [f-1-2] offer a validator grant only for the denial that stopped the session
  - ✓ maybe_park_for_grant parks on stopping_denied_command and never on an earlier denied command that did not stop the session
  - ✓ When the session had denied commands but none stopped it, an orchestrator.decision starting `not offering a grant for` is emitted and no grant.requested event is raised
  - ✓ validator_session_end_summary produces `the session ended its turn without a report`, `last tool call: `<tool>: <summary>``, and `<n> tool call(s) ran after the last denied tool result` for a partial session that continued past a denial
  - ✓ validator_session_end_summary describes aborted, failed and error-result endings and the no-tool-call case
  - ✓ The untrusted-validator MilestoneBlocked reason and the retry decision include the session-end summary; run_outcome_summary is unchanged
  - ✓ Existing validator grant mission tests (validator_denial_*, grant_*) still pass
  - ✓ Unit tests are named with the prefix validator_session_end_summary_ and pass under `cargo test -p kranz-engine --lib validator_session_end_summary`
- ✅ **Mission tests: no grant for a denial the validator carried on past; stopping denial still parks** — 1 run
  Repo: kranz (Rust workspace).
  - `2595f34` [f-1-3] test: grant only parks for the denial that stopped the validator session
  - ✓ validator_denial_not_stopping_retry_completes_without_grant: an early denied command followed by successful tool calls and a partial end raises no grant.request, a `not offering a grant for` decision is recorded, and the mission completes after a clean retry
  - ✓ validator_denial_not_stopping_blocks_naming_session_end: with both sessions continuing past the denial, the mission blocks with no grant request and the milestone.blocked reason names that the session ended its turn without a report, its last tool call, and the tool calls after the denial
  - ✓ validator_stopping_denial_parks_for_that_command: a denial followed by no tool call and a partial non-error end raises grant.requested naming exactly `gc audit --deep` on ms-1
  - ✓ Existing grant mission tests still pass under `cargo test -p kranz-engine --test mission_test grant`
- ✅ **Record the D-X decisions, open questions and changelog entry** — 2 runs, 1 respawn
  Repo: kranz.
  - `9831b62` [f-1-4] document the D-X decisions for validator grant-parking and changelog
  - `1f0b24d` [f-1-4] fix factual errors in the validator grant-parking scoping doc
  - ✓ docs/scoping/validator-park-only-when-denial-stopped-it.md exists and flags D-A (stopping rule) and D-B (offered command) as operator decisions with their rejected alternatives
  - ✓ The scoping doc lists as not changed: the grant wait, the deferred prompt guidance, the worker deny-grant path, egress grants and parallel-batch attribution
  - ✓ CHANGELOG.md Unreleased gains one Fixed entry describing the change in user-visible terms
  - ✓ mission-pipeline.md gains one dated recheck paragraph and no other change
  - ✓ No source file changed in this feature

## Validation history

### ms-1 round 1 — A validator is offered a command grant only for the denial that stopped it

No findings.

## Contract outcomes

- ✅ **[a1]** The runner records, per session, whether its last denied tool result was followed by any further tool call (count), the shell command of a denial that stopped the session, and the session's last tool call; an early denial followed by tool calls records no stopping command, and a denial with no tool call after it records that command. *(command: `out=$(cargo test -p kranz-engine --test runner_test session_end_denial 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a2]** The validator session-end description names how the session ended (no report / error result / aborted / failed), its last tool call, and how many tool calls ran after the last denied tool result. *(command: `out=$(cargo test -p kranz-engine --lib validator_session_end_summary 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a3]** A validator session with an early denied command, further successful tool calls and a partial end raises no grant request; the validator is retried and, on a clean retry, the mission completes. *(command: `out=$(cargo test -p kranz-engine --test mission_test validator_denial_not_stopping_retry 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a4]** When both the primary and the retry validator sessions continue past an early denial and end without a report, no grant request is raised and the milestone blocks with a reason naming how the session ended (no report, its last tool call, and the tool calls after the denial). *(command: `out=$(cargo test -p kranz-engine --test mission_test validator_denial_not_stopping_blocks 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a5]** A validator session whose denied command is followed by no further tool call before a partial (non-error) end still parks a grant request naming exactly that command. *(command: `out=$(cargo test -p kranz-engine --test mission_test validator_stopping_denial_parks 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a6]** The existing grant-flow mission tests (validator, worker-deny, egress, touch-path, cap, retry and superseded-request grants) still pass. *(command: `out=$(cargo test -p kranz-engine --test mission_test grant 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a7]** An incidental denial on a validator that still produces a trusted pass does not park for a grant. *(command: `out=$(cargo test -p kranz-engine --test mission_test incidental_denial_on_a_passing_validator_does_not_park 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a8]** Contract files change additively only: no line of events.rs, types.rs, backend.rs or error.rs is removed or modified against the mission base. *(command: `! (git diff -U0 $KRANZ_BASE_SHA -- crates/engine/src/events.rs crates/engine/src/types.rs crates/engine/src/backend.rs crates/engine/src/error.rs | grep -E '^-' | grep -qvE '^--- ')`)*
- ✅ **[a9]** The frozen prompt module is unchanged. *(command: `git diff --quiet $KRANZ_BASE_SHA -- crates/engine/src/prompts.rs`)*
- ✅ **[a10]** The grant request wait is unchanged: the one-hour default constant is intact and the grantRequestTimeoutSecs config field is untouched. *(command: `grep -q 'pub const DEFAULT_GRANT_REQUEST_TIMEOUT: Duration = Duration::from_secs(3600);' crates/engine/src/orchestrator.rs && ! (git diff -U0 $KRANZ_BASE_SHA -- crates/engine/src/types.rs | grep -E '^[-+]' | grep -vE '^(---|\+\+\+) ' | grep -q 'grant_request_timeout')`)*
- ✅ **[a11]** The workspace is formatted. *(command: `cargo fmt --all --check`)*
- ✅ **[a12]** The workspace builds and lints clean with warnings denied, including all test targets. *(command: `cargo clippy --workspace --all-targets -- -D warnings`)*
- ✅ **[a13]** The engine library test suite passes. *(command: `out=$(cargo test -p kranz-engine --lib 2>&1) && printf '%s\n' "$out" | grep -qE 'test result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a14]** docs/scoping/validator-park-only-when-denial-stopped-it.md records the stopping rule and the offered-command rule as flagged D-X operator decisions with their rejected alternatives, and records as open and not acted on: the one-hour grant wait question, the operator's one-command-per-call / no-background-agent validator guidance (prompt change deferred under the positioning freeze), the worker deny-grant path's same first-denial shape, and egress grants (no stream position); the CHANGELOG has an Unreleased entry describing the change in user-visible terms. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
