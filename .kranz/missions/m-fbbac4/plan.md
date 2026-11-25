# Mission plan — m-fbbac4

**Goal:** Validators of both roles can read the mission's own diff with the sanctioned form `git diff $KRANZ_BASE_SHA..HEAD [-- <paths>]` without raising a grant request. Both validator prompts say this form is pre-approved and that pipes are not permitted. Git options that write files or run programs stay denied to validators, and a matcher-backed test proves it. The worker profile, the orchestrator profile and the grant flow are unchanged.

Branch `kranz/mission-m-fbbac4` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Authority granted on approval

Approving this plan grants the following to the mission's sessions with no further prompt.

**Commands** (1; each is a prefix, so `x` also allows `x <anything>`):
- `git diff $KRANZ_BASE_SHA..HEAD -- crates/engine/src/permissions.rs crates/engine/prompts/validator-scrutiny.md crates/engine/prompts/validator-functional.md docs/design.md | grep -nE`

**Write paths** (3; glob patterns the mission may change):
- `crates/engine/src/permissions.rs`
- `crates/engine/prompts/validator-*.md`
- `docs/design.md`

## Cost estimate

Estimated **$3.67 – $18.37** (expected ~$8.36). Rough estimate — live usage is authoritative; based on 54 completed mission(s).

Context-fit check (corpus p90 over 65 plan(s): 4747 chars / 25 files):
- **f-1-1** (Validator git diff rules: sanctioned base-SHA form plus write/run option denies, with a matcher-backed test) — spec is 7001 chars (corpus p90: 4747); spec points at 50 files (corpus p90: 25)

## Considered alternatives

**Chosen approach:** Keep the existing broad validator git rules, which the prompts' `{startSha}..HEAD` instructions depend on. Add the named `Bash(git diff $KRANZ_BASE_SHA..HEAD*)` rule, plus validator-only deny rules (which win over allows) for the options that write or run programs, including their abbreviations and `git difftool`. Rule out pipes in both prompts. A test-side matcher proves the worst commands stay denied. The scoping answers asked for other ranges to stay denied as well. That is not delivered: `Bash(git diff*)` already allows any range today, and `git show`/`git log` read any revision, so narrowing ranges for `git diff` alone would break the milestone-range instruction and protect nothing.

Rejected shapes:
- **Prompt-and-test only (no permission change): name the already-allowed form in the prompts.** — Leaves `--output`, `--ext-diff`, `--textconv` and `git difftool -x` (arbitrary program execution via the `git diff*` prefix) open, contradicting the scoping answer that these stay denied.
- **Range-anchored rules: replace validator `git diff*` with `git diff {startSha}..HEAD*` and base-SHA forms (literal and hex), threading the SHAs through `for_role`.** — Changes `for_role`'s signature and its callers across the runner and tests, and still doesn't stop `git show <any-rev>` or `git log -p` from reading other history, so the range restriction is theater.
- **Allow pipes into filters by adding `Bash(grep*)` and `Bash(head*)` validator rules.** — The CLI checks pipe parts separately, so this would let validators run grep/head on anything, with redirection, just to filter text the Grep tool already filters.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** Exactly one test matches the filter `validator_mission_diff_reads` in the engine lib, and it passes. It shows the sanctioned diff form is allowed for both validator roles and that bare `git`, `git -c`, the output, external-diff and textconv options (and their abbreviations), `git difftool`, env-prefixed git, pipes, redirection and chained commands are not allowed.
  `cargo test -p kranz-engine --lib permissions::tests::validator_mission_diff_reads 2>&1 | grep -qE 'test result: ok\. 1 passed; 0 failed'`
- **[a2]** Every permissions unit test in the engine lib passes, including the existing printenv, grant, worker-deny and composition-audit tests.
  `cargo test -p kranz-engine --lib permissions:: 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed; 0 failed'`
- **[a3]** The runner integration tests pass, including the orchestrator read-only profile test that still expects `Bash(git diff*)`.
  `cargo test -p kranz-engine --test runner_test 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed; 0 failed'`
- **[a4]** The worker profile is unchanged: the `WORKER_DENY` constant and the `Role::Worker` arm of `for_role` in permissions.rs are byte-identical to the base commit.
  `A=$(git show "$KRANZ_BASE_SHA":crates/engine/src/permissions.rs | sed -n -e '/^const WORKER_DENY/,/^];/p' -e '/Role::Worker => {/,/^        }$/p'); B=$(sed -n -e '/^const WORKER_DENY/,/^];/p' -e '/Role::Worker => {/,/^        }$/p' crates/engine/src/permissions.rs); [ -n "$A" ] && [ "$A" = "$B" ]`
