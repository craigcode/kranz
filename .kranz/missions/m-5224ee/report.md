# Mission report — m-5224ee

**Goal:** Ticket surfaces (REST /api/tickets projection, CLI ticket list/show, Slack, and the dashboard BacklogPanel) distinguish Delivered (mission complete-but-unmerged) from Landed (mission branch merged into base) by reusing the existing merged-ancestor probe, instead of collapsing both into `done`.

Branch `kranz/mission-m-5224ee` (from `main`). Plan of record: [plan.md](plan.md).

**Elapsed:** 48m 41s
**Tokens:** 39069 in / 160049 out / 19960562 cache read / 762342 cache write
**Cost:** $66.35 actual vs $9.18–$45.88 estimated (expected $18.35)

## What shipped

### Milestone 1 — Ticket projection carries the live merged bit ✅

- ✅ **Shared engine merge-ancestry derivation for tickets** — 1 run
  - `dc2ac11` [f-1-1] lift merged_bit into kranz-engine and add ticket_merged derivation
- ✅ **Surface the merged bit on /api/tickets and de-duplicate the probe** — 1 run
  - `7d14f27` [f-1-2] surface merged bit on ticket projections

### Milestone 2 — Every ticket surface renders Delivered vs Landed ✅

- ✅ **CLI ticket list/show render Delivered vs Landed** — 1 run
  - `2f8ff8e` [f-2-1] CLI ticket list/show render Delivered vs Landed
- ✅ **Slack ticket list/show render Delivered vs Landed** — 1 run
  - `2bac5eb` [f-2-2] Slack ticket list/show render Delivered vs Landed
- ✅ **Dashboard BacklogPanel renders Delivered vs Landed** — 2 runs, 1 respawn
  - `ef30f3e` [f-2-3] fix null->landed bug and add Delivered/Landed tests to BacklogPanel

## Validation history

### ms-1 round 1 — Ticket projection carries the live merged bit

- [critical] a2 - cargo test --workspace cli_ticket_delivered_landed — cargo test --workspace cli_ticket_delivered_landed matches zero tests across every crate (all `test result: ok. 0 passed` lines; the required assertion pattern 'result: ok. [1-9][0-9]* passed' never a… [truncated]
- [critical] a3 - cargo test --workspace slack_ticket_delivered_landed — cargo test --workspace slack_ticket_delivered_landed matches zero tests across every crate (all `test result: ok. 0 passed` lines). No test named slack_ticket_delivered_landed exists, and the Slack ti… [truncated]
- [critical] a4 - npm --prefix apps/dashboard test -- BacklogPanel — `npm --prefix apps/dashboard test -- BacklogPanel` fails to start: `sh: vitest: command not found`. apps/dashboard/node_modules does not exist (not installed in this checkout), so the BacklogPanel tes… [truncated]
- [critical] a6 - npm --prefix apps/dashboard test — `npm --prefix apps/dashboard test` fails to start with the same error: `sh: vitest: command not found` (missing node_modules). No dashboard tests ran, so no regression coverage was exercised for this … [truncated]

Disposition: waived.
- a2 - cargo test --workspace cli_ticket_delivered_landed: Out of ms-1 scope: CLI Delivered/Landed rendering is feature f-2-1 (ms-2, still pending). Not a defect in ms-1, whose bar is a1 (engine+REST projection), which passed.
- a3 - cargo test --workspace slack_ticket_delivered_landed: Out of ms-1 scope: Slack Delivered/Landed rendering is feature f-2-2 (ms-2, still pending). Will be implemented and validated there.
- a4 - npm --prefix apps/dashboard test -- BacklogPanel: Out of ms-1 scope: dashboard BacklogPanel rendering is feature f-2-3 (ms-2, still pending). It also can't run here because the validator checkout has no apps/dashboard/node_modules (vitest not installed) — an environment gap, not ms-1 code.
- a6 - npm --prefix apps/dashboard test: No dashboard code changed in ms-1, so there is nothing to regress; and it can't run due to missing apps/dashboard/node_modules (vitest not found). Environment issue, not an ms-1 defect.

### ms-2 round 1 — Every ticket surface renders Delivered vs Landed

No findings.

## Contract outcomes

- ✅ **[a1]** The ticket projection reports a completed-but-UNMERGED mission's ticket as Delivered (merged=false) and a completed-and-MERGED mission's ticket as Landed (merged=true), at both the engine helper and the /api/tickets (list and show) REST layer. *(command: `cargo test --workspace ticket_projection_merged 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a2]** CLI `kranz ticket list`/`show` render DELIVERED for a Done ticket whose mission is unmerged and LANDED when merged (or when there is no linked mission), instead of a single DONE label. *(command: `cargo test --workspace cli_ticket_delivered_landed 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a3]** The Slack ticket surfaces (`/kranz ticket list` and `show`) render Delivered vs Landed for a completed ticket based on merge status, instead of the collapsed `Done`. *(command: `cargo test --workspace slack_ticket_delivered_landed 2>&1 | grep -qE 'result: ok\. [1-9][0-9]* passed'`)*
- ✅ **[a4]** The dashboard BacklogPanel renders a done-but-unmerged ticket with a delivered/UNMERGED indicator distinct from a landed ticket, rather than a bare `done` pill. *(command: `npm --prefix apps/dashboard test -- BacklogPanel 2>&1 | grep -qE '[1-9][0-9]* passed'`)*
- ✅ **[a5]** The full Rust workspace still builds and all tests pass (no regression to mission-row merged detection or existing ticket behavior). *(command: `cargo test --workspace`)*
- ✅ **[a6]** The dashboard test suite still passes as a whole (no regression to PipelineView or other components). *(command: `npm --prefix apps/dashboard test`)*
- ✅ **[a7]** The Delivered/Landed derivation reuses the existing git_ops::is_ancestor / merged_bit probe and introduces no new git-subprocess or ancestry logic; the probe exists as a single shared engine function used by both mission rows and tickets. *(agent judgement)*

All assertions passed at the final contract gate (waivers, if any, appear in the validation history).
