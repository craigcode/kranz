# Research — m-2a80fd

Evidence behind the approved plan (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates feed `docs/knowledge/`.

## Files & docs read

- docs/knowledge/validation/child-containment.md
- docs/knowledge/validation/gates.md
- crates/engine/src/command_exec.rs
- crates/engine/src/contract_gates.rs
- crates/engine/src/contract_lint.rs
- crates/engine/src/appcontainer_windows.rs
- crates/engine/src/test_shell.rs
- crates/engine/src/orchestrator.rs
- crates/engine/src/orchestrator/finalization.rs
- crates/engine/src/types.rs
- crates/engine/src/lib.rs
- .github/workflows/ci.yml
- .kranz/merge-gates.json
- AGENTS.md
- docs/handoff.md

## External sources

- crates/engine/src/command_exec.rs:408
- crates/engine/src/appcontainer_windows.rs:3273-3321
- crates/engine/src/orchestrator.rs:1527-1543
- .github/workflows/ci.yml:190-425

## Facts

- shell_argv returns bare `cmd` with [`/C`, command] on Windows and `sh -c` elsewhere, and GateSandbox::Disabled reproduces it — `crates/engine/src/command_exec.rs:408-423, 600-609`
- The AppContainer gate path re-encodes args in the trusted helper with its own quote_arg/command_line, so the cmd quoting bug exists there too — `crates/engine/src/appcontainer_windows.rs:3273-3321, 2979-2989 (LaunchPlan.args), test at 5148`
- sandbox.enforce defaults to off, so the reporter most likely hit the uncontained Disabled path — `crates/engine/src/types.rs:1170-1176`
- The approval contract lint always runs contained (raised to fs) or is skipped, so on Windows it runs under AppContainer — `docs/knowledge/validation/child-containment.md:27; crates/engine/src/orchestrator.rs:1499-1510`
- The named contract gates are advisory: approval never blocks on them — `crates/engine/src/orchestrator.rs:1527-1535; crates/engine/src/contract_gates.rs:53-58`
- shell_argv is shared by repo-authored lanes: merge gates, workspace hooks, external gates and pack gate declarations — `crates/engine/src/orchestrator/finalization.rs:418; crates/engine/src/workspace_gate.rs:458; crates/engine/src/orchestrator/external_gates.rs:98; command_exec.rs:1866-1939`
- Windows fixtures hard-code cmd syntax in about 34 places across 18 files — `grep `if exist|exit /b|%VAR%|set X=..&&` over crates/**/*.rs; crates/engine/src/test_shell.rs`
- appcontainer_windows is a cfg(windows) module, so its tests never compile on the macOS mission host — `crates/engine/src/lib.rs:27-28`
- CI's Windows stock-box guard strips Git usr/bin from PATH, so sh-lane fixtures may use only sh builtins — `.github/workflows/ci.yml:415-425`
- Contract command evidence today is text: validation-round verdict lines and final-gate finding evidence — `crates/engine/src/orchestrator.rs:5776-5793; crates/engine/src/orchestrator/finalization.rs:235-246`
- Lesson m-c22294 applies: keep `cargo test --workspace` bound at the final gate and never waive it on 'Operation not permitted' lines — `.kranz/lessons/m-c22294.md`

## Ambiguities & stale docs

- The operator did not answer the planning questions. The plan applies the stated defaults: (1) contained AppContainer stays cmd pending a CI receipt; (2) host-shell-syntax is advisory like its siblings, matching the hint 'records a failing result'; (3) only validation-contract commands switch to sh, and repo-authored lanes keep cmd; (4) Windows behaviour is proven by named CI tests the operator runs after pushing, not by contract commands on this macOS host.
- The goal says 'reject at approval', but the sibling gates and approval code are advisory by design. The plan records FAIL without refusing approval.
- Whether `cargo check --target x86_64-pc-windows-*` works on this host is unverified, so it is a best-effort worker step, not a contract assertion. Windows-only code may only be compile-checked by CI.
- Whether MSYS sh can run under LPAC is unknown. The receipt records the outcome without failing CI and without widening the sandbox.

## Candidate knowledge updates

- docs/knowledge/validation/gates.md: a 'Which shell runs a contract command' section and the host-shell-syntax gate
- docs/knowledge/validation/child-containment.md: sh resolution outside writable roots; AppContainer contract path stays cmd pending the receipt
- docs/scoping/m7-windows-containment.md: the contained-sh receipt and its marker
