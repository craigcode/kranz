---
state: done
state-note: Fixed on codex/stabilization-proof-sprint; live m-760060 receipt reproduced both aliases, regression tests cover alias equivalence, symlink escape rejection, and scratch-HOME creation under real Seatbelt.
title: Normalize macOS temporary-path aliases in hook and validator containment
priority: 1
schedule: once
traced-from-mission: m-760060
---

## Goal

Fix the live M8 proof failures caused by macOS exposing one temporary path as
both `/var/...` and `/private/var/...`: the PreToolUse touch-set guard must
allow either spelling of an in-contract target while rejecting a real symlink
escape, and a sandbox-contained validator must be able to create Claude's
session state under its private scratch HOME.

## Context

Mission `m-760060` produced both receipts. Worker `Write`/`Edit` calls using
the canonical `/private/var/...` worktree spelling were falsely blocked, then
the same targets were allowed through `/var/...`. Both validators subsequently
received `EPERM` creating `$HOME/.claude/session-env`: the Seatbelt profile was
generated before the scratch root existed, so it could not canonicalize and
allow both temp-path spellings.

## Acceptance hints

- Hook evaluation resolves the longest existing prefix, allowing new targets
  through either checkout alias and blocking a symlink that resolves outside.
- Claude child-environment seeding occurs before Seatbelt profile generation,
  so a previously absent scratch root contributes both raw and canonical
  write-allow paths.
- A live macOS backend-start test creates `$HOME/.claude/session-env` inside
  the sandbox while an outside write remains denied.
- `cargo test --workspace hook_gate_projection_ 2>&1 | grep -qE 'test result: ok\. [1-9]'`
  and the macOS sandbox-start test pass, followed by all workspace gates.

## Verification

- `cargo test --workspace hook_gate_projection_` ran 12 matching engine tests,
  including the new alias/escape regression, plus 6 matching projection tests
  in consumer crates; all passed.
- `cargo test -p kranz-engine --test backend_claude_test
  sandbox_wrap::sandbox_wrap_macos_start_confines_spawned_process -- --exact`
  ran one live `sandbox-exec` test and passed. The test starts with a missing
  scratch root, creates `$HOME/.claude/session-env` inside the wrap, and proves
  a simultaneous outside write is denied.
