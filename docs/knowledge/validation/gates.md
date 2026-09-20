---
title: Mission gates and deterministic safety nets
owner: agent
freshness: check-on-touch
last_verified: 2026-09-20
verified_against:
  - crates/engine/src/reviewer_independence.rs
  - crates/engine/src/sandbox_container.rs
  - AGENTS.md
  - crates/engine/src/orchestrator.rs
  - crates/engine/src/orchestrator/finalization.rs
  - crates/engine/src/command_exec.rs
  - crates/engine/src/contract_controls.rs
  - crates/engine/src/merge.rs
  - crates/engine/src/merge_gate.rs
  - crates/engine/src/scrub.rs
  - crates/engine/src/contract_sweep.rs
  - crates/engine/src/event_log.rs
  - crates/engine/src/runner.rs
  - crates/engine/src/judgement.rs
  - crates/cli/src/commands.rs
  - crates/engine/src/knowledge.rs
  - crates/engine/src/test_capability.rs
  - .github/workflows/ci.yml
  - scripts/check-rust-notices.sh
  - scripts/check-package-licenses.sh
  - scripts/package-release.py
  - scripts/audit-operator-markers.py
  - docs/tickets.md
---

## Explicit external evaluator checks

The schema-5 evaluator implementation is separate from mission-stage consumption.
`load_for_config` refuses configured external evaluators until S5 is wired; legacy
command gates and standards remain unchanged. `rust-linux-external-evaluator`
runs `gate_subprocess_v1` with the Docker opt-in and a pinned synthetic Python
image. Missing Docker/image support fails that job; ordinary workspace tests
print `SKIP-EXTERNAL-EVALUATOR` when the explicit opt-in is absent. These tests
exercise protocol, byte pinning, artifact imports and containment without model
calls. See [external evaluators](../../external-evaluators.md) for API boundaries,
retention and recovery limits.
Container creation consumes the evaluation deadline; inspection and cleanup keep
their short control bounds. The serial CI lane requires the delayed-create,
deadline and cancellation regression without retrying uncertain creation.

## The full-workspace gate suite

Before declaring any Rust change done, run all four — bare, reading raw exit
codes ([AGENTS.md](../../../AGENTS.md) rules 1–3):

