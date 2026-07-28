---
title: Prove validators cannot write: before/after assertions + drop wildcard Bash (P1)
priority: 2
schedule: once
---

## Goal
The "read-only validator" is nominal today: `writable: false` is not
enforced by the CLI, contract commands become `Bash(<cmd>*)` wildcard
prefixes (as loose as `python3 -*`), and the sandbox allows writes to the
session checkout. Make it structural: assert HEAD/index/worktree are
byte-identical before and after every validator session (a validator that
commits or edits fails the round), and drop the `python3 -*` wildcard
from command allow patterns. Engine-run contract commands (shipped) make
validator Bash unnecessary for the contract itself; scrutiny's split
(shipped) keeps it read-only by construction.

## Context
From the review (P1 #5). The out-of-contract sweep detects tampering
post-hoc; this turns detection into prevention per session.

## Acceptance hints
- A validator that writes/commits fails its round with a named violation.
- python3 -* is no longer in any validator allow-set; existing contracts
  using heredoc forms get a documented engine-run path.
- cargo test --workspace green.
