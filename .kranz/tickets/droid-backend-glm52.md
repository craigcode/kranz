---
title: DroidBackend — GLM 5.2 (Fireworks) as a second scrutiny-validator vendor
priority: 3
schedule: once
---

## Goal

Add a DroidBackend (AgentBackend over `droid exec`) selectable ONLY for
the scrutiny validator, exactly mirroring the CodexBackend posture
(m-5d2c79): cross-vendor audit, never building. Default model:
`accounts/fireworks/models/glm-5p2` (configured in the operator's
~/.factory/settings.json). config::validate must reject any attempt to
use it for worker or functional-validator roles, mirroring the codex
rule; selection goes through the existing select_scrutiny_backend seam
with the same loud fallback decision.

## Context

Operator has Factory droid 0.164.0 installed with custom models in
~/.factory/settings.json: GLM 5.2 on Fireworks plus two local Ollama
models (Qwen3 Coder 30B, Nemotron Nano 4B). Fireworks billing is
per-token API — decoupled from both the Max subscription and any
Anthropic promo window, so this is the designated post-promo cost
valve for scrutiny.

CLI probe (2026-07-06, live against GLM 5.2):
- `droid exec -m "accounts/fireworks/models/glm-5p2" -o json --auto low "<prompt>"`
  returns a single JSON result object:
  `{"type":"result","subtype":"success","is_error":false,"duration_ms":1976,
  "num_turns":1,"result":"...","session_id":"...","usage":{"input_tokens":14631,
  "output_tokens":5,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}`
  — Anthropic-shaped envelope; token usage IS reported (unlike the codex
  probe's gap), so cost tracking can price it directly.
- Relevant flags confirmed: `-o/--output-format` (text|json|…; probe
  `stream-json` event kinds at build time for live AgentEvent
  translation), `-m/--model`, `--cwd`, `--auto low|medium|high`,
  `--enabled-tools/--disabled-tools`, `-f` prompt-from-file,
  `--append-system-prompt`.
- Fixed overhead: ~14.6k input tokens of droid system prompt per
  session — include in the pricing entry so estimates stay honest.
- droid also has `--mission/--worker-model` orchestration flags: NOT
  used; kranz remains the orchestrator, droid is a session runner only.

Engine seams (all proven by m-5d2c79): AgentBackend trait,
select_scrutiny_backend with loud fallback decision, config::validate
role restriction, DEFAULT_CODEX_MODEL-style pricing constant,
stub-binary tests for the exec translation. Missions must gate
--workspace, not -p kranz-engine (repo meta-lesson).

## Scoping answers

- Scrutiny-only this mission; promoting droid-routed models to worker
  roles is a separate future decision requiring soak data.
- Default model glm-5p2 via Fireworks; model id passed with -m using
  the full `accounts/fireworks/models/...` path. Local Ollama models
  are reachable through the same backend by model id but are NOT
  wired to any role this mission.
- Auth lives in ~/.factory/settings.json (operator-managed); the
  backend must fail loudly (decision event + fallback) if droid or the
  model is unavailable, mirroring codex fallback behavior.

## Acceptance hints

- A stub-droid-binary test proves exec invocation, JSON parse, usage
  extraction, and error/fallback paths without network.
- config accepts the droid scrutiny backend selection and rejects it
  for worker/functional roles (mirror the codex validate tests).
- A live scrutiny round against GLM 5.2 completes on a mock mission
  (manual/live check acceptable, like m-5d2c79's live verify).
- cargo test --workspace green; fmt/clippy clean.
