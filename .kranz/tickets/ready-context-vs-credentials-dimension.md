---
state: done
state-note: d7c8330: 'context over credentials' dimension in ready.rs — vault present / scan gate committed / no tracked .env (index-read) / worker docs; evidence names failing signals + offending env files; fixtures for all three acceptance cases; dogfood on this repo 10/10. Contributes to score + --json; AMM ladder unchanged (MAPPING_VERSION still 1).
title: kranz ready dimension — context-rich without credential-rich
priority: 3
schedule: once
---

## Goal
Add a readiness dimension to `kranz ready` that scores whether agents can be
context-rich WITHOUT being credential-rich: context lives in git artifacts
(committed knowledge vault, docs, lessons, plan/research artifacts) rather
than in live secrets. Signals: presence of `docs/knowledge/` (or equivalent
vault), secret-scan gate present and passing (`.kranz/merge-gates.json` +
secret-allowlist hygiene), no tracked `.env`-shaped files, worker-readable
docs for setup/build/test (existing dimension reuses where they overlap).

## Context
At AI Tinkerers SF (2026-07), Zachary Goldman showed pre-seeded sandboxes
keeping internal CRM context in GitHub without handing over account
passwords — the same principle kranz already runs on (knowledge vault +
worker env hygiene + scratch-HOME), phrased better: agents should get
context from git, not credentials. The repo's own history shows why the
dimension matters (burned serve.token in history until the 2026-07-20
scrub). A repo that scores well here is materially safer to point an agent
at than one whose context lives in `.env`.

## Acceptance hints
- `kranz ready` reports the new dimension with per-signal pass/fail and
  remediation hints (e.g. "no knowledge vault found", "tracked .env file:
  .env.production").
- The dimension contributes to the overall readiness score and appears in
  `--json` output.
- cargo test --workspace green incl. fixture repos: vault-present clean,
  .env-tracked flagged, missing vault flagged.
