---
title: Blocked-ness is computed server-side; chips render derivation, not stored edges
priority: 2
schedule: once
---

## Goal
The backlog panel renders an active red 'blocked by backlog-host-draft-deps' chip on a DONE ticket whose blocker is itself DONE: the chip is driven by the stored blocked_by edge instead of the blockers' current state. Stored edges persist (provenance); active-ness must be computed — in ONE place, server-side. Expose a computed isBlocked field on GET /api/tickets and /api/tickets/:slug derived from the SAME engine predicate the approve gate uses (kranz_engine::deps::unsatisfied_blockers — do not invent a second notion of blockedness); the panel renders the red chip only when isBlocked, renders non-empty-but-satisfied edges as muted provenance ('was blocked by …'), and a done ticket never shows an active chip regardless of payload (belt-and-braces render guard).

## Context
Observed live on the shipped backlog panel (m-bc11fb): ticket
backlog-ui-dashboard-slack is done, its blocker backlog-host-draft-deps is
done, and the panel still shows an active red blocked chip — the UI
computes from the raw blockedBy array (BacklogPanel.tsx badges, from
ticket_summary_json in crates/server/src/tickets.rs).

THE DESIGN DECISION (matters more than the fix): one blocked-ness
predicate, engine-side. Three consumers need the same answer (panel chip,
dashboard Approve button, Slack approve-by-slug); if the chip derives one
way and the gate another, a ticket can look unblocked but refuse approval
or vice versa. The predicate ALREADY EXISTS as the approve gate:
kranz_engine::deps::unsatisfied_blockers (Complete-only mission
satisfaction, missing-mission = blocked). Wrap it (e.g.
deps::is_blocked(repo, slug) -> bool) rather than writing a parallel
ticket-state-based notion, and comment it: 'Single source of blocked-ness.
Approve gates (dashboard + Slack) and every rendering surface MUST call
this, never re-derive.' Serve it as isBlocked (camelCase) in BOTH
ticket_summary_json and ticket_full_json; no client computes blockedness
locally. Panel: red chip only when isBlocked; blockedBy non-empty with
isBlocked false renders muted 'was blocked by …' provenance; done tickets
never render an active chip (render guard even against a lying payload).
No event-log schema or reducer changes — projection/render only.

## Scoping answers

## Acceptance hints
- Engine unit tests: blocker's mission Complete -> is_blocked false; blocker mission absent or non-Complete -> true; empty blocked_by -> false; run via cargo test -p kranz-engine 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'.
- REST: GET /api/tickets shows isBlocked:false with blockedBy intact for a done ticket whose blocker is done (tickets_rest integration test, passed-count guard).
- UI: vitest covers BacklogPanel chip logic — no red chip when isBlocked false; red chip when true; muted provenance when edges exist but satisfied; done ticket never active. npx tsc --noEmit and npm run build pass.
- cargo clippy --workspace --all-targets -- -D warnings and cargo fmt --all --check clean.
