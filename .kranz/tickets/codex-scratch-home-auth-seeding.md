---
title: Codex backend 401s in scratch HOMEs — nothing seeds ~/.codex/auth.json
priority: 2
schedule: once
---

## Goal
Codex sessions spawned with a relocated scratch HOME get 401 Unauthorized (observed live on m-eee81f orch-13): agent_env clears ambient env and relocates HOME, and unlike backend_claude (OAuth credentials copy) and backend_kimi (KIMI_SEED_ENTRIES), backend_codex has no seeding path — ~/.codex/auth.json never reaches the session. Add a codex seeding step mirroring the kimi/claude idiom (copy auth.json, and whatever minimal config codex needs, into the session scratch HOME), failing loudly when unauthenticated rather than 401-retry-looping. Found 2026-08-09 while rerouting missions off claude; codex CLI 0.131.0 also predates the account's current default model (gpt-5.6-sol) — a session that pins an older model string (gpt-5.5 verified live) works, so the seeding should not rely on account-default models.

## Context


## Scoping answers

## Acceptance hints
