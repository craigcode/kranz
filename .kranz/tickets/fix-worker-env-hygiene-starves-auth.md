---
title: Worker env hygiene must not starve the worker of live auth (macOS Keychain)
priority: 1
schedule: once
---

## Goal

Re-enable worker HOME/CLAUDE_CONFIG_DIR hygiene WITHOUT breaking worker
authentication. The scratch-HOME relocation is currently DISABLED (see
the HOTFIX in seed_worker_env, crates/engine/src/runner.rs) because it
copied ~/.claude/.credentials.json into a scratch HOME while the live
OAuth token lives in the macOS login Keychain — so relocated-HOME
workers launched unauthenticated and produced nothing. A correct hygiene
implementation must GUARANTEE the worker can still authenticate before
trusting the scratch HOME.

## Context

Root cause (2026-07-06, m-66aff8): seed_worker_env set HOME to a scratch
dir seeded only with a copied .credentials.json. On this Mac that file
is 2 weeks stale (live token in Keychain), so `claude` in the worker
authenticated as nothing → "no report, no commits, empty diff" → total
silent work-loss, mission falsely COMPLETE. Seeding runs unconditionally
(both worker-spawn paths in runner.rs), so it broke every mission on the
post-sandbox-2 binary. The original hygiene goal (don't leak operator
dotfiles into the worker) is still valid — this ticket makes it safe.

Design options to weigh (flag as D-X in the plan):
- Detect the credential mechanism: if auth is Keychain-backed (no usable
  file credential), do NOT relocate HOME — inherit it. Only relocate
  when a real, non-stale file credential can be provisioned.
- Or: keep the real HOME but relocate ONLY the parts that leak (a
  narrower allowlist that never includes the auth path).
- Or: a preflight that spawns a trivial worker and verifies it
  authenticates under the candidate scratch HOME; fall back to the real
  HOME on failure (never launch into an env that can't auth).
- Cross-platform: the Keychain case is macOS; Linux/file credentials
  differ — the check must be per-platform.

## Acceptance hints

- A worker under the hygiene env can authenticate and produce real
  output on macOS (Keychain auth) — verified by a live mission that
  lands actual commits.
- Hygiene never silently launches a worker that cannot authenticate;
  when it cannot guarantee auth, it inherits the real HOME (loud
  decision, not silent).
- The disabled HOTFIX in seed_worker_env is replaced by the safe path;
  the no-relocation assertions in runner.rs tests are updated to the new
  contract; cargo test --workspace green.
