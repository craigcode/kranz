---
title: Stop workers running git stash in the mission worktree
priority: 2
schedule: once
---

## Goal

A worker cannot run `git stash` in the mission's worktree. A worker that wants to compare against a clean tree is told the read-only way to do it.

## Context

Seen in a mission run on 2026-11-16. Workers ran `git stash push -u` five times across two features to check whether a build failure was already present before their change. Each call moved the feature's uncommitted work, untracked files included, out of the worktree. In those runs the work came back with `git stash pop` or `git stash apply`. One worker applied a stash by hash and then dropped it by name in a later command.

The risk is the session ending between the push and the pop. A worker session can end at any tool call (turn limit, budget, a lost command). The engine then checkpoints or judges a tree that is missing the feature's work, and the work sits in a stash that nothing in the mission knows about. Nothing in the event log says a stash was taken.

`git stash` is not in the built-in worker deny list (`WORKER_DENY` in `crates/engine/src/permissions.rs`). The same question can be answered without changing the tree: `git diff <base> -- <path>`, `git show <base>:<path>`, or a build in a separate worktree.

## Scoping answers

- Add `git stash` to the built-in worker deny list. An operator can still lift it through the existing worker-deny grant flow.
- Check how the deny rule matches a compound command before relying on it. Most of the observed calls were not the first command on the line: they followed `;`, `&&` or a newline. If a prefix rule only sees the first command, say so in the ticket's plan and decide whether the existing matcher covers it.
- Decide whether `git stash list` and `git stash show` stay allowed. They change nothing.
- Read `docs/knowledge/validation/child-containment.md` before changing the deny list.
- Telling the worker the read-only alternative is prompt text. `prompts.rs` is a frozen surface (docs/knowledge/decisions/positioning-governance-evidence-layer.md). Flag as a D-X operator decision whether the denial message may name the alternative, or whether the deny rule alone is the change.
- Do not add a stash recovery mechanism. This ticket prevents the call.

## Acceptance hints

- A unit test shows the built-in worker deny set matches `git stash push -u` and `git stash`.
- A test covers a stash call that is not the first command on the line, and records whichever behaviour the matcher has.
- Lifting the rule through a worker-deny grant removes exactly that rule.
- Anti-vacuity: the test filter matches only the new tests.