- **[a5]** Both validator prompts name `git diff $KRANZ_BASE_SHA..HEAD` as the sanctioned way to read the mission diff and say that pipes are not permitted.
  `for f in crates/engine/prompts/validator-scrutiny.md crates/engine/prompts/validator-functional.md; do grep -qF 'git diff $KRANZ_BASE_SHA..HEAD' "$f" && grep -qF 'Pipes are not permitted' "$f" || exit 1; done`
- **[a6]** The grant flow is unchanged: the orchestrator and runner sources, which park, time out and deny-default grant requests, are not modified by the mission.
  `git diff --quiet "$KRANZ_BASE_SHA" -- crates/engine/src/orchestrator.rs crates/engine/src/runner.rs`
- **[a7]** The workspace is rustfmt-clean.
  `cargo fmt --all -- --check`
- **[a8]** Judged against the worst command a prompt-injected validator could write, not the one in the m-dd7d22 report: every new validator allow rule is covered by deny rules for options that write files or run programs (`--output`, `--ext-diff`, `--textconv` and their accepted abbreviations, `git difftool`) on `git diff`, `git log` and `git show`. The prompts claim nothing the rules don't enforce, and the orchestrator profile is untouched. *(agent judgement)*
## Contract lint

Each `check: command` assertion above was run once against the untouched base tree at approval time. Suspects are assertions that already pass (or could not reach a verdict) before this plan's work lands — a possible polarity/vacuity bug in the assertion itself. This never blocks approval.

author-bug suspects (already pass / no verdict on the untouched base): [a2] cargo test -p kranz-engine --lib permissions:: 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed; 0 failed'; [a3] cargo test -p kranz-engine --test runner_test 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed; 0 failed'; [a4] A=$(git show "$KRANZ_BASE_SHA":crates/engine/src/permissions.rs | sed -n -e '/^const WORKER_DENY/,/^];/p' -e '/Role::Worker => {/,/^        }$/p'); B=$(sed -n -e '/^const WORKER_DENY/,/^];/p' -e '/Role::Worker => {/,/^        }$/p' crates/engine/src/permissions.rs); [ -n "$A" ] && [ "$A" = "$B" ]; [a6] git diff --quiet "$KRANZ_BASE_SHA" -- crates/engine/src/orchestrator.rs crates/engine/src/runner.rs; [a7] cargo fmt --all -- --check
base-expected-to-fail (benign): [a1] cargo test -p kranz-engine --lib permissions::tests::validator_mission_diff_reads 2>&1 | grep -qE 'test result: ok\. 1 passed; 0 failed'; [a5] for f in crates/engine/prompts/validator-scrutiny.md crates/engine/prompts/validator-functional.md; do grep -qF 'git diff $KRANZ_BASE_SHA..HEAD' "$f" && grep -qF 'Pipes are not permitted' "$f" || exit 1; done

Lint commands ran under shell: sh

named contract gates (defect classes):
- vacuous-filter: FAIL
  [a1] test-runner pipeline's grep anchors no nonzero count (missing the `[1-9]` guard — zero matching tests still print `test result: ok.`): `cargo test -p kranz-engine --lib permissions::tests::validator_mission_diff_reads 2>&1 | grep -qE 'test result: ok\. 1 passed; 0 failed'`
- wrong-polarity: PASS
- host-shell-syntax: PASS
- passes-on-base: FAIL
  [a2] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo test -p kranz-engine --lib permissions:: 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed; 0 failed'`
  [a3] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo test -p kranz-engine --test runner_test 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed; 0 failed'`
  [a4] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `A=$(git show "$KRANZ_BASE_SHA":crates/engine/src/permissions.rs | sed -n -e '/^const WORKER_DENY/,/^];/p' -e '/Role::Worker => {/,/^        }$/p'); B=$(sed -n -e '/^const WORKER_DENY/,/^];/p' -e '/Role::Worker => {/,/^        }$/p' crates/engine/src/permissions.rs); [ -n "$A" ] && [ "$A" = "$B" ]`
  [a6] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `git diff --quiet "$KRANZ_BASE_SHA" -- crates/engine/src/orchestrator.rs crates/engine/src/runner.rs`
  [a7] already exits zero on the untouched base tree — a correctly-scoped "the work landed" assertion must FAIL before the work lands: `cargo fmt --all -- --check`
