---
state: done
state-note: Fixed on codex/stabilization-proof-sprint after hostile proof mission m-ed91b6 showed every Claude Bash and Git command failing before the intended containment probes.
title: Keep /dev/null writable in every macOS Seatbelt session
priority: 1
schedule: once
traced-from-mission: m-ed91b6
---

## Goal

Permit the narrowly scoped `/dev/null` sink in every generated macOS
Seatbelt profile so ordinary shell redirects, Git commands, and agent tool
runners work under `fs` and `fs+net` enforcement.

## Context

The session profile previously added the device allowance only when validator
read-deny roots were present. Hostile M7 mission `m-ed91b6` used a normal
worker session, so Claude's Bash tool repeatedly failed with `operation not
permitted: /dev/null` before it could execute either the hostile probes or the
legitimate repository work. Gate profiles already carried the same narrow
allowance for this reason.

## Acceptance hints

- The base profile contains exactly the existing literal `/dev/null`
  `file-write*` allowance without widening access to `/dev`.
- The live macOS sandbox test proves a shell redirect to `/dev/null` succeeds,
  an in-session write succeeds, and an outside write remains denied.
- The unchanged hostile fs+net proof mission is rerun with the rebuilt CLI.
