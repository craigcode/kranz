---
state: done
state-note: landed in this polish commit; reviewed + gates green
title: Unvalidated mission_id from Slack reaches MissionPaths (path traversal)
priority: 3
schedule: once
---

## Goal
build_status_reply (crates/slack/src/bridge.rs:~2276, reached from /kranz status <id>) passes mission_id straight to MissionPaths::new(repo_root, mission_id) -> missions_dir().join(mission_id) with NO validation (ticket slugs use valid_slug; mission ids do not). /kranz status is read-only and not allowlist-gated, so /kranz status ../../../<other>/.kranz/missions/<id> can disclose another repo's mission metadata on a shared host. The server side already guards via safe_id — the Slack path is the gap. NOTE: this is in the /kranz status code merged 2026-07-07. Fix: add a mission-id validator (mirror valid_slug) before constructing MissionPaths, or enforce inside MissionPaths::new.

## Context
Found by a security code review 2026-07-07 (HEAD 0740072). Read-only metadata disclosure on a shared host; in tonight's /kranz status code.

## Acceptance hints
- A traversal-shaped mission id is rejected before MissionPaths; valid ids work. Test the validator.
