# Mission report — m-7613f3

**Goal:** Add a dedicated macos-latest job to .github/workflows/ci.yml that runs cargo test --workspace --no-fail-fast, so the macOS Seatbelt sandbox and unix process-group/kill test paths are exercised in CI while fmt/clippy stay ubuntu-only.

Branch `kranz/mission-m-7613f3` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 52m 49s
**Tokens:** 18117 in / 47925 out / 3896269 cache read / 252319 cache write
**Cost:** $8.71 actual vs $2.52–$12.62 estimated (expected $5.50)

## Workspace
- **Isolation:** `worktree`
- **Worker/validator cwd:** `/var/folders/09/j5btthkd6_qb3trdtjs4wwd80000gn/T/kranz-wt-8fc3563e44c243a57868260c-m-7613f3-_integration`
- **Sandbox:** worker `off`; scrutiny `off`; functional `off`
- **Preflight:** preflight: 1 issue(s): [warn] command assertion [a1] runs `cargo test` without anti-vacuity (`ok. [1-9]`); a zero-test filter would pass vacuously

## What shipped

### Milestone 1 — macOS is tested in CI ✅

- ✅ **Add a dedicated macos-latest test job to ci.yml with a structural guard script** — 1 run
  - `287d9d0` [f-1-1] add macos-latest CI job and structural guard script

## Validation history

### ms-1 round 1 — macOS is tested in CI

No findings.

## Contract outcomes

- ✅ **[a1]** The full workspace test suite (including the macOS-gated Seatbelt sandbox tests and unix process-group/kill tests) passes on macOS. *(command: `cargo test --workspace --no-fail-fast`)*
- ✅ **[a2]** .github/workflows/ci.yml is valid YAML, declares a job that runs on macos-latest which invokes 'cargo test --workspace --no-fail-fast', does NOT run cargo fmt or cargo clippy, configures Swatinem/rust-cache, and every 'uses:' in the file is pinned to a 40-hex commit SHA. *(command: `bash scripts/check-macos-ci.sh`)*
- ✅ **[a3]** The mission is CI-only: no Rust product code or Tauri/frontend code was changed (nothing under crates/ or apps/). *(command: `git diff --quiet $KRANZ_BASE_SHA -- crates apps`)*
- ✅ **[a4]** The change is additive and correctly scoped: the existing ubuntu+windows rust job (fmt, clippy, test), and the msrv, dashboard, docker, and smoke jobs are unchanged in behaviour; only a new macOS test job and the guard script are added. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
