# Mission report — m-c4f925

**Goal:** Compute blocked-ness in one engine-side predicate, serve it as isBlocked on both ticket REST endpoints, and make every render surface (backlog chip + ticket-detail approve gate) derive from it so a done ticket whose blocker is done never shows an active blocked chip.

Branch `kranz/mission-m-c4f925` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 52m 41s
**Tokens:** 38487 in / 88422 out / 7525141 cache read / 472521 cache write
**Cost:** $23.37 actual vs $6.10–$30.50 estimated (expected $12.20)

## What shipped

### Milestone 1 — Server computes and serves a single blocked-ness predicate ✅

- ✅ **deps::is_blocked wrapper + isBlocked on both ticket REST projections** — 1 run
  - `8363a7e` [f-1-1] add deps::is_blocked wrapper + serve isBlocked on both ticket REST projections
  - `a5d6187` [f-1-1] checkpoint (engine commit)

### Milestone 2 — Every render surface derives the chip and approve gate from isBlocked ✅

- ✅ **Backlog chip + ticket-detail approve gate consume server isBlocked** — 1 run
  - `cbd61e1` [f-2-1] derive backlog chip + ticket-detail approve gate from server isBlocked

## Validation history

### ms-1 round 1 — Server computes and serves a single blocked-ness predicate

No findings.

### ms-2 round 1 — Every render surface derives the chip and approve gate from isBlocked

No findings.

### Final gate

- [critical] a6 *(final gate)* — command failed: git rev-parse -q --verify "$KRANZ_BASE_SHA^{commit}" >/dev/null && ! git diff --name-only "$KRANZ_BASE_SHA" | grep -qE 'crates/engine/src/(reducer|event_log)\.rs'

Disposition: waived.
- a6: Environmental false-positive: the mission diff (main...HEAD) touches neither crates/engine/src/reducer.rs nor crates/engine/src/event_log.rs (verified directly), so a6's substance passes; the command failed only because $KRANZ_BASE_SHA was unset in the validator env, short-circuiting the rev-parse guard (the known m-d341a7 gap tracked by ticket fix-base-sha-final-gate-env). No mission-code fix exists to make.

## Contract outcomes

- ✅ **[a1]** Engine unit tests prove deps::is_blocked: blocker's mission Complete -> false; blocker mission absent or non-Complete -> true; empty blocked_by -> false. *(command: `cargo test -p kranz-engine is_blocked 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a2]** GET /api/tickets returns isBlocked:false with blockedBy intact for a done ticket whose blocker's mission is Complete (tickets_rest integration test). *(command: `cargo test -p kranz-server --test tickets_rest is_blocked 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a3]** The dashboard vitest suite (including new BacklogPanel chip-logic and TicketDetail approve-gate cases) passes. *(command: `cd apps/dashboard && npm test`)*
- ✅ **[a4]** The dashboard typechecks and builds. *(command: `cd apps/dashboard && npx tsc --noEmit && npm run build`)*
- ✅ **[a5]** The Rust workspace is clippy-clean under -D warnings and fmt-clean. *(command: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all --check`)*
- ✅ **[a6]** No event-log schema or reducer change: the mission diff touches neither crates/engine/src/reducer.rs nor crates/engine/src/event_log.rs. *(command: `git rev-parse -q --verify "$KRANZ_BASE_SHA^{commit}" >/dev/null && ! git diff --name-only "$KRANZ_BASE_SHA" | grep -qE 'crates/engine/src/(reducer|event_log)\.rs'`)*
- ✅ **[a7]** One engine predicate is the sole source of blocked-ness: it is documented as such, every render surface (backlog chip, ticket-detail approve gate) and the isBlocked REST field derive from it with no client re-deriving blockedness locally, non-empty-but-satisfied edges render as muted 'was blocked by …' provenance rather than an active chip, and a done ticket never renders an active chip even against a payload that claims isBlocked:true. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
