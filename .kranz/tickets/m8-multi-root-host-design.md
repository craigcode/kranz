---
title: M8 multi-root host design — config, tokens, queues, Slack routing
priority: 2
schedule: once
---

## Goal
Write and land the design (scoping doc + accepted D-X decisions) for one
`kranz serve` / one Slack bridge fronting many repo roots: config shape,
per-repo capability/token model, queue identity, mission id uniqueness,
and Slack team/channel → repo routing. No dashboard picker UI in this
ticket.

## Context
`multi-repo-project-picker` cannot honestly claim "selecting a repo scopes
mutation token behavior" under today's single `repo_root` + single
`.kranz/serve.token` MissionHost. Build the host model first.

## Decisions this ticket must close
- Config: list of repo roots (global and/or project) with display name,
  optional group/pin metadata ownership (operator config vs repo).
- Auth: one serve token vs per-repo tokens vs path-scoped capabilities;
  how dashboard/Slack attach the right scope.
- Queues: one queue dir per repo (required) and how drain picks a repo.
- Identity: mission ids stay per-repo; API paths must disambiguate repo.
- Slack: how `/kranz` commands bind to a repo (channel map, flag, recent).
- Failure mode: missing/moved repo → visible error, not cross-repo bleed.

## Acceptance hints
- New `docs/scoping/m8-multi-root-host.md` (or equivalent) with flagged D-X
  decisions accepted or explicitly deferred.
- No production UI required; fixture/unit tests may cover config parse +
  routing table pure functions if introduced.
- Explicit non-goal: implementing the project picker (separate ticket).
