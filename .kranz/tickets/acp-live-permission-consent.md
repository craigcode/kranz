---
state: open
title: ACP live permissions — durable one-call consent across existing operator surfaces
priority: 1
schedule: once
blocked-by: [gate-evaluation-contract-v1, acp-adapter-compatibility-proof]
---

## Goal

An ACP request can wait for an authorized human and receive only the decision
for that exact invocation, with a durable record before the answer is sent.

## Context

S4 of docs/scoping/acp-worker-gate-contract.md. Current ACP answers inline;
current grant.approved extends mission policy for retries. They cannot be
connected by treating a one-time ACP choice as an existing command grant.

## Scope

- Implement the approved backend/runner permission broker contract, carrying
  run/session/RPC/tool identity, immutable subject/options, policy and deadline.
- Reuse the single-writer engine, control inbox and authenticated CLI/server/
  Slack/dashboard controls with distinct one-call scope. No question-event
  permission lane, mission-wide widening or invented human actor.
- Persist request and decision before wire response; record send/expiry/
  cancellation separately. Return only certified offered one-time option IDs.
- Deny engine prohibitions; require policy proof or human consent for other
  understood actions. Unknown effects fail closed. Never fall back to
  allow_always or the first option.
- Keep transport/control/cancel progressing while waiting, including existing
  buffered parallel-worker paths; cap pending requests and memory.

## Acceptance hints

- A fixture pauses before effect, accepts one click once, then executes once.
- Wrong-session, changed-subject, stale, duplicate, expired and interrupted
  replies cannot authorize another call; deadlines survive restart.
- Unavailable human input fails closed; cancellation cannot deadlock on I/O.
- One-call consent leaves old mission-wide grant lists unchanged.
- Policy approvals and human approvals have distinct provenance; wait, decision
  and response timestamps support honest latency and uncertain-send reporting.
- Read-only tokens and unauthorized actors cannot answer requests.
- Full workspace and dashboard gates pass; embedded bundle is synchronized.

## Out of scope

Persistent adapter permission edits, mode escalation, new glasses interaction
design, session pooling and cross-feature resume.
