# Codex backend: `codex exec --json` event mapping

Notes for the next feature (`backend_codex.rs`, a second `AgentBackend` impl
alongside the Claude CLI backend in `crates/engine/src/backend.rs`). Grounded
in one real minimal session captured locally with `codex-cli 0.131.0`
(authenticated) via `codex exec --json --sandbox read-only "<prompt>"`; the
adapted fixture lives at
`crates/engine/tests/fixtures/codex_exec_scrutiny.jsonl`.

Codex emits one JSON object per line (no `type: "system"/"assistant"/"result"`
envelope like the Claude CLI — codex uses its own `thread.*`/`turn.*`/`item.*`
vocabulary). Kranz only reads `validatorScrutiny.backend = "codex"` sessions,
always single-shot, so only these event types matter:

| codex JSONL event | shape | maps to `AgentEvent` |
|---|---|---|
| `{"type":"thread.started","thread_id":...}` | first line of every run | `Init { session_id: thread_id, model: <configured model, not on the wire>, raw }` |
| `{"type":"turn.started"}` | precedes model work each turn | `Other { raw }` (no engine action; single-shot has exactly one turn) |
| `{"type":"item.completed","item":{"type":"agent_message","text":...}}` | assistant text; may appear more than once per turn (narration, then final report) | `Text { text: item.text, raw }` |
| `{"type":"item.started","item":{"type":"command_execution","command":...,"status":"in_progress"}}` | shell command about to run under the sandbox (`read-only` for scrutiny) | `ToolUse { tool: "command_execution", summary: item.command, raw }` |
| `{"type":"item.completed","item":{"type":"command_execution","command":...,"aggregated_output":...,"exit_code":...,"status":"completed"}}` | same command's result | `ToolResult { tool: Some("command_execution"), denied: exit_code == null && status == "failed" (sandbox refusal; a non-zero exit_code with a real value is a normal failed command, not a denial), summary: aggregated_output, raw }` |
| `{"type":"turn.completed","usage":{"input_tokens","cached_input_tokens","output_tokens","reasoning_output_tokens"}}` | terminal event for the turn; carries token usage | `Result { text: <text of the last agent_message item.completed this turn>, is_error: false, usage: TokenUsage { input: input_tokens, output: output_tokens + reasoning_output_tokens, cached: cached_input_tokens }, cost_usd: <computed engine-side via cost.rs pricing_for_model(DEFAULT_CODEX_MODEL), codex does not report cost>, num_turns: Some(1), raw }` |

Everything else observed or documented (`rate_limit_event`-style throttling
info, `item.started`/`item.completed` for `reasoning` items, error frames) is
not in the fixture and should fall through to `AgentEvent::Other` — codex has
no analogue to Claude's `--json-schema`, so the final report is parsed
engine-side from the last `agent_message` text via
[`parse_validator_report`](../../crates/engine/src/runner.rs) exactly as today,
not enforced at the source.

`turn.completed` has no `text` field of its own — the engine must remember the
most recent `agent_message` text within the turn and treat that pairing as the
terminal `Result`. This is the "loud claude fallback" seam: if a
`turn.completed` arrives with no prior `agent_message` in the turn, or the
remembered text fails `parse_validator_report`, the engine reports a hard
failure rather than silently falling back to a Claude re-run — scrutiny
findings must come from the backend actually configured, or not at all.
