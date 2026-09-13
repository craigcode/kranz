---
state: done
state-note: 418d257: checkpoint partitions dirty paths — tracked files scan added lines only (git diff HEAD + scan_unified_diff), new untracked files still full-file; scoping note records the trade (strictness unchanged, target changed). Regression tests: base-region ignored / added-line refused / untracked refused.
title: Scope the checkpoint secret scan to the dirty diff, not full dirty files
priority: 3
schedule: once
---

## Goal
Change the dirty-tree checkpoint's secret scan (`git_ops::secret_scan_refusal`,
currently `scrub::scan_paths` over the full contents of dirty paths) to scan
only the mission's added lines (the `git diff HEAD` of the dirty paths, via
`scrub::scan_unified_diff`), aligning it with the merge pre-gate and
`kranz scan`. A mission must not be refused for pre-existing base content in
a file it merely touches.

## Context
2026-07-19: mission m-0f1abd was checkpoint-refused twice by findings in
UNCHANGED base code — an Option-wrap assignment to a `token` local
(orchestrator.rs), a credential-flavored literal in a runner.rs test
fixture, a "lan-test-…" constant and an Option<&str> param of the same name
(server_test.rs) — while the mission's own
diff was clean (`kranz scan --range HEAD` passed). Full-file scanning means
any mission touching a file with a token-shaped pattern can never checkpoint
until base content is waived or renamed — recurring friction with no security
benefit, since base content is already covered by CI range scans and the
pre-public history scrub, and anything the mission ADDS is caught by the
diff scan. Design question, not a drive-by: the write-side gate strength
(secret-scanning D-A) is intentionally strict, so make the trade explicit —
document why the commit gate now matches the merge gate's view (both judge
the mission's changes, not the base's history), keep the merge-time
full-diff scan unchanged, and keep full-file scanning for NEW files (their
whole content is added lines anyway).

## Acceptance hints
- Checkpoint refuses only on findings in added lines of dirty paths;
  a mission touching a file with allowlisted-or-not base patterns passes if
  its own diff is clean.
- A regression test: dirty file contains a base-shaped secret pattern in an
  unchanged region AND a real secret in an added line → refused for the
  added line only.
- docs/scoping/secret-scanning.md gains a note recording the decision and
  its rationale.
- cargo test --workspace, clippy, fmt green.
