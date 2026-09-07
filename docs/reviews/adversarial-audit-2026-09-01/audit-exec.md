# Kranz exec/permissions/git surface — adversarial audit

Target: `craigcode/kranz` @ `33732c27` (read-only copy at `.../scratchpad/main-audit`).
Surface: `command_exec.rs`, `permissions.rs`, `git_ops.rs`, `hooks.rs`, `hook_gates.rs`, `hook_status.rs`,
`test_shell.rs`, `test_capability.rs`, `pty_harness.rs`, `runner.rs`, `gate.rs`, `merge.rs`, `merge_gate.rs`,
`workspace_gate.rs`, `contract_gates.rs`, `control.rs`, `config.rs`, `cli/hook_guard.rs`, `cli/exec.rs`
(plus the parts of `orchestrator.rs`, `sandbox.rs`, `pack.rs`, `backend_claude.rs` those call into).

## Summary

The command-grant model itself is sound: contract commands execute from in-memory state folded from the event
log (never re-read from a worker-writable `plan.json`), merge-gate and workspace-contract command strings are
read from the **live base branch** so a mission cannot weaken the gates judging it, grant approvals require an
exact match against a parked request, timeouts group-kill the whole process tree, and the merge-gate env is a
strict allowlist. The weaknesses are all at the *edges* of that model, and they share one shape: **the engine
performs privileged operations inside trees and config files the agent can write.** Three of them are, in my
reading, real containment breaks — engine-side `git` running unhardened in the worker's own worktree
(worker-planted hook/gitlink → host code execution with the engine's full ambient env), `.kranz/config.json`
being read-denied but not write-denied to sandboxed sessions (rewrite the trust root: `claudeBinary`,
`dangerouslyAllowAll`, `sandbox.enforce: off`), and workspace bootstrap commands running with the engine's
complete inherited environment and no sandbox wrap at all. Everything else is second-order gate ownership,
display spoofing, and hardening gaps.

I read the code and traced from input to `exec` for each finding. Nothing was executed; no file was modified.
Where I could not close a loop by reading alone (Claude Code's own handling of a repo-provided
`.claude/settings.json`; the gitlink-rewrite variant in worktree mode) I say so and name the check.

---

## CRITICAL

*(none — the worst findings below are HIGH; nothing reached unauthenticated remote or zero-precondition
host compromise.)*

---

## HIGH

### H1. Engine-side `git` runs unhardened in the tree the worker controls → agent-planted git config/hooks execute with the engine's full environment

**Severity:** HIGH  **Confidence:** CONFIRMED (checkout mode, by code trace); PLAUSIBLE-strong (worktree mode — see preconditions)

**Refs**
- `crates/engine/src/orchestrator.rs:3359` — the worker's session cwd IS the active tree:
  `let session_cwd = self.active_root().to_path_buf();`
- `crates/engine/src/orchestrator.rs:689-701` — `active_root()` / `active_repo()` = mission integration
  worktree (worktree mode) or the primary repo root (checkout mode)
- `crates/engine/src/orchestrator.rs:4451` — `let wt_repo = GitRepo::open(&path)?;` (integration worktree
  handle, **no** `.with_hooks_disabled()`)
- `crates/engine/src/orchestrator.rs:416`, `:550` — `let repo = GitRepo::open(&repo_root)?;` (primary handle,
  unhardened)
- `crates/engine/src/orchestrator.rs:4135-4137` — `self.active_repo().commit_dirty_paths(...)` after every
  feature; `git_ops.rs:584-596` → `dirty_paths()` (`git status`) then `commit_paths_unscanned` (`git add` +
  `git commit`)
- `crates/engine/src/git_ops.rs:199-212` — `with_hooks_disabled` is **opt-in per handle**, "worker-side git
  behavior is deliberately unchanged"
- `crates/engine/src/git_ops.rs:253-275` — the `-c` neutralization set that this path never gets
- `crates/engine/src/sandbox.rs:1160-1176` — `(allow file-write* (subpath "<session_cwd>"))`
- `crates/engine/src/sandbox.rs:793-815` — the only write carve-outs are `events.jsonl`, `events.jsonl.lock`,
  `state.json(.tmp)`, `estimate.json`, `control/`, `runs/*.jsonl`. **`.git` is not among them.**

**Attacker + preconditions.** A worker agent (hostile, prompt-injected via repo content, or simply
misbehaving). Under `workerIsolation: "checkout"` with `sandbox.enforce: fs` — the *hardened* configuration —
the session cwd is the repo root and `<repo>/.git/**` is inside the write allowlist, so the worker writes
`.git/hooks/pre-commit` (or `.git/config` with `core.hooksPath`/`core.fsmonitor`/a filter driver). The very
next engine checkpoint (`commit_dirty_paths`, which runs `git status` then `git add`/`git commit` on the
unhardened handle) executes it. Under the default `workerIsolation: "worktree"`, `<worktree>/.git` is a
gitlink *file* inside the writable root; rewriting it to point at an attacker-built git dir redirects the
engine's subsequent `git status`/`add`/`commit` in that cwd to attacker-controlled `config` and `hooks/`
(this is standard git resolution, but I did not execute it — see "what I'd check").

**Impact.** Arbitrary host command execution **outside** every sandbox, with the engine process's complete
ambient environment (agent API keys, `GH_TOKEN`, cloud credentials, SSH agent socket) — precisely what the
sanitized gate env and the session sandbox exist to withhold. It bypasses the deny list, the grant flow, the
touch set and the human entirely. The same unhardened handle is used by `active_repo().merge_no_ff(...)`
(`orchestrator.rs:4686`), `self.repo.checkout(...)` (`:1369`, `:2628`), `tag` (`:6161`), and
`cli/exec.rs:427-429`'s `push_mission_branch` (→ `pre-push`, and `core.sshCommand`).

**Evidence.** The countermeasure exists and is well built — it is simply not applied here:

```rust
// git_ops.rs:199
pub fn with_hooks_disabled(&self) -> Result<GitRepo> { ... }   // opt-in
// orchestrator.rs:4451  (mission integration worktree = the worker's cwd)
let wt_repo = GitRepo::open(&path)?;                            // unhardened
```

Hardened handles exist at exactly five production sites: `merge.rs:315`, `merge.rs:378`,
`validator_integrity.rs:102`, `validator_snapshot.rs:179`, `orchestrator.rs:3992` and `:4862` (post-run
worktree *inspection*). The author's own comment at `git_ops.rs:189-192` shows the class is understood
("get its payload executed by the detection itself") — the conclusion drawn was that worker-side git is out
of scope, but this is engine-side git in a worker-controlled tree.

**Suggested fix.** Make `with_hooks_disabled()` the default construction for every `GitRepo` the engine
operates with (invert the opt-in), or at minimum apply it to `active_repo()`/`self.repo` and to
`push_mission_branch`. Additionally: deny `<session_cwd>/.git` writes in the sandbox profiles (the
`mission_write_denies` seam already exists and already handles the checkout-mode-ancestor case), and add
`GIT_CONFIG_NOSYSTEM=1` / `GIT_CONFIG_GLOBAL=/dev/null` / `GIT_CONFIG_COUNT` scrubbing to `probe_os`
(`git_ops.rs:1640-1655`) — the idiom is already used in `contract_lint.rs:174-176` and `sandbox.rs:2660`.

**Existing tests.** `git_ops.rs:1805-1990` proves `with_hooks_disabled` neutralizes planted hooks, filter
drivers and `gpg.program`. There is **no** test asserting that the checkpoint-commit path, the integration
worktree handle, or `push_mission_branch` are hooks-disabled — the gap is untested in both directions.

**What I'd check to move worktree mode to CONFIRMED.** Create a mission worktree, replace its `.git` gitlink
file with `gitdir: /tmp/evil` containing `hooks/pre-commit` + `config`, run `git status`/`git commit` with
cwd = worktree, observe the payload.

---

### H2. `.kranz/config.json` is read-denied to sandboxed sessions but not write-denied — a contained session can rewrite the trust root

**Severity:** HIGH  **Confidence:** CONFIRMED

**Refs**
- `crates/engine/src/sandbox.rs:856-897` — `authority_read_deny_paths()` denies reads of
  `<repo>/.kranz/{serve.token, serve.read.token, config.json, domain-terms.local}`
- `crates/engine/src/sandbox.rs:791-815` — `MISSION_METADATA_FILES` / `mission_write_denies()`: the write
  denies cover only mission-dir metadata, `control/` and `runs/*.jsonl`
- `crates/engine/src/sandbox.rs:751-767` — `write_allowlist()` = `session_cwd` (repo root in checkout mode)
  + private scratch + `extraWrite`
- `crates/engine/src/config.rs:6-7`, `:288-296` — layer 3 is `<repo>/.kranz/config.json`, merged last, wins
- `crates/engine/src/types.rs:1545` `claude_binary`, `:1537` `dangerously_allow_all`, `:1559`
  `contract_env_passthrough`, `:1547` `worker_isolation`, per-role `sandbox`
- `crates/engine/src/backend_claude.rs:176-183` — `discover_claude_binary(configured)` pushes the configured
  string as the first candidate and executes it
- `crates/engine/src/permissions.rs:139-147` — `dangerously_allow_all` ⇒ `bypassPermissions`, empty
  allow/deny lists, for **every** role

**Attacker + preconditions.** A worker session under `workerIsolation: "checkout"` with any enforcement tier
(the file sits under the allowed `session_cwd` subpath). It cannot *read* the file — that deny is explicit —
but it can *write* it. `.kranz/config.json` is gitignored by kranz's own materialized rules
(`paths.rs:33-49`), so the write leaves no diff, trips no dirty-tree sweep, and survives the mission.

**Impact.** The next `kranz` invocation in that repo loads the attacker's config: `claudeBinary` names an
arbitrary executable that runs unsandboxed with the operator's full environment; `dangerouslyAllowAll: true`
puts every agent session in `bypassPermissions`; `worker.sandbox.enforce: "off"` disables the containment
that was supposed to be preventing this; `contractEnvPassthrough: ["AWS_SECRET_ACCESS_KEY"]` pipes a named
ambient secret into a contract command. This is a clean escape from the fs tier to the next run.

**Evidence.**
```rust
// sandbox.rs:876 — the file is treated as authority for READS
for name in ["serve.token", "serve.read.token", "config.json", "domain-terms.local"] {
    paths.push(kranz_dir.join(name));
}
// sandbox.rs:793 — but the WRITE-deny set never mentions it
pub(crate) const MISSION_METADATA_FILES: &[&str] =
    &["events.jsonl", "events.jsonl.lock", "state.json", "state.json.tmp", "estimate.json"];
```

**Suggested fix.** Add `<repo>/.kranz` (or at least `config.json`, `serve.token`, `serve.read.token`,
`domain-terms.local`, `.gitignore`) to `mission_write_denies`, with the same raw+canonical expansion the read
denies use. Consider refusing to honor a project-layer `claudeBinary`/`dangerouslyAllowAll` at all (see H3).

**Existing tests.** `sandbox.rs:2829-2900` asserts `.kranz/*` reads are denied; `:1727-1760` asserts
`extraWrite` cannot un-deny the mission metadata floor. No test asserts anything about writes to
`.kranz/config.json`.

---

### H3. Repo-provided `.kranz/config.json` silently supplies `claudeBinary` and `dangerouslyAllowAll` — no banner, no allowlist, no trust prompt

**Severity:** HIGH  **Confidence:** CONFIRMED

**Refs**
- `crates/engine/src/config.rs:288-334` — `load()`/`load_layers()`: project layer merged over global with no
  key restriction; `validate()` (`:377+`) checks model tiers, ranges and backends but never
  `dangerouslyAllowAll` or `claudeBinary`
- `crates/cli/src/commands.rs:41-43` — the danger banner fires **only** for the CLI flag:
  `if cli.dangerously_allow_all { print_danger_banner(); }`
- `crates/cli/src/commands.rs:610-615`, `crates/cli/src/exec.rs:174` — `load_config` ORs the flag onto
  whatever the config already said
- `crates/engine/src/backend_claude.rs:176` — configured binary is probed with `--version` and then used

**Attacker + preconditions.** Anyone who authors a repository the operator clones and runs `kranz` in
(`kranz ready`, `kranz plan`, `kranz exec`). No agent needed. `.kranz/config.json` is only gitignored in
repos where `kranz init` wrote that ignore file — a hostile repo simply commits it.

**Impact.** `claudeBinary: "./tools/claude"` runs an arbitrary repo-supplied binary with the operator's full
environment and no sandbox, on the first command that resolves a backend. `dangerouslyAllowAll: true` puts
every role in `bypassPermissions` with **no banner**, so an operator who never passed the flag has no signal
that the entire permission profile system is off.

**Mitigating context.** `SECURITY.md` says "run untrusted repositories in a separate operating-system or
container account", so hostile-clone is partly a documented operator responsibility. The *silence* is still a
defect: the CLI flag is loud, the identical config key is mute.

**Suggested fix.** Print `print_danger_banner()` whenever the effective config has `dangerouslyAllowAll`,
whatever its source. Restrict `claudeBinary` (and `packDir` absolute paths, and `contractEnvPassthrough`) to
the *global* `~/.kranz/config.json` layer, or require an explicit `--trust-project-config` on first use;
`load_layers` already knows which layer each key came from.

**Existing tests.** `config.rs:728-925` covers layering and defaults; nothing covers a project layer setting
an execution-authority key.

---

### H4. Workspace bootstrap / readiness / data-hook commands run with the engine's complete ambient environment and no sandbox wrap

**Severity:** HIGH  **Confidence:** CONFIRMED

**Refs**
- `crates/engine/src/workspace_gate.rs:196-218` — `run_gate_commands` → `run_shell_command_with_code`
- `crates/engine/src/command_exec.rs:237-253` — `run_shell_command_with_code` → `..._with_timeout_env(..., false)`
- `crates/engine/src/command_exec.rs:271-286` — `clear_env == false` ⇒ **no `env_clear()`**; the child
  inherits the engine's whole environment, `env` is only an overlay
- `crates/engine/src/workspace_provider.rs:603`, `:343-345` — bootstrap/readiness/data hooks; `handle.env`
  carries only `KRANZ_BASE_SHA` (`workspace_provider.rs:1203-1207`)
- No `GateSandbox` parameter exists on this path — contrast `run_shell_command_sandboxed`
  (`command_exec.rs:991`), which every contract/final/merge gate uses
- `crates/engine/src/workspace_contract.rs:33` `.kranz/workspace.json`; read from the base branch at
  `workspace_provider.rs:661-664`

**Attacker + preconditions.** (a) A hostile clone shipping `.kranz/workspace.json` — the gate runs
automatically at run start, before any agent spawns. (b) More interestingly: a worker whose approved touch
set covers `.kranz/**` lands a commit editing `workspace.json`; it cannot affect its own run (the contract is
read from the live base — good), but once that mission merges, **every subsequent mission's** bootstrap runs
the attacker's command.

**Impact.** Unsandboxed host command execution with every ambient credential the engine holds. This is
strictly weaker containment than the contract-command path in the same binary, which clears the env, isolates
`HOME`/`TMPDIR`/`CARGO_HOME`, and wraps in Seatbelt/bwrap/container. The doc justifies the ambient env by
pointing at the contract's declared `secrets` list — but nothing filters to that list: `contract.secrets` is
referenced only by the *remote* provider (`workspace_remote.rs:494`) and by name-shape validation
(`workspace_contract.rs:239`).

**Evidence.**
```rust
// command_exec.rs:277
if clear_env { cmd.env_clear(); }        // false for the workspace gate
```

**Suggested fix.** Build the workspace-gate env the way `agent_env::contract_command_env` does — cleared,
plus exactly the contract's declared `secrets[]` names — and route it through
`run_shell_command_sandboxed` with the mission's resolved `GateSandbox`, as every other engine-run command
already is.

**Existing tests.** `workspace_provider.rs:1290-1320`, `:1576-1601` assert the handle env carries
`KRANZ_BASE_SHA`; `command_exec.rs:1743-1855` assert the *contract* path cannot see ambient secrets. No test
asserts anything about what the workspace gate can see, and no test wraps it.

---

## MEDIUM

### M1. Worker-reported `commandsRun` silently widen the read-only validator's Bash allow-list

**Severity:** MEDIUM  **Confidence:** CONFIRMED

**Refs**
- `crates/engine/src/orchestrator.rs:8280-8303` — `worker_commands_for_milestone()` harvests
  `report.commands_run` from **worker agent reports**
- `crates/engine/src/runner.rs:1296-1303` — merged into `combined_commands`
- `crates/engine/src/runner.rs:1444` — `permissions::for_role(kind, cfg, &combined_commands, grants, &[])`
- `crates/engine/src/permissions.rs:205-212`, `:247-284` — each becomes `Bash(<command>*)` plus one rule per
  `&&`/`||`/`;`/`|` segment, added to the *functional validator's* allow list

**Attacker + preconditions.** A worker agent. Its report is model-authored JSON; `commands_run` is a free
list of strings with no validation, no dedup against the contract, and **no human step** between report and
allow-rule.

**Impact.** The validator is the "read-only" role whose containment rests on `disallowed_tools` +
`validator.tamper` identity assertions plus a *precise* Bash allow set. A worker can inject arbitrary
`Bash(...)` prefix rules into it — e.g. `commandsRun: ["python3 -c"]` or `["bash -c"]` — turning the
next validator session into a general command runner if it is subsequently prompt-injected by the diff it is
reading (which the worker also authored). The design comment at `permissions.rs:257-272` explains at length
why *contract* command patterns were narrowed to avoid exactly this ("arbitrary interpreter use under a
read-only role") — and then the worker-report channel reopens it without the narrowing.

**Suggested fix.** Restrict validator allow-rules to the approved contract commands +
`allowValidatorCommands` (operator config) + human-approved grants. Feed `report.commands_run` to the
validator as *prompt text* ("the worker says it ran these") without granting them, or require a grant for any
worker-reported command not already in the contract.

**Existing tests.** `permissions.rs:598+` covers `dangerously_allow_all` and grants; `runner.rs` tests cover
the scrutiny/functional split. Nothing tests the provenance of `combined_commands`.

---

### M2. Sandboxed sessions and gates keep unrestricted read of the operator's credential files, and the `fs` tier keeps full network egress

**Severity:** MEDIUM  **Confidence:** CONFIRMED

**Refs**
- `crates/engine/src/sandbox.rs:1077` — `(allow file-read*)` (global), then an **enumerated** deny list
- `crates/engine/src/sandbox.rs:856-897` — the deny list: 4 `.kranz` files + `$CARGO_HOME/credentials{,.toml}`
- `crates/engine/src/sandbox.rs:1150-1156` — `fs` and `off` emit `(allow network*)`
- `crates/engine/src/command_exec.rs:1518-1540` — `sanitized_gate_env()` keeps ambient `HOME` for merge gates
  (`command_exec.rs:1489-1496` only redirects `TMPDIR`/`CARGO_HOME`, and `USERPROFILE` on Windows)

**Attacker + preconditions.** Any worker session or any worker-authored build script/test run at a gate,
under the *enforced* `fs` tier.

**Impact.** `~/.ssh/id_*`, `~/.aws/credentials`, `~/.claude/.credentials.json`, `~/.config/gh/hosts.yml`,
`~/.netrc`, `~/.docker/config.json` are all readable, and `fs` permits arbitrary outbound TCP, so they are
also exfiltratable. The deny list's careful inclusion of the serve token and Cargo credentials implies
credential containment is intended; the enumeration stops well short of the credentials an agent host
actually has. For merge gates the ambient `HOME` makes it a one-liner.

**Mitigating context.** Documented posture ("write containment is the fs-tier promise"; `SECURITY.md`'s
least-privilege advice). Reported because the partial denylist reads as a stronger promise than it is.

**Suggested fix.** Extend `authority_read_deny_paths` with the common credential paths above (raw +
canonical), and give merge gates the same isolated `HOME` the contract path already builds.

**Existing tests.** `sandbox.rs:2829-2900` (`.kranz` reads denied), `command_exec.rs:1657-1710`
(merge-gate env excludes server secrets — env only, not filesystem).

---

### M3. Merge-gate suite and pack gates are second-order writable: a landed mission changes what runs for the *next* mission, and pack gates are read unpinned from the working tree

**Severity:** MEDIUM  **Confidence:** CONFIRMED

**Refs**
- `crates/engine/src/merge_gate.rs:21` `.kranz/merge-gates.json`; `crates/engine/src/merge.rs:325`,
  `:339-352` — read from `live_base_sha` (correct for the current merge)
- `crates/engine/src/pack.rs:389-416` `load_for_config` → `repo_root.join(packDir)`; `pack.rs:197-207`
  `load_with_trust` → plain `std::fs::read_to_string` of the **working tree** `pack.toml` (no git ref)
- `crates/engine/src/orchestrator.rs:6504` — `run_shell_command_sandboxed(&gate_root, &decl.command, ...)`
  where `decl` came from that unpinned pack when no standards pin exists
- No governance-path protection exists anywhere: I grepped for a protected/immutable path list covering
  `.kranz/merge-gates.json`, `.kranz/workspace.json`, `packDir`, `.claude/` — there is none. `contract_sweep`
  enumerates *meta* paths (`contract_sweep.rs:137-170`) but that is an exemption list, not a prohibition.

**Attacker + preconditions.** A worker whose approved `touch_set` covers `.kranz/**` (or `**`), plus a human
who merges. The mission's own gates come from the pre-merge base, so it passes; the poisoned gate command
then executes for every later mission. For the pack path in `workerIsolation: "checkout"`, the worker writes
`packDir/pack.toml` in the primary checkout directly and the final gate of the *same* mission executes it.

**Impact.** Persistent gate-command execution (sandbox-wrapped when `enforce != off`, unwrapped otherwise).
Together with H4's workspace contract, this is a general "governance files are ordinary repo files" gap.

**Suggested fix.** Refuse a mission diff that touches `.kranz/merge-gates.json`, `.kranz/workspace.json`, the
configured `packDir`, or `.claude/**` unless the operator explicitly grants it (a `GrantKind` already exists
for touch paths); pin pack loading to the approval base tree even without a standards pin, as
`resolution::approval_pin` already does for the pinned case.

**Existing tests.** `merge_gate.rs:305-360` covers fail-closed parsing; `pack/resolution.rs` covers pinned
drift. Nothing covers "the mission edited the gate config".

---

### M4. Control inbox is an unauthenticated filesystem channel carrying `ApproveGrant` and unrestricted `ConfigChange`

**Severity:** MEDIUM  **Confidence:** CONFIRMED

**Refs**
- `crates/engine/src/control.rs:41-72` (`enqueue`), `:88-109` (`drain`) — the only authorization is the
  ability to create a file in `<mission>/control/`
- `crates/engine/src/types.rs:1777-1799` — `ConfigChange { patch }`, `ApproveGrant { command }`
- `crates/engine/src/orchestrator.rs:2860-2868` → `preview_config_patch` → `config::apply_validated_patch`
  (`config.rs:363-376`) — validates *shape*, never restricts which keys a patch may touch
- `crates/engine/src/sandbox.rs:811`, `:909-918` — `control/` is write-denied and read-denied to sandboxed
  sessions (good), but only when a sandbox is resolved; `sandbox.enforce` defaults to `off`
  (`config.rs:1201-1215` `sandbox_config_defaults_to_off`)

**Attacker + preconditions.** Any process running as the operator with access to the repo — which, at the
default `enforce: off`, includes every worker session.

**Impact.** Self-approval of parked grants and arbitrary mid-mission config patches
(`dangerouslyAllowAll`, `denyPatterns: []`, `contractEnvPassthrough`, sandbox downgrade). The reducer's
`expect_pending_grant` cross-check (`reducer.rs:157-174`) prevents *forging* a grant that was never
requested — good — but a worker can request and then approve.

**Mitigating context.** Under an enforced sandbox this is closed by the `control/` deny, and the design
clearly intends the sandbox to be the boundary. The default posture is what makes it reportable.

**Suggested fix.** Restrict `ConfigChange` patches to a key allowlist that excludes the authority keys
(`dangerouslyAllowAll`, `denyPatterns`, `contractEnvPassthrough`, `claudeBinary`, `*.sandbox.*`,
`workerIsolation`), and consider a per-mission operator secret in control files (the serve-token idiom
already exists).

**Existing tests.** `orchestrator.rs:10096-10123` covers an invalid patch producing an audit receipt;
`reducer.rs` covers the pending-grant cross-check. No test constrains which keys a patch may set.

---

### M5. Agent-authored strings reach the operator's terminal with no control-character or newline sanitization

**Severity:** MEDIUM  **Confidence:** CONFIRMED

**Refs**
- `crates/cli/src/tail.rs:102-142` — `EventRenderer::render` interpolates `goal`, and at `:135-145` the
  `GrantRequested { command }` body, straight into the rendered line
- `crates/cli/src/tail.rs:736` — `eprintln!("{}", renderer.render(event));`
- `crates/engine/src/scrub.rs:513` — `scrub()` redacts secret *patterns*; a repo-wide grep for
  `is_control` / ANSI stripping finds only `review_artifact.rs:41` (a path check) and `output.rs:17-25`
  (kranz's own color constants)

**Attacker + preconditions.** Any agent whose output lands in an event: worker report summaries, grant
request commands, validator findings, gate output tails (`command_exec.rs:1562` `tail_chars` is char-safe but
not control-safe).

**Impact.** The grant prompt is the consent surface. An agent-chosen `command` string containing `\r`, `\n`,
or ANSI/OSC sequences can overwrite or forge what the operator reads before typing `kranz grant approve` —
including painting a benign command over a hostile one. OSC 52 additionally writes the operator's clipboard
in terminals that honor it. The engine-side match on approval is exact, so this is a *deception* attack on
the human, not an allow-set bypass.

**Suggested fix.** One sanitizer at the render boundary: strip/escape C0 and C1 control characters (keep
kranz's own color constants, which are applied after) and collapse newlines in any interpolated
agent-authored string. Apply it in `tail.rs`, `output.rs`, and the report renderers.

**Existing tests.** None found for control characters in rendered events.

---

### M6. `changed_paths` and `remote_has_branch` parse git output in spoofable ways, and the quoting case silently *skips* path-scoped gates

**Severity:** MEDIUM  **Confidence:** CONFIRMED

**Refs**
- `crates/engine/src/git_ops.rs:966-985` — `git diff --name-only` split on `lines()`, **no `-z`**. Git
  C-quotes paths containing control/non-ASCII bytes, so such a path arrives as a literal `"a\nb.rs"`.
  Compare `ls_tree_recursive` (`:1367-1370`), which deliberately fails closed on the quoted form, and
  `dirty_paths` (`:520-583`), which uses `-z` correctly.
- Consumers: `git_ops.rs:987-993` `dashboard_touched` (`starts_with("apps/dashboard/")`);
  `merge.rs:337-338` → `MergeSuiteGate` `whenPaths` filtering; `merge.rs:396` →
  `standards_enforcement::applicable_rules`
- `crates/engine/src/git_ops.rs:1516-1518` — `remote_has_branch` uses `line.contains(&needle)`, a substring
  match against `refs/heads/<branch>`
- `crates/engine/src/git_ops.rs:1412-1415` — `is_untracked` fails *open* on a quoted path
- `crates/engine/src/git_ops.rs:1431-1445` — `list_worktrees` parses `--porcelain` without `-z`; the parsed
  paths are fed back into `git worktree remove --force` (`merge.rs:626-645`)

**Attacker + preconditions.** A worker that commits a file whose name contains a newline or a non-ASCII byte.

**Impact.** A conditional merge gate (`whenPaths`) or a path-scoped enforced Flight Rule can be made not to
apply to the very change that touched its path — a quiet gate bypass, not a crash. `remote_has_branch`
returning true for `refs/heads/foobar` when asked about `foo` is decided by anyone who can create a remote
branch.

**Suggested fix.** `-z` everywhere git output is parsed (`changed_paths`, `is_untracked`, `list_worktrees`,
plus `cli/ready.rs:805`, `:865`), or fail closed on a leading `"` as `ls_tree_recursive` does. Make
`remote_has_branch` compare the ref field for equality.

**Existing tests.** `dirty_paths` has NUL-delimited coverage; the C-quoting case is untested for
`changed_paths`.

---

## LOW

### L1. Gate and contract commands can leave orphaned background descendants on the success path
**Confidence:** CONFIRMED. `crates/engine/src/command_exec.rs:1115-1129` puts the child in its own process
group and only the **timeout** branch (`:1195-1211`) sends `kill(-pid, SIGKILL)`. On normal exit,
`kill_on_drop` reaps the leader only; a gate command that does `nohup payload &` survives the "passing" gate
indefinitely. `run_with_timeout` (`:156-182`) does not group-kill at all. Fix: group-kill on every exit path,
not just timeout.

### L2. Git argument-injection surface: no ref-name validation, inconsistent `-`-guards
**Confidence:** CONFIRMED. Guarded functions use an ad-hoc `starts_with('-')` check (e.g.
`git_ops.rs:383-390`); `create_branch` (`:428-436`), `checkout` (`:438-441`), `tag` (`:1087-1090`),
`apply_patch` (`:878-882`), the `path` slot of `add_worktree*`/`remove_worktree` (`:1108`, `:1136`, `:1157`,
`:1183`) and the `remote` slot of `push_mission_branch` (`:1560-1590`, branch is validated hard, remote not
at all) have neither a guard nor a `--`. `commits_between`/`diff_stat`/`diff_full` (`:763`, `:837`, `:843`)
build `{from}..{to}` from unguarded input. Not currently reachable from agent input — mission ids are UUIDs
(`orchestrator.rs:473`), feature ids are engine-assigned (`reducer.rs:59-66`), branch names are
prefix-constructed, and worktree paths are sanitized (`orchestrator.rs:8081-8094`) — so this is defense in
depth, not a live path. `MissionPaths::is_safe_id` (`paths.rs:69`) permits a leading `-`, whitespace and `.`.

### L3. The hook-gate spec file lives in session-writable scratch, so a session can neuter its own PreToolUse guard
**Confidence:** CONFIRMED. `hook_gates.rs:166-176` puts `spec.json` under
`backend_claude::scratch_home_root(session_id)` = `/tmp/kranz-worker-home-<id>`
(`backend_claude.rs:297-299`), which every sandbox tier keeps writable by design. Rewriting `touchSet` to
`["**"]` makes `evaluate()` allow everything. The module doc states the *record* file is session-writable
and untrusted (`hook_gates.rs:90-93`) but does not say the same of the spec. Impact is bounded: the guard is
explicitly defense-in-depth, fails open on error (`cli/hook_guard.rs:40-51`), and `contract_sweep` remains
authoritative. Fix: put the spec outside the session-writable set, or HMAC it with a per-session key passed
on the hook command line.

### L4. Mission worktrees live at predictable paths in the shared system temp dir
**Confidence:** CONFIRMED (code), impact PLAUSIBLE. `orchestrator.rs:8092-8095`,
`merge.rs:365-367` (`kranz-merge-<uuid>`, unpredictable) vs
`kranz-wt-<repo-namespace>-<mission-id>-_integration` (namespace derived from the repo path, mission id a
6-hex UUID prefix). On a shared host, another local user can pre-create or symlink these. No `O_NOFOLLOW`
discipline on the worktree root comparable to `paths::open_mission_dir_nofollow`.

---

## INFO

### I1. Repo-provided `.claude/settings.json` / `.mcp.json` are not neutralized for the agent CLI
**Confidence:** PLAUSIBLE. `backend_claude.rs:355-433` builds the argv: `--settings <json>` carries kranz's
hook projection, but there is no `--strict-mcp-config` and no settings-source restriction, and the session
cwd is the repo worktree. If the Claude Code CLI honors a project-level `.claude/settings.json` in a
non-interactive `-p` session with a freshly seeded scratch `HOME` (no trust record), a repo-supplied
`PreToolUse` hook would execute outside the model's permission system entirely. **What to check:** run
`claude -p` with `HOME` pointed at an empty dir, in a directory containing `.claude/settings.json` with a
`PreToolUse` hook, and see whether the hook fires or a trust prompt blocks it. If it fires, this is HIGH and
the fix is to pass `--strict-mcp-config` and pin settings sources.

### I2. `kranz exec` executes model-authored shell with no human in the loop
**Confidence:** CONFIRMED, documented. `cli/exec.rs:1-21`, `:267-270` (`// Auto-approve`),
`cli.rs:338`. The orchestrator's plan — including every `Assertion.command` and `PtyScript.command` — is
approved automatically and then executed at approval lint, preflight, each validation round and the final
gate. Contained (cleared env, gate sandbox, disposable base worktrees at approval) but it is the mode in
which the consent surface does not exist. Worth stating plainly in `SECURITY.md` alongside the sandbox advice.

### I3. Plan trust boundary holds
**Confidence:** CONFIRMED. Contract commands execute from `self.state.mission.validation_contract`
(`orchestrator.rs:6304`, `:6344`), folded from `events.jsonl`; `plan.json` on disk is read only for report
rendering and reseed prompts (`orchestrator.rs:7802`). A worker editing a committed `plan.json` changes what
a *reseeded orchestrator reads as context* (prompt-injection surface) but never what the engine executes.
`PtyScript` is deliberately inlined in the contract rather than referenced by path (`types.rs:556-562`).
`reducer.rs:877-881` prevents a revision from removing assertions, though it does permit adding new command
assertions — those go through the human revision-approval path (`orchestrator.rs:1704-1740`), except under
`kranz exec`.

---

## Areas checked with no finding

- **`hooks.rs` (GitHub webhook triggers).** HMAC-SHA256 implemented over `sha2` and compared with
  `subtle::ConstantTimeEq` on the hex bytes (`hooks.rs:118-165`); missing/malformed headers never verify;
  the action set is a two-element const with a regression test (`TRIGGER_ACTIONS`), payload text is bounded
  before scrubbing and again after (`:250-268`), and the module performs no git operations. No finding.
- **Grant approve/deny replay.** `reducer.rs:157-180` cross-checks every `GrantApproved`/`GrantDenied`
  against the parked request's kind *and* target, and the CLI/REST surfaces re-check before enqueueing
  (`commands.rs:1417-1443`, `:1555-1570`; `rest.rs:1157-1179`). Grants are extend-only and deduped. No finding.
- **Merge flow structure.** The `--no-ff` merge happens in a throwaway detached worktree at `live_base_sha`
  and the primary tree is only fast-forwarded to the exact tested commit (`merge.rs:365-386`, `:584-586`);
  both handles are hooks-disabled (`:315`, `:378`); dirty trees refuse; secret scanning runs against the
  base-owned allowlist; there is no force-push and the only push in the tree
  (`git_ops.rs:1560-1590`) hard-requires a `kranz/` prefix and rejects `:`/leading-`-`/whitespace. The one
  residual I'd note rather than file: the filter-driver neutralization set is frozen when the handle is
  built (`merge.rs:315`), so a *new* driver name written to `.git/config` by a gate command mid-run is not in
  the `-c` set — the post-gate integrity check (`merge.rs:544-582`) verifies HEAD/branch/tree cleanliness but
  not `.git/config`, which `validator_integrity.rs:70-73` does fingerprint on its own path.
- **Timeout / kill discipline.** `command_exec.rs:1149-1225`: concurrent pipe drain plus wait (no deadlock on
  a full pipe), unix process-group SIGKILL, Windows Job Object with `KILL_ON_JOB_CLOSE`, bounded tails in
  `read_stream_tail` (`:1233-1258`). Tested at `command_exec.rs:1592-1656`, `:1892-1988`, `:2978-3058`.
  Only L1 above is outstanding.
- **`pty_harness.rs`.** Transcripts bounded at 256 KiB with a `truncated` flag, session and per-step
  timeouts, failure tail scrubbed and capped at 2 KiB, the wrapped child leads a new session so the
  end-of-script SIGKILL reaches the group, and the script's command runs through the same
  `prepare_gate_command` wrap as its sibling contract commands (`pty_harness.rs:26-40`, `:80-90`). Raw
  transcripts go to gitignored `runs/`, not the terminal. No finding beyond M5's general rendering issue.
- **`hook_status.rs` / `cli/hook_guard.rs`.** Non-authoritative by construction (no `EventKind` reference at
  all), capability token compared as a constant-time hash, bounded stdin reads with an explicit test
  (`hook_guard.rs:232-250`), fail-open documented and deliberate, hook config written only into the
  session-private scratch `HOME` and never the tracked `.cursor/hooks.json`. The token living in a
  session-readable spec is documented and accepted. No finding beyond L3.
- **`gate.rs` / `contract_gates.rs`.** `gate.rs` is a typed pipeline with no execution and no way to derive a
  verdict from a score; `contract_gates.rs` is static analysis over command *text* (vacuous-filter,
  wrong-polarity, passes-on-base, env-sensitive) and executes nothing. Gate verdicts come from process exit
  codes, never from parsing "PASS" out of output — an agent printing `PASS` cannot green a gate. No finding.
- **`merge_gate.rs` parsing.** `deny_unknown_fields`, empty suites rejected, at least one unconditional gate
  required, `.`-only `whenPaths` rejected, absolute/parent-component `cwd` and prefixes rejected, commands
  must be a single non-NUL line — every weakening shape fails closed, with a named composition-audit test
  (`merge_gate.rs:305-360`). No finding.
- **`test_shell.rs` / `test_capability.rs`.** Test-fixture portability helpers and a skip ledger that panics
  when a declared-required capability is missing. No production surface, no security relevance.
- **Contract-command environment.** `agent_env::contract_command_env` (`:580-626`) clears the env, builds an
  isolated `HOME`/`TMPDIR`/`CARGO_HOME`, and refuses `contractEnvPassthrough` names that collide with its own
  managed keys **case-insensitively** (the Windows duplicate-case trap is handled). Merge-gate env is a
  13-entry allowlist with `CARGO_HOME` deliberately excluded and replaced by a cache-only root that fails
  closed. Tested at `command_exec.rs:1657-1855`. Sound; the only gap is M2's filesystem side.
- **`sh -c` interpolation.** The only shell in the engine is `shell_argv` (`command_exec.rs:291-313`), and it
  passes the command as a single argv element — there is no string concatenation of agent data into a shell
  line anywhere. `hook_gates::shell_quote` (`:284-286`) correctly closes-escapes-reopens single quotes for
  the one command line it builds. `git_ops.rs` is argv-only throughout. No shell-injection finding.
