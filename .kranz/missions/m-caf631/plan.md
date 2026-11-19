# Mission plan — m-caf631

**Goal:** Add `git stash` to the built-in worker deny list as one rule that an operator can lift with a grant. Make the worker-deny grant offer recognise a stash call that follows `;`, `&&`, `||`, `|` or a newline, and pin in tests how each engine path treats such a call. Naming the read-only alternative in the denial is left to operator decision D-X; this plan delivers the deny rule only.

Branch `kranz/mission-m-caf631` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Authority granted on approval

Approving this plan grants the following to the mission's sessions with no further prompt.

**Write paths** (5; glob patterns the mission may change):
- `crates/engine/src/permissions.rs`
- `crates/engine/src/backend_acp.rs`
- `crates/engine/src/backend_acp/terminals.rs`
- `crates/engine/tests/runner_test.rs`
- `docs/design.md`

## Cost estimate

Estimated **$3.86 – $19.28** (expected ~$8.83). Rough estimate — live usage is authoritative; based on 50 completed mission(s).

Context-fit check (corpus p90 over 59 plan(s): 4326 chars / 23 files):
- **f-1-1** (Add Bash(git stash*) to WORKER_DENY and make matching_deny_rule check each shell segment) — spec is 5505 chars (corpus p90: 4326); spec points at 32 files (corpus p90: 23)

## Considered alternatives

**Chosen approach:** One broad `Bash(git stash*)` rule, so a grant lifts exactly one rule. The stash ref is shared across worktrees, so even list/show only read the operator's stashes. `matching_deny_rule` is made segment-aware so the grant offer works for the observed compound calls, and the ACP and Sgian paths are recorded in tests without being changed. D-X (whether the denial may name the read-only alternative) defaults to the deny rule alone: the Claude CLI writes its own denial text, and prompts.rs is frozen.

Rejected shapes:
- **List only the stash forms that change something (push, save, pop, apply, drop, clear, store, branch, create, and the flag-led push forms `git stash -u`/`--`/`-p`), so list/show stay allowed** — Deny beats allow, so every push spelling must be listed, which is fragile and easy to get around. It also lets a worker read the operator's shared stashes, and a grant would have to lift many rules.
- **Also make ACP `decide_permission` check each command segment** — It would tighten every deny rule (git push, curl, sudo…) on ACP workers. That is a wider enforcement change than this ticket, so the gap is recorded in a test instead.
- **Add a PreToolUse Bash hook that blocks stash calls with a message naming `git diff <base> -- <path>` / `git show <base>:<path>`** — It would deliver the goal's 'is told' sentence on the Claude backend, but it is a new hook projection that hook_gates.rs explicitly scopes out, and it roughly doubles the mission. It needs an operator decision (D-X).

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The built-in worker deny set denies `git stash push -u` and bare `git stash`, and also every other `git stash` form, `list` and `show` included.
  `out=$(cargo test -p kranz-engine --lib worker_deny_blocks_git_stash 2>&1); printf '%s\n' "$out" | tail -n 40; printf '%s\n' "$out" | grep -Eq 'test result: ok\. [1-9][0-9]* passed'`
- **[a2]** Lifting `Bash(git stash*)` through a worker-deny grant removes exactly that rule from the worker's disallowed tools. Every other rule stays, in its original order.
  `out=$(cargo test -p kranz-engine --lib worker_deny_grant_lifts_exactly_the_git_stash_rule 2>&1); printf '%s\n' "$out" | tail -n 40; printf '%s\n' "$out" | grep -Eq 'test result: ok\. [1-9][0-9]* passed'`
- **[a3]** The worker-deny grant matcher (`matching_deny_rule`) names `Bash(git stash*)` for a stash call that follows `;`, `&&`, `||`, `|` or a newline. A compound command with no denied segment still matches nothing.
  `out=$(cargo test -p kranz-engine --lib matching_deny_rule_finds_git_stash_after_shell_operators 2>&1); printf '%s\n' "$out" | tail -n 40; printf '%s\n' "$out" | grep -Eq 'test result: ok\. [1-9][0-9]* passed'`
