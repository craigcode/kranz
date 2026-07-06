# Mission report — m-9f13d2

**Goal:** Every contract-command execution context — worker, validator, and the engine's own final gate — carries the same approval-pinned KRANZ_BASE_SHA, constructed by one shared function.

Branch `kranz/mission-m-9f13d2` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 1h 05m 47s
**Tokens:** 18961 in / 57637 out / 5401243 cache read / 334253 cache write
**Cost:** $12.75 actual vs $3.05–$15.25 estimated (expected $6.10)

## What shipped

### Milestone 1 — KRANZ_BASE_SHA reaches every contract-command execution context via one shared env constructor ✅

- ✅ **Centralize contract-command env construction and thread it through the final gate** — 2 runs, 1 respawn
  - `0036a60` [f-1-1] fix no_base_sha_means_no_gate_env_var to not depend on ambient env

## Validation history

### ms-1 round 1 — KRANZ_BASE_SHA reaches every contract-command execution context via one shared env constructor

No findings.

## Contract outcomes

- ✅ **[a1]** A contract command executed through the engine's final-gate path resolves $KRANZ_BASE_SHA to the SHA pinned at plan approval (not empty). *(command: `cargo test -p kranz-engine base_sha_reaches_final_gate_env 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a2]** When no base SHA is pinned (base_sha = None), the final-gate command environment does not define KRANZ_BASE_SHA — no empty-variable regression. *(command: `cargo test -p kranz-engine no_base_sha_means_no_gate_env_var 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a3]** The existing worker-session and validator-session KRANZ_BASE_SHA env behaviour is preserved: both carry the pinned SHA. *(command: `cargo test -p kranz-engine base_sha_reaches_worker_and_validator_env 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a4]** The worker, validator, and final-gate KRANZ_BASE_SHA environments are all produced by a single shared constructor function; the KRANZ_BASE_SHA insertion is not duplicated across the three execution paths. *(agent judgement)*
- ✅ **[a5]** The full workspace test suite passes with no regressions. *(command: `cargo test --workspace 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
