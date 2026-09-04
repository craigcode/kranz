# Adversarial audit — kranz sandbox / egress / containment surface

Target: `craigcode/kranz` @ `33732c27` (read-only copy at
`<audit worktree>`). Surface: `sandbox.rs`,
`sandbox_container.rs`, `sandbox_windows.rs`, `appcontainer_windows.rs`,
`container_egress.rs`, `egress_proxy.rs`, `workspace_container.rs`,
`workspace_provider.rs`, `workspace_remote.rs`, `disk_preflight.rs`, every
`unsafe` block in `crates/`, plus `docs/reviews/m7-*.md` and
`docs/knowledge/validation/gates.md`.

## Summary (5 lines)

The fail-closed discipline is genuinely good: every resolution path
(`resolve_for_session_target`, `resolve_gate_sandbox_target`,
`resolve_validator_containment_target`, `maybe_start_for_session`,
`ContainerEgressBoundary::start`) refuses rather than degrades, and I found no
place where the harness reports "sandboxed" while running unsandboxed.
The bugs are in what the profiles *allow*, not in whether they are applied.
Three stand out: the macOS **gate** profile grants read/write/`file-ioctl` on
every `/dev/tty[p-t]*` node — which on macOS includes the operator's own login
terminal — to repo-authored gate commands; the Linux bubblewrap argv unshares
**only** the network namespace and only under `fs+net`, leaving the host PID/IPC
namespace, `/proc` of the engine, and the controlling terminal reachable; and
`<repo>/.kranz/config.json` (the file that decides `enforce`, `claudeBinary`,
`dangerouslyAllowAll`) is read-denied but **write-allowed** in checkout mode.
Two more: the tier-3 container mounts the operator's whole `$CARGO_HOME`
(credentials included) into agent sessions, and the Seatbelt egress proxy is
unauthenticated on loopback while an identical authenticated constructor exists
and is used only for the container path.

---

## CRITICAL

*(none — see HIGH-1, which I would call CRITICAL if I had executed the TIOCSTI
step rather than only reading the profile that grants it.)*

---

## HIGH

### HIGH-1 — macOS gate profile grants write + `file-ioctl` on the operator's terminal device

- **Severity:** HIGH
- **Confidence:** CONFIRMED that the grant is emitted into every wrapped gate
  profile. PLAUSIBLE (not executed) that `TIOCSTI` on that fd yields host
  command execution; see "what I'd check".
- **Refs:**
  - `crates/engine/src/command_exec.rs:604` `gate_profile_extras()`
  - `crates/engine/src/command_exec.rs:635-636` the two offending lines
  - `crates/engine/src/command_exec.rs:899-905` where the extras are appended
    to `sandbox::generate_profile()` for every Seatbelt gate
- **Attacker + preconditions:** a repository whose `.kranz/merge-gates.json`
  or mission `validation_contract` supplies a gate command (SECURITY.md's
  "repository-provided commands"), or a worker-authored contract command that
  the final gate runs. macOS host, `sandbox.provider = process`,
  `enforce = fs` or `fs+net`, and the operator ran `kranz` from an interactive
  terminal (the normal case).
- **Impact:** the gate command can `open("/dev/ttys000", O_RDWR)` — the
  operator's own terminal, same uid, mode `0620` — and then either write raw
  escape sequences to it or issue `ioctl(fd, TIOCSTI, &c)` to push characters
  into the terminal's input queue. Those characters are consumed by the
  operator's shell once `kranz` returns: arbitrary command execution as the
  operator, entirely outside the sandbox. This defeats the whole point of the
  gate wrap.
- **Evidence:**

  ```rust
  // command_exec.rs:635
  (allow file-read* file-write* (regex #"^/dev/tty[p-t][0-9a-f]+$"))
  (allow file-ioctl (literal "/dev/ptmx") (regex #"^/dev/tty[p-t][0-9a-f]+$"))
  ```

  The comment three lines above (`command_exec.rs:620-624`) states the scoping
  exists precisely to prevent "terminal injection into the operator's tty,
  TIOCSTI-class surfaces". On macOS the pty slave pool *is* the terminal pool:
  a Terminal.app/iTerm session is `/dev/ttys000`, `/dev/ttys003`, …, matched by
  `^/dev/tty[p-t][0-9a-f]+$` (`s` ∈ `[p-t]`). The narrowing therefore did not
  exclude the thing it names.
- **Suggested fix:** do not grant a device-class regex. Allocate the pty in the
  trusted parent (the harness already has `pty_harness.rs` doing `openpty`),
  pass the slave fd to the child, and grant the profile only `/dev/ptmx` plus
  the *specific* slave path allocated for that run (a `literal`, generated per
  wrap). Failing that, at minimum stat the process's own controlling terminal
  (`ttyname(0)` on the parent) and emit an explicit
  `(deny file-read* file-write* file-ioctl (literal "<operator tty>"))` — SBPL
  denies beat allows regardless of order, which this file already relies on.
- **Existing test coverage:** `command_exec.rs:2298`
  `gate_profile_extras_scopes_file_ioctl_to_pty_devices` and `:2180` assert the
  *presence* of the scoped grant and the absence of a bare
  `(allow file-ioctl)`. No test asserts that the operator's tty is outside the
  matched set — the tests lock in the bug.
