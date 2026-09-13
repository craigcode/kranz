---
state: done
state-note: Landed at a5a5f23: merge gates substitute a fresh cache-only CARGO_HOME (fail-closed); contract paths already covered via contract_command_env. Stage 2 split into engine-gates-sandbox-wrapped. contract_cargo_home filter: 2 tests green.
title: "Engine-run gate commands: cache-only CARGO_HOME, then sandbox-wrapped execution"
priority: 2
schedule: once
---

# Engine-run gate commands: cache-only CARGO_HOME, then sandbox-wrapped execution

Source: 7th-pass review P1 (agent_env.rs CARGO_HOME restoration). The sandbox
read-deny of `credentials.toml` covers sandboxed AGENT sessions, but
engine-run validation/final-gate commands (`run_shell_command` /
`run_bounded_argv`) execute OUTSIDE any sandbox profile — with full network
and `CARGO_HOME` pointing at the operator's real cargo home. An
operator-approved `cargo test` necessarily runs WORKER-AUTHORED build
scripts and test binaries, which can read `credentials.toml` (and
credential-provider config) and exfiltrate over unrestricted egress.

## Problem

The trust chain has one uncovered link: mission-authored code (build.rs,
test binaries) runs engine-side, unsandboxed, beside a credential
directory. Agent sessions are env-cleared and profile-wrapped; engine-run
contract commands are neither (env-cleared since agent-env-clear, but never
profile-wrapped). The operator approved the COMMAND TEXT at plan approval —
they did not approve the worker's build scripts reading their registry
tokens.

## Design (locked, two stages)

**Stage 1 — cache-only CARGO_HOME (this ticket's fix).** Contract commands
get a per-mission CARGO_HOME at `<mission>/runs/contract-home/.cargo`
containing ONLY the cache, never credentials: the engine seeds it with
symlinks to the real home's `registry/` and `git/` dirs (read-mostly;
documented that a poisoned-cache write by worker code would land in the
operator's shared cache — so seed per-mission COPIES via the tiered
clonefile/reflink copy already built for validator snapshots when the
registry is small, symlink otherwise with the trade named). No
`credentials.toml`, no `credentials`, no credential-provider config is ever
linked or copied. A contract needing a private registry gets its token via
`contractEnvPassthrough` (the existing sanctioned channel).

**Stage 2 — sandbox-wrapped engine gates (follow-up, named).** Wrap
`run_shell_command`/`run_bounded_argv` in the resolved sandbox profile when
`sandbox.enforce != off`, mirroring how preflight probes already run under
the profile (sandbox_command_preflight). Sized as its own ticket: the
profile's write allowlist needs the worktree + target + contract-home
shape, and the performance question (full workspace gates under Seatbelt
spawn cost) needs measuring first.

## Test gate

- `cargo test --workspace contract_cargo_home 2>&1 | grep -qE 'test result: ok. [1-9]'`
  — the seeded contract cargo home contains registry/git caches and NEVER
  any credentials-shaped file; a contract command's env shows CARGO_HOME
  pointing at it; `contractEnvPassthrough` can still name a credential var
  explicitly.
- Workspace gates green, bare exit codes, never piped.

## Out of scope

Changing what runs (contract text is operator-approved), the egress proxy
posture for agent sessions, and Stage 2's actual wrapping (its own ticket).

## Resolution (2026-08-02, landed)

Stage 1 landed in two parts. The contract-command paths (validation round,
final gate, approval-time lint, preflight probes) already built
`contract_command_env` over `agent_env::cache_only_cargo_home` — with one
deliberate variance from the "locked" design above: the home is a FRESH
UNPREDICTABLE dir per generated env (`<scratch>/.cargo-cache-only-<uuid>`),
not a stable per-mission path, so worker code cannot pre-plant a poisoned
`config.toml`/credential-provider in a known location. Seeding is by symlink
into the real home's `registry/`+`git/` (the poisoned-cache-write trade is
documented at `cache_only_cargo_home`; per-mission copies were not needed —
the validator snapshot's tiered copy stays available if that changes). This
commit closed the one uncovered link: merge gates
(`command_exec::run_bounded_gate_command`) passed the AMBIENT `CARGO_HOME`
straight through; they now substitute a fresh cache-only home and FAIL CLOSED
when it cannot be created. Stage 2 is ticketed as
`engine-gates-sandbox-wrapped`.
