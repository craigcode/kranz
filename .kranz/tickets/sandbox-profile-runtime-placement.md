---
state: done
state-note: Fixed on codex/stabilization-proof-sprint after live M8 mission m-bb3632 left six untracked profiles in the proof repository.
title: Keep generated Seatbelt profiles in ignored mission runtime storage
priority: 1
schedule: once
traced-from-mission: m-bb3632
---

## Goal

Write per-session `kranz-sandbox-*.sb` files under the mission's ignored
`runs/` runtime directory instead of the tracked mission root, so a successful
sandboxed run does not dirty a freshly initialized repository.

## Context

The M8 TypeScript proof reached a gated merge successfully but `git status`
then reported six untracked `.kranz/missions/<id>/kranz-sandbox-*.sb` files.
`kranz init` correctly ignores `.kranz/missions/*/runs/`; the backend was
simply choosing the wrong profile directory.

## Acceptance hints

- `ClaudeBackend::start` writes its generated Seatbelt profile under
  `<mission>/runs/` and retains the private scratch fallback.
- The live macOS backend-start confinement test asserts no profile lands at
  the mission root and at least one profile lands under `runs/`.
- Outside writes remain denied and scratch-HOME writes remain allowed.