- **What I'd check to promote to CONFIRMED:** run a gate command under the
  generated profile on macOS that opens `/dev/ttys00N` for the parent shell and
  issues `TIOCSTI`. XNU's `ttioctl` permits `TIOCSTI` when the fd is open for
  read (or the caller is root); the profile supplies the open.

### HIGH-2 — bubblewrap argv unshares only the network namespace, and only under `fs+net`

- **Severity:** HIGH
- **Confidence:** CONFIRMED for the missing flags (the argv is fully
  enumerated in one function). PLAUSIBLE for the `/proc/<pid>/environ` read and
  the TIOCSTI variant, both of which are kernel/`yama` dependent.
- **Refs:** `crates/engine/src/sandbox.rs:1262-1288` (`bubblewrap_args`, the
  complete flag set), `crates/engine/src/sandbox.rs:1278` (`--unshare-net`
  only under `FsNet`)
- **Attacker + preconditions:** a prompt-injected or hostile agent session on
  Linux with `sandbox.provider = process`, `enforce = fs` (the tier the docs
  call "write containment with the agent's API egress intact"). `bwrap` present.
- **Impact:** the argv is exactly

  ```
  --die-with-parent --ro-bind / / --dev /dev --proc /proc
  [--unshare-net]  <binds…> <masks…> --chdir <cwd> -- <binary> <args>
  ```

  There is no `--unshare-pid`, `--unshare-ipc`, `--unshare-uts`,
  `--unshare-user`, `--unshare-cgroup`, `--new-session`, and no seccomp filter.
  Consequences, in order of severity:

  1. `--proc /proc` without a PID namespace mounts a procfs that shows **host**
     processes. The contained agent can read `/proc/<kranz-pid>/environ`,
     `/proc/<kranz-pid>/cwd`, `/proc/<kranz-pid>/cmdline`. `environ` is the
     exact set that `backend_claude.rs`' `.env_clear()` and the `agent_env`
     module exist to keep away from a prompt-injectable child
     (`GH_TOKEN`, `SLACK_*`, `AWS_*`). Under `kernel.yama.ptrace_scope = 0`
     (Debian/RHEL/Arch default; Ubuntu ships 1) that read succeeds for a
     same-uid process; under scope 1 it does not (a child cannot ptrace its
     ancestor). The `serve.token` `/dev/null` mask is likewise moot if
     `/proc/<pid>/mem` is reachable.
  2. No `--new-session`: bubblewrap's own documentation calls this out — the
     contained process keeps the caller's controlling terminal and can
     `TIOCSTI` into it. Same escape as HIGH-1, on Linux, kernel-version
     dependent (`CONFIG_LEGACY_TIOCSTI`, off by default since 6.2 / Ubuntu
     24.04, still on in older LTS and many distros).
  3. Shared PID namespace also means the agent can `kill -9` the kranz engine
     and any same-uid host process.
- **Evidence:** `sandbox.rs:1265-1279`

  ```rust
  let mut out = vec![
      "--die-with-parent".to_string(),
      "--ro-bind".to_string(), "/".to_string(), "/".to_string(),
      "--dev".to_string(), "/dev".to_string(),
      "--proc".to_string(), "/proc".to_string(),
  ];
  if inputs.enforce == crate::types::SandboxEnforce::FsNet {
      out.push("--unshare-net".to_string());
  }
  ```

  `docs/reviews/m7-linux-bubblewrap-live-proof.md` proves only two things — a
  sibling-tempdir write denial and a loopback connect denial under `fs+net`.
  Neither exercises the PID/IPC/session surface, so the receipt does not
  contradict this.
- **Suggested fix:** add `--unshare-pid --unshare-ipc --unshare-uts
  --unshare-cgroup --new-session` unconditionally (a PID namespace also makes
  `--proc /proc` mean what the comment assumes), and consider
  `--unshare-user` plus a seccomp filter for the `fs` tier. `--new-session`
  will require the caller to stop relying on an inherited ctty — the claude
  backend already uses piped stdio, so this should be free there; check the
  `pty_harness` path separately.
- **Existing test coverage:** `sandbox.rs:2173` asserts `--unshare-net` is
  present under `fs+net`. Nothing asserts any other namespace flag; there is no
  negative test for host-`/proc` visibility.

### HIGH-3 — `<repo>/.kranz/config.json` is read-denied but write-allowed (checkout isolation)

- **Severity:** HIGH
- **Confidence:** CONFIRMED for the write grant. CONFIRMED that the file is the
  source of `enforce`/`claudeBinary`/`dangerouslyAllowAll`. CONFIRMED that a
  *running* mission is immune (config is pinned into `MissionCreated`);
  the escalation lands on the **next** mission.
- **Refs:**
  - `crates/engine/src/sandbox.rs:760-767` `write_allowlist` — the writable set
    is `{session_cwd, tmpdir, extra_write…}` and nothing else is subtracted
  - `crates/engine/src/sandbox.rs:793-800` `MISSION_METADATA_FILES` — the only
    write denies are `events.jsonl`, `events.jsonl.lock`, `state.json`,
    `state.json.tmp`, `estimate.json`, plus `control/` and `runs/*.jsonl`
  - `crates/engine/src/sandbox.rs:874-890` `authority_read_deny_paths` — the
    same file is explicitly named as authority material *for reads*
  - `crates/engine/src/appcontainer_windows.rs:1991-1998` — Windows grants the
    identical inheritable `rwx` on every write root, with the same deny set
  - `crates/engine/src/orchestrator.rs:492` — config is folded into
    `MissionCreated`; `crates/server/src/host.rs:348,1035` and
    `crates/cli/src/commands.rs:611` re-`config::load` per new mission
