---
title: Child containment (children are hostile by default)
owner: human
freshness: check-on-touch
last_verified: 2026-11-11
verified_against:
  - AGENTS.md
  - crates/engine/src/agent_env.rs
  - crates/engine/src/command_exec.rs
  - crates/engine/src/workspace_gate.rs
  - crates/engine/src/workspace_data.rs
  - crates/engine/src/workspace_provider.rs
  - crates/engine/src/workspace_container.rs
  - crates/engine/src/sandbox.rs
  - crates/engine/src/validator_snapshot.rs
  - docs/acp-containment.md
---

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
