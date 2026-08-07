---
state: done
state-note: landed in 8da93ce (Craig); reviewed + tested + gates green
title: Slack outbound cursor advances before posting -> dropped notifications
priority: 2
schedule: once
---

## Goal
poll_mission (crates/slack/src/bridge.rs:~210) sets cursor.last_seq = event.seq BEFORE post_outbound().await, and a post error is only warn-logged. read_events_after(last_seq) next tick then skips that seq. Slack rate-limits chat.postMessage (~1/s/channel), so a burst (plan.approved + milestone.blocked) yields a 429 and that notification is lost permanently. Fix: advance last_seq only after a successful (or permanently-failed) post; on transient/429 leave it unchanged so the next tick retries.

## Context
Found by a security code review 2026-07-07 (HEAD 0740072). Notification reliability.

## Acceptance hints
- A transient post failure does not advance the cursor; the event reposts next tick. Test the ordering with a failing-then-succeeding post.
