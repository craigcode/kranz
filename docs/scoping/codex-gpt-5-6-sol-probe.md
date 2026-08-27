# Codex GPT-5.6 Sol probe

Probe date: 2026-08-25

## Question

Does kranz need a separate ChatGPT CLI backend to dispatch GPT-5.6 Sol, or
does the existing Codex backend already provide the required headless seam?

## Environment and method

- Binary: `codex-cli 0.147.0` (`codex`)
- Authentication: `codex login status` reported `Logged in using ChatGPT`
- Model: `gpt-5.6-sol`
- Mode: ephemeral, read-only, non-interactive JSONL in a throwaway directory
- Prompt: a tool-free request for the exact text `KRANZ_PROBE_OK`

The probe command used `codex exec --ephemeral --ignore-user-config
--ignore-rules --skip-git-repo-check --sandbox read-only --json --model
gpt-5.6-sol`. The committed fixture redacts the session identifier and keeps
the event structure and usage counts intact.

## Observed wire

The successful run exited 0 and emitted the same four event classes the
existing `backend_codex` parser consumes:

1. `thread.started` with a session id and no model field;
2. `turn.started`;
3. `item.completed` carrying the final `agent_message` text;
4. `turn.completed` carrying input, cached-input, output, and reasoning-output
   token counts, but no dollar-cost field.

`backend_codex` already supplies the configured model when `thread.started`
omits it, stitches the final agent message into the terminal result, and
derives cost when the CLI omits dollars. The probe also confirmed that
`cached_input_tokens` is reported inside total `input_tokens`; the parser now
stores uncached and cached input in disjoint `TokenUsage` lanes before applying
the documented Sol rates. The fallback also applies the documented 2× input /
1.5× output multiplier when a request exceeds 272K input tokens.

Fixture: `crates/engine/tests/fixtures/codex_exec_gpt_5_6_sol_probe.jsonl`.

## Decision

Do not add a duplicate `backend_chatgpt`. The supported ChatGPT-authenticated
CLI surface is `codex exec`, and kranz already owns that adapter. Make
`gpt-5.6-sol` the Codex backend default, retain the existing non-Claude
sandbox rules, and close `chatgpt-cli-backend` as absorbed by
`backend_codex`.

## Official references checked

- [Codex CLI developer commands](https://learn.chatgpt.com/docs/developer-commands?surface=cli)
  for the supported non-interactive `codex exec` and ChatGPT login surface.
- [GPT-5.6 Sol model reference](https://developers.openai.com/api/docs/models/gpt-5.6-sol)
  for the model id and input, cached-input, and output rates captured by the
  pricing fallback.
