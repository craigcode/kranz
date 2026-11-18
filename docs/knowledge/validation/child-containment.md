---
title: Child containment (children are hostile by default)
owner: human
freshness: check-on-touch
last_verified: 2026-11-18
verified_against:
  - AGENTS.md
  - crates/engine/src/agent_env.rs
  - crates/engine/src/command_exec.rs
  - crates/engine/src/contract_shell.rs
  - crates/engine/src/workspace_gate.rs
  - crates/engine/src/workspace_data.rs
  - crates/engine/src/workspace_provider.rs
  - crates/engine/src/workspace_container.rs
  - crates/engine/src/sandbox.rs
  - crates/engine/src/validator_snapshot.rs
  - docs/acp-containment.md
---

Rechecked 2026-11-18 (UTC) by an agent under the human-note rule in `CONVENTIONS.md`: `AGENTS.md`'s Even G2 command line now runs `scripts/check-npm-audit.py` in place of `npm audit --audit-level=high`, and the macOS test `sandbox_fs_tier_reaches_only_its_own_unix_sockets` in `sandbox.rs` now measures the canonical temp path before its socket-length check, so a short symlink to a long directory no longer makes the bind fail. Test code only. No containment rule, sandbox profile or gate signal policy changed, and nothing this note states changed.

Rechecked 2026-11-17 (UTC) against a test move in `command_exec.rs`, `appcontainer_windows.rs` and `.github/workflows/ci.yml`: the contained contract-shell checks (the `cmd.exe` quoting check and the Git `sh` attempt that prints `KRANZ_WINDOWS_SH_CONTAINED=`) now run inside the CLI's AppContainer normal-gate self test, because the AppContainer launcher is the `kranz` binary itself. No shell selection, containment rule or gate behaviour changed.

Rechecked 2026-11-16 (UTC) against the Windows contract-shell change in `command_exec.rs`, `contract_shell.rs` and `appcontainer_windows.rs`: on Windows an uncontained contract command now runs under Git for Windows' `usr\bin\sh.exe`, found from the engine's own Git installation, canonicalized, and refused if it lies inside any worker-writable root. It is never resolved by bare name from a child's `PATH` or working directory. The AppContainer contract path stays on `cmd.exe` until the CLI normal-gate self test that Windows CI runs prints `KRANZ_WINDOWS_SH_CONTAINED=pass`; no root, ACL grant, capability or profile rule was added to try it. `cmd.exe` receives the command as `/S /C "<command>"` with no escaping added. Containment rules and the gate signal policy are unchanged, and a wrap or resolution failure still fails closed. The shell table is in [gates.md](gates.md).

Rechecked 2026-11-15 (UTC) against a test-only change in `command_exec.rs`: one gate test waits longer for its stub runtime to record its launch on a loaded host. No product code changed, and the containment rules here are unchanged.

Rechecked 2026-11-13 (UTC) against a test-only change in `agent_env.rs`, `command_exec.rs`, `comparison_metrics.rs`, `knowledge.rs` and `sandbox.rs`: five engine tests no longer depend on where the contract scratch directory lives. No product code changed. The unix socket reach test now prints `SKIP-UNDER-WRAP` when it has no short writable temp root for a socket path. Containment rules are unchanged.

