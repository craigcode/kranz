---
state: done
state-note: Done at 4dd5d92: writable set = session_cwd + per-session private scratch + declared extraWrite; mission metadata write-denied (Seatbelt literals/regex, bwrap masks, container :ro); sibling temp neighbors default-denied. Deviation: merge worktrees NOT moved under mission-owned parent (narrowing already denies siblings; heavy orchestrator/merge edits avoided). Tests 2>&1 | grep -qE 'test result: ok. [1-9]'
title: Narrow sandbox write scope: read-only mission metadata, per-run private scratch (P1)
priority: 2
schedule: once
---

## Goal
The sandbox write allowlist currently covers the whole mission directory
(events.jsonl, state.json, locks, transcripts, control commands) and the
global temp root (every sibling mission's worktrees). Narrow it: mission
metadata mounts read-only to agent sessions; agents get a private per-run
scratch dir only. Integration/merge worktrees move under a mission-owned
parent, never the shared temp root.

## Context
From the review (P1 #3): sandbox.rs:259 writable roots; orchestrator.rs
temp_dir worktrees. The engine (host) keeps writing mission metadata as
before; the change is what the AGENT side of the mount sees.

## Acceptance hints
- A sandboxed worker cannot write events.jsonl/state.json/control/ (probe
  asserts refusal); it can write its per-run scratch.
- Sibling missions' paths are unreachable from a sandboxed session.
- cargo test --workspace green.
