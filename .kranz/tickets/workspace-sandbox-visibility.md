---
title: Workspace and sandbox visibility in mission records and dashboard
priority: 2
schedule: once
---

## Goal
Make the effective execution workspace visible everywhere an operator reviews
or steers a mission: provider, worktree path or remote workspace id, template
or image version, readiness checks, setup command status, preview URLs, and
human takeover instructions. Record these as mission artifacts/events so the
workspace can be audited after the run.

## Context
Mission Control's scope switcher for local, worktree, and remote VM sandboxes
is a good operator pattern. Monaco's post already pushed kranz toward the same
lesson: a worktree is not a full runnable workspace. M6 needs a clear
workspace seam, and the dashboard must show the actual runtime environment
without making the operator infer it from branch names or logs.

This should complement, not replace, the existing event-sourced mission model.
The workspace provider provisions source, services, data, and previews; kranz
still owns plan approval, validation, merge gates, and audit.

## Acceptance hints
- Mission approval pins the effective workspace provider and provider version
  or template/image identity.
- Dashboard and report.md show workspace readiness, setup results, preview
  links, and takeover details when present.
- Workspace failures are surfaced as preflight or blocked states with a clear
  owner: operator, provider, or repo setup.
- Remote workspace URLs or preview links reveal nothing sensitive without
  their own access control; secrets are never recorded in the event log.
- Local worktree-only missions still render a useful workspace summary.
