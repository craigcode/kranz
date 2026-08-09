---
title: Cursor validator sessions fail auth — seeding covers only the worker spawn path
priority: 2
schedule: once
---

## Goal
Cursor validator sessions exit 1 with an auth error (observed live on m-eee81f functional validator, 2026-08-09: 'cursor exited with exit status: 1 without emitting a terminal event; stderr tail: Error: Authentic...') while cursor workers authenticate fine. The ~/.cursor seed + CURSOR_API_KEY passthrough (backend_cursor.rs) evidently reach only the worker spawn path; validator sessions (validator_snapshot + containment profile) get a HOME without the seeded cursor config. Extend the seeding to the validator spawn path (or document and enforce workers-only), so a cursor validator can authenticate in a scratch HOME. Sibling: codex-scratch-home-auth-seeding (same gap, codex side).

## Context


## Scoping answers

## Acceptance hints
