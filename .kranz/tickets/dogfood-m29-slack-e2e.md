---
title: Dogfood M2.9 Slack E2E — check the live-validation gate
priority: 2
schedule: once
---

## Goal
Complete this ticket entirely via Slack (draft → approve/queue → work → merge) to prove the M2.9 full control surface. Deliverable: check off `M2.9 live Slack validation` in docs/operator-gates.md, and add a short dated note (5–10 lines) under docs/knowledge/surfaces/slack-commands.md documenting the path used (serve --slack, /kranz draft, Approve & queue, work run, merge) and that deep links used dashboardUrl. Do not change Slack bridge code unless a blocker is discovered mid-run — file a follow-up ticket instead.

## Context
Roadmap M2.9 is shipped in slices but still marked ◑ pending live validation
(`docs/operator-gates.md`). This ticket is the validation mission itself —
intentionally docs-only so the Slack loop is the risk under test, not product
code. `slack.dashboardUrl` is set to `http://127.0.0.1:4560/` for deep links.

## Scoping answers

## Acceptance hints

- Mission completed via Slack surfaces only (approve/queue/work/merge); CLI may only be used for `kranz serve --slack`.
- `docs/operator-gates.md` has `[x] M2.9 live Slack validation`.
- `docs/knowledge/surfaces/slack-commands.md` has a short dated dogfood note mentioning dashboardUrl deep links.
- `cargo test --workspace` and fmt/clippy are green if any code changed; docs-only is fine with no Rust changes.