- **[a4]** A test records the decision the ACP `session/request_permission` check (`decide_permission`) makes for a stash call that is first on the line, and for one that follows `;`, `&&` or a newline.
  `out=$(cargo test -p kranz-engine --lib acp_permission_git_stash_compound_behaviour 2>&1); printf '%s\n' "$out" | tail -n 40; printf '%s\n' "$out" | grep -Eq 'test result: ok\. [1-9][0-9]* passed'`
- **[a5]** The Sgian `terminal/create` check denies a shell-wrapped stash call that follows `;`, `&&` or a newline. It still allows a read-only `git diff` and `git show`.
  `out=$(cargo test -p kranz-engine --lib sgian_terminal_denies_git_stash_after_shell_operators 2>&1); printf '%s\n' "$out" | tail -n 40; printf '%s\n' "$out" | grep -Eq 'test result: ok\. [1-9][0-9]* passed'`
- **[a6]** Anti-vacuity: the `git_stash` test filter matches at least five lib tests, and the base tree has no `git_stash` text under crates/engine, so the filter matches only the new tests.
  `list=$(cargo test -p kranz-engine --lib git_stash -- --list 2>/dev/null | grep ': test$'); printf '%s\n' "$list"; n=$(printf '%s\n' "$list" | grep -c ': test$'); [ "$n" -ge 5 ] && ! git grep -q git_stash "$KRANZ_BASE_SHA" -- crates/engine`
- **[a7]** The existing worker-profile and permission tests still pass with the new rule in place.
  `cargo test -p kranz-engine --lib permissions::tests && cargo test -p kranz-engine --test runner_test worker_profile_denies_push_publish_network_and_config_extras`
- **[a8]** The authoritative role/permission table in docs/design.md lists `Bash(git stash*)` among the worker deny rules.
  `grep -F 'Bash(git stash*)' docs/design.md`
- **[a9]** The frozen prompt surface is untouched: crates/engine/src/prompts.rs is unchanged against the mission base.
  `git diff --quiet "$KRANZ_BASE_SHA" -- crates/engine/src/prompts.rs`
- **[a10]** The code is formatted and clippy-clean under the CI settings.
  `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`
- **[a11]** The full workspace test suite passes.
  `cargo test --workspace`
- **[a12]** The mission records ACP and Sgian enforcement behaviour without changing it. Production code in crates/engine/src/backend_acp.rs and crates/engine/src/backend_acp/terminals.rs is unchanged; only their `#[cfg(test)]` modules gain tests. The mission adds no stash recovery or detection mechanism. *(agent judgement)*
## Contract lint

Each `check: command` assertion above was run once against the untouched base tree at approval time. Suspects are assertions that already pass (or could not reach a verdict) before this plan's work lands — a possible polarity/vacuity bug in the assertion itself. This never blocks approval.

author-bug suspects (already pass / no verdict on the untouched base): [a7] cargo test -p kranz-engine --lib permissions::tests && cargo test -p kranz-engine --test runner_test worker_profile_denies_push_publish_network_and_config_extras; [a9] git diff --quiet "$KRANZ_BASE_SHA" -- crates/engine/src/prompts.rs; [a10] cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
base-expected-to-fail (benign): [a1] out=$(cargo test -p kranz-engine --lib worker_deny_blocks_git_stash 2>&1); printf '%s\n' "$out" | tail -n 40; printf '%s\n' "$out" | grep -Eq 'test result: ok\. [1-9][0-9]* passed'; [a2] out=$(cargo test -p kranz-engine --lib worker_deny_grant_lifts_exactly_the_git_stash_rule 2>&1); printf '%s\n' "$out" | tail -n 40; printf '%s\n' "$out" | grep -Eq 'test result: ok\. [1-9][0-9]* passed'; [a3] out=$(cargo test -p kranz-engine --lib matching_deny_rule_finds_git_stash_after_shell_operators 2>&1); printf '%s\n' "$out" | tail -n 40; printf '%s\n' "$out" | grep -Eq 'test result: ok\. [1-9][0-9]* passed'; [a4] out=$(cargo test -p kranz-engine --lib acp_permission_git_stash_compound_behaviour 2>&1); printf '%s\n' "$out" | tail -n 40; printf '%s\n' "$out" | grep -Eq 'test result: ok\. [1-9][0-9]* passed'; [a5] out=$(cargo test -p kranz-engine --lib sgian_terminal_denies_git_stash_after_shell_operators 2>&1); printf '%s\n' "$out" | tail -n 40; printf '%s\n' "$out" | grep -Eq 'test result: ok\. [1-9][0-9]* passed'; [a6] list=$(cargo test -p kranz-engine --lib git_stash -- --list 2>/dev/null | grep ': test$'); printf '%s\n' "$list"; n=$(printf '%s\n' "$list" | grep -c ': test$'); [ "$n" -ge 5 ] && ! git grep -q git_stash "$KRANZ_BASE_SHA" -- crates/engine; [a8] grep -F 'Bash(git stash*)' docs/design.md; [a11] cargo test --workspace

