---
title: Set Windows sandbox ACLs without re-propagating to every descendant
priority: 1
schedule: once
---

## Goal

Applying or removing a sandbox access entry on a directory changes that directory only. It does not make Windows rewrite the security descriptor of everything beneath it.

## Context

Reported from a Windows 11 host running release 0.5.0 on 2026-11-15. The user profile held about 914,000 files in 176,000 folders.

- Every sandboxed validator session logged `slow AppContainer ACL grant` for the profile directory with `inherit=false`: between 58 and 130 seconds, ten sessions out of ten. A second directory took 5 to 47 seconds each time.
- `kranz sandbox-prepare --target C:\` ran for a long time with steady CPU on a drive with 317 GB in use.

Both paths are in `crates/engine/src/appcontainer_windows.rs`. Per-session changes go through `apply_acl_change_security`, which calls `SetSecurityInfo`. Host preparation calls `SetNamedSecurityInfoW`. Both are the high-level calls that re-apply inheritable entries to existing children whenever a DACL is set, including when the entry being added does not inherit: the directory's other entries do, so the whole tree is walked.

Two statements in the file are wrong and should be corrected with the fix: the doc comment on `prepare_appcontainer_host` says the sequence avoids the descendant walk, and the slow-grant warning says an inheritable entry is the cause.

## Scoping answers

- Use a call that sets the descriptor on the one object and does not propagate. The file already uses `SetKernelObjectSecurity` for the null device; `SetFileSecurityW` is the path-based equivalent.
- The low-level calls do not maintain inheritance for you. Preserve the existing entries exactly, including their inherited flags, and preserve the descriptor's protected and auto-inherited control state. A grant must not turn inherited entries into explicit ones or change who else has access.
- The restore path at session end must return the DACL to its recorded bytes by the same non-propagating route.
- Changes that are meant to inherit (`inherit=true`) need a decision per call site: either the tree is one kranz created and is small, or the entry must be applied in a way that does not walk a large existing tree. State which, per site.
- This is containment code. Read `docs/knowledge/validation/child-containment.md` and `docs/scoping/m7-windows-containment.md` first. The hostile-child proof must still pass unchanged, and nothing may be widened to make it pass.
- The Windows CI runner has a near-empty disk and cannot show the slowness. The ticket is done when the tests below pass. The field timing is confirmed separately on the reporting host.

## Acceptance hints

- A Windows test applies a non-inheriting entry to a directory that has an inheritable entry and children, and proves the children were not rewritten.
- A Windows test proves the directory's own DACL after apply-then-restore equals its bytes before.
- The existing Windows hostile-child fixture passes.
- `kranz sandbox-prepare` reports the same applied state as before on a prepared and an unprepared root.
- The corrected doc comment and warning text name the real cause.
