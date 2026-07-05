# Mission plan — m-2bc647

**Goal:** Enforce a scrutiny floor in `kranz exec` and close two long-parked gaps (SessionSpec `tools` plumbing; a documented finding for piped line-mode readline), with no new dependencies.

Branch `kranz/mission-m-2bc647` (from `ring-approve`). Approved plan of record; the machine-readable twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.

## Validation contract

Defined before any feature; gates mission completion.

- **[a1]** The whole workspace builds and its tests pass. 
  `cargo test --workspace`
- **[a2]** Clippy is clean across the workspace with warnings denied. 
  `cargo clippy --workspace --all-targets -- -D warnings`
- **[a3]** `kranz exec` refuses a mission whose effective config has skipScrutiny=true, exiting non-zero with an error that names `--allow-unvalidated`, and proceeds when the flag (or KRANZ_ALLOW_UNVALIDATED=1) is supplied — proven by a CLI-level test that never spawns claude. 
  `cargo test -p kranz-cli --test exec_test`
- **[a4]** The scrutiny gate is evaluated after config load and before MissionEngine::create in cmd_exec, so a refused run creates no .kranz/missions entry. *(agent judgement)*
- **[a5]** Per-role `tools` flows from MissionConfig through SessionSpec to the backend: the mock backend's started_specs reflect a configured worker tools list, and build_args emits the tool-restriction flag only when the list is non-empty. 
  `cargo test -p kranz-engine`
- **[a6]** No new readline-style third-party dependency was introduced anywhere in the mission. 
  `bash -c '! git diff "$KRANZ_BASE_SHA" -- "*Cargo.toml" | grep -Eiq "rustyline|reedline|linefeed|liner|termion|dialoguer"'`
- **[a7]** The piped line-mode readline item is resolved as either working minimal editing on the real-TTY line-mode fallback (with no new dependencies) or a docs/handoff.md note explaining why piped mode cannot support interactive editing. *(agent judgement)*

## Milestone 1 — Unattended scrutiny floor in `kranz exec`

### 1.1 Refuse skipScrutiny runs in exec unless explicitly overridden

Add an unattended scrutiny floor to `kranz exec`. Background: `kranz exec` (crates/cli/src/exec.rs) is the fully-headless CI entry point — it loads config, builds the backend, then `MissionEngine::create` creates the mission directory `.kranz/missions/<id>`, auto-approves the plan, and runs it with NO human present. docs/gascity.md lesson 3 records a live incident where an autonomous mission with skipScrutiny+skipFunctional had no adversarial reader and passed its own tautological acceptance. Interactive `kranz run`/`kranz plan` must NOT be gated (a human is present) — only `exec`.

Implement:
1. In crates/cli/src/cli.rs, add a boolean flag to the `Exec` subcommand variant (around line 179): `#[arg(long)] allow_unvalidated: bool`, with a doc comment explaining it overrides the unattended scrutiny floor. Follow the existing field style (the `yes`/`max_cycles`/`push` fields).
2. Thread it through the dispatch in crates/cli/src/commands.rs (the `Command::Exec { .. }` arm near line 120, which calls `crate::exec::cmd_exec(...)`) into `cmd_exec`'s signature in crates/cli/src/exec.rs.
3. In exec.rs, add a PURE, unit-testable gate function, e.g. `pub fn scrutiny_gate(skip_scrutiny: bool, allow_unvalidated: bool) -> Result<(), String>` that returns an Err naming `--allow-unvalidated` when `skip_scrutiny && !allow_unvalidated`, else Ok. The error message must (a) name the `--allow-unvalidated` override and (b) state the letter-over-spirit rationale (an unattended mission with the scrutiny validator disabled has no adversarial reader and can satisfy its acceptance tautologically — cite docs/gascity.md lesson 3 in spirit).
4. In `cmd_exec`, call the gate IMMEDIATELY after `let cfg = load_config(&repo, dangerously_allow_all)?;` (exec.rs:94) and BEFORE `MissionEngine::create` (exec.rs:98). Compute the effective override as `allow_unvalidated || std::env::var("KRANZ_ALLOW_UNVALIDATED").ok().as_deref() == Some("1")` — honoring the env var keeps the existing Gas City pack (packaging/gascity/bin/kranz-dispatch, which sets KRANZ_ALLOW_UNVALIDATED=1 when it deliberately permits an unvalidated rig) working. On gate failure, print the message to stderr (eprintln!) and return a non-zero exit code (reuse an existing convention or return Ok(1)); do NOT create the mission. Since the gate is before `create`, no `.kranz/missions/<id>` directory is written.
5. Do NOT gate `kranz run`, `kranz plan`, `kranz work`, or `kranz draft`.