Lint commands ran under shell: sh

named contract gates (defect classes):
- vacuous-filter: FAIL
  [a7] cargo test filter `worker_profile_denies_push_publish_network_and_config_extras` collides with an existing test at crates/engine/tests/runner_test.rs:177 (`fn worker_profile_denies_push_publish_network_and_config_extras`) — the gate can pass on pre-existing tests with zero implementation; the filter must match ONLY the not-yet-written tests: `cargo test -p kranz-engine --lib permissions::tests && cargo test -p kranz-engine --test runner_test worker_profile_denies_push_publish_network_and_config_extras`
- wrong-polarity: PASS
- host-shell-syntax: PASS
- passes-on-base: FAIL
  [a7] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo test -p kranz-engine --lib permissions::tests && cargo test -p kranz-engine --test runner_test worker_profile_denies_push_publish_network_and_config_extras`
  [a9] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `git diff --quiet "$KRANZ_BASE_SHA" -- crates/engine/src/prompts.rs`
  [a10] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings`
- env-sensitive: PASS


## Milestone 1 — Workers are denied git stash, and the deny is liftable and pinned on every engine-enforced path

### 1.1 Add Bash(git stash*) to WORKER_DENY and make matching_deny_rule check each shell segment

Repository: kranz (Rust workspace). Crate: kranz-engine (crates/engine). This feature edits only crates/engine/src/permissions.rs, crates/engine/tests/runner_test.rs and docs/design.md. Do NOT touch crates/engine/src/prompts.rs (a frozen surface), crates/engine/src/backend_acp.rs or crates/engine/src/backend_acp/terminals.rs; another feature owns those.

WHY: Workers ran `git stash push -u` in the mission worktree to check whether a build failure existed before their change. That moves the feature's uncommitted and untracked work into a stash. If the session ends before the pop, the engine judges a tree missing that work. The stash ref is also shared by every worktree of the repo, so stash list/show/pop/drop in a worker reach the operator's own stashes. The decision is ONE broad rule that denies every `git stash` form, list/show included. Deny beats allow in the agent CLI, so read-only forms cannot be carved back out of a broad deny, and one rule keeps the operator's grant lift to exactly one rule.

CHANGE 1: In `const WORKER_DENY` (crates/engine/src/permissions.rs, around line 99), add the entry `"Bash(git stash*)"` directly after `"Bash(git remote add*)"`. Extend the doc comment above it in one short clause, matching the existing style, e.g. 'no tree-moving `git stash` (the stash ref is shared across worktrees and a session can end between push and pop)'.

CHANGE 2: `pub fn matching_deny_rule(command, deny_rules)` (same file, around line 361) currently prefix-matches only the whole trimmed command (`cmd.starts_with(prefix)`). A denied `cargo build; git stash push -u` therefore yields None and the operator is never offered a worker-deny grant. Change it to split the command into segments on `&&`, `||`, `;`, `|` and newline (`\n`, plus `\r\n`), trim each segment, and prefix-match every non-empty segment as well as the whole command. Keep the existing semantics: only `Bash(<pattern>)` rules participate, a trailing `*` is stripped to form the prefix, an empty prefix never matches, and among all matches across all segments the rule with the LONGEST prefix wins. Update the doc comment to say segments are checked. Keep the code in the style of the neighbouring `command_allow_patterns`, which already splits on `&&`/`||`/`;`/`|`.

CHANGE 3: crates/engine/tests/runner_test.rs, test `worker_profile_denies_push_publish_network_and_config_extras` (around line 193). Add `"Bash(git stash*)"` to its expected list after `"Bash(git remote add*)"`.

