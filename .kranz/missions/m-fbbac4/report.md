# Mission report — m-fbbac4

**Goal:** Validators of both roles can read the mission's own diff with the sanctioned form `git diff $KRANZ_BASE_SHA..HEAD [-- <paths>]` without raising a grant request. Both validator prompts say this form is pre-approved and that pipes are not permitted. Git options that write files or run programs stay denied to validators, and a matcher-backed test proves it. The worker profile, the orchestrator profile and the grant flow are unchanged.

Branch `kranz/mission-m-fbbac4` (from `main`). Plan of record: [plan.md](plan.md).

## The plan

1 milestone, 2 features, gated by 8 contract assertions (7 command, 1 judgement).

1. **Validators read the mission diff without a grant; git's write/run options stay denied**
   - Validator git diff rules: sanctioned base-SHA form plus write/run option denies, with a matcher-backed test — Context: Kranz builds Claude CLI permission profiles per role in `crates/engine/src/permissions.rs` (`for_role`).
   - Validator prompts name the sanctioned mission-diff form and rule out pipes — Context: Kranz validator sessions receive the system prompts in `crates/engine/prompts/validator-scrutiny.md` and `crates/engine/prompts/va…

**Chosen approach:** Keep the existing broad validator git rules, which the prompts' `{startSha}..HEAD` instructions depend on.
**Elapsed:** 3h 02m 18s
**Tokens:** 140 in / 83209 out / 3993790 cache read / 394710 cache write
**Cost:** $4.22 actual vs $3.67–$18.37 estimated (expected $8.36)
**Interventions:** 0 (0 steers, 0 grant decisions, 0 revision rulings, 0 operator unblocks)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-fbbac4-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Provider:** local-worktree (source isolation) · template: worktree · no workspace contract
- **Workspace contract:** no workspace contract (source isolation only)
- **Preflight:** preflight: 3 issue(s): [warn] command assertion [a1] runs `cargo test` without anti-vacuity (`ok. [1-9]`); a zero-test filter would pass vacuously; [warn] command assertion [a4] uses 'show', which was… [truncated]

## What shipped

### Milestone 1 — Validators read the mission diff without a grant; git's write/run options stay denied ✅

- ✅ **Validator git diff rules: sanctioned base-SHA form plus write/run option denies, with a matcher-backed test** — 1 run
  Context: Kranz builds Claude CLI permission profiles per role in `crates/engine/src/permissions.rs` (`for_role`).
  - `d44a346` [f-1-1] Sanction base-SHA diff reads for validators, deny git write/run options
  - ✓ Both validator profiles contain the allow rule `Bash(git diff $KRANZ_BASE_SHA..HEAD*)`.
  - ✓ Under the test matcher, `git diff $KRANZ_BASE_SHA..HEAD` with and without `-- <paths>` is allowed for both validator roles.
  - ✓ Under the test matcher, the following are denied for both validator roles: bare `git`; `git -c ...`; `--output`/`--out=`, `--ext-diff`/`--ext` and `--textconv`/`--textc` on `git diff`, `git log` and `git show`; `git difftool`; env-prefixed `GIT_EXTERNAL_DIFF=... git diff`; piped, redirected and `;`-chained forms; `git commit` and `git checkout`.
  - ✓ `--no-ext-diff` and `--no-textconv` remain allowed with the sanctioned form.
  - ✓ The orchestrator profile carries none of the new rules and its deny set equals `read_only_deny()`.
  - ✓ The worker profile is unchanged: `WORKER_DENY` and the `Role::Worker` arm are byte-identical to the base commit, and the worker's no-grant `allowed_tools` is exactly `["Bash"]`.
  - ✓ `cargo test -p kranz-engine --lib permissions::tests::validator_mission_diff_reads` reports exactly 1 passed.
  - ✓ All existing `permissions::` lib tests, `runner_test`, and the `backend_mock_test` test that checks `Bash(git diff*)` still pass.
- ✅ **Validator prompts name the sanctioned mission-diff form and rule out pipes** — 1 run
  Context: Kranz validator sessions receive the system prompts in `crates/engine/prompts/validator-scrutiny.md` and `crates/engine/prompts/va…
  - `d08d393` [f-1-2] Document sanctioned mission-diff form and no-pipes rule in validator prompts
  - ✓ `crates/engine/prompts/validator-scrutiny.md` contains the literal `git diff $KRANZ_BASE_SHA..HEAD` and the literal `Pipes are not permitted`.
  - ✓ `crates/engine/prompts/validator-functional.md` contains the literal `git diff $KRANZ_BASE_SHA..HEAD` and the literal `Pipes are not permitted`.
  - ✓ Both prompts state the form is pre-approved and needs no grant request, and name `--output`, `--ext-diff`, `--textconv`, `git -c` and `git difftool` as denied.
  - ✓ No new curly-brace template tokens are introduced, and the existing `{startSha}..HEAD` lines and the `printenv KRANZ_<NAME>` paragraph are unchanged.
  - ✓ `cargo test -p kranz-engine --lib prompts::` passes (if such tests exist).