- env-sensitive: PASS


## Milestone 1 — Validators read the mission diff without a grant; git's write/run options stay denied

### 1.1 Validator git diff rules: sanctioned base-SHA form plus write/run option denies, with a matcher-backed test

Context: Kranz builds Claude CLI permission profiles per role in `crates/engine/src/permissions.rs` (`for_role`). Validators (`Role::ValidatorScrutiny`, `Role::ValidatorFunctional`) get `Read`/`Glob`/`Grep`, the shared `GIT_INSPECT` list (which already contains the broad `Bash(git diff*)`, `Bash(git log*)` and `Bash(git show*)`), and `Bash(printenv KRANZ_*)`. Their deny set is `read_only_deny()`. The Claude CLI matches a Bash rule `Bash(<glob>)` against each part of a compound command separately: it splits on `&&`, `||`, `;`, `|` and newlines. `*` matches any characters, including none. Every part must match an allow rule, and deny rules win over allows.

In mission m-dd7d22 a validator's `git diff <sha>..HEAD -- <files> | grep -nE ... | head -20` was denied because of the `grep`/`head` parts, parked as a grant request and blocked the milestone. This feature sanctions one named diff form and closes the dangerous options that the existing broad `git diff*`/`git log*`/`git show*` allows already admit. Validators are prompt-injectable, so judge every rule by the worst command it matches.

Do exactly this, in `crates/engine/src/permissions.rs` only (and optionally `docs/design.md`, see the end):

1. In the validator arm of `for_role` (the `Role::ValidatorScrutiny | Role::ValidatorFunctional` arm), push the allow rule `Bash(git diff $KRANZ_BASE_SHA..HEAD*)` next to the existing `Bash(printenv KRANZ_*)` push. Write `$KRANZ_BASE_SHA` literally: it is the engine-injected env var naming the base commit pinned at plan approval. Add a comment in the style of the printenv one: cite ticket `validator-diff-inspection-grant` and m-dd7d22, say this is the sanctioned way to read the mission diff, and say pipes are deliberately not allowed (no `grep`/`head` rules: the Grep/Read tools filter instead).

2. Add a new `const VALIDATOR_GIT_DENY: &[&str]` with a doc comment explaining why. Fold it into the validator arm's `disallowed_tools` only: e.g. `let mut deny = read_only_deny(); deny.extend(to_strings(VALIDATOR_GIT_DENY));` and dedup. Do NOT change `READ_ONLY_DENY`, `read_only_deny()`, `GIT_INSPECT`, the Orchestrator arm, the `WORKER_DENY` constant or the `Role::Worker` arm. Those must stay byte-identical; a contract check compares the worker const and arm against the base commit. For each SUB in `diff`, `log`, `show` the list contains these mid-pattern rules:
   - `Bash(git SUB*--ou*)` (`--output`; git accepts unambiguous long-option abbreviations, so cover `--out=`, `--outp` etc.)
   - `Bash(git SUB*--ext*)` (`--ext-diff`, abbreviation `--ext`)
   - `Bash(git SUB*--textc*)` (`--textconv`, abbreviation `--textc`)
   It also contains prefix-only fallbacks `Bash(git SUB --ou*)`, `Bash(git SUB --ext*)` and `Bash(git SUB --textc*)`, for CLI versions that only honour a trailing `*`. Finally it contains `Bash(git difftool*)`, because the existing `Bash(git diff*)` allow also matches `git difftool -x <cmd>`, which runs an arbitrary program. Note in the comment that `--no-ext-diff` and `--no-textconv` do NOT match these rules (the substring `--ext`/`--textc` needs two leading dashes) and stay usable.

3. In `mod tests`, add a test-only matcher that copies the CLI semantics. `fn rule_glob_matches(rule: &str, segment: &str) -> bool` handles only `Bash(<glob>)` rules: `*` matches any sequence and everything else is literal, matched against the whole trimmed segment. A bare `Bash` rule matches everything. `fn profile_allows(profile: &PermissionProfile, command: &str) -> bool` splits on `&&`, `||`, `;`, `|` and `\n`, then returns true only if every non-empty trimmed part matches some allow rule and no part (nor the whole command) matches any deny rule. To fail closed, any command containing `$(`, a backtick, `>` or `<` is treated as not allowed. Doc-comment that the helper models, but is not, the CLI's enforcement.