Tests (in crates/cli/tests/exec_test.rs, following the existing patterns there — clap `try_parse_from` for flags, direct calls for pure fns; never spawn a real claude binary):
- Assert the `Exec` variant parses `--allow-unvalidated` (destructure and check the bool), and that it defaults to false.
- Unit-test `scrutiny_gate`: (skip_scrutiny=true, allow=false) → Err whose message contains "--allow-unvalidated"; (true, true) → Ok; (false, false) → Ok.
- If practical without spawning a backend, add a test that a refused `cmd_exec` invocation against a temp repo whose .kranz/config.json has {"skipScrutiny": true} leaves no .kranz/missions entry — but if `cmd_exec` cannot be exercised without a backend, the pure gate test plus asserting call-order in code is sufficient (mirror how exec_test.rs already only exercises pure helpers).

Keep everything `cargo clippy --workspace --all-targets -- -D warnings` clean.

Done when:
- `cargo test -p kranz-cli --test exec_test` passes.
- The Exec subcommand parses `--allow-unvalidated` and it defaults to false.
- scrutiny_gate(true, false) returns an error whose text contains "--allow-unvalidated"; scrutiny_gate(true, true) and scrutiny_gate(false, false) return Ok.
- In cmd_exec the gate is called after load_config and before MissionEngine::create, and honors KRANZ_ALLOW_UNVALIDATED=1 as equivalent to the flag.
- `kranz run`/`kranz plan` are not gated.


## Milestone 2 — Per-role tool restriction plumbed config → SessionSpec → backend

### 2.1 Add SessionSpec.tools and per-role config tools, emitted only when non-empty

Wire a per-role tool-restriction allow-list from config to the claude backend. This is an AUTHORIZED contract change: crates/engine/src/backend.rs is normally a contract file ("do not modify"), but this plan explicitly sanctions adding one field to SessionSpec — proceed with the edit; do not merely report it.