CHANGE 4: docs/design.md line ~42 (the Worker row of the role/permission table). Make the deny column list exactly the WORKER_DENY entries in order, `Bash(git stash*)` included. The row is already stale: it is missing `git remote add`, `yarn publish`, `pnpm publish` and `gem push`. Keep `+ config denyPatterns`. Below the table, add one short paragraph saying how a worker deny rule is matched against a command line with `;`, `&&`, `||`, `|` or a newline:
- the Claude backend relies on the Claude Code CLI's own matcher, which this repo does not test;
- the ACP `session/request_permission` check matches the whole command string from its start, so a later segment is not matched (a known gap);
- the Sgian `terminal/create` check splits `sh -c` scripts and catches later segments;
- the grant offer (`matching_deny_rule`) checks each segment.
Also state that codex/droid/kimi/cursor backends do not read `disallowed_tools`. Write plain declarative sentences.

TESTS: add these three tests to the existing `#[cfg(test)] mod tests` in permissions.rs. Use EXACTLY these names; the mission contract runs them by name, and every name contains `git_stash`:
(1) `worker_deny_blocks_git_stash_push_and_bare_git_stash`: assert WORKER_DENY contains `Bash(git stash*)`; assert `for_role(Role::Worker, &MissionConfig::default(), &[], &[], &[]).disallowed_tools` contains it; with `to_strings(WORKER_DENY)` assert `matching_deny_rule` returns `Some("Bash(git stash*)")` for `git stash push -u`, `git stash`, `git stash list`, `git stash show -p`, `git stash pop`, `git stash apply stash@{0}`, `git stash drop` and `git stash -u`, and returns None for `git status` and `git diff HEAD -- src/lib.rs`.
(2) `worker_deny_grant_lifts_exactly_the_git_stash_rule`: compute the base worker profile and a lifted profile with deny_exceptions `["Bash(git stash*)"]`. Assert the lifted disallowed_tools equals the base list with exactly that one entry removed, order preserved and length one less. Assert `Bash(git push*)` is still present.
(3) `matching_deny_rule_finds_git_stash_after_shell_operators`: with `to_strings(WORKER_DENY)` assert `Some("Bash(git stash*)")` for `cargo build; git stash push -u`, `cargo build && git stash push -u`, `cargo build\ngit stash push -u`, `cargo build || git stash` and `echo x | git stash`. Assert None for `cargo build && cargo test`. Assert the longest-prefix rule still wins across segments: with deny `["Bash(git push*)", "Bash(git push --force*)"]`, `cargo build && git push --force origin main` gives `Bash(git push --force*)`.
Write the tests first and watch (3) fail before changing `matching_deny_rule`.

VERIFY with: `cargo test -p kranz-engine --lib permissions::tests`, `cargo test -p kranz-engine --test runner_test worker_profile_denies_push_publish_network_and_config_extras`, `cargo fmt --all --check`, `cargo clippy -p kranz-engine --all-targets -- -D warnings`. Put the test-runner output in testEvidence. Do NOT add any stash recovery or detection mechanism, and do not edit any prompt text.

Done when:
- WORKER_DENY contains `Bash(git stash*)`, and the default worker profile's disallowed_tools contains it
- matching_deny_rule maps `git stash push -u`, `git stash`, `git stash list`, `git stash show -p`, `git stash pop`, `git stash drop` and `git stash -u` to `Bash(git stash*)`, and maps `git status` to None
- a worker-deny grant for `Bash(git stash*)` removes exactly that one rule and preserves the order of the rest
- matching_deny_rule names `Bash(git stash*)` for a stash call after `;`, `&&`, `||`, `|` and a newline, returns None for `cargo build && cargo test`, and keeps longest-prefix precedence across segments
- runner_test's worker-profile test expects `Bash(git stash*)`, and permissions::tests passes in full
- docs/design.md's Worker row lists exactly the WORKER_DENY entries, and a paragraph states the per-path compound-command matching behaviour

### 1.2 Pin how the ACP permission and Sgian terminal checks treat a git stash call after shell operators (test-only)

Repository: kranz (Rust workspace). Crate: kranz-engine (crates/engine). This feature is TEST-ONLY. Add tests inside the existing `#[cfg(test)] mod tests` blocks of crates/engine/src/backend_acp.rs and crates/engine/src/backend_acp/terminals.rs. Do NOT change any non-test code in either file. Do NOT touch crates/engine/src/permissions.rs (another feature adds `Bash(git stash*)` to WORKER_DENY there). Your tests pass an explicit disallowed list containing `Bash(git stash*)`, so they do not depend on that feature.

