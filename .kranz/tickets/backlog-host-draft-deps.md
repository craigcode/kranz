---
title: Host-side ticket drafting, REST backlog surface, and blocked-by dependencies
priority: 2
schedule: once
---

## Goal
Hoist the non-interactive draft loop out of the CLI into the engine/MissionHost so any surface can draft a ticket; expose the backlog over REST (list, show, draft, approve); and add the blocked-by dependency primitive (frontmatter list of slugs, approve-gate, work-time recheck, cycle detection).

## Context
Three pieces, one crate-boundary move. (1) DRAFT HOIST: the non-interactive
draft loop (seed orchestrator with the whole ticket -> request plan -> park
committed plan.md for review -> or append the orchestrator's questions to the
ticket and flag NEEDS-CONTEXT) lives in crates/cli/src/backlog.rs today; move
its core into kranz-engine (or a server-callable form) and give
kranz_server::MissionHost a draft entry point. The CLI command becomes a thin
caller — behavior byte-compatible (same ticket state transitions, same
NEEDS-CONTEXT append format, spend bounded by the orchestrator budget cap).
(2) REST: GET /api/tickets (list: slug, priority, state, title, blocked-by),
GET /api/tickets/:slug (full parsed ticket + needs-context), POST
/api/tickets/:slug/draft (long-running; progress observable over the existing
SSE feed), POST /api/tickets/:slug/approve — mutation-token gated like every
other POST. Slugs pass Ticket::ensure_valid_slug at the route boundary.
(3) BLOCKED-BY: frontmatter key `blocked-by: [slug, ...]` (parser already
ignores unknown keys, so existing tickets are untouched). Semantics: a blocker
is satisfied ONLY when its mission reached Complete — approved/queued/failed
blockers all block. Enforcement at exactly two choke points: `kranz ticket
approve` (and the REST approve) refuses with an honest message naming the
unsatisfied blocker, `--force` (REST: {"force":true}) overrides; the `kranz
work` dispatcher re-checks blockers when it resolves a claimed entry's ticket
and skips-with-warning if a blocker failed mid-drain — do NOT add ordering
logic inside crates/engine/src/queue.rs's claim machinery (recently hardened;
insertion order remains the scheduler). Cycle detection (DFS over the ticket
files) at approve time. OUT OF SCOPE, deliberately: serve auto-draining the
queue (that is design decision D5 in docs/gascity-citizenship.md — the
dispatcher stays `kranz work`); DAG visualization; cross-repo dependencies;
auto-topological drafting. Update docs/tickets.md (usage) and
docs/protocol.md (new endpoints).

## Scoping answers

## Acceptance hints
- kranz draft <slug> behaves exactly as before (existing backlog tests green) with the core living in the engine/server layer; a new host-level test drafts a ticket end-to-end against the mock backend.
- GET /api/tickets lists the real backlog with states and blocked-by; POST draft/approve work token-gated; a bad slug 400s (traversal test).
- A ticket with an unsatisfied blocked-by refuses approve (CLI and REST) naming the blocker; --force overrides; approve succeeds once the blocker's mission is Complete; a cycle is rejected with the cycle path in the message.
- kranz work skips (with a recorded warning) a queued entry whose blocker's mission Failed after batch-approval.
- cargo test --workspace passes, piped through grep -qE 'result: ok\. [1-9][0-9]* passed'; cargo clippy --workspace --all-targets clean; cargo fmt --all --check clean.
