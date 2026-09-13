# Mission report — m-bc11fb

**Goal:** Add a dashboard backlog panel (ticket list, detail, Draft-with-live-progress, blocked-by-aware Approve) and Slack `/kranz ticket list|show`, `/kranz draft <slug>` (spend-gated), and approve-by-slug — all riding the REST/host surface landed by backlog-host-draft-deps.

Branch `kranz/mission-m-bc11fb` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 1h 29m 09s
**Tokens:** 42729 in / 201086 out / 25542166 cache read / 966070 cache write
**Cost:** $87.94 actual vs $10.20–$51.00 estimated (expected $20.40)

## What shipped

### Milestone 1 — Dashboard backlog panel ✅

- ✅ **Ticket data layer: types, api wrappers, and store actions** — 1 run
  - `ed3e366` [f-1-1] add ticket data layer: types, api wrappers, store actions
  - `38737c3` [f-1-1] checkpoint (engine commit)
- ✅ **Backlog UI: routes, panel, detail, Draft-live, blocked-aware Approve** — 1 run
  - `5ab669d` [f-1-2] add dashboard backlog panel: list, detail, draft-live, blocked-aware approve

### Milestone 2 — Slack backlog verbs ✅

- ✅ **Read-only Slack ticket verbs: /kranz ticket list and show** — 1 run
  - `154cf2c` [f-2-1] add read-only Slack ticket verbs: /kranz ticket list and show
- ✅ **Spend-gated Slack /kranz draft <slug>** — 2 runs, 1 respawn
  - `4836b4b` [f-2-2] finish /kranz draft <slug>: define run_draft_command, wire slow-action + apply_action, add FakeHost tests
- ✅ **Slack approve-by-slug** — 1 run
  - `5a86d90` [f-2-3] extend /kranz approve to accept a ticket slug (approve-by-slug)
- ✅ **Docs: tickets.md Slack verbs and slack-management slice ledger** — 1 run
  - `49e0951` [f-2-4] document Slack backlog verbs and add slice ledger entry
- ✅ **Fix /kranz draft ack ordering: post hourglass before the slow draft runs** *(fix)* — 1 run
  - `f498eba` [ms-2-fix-1-1] post hourglass ack before the slow draft turn runs

## Validation history

### ms-1 round 1 — Dashboard backlog panel

- [critical] findings — placeholder

Disposition: waived.
- findings: Non-actionable placeholder: evidence is literally 'placeholder', suggestedFix empty, no defect/file/behavior named. Both ms-1 features were verified against their diffs with concrete green test evidence (25/25 vitest, tsc/build/lint clean); no real contract violation is described, so a fresh worker session has nothing to act on.

### ms-2 round 1 — Slack backlog verbs

- [major] f-2-2 / j5 — authorized draft must post an immediate hourglass ack while the draft runs off the socket loop — In crates/slack/src/bridge.rs, run_draft_command builds `ack` then awaits `host.draft(slug).await` (the multi-minute create+seed+plan turn) BEFORE returning, packing both `ack` and `result` into one D… [truncated]

Disposition: 1 fix feature(s) created.

### ms-2 round 2 — Slack backlog verbs

No findings.

## Contract outcomes

- ✅ **[a1]** The dashboard typechecks cleanly: `npx tsc --noEmit` succeeds in apps/dashboard. *(command: `cd apps/dashboard && npx tsc --noEmit`)*
- ✅ **[a2]** The dashboard production build succeeds: `npm run build` passes in apps/dashboard. *(command: `cd apps/dashboard && npm run build`)*
- ✅ **[a3]** The full Rust workspace test suite runs and reports passing tests. *(command: `cargo test --workspace 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a4]** Clippy is clean across the workspace with warnings denied. *(command: `cargo clippy --workspace --all-targets -- -D warnings`)*
- ✅ **[a5]** Rust formatting is clean. *(command: `cargo fmt --all --check`)*
- ✅ **[a6]** The Slack backlog-verb fake-host test suite (crates/slack/tests/tickets.rs) exists and passes: ticket list/show ephemeral, draft allowlist-gate + no-spawn-on-refusal, and approve-by-slug end-to-end. *(command: `cargo test -p kranz-slack --test tickets 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[j1]** A backlog panel reachable from the mission picker lists the live .kranz/tickets/ state — each row showing slug, priority, a state chip, title, and blocked-by badges — sourced from GET /api/tickets. *(agent judgement)*
- ✅ **[j2]** Drafting a ticket from the browser fires POST /api/tickets/:slug/draft and shows live progress via the returned mission's existing WS feed (the planning-chat-pane pattern), parking a reviewable plan without a terminal. *(agent judgement)*
- ✅ **[j3]** The Approve button appears only on REVIEW tickets and is disabled with the blocker named when a blocked-by dependency is unsatisfied; the server 409 refusal stays authoritative and its message is surfaced verbatim if returned. *(agent judgement)*
- ✅ **[j4]** Slack `/kranz ticket list` and `/kranz ticket show <slug>` are read-only (no allowlist gate) and reply ephemerally to the invoker. *(agent judgement)*
- ✅ **[j5]** Slack `/kranz draft <slug>` is allowlist-gated exactly like `/kranz new`: unauthorized users get the standard refusal and no engine/draft spawns; authorized invokers get an immediate hourglass ack, the draft runs off the socket loop, and NEEDS-CONTEXT questions are posted back to the invoker. *(agent judgement)*
- ✅ **[j6]** Slack `/kranz approve <slug>` resolves the slug to its drafted mission and queues a REVIEW ticket end-to-end via kranz_engine::deps::approve_ticket; blocked-by refusals surface the engine's message verbatim and queue nothing. *(agent judgement)*
- ✅ **[j7]** Slash-command platform rules are honored for the new verbs: single-line commands only, no dispatch in threads. *(agent judgement)*
- ✅ **[j8]** docs/tickets.md's listing/Slack sections and docs/slack-management.md's Build-slices ledger are updated to describe the new dashboard panel and Slack backlog verbs, accurate to what shipped. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
