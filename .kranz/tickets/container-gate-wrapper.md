---
state: done
state-note: Done: container gates execute inside the mission container (run --rm --read-only, mission ro, authority masked, named + teardown); runtime-unavailable and fs+net+egress fail closed; real Cargo root never mounted. Live fixture runtime-gated for CI; 7 unit tests green locally; full gates green.
title: "Container gate wrapper — engine-run gates inside the mission container"
priority: 1
schedule: once
---

# Container gate wrapper

Source: 13th-pass review (2026-08-03, P1) — `command_exec.rs`
`resolve_gate_sandbox_target`. With `provider: container` and
`enforce: fs/fs+net`, engine-run gates (validation, final gate, merge
gates) resolve to `GateSandbox::Disabled` and run worker-authored build
scripts and test binaries on the HOST — the degradation is recorded
(engine decision event + merge-path warn, landed), but recording is not
containment.

## Problem

The container provider wraps agent SESSIONS in containers with real
isolation; engine-side gates run on the host beside it. A
container-configured operator reasonably expects every mission-code
execution to be contained.

## Design (direction)

Run engine-run gates INSIDE the mission's container: the gate command
executes through the container provider's exec path (the same one
sessions use) with the gate shape mounted — worktree + target/ + the
cache-only contract home, metadata write-denied, authority read-denied.
The process-sandbox gate wrap (engine-gates-sandbox-wrapped, landed) is
the behavioral contract to mirror; the container wrapper reuses
`resolve_gate_sandbox`'s resolution and the bounded-run core (timeouts,
tree kill).

Open work named up front:

- The container provider's exec API today is session-shaped; a
  one-shot command exec needs the same env-clearing + bounded-pipe
  discipline as `run_command_bounded`.
- Image/toolchain assumptions: the gate runs the repo's own toolchain
  (cargo etc.) — the mission image must carry it, or the wrapper mounts
  the host toolchain read-only (decide and document).
- Until this lands, the recorded degradation stays; fail-closed on
  container+enforce is the alternative posture — decide deliberately.

## Test gate

- With provider: container + enforce != off, a contract command executes
  inside the container (probe: writes outside the mount set fail; the
  host's serve.token is unreachable) — proven by fixture against the
  local container provider.
- Workspace gates green, bare exit codes, never piped.

## Out of scope

The process-sandbox gate wrap (landed), container image build/policy,
egress proxy posture.
