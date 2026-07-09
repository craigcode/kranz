---
title: Cursor CLI live capture + backend route decision (post-auth)
priority: 2
schedule: once
---

## Goal

Complete the live half of the Cursor CLI probe now that authentication is
green, and finalize the `backend_cursor` route decision (direct-parser vs ACP
vs defer) with an implementation brief if green. Extends the evidence merged
from mission m-7820b9 under `docs/scoping/cursor-probe-evidence/`.

## Context

m-7820b9 deferred on auth. Conditions changed on 2026-07-09 (verified live):

- `agent login` completed: `agent status` → "Logged in as <redacted>",
  subscription tier Ultra, `agent about` resolves user details.
- The CLI self-updated: `2026.04.13-a9d7fb5` → `2026.07.08-0c04a8a`.
  RE-VERIFY the flag surface against the new binary; do not trust the
  merged preflight's flag list blindly.
- Models are provisioned: `agent models` returns ~193 entries. Verified ids:
  - `grok-4.5-xhigh` ("Cursor Grok 4.5" — the headline model),
    `grok-4.5-fast-xhigh`; WARNING: id↔display-name tiers are OFF BY ONE
    (`grok-4.5-medium` displays "Grok 4.5 Low", `grok-4.5-high` displays
    "Grok 4.5 Medium"). Record this trap in the evidence.
  - `gpt-5.6-sol-*`, `gpt-5.6-terra-*`, `gpt-5.6-luna-*` families (each
    none/low/medium/high/xhigh/max × fast, 1M context) — great candidate
    alternates; characterize at least one cheaply.
  - `composer-2.5` / `composer-2.5-fast` (Cursor in-house).
  - `claude-opus-4-8-thinking-high` — Claude models are reachable through
    this lane too; note the aggregator value in the decision.

Design note: DO NOT build the backend in this ticket. Evidence, fixtures,
decision, brief only. Keep prompt spend minimal: smallest prompts that show
the needed structure. See docs/scoping/cursor-cli-backend.md probe plan
steps 2–5 and the acceptance bar.

## Acceptance hints

- Work in a throwaway temp git repo (never this repo) for all live prompts.
- Capture the smallest `--output-format stream-json` fixture showing:
  assistant text, one tool-use, one tool-result, and a terminal usage/cost
  event. Redact emails/tokens/session-ids/home paths. Commit it under
  docs/scoping/cursor-probe-evidence/ and set probe-result.json.fixture.
- Also capture `text` and `json` outputs once each for shape comparison.
- Live model matrix: `auto`, `grok-4.5-xhigh`, one cheap gpt-5.6 (e.g.
  `gpt-5.6-luna-low`), and an invalid id — record real responses and how
  model-availability failures present. Update probe-result.json.model_matrix.
- Write-capable behavior in the temp repo: file edit, shell command, failed
  command, no-op; inspect git diff + stream. Record permission posture for
  `--mode ask`, `--mode plan`, default, `--force`, `--sandbox enabled|disabled`
  against worker vs validator role needs.
- Score all seven acceptance-bar items in probe-result.json.acceptance_bar
  from this evidence; set recommendation strictly from observed structure.
- Update the Decision section in docs/scoping/cursor-cli-backend.md (new
  dated entry; keep the 2026-07-09 defer entry as history) with the final
  route and, if green, a scoped implementation brief mirroring the
  Codex/Droid path (single-shot, validator-first).
- The mission must not modify anything under crates/.
- Verified 2026-07-09: `agent` auth state lives under `$HOME/.cursor` and does
  NOT survive a relocated HOME (`HOME=/tmp/x agent status` → "Not logged in").
  The implementation brief must require either carrying `~/.cursor` into any
  relocated worker HOME or a `CURSOR_API_KEY` session env — record this in the
  permission/auth section of the decision.
