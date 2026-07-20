---
title: Add Kimi K3 as an optional AgentBackend (backend_kimi, validator-first)
priority: 3
schedule: once
---

## Goal
Add a fourth agent backend, `backend_kimi`, driving the Kimi Code CLI
(`kimi`) in headless mode, so workers and validators can run on Kimi's K3
flagship (model id `k3`, thinking effort low/high/max) or K2.7
(`kimi-for-coding[-highspeed]`). Follow the established single-shot backend
pattern (backend_codex.rs / backend_droid.rs): `kimi -p "<prompt>"
--output-format stream-json`, parse the JSONL event stream into `AgentEvent`,
binary discovery `KRANZ_KIMI_BIN` → PATH → well-known install paths, recorded
fixture as parser ground truth. Validator-first (cross-vendor scrutiny
diversity), worker-capable behind the same role config.

## Context
kranz's defensible layer is the mission/audit/consent harness; importing
headless coding CLIs as backends turns vendor pressure into model/runtime
leverage (roadmap-options "Now" lane; precedents shipped for codex and droid,
probe completed for cursor). Official docs confirm the interface:
`kimi -p` is non-interactive with auto permission policy (static deny rules
remain), `--output-format stream-json` emits one JSON object per line
(Assistant messages, tool_calls → Tool messages, thinking/progress on
stderr), `-m` selects a model alias, `kimi login` (device flow) caches auth
locally and console API keys serve third-party tools. Model IDs: `k3`
(K3 2.8T, effort low/high/max), `kimi-for-coding`, `kimi-for-coding-highspeed`.

Probe discipline (docs/scoping/cursor-cli-backend.md precedent — abstain from
billed calls until auth is proven, capture verbatim evidence):
1. AUTH FIRST: prove headless auth in an isolated env (login token location;
   env-var auth for third-party tools) and record whether auth survives a
   relocated $HOME (the claude-cli-min-env trap; cursor lost auth there).
2. Capture a real `kimi -p --output-format stream-json` session as a
   committed fixture; document the event schema — Init/Text/ToolUse/
   ToolResult/Result shapes, and CRITICALLY whether the Result object carries
   token usage (cost.rs needs it; Kimi Code is subscription — if usage is
   unmetered, record tokens but mark cost Meterless, matching the
   backend_readiness "never invent a 0% quota bar" rule).
3. Determine the `-m` alias form that selects K3 and each effort level, and
   how static deny rules / permission policy are configured headlessly.
Then implement: backend_kimi.rs (single-shot only; resume/streaming-input
rejected at the seam like codex/droid), SessionSpec claude-isms documented as
ignored, backend_kind config plumbing + `kimi` in role/backend validation and
the dashboard pickers, readiness probe wiring (version probe + auth classify),
cost.rs pricing (or Meterless) for the kimi model family. Permission profiles
degrade to sandbox + engine sweeps on this backend — say so in config docs
(same caveat as codex/droid).

## Acceptance hints
- Probe evidence committed (fixture JSONL + notes): auth classification,
  stream-json schema, model-alias/effort mapping, deny-rule configuration.
- `backend_kimi` parses the committed fixture into AgentEvent with unit
  tests; discovery honors KRANZ_KIMI_BIN then PATH then well-known paths.
- A mock-configured kimi validator runs in a mission_test-style harness;
  config rejects an invalid `kimi` model/effort pair and accepts `k3` with
  each of low/high/max.
- `cargo test --workspace`, clippy -D warnings, fmt --check all green.