- **Attacker + preconditions:** a prompt-injected worker in a mission running
  with `workerIsolation: "checkout"` (non-default but supported — the code
  documents it as the mode "for backends that cannot write into temp-dir
  worktrees"). In checkout mode `session_cwd` **is** the repo root, so
  `<repo>/.kranz/` sits inside an inheritable write allow. A second mission
  must then be created on the same repo (routine under `autoWork` / the queue
  drain).
- **Impact:** the worker overwrites `.kranz/config.json`. It cannot read the
  file first (read is denied), but it does not need to — it can write a whole
  new one. Useful payloads:
  `{"worker":{"sandbox":{"enforce":"off"}}}` (next mission's worker is
  unsandboxed), `"claudeBinary": "<path the worker just wrote>"` (the engine
  spawns that binary directly — `server/src/host.rs:362` and `cli/src/commands.rs:622` read `cfg.claude_binary` from the freshly loaded config on every backend construction),
  `"dangerouslyAllowAll": true`, `"validatorAllowUncontainedDegrade": true`
  (turns HIGH-severity fail-closed validator containment back into the
  documented-as-dangerous degrade), and `"hooks": {"secret": "known"}` (lets
  the attacker forge authenticated GitHub webhooks into `kranz serve`,
  `hooks.rs:79`). The chain ends in host code execution as the operator.
  Note the asymmetry is the bug: the code goes to real trouble to deny *reads*
  of exactly this file and never denies *writes* to it.
- **Evidence:** `sandbox.rs:874` — "Authority files a sandboxed session must
  never read … `config.json` carries Slack tokens and remote-workspace
  credentials". `mission_write_denies` (`sandbox.rs:801`) covers only the
  mission dir; `<repo>/.kranz/config.json` is one level above it and appears in
  no deny list on any of the three tiers.
- **Suggested fix:** add the `.kranz` authority set to the **write** deny path
  on all three tiers — reuse `authority_read_deny_paths` /
  `authority_read_deny_dirs` for `(deny file-write* …)` in `generate_profile`,
  for the `/dev/null` mask set in `bubblewrap_args` (it already masks these
  files, but only against reads — the mask happens to close writes too on
  bwrap, so the real gap there is any authority file absent at spawn), and for
  the `AclMode::Deny` list in `acl_changes` (which already denies `deny_all` on
  `authority_read_deny_paths` — Windows is in fact the one tier that is
  correct here). Additionally consider denying writes to all of
  `<repo>/.kranz/` except the mission's own worktree subtree.
- **Existing test coverage:** none. `sandbox_profile_denies_authority_material_reads`
  (`sandbox.rs:1656`) covers only the read side. No test asserts that a
  checkout-mode profile refuses a write to `.kranz/config.json`.

### HIGH-4 — tier-3 container sessions mount the operator's whole `$CARGO_HOME`, credentials included

- **Severity:** HIGH
- **Confidence:** CONFIRMED (traced `container_run_args` → `push_toolchain_caches`)
- **Refs:**
  - `crates/engine/src/sandbox_container.rs:680`
    `push_toolchain_caches(&mut out, ToolchainMount::Session)`
  - `crates/engine/src/sandbox_container.rs:598-628` the mount loop
  - `crates/engine/src/sandbox_container.rs:576-580` the `ToolchainMount::Gate`
    doc, which names this exact exposure as the reason gates must not do it
  - `crates/engine/src/sandbox.rs:884-889` — the process tier read-denies
    `$CARGO_HOME/credentials.toml` and `$CARGO_HOME/credentials`
- **Attacker + preconditions:** any agent session with
  `sandbox.provider = "container"` on a supported host. No worker cleverness
  needed beyond reading a file.
- **Impact:** `-v $CARGO_HOME:$CARGO_HOME:ro` plus `-e CARGO_HOME=…` puts
  `credentials.toml` (crates.io registry auth tokens) and any
  credential-provider config inside the container, readable by the agent.
  The tier-2 process sandbox denies exactly these two filenames as "the same
  credential class as the serve token". So tier 3 — the tier
  `resolve_validator_containment_target` describes as "already the stronger
  containment" (`sandbox.rs:476-478`) — is strictly weaker than tier 2 for
  registry credentials.
- **Evidence:** `sandbox_container.rs:576-580`

  > "the real Cargo root NEVER crosses — the gate's `CARGO_HOME` is a seeded
  > cache-only home precisely because the real root carries registry
  > credentials … and ro-mounting it would reopen the exact read exposure that
  > home exists to close"

  and `sandbox_container.rs:565-567` for the session arm:

  > "Agent sessions: the whole Cargo home crosses read-only (registry + shims
  > + credentials alike — the session posture landed with tier 3)."

  That is a statement of fact, not a justification; the Gate arm is the fixed
  version of the same code path.
