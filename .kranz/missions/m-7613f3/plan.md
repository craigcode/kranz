# Mission plan — m-7613f3

**Goal:** Add a dedicated macos-latest job to .github/workflows/ci.yml that runs cargo test --workspace --no-fail-fast, so the macOS Seatbelt sandbox and unix process-group/kill test paths are exercised in CI while fmt/clippy stay ubuntu-only.

Branch `kranz/mission-m-7613f3` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$2.52 – $12.62** (expected ~$5.50). Rough estimate — live usage is authoritative; based on 41 completed mission(s).

## Considered alternatives

**Chosen approach:** A single small feature: one separate `rust-macos` job (not a matrix extension) plus a committed ruby-backed bash guard script. A separate job keeps fmt/clippy scoping trivial and makes the structural contract cleanly checkable; the guard script lets validators re-verify the wiring without a live GitHub Actions run, which cannot be triggered from a worker session.

Rejected shapes:
- **Add macos-latest to the existing rust matrix and gate the fmt/clippy steps with per-OS `if:` conditions.** — Forces `if: matrix.os != 'macos-latest'` gating on shared steps, entangling windows' current fmt/clippy behaviour and making the 'macOS runs tests only' assertion harder to verify structurally; a separate job is clearer and lower-risk.
- **Express the structural checks as inline ruby one-liners in the contract commands instead of a committed guard script.** — Kranz runs contract commands without a shell, so complex quoted one-liners are brittle to tokenize and cannot be re-run identically by validators; a committed `bash scripts/check-macos-ci.sh` is a single unambiguous argv and doubles as a durable regression guard.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The full workspace test suite (including the macOS-gated Seatbelt sandbox tests and unix process-group/kill tests) passes on macOS.
  `cargo test --workspace --no-fail-fast`
- **[a2]** .github/workflows/ci.yml is valid YAML, declares a job that runs on macos-latest which invokes 'cargo test --workspace --no-fail-fast', does NOT run cargo fmt or cargo clippy, configures Swatinem/rust-cache, and every 'uses:' in the file is pinned to a 40-hex commit SHA.
  `bash scripts/check-macos-ci.sh`
- **[a3]** The mission is CI-only: no Rust product code or Tauri/frontend code was changed (nothing under crates/ or apps/).
  `git diff --quiet $KRANZ_BASE_SHA -- crates apps`
- **[a4]** The change is additive and correctly scoped: the existing ubuntu+windows rust job (fmt, clippy, test), and the msrv, dashboard, docker, and smoke jobs are unchanged in behaviour; only a new macOS test job and the guard script are added. *(agent judgement)*

## Milestone 1 — macOS is tested in CI

### 1.1 Add a dedicated macos-latest test job to ci.yml with a structural guard script

Add macOS test coverage to the existing CI workflow at .github/workflows/ci.yml. Today that workflow has a `rust` job with `strategy.matrix.os: [ubuntu-latest, windows-latest]` running (in order) `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, then `cargo test --workspace --no-fail-fast`; plus `msrv`, `dashboard`, `docker`, and `smoke` jobs. Do NOT touch those existing jobs — leave the ubuntu/windows matrix and its fmt/clippy/test steps exactly as they are.

Deliverable 1 — new job. Add a NEW, separate top-level job (name it `rust-macos`) with `runs-on: macos-latest`. Do NOT extend the existing matrix; a distinct job keeps the scope clean and avoids gating fmt/clippy per-OS. The job's steps, in order:
  1. checkout: `uses: actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5 # v4`
  2. toolchain: `uses: dtolnay/rust-toolchain@4be7066ada62dd38de10e7b70166bc74ed198c30 # stable`
  3. cache: `uses: Swatinem/rust-cache@e18b497796c12c097a38f9edb9d0641fb99eee32 # v2`
  4. git identity (the git_ops tests require it) — mirror the existing job's step: run `git config --global user.name kranz-ci` and `git config --global user.email ci@kranz.local`.
  5. `run: cargo test --workspace --no-fail-fast`
Reuse the EXACT same pinned action SHAs already present elsewhere in the file (shown above). The macOS job must NOT run `cargo fmt` or `cargo clippy` — those stay ubuntu-only to save runner minutes. macOS needs no extra tooling install (it uses the built-in `sandbox-exec`; there is no bubblewrap step). Add a brief comment noting macOS runs tests only, fmt/clippy stay on ubuntu.

Deliverable 2 — guard script. Create `scripts/check-macos-ci.sh` (bash, invoked as `bash scripts/check-macos-ci.sh`, so pipes/heredocs inside it are fine). It must parse `.github/workflows/ci.yml` and exit non-zero with a clear diagnostic message if ANY of the following is false, and exit 0 when all hold: (1) the file parses as YAML; (2) some job has `runs-on: macos-latest`; (3) that macOS job's steps include a `cargo test --workspace` invocation with `--no-fail-fast`; (4) that macOS job does NOT invoke `cargo fmt` or `cargo clippy`; (5) that macOS job uses `Swatinem/rust-cache`; (6) every `uses:` value anywhere in the file is pinned to a 40-hex commit SHA (matches `@[0-9a-f]{40}`). Ruby is available on the runner and ships YAML support — implement the parsing in ruby (e.g. `ruby -ryaml` inside the bash script) rather than fragile grep. Make failure messages name the specific check that failed. IMPORTANT constraints: this script is executed by Kranz WITHOUT a shell as a single argv (`bash scripts/check-macos-ci.sh`), so its own exit code is what matters — do not rely on a leading `!` negation at the call site; encode all pass/fail logic in the script's exit code.

Encode the criteria as executable checks while implementing: run `bash scripts/check-macos-ci.sh` and confirm it passes against your edited ci.yml, and confirm it FAILS if you temporarily remove the cargo test line or unpin an action (then restore). Run `cargo test --workspace --no-fail-fast` locally (this host is macOS) to confirm the mac-gated tests are green. Do NOT modify anything under `crates/` or `apps/`. Report the guard-script output and the tail of the cargo test run as test evidence.

Done when:
- A new job in .github/workflows/ci.yml runs on macos-latest and executes `cargo test --workspace --no-fail-fast`.
- That macOS job configures Swatinem/rust-cache and every `uses:` in ci.yml is pinned to a 40-hex commit SHA.
- That macOS job does NOT run `cargo fmt --all --check` or `cargo clippy`.
- The existing ubuntu+windows `rust` job and the `msrv`, `dashboard`, `docker`, and `smoke` jobs are unchanged.
- `scripts/check-macos-ci.sh` exists, exits 0 against the edited ci.yml, and exits non-zero if the macOS cargo-test line is removed or any action is unpinned.
- No files under `crates/` or `apps/` are modified (`git diff --quiet $KRANZ_BASE_SHA -- crates apps` exits 0).
- `cargo test --workspace --no-fail-fast` passes on this macOS host.
