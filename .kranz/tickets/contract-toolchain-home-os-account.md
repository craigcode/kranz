---
title: Derive contract/gate toolchain HOME from the OS account, not the ambient env
priority: 2
schedule: once
---

## Goal
Make engine-run contract and gate commands resolve the operator toolchain home (CARGO_HOME/RUSTUP_HOME derivation, agent_env.rs) from the OS account record (getpwuid_r/passwd) rather than the ambient HOME env var, which is absent or wrong in env_clear'd and sandboxed gate contexts. During m-eee81f, five separate workers independently diagnosed this and tried to fix agent_env.rs out-of-scope — all reverted or grant-parked (branch commits 898f77c, 5910a1a, cc8b671, b15f561, plus the parked grant at event seq 6542). Verify the exact failure mode first; the fix must not weaken the env_clear / scratch-HOME posture for agent sessions themselves.

## Context

agent_env.rs resolves the real operator home via `std::env::var_os("HOME")`
(around agent_env.rs:298) and derives CARGO_HOME/RUSTUP_HOME from it for
engine-run contract/gate commands. In contexts where the engine itself was
spawned with a cleared or relocated env the derivation silently degrades, and
rustup shims/cargo then fail in ways workers misread as in-scope mission
bugs — hence the five independent out-of-scope fix attempts on m-eee81f.
Related, already landed: contract-cargo-home-cache-only (merge gates get a
fresh cache-only CARGO_HOME) and engine-gates-sandbox-wrapped. This ticket is
the remaining derivation-robustness slice. The security posture is not
negotiable: agent sessions keep env_clear + scratch HOME; only the
operator-toolchain resolution for engine-run commands changes.

## Scoping answers

## Acceptance hints

- Reproduce the failure first (a gate/contract command launched with HOME
  unset or relocated) and name the exact broken path in the plan; if the real
  cause differs from the m-eee81f workers' diagnosis, say so and re-scope.
- With HOME unset in the engine's own environment, engine-run contract/gate
  commands still resolve the correct operator CARGO_HOME/RUSTUP_HOME from the
  OS account record; env var remains an explicit override only.
- Agent (worker/validator) session envs are byte-identical to before —
  env_clear + scratch HOME untouched; regression test pinning that shape.
- Anti-vacuity: test-name filters must match only the new tests.