4. Add exactly ONE new test named `validator_mission_diff_reads_allow_base_sha_diff_only`. No other test name in the crate may contain the substring `validator_mission_diff_reads`; the contract filters on it and requires exactly one match. For both validator roles, built with `for_role(role, &MissionConfig::default(), &[], &[], &[])`:
   (a) `allowed_tools` contains `Bash(git diff $KRANZ_BASE_SHA..HEAD*)`.
   (b) `profile_allows` is true for `git diff $KRANZ_BASE_SHA..HEAD`, `git diff $KRANZ_BASE_SHA..HEAD -- crates/engine/src/permissions.rs docs/design.md`, `git diff --no-ext-diff --no-textconv $KRANZ_BASE_SHA..HEAD` and `printenv KRANZ_BASE_SHA`.
   (c) `profile_allows` is false for each of: `git`; `git -c diff.external=/tmp/x diff $KRANZ_BASE_SHA..HEAD`; `git diff $KRANZ_BASE_SHA..HEAD --output=/tmp/x`; `git diff --output /tmp/x $KRANZ_BASE_SHA..HEAD`; `git diff --out=/tmp/x $KRANZ_BASE_SHA..HEAD`; `git diff --ext-diff $KRANZ_BASE_SHA..HEAD`; `git diff $KRANZ_BASE_SHA..HEAD --ext`; `git diff --textconv $KRANZ_BASE_SHA..HEAD`; `git diff --textc $KRANZ_BASE_SHA..HEAD`; `git log -p --ext-diff $KRANZ_BASE_SHA..HEAD`; `git log -p --output=/tmp/x`; `git show --textconv HEAD`; `git difftool -y -x sh $KRANZ_BASE_SHA..HEAD`; `GIT_EXTERNAL_DIFF=/tmp/x git diff $KRANZ_BASE_SHA..HEAD`; `git diff $KRANZ_BASE_SHA..HEAD -- a.rs | grep -nE x`; `git diff $KRANZ_BASE_SHA..HEAD | head -20`; `git diff $KRANZ_BASE_SHA..HEAD > /tmp/x`; `git diff $KRANZ_BASE_SHA..HEAD; touch /tmp/x`; `git commit -m x`; `git checkout main`.
   (d) The Orchestrator profile's `disallowed_tools` equals `read_only_deny()`, and it contains none of the `VALIDATOR_GIT_DENY` rules and not `Bash(git diff $KRANZ_BASE_SHA..HEAD*)` (orchestrator unchanged).
   (e) The Worker profile with no grants has `allowed_tools == vec!["Bash"]` and contains no `VALIDATOR_GIT_DENY` rule (worker unchanged).
   Do NOT assert that other revision ranges (e.g. `git diff HEAD~3..HEAD`) are denied: the existing `Bash(git diff*)`, which the validator prompts rely on for `{startSha}..HEAD`, still allows them, and this mission deliberately leaves it. Note that in a comment on the test.

5. `docs/design.md`: if its §4.7 role/permission table lists validator Bash rules explicitly, add the new allow and deny rules there in one short line. If it does not enumerate them, leave the file untouched.

Testing: run ONLY filtered engine tests. Never run the unfiltered `cargo test -p kranz-engine --lib` or `cargo test --workspace`: the full suite runs past a worker's command limit. Use `cargo test -p kranz-engine --lib permissions::`, `cargo test -p kranz-engine --lib permissions::tests::validator_mission_diff_reads` (must report exactly `1 passed`) and `cargo test -p kranz-engine --test runner_test`. `crates/engine/tests/backend_mock_test.rs` around line 652 asserts `Bash(git diff*)` is present: find that test's name and run it filtered with `cargo test -p kranz-engine --test backend_mock_test <name>`. Run `cargo fmt --all` before committing. Do not touch `orchestrator.rs`, `runner.rs`, the prompts, or any file other than `permissions.rs` and possibly `docs/design.md`.

