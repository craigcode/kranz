---
title: Mission gates and deterministic safety nets
owner: agent
freshness: check-on-touch
last_verified: 2026-07-08
verified_against:
  - AGENTS.md
  - crates/engine/src/orchestrator.rs
  - crates/engine/src/merge.rs
  - crates/engine/src/merge_gate.rs
  - crates/engine/src/scrub.rs
  - crates/engine/src/contract_sweep.rs
  - crates/engine/src/event_log.rs
  - crates/engine/src/runner.rs
  - crates/cli/src/commands.rs
  - .github/workflows/ci.yml
  - docs/tickets.md
---

## The full-workspace gate suite

Before declaring any Rust change done, run all four — bare, reading raw exit
codes ([AGENTS.md](../../../AGENTS.md) rules 1–3):

```bash
cargo fmt --all --check     # fix with: cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace
```

`.github/workflows/ci.yml` runs the same fmt/clippy/test gates on
ubuntu+windows (no standalone `build` job — `cargo test` covers it); its
dashboard job runs `npm ci`, `npx tsc --noEmit`, `npm run build`,
`npm run test`, `npm run lint` under `apps/dashboard`.

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

The `[1-9]` forces at least one passing test. Docs prefer the stricter
`grep -qE 'result: ok\. [1-9][0-9]* passed'` ([docs/tickets.md](../../tickets.md)).

## Empty-deliverable safety net

`final_gate()` in [orchestrator.rs](../../../crates/engine/src/orchestrator.rs)
(feature f-2-2) counts `commits_between(base, "HEAD")` filtered by
`!contract_sweep::is_meta_commit` — meta commits are the `[kranz]`-prefixed
engine commits (plan/report; see
[contract_sweep.rs](../../../crates/engine/src/contract_sweep.rs)). If **zero**
non-meta feature commits landed, it emits `MissionFailed` ("Refusing to
COMPLETE on an empty deliverable diff") and returns `Failed`. This runs FIRST,
before any contract assertion — a green contract can never override an empty
deliverable ([AGENTS.md](../../../AGENTS.md) rule 8).

## Final-gate judgement (diff against the pinned base)

Once all milestones complete, `final_gate()` runs the mission's
`validation_contract`:

- **`command` assertions** are engine-run via `run_shell_command` in
  `active_root()` with `runner::contract_env(base_sha)`, which exports
  `KRANZ_BASE_SHA` set to the sha pinned at approval (never the live base
  branch). 10-minute timeout (`COMMAND_TIMEOUT`). A non-zero exit → critical
  finding.
- **`agent-judgement` assertions** get one orchestrator verdicts turn, shown
  the `diff_stat(base, "HEAD")` where `base` is the pinned `base_sha` (falls
  back to `base_branch` only for legacy missions with no pinned sha).
  Unparseable or missing verdicts fail conservatively.

Empty findings → `complete_mission()`. Otherwise findings route through
`convert_findings`: a waive-all answer completes the mission; a fix answer
reopens the last milestone with fix features (until the fix-cycle cap, then
`MilestoneBlocked`).

## Merge pre-gate

`merge_mission` in [merge.rs](../../../crates/engine/src/merge.rs) sequences,
in order, refusing early and leaving base untouched on any failure:

1. `is_clean_tracked()` — refuse a dirty tracked tree (`RefusedDirtyTree`); no
   gates run.
2. **Secret scan** — `scan_unified_diff(diff_full(base_sha, mission_branch))`
   filtered against the allowlist read from the mission branch;
   `SecretScanFailed` on any unwaived finding.
3. `run_gate_suite` ([merge_gate.rs](../../../crates/engine/src/merge_gate.rs))
   replays CI locally — `cargo fmt --all --check`, then clippy, then
   `cargo test --workspace`, then the five dashboard gates only when
   `dashboard_touched`. Stops at the first failure → `GateFailed`.
4. `merge_no_ff` into the base branch; a conflict rolls back to a clean tree.

The engine **never pushes** ([AGENTS.md](../../../AGENTS.md) rule 4); a human
runs `git push`. A non-blocking `StaleBaseWarning` fires when the pinned base
trails ≥1 merge commit already on the live base.

## Secret scanning — three layers

All backed by [scrub.rs](../../../crates/engine/src/scrub.rs) (curated
`OnceLock` regexes for PEM/vendor tokens/headers/connection-strings + one
entropy-gated pass ≥4.0 bits/char; no external deps).

- **Redact-at-write ingest gate:** `EventLog::append_redacting`
  ([event_log.rs](../../../crates/engine/src/event_log.rs)) runs
  `scrub_json_value` over every string leaf before the line reaches
  `events.jsonl`; the `append_with_redaction_audits` wrapper then emits
  `secret.redacted` audit events carrying only `rule_id` + `fingerprint` +
  `location` (never the raw value). Worker messages also pass through
  `scrub_and_truncate` ([runner.rs](../../../crates/engine/src/runner.rs)).
- **Merge pre-gate:** step 2 above.
- **`kranz scan`** ([commands.rs](../../../crates/cli/src/commands.rs) `cmd_scan`):
  `kranz scan --range main..HEAD` (or `--staged`, or default `HEAD`); prints
  findings and exits `2`, else "secret scan passed" exit `0`. CI runs it as the
  `secret-scan` job.

Allowlist lives at `.kranz/secret-allowlist` (one fingerprint per line, `#`
comments); add a line only for a reviewed false positive.
