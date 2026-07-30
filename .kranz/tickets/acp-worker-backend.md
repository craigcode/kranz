---
title: ACP worker backend — spawn, supervise, and record an external agent
priority: 1
schedule: once
---

## Goal
An AgentBackend implementation speaking ACP (Agent Client Protocol): spawn
and supervise an external agent process, streaming its lifecycle, tool
calls, and results into the event log as first-class events.

## Context
From docs/scoping/governance-evidence-layer.md (KRZ-301) — promoted to
critical path by the positioning ADR: if kranz dispatches rather than
executes, this seam is the product. No prior KRZ-109 artifact exists
in-repo; nearest prior art is docs/scoping/cursor-cli-backend.md (the
direct-parser-vs-ACP route decision and its evidence bar). backend.rs is a
CONTRACT FILE — implement behind the trait, do not modify the seam. The
standing backend proof bar applies (roadmap): report parsing, model/cost
capture, worktree cwd discipline, permission mapping, and the
no-push/no-main-write invariants, all proven before the backend is a
default option.

## Acceptance hints
- A mock ACP peer fixture drives a full session (spawn → tool calls →
  result → exit) with ordered events appended; tool calls appear as events,
  not just transcript text.
- Kill mid-session leaves a resumable log (contiguous seq, torn-line repair
  per event_log.rs semantics).
- Cost/model fields captured when the peer reports them; absent data is
  recorded as absent, never fabricated.
- Permission mapping: a disallowed tool in the SessionSpec is refused at the
  seam, with the refusal visible as an event.
- Anti-vacuity grep on a named filter unique to this work.
