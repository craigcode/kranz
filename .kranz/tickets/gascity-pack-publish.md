---
title: Stage 4 — lint-clean published Gas City pack
priority: 2
schedule: once
state: open
state-note: CI lint + stub-safe pack tests landed on tickets/gascity-citizenship-remainder (pinned gc 1.3.2, scripts/ci-gascity-pack.sh). Publish metadata drafted in packaging/gascity/README.md Distribution. Human gc pack release still blocked on Stage 1 receipt + a second consumer.
---

## Goal

Land Stage 4 of `docs/gascity-citizenship.md`: keep `packaging/gascity/`
lint-clean in CI, write the publish-facing metadata, and leave the actual
`gc pack registry` / `gc pack release` step as a human action once a second
operator wants to fetch the pack.

## Context

The citizenship plan's Stage 4 has three work items and a hard trigger:

1. **CI-wired `gc lint`** — autonomous, already passing locally
   (`gc lint packaging/gascity` → `ok` on gc 1.3.2). Local merge-gates run
   it only via `order-trigger-event.sh` when `packaging/gascity` changes;
   GitHub CI never installed `gc` and never ran the pack tests. This is
   the slice a mission may land without a city.
2. **Publish-facing metadata** — registry description and versioning
   policy for `pack.toml`'s schema field. Documentation-shaped; no
   runtime change.
3. **Human publish** — `gc pack registry` / `gc pack release` so another
   city can `gc pack fetch`. Networked and state-mutating. The plan
   forbids autonomous publish. Trigger: a second operator or city actually
   wants the pack. Publishing with no consumer only exports the still-open
   Stage 1 gap (live supervisor+worker has never been proven).

Also named, not required to start: once a live supervisor exists, evaluate
collapsing `kranz-dispatch` + `kranz-city-worker` into a `gc hook`-driven
claim (`work_query`). That evaluation is blocked on Stage 1, same as
publish.

Do not run `gc init`, `gc start`, `gc pack release`, or any other
state-mutating `gc` command from a mission or CI.

## Scoping answers

- Second consumer who asked to fetch the pack (required before publish):
- Stage 1 disposable-city receipt (required before publish):

## Acceptance hints

- GitHub CI installs a *pinned* `gc` (checksummed release tarball, same
  posture as the Gitleaks pin in `release.yml`) and runs
  `gc lint packaging/gascity` on every change that can affect the pack
  (or on every CI run). The job fails if lint is not `ok`.
- Stub-safe pack tests that do not need a live city or live `bd`
  (`order-trigger-event.sh`, `kranz-run-bead-events.sh`,
  `check-bridge-hygiene.sh` and its selftest, header selftests) run in
  that job. Do not run `kranz-dispatch-roundtrip.sh` or its selftests in
  CI (they need a live `bd` / writable git hooks).
- Publish-facing metadata exists (pack README or citizenship plan): what
  the registry listing should say, and how `pack.toml` `schema` is
  versioned.
- `gc pack registry` / `gc pack release` is *not* performed by this
  ticket's mission. A human runs it after the trigger and Stage 1
  receipt are real; record the command and version in this ticket's
  `state-note` when that happens.
- No queue-swap and no fleet work in this ticket.
