---
title: "Sandbox-wrap engine-run gate commands when enforcement is on"
priority: 2
schedule: once
---

# Sandbox-wrap engine-run gate commands when enforcement is on

Stage 2 of `contract-cargo-home-cache-only` (which landed stage 1: cache-only
`CARGO_HOME` for every engine-run contract/gate path). Engine-run validation,
final-gate, and merge-gate commands (`command_exec::run_shell_command` /
`run_bounded_argv` / `run_bounded_gate_command`) execute worker-authored build
scripts and test binaries OUTSIDE any sandbox profile, with full network and
— for merge gates — ambient `HOME`. Stage 1 closed the Cargo credential link;
this closes the rest of the filesystem/egress surface.

## Problem

The operator approved the contract's COMMAND TEXT at plan approval. The
worker's build scripts and test binaries inherit whatever filesystem and
network posture the engine process has. Agent sessions are profile-wrapped;
engine-run gates are not — an asymmetry a prompt-injected worker can aim at
(the gate is the one place worker code is guaranteed to run).

## Design

Wrap engine-run gate commands in the resolved sandbox profile when
`sandbox.enforce != off`, mirroring how preflight probes already run under the
profile (`sandbox_command_preflight`). `enforce == off` keeps today's behavior
(the operator opted out; the cache-only `CARGO_HOME` still applies).

Open work named up front:

- The profile's write allowlist needs the gate shape: worktree + `target/` +
  the cache-only contract home. Reuse the session profile's writable-root
  computation rather than a second hand-rolled list.
- The merge gate's ambient `HOME` pass-through (kept for git identity and
  toolchain config) should be re-evaluated under the profile: a read-only
  HOME mount plus a writable scratch may replace the pass-through entirely.
- Performance: full workspace gates under Seatbelt/bwrap spawn cost needs
  measuring on a real mission before this becomes the default — if the cost
  is material, gate the wrapping on `sandbox.enforce == strict`-style opt-in
  first.

## Test gate

- A contract command run under `enforce != off` provably executes inside the
  profile: a probe command that attempts a write outside the allowlist fails
  under enforcement and succeeds with `enforce == off`.
- Workspace gates green on all three OS CI legs, bare exit codes, never
  piped.

## Out of scope

Changing what runs (contract text is operator-approved), the agent-session
profile itself, and the egress proxy posture.