- **Suggested fix:** give `ToolchainMount::Session` the same treatment as
  `Gate` — mount `<cargo>/registry`, `<cargo>/git`, and `<cargo>/bin` rather
  than the root — or stack `-v /dev/null:<cargo>/credentials.toml:ro` and
  `-v /dev/null:<cargo>/credentials:ro` masks the way
  `push_authority_masks` already does for `serve.token`.
- **Existing test coverage:** `container_run_args_mask_authority_material_under_session_root`
  (`sandbox_container.rs:1028`) covers the `.kranz` masks only. No test asserts
  cargo credentials are unreachable in session mode.

---

## MEDIUM

### MED-1 — the Seatbelt egress proxy is unauthenticated on loopback, while every `fs+net` sandbox is allowed to reach all of loopback

- **Severity:** MEDIUM
- **Confidence:** CONFIRMED for the mechanism (unauthenticated bind +
  `localhost:*` allow). CONFIRMED that concurrent missions across repos are
  supported. PLAUSIBLE that two such missions have *different* allowlists in a
  given deployment.
- **Refs:**
  - `crates/engine/src/egress_proxy.rs:265-272` `start` binds
    `127.0.0.1:0` with **no** `relay_authority`
  - `crates/engine/src/egress_proxy.rs:558-562` — the Seatbelt path calls
    `EgressProxy::start`, not `start_authenticated_bound`
  - `crates/engine/src/egress_proxy.rs:610-615` — `route_host = "127.0.0.1"`
  - `crates/engine/src/sandbox.rs:1148`
    `(allow network-outbound (remote tcp "localhost:*"))`
  - `crates/server/src/host.rs:102-105`, `:1045` — `host.maxConcurrentRepos`
    lets several repositories drain concurrently in one process
- **Attacker + preconditions:** a prompt-injected `fs+net` Seatbelt session in
  repo A, running while a mission in repo B is live on the same host (or any
  other local process/user on a shared box). The agent scans loopback ephemeral
  ports for a proxy that answers `CONNECT`.
- **Impact:** repo B's proxy is an open forwarder to *B's* allowlist. If B has
  an operator-granted egress destination A does not
  (`apply_egress_grants`, `runner.rs:1508`), A reaches it — the per-mission
  egress allowlist is bypassed. Secondary: the denial record A provokes is
  written to **B's** `runs/egress-denials.jsonl` and returned in B's
  `RunOutcome.denied_egress`, forging a grant-request signal against a mission
  that never made it. The module doc (`egress_proxy.rs:49-50`) says the
  container relay's authentication exists precisely so "another container
  [cannot] forge requests into this run's proxy"; the same reasoning was not
  applied to the loopback path, where the threat is identical.
- **Evidence:** `EgressProxy::start_authenticated_bound` already exists and is
  used exclusively by `container_egress.rs:184`. The Seatbelt path passes
  `None` for `relay_authority` (`egress_proxy.rs:281`).
- **Suggested fix:** always mint a per-run authority and pass it via
  `start_authenticated_bound`; wire it into the session env as
  `HTTPS_PROXY` carrying the token as URL userinfo on loopback — but note that puts the
  token in a worker-readable env var, which only closes the *cross-mission*
  case, not a within-mission one. A stronger fix is to bind the proxy to a
  unix domain socket under the session-private scratch (Seatbelt can allow one
  `network-outbound (literal "<path>")`), which is unforgeable from another
  mission because the path is not in its writable/readable set.
- **Existing test coverage:**
  `egress_proxy_authenticated_listener_rejects_forgery_without_denial_record`
  (`egress_proxy.rs:897`) proves the container path is authenticated.
  `egress_proxy_maybe_start_wires_env_for_seatbelt_fs_net` (`:1038`) locks in
  the unauthenticated Seatbelt shape.

### MED-2 — worker container gets none of the hardening flags the relay gets

- **Severity:** MEDIUM
- **Confidence:** CONFIRMED (both argv builders are in one file)
- **Refs:**
  - `crates/engine/src/sandbox_container.rs:497-504` `run_prologue()` —
    `run --rm -i --read-only` and nothing else
  - `crates/engine/src/sandbox_container.rs:665-700` `container_run_args`
  - contrast `crates/engine/src/container_egress.rs:246-271` (loader) and
    `:294-322` (relay): `--user`, `--cap-drop ALL`,
    `--security-opt no-new-privileges`, `--pids-limit 64`, `--read-only`,
    a size-capped tmpfs, a digest-pinned image
- **Attacker + preconditions:** any container-provider agent session.
- **Impact:** the worker runs as uid 0 inside the container with Docker's
  default capability set (`CAP_DAC_OVERRIDE`, `CAP_CHOWN`, `CAP_FOWNER`,
  `CAP_SETUID`, `CAP_MKNOD`, `CAP_NET_RAW`), `no-new-privileges` off, no pids
  or memory limit. Because `session_cwd` and each `extra_write` root are bound
  **rw at the identical host path**, container-root writes land in the
  operator's tree as uid 0 (breaking the tree and, on a rootful daemon,
  requiring `sudo` to clean up), and the agent can plant a setuid-root binary
  in a host-visible directory. No pids limit means a fork bomb in the worker
  takes the host down. `--read-only` root and the absence of `CAP_SYS_ADMIN`
  do block the classic mount-based escapes, so I am *not* claiming a container
  breakout.
