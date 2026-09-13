---
state: done
title: Cross-vendor scrutiny: Codex as an optional validator backend
priority: 2
schedule: once
---

## Goal
Add a CodexBackend implementing AgentBackend over codex exec --json, scoped FIRST to the scrutiny-validator role only: validators are fresh single-shot sessions (no resume semantics needed) and never need the full tool surface, making this the smallest possible seam for the biggest genuine win — a scrutiny gate from a different model family does not share the worker's blind spots. Config: validatorScrutiny gains an optional backend field (default claude, unchanged). Engine-side fallbacks for capability gaps (budget enforcement via existing token accounting, report-schema validation engine-side when the CLI cannot enforce it), degrading loudly via preflight, never silently.

## Context
Evidence this improves the product, from this repo's own history: the
P1/P2/P3 queue findings (2026-07-05 remediation series c1e7f3b..87823c2)
came from a Codex review of claude-built code — it caught a real
concurrency gap the builder missed; a further review pass then caught
two more defects in the remediation. Same-family models share failure
modes (the letter-over-spirit pathology is model-correlated); scrutiny's
whole value is independence from the worker.

Seam: crates/engine/src/backend.rs (AgentBackend trait, SessionSpec) —
MockBackend already proves the engine is backend-agnostic. New
crates/engine/src/backend_codex.rs translating codex exec --json JSONL
events onto AgentEvent (token usage included; add a codex pricing table
to cost.rs). Reuse the process-group/Job-Object tree-kill from
backend_claude.rs. SessionSpec carries claude-isms (append_system_prompt,
permission patterns, --json-schema, effort names, --max-budget-usd,
--resume): scope THIS ticket to what a single-shot validator needs —
prompt, model, sandbox posture, JSON output — and tag remaining
claude-specific SessionSpec fields with comments as part of the work so
future backends know the boundary. Codex ships OS-level sandboxing
(--sandbox read-only fits a scrutiny validator exactly). Codex CLI
must be on PATH and authenticated; preflight reports absence as an
issue and the config falls back to the claude validator with a loud
decision, never silently.

Deliberately OUT of scope: codex workers/orchestrators (resume
semantics, tool-permission mapping), API-direct backends (kranz
supervises CLIs, it does not reimplement tool execution), gemini (falls
out cheaply once the capability map exists). Trigger for the wider
backend generalization: a Gas City heterogeneous fleet materializing or
chronic Max-window starvation (docs/gascity-citizenship.md futures).

## Scoping answers

## Acceptance hints
- With validatorScrutiny.backend = "codex" and codex on PATH, a mission's scrutiny validation runs through codex exec and its findings flow into the normal fix-cycle machinery (integration test may stub the codex binary with a script emitting canned JSONL — no real API spend in CI).
- Default config is byte-identical in behavior: no backend field means ClaudeBackend, all existing suites green unchanged.
- Codex absent/unauthenticated => preflight issue + loud fallback decision to the claude validator, never a silent swap.
- Validator cost/tokens from codex sessions appear in mission totals with codex pricing.
- cargo test --workspace passes, piped through grep -qE 'result: ok\. [1-9][0-9]* passed'; clippy and fmt clean.