```bash
cargo fmt --all --check     # fix with: cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

`.github/workflows/ci.yml` runs fmt/clippy/workspace tests on Ubuntu and
Windows (no standalone `build` job — `cargo test` covers it), a separate macOS
workspace suite, and a wrapped macOS dogfood suite. The Windows lane begins
with the explicit minimal host-preparation step — drive-root and derived
profile-parent metadata ACEs plus the per-boot null-device descriptor — then
runs the production LPAC/AppContainer containment receipts. It ends with a
stock-developer-box regression guard that re-runs the exposed targets from
`cmd.exe` with Git's `usr/bin` stripped from PATH, because the ordinary lane
runs under `pwsh` (which never sets the `=ExitCode` pseudo variable) and with
POSIX coreutils present, and so cannot observe either failure class. The
Ubuntu lane enables unprivileged user namespaces on its ephemeral runner and
executes the real bubblewrap hostile-boundary and warm-overhead receipt. The
MSRV lane runs
`cargo check --workspace --locked` on Rust 1.88. Dedicated jobs cover the
full-history knowledge refresh, supply chain/public-tree checks, Gas City pack,
dashboard, Tauri on macOS/Windows, and Docker. The dashboard lane runs
`npm ci`, high-severity audit, `npx tsc -b`, build, embedded-bundle freshness,
tests, and lint. The Even G2 lane runs install, high-severity audit, tests,
package construction (including type/build checks and license notices), and lint.
Physical glasses acceptance remains a separate operator receipt. Ubuntu and
macOS also build and exercise the bounded ACP compatibility probe with a local
synthetic peer; this check requires no provider credentials or model calls.

The supply-chain job also checks the four package MIT notices, regenerates
the locked Rust dependency notices with pinned cargo-about 0.9.2, and rejects
generic-license fallbacks without upstream source text. Archive tests verify
the binary and notice payload for all five release targets. The dashboard
build generates its bundled dependency notices; the embedded-bundle freshness
check covers that file too. `kranz licenses` exposes the project and dependency
notices from a copied executable outside a checkout.

Windows session and gate completion explicitly retire temporary AppContainer
ACL grants and profiles. Cleanup failure is a non-success result, with Drop as
a retry fallback. Overlapping leases retain shared boundary baselines until
their grants are removed and inheritance is restored.

Public releases require Gitleaks and the committed domain-policy check. An
additional owner-supplied confidentiality vocabulary is optional; when configured,
`KRANZ_REQUIRE_OPERATOR_MARKERS=1` fails on missing/empty input. Tree checks
scan tracked paths and content, while history checks inspect all reachable
objects and refuse shallow clones. Output contains counts, not private terms
or matching content. Ordinary CI exercises this behavior with synthetic
positive/negative fixtures; the release workflow consumes the reviewed Actions
secret when present. An unconfigured optional scan explicitly reports its skip
and is not an owner-vocabulary receipt.

The `knowledge-refresh` job checks out full history, then runs
`cargo run --locked --package kranz -- knowledge-refresh`. Full history is
required: a shallow clone cannot prove whether a cited path changed after a
note's `last_verified` date. Any stale or unverifiable note fails the job.

**Why not `-p kranz-engine`?** A crate-scoped pass hides breakage in the
crate's consumers (cli, server, slack). `--workspace` is the only trustworthy
final regression check. Never pipe a gate (`... | tail -1` once masked a build
failure and shipped a broken push) — run it and read its exit code.

## Anti-vacuity test contract

A contract `command` assertion that names a test filter must guard against a
filter matching **zero** tests (which passes vacuously as `0 passed`). Write
it as ([AGENTS.md](../../../AGENTS.md) rule 5):

```bash
cargo test --workspace <filter> 2>&1 | grep -qE 'test result: ok\. [1-9]'
```

The `[1-9]` forces at least one passing test. Before shipping the filter, also
confirm it does not collide with a pre-existing test name.

## Runtime-gated tests: skip loudly, fail where required

The rule above catches a filter matching zero tests. It does not catch the
neighbouring shape: a test that RUNS, returns early because a tool is missing,
and prints `ok`. libtest reports that identically to a test that did the work,
and the explanatory `eprintln!` is captured and never shown — a passing test's
output is swallowed, and a skipping test passes. So a capability can quietly
stop being exercised while CI keeps reporting success.

That is not hypothetical. `command_available` did not consult `PATHEXT`, so
`sandbox_container::detect()` never found `docker.exe` and the Windows
container tests skipped for the life of that lane. Fixing the lookup ran them
for the first time and they immediately failed on two real bugs.

Gate on [`test_capability`](../../../crates/engine/src/test_capability.rs)
rather than a bare `eprintln!` + `return`:

```rust
let Some(runtime) = detect() else {
    test_capability::skip(capability::CONTAINER, "no runtime on PATH");
    return;
};
```

Two mechanisms, because printing alone is not enough:

- **`KRANZ_REQUIRED_CAPABILITIES`** names what a platform must actually
  exercise. `skip` PANICS when a listed capability is missing, so a lane that
  starts skipping goes red instead of quietly green. CI declares expectations
  up front — ubuntu `git,bwrap,container,grep`; macOS `git,sandbox-exec`;
  Windows only `git`. The container sandbox provider is EVIDENCE-gated, not
  platform-gated: Linux rides its continuous CI receipt, Windows is refused
  outright (POSIX guest paths and `/dev/null` masks), and any other host is
  supported exactly when it passes a bind-mount round trip at run time. A
  runtime can accept a `-v` mount for a path its daemon cannot see and share
  nothing, so `container` stays out of the macOS required list: a Mac only
  qualifies when its runtime shares the paths a mission mounts.
- **`KRANZ_SKIP_LOG`** receives a ledger line per skip. A file survives
  libtest's capture where stdout does not, and each lane prints the ledger
  after the suite, so a green run still shows what it did not exercise.

Container gate launches and timeout cleanup use the same allowlisted host
runtime context. The worker's sanitized environment crosses only through
explicit container flags, so a contract's `DOCKER_HOST` cannot redirect the
host runtime. Explicit empty proxy variables also prevent Docker's client
configuration from injecting proxy credentials into the payload.

## Empty-deliverable safety net

Selected command assertions can carry explicit valid/defective controls.
Approval and final validation run the same approved command in read-only,
contained disposable checkouts and record fresh advisory evidence. A positive
behavioral-check receipt is required: setup errors, zero tests and timeouts
remain inconclusive. Their execution directory is the read-only checkout;
writable sandbox roots remain private scratch. The control-specific runner
retains the leader PID until same-group descendants are killed on completion,
error, cancellation, or timeout. Ordinary gate execution is unchanged.
Controls never replace ordinary validation or the
empty-deliverable gate. See [critical assertion controls](../../contract-controls.md).

`final_gate()` in [finalization.rs](../../../crates/engine/src/orchestrator/finalization.rs)
(feature f-2-2) counts `commits_between(base, "HEAD")` filtered by
`!contract_sweep::is_meta_commit_with_paths` — a meta exemption requires both
an engine subject and permitted mission-artifact paths. A worker cannot hide
source changes merely by using a `[kranz]` subject (see
[contract_sweep.rs](../../../crates/engine/src/contract_sweep.rs)). If **zero**
non-meta feature commits landed, it emits `MissionFailed` ("Refusing to
COMPLETE on an empty deliverable diff") and returns `Failed`. This runs FIRST,
before any contract assertion — a green contract can never override an empty
deliverable ([AGENTS.md](../../../AGENTS.md) rule 8).

## Final-gate judgement (diff against the pinned base)

Once all milestones complete, `final_gate()` runs the mission's
`validation_contract`:

- **`command` assertions** are engine-run via `run_shell_command_sandboxed` in
  `active_root()` with the sanitized `contract_command_env(base_sha)`, which exports
  `KRANZ_BASE_SHA` set to the sha pinned at approval (never the live base
  branch). When enforcement is enabled, the resolved worker sandbox also wraps
  the gate; `off` keeps the environment-only posture. The timeout is 10 minutes
  (`COMMAND_TIMEOUT`). A non-zero exit produces a critical, non-waivable
  `command-assertion` finding.
- **`agent-judgement` assertions** get one orchestrator verdicts turn, shown
  the `diff_stat(base, "HEAD")` where `base` is the pinned `base_sha` (falls
  back to `base_branch` only for legacy missions with no pinned sha).
  Unparseable, missing, or duplicated verdicts fail conservatively. Decision
  turns parse with `runner::parse_decision`, which takes the whole trimmed
  reply or the content of exactly one fenced block. The old greedy
  first-`{`-to-last-`}` span is gone: it read a JSON object the model had quoted
  and explicitly disowned (the 2026-09-01 adversarial audit, H10).

Empty findings → `complete_mission()`, subject to the approved review-evidence
and checkout-integrity checks below. Otherwise findings route through
`convert_findings`: only waivable findings can be cleared by model judgement.
Command failures and enforced Flight Rules findings reject model waivers; an
enforced rule's separately authenticated human waiver must already match its
exact evidence, and a missing manual attestation parks for operator action.
A fix answer reopens the last milestone with fix features (until the fix-cycle cap, then
`MilestoneBlocked`). A third route exists only for `command-assertion`
findings: if the orchestrator judges a failing command assertion
author-broken (a false negative — the requirement is genuinely met, verified
by the milestones that already passed, but the assertion's own command is
wrong), it escalates straight to the operator via `MilestoneBlocked` with the
failing-command evidence attached, spending no fix cycle. The guard in
`convert_findings` honors this verdict only for findings whose
`class == "command-assertion"`; a mislabelled non-command finding falls
through to the normal fix/waive handling.

## What a validator is allowed to run

A validator session's `Bash(<command>*)` allow list comes from the approved
contract's `command` strings plus config `allowValidatorCommands` and operator
grants, folded in by `permissions::for_role`. The commands a worker REPORTS it
ran are not in it: that field is model-authored JSON with no human step between
the report and the rule, so feeding it in let a worker mint an allow rule for
the read-only role. Those commands still reach the validator prompt, labelled
as the untrusted claim they are (the 2026-09-01 adversarial audit).

## Merge pre-gate

`merge_mission` in [merge.rs](../../../crates/engine/src/merge.rs) sequences,
in order, refusing early and leaving base untouched on any failure:

1. `is_clean_tracked()` — refuse a dirty tracked tree (`RefusedDirtyTree`); no
   gates run.
2. Pin the live base and mission tip SHAs. **Secret scan** —
   `scan_unified_diff(diff_full(base_sha, mission_tip_sha))` filtered against
   the allowlist read from the pinned live base;
   `SecretScanFailed` on any unwaived finding.
3. Parse the tracked `.kranz/merge-gates.json` from the pinned live base. A
   missing, invalid, empty, or conditional-only suite fails closed before any
   command; the mission branch cannot weaken the policy judging itself.
4. Merge the pinned mission SHA into the pinned live base in a detached scratch
   worktree. A conflict is aborted and the primary checkout remains untouched.
5. `run_gate_suite` ([merge_gate.rs](../../../crates/engine/src/merge_gate.rs))
   runs applicable repo-defined commands against that exact integration commit
   in order, with a 600-second process-tree timeout and sanitized environment.
   `cwd` must be repo-relative without parent components;
   optional `whenPaths` prefixes select component-specific gates. Stops at the
   first failure → `GateFailed`.
6. Recheck that the live base SHA has not moved, then fast-forward it to the
   exact tested merge commit. The repo-busy lock serializes this transaction
   against runs and sibling Merge requests.

Kranz's tracked suite mirrors its CI: fmt/clippy/workspace tests always, plus
the six dashboard gates when `apps/dashboard` changed. Other repositories
define their own language/toolchain commands; see
[docs/merge-gates.md](../../merge-gates.md).

The engine **never pushes** ([AGENTS.md](../../../AGENTS.md) rule 4); a human
runs `git push`. A non-blocking `StaleBaseWarning` fires when the pinned base
trails ≥1 merge commit already on the live base.

## Reviewer independence

Optional approval-pinned `reviewerIndependence` requirements are checked against
recorded worker backend/model identities after reviewer resolution and before
every primary, retry and confirmation launch. Unknown identity, same-family
fallback or skipping a required reviewer blocks with an event-log explanation;
config changes and replay cannot erase the approval pin. Before milestone
closure (including a skip), final gates, and mission completion, each required
role must also have a successful compatible run in the latest relevant
validation round. Later work, changed review context, or tamper invalidates that
evidence. A pending-only plan revision preserves unaffected completed-prefix
reviews. Final gates and lesson preparation must leave the clean reviewed
checkout unchanged; detected drift records a fresh validation epoch and blocks
until the required reviews run again. Existing validator containment remains
mandatory. See [config composition](../../config-composition.md#reviewer-independence-reviewerindependence)
and [the gate](../../../crates/engine/src/reviewer_independence.rs).

## Secret scanning — three layers

All backed by [scrub.rs](../../../crates/engine/src/scrub.rs) (curated
`OnceLock` regexes for PEM/vendor tokens/headers/connection-strings + one
entropy-gated pass ≥4.0 bits/char; no external deps).

- **Redact-at-write ingest gate:** `EventLog::append_redacting`
  ([event_log.rs](../../../crates/engine/src/event_log.rs)) runs
  `scrub_json_value` over every string leaf before the line reaches
  `events.jsonl`; the `append_with_redaction_audits` wrapper then emits
  `secret.redacted` audit events carrying only `rule_id` + `fingerprint` +
  `location` (never the raw value). The line's integrity fields are computed
  after the scrub, so the chain covers the redacted bytes that land on disk and
  a redaction is never mistaken for tampering. Worker messages also pass
  through `scrub_and_truncate`
  ([runner.rs](../../../crates/engine/src/runner.rs)).
- **Merge pre-gate:** step 2 above.
- **`kranz scan`** ([commands.rs](../../../crates/cli/src/commands.rs) `cmd_scan`):
  `kranz scan --range main..HEAD` (or `--staged`, or default `HEAD`); prints
  findings and exits `2`, else "secret scan passed" exit `0`. CI runs it in a
  dedicated `pull_request_target` workflow as the `secret-scan` job; repository
  contents stay read-only (the token can only report commit statuses), the
  workflow, scanner binary, rules, and allowlist all come from the trusted base
  commit, and proposed commits are fetched only as inert Git data.

Allowlist lives at `.kranz/secret-allowlist` (one fingerprint per line, `#`
comments); add a line only for a reviewed false positive.