Scope decision (follow exactly): the field is OPT-IN and config-driven. Default behavior must be byte-identical — no `--tools` flag is emitted unless a user configures a non-empty per-role tools list. Do NOT auto-wire the existing read-only `INSPECT_TOOLS`/`PermissionProfile.tools` into the new flag (design.md's "Verified CLI behavior" only verifies `--allowedTools`/`--disallowedTools` as input flags; emitting an unverified `--tools` on every session is a regression risk). Leave `PermissionProfile.tools`, `permissions::for_role`, and `permissions::apply` behaviorally UNCHANGED (this keeps crates/engine/tests/runner_test.rs:164 and :196, which assert profile.tools, green).

Implement:
1. crates/engine/src/backend.rs: add a field to `SessionSpec` (place it next to `disallowed_tools`): `/// Passed to `--tools` (the built-in exclusive tool allow-list): empty = CLI default set, no flag emitted. Distinct from `allowed_tools`/`disallowed_tools`, which are permission patterns.\n pub tools: Vec<String>,`.
2. crates/engine/src/types.rs: add to `RoleConfig` (struct near line 313): `#[serde(default, skip_serializing_if = "Vec::is_empty")] pub tools: Vec<String>,` (the `skip_serializing_if` keeps default-config serialization snapshots unchanged; camelCase key is `tools`). Add `tools: vec![]` to each of the four RoleConfig literals in `MissionConfig::default()` (orchestrator/worker/validatorScrutiny/validatorFunctional, lines ~354-377).
3. Populate `spec.tools` from config at every SessionSpec construction site, using the existing `cfg.role(role)` accessor (types.rs `pub fn role`): in crates/engine/src/runner.rs the worker-spec builder (~line 645, has the `role` param) set `tools: cfg.role(role).tools.clone()`; the validator-spec builder (~line 755, has the `kind` param) set `tools: cfg.role(kind).tools.clone()`; in crates/engine/src/orchestrator.rs (~line 2536) set `tools: cfg.role(Role::Orchestrator).tools.clone()`. Because `permissions::apply` does not touch `spec.tools`, the value set in the literal survives.
4. Every OTHER SessionSpec struct literal must gain `tools: Vec::new()` (or `vec![]`) so the crate compiles. Find them all with a search for `SessionSpec {` — they include the test literals in crates/engine/tests/backend_claude_test.rs (around lines 44 and 885), crates/engine/tests/backend_mock_test.rs (around line 20), and crates/engine/tests/runner_test.rs (around lines 110 and 348). Add the field to each.
5. crates/engine/src/backend_claude.rs `build_args` (the pure argv builder, ~line 287): after the `disallowed_tools` block (~line 320-323), add `if !spec.tools.is_empty() { args.push("--tools".into()); args.extend(spec.tools.iter().cloned()); }` — mirroring the space-separated multi-arg style already used for `--allowedTools`.
6. Refresh the now-stale documentation: the module doc comment in crates/engine/src/permissions.rs (lines ~8-11) and the `apply` doc comment (lines ~168-170) both say SessionSpec has no field for the built-in tool restriction — update them to note SessionSpec now carries a `tools` field populated from per-role config, while `apply` still leaves the read-only restriction expressed via allowed/disallowed by design.

Tests:
- In crates/engine/tests/backend_claude_test.rs, add a `build_args` unit test: a spec with `tools: vec!["Bash".into(), "Read".into()]` yields argv containing `--tools`, `Bash`, `Read` in order; a spec with empty `tools` yields NO `--tools` token anywhere.
- In crates/engine/tests/backend_mock_test.rs, add a test that runs a worker session through the engine with a MissionConfig whose `worker.tools` is a non-default list, then asserts `MockBackend::started_specs()` for the worker spec has `tools` equal to that list. Follow the existing started_specs assertion pattern at backend_mock_test.rs:272 and the config-construction pattern already in that file.

Keep `cargo test --workspace` green and `cargo clippy --workspace --all-targets -- -D warnings` clean.

Done when:
- SessionSpec has a `tools: Vec<String>` field and RoleConfig has a `tools: Vec<String>` field (serde camelCase `tools`, defaulting to empty and skipped when empty).
- build_args emits `--tools` followed by each tool as a separate arg when spec.tools is non-empty, and emits no `--tools` token when spec.tools is empty (unit-tested).
- A mock-backend test drives a worker session with a configured worker.tools list and asserts started_specs reflect that list on the worker spec.
- Default missions (empty tools) produce byte-identical argv — no `--tools` flag; permissions::for_role/apply and their profile.tools tests remain unchanged and green.
- `cargo test --workspace` passes and clippy is clean.


## Milestone 3 — Piped line-mode planning readline finding

### 3.1 Resolve the piped line-mode readline gap (minimal editing or documented not-applicable)

Resolve the long-parked 'no readline in piped line mode' item WITHOUT adding any dependency. Do NOT add rustyline/reedline/linefeed/liner/termion/dialoguer or any other crate.

Investigate the code paths first:
- crates/cli/src/planning_tui.rs is the full-screen TUI; it already provides complete interactive editing (arrow keys, history, word-nav, Ctrl-U/A/E) via crossterm raw mode and is selected only when BOTH stdin and stdout are TTYs (crates/cli/src/commands.rs:486).
- The line-mode REPL in `cmd_plan` (crates/cli/src/commands.rs, ~lines 517-624) uses `std::io::stdin().read_line()` via the StdinLines reader thread and runs only when NOT both-TTY. Its two cases are: (a) piped stdin — lines are delivered up-front, so there is fundamentally no interactive editing to add; and (b) real-TTY stdin with piped stdout, where the OS cooked-mode terminal line discipline already provides basic editing (backspace, Ctrl-U kill-line, Ctrl-W word-erase) for free.

Decision rule:
- If (and only if) you find a genuinely minimal, zero-new-dependency improvement to the real-TTY line-mode fallback that does not amount to re-implementing the TUI and does not disturb the piped/scripted contract (piped stdin lines must never be discarded; scripts depend on `kranz plan` behavior being stable), implement it with a small pure-unit test.
- Otherwise (the expected outcome), make NO functional code change and instead add a concise note to docs/handoff.md that documents the finding: the full TUI already covers interactive editing on both-TTY sessions; line-mode runs only for piped stdin (no editing possible — lines are pre-delivered) or real-TTY-stdin/piped-stdout (where cooked-mode already gives basic editing); therefore richer editing would require raw-mode handling that belongs to the TUI, and no zero-dependency line-mode enhancement is warranted. Close the item as not-applicable in that note.

Either way: no new dependencies (verify Cargo.toml/Cargo.lock are not modified to add one), and `cargo test --workspace` stays green.

Done when:
- No new third-party dependency is added (no rustyline/reedline/linefeed/liner/termion/dialoguer or similar in any Cargo.toml).
- The item ends in EITHER a working, unit-tested minimal editing improvement on the real-TTY line-mode fallback OR a docs/handoff.md note explaining why piped mode cannot support interactive editing and closing the item as not-applicable.
- The piped/scripted planning contract is unchanged (piped stdin lines are never discarded).
- `cargo test --workspace` passes.

