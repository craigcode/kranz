---
title: Fresh-repository onboarding with kranz init
priority: 3
schedule: once
blocked-by: [m8-multi-root-host-design]
---

## Goal

Add an idempotent `kranz init` first run that turns an existing Git worktree
into a Kranz-ready repository without overwriting operator-owned files. It
must scaffold tracked merge gates, a committed-safe runtime ignore template,
and the ticket directory; detect common Rust, Node, and Python validation
commands; and optionally register the canonical repository root in the
operator's multi-repo host catalog.

## Context

This is the final implementation seam in the accepted M8 multi-root design.
Per-repository mission state, tickets, gates, calibration, and lessons remain
local. Host membership remains operator-owned global configuration. The
compiled worker-isolation default is already `worktree`, so onboarding should
report that safety default instead of writing a redundant project override.

Cold-start estimates are honest only when the first-run output says they are
based on zero completed missions. A repository with an unfamiliar toolchain
must fail with an actionable `--gate` instruction rather than create an empty
or conditional-only gate suite.

## Acceptance hints

- A fresh Rust or Node Git repository gets a valid unconditional
  `.kranz/merge-gates.json`, runtime ignore rules, and a trackable tickets
  directory without editing existing file content away.
- `--gate <COMMAND>` is repeatable and supports unfamiliar toolchains; zero
  detected/provided gates fail closed.
- Re-running `kranz init` is byte-idempotent and validates existing gates
  instead of replacing them.
- `--register` merges one canonical root into `~/.kranz/config.json`, keeps
  unrelated keys/secrets, and rejects duplicate ids or roots.
- Output explicitly reports worktree isolation and `0 completed missions`
  for a cold repository.
- `cargo test --workspace kranz_init_ 2>&1 | grep -qE 'test result: ok\. [1-9]'
  passes, guarding the named filter against vacuity.
