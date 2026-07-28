---
title: Clear ambient environment for agent + contract subprocesses (P1 trust boundary)
priority: 1
schedule: once
---

## Goal
Spawn every agent backend and every contract/gate command with `env_clear`
plus a minimal allowlist, instead of inheriting the full ambient environment
(backend_claude.rs:769 `.envs(&spec.env)`; command_exec.rs contract commands
with `clear_env = false`). Ambient server secrets (Slack tokens, GH_TOKEN,
cloud credentials, remote-workspace tokens) currently reach every
prompt-injectable child.

## Context
From the 2026-07-28 hostile-workload review (P1 #1). sanitized_gate_env
already exists for merge gates — extend the pattern: contract commands get a
per-mission scratch HOME + toolchain allowlist; agent backends get the
allowlist plus backend-specific auth injected explicitly (never the ambient
set). Additive escape hatch: a config `contractEnvPassthrough: [names]` for
contracts that legitimately need one named var. OAuth seeding must keep
working via the existing scratch-HOME path.

## Acceptance hints
- A secret set in the parent env (GH_TOKEN, SLACK_BOT_TOKEN, AWS_*) is
  ABSENT from every spawned agent session env and every contract command env.
- PATH/scratch-HOME/toolchain vars present; backend auth key present only
  for the backend that needs it.
- Passthrough config lets exactly the named var through, and is logged.
- cargo test --workspace green incl. an exfiltration-proof test.