- **Suggested fix:** apply the same four flags the relay already gets, plus
  `--user <uid>:<gid>` matching the operator (the code already has
  `credential_owner()` in `container_egress.rs:574` doing exactly this
  derivation) and a `--pids-limit`/`--memory` bound.
- **Existing test coverage:** `container_run_args_mounts_policy_and_runs_image`
  (`sandbox_container.rs:985`) asserts the mount set; no test asserts a
  capability or user posture for the worker.

### MED-3 — container tier is missing three of the process tier's authority denies

- **Severity:** MEDIUM
- **Confidence:** CONFIRMED
- **Refs:** `crates/engine/src/sandbox_container.rs:545-556`
  (`push_authority_masks`) vs `crates/engine/src/sandbox.rs:874-905`
  (`authority_read_deny_paths` / `authority_read_deny_dirs`)
- **Attacker + preconditions:** a container-provider session in checkout mode
  (`session_cwd` = repo root, so `<repo>/.kranz` is inside the rw mount).
- **Impact:** `push_authority_masks` masks exactly three names:

  ```rust
  for name in ["serve.token", "serve.read.token", "config.json"] {
  ```

  The process tier additionally denies `domain-terms.local` (added in the
  14th-pass review as "the plaintext clean-room lint vocabulary that must never
  be readable outside the engine-side lint"), the `<repo>/.kranz/hook-status/`
  projection directory, and the `<mission>/control/` inbox. The mission dir is
  mounted `:ro` so `control/` is *readable* inside the container — the exact
  posture `authority_read_deny_dirs` was written to close. `.kranz/config.json`
  is masked for reads but, as in HIGH-3, the rw `session_cwd` mount leaves it
  writable (a `/dev/null:…:ro` mask does close writes, so this one filename is
  actually safe on the container tier — but only that one).
- **Suggested fix:** drive `push_authority_masks` off
  `sandbox::authority_read_deny_paths(inputs)` and
  `sandbox::authority_read_deny_dirs(inputs)` rather than a hand-copied
  three-name list, so the two tiers cannot drift again.
- **Existing test coverage:** `container_run_args_mask_authority_material_under_session_root`
  (`sandbox_container.rs:1028`) asserts the three-name list — it locks in the
  drift.

### MED-4 — `workspace.json` `mounts[]` is an unbounded rw host-path grant

- **Severity:** MEDIUM
- **Confidence:** CONFIRMED for the mount construction and validation.
  CONFIRMED that the *provisioning* read is base-branch-pinned (so a mission
  cannot self-escalate mid-run); the exposure is the merge path.
- **Refs:**
  - `crates/engine/src/workspace_container.rs:368-372` —
    `for mount in &contract.mounts { volumes.push(bind_mount(mount, mount)); }`
    (no `:ro`, no allowlist)
  - `crates/engine/src/workspace_contract.rs:248-259` — the only validation is
    "absolute path without `..` components"
  - `crates/engine/src/workspace_provider.rs:661-663` and
    `crates/engine/src/orchestrator.rs:1225` — the contract is loaded
    `_at_ref(base_sha / base_branch)`, explicitly "so a mission cannot weaken
    the contract that judges it"
- **Attacker + preconditions:** `workspace.provider = "container"`. A mission
  whose diff edits the tracked `.kranz/workspace.json` to add
  `"mounts": ["/"]` or `["/Users/<op>/.ssh"]` or `["/var/run/docker.sock"]`,
  merged through the normal (operator-approved, gate-passing) merge path. The
  *next* mission's provision then honours it.
- **Impact:** an arbitrary host path is bind-mounted **read-write** into the
  workspace container, whose `bootstrap[]`/`readiness[]` commands are executed
  via `compose exec -T workspace sh -c …`. `/var/run/docker.sock` is a full
  host takeover; `/` is a full host filesystem write. The base-branch pinning
  that protects `merge-gates.json` and the contract's *commands* does not help
  here, because the merge gate has no rule about contract mount changes.
- **Suggested fix:** validate `mounts[]` against a policy — refuse `/`,
  `/etc`, `/var/run`, `$HOME` and any ancestor of the kranz repo; default to
  `:ro` with an explicit opt-in for rw; and surface a contract-mount diff as a
  named finding in the merge pre-gate the way `merge.rs` surfaces the secret
  scan. `docs/knowledge/validation/gates.md`'s "the mission branch cannot
  weaken the policy judging itself" argument should extend to this file.
- **Existing test coverage:** `workspace_contract.rs:640-652` tests only the
  `..`/absolute rule; there is no policy test.

### MED-5 — AppContainer launch plan lives in a directory the contained principal is granted `rwx`

- **Severity:** MEDIUM
- **Confidence:** PLAUSIBLE (I traced the write and the grant; I did not build
  a working race)
- **Refs:**
  - `crates/engine/src/appcontainer_windows.rs:2296-2312` — the plan JSON is
    written to `inputs.tmpdir/appcontainer-plan-<uuid>.json`
  - `crates/engine/src/appcontainer_windows.rs:1991-1998` — every entry of
    `write_allowlist(inputs)` (which includes `inputs.tmpdir`) gets an
    inheritable `FILE_GENERIC_READ|WRITE|EXECUTE|FILE_DELETE_CHILD|DELETE` ACE
    for the AppContainer SID
  - `crates/engine/src/appcontainer_windows.rs:2286-2292` — the plan carries
    `allow_network`, `executable`, `args`, `cwd`, `path`
- **Attacker + preconditions:** a contained process that is live and sharing
  the same `tmpdir` at the moment a *subsequent* launch plan is written. That
  is the gate posture, where one `AppContainerLease` spans several contract
  commands (`lease.plan_paths.push`, `:2293`). Within a single agent session
  the child does not exist yet when the plan is read, so the window is narrow.
- **Impact:** the plan is the trusted launcher's only input. Overwriting it
  between `create_new` and the helper's read flips `allow_network` (turning a
  hard-offline `fs+net` AppContainer into one holding the `internetClient`
  capability — `appcontainer_windows.rs:2545-2551`) or substitutes the
  executable/args. The ACLs already applied constrain what the substituted
  executable can reach, so this is a boundary weakening rather than an escape.