Done when:
- Both validator profiles contain the allow rule `Bash(git diff $KRANZ_BASE_SHA..HEAD*)`.
- Under the test matcher, `git diff $KRANZ_BASE_SHA..HEAD` with and without `-- <paths>` is allowed for both validator roles.
- Under the test matcher, the following are denied for both validator roles: bare `git`; `git -c ...`; `--output`/`--out=`, `--ext-diff`/`--ext` and `--textconv`/`--textc` on `git diff`, `git log` and `git show`; `git difftool`; env-prefixed `GIT_EXTERNAL_DIFF=... git diff`; piped, redirected and `;`-chained forms; `git commit` and `git checkout`.
- `--no-ext-diff` and `--no-textconv` remain allowed with the sanctioned form.
- The orchestrator profile carries none of the new rules and its deny set equals `read_only_deny()`.
- The worker profile is unchanged: `WORKER_DENY` and the `Role::Worker` arm are byte-identical to the base commit, and the worker's no-grant `allowed_tools` is exactly `["Bash"]`.
- `cargo test -p kranz-engine --lib permissions::tests::validator_mission_diff_reads` reports exactly 1 passed.
- All existing `permissions::` lib tests, `runner_test`, and the `backend_mock_test` test that checks `Bash(git diff*)` still pass.

### 1.2 Validator prompts name the sanctioned mission-diff form and rule out pipes

Context: Kranz validator sessions receive the system prompts in `crates/engine/prompts/validator-scrutiny.md` and `crates/engine/prompts/validator-functional.md`, embedded with `include_str!` in `crates/engine/src/prompts.rs`. Both already contain a paragraph saying `printenv KRANZ_<NAME>` is the pre-approved way to read the session env (scrutiny line ~17, functional line ~18). Both also contain a paragraph telling the validator to use `$KRANZ_BASE_SHA` for comparisons against the pre-mission state. In mission m-dd7d22 a validator piped `git diff <sha>..HEAD -- <files>` into `grep` and `head`. The pipe parts are outside the validator allow-set, so the command parked for a grant and blocked the milestone. A parallel feature adds the allow rule `Bash(git diff $KRANZ_BASE_SHA..HEAD*)` and deny rules for `--output`, `--ext-diff`, `--textconv`, `git -c` and `git difftool`. This feature tells validators about it.

Do exactly this: in BOTH prompt files, immediately after the existing `printenv KRANZ_<NAME>` paragraph, add one paragraph with this content (the wording may be adapted to each file's voice, but the two quoted literals must appear verbatim):

"To read the mission's whole diff against the pre-mission state, run `git diff $KRANZ_BASE_SHA..HEAD`, optionally limited with `-- <path>...`. This is the sanctioned form: it is pre-approved and needs no grant request. Run it bare. Pipes are not permitted: piping it into a filter (`| grep`, `| head`) or redirecting its output parks the milestone on a grant request, so read the unfiltered output, narrow it with `-- <paths>`, and use the Grep and Read tools to search files. Options that write files or run programs (`--output`, `--ext-diff`, `--textconv`, `git -c`, `git difftool`) are denied."

The literal strings `git diff $KRANZ_BASE_SHA..HEAD` and `Pipes are not permitted` must each appear in both files; a contract command greps for them with `grep -qF`. Do not use curly braces `{` or `}` in the new text, because the prompts contain `{startSha}`-style template placeholders. Do not alter any other line of either prompt, and in particular keep the existing `{startSha}..HEAD` instructions and the scrutiny prompt's existing no-pipes sentence. Touch only these two files.

Testing: if `crates/engine/src/prompts.rs` has unit tests (placeholder substitution, size budgets), run them filtered with `cargo test -p kranz-engine --lib prompts::`. Never run the unfiltered `cargo test -p kranz-engine --lib` or `cargo test --workspace`: the full suite runs past a worker's command limit. Also verify with `grep -qF 'git diff $KRANZ_BASE_SHA..HEAD' <file> && grep -qF 'Pipes are not permitted' <file>` for each file.

Done when:
- `crates/engine/prompts/validator-scrutiny.md` contains the literal `git diff $KRANZ_BASE_SHA..HEAD` and the literal `Pipes are not permitted`.
- `crates/engine/prompts/validator-functional.md` contains the literal `git diff $KRANZ_BASE_SHA..HEAD` and the literal `Pipes are not permitted`.
- Both prompts state the form is pre-approved and needs no grant request, and name `--output`, `--ext-diff`, `--textconv`, `git -c` and `git difftool` as denied.
- No new curly-brace template tokens are introduced, and the existing `{startSha}..HEAD` lines and the `printenv KRANZ_<NAME>` paragraph are unchanged.
- `cargo test -p kranz-engine --lib prompts::` passes (if such tests exist).