WHY: the mission denies `git stash` to workers. Workers were seen running stash calls that were NOT first on the line, e.g. `cargo build; git stash push -u` or after `&&` or a newline. The engine enforces worker deny rules on ACP backends in two places, and the mission must RECORD what each does with such a call, not change it.

PATH A: `fn decide_permission(spec: &SessionSpec, call: &ToolCallInfo) -> PermissionDecision` in crates/engine/src/backend_acp.rs (around line 314). For an `execute` call it matches each subject string against each `Bash(<glob>)` deny pattern with `wildcard_match`. That match is anchored at the START of the string, and the subject is the whole command string, unsplit. Expected current behaviour: a lone `git stash push -u` is denied, but `cargo build && git stash push -u` is ALLOWED, a known gap. Add test `acp_permission_git_stash_compound_behaviour` to the tests module there. Use the existing helper `spec_with(true, &["Bash(git stash*)"])` (around line 2266) and build `ToolCallInfo { kind: "execute", title, subject: vec![cmd] }` as `backend_acp_permission_denies_disallowed_and_mutating_kinds` does (around line 2291). Assert Deny, with the reason containing `Bash(git stash*)`, for `git stash push -u` and `git stash`. Then, for `cargo build; git stash push -u`, `cargo build && git stash push -u` and `cargo build\ngit stash push -u`, assert whatever decision the code ACTUALLY returns today. Run it first to observe; the expectation is `PermissionDecision::Allow`. Add a comment above those asserts: this records a known gap, `decide_permission` matches the whole command from its start, so a later segment is not denied on this path, and the Claude backend and the Sgian terminal path behave differently (see docs/design.md). If the observed behaviour differs from Allow, assert what you observe and say so in your report.

PATH B: `fn sgian_terminal_prohibition(spec, action: &kranz_acp::terminal::Create, value: &Value) -> Option<String>` in crates/engine/src/backend_acp/terminals.rs (around line 784). For shell interpreters with a `-c`-style flag it splits the script on `\n ; | & ( )` and backtick and checks each segment. Add test `sgian_terminal_denies_git_stash_after_shell_operators` to the tests module there, modelled on `sgian_terminal_prohibition_enforces_worker_and_authority_deny_patterns` (around line 1289), which builds a `SessionSpec` and a `make_create` closure. Use disallowed_tools `["Bash(git stash*)"]`, writable true. Assert `Some(reason)` with the reason containing `Bash(git stash*)` for:
- `("/bin/sh", ["-c", "cargo build; git stash push -u"])`
- `("/bin/sh", ["-c", "cargo build && git stash push -u"])`
- `("/bin/bash", ["-lc", "cargo build\ngit stash push -u"])`
- `("git", ["stash"])`
- `("/usr/bin/git", ["stash", "push", "-u"])`
Assert None for `("/bin/sh", ["-c", "git diff HEAD -- src/lib.rs"])` and `("git", ["show", "HEAD:src/lib.rs"])`. If any of these does not behave as stated, assert the observed behaviour, comment why, and report it. Do not change production code.

Both test names must contain `git_stash` exactly as given; the mission contract runs them by name. VERIFY with `cargo test -p kranz-engine --lib git_stash`, `cargo test -p kranz-engine --lib backend_acp`, `cargo fmt --all --check` and `cargo clippy -p kranz-engine --all-targets -- -D warnings`, and put the runner output in testEvidence. Do not edit prompt text or add any stash recovery mechanism.

Done when:
- test `acp_permission_git_stash_compound_behaviour` asserts Deny naming `Bash(git stash*)` for a lone `git stash push -u` and `git stash`, and asserts the observed decision for a stash call after `;`, `&&` and a newline, with a comment naming the gap
- test `sgian_terminal_denies_git_stash_after_shell_operators` asserts denial for shell-wrapped stash calls after `;`, `&&` and a newline and for direct `git stash` invocations, and asserts no denial for `git diff` and `git show`
- no non-test code in backend_acp.rs or backend_acp/terminals.rs changes
- `cargo test -p kranz-engine --lib backend_acp` passes in full