- **Suggested fix:** write the plan into a directory that is *not* in
  `write_allowlist` (a per-launch dir under the engine's own state, with no
  AppContainer ACE), or pass the plan to the helper over an inherited anonymous
  pipe / on stdin instead of by path, or HMAC the plan with a key held only by
  the parent and verified by the helper.
- **Existing test coverage:** none found for plan integrity.

---

## LOW

### LOW-1 — the container relay's host-side proxy binds `0.0.0.0`

- **Severity:** LOW · **Confidence:** CONFIRMED
- `crates/engine/src/container_egress.rs:184-190`:
  `EgressProxy::start_authenticated_bound(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)), …)`
- The bind is `INADDR_ANY` so the relay can reach it via
  `host.docker.internal:host-gateway`. It is bearer-token protected
  (128 hex chars of UUID entropy, constant-time compared —
  `egress_proxy.rs:424-437`), so this is exposure widening rather than an open
  relay: anyone on the LAN can reach the listener and probe it. Fix: bind the
  specific docker-bridge gateway address (or publish through a container port
  rather than the host wildcard).

### LOW-2 — `workspace.remote` sends its substrate token over plain HTTP and across redirects

- **Severity:** LOW · **Confidence:** PLAUSIBLE
- `crates/engine/src/workspace_remote.rs:654` accepts `"http"` as a valid
  `baseUrl` scheme; `:680-681` sends the session token header on every
  request. `reqwest`'s default redirect policy is `limited(10)`, and its
  sensitive-header stripping covers `Authorization`/`Cookie`/`Proxy-Authorization`
  only — a custom header like `Coder-Session-Token` is forwarded across a
  cross-host redirect. A hostile or compromised substrate (or a MITM on the
  plaintext path) therefore harvests the operator's Coder session token.
- Fix: refuse `http://` for non-loopback hosts, and set
  `.redirect(reqwest::redirect::Policy::none())` on the client.
- To confirm: check the pinned `reqwest` version's
  `remove_sensitive_headers` list in `Cargo.lock`.

### LOW-3 — macOS has no `--die-with-parent` equivalent; a killed engine orphans the agent tree

- **Severity:** LOW · **Confidence:** CONFIRMED
- `crates/engine/src/backend_claude.rs:872` sets `.kill_on_drop(true)` and
  `:886` `command.process_group(0)`; `kill_group` (`:1015`) SIGKILLs the group
  on cancel. All of that requires the engine to run its own Drop/cancel path.
  Linux gets `--die-with-parent` (`sandbox.rs:1266`) and Windows gets a
  kill-on-close Job Object (`backend_claude.rs:896-908`); macOS gets neither.
  `kill -9 kranz` therefore leaves `sandbox-exec` + the agent CLI + its tool
  subprocesses running (still sandboxed, still spending). Fix: a watchdog that
  reaps by recorded pgid on next start, mirroring
  `container_egress::recover_stale_boundaries`.

### LOW-4 — mount-proof cache is process-lifetime and never invalidated

- **Severity:** LOW · **Confidence:** CONFIRMED
- `crates/engine/src/sandbox_container.rs:337-357` `cached_bind_mount_proof`
  memoizes per `(runtime, path)` "for the life of the process", justified by
  "the answer cannot change while a daemon keeps running". A daemon *restart*
  with a changed file-sharing list is exactly a case where it changes, and a
  long-lived `kranz serve` will keep serving `Proven`. Consequence is the
  silent-empty-mount hazard the whole module exists to prevent. Fix: key the
  cache on the daemon's boot/instance id (`docker info -f '{{.ID}}'`) or add a
  TTL.

### LOW-5 — `fcntl(F_SETFL, O_NONBLOCK)` discards existing flags

- **Severity:** LOW (correctness, not security) · **Confidence:** CONFIRMED
- `crates/engine/src/pty_harness.rs:514-516` writes the flag word rather than
  OR-ing it into `F_GETFL`. No flag that matters is set on a freshly opened
  pty master, so there is no live bug; it is a latent one. The return value is
  also ignored, so a failure silently turns the poll loop blocking.

### LOW-6 — container mount specs are not validated against `:` in paths