Rechecked 2026-11-11 (UTC) against the Linux socket mask: the bubblewrap wrap shadows the known host control sockets that exist (Docker, containerd, Podman, CRI-O, snapd, the system bus, the systemd private socket, the per-user runtime directory, and the Docker Desktop, Colima, Lima and Rancher directories under the operator's home), on both tiers, unless the location is inside a path the operator made writable. It is a list of known places: a control socket elsewhere and abstract-namespace sockets under `fs` stay reachable.

Rechecked 2026-11-11 (UTC) against the containment fixes: workspace bootstrap, readiness and data commands receive `SSH_AUTH_SOCK` and the operator's global git config only when the contract's `secrets[]` declares the name and `contractEnvPassthrough` lists it, and otherwise commit as a neutral identity; cleanup kills a child's descendant tree by parent links, so a descendant in its own session dies with it (a double-forked process whose parent already exited is still not found); the validator snapshot opens each untracked file once with no link followed at any component.

Rechecked 2026-11-11 (UTC) against the sandbox fixes. The Seatbelt `fs` tier keeps IP networking but denies unix-socket connections except the system resolver, syslog and sockets under the session's own writable roots, so a contained child cannot drive the Docker socket or the ssh-agent; another socket needs its directory in the operator-only `sandbox.extraWrite`. Bubblewrap pathname sockets are not yet restricted. Seatbelt profile files (`runs/kranz-sandbox-*.sb`) are write-denied to the code they confine. One exception to "`off` keeps the env-only posture" below: the approval-time contract lint runs planner-authored commands before human review, so it always runs contained (raised to `fs`) or is skipped, and receives no `contractEnvPassthrough` credentials.

Workspace bootstrap, readiness and data hooks now use the resolved worker gate sandbox as well as the cleared environment. Resolution or launch failures refuse execution. Between-round resets refresh the current worker policy after runtime changes. Container `disk.prune` host commands are refused before provisioning; owned Compose teardown remains available.

# Child containment

Moved verbatim from AGENTS.md rule 10 on 2026-11-06. AGENTS.md keeps a short
summary and points here.

**Children are hostile by default (2026-07-28 hardening):** every
prompt-injectable child spawns `env_clear`d (`agent_env.rs`) — never
reintroduce ambient inheritance; the sanctioned credential channels are
`contractEnvPassthrough` (contract commands) and the workspace contract's
`secrets` (bootstrap/data hooks). Enforced sandboxes are honored by the native claude backend and the
explicit qualified ACP worker profiles (docs/acp-containment.md; `-v2`
profiles additionally verify and pin the admitted local Docker host before
session mount proofs or `env_clear`d container-egress provisioning). ACP profiles pin
image, startup and selected file credentials; generic ACP commands and all
other unsupported pairs remain refused. Do not widen without proof. `serve.token` is mutation authority, `serve.read.token`
reads only, and `serve.grant.key` signs Sgian execution grants; all three must stay unreadable inside sandboxes
(`authority_read_deny_paths`). Engine-run gates (validation-round,
final-gate, and merge-gate commands) also execute worker-authored code:
they run WRAPPED in the resolved worker sandbox profile when
`sandbox.enforce != off` (`command_exec::GateSandbox`,
`run_bounded_gate_command_sandboxed`); `off` keeps the env-only posture
byte-for-byte. The gate wrap's supervision policy is gate-SPECIFIC
(ticket gate-sandbox-supervision-dogfood, `gate_profile_extras`):
`(allow signal (target same-sandbox))` lets a wrapped gate signal its
OWN descendant tree (never host processes) so `cargo test --workspace`
runs green as a wrapped contract command — the session profile
generator stays untouched, and what no sandbox can host (setuid
`/bin/ps` exec, nested `sandbox_apply`) skips with the detectable
`SKIP-UNDER-WRAP` marker, held green by the `rust-macos-wrapped-suite`
CI job. Validators never see the real checkout:
each session runs in a throwaway snapshot (`validator_snapshot.rs`,
warmed `target/` copy included) and only its verdict crosses back — and
the snapshot is separation, not containment, so validator sessions are
ADDITIONALLY wrapped regardless of `sandbox.enforce`
(`sandbox::resolve_validator_containment`): the snapshot is the sole
writable root, the real checkout's source tree is read-denied, and the
shared `.git` is readable but write-denied; uncontainable
platforms/backends FAIL CLOSED by default (14th-pass reversal of the
224fa73 loud-degrade decision — the degrade reopens the
modify→use→restore path; `validatorAllowUncontainedDegrade` in
`.kranz/config.json` is the explicit per-repo opt-in back to the loud
per-round degrade). The `validator.tamper` fingerprint on the real checkout is
the tripwire (defense-in-depth) whose drift means the isolation itself
failed.