## Validation history

### ms-1 round 1 — Validators read the mission diff without a grant; git's write/run options stay denied

No findings.

## Contract outcomes

- ✅ **[a1]** Exactly one test matches the filter `validator_mission_diff_reads` in the engine lib, and it passes. It shows the sanctioned diff form is allowed for both validator roles and that bare `git`, `git -c`, the output, external-diff and textconv options (and their abbreviations), `git difftool`, env-prefixed git, pipes, redirection and chained commands are not allowed. *(command: `cargo test -p kranz-engine --lib permissions::tests::validator_mission_diff_reads 2>&1 | grep -qE 'test result: ok\. 1 passed; 0 failed'`)*
- ✅ **[a2]** Every permissions unit test in the engine lib passes, including the existing printenv, grant, worker-deny and composition-audit tests. *(command: `cargo test -p kranz-engine --lib permissions:: 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed; 0 failed'`)*
- ✅ **[a3]** The runner integration tests pass, including the orchestrator read-only profile test that still expects `Bash(git diff*)`. *(command: `cargo test -p kranz-engine --test runner_test 2>&1 | grep -qE 'test result: ok\. [1-9][0-9]* passed; 0 failed'`)*
- ✅ **[a4]** The worker profile is unchanged: the `WORKER_DENY` constant and the `Role::Worker` arm of `for_role` in permissions.rs are byte-identical to the base commit. *(command: `A=$(git show "$KRANZ_BASE_SHA":crates/engine/src/permissions.rs | sed -n -e '/^const WORKER_DENY/,/^];/p' -e '/Role::Worker => {/,/^        }$/p'); B=$(sed -n -e '/^const WORKER_DENY/,/^];/p' -e '/Role::Worker => {/,/^        }$/p' crates/engine/src/permissions.rs); [ -n "$A" ] && [ "$A" = "$B" ]`)*
- ✅ **[a5]** Both validator prompts name `git diff $KRANZ_BASE_SHA..HEAD` as the sanctioned way to read the mission diff and say that pipes are not permitted. *(command: `for f in crates/engine/prompts/validator-scrutiny.md crates/engine/prompts/validator-functional.md; do grep -qF 'git diff $KRANZ_BASE_SHA..HEAD' "$f" && grep -qF 'Pipes are not permitted' "$f" || exit 1; done`)*
- ✅ **[a6]** The grant flow is unchanged: the orchestrator and runner sources, which park, time out and deny-default grant requests, are not modified by the mission. *(command: `git diff --quiet "$KRANZ_BASE_SHA" -- crates/engine/src/orchestrator.rs crates/engine/src/runner.rs`)*
- ✅ **[a7]** The workspace is rustfmt-clean. *(command: `cargo fmt --all -- --check`)*
- ✅ **[a8]** Judged against the worst command a prompt-injected validator could write, not the one in the m-dd7d22 report: every new validator allow rule is covered by deny rules for options that write files or run programs (`--output`, `--ext-diff`, `--textconv` and their accepted abbreviations, `git difftool`) on `git diff`, `git log` and `git show`. The prompts claim nothing the rules don't enforce, and the orchestrator profile is untouched. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