- **Severity:** LOW · **Confidence:** CONFIRMED
- `crates/engine/src/sandbox_container.rs:465-470` `mount_arg` formats
  `host:host[:ro]`. A colon in an `extraWrite` path or a checkout path makes
  the runtime mis-count the delimiters. Docker errors out ("too many colons"),
  so this fails closed today, but the file already handles one delimiter hazard
  (the Windows `\\?\` verbatim prefix) and should reject colons explicitly
  rather than relying on the runtime's parser.

---

## INFO

- **INF-1 — broad `(allow file-read*)` on macOS.** `sandbox.rs:1077` allows
  reads of everything (`~/.ssh`, `~/.aws`, other repos, the keychain files)
  with the `authority_read_deny_paths`/`_dirs` carve-out. This is explicitly
  documented as the design (`sandbox.rs:1073-1076`: "Secrecy is not the fs-tier
  promise — write containment is") and matches SECURITY.md's "not a
  general-purpose containment boundary". Not a finding, but worth restating
  that the *only* secrets the process tier protects are the kranz serve
  tokens, the repo config, the domain vocabulary, and the cargo credentials —
  a prompt-injected agent reads `~/.ssh/id_ed25519` under `enforce: fs` and
  (under `fs+net`) can only exfiltrate it through an allowlisted host.
- **INF-2 — SBPL string/regex escaping is correct.** `escape_sbpl_string`
  (`sandbox.rs:730`) escapes `\` and `"`; `escape_sbpl_regex` (`sandbox.rs:738`)
  escapes `\ " ^ . + $ * ? ( ) [ ] { } |`. A path containing quotes, parens,
  semicolons, or newlines stays inside the string literal — I could not
  construct an injection. Every path that reaches the profile goes through one
  of the two.
- **INF-3 — bubblewrap arg injection is not possible.** Paths are pushed as
  separate `Vec<String>` argv elements (`sandbox.rs:1281-1286`) and the argv
  ends with `--` before the binary (`sandbox.rs:1447`). A path beginning with
  `--` becomes the operand of `--bind`, never a flag.
- **INF-4 — egress allowlist matching is sound.** `AllowEntry::matches`
  (`egress_proxy.rs:138-148`) requires `host.len() > suffix.len()` **and**
  `ends_with(".<suffix>")`, so `evil-github.com` cannot match `github.com` and
  the apex is deliberately excluded from a `*.` entry. Ports must match
  exactly. `parse_connect_request` (`:172-190`) refuses IPv6 literals, empty
  hosts, path-shaped authorities, port 0, and anything that is not exactly
  three whitespace-separated tokens; the head read is bounded to 8 KiB with a
  10 s timeout (`:491-520`); pipelined bytes past the header block are
  forwarded correctly (`:468-474`), so there is no smuggling gap I could find.
  Malformed entries fail the whole allowlist parse (`:151-153`) rather than
  being silently dropped.
- **INF-5 — the container relay's header injection is fail-closed.** A worker
  that pre-inserts its own `Proxy-Authorization` header wins the
  `head.lines().skip(1).find_map` scan in `proxy_authorization`
  (`egress_proxy.rs:479-487`) *before* the relay's appended real token
  (`container_egress.rs:80`), and so gets a 407. Confusing, but the wrong
  direction for an attacker. The worker never sees the token.
- **INF-6 — `unsafe` FFI review.** All 21 non-Windows FFI sites are correct.
  `statvfs` (`disk_preflight.rs:44-49`) checks `rc` and rejects interior NULs
  via `CString::new(...).ok()?`. `getpwuid_r` (`agent_env.rs:345-364`) uses the
  reentrant form with a 4 KiB buffer, checks `rc`, `entry_ptr`, and
  `pwd.pw_dir` for NULL, and copies out of the buffer before it drops
  (it does not retry on `ERANGE`, but returns `None`, which falls back
  correctly). `proc_pidinfo` (`event_log.rs:1225-1237`) zeroes the struct and
  checks `rc <= 0` — it does not check `rc == size_of::<proc_bsdinfo>()`, so a
  short read would render the epoch rather than fail, a cosmetic issue in an
  identity token. `libc::kill(pid, 0)` liveness polls and
  `libc::kill(-pgid, SIGKILL)` are safe by construction.
  `pty_harness.rs:451-548` handles every fd exactly once: `openpty` failure
  leaves both at `-1`, the `dup` failure path closes master/slave and any
  successful dup, `Stdio::from_raw_fd` takes ownership of three distinct fds,
  and the spawn-failure path closes only `master`. The `pre_exec` closure uses
  `setsid`/`ioctl(TIOCSCTTY)`, both async-signal-safe, and `slave` is still
  open in the child at that point because std dup2s stdio before running
  pre-exec callbacks. `mkfifo` uses are test-only. The Windows `JobHandle`
  (`backend_claude.rs:88-162`) closes its handle exactly once in `Drop`;
  `FreeSid` on a `CreateAppContainerProfile` SID (`sandbox_windows.rs:502-504`)
  matches the documented contract; `InitializeProcThreadAttributeList` is
  correctly two-phase-sized (`:530-537`).
- **INF-7 — fail-open audit came back clean.** I traced every resolution path
  and found no arm that runs unsandboxed while reporting sandboxed:
  `resolve_for_session_target` (`sandbox.rs:246-287`) returns `(None, warn)` and
  `runner::resolve_sandbox_or_refuse` (`runner.rs:1481-1503`) converts that to
  an `Err` whenever `enforce != Off`; `backend_claude::start`
  (`backend_claude.rs:869-875`) errors on a resolved backend that the platform
  cannot apply; `resolve_gate_sandbox_target` (`command_exec.rs:892-916`)
  fails closed on unsupported platform, missing `bwrap`, missing container
  runtime, unproven mount contract, and the advisory `fs+net`+egress container
  pair; `egress_proxy::maybe_start_for_session_with` (`egress_proxy.rs:618-622`)
  turns a proxy-start failure into a refused run and leaves no proxy env
  behind; the container mount proof (`sandbox_container.rs:239-303`) refuses on
  a one-way mount rather than trusting the runtime's exit 0.
- **INF-8 — symlink/TOCTOU handling is above average.** `lexical_absolute`
  (`sandbox.rs:722`) deliberately does not resolve mount destinations so a
  hostile symlink cannot redirect a mask; the `state.json.tmp` pre-creation
  (`sandbox.rs:1359-1408`) refuses anything that is not a regular file and then
  opens through `paths::open_parent_nofollow` + `FollowSymlinks::No`;
  `disk_preflight::dir_size_capped_with_entry_limit`
  (`disk_preflight.rs:92-107`) pins the parent and opens the leaf no-follow so
  a repo-authored symlink is never priced as its target;
  `remove_recovered_credential_dir` (`container_egress.rs:739-757`) canonicalizes
  the parent and requires both the root match and the name prefix before any
  `remove_dir_all`. The AppContainer lease retains no-follow handles for every
  DACL it touches. I found nothing to report here.
- **INF-9 — resource bounds are in place.** `stream_bounds.rs` keeps an
  amortized-O(1) tail window (8 MiB per stdout line, 64 KiB total stderr);
  `disk_preflight` caps the `target/` walk at 256 GiB and 250 000 entries;
  the egress proxy bounds the request head and reaps finished tunnel tasks
  (`egress_proxy.rs:344-346`); `EgressProxy::Drop` aborts both task sets. No
  finding.

---

## Areas checked with no finding

- **SBPL profile string construction** — escaping of quotes, backslashes,
  newlines, parens, semicolons and regex metacharacters in every path that
  reaches `generate_profile`. No injection path (INF-2).
- **bubblewrap argument construction** — leading-`--` paths, the `--` sentinel,
  bind ordering (later binds win, and the validator masks are emitted after
  every rw bind so a wide writable root cannot re-expose a denied entry).
  No injection path (INF-3). The finding here is missing *flags*, not
  malformed argv.
- **Egress allowlist matching** — suffix confusion, apex matching, case
  folding, IP literals, IPv6, port handling, malformed-entry handling.
  All correct (INF-4).
- **CONNECT parsing / request smuggling / unbounded buffering** — head size
  cap, dribble timeout, EOF handling, leftover-buffer forwarding, one CONNECT
  per connection. No smuggling gap found (INF-4).
- **Container relay token handling** — the worker never receives the token; a
  forged `Proxy-Authorization` fails closed; the credential dir is 0700/0600,
  loaded via a stopped networkless `--cap-drop ALL` loader through `docker cp`
  with no host bind, and deleted before any worker starts; the relay image is
  digest-pinned. Stale-boundary recovery keys on pid + immutable process
  identity, so it cannot reap a live sibling. No finding (INF-5).
- **Fail-open behaviour** across session, validator, gate, merge-gate, egress
  and container-mount resolution. Clean (INF-7).
- **`unsafe` correctness** — all 21 non-Windows FFI call sites plus spot checks
  of the Windows Job Object, SID, and attribute-list code. No memory-safety or
  fd-leak defect (INF-6).
- **Symlink / TOCTOU on workspace roots, scratch dirs, mask targets, and
  credential cleanup.** Clean (INF-8).
- **Resource limits / stream bounds / process-group kill on cancel.** Clean on
  Linux and Windows; the macOS orphan case is LOW-3 (INF-9).
- **`sandbox_windows.rs`** — the experimental-API probe loads
  `processmodel.dll` with `LOAD_LIBRARY_SEARCH_SYSTEM32` only (so a
  worker-planted DLL on PATH/cwd cannot spoof capability evidence), and the
  probe records capability without ever claiming enforcement it did not
  achieve. No finding.
- **`workspace_provider.rs`** — provider selection fails closed on an unknown
  name and on incomplete remote config, at both plan approval and run start.
  No finding.
- **`disk_preflight.rs`** — degrades to "proceed" only when `statvfs` genuinely
  fails, never fabricating a refusal; walk is bounded; symlinks are not
  traversed. No finding.
- **Validator containment resolution** — the `enforce != off`,
  `enforce == off`, unsupported-platform, missing-`bwrap`, and
  unsupported-backend arms all end in `Err` unless
  `validatorAllowUncontainedDegrade` is set. The `.git`/`.kranz` carve-outs are
  reasoned and the residual gaps (entries created after profile generation,
  the root's own listing) are documented in-code. No finding beyond HIGH-3's
  observation that the config flag enabling the degrade is itself writable in
  checkout mode.
