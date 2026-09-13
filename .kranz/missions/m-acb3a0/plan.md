# Mission plan — m-acb3a0

**Goal:** Add a config-gated macOS Seatbelt (sandbox-exec) filesystem sandbox per session: generate a write-allowlist profile (session worktree + mission dir + TMPDIR + opt-in extraWrite paths, broad read, no network restriction), wrap the claude spawn when a role opts into enforce:fs, and surface profile-vs-contract failures as preflight issues; Windows out of scope.

Branch `kranz/mission-m-acb3a0` (from `main`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Cost estimate

Estimated **$8.59 – $42.93** (expected ~$17.17). Rough estimate — live usage is authoritative; based on 27 completed mission(s).

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** Per-role config exposes sandbox: { enforce, extraWrite }, defaults to enforce:off with empty extraWrite, and config::validate rejects an unknown/unsupported enforce value (including the next-ticket fs+net) with a clear message. 
  `cargo test --workspace sandbox_config 2>&1 | grep -qE 'result: ok\.'`
- **[a2]** The generated Seatbelt profile write-allows the session cwd, the mission dir, TMPDIR, and each extraWrite path, allows broad file-read, leaves network unrestricted, and does NOT write-allow an arbitrary unrelated path. 
  `cargo test --workspace sandbox_profile 2>&1 | grep -qE 'result: ok\.'`
- **[a3]** Under real macOS enforcement (sandbox-exec -f <profile>), a process can create a file inside the allowlisted session cwd and is denied creating a file outside it (e.g. under $HOME). 
  `cargo test --workspace sandbox_enforcement_macos 2>&1 | grep -qE 'result: ok\.'`
- **[a4]** With enforce:off the spawned claude command is unwrapped (byte-identical to today); with enforce:fs on macOS it becomes sandbox-exec -f <profile> <claude> <args...>; the mock and codex backends ignore the sandbox field. 
  `cargo test --workspace sandbox_wrap 2>&1 | grep -qE 'result: ok\.'`
- **[a5]** When the worker role has enforce:fs, preflight runs each contract command under the profile and yields a warn PreflightIssue naming a command that writes outside the allowlist, none for a benign command, and none when enforce:off; preflight never blocks the run. 
  `cargo test --workspace sandbox_preflight 2>&1 | grep -qE 'result: ok\.'`
- **[a6]** The platform-support decision is a pure function of (enforce, target_os): fs on a non-macOS platform resolves to unsandboxed-with-warning, never to a silent contained state; off resolves to no sandbox on every platform. 
  `cargo test --workspace sandbox_platform 2>&1 | grep -qE 'result: ok\.'`
- **[a7]** Docs record Windows as explicitly out of scope for tier 2 and reflect the shipped tier-2 macOS status. 
  `grep -qiE 'windows.*out of scope' docs/scoping/worker-sandboxing.md`
- **[a8]** The tests behind a1-a6 are non-vacuous: each command assertion's filter matches at least one test that actually exercises the described behavior and would fail if that behavior were removed (judged against the mission diff), so no assertion passes on zero matched tests. *(agent judgement)*

## Milestone 1 — Containment primitive: Seatbelt profile + sandbox config surface

### 1.1 Per-role sandbox config surface

Add a per-role `sandbox` config surface to `crates/engine/src/types.rs` and validation in `crates/engine/src/config.rs`.

Context: `RoleConfig` (types.rs:327) already carries per-role `model`/`reasoningEffort`/`tools`/`backend`, deserialized camelCase and merged via the layered config in config.rs (defaults < ~/.kranz/config.json < <repo>/.kranz/config.json). `MissionConfig::default()` (types.rs:393) sets each role's defaults. `config::validate` (config.rs:97) already validates efforts, backend scoping, etc.

Do:
1. Add an enum `SandboxEnforce` with variants `Off` and `Fs`, `#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]`, `#[serde(rename_all = "lowercase")]`, `#[default] Off`. Serde must accept the JSON strings "off" and "fs".
2. Add a struct `SandboxConfig { enforce: SandboxEnforce, extra_write: Vec<String> }` with `#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]`, `#[serde(rename_all = "camelCase", default)]`. `extra_write` holds filesystem paths the operator opts into as writable (e.g. "~/.cargo", npm cache) — store them as raw strings, do NOT expand or canonicalize here.
3. Add `#[serde(default)] pub sandbox: SandboxConfig` to `RoleConfig`. It must default to `SandboxConfig { enforce: Off, extra_write: vec![] }` when absent from config JSON. Ensure every `RoleConfig` literal in `MissionConfig::default()` (orchestrator, worker, validatorScrutiny, validatorFunctional) and any test/fixture `RoleConfig` literals in the engine crate still compile (add `sandbox: SandboxConfig::default()`).
4. Serialization: because the default is off/empty and RoleConfig is not fully skip-based, a plain `#[serde(default)]` is fine; you do NOT need skip_serializing_if, but the roundtrip (serialize default RoleConfig -> deserialize) must preserve enforce:off and empty extraWrite.
5. In `config::validate`, after the existing per-role checks, validate each role's `sandbox.enforce`. Since the enum only has Off|Fs, an out-of-range STRING like "fs+net" or "foo" fails at serde deserialization time (in `load_layers`), so add a test proving that a config layer containing `{"worker":{"sandbox":{"enforce":"fs+net"}}}` fails to load with an error message that mentions the offending value or field. Do NOT silently accept fs+net. If you find serde's default error message unclear, add an explicit check, but the requirement is: loading a config with enforce:"fs+net" (the next ticket's value) is a hard error, and enforce:"off"/"fs" both load.

Constraints: do not touch backend.rs, backend_claude.rs, runner.rs, or orchestrator.rs in this feature. Keep the change confined to types.rs and config.rs (+ their tests).

Write tests named so `cargo test --workspace sandbox_config` matches them (e.g. `sandbox_config_defaults_to_off`, `sandbox_config_parses_fs`, `sandbox_config_rejects_fs_plus_net`, `sandbox_config_extra_write_roundtrips`). Tests must be non-vacuous: assert the actual default value, that "fs" parses to Fs, that a full MissionConfig with a project layer setting worker.sandbox.enforce=fs deserializes correctly, and that enforce:"fs+net" is rejected.

Done when:
- A default MissionConfig has every role's sandbox.enforce == Off and extra_write empty; a test named with the `sandbox_config` prefix asserts this and would fail if the default changed.
- A config layer `{"worker":{"sandbox":{"enforce":"fs","extraWrite":["~/.cargo"]}}}` merges so that worker.sandbox.enforce == Fs and extra_write == ["~/.cargo"], proven by a `sandbox_config`-matching test.
- Loading a config layer with `"enforce":"fs+net"` (the next ticket's value) is a hard error naming the offending value or field; a `sandbox_config`-matching test asserts the load fails.
- `cargo test --workspace sandbox_config 2>&1 | grep -qE 'result: ok\.'` succeeds and matches at least one test.

### 1.2 Seatbelt profile generator + macOS enforcement proof

Add a new module `crates/engine/src/sandbox.rs` (declare `mod sandbox;` / `pub mod sandbox;` in lib.rs as appropriate) that generates a macOS Seatbelt (SBPL) profile string, plus a `#[cfg(target_os="macos")]` integration test proving real enforcement.

Context: Seatbelt profiles are consumed by `sandbox-exec -f <profile-file> <cmd>`. The goal is a filesystem write-allowlist with broad read and NO network restriction (network is the next ticket). Design of record: docs/scoping/worker-sandboxing.md tier 2.

Do:
1. Define an input struct, e.g. `pub struct SandboxInputs { pub session_cwd: PathBuf, pub mission_dir: PathBuf, pub tmpdir: PathBuf, pub extra_write: Vec<PathBuf> }`.
2. `pub fn generate_profile(inputs: &SandboxInputs) -> String` returning an SBPL profile that:
   - starts with `(version 1)` and `(deny default)`;
   - `(allow process*)`, `(allow signal (target self))`, and enough sysctl/mach basics that a normal process (a shell, node) can run — keep this pragmatic; the macOS test below is the real check;
   - `(allow file-read* )` broadly (unrestricted read);
   - `(allow file-write* ...)` limited to subpaths of: session_cwd, mission_dir, tmpdir, and each extra_write entry, using `(subpath "...")` literals. Canonicalize/absolutize each path before emitting; if a path does not exist yet still emit its absolute form. Escape any embedded quotes/backslashes in emitted path literals.
   - imposes NO network restriction (do not add any `(deny network*)`; network is allowed by virtue of not being denied, but do not rely on `(deny default)` blocking it — explicitly `(allow network*)` so the profile is network-open as intended for this tier).
3. Provide a small helper to write the profile to a file and return the path, or leave file-writing to the spawn feature — your choice, but if you add it keep it here (e.g. `pub fn write_profile_file(dir: &Path, profile: &str) -> std::io::Result<PathBuf>`), writing to a uniquely-named file (uuid) under the given dir.

Tests (name so `cargo test --workspace sandbox_profile` and `cargo test --workspace sandbox_enforcement_macos` match):
- Cross-platform unit tests (`sandbox_profile_*`): assert the profile contains `(deny default)`, a `file-write*` subpath rule for session_cwd, mission_dir, tmpdir and each extra_write path, an `allow file-read*`, an `allow network*`, and that a path NOT in any allowlist input does NOT appear as a `file-write*` subpath. These must be non-vacuous (build inputs with concrete temp dirs and assert exact substrings).
- `#[cfg(target_os="macos")] sandbox_enforcement_macos_*`: create a temp dir as session_cwd, generate the profile, write it to a file, then run `sandbox-exec -f <profile> /bin/sh -c 'echo hi > <session_cwd>/inside.txt'` and assert it succeeds and the file exists; run `sandbox-exec -f <profile> /bin/sh -c 'echo hi > $HOME/kranz_sandbox_should_fail_<uuid>'` (or a path under an env HOME you control) and assert it FAILS (non-zero exit / file absent). The denied-write test MUST assert the write did not happen — it must fail if enforcement is broken. Skip gracefully (not fail) only if `sandbox-exec` is entirely absent, but on a normal macOS host it is present.

Constraints: do not modify backend.rs, backend_claude.rs, runner.rs, orchestrator.rs, config.rs, or types.rs in this feature. This feature is the pure primitive + its enforcement proof only.

Done when:
- `generate_profile` emits `(version 1)`, `(deny default)`, an `allow file-read*`, an `allow network*`, and a `file-write*` `(subpath ...)` rule for each of session_cwd, mission_dir, tmpdir, and every extra_write path; a `sandbox_profile`-matching test asserts each substring against concrete inputs.
- A path that is none of the allowlist inputs does not appear as a `file-write*` subpath in the generated profile; a `sandbox_profile`-matching test asserts its absence.
- On macOS, a process run via `sandbox-exec -f <profile>` can create a file inside session_cwd and is denied creating one outside it; a `#[cfg(target_os="macos")]` test named to match `sandbox_enforcement_macos` proves both, and the denied case fails if the write unexpectedly succeeds.
- `cargo test --workspace sandbox_profile 2>&1 | grep -qE 'result: ok\.'` and `cargo test --workspace sandbox_enforcement_macos 2>&1 | grep -qE 'result: ok\.'` both succeed, each matching at least one test.


## Milestone 2 — Enforced spawn + sandbox preflight

### 2.1 SessionSpec sandbox field, resolve wiring, and platform decision

Thread a resolved sandbox policy onto `SessionSpec` and populate it for worker + validator sessions.

AUTHORIZATION: `crates/engine/src/backend.rs` is marked a contract file ("report instead of editing"). This feature is explicitly authorized by the design of record (docs/scoping/worker-sandboxing.md: "SessionSpec grows a sandbox field; backend_claude wraps the spawn") to add ONE additive optional field to SessionSpec. Keep the edit minimal and additive; do not change existing fields or seam semantics.

Context: `SessionSpec` (backend.rs:32) is built in `runner.rs` for workers (run_worker_in ~line 684) and validators (run_validator_in ~line 849), each with a `session_cwd` (the worktree in worktree mode, else repo_root) and `spec.env` from `contract_env`. `MissionPaths` (paths.rs) provides `mission_dir()`. Role sandbox config comes from f1.1 (`RoleConfig.sandbox`). The profile generator + SandboxInputs come from f1.2 (`crate::sandbox`).

Do:
1. Define `pub struct ResolvedSandbox { pub inputs: crate::sandbox::SandboxInputs, /* the generated profile is produced at spawn time from inputs, or store the profile string too if convenient */ }` (a plain data carrier; keep it Clone + Debug). It represents an ENFORCED fs sandbox; if a role is off or the platform is unsupported, no ResolvedSandbox is attached.
2. Add `pub sandbox: Option<ResolvedSandbox>` to `SessionSpec` (the authorized additive field). Update every `SessionSpec { .. }` literal in the workspace (runner.rs x2, orchestrator.rs, backend_codex.rs, and all engine tests: backend_mock_test.rs, runner_test.rs, backend_claude_test.rs) to set `sandbox: None` so the workspace compiles.
3. Add a pure decision fn, e.g. `pub fn platform_support(enforce: SandboxEnforce, target_os: &str) -> SandboxDecision` returning an enum `SandboxDecision { Off, Enforce, UnsupportedWarn }`: Off when enforce==Off (any OS); Enforce when enforce==Fs && target_os=="macos"; UnsupportedWarn when enforce==Fs && target_os!="macos". This takes target_os as a parameter so it is testable cross-platform. Put it in `crate::sandbox`.
4. Add a resolve helper, e.g. `pub fn resolve_for_session(role_sandbox: &SandboxConfig, session_cwd: &Path, mission_dir: &Path) -> (Option<ResolvedSandbox>, Option<String>)` returning the ResolvedSandbox (Some only when the decision is Enforce) and an optional warning string (Some when UnsupportedWarn, so the caller can log it once). Use `std::env::consts::OS` for the current target_os inside this helper (but the underlying decision fn stays param-based for testing). TMPDIR: read the `TMPDIR` env var, falling back to `std::env::temp_dir()`. Expand a leading `~/` in each extra_write entry using the HOME env var; leave other entries as-is; convert to PathBuf.
5. Wire `resolve_for_session` into run_worker_in and run_validator_in: compute `(spec.sandbox, warn)` from `cfg.role(role).sandbox`, the session_cwd, and `paths.mission_dir()`; set the field; if `warn` is Some, log it via `tracing::warn!` once (do not emit a hard error). Do NOT wire the orchestrator session.

Constraints: do NOT modify backend_claude.rs spawn logic in this feature (that is f2.2) — only populate the field and the resolve/decision logic. The mock and codex backends must keep ignoring the field (they already will, since only backend_claude reads it in f2.2).

Tests (name to match `cargo test --workspace sandbox_platform` and, for resolve, `sandbox_resolve` or reuse sandbox_platform): assert platform_support(Off, "macos")==Off, (Fs,"macos")==Enforce, (Fs,"linux")==UnsupportedWarn, (Off,"linux")==Off; assert resolve_for_session with enforce:off yields (None, None); with enforce:fs on macos yields Some(ResolvedSandbox) whose inputs include the given session_cwd and mission_dir and a tmpdir; assert a leading `~/` in extra_write is expanded using HOME. Guard the on-macos resolve assertions with `#[cfg(target_os="macos")]` where they depend on the current OS.

Done when:
- `SessionSpec` has an additive `sandbox: Option<ResolvedSandbox>` field; the whole workspace compiles with all existing SessionSpec literals setting it to None; existing backend/runner tests still pass.
- `platform_support` is a pure fn of (enforce, target_os): (Off,*)->Off, (Fs,"macos")->Enforce, (Fs, non-macos)->UnsupportedWarn; a `sandbox_platform`-matching test asserts all four cases and would fail if any mapping changed.
- `resolve_for_session` returns None for enforce:off, and on macOS returns Some with inputs carrying the passed session_cwd, mission_dir, and a tmpdir, and expands a leading `~/` in extra_write via HOME; a matching non-vacuous test asserts this.
- run_worker_in and run_validator_in populate spec.sandbox from role config and log a one-time warning (not an error) on an unsupported platform; the orchestrator session is left unwired.
- `cargo test --workspace sandbox_platform 2>&1 | grep -qE 'result: ok\.'` succeeds and matches at least one test.

### 2.2 backend_claude wraps the spawn under sandbox-exec

Make `ClaudeBackend::start` (crates/engine/src/backend_claude.rs:602) launch the claude process under `sandbox-exec` when the SessionSpec carries an enforced sandbox on macOS.

Context: today `start` does `Command::new(&self.binary).args(&build_args(&spec)).current_dir(&spec.cwd).envs(&spec.env)`. On unix it also calls `command.process_group(0)` for tree-kill; on windows it assigns a Job Object after spawn. `spec.sandbox: Option<ResolvedSandbox>` comes from f2.1; the profile generator and `write_profile_file` come from f1.2 (`crate::sandbox`).

Do:
1. Add a pure builder, e.g. `pub fn sandbox_command(profile_path: &Path, binary: &Path, args: &[String]) -> (PathBuf, Vec<String>)` (or return the full argv) that yields program `sandbox-exec` and args `["-f", <profile_path>, <binary>, <args...>]`. Keep it pure so it is unit-testable without spawning.
2. In `start`, BEFORE building the Command: if `spec.sandbox` is Some AND `cfg!(target_os="macos")`, generate the profile from the ResolvedSandbox inputs (crate::sandbox::generate_profile), write it to a file (under the mission dir if available via the inputs, else TMPDIR) via write_profile_file, and construct the Command as `Command::new("sandbox-exec").args(["-f", profile_path, binary, ...build_args])`. Otherwise construct the Command exactly as today (unwrapped). If `spec.sandbox` is Some but NOT macos, log `tracing::warn!` once that enforcement is unavailable on this platform and run unwrapped (do not fail).
3. Preserve ALL existing spawn behavior in both branches: `current_dir(&spec.cwd)`, `envs(&spec.env)`, stdin/stdout/stderr wiring, `kill_on_drop(true)`, unix `process_group(0)`, and the windows Job Object assignment. The sandbox-exec wrapper must remain the process-group leader / Job Object member so tree-kill still works (sandbox-exec execs the child in-process, so the pgid/job semantics carry through — keep process_group(0) on the outer sandbox-exec Command).
4. The profile file should be cleaned up on a best-effort basis if practical, but correctness (leaving a temp profile behind) is acceptable; do not add complex lifecycle management.

Tests (name to match `cargo test --workspace sandbox_wrap`):
- Pure builder tests (cross-platform): `sandbox_command` given a profile path, binary, and args produces program `sandbox-exec` with args `[-f, <profile>, <binary>, <args...>]` in that order; non-vacuous exact-vector assertion.
- A test proving enforce:off / spec.sandbox==None leaves the argv/command construction unchanged (the existing `build_args` path is untouched) — assert against `build_args` output directly.
- Backend-ignore: construct a SessionSpec with `sandbox: Some(..)` and assert the mock backend (and, if cheaply testable, codex) do not consult it / behave unchanged — at minimum assert that only backend_claude references the field (a compile/behavior test).
- `#[cfg(target_os="macos")]` end-to-end: use a tiny stub 'binary' (e.g. `/bin/sh` script or `/usr/bin/true`) as the ClaudeBackend binary with a ResolvedSandbox whose session_cwd is a temp dir, and assert the process actually launches under sandbox-exec (e.g. the stub writes inside session_cwd and that succeeds, or a write outside is denied). If wiring a full ClaudeSession is too heavy, assert via `sandbox_command` + a direct `std::process::Command` run of sandbox-exec with the generated profile that inside-writes succeed and outside-writes are denied (mirrors f1.2's proof but through the backend's builder).

Constraints: keep the diff focused on backend_claude.rs (+ its tests). Do not change the AgentBackend trait or SessionSpec shape.

Done when:
- `sandbox_command` (pure builder) produces program `sandbox-exec` and args `["-f", <profile>, <binary>, <args...>]` in exact order; a `sandbox_wrap`-matching test asserts the exact vector.
- With `spec.sandbox == None` (enforce:off) the spawn command is constructed exactly as before (unwrapped) — a `sandbox_wrap`-matching test asserts the argv equals the pre-existing `build_args`-based command with no sandbox-exec prefix.
- On macOS with an enforced ResolvedSandbox, the claude process is launched under `sandbox-exec -f <profile>`, and existing spawn behavior (cwd, env, tree-kill process group) is preserved; a `#[cfg(target_os="macos")]` test demonstrates the wrapped launch enforces the write-allowlist.
- The mock and codex backends ignore the sandbox field (behavior unchanged); a test or the focused diff confirms only backend_claude consults it.
- `cargo test --workspace sandbox_wrap 2>&1 | grep -qE 'result: ok\.'` succeeds and matches at least one test.

### 2.3 Sandbox preflight probe + docs

Extend the mission preflight so that when the worker role opts into fs enforcement on macOS, the contract's validation commands are run under the generated worker profile and any failure surfaces as an advisory preflight issue.

Context: `MissionEngine::preflight()` (crates/engine/src/orchestrator.rs:534) already returns `Vec<PreflightIssue>` (`severity: "warn"|"error"`, `message`), advisory only, surfaced once as an `orchestrator.decision`, never blocking (the final contract gate is authoritative). It iterates `self.state.mission.validation_contract` command assertions. The worker sandbox config is `self.state.config.worker.sandbox`; paths via `self.paths` (repo_root, mission_dir()); the profile generator + resolve helpers come from f1.2/f2.1 (`crate::sandbox`).

Do:
1. In `preflight()`, after the existing checks, add a block: if `self.state.config.worker.sandbox.enforce == SandboxEnforce::Fs` AND `cfg!(target_os="macos")`, resolve the worker sandbox for a representative session (session_cwd = repo_root or the mission worktree root if you can obtain it cheaply — repo_root is acceptable, state the choice in a comment; mission_dir = self.paths.mission_dir()), generate the profile, write it to a temp file, and for each distinct contract `command` assertion, run the command under `sandbox-exec -f <profile> /bin/sh -c '<command>'` with a bounded wall-clock (e.g. a few seconds; reuse existing patterns if any, else a simple std::process with a timeout guard is acceptable — keep it best-effort). If the command exits non-zero UNDER the sandbox, push a `PreflightIssue { severity: "warn", message: "command assertion [<id>] fails under the fs sandbox profile: <stderr tail>" }`.
2. Do not run this probe when worker enforce is off, or on non-macOS: those paths must add zero sandbox preflight issues. Never escalate to `error` and never block the run.
3. Keep the probe cheap and safe: dedupe commands like the existing program probe; cap the number probed if needed; truncate stderr in the message. Avoid running commands that are obviously destructive is out of scope — treat contract commands as trusted (they are the operator's own contract).
4. Docs: update `docs/scoping/worker-sandboxing.md` — in the tier-2 section, record the shipped status of this ticket (Seatbelt profile generation, sandbox config surface, enforced spawn, preflight probe) and ensure the document explicitly states Windows is out of scope for tier 2 (a line matching /windows.*out of scope/i must be present — the existing tier-2 bullet already says this; confirm/keep it and add the shipped-status note).

Tests (name to match `cargo test --workspace sandbox_preflight`):
- Cross-platform: build a MissionEngine (reuse existing test harness/helpers for constructing an engine with a given config + contract) with worker.sandbox.enforce=off and a contract command; assert `preflight()` yields ZERO sandbox-related issues (the probe is inert when off). Also assert on a non-macos build path the probe is inert (guard with `#[cfg(not(target_os="macos"))]`).
- `#[cfg(target_os="macos")] sandbox_preflight_*`: with worker.sandbox.enforce=fs, a contract command that writes OUTSIDE the allowlist (e.g. `sh -c 'echo x > $HOME/kranz_pf_<uuid>'`) yields a `warn` PreflightIssue naming that assertion id; a benign contract command (e.g. `true`) yields no sandbox issue; assert preflight never returns an `error` for the sandbox probe and the mission is not blocked.

Constraints: keep changes to orchestrator.rs (preflight) + the docs file (+ tests). Reuse `crate::sandbox` helpers; do not duplicate profile generation.

Done when:
- When worker.sandbox.enforce == Fs on macOS, `preflight()` runs each contract command under the generated worker profile and returns a `warn` PreflightIssue (naming the assertion id) for a command that writes outside the allowlist; a `#[cfg(target_os="macos")]` `sandbox_preflight`-matching test proves it.
- A benign contract command yields no sandbox preflight issue, and the probe never returns `error` nor blocks the run; a matching test asserts this.
- When worker.sandbox.enforce == off (or on a non-macos build), the probe adds zero sandbox preflight issues; a cross-platform `sandbox_preflight`-matching test asserts inertness.
- `docs/scoping/worker-sandboxing.md` records the shipped tier-2 status and contains a line matching /windows.*out of scope/i so that `grep -qiE 'windows.*out of scope' docs/scoping/worker-sandboxing.md` succeeds.
- `cargo test --workspace sandbox_preflight 2>&1 | grep -qE 'result: ok\.'` succeeds and matches at least one test.

