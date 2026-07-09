# Cursor CLI backend probe

Status: probe opened 2026-07-09; live headless prompt capture is blocked on
usable Cursor CLI authentication.

## Why

Cursor now overlaps unattended coding work through its CLI, Agents Window,
cloud agents, worktrees, automations, hooks, Agent Review, and first-party
model pool including Grok 4.5. Kranz should not chase Cursor as an IDE. The
strategic fit is to treat Cursor as another session runtime behind kranz's
mission/audit/consent harness.

## Probe findings so far

- `agent` is installed at `<operator-home>/.local/bin/agent`; version
  `2026.04.13-a9d7fb5`.
- `cursor` is installed at `<operator-home>/.local/bin/cursor`; desktop
  wrapper version reported `Cursor 3.10.20`, with an `agent` subcommand.
- `agent --help` confirms the headless surface needed for a candidate backend:
  `--print`, `--output-format text|json|stream-json`, `--model`,
  `--list-models`, `--mode plan|ask`, `--force`, `--sandbox`, `--trust`,
  `--workspace`, and `--worktree`.
- In the sandbox, authenticated commands crashed with
  `SecItemCopyMatching failed -50`; outside the sandbox, `agent status`
  reported login success but could not fetch user details.
- `agent models` outside the sandbox returned `No models available for this
  account`; `agent about` reported default `Composer 2 Fast` but
  `User Email Not logged in`.
- `agent --print --output-format json --mode ask --trust ...` and the same
  command with `stream-json` both failed with `Authentication required`.
- `agent login` opened the browser/deep-link auth flow but did not complete
  during the probe window.

Conclusion: before `backend_cursor` can be implemented, kranz needs a Cursor
preflight that distinguishes "CLI installed" from "headless auth usable" and
from "requested model available".

## Probe plan

1. Establish a usable auth path: either complete `agent login` or provide
   `CURSOR_API_KEY`; verify with `agent status`, `agent models`, and one
   `agent --print` prompt.
2. Capture read-only fixtures in a temp git repo for `--output-format text`,
   `json`, and `stream-json`; include a prompt that returns a kranz-style
   terminal report.
3. Capture write-capable behavior in a temp git repo using `--workspace` and
   controlled prompts: file edit, shell command, failed command, and no-op.
   Inspect the resulting git diff and output stream.
4. Probe model selection for the default model, Grok 4.5, and an invalid model.
   Do not assume the public model display name is the CLI model id.
5. Probe permission posture: `--mode ask`, `--mode plan`, default agent mode,
   `--force`, and `--sandbox enabled|disabled`; record which modes can satisfy
   worker vs validator roles.
6. Decide implementation route:
   - direct CLI parser if `stream-json` exposes enough event structure;
   - ACP-backed adapter if the CLI output is too lossy but ACP gives a stable
     protocol;
   - no backend yet if auth/model availability is not reliable headlessly.

## Backend acceptance bar

Only build `backend_cursor` after the probe can answer these:

- terminal assistant text can be stitched into `AgentEvent::Result.text`;
- tool use and tool results are observable enough for transcripts and denial
  reporting;
- usage/cost is available on the wire or can be priced from model ids;
- `cwd`/`--workspace` honors kranz worktree isolation;
- model availability failures are deterministic and user-readable;
- permission mapping can preserve kranz's no-push/no-publish/no-main-write
  invariants;
- a fixture test can prove parser behavior without network access.

First implementation should be single-shot and validator-first, mirroring the
Codex/Droid path. Worker use waits for live soak.

## Decision (2026-07-09)

**Recommendation: defer.**

### Rationale

The read-only preflight (`docs/scoping/cursor-probe-evidence/preflight.md`,
`probe-result.json`) confirms the full flag surface a direct-parser or
ACP-backed backend would need — `--print`, `--output-format
text|json|stream-json`, `--model`, `--workspace`, `--worktree`, `--sandbox`,
`--trust`, `--mode ask|plan` — so this is not a "CLI can't do it" defer. It is
an auth defer: `agent status` and `agent about` disagree with each other
("Login successful!" vs. "User Email: Not logged in"), and the decisive
signal, `agent models` / `agent --list-models`, reports **zero models
available for this account** regardless of `--model` value (default `gpt-5`,
target `grok-4.5`, and a deliberately invalid id all return byte-identical
output). `agent --print` is not un-invokable: `--output-format json` and the
same command with `stream-json` were each run once during the probe and both
failed free at the auth gate with `Authentication required`, before any
billed turn could start — a deterministic, user-readable, post-arg-parse
`--print` runtime rejection that *was* observed. Beyond that single free
failure, `agent --print` was deliberately not invoked further; that
abstention is clean-room read-only discipline (no billed turn was ever put
at risk against an account with no confirmed model access), not evidence
that `--print` produces nothing observable. `probe-result.json` still
carries `"fixture": null` — there is no captured *successful* `stream-json`
output to build a parser against, and no evidence that terminal text,
tool-use/result events, usage/cost, or permission-mode behavior are
observable on the wire for an authenticated turn.
Per the acceptance bar, a route decision of `direct-parser` or `acp` requires
evidence a fixture would provide; with `auth_usable: false` and no fixture,
neither is supportable, so the honest recommendation is `defer`.

### Acceptance-bar verdict table

| # | Acceptance-bar item | Verdict | Why |
|---|---|---|---|
| 1 | Terminal assistant text stitches into `Result.text` | unresolved | No `--print` run was captured; no output to inspect. |
| 2 | Tool-use/tool-results observable for transcripts & denial reporting | unresolved | No write-capable run (file edit, shell command) was exercised. |
| 3 | Usage/cost on the wire or priceable from model ids | unresolved | No captured turn to inspect for usage/cost fields. |
| 4 | `cwd`/`--workspace` honors kranz worktree isolation | unresolved | Flags exist (`--workspace`, `--worktree`, `--worktree-base`, `--skip-worktree-setup`) but were never exercised against a real run. |
| 5 | Model-availability failures are deterministic and user-readable | unresolved | `agent --list-models` is deterministic ("No models available…", exit 0) but not diagnostic across model ids. Separately, `agent --print` (`json` and `stream-json`) failed free at the auth gate with a deterministic, user-readable `Authentication required` before any billed turn — so a post-arg-parse `--print` runtime failure mode *was* partially observed. What remains unresolved: distinguishing an invalid model id from a valid-but-unentitled one, and observing failure (or success) semantics of an actually-authenticated `--print` turn. |
| 6 | Permission mapping preserves no-push/no-publish/no-main-write invariants | unresolved | `--mode ask\|plan`, `--force`/`--yolo`, `--sandbox`, `--trust` are documented in `--help` but none were exercised in a live run. |
| 7 | Fixture test proves parser behavior offline | unresolved | `probe-result.json.fixture` is `null`; no captured output exists to derive a fixture from. |

All seven items are unresolved and none can be satisfied without a live,
authenticated `--print` capture. This is consistent with `defer`.

### Preconditions to unblock

Re-run this probe (or open a follow-up feature) once **both** of the
following hold, then re-attempt items 1–7 with a real `agent --print
--output-format stream-json` capture in a temp git repo:

1. **Reachable non-interactive credential.** A `CURSOR_API_KEY` (or
   equivalent bearer token) must be present in the worker's environment so
   headless auth does not depend on the interactive `agent login`
   browser/deep-link flow, which a sandboxed headless worker cannot complete.
2. **At least one entitled model on the account.** The Cursor account tied to
   that credential must have a model provisioned — `agent models` currently
   reports zero models for every `--model` value tried, and there is no
   local keychain/session-store access available to this sandboxed worker to
   inspect or repair that account state directly.

Once both hold, capture `--output-format text`, `json`, and `stream-json` for
at least one read-only prompt and one write-capable prompt (file edit, shell
command, failed command, no-op), record the raw output as
`probe-result.json.fixture`, and re-evaluate the acceptance bar. If
`stream-json` exposes per-event structure comparable to `backend_codex`'s
`item.started`/`item.completed`/`turn.completed` with usage, route to
`direct-parser`; if it is too lossy but ACP gives a stable protocol, route to
`acp`. No backend implementation brief is written here since the route is
not yet decided — write it as part of that follow-up once the acceptance bar
is green.

## Decision (2026-07-09, revised)

**Recommendation: direct-parser.**

### What changed since the defer decision above

Both preconditions listed under "Preconditions to unblock" above are now
resolved. A later probe pass (features f-1-1 through f-1-4, same day) ran
against a re-verified CLI (`2026.07.08-0c04a8a`) with a reachable,
non-interactive credential already established in the worker's environment:
`agent status`/`agent about` now agree on a logged-in, identifiable account
(Subscription Tier: Ultra), and `agent models`/`agent --list-models` return a
populated ~190-entry model catalog (was zero). With auth usable and models
provisioned, `agent --print --output-format json` and `stream-json` were run
live (single smallest-prompt calls per model id, plus one write-capable
`stream-json` capture), producing the evidence this decision is based on. See
`docs/scoping/cursor-probe-evidence/probe-result.json` (`.model_matrix`,
`.permission_posture`, `.fixture_capture`, `.recommendation_note`,
`.acceptance_bar`) and the committed fixture
`docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl`.

This decision's route (`direct-parser`) equals
`probe-result.json.recommendation` and `probe-result.json.route_decision.decision`.

### Rationale

- `--print --output-format json` (single result object) and `stream-json`
  (event stream) are stable, self-describing structures on their own — no
  need to speak Cursor's Agent Client Protocol to get structured
  tool-call/tool-result/usage data; it is already on the wire in plain JSON.
- The uncertainty that justified `defer` above — an account with zero
  provisioned models, making model-selection and model-availability failures
  indistinguishable — is gone: the account now has a populated ~190-model
  catalog, and an invalid `--model` id fails deterministically and
  diagnostically (exit 1, plain-text `Cannot use this model: <id>. Available
  models: ...`, no `session_id`/`usage` present, i.e. failed before any
  billed turn started).
- Permission posture (`--mode ask`/`plan` vs. default/`--force`/`--yolo`) is
  fully controllable and observable through CLI flags plus output parsing —
  no ACP round-trip needed to enforce or detect a read-only vs. read-write
  turn.
- The one caveat: `--sandbox` is not a security boundary (see Permission and
  auth requirements below) — that does not block choosing a route, but it
  does constrain how `backend_cursor` must be deployed.

### Refreshed acceptance-bar verdict table

| # | Acceptance-bar item | Verdict | Why |
|---|---|---|---|
| 1 | Terminal assistant text stitches into `Result.text` | RESOLVED | `--print` json/stream-json both captured live; json gives a single terminal `result` event with the full text in `.result`; stream-json splits the same text across the final `assistant` event and the terminal `result` event. |
| 2 | Tool-use/tool-results observable for transcripts & denial reporting | RESOLVED | Write-capable `stream-json` fixture captured showing `tool_call/started` + `tool_call/completed` pairs with distinct `success`/`failure` result variants (`shellToolCall`, `editToolCall`, `readToolCall`). |
| 3 | Usage/cost on the wire or priceable from model ids | RESOLVED | Every `--print` json/stream-json run's terminal `result` event carries a `usage` object (`inputTokens`, `outputTokens`, `cacheReadTokens`, `cacheWriteTokens`); no separate dollar-cost field is on the wire — cost must be computed client-side from usage tokens against a locally maintained per-model price table. |
| 4 | `cwd`/`--workspace` honors kranz worktree isolation | PARTIALLY RESOLVED | `--workspace` was exercised live in every run (throwaway tmpdir git repos) and worked as documented. `--worktree` was intentionally never exercised (spec required `--workspace` only, to avoid mutating `~/.cursor/worktrees` state) — its behavior remains unobserved by design. |
| 5 | Model-availability failures are deterministic and user-readable | RESOLVED | As of the populated ~190-model catalog, an invalid `--model` id fails deterministically and diagnostically — exit 1, plain-text `Cannot use this model: <id>. Available models: ...` naming the bad id and enumerating every valid id, before any billed turn starts. |
| 6 | Permission mapping preserves no-push/no-publish/no-main-write invariants | RESOLVED | `--mode ask`/`plan`: read-only, safe for a validator role (observed, no filesystem mutation). Default mode / `--force` / `--yolo`: read-write, worker role only (observed for default+`--force`; `--yolo` inferred as a documented alias of `--force`). `--sandbox enabled`/`disabled`: observed to make **no** difference to outbound network access or writes outside `--workspace` — must not be relied on as a kranz isolation boundary; isolation must come from external process/workspace controls instead (see below). |
| 7 | Fixture test proves parser behavior offline | RESOLVED | `probe-result.json.fixture` points at the committed `fixture-stream-json.jsonl` (fixture-json.json/fixture-text.txt also exist), captured from a real write-capable run, redacted, suitable for an offline fixture-driven parser test. |

All seven items are now RESOLVED or PARTIALLY RESOLVED (item 4, by design —
`--worktree` was never exercised). This is consistent with `direct-parser`.

### Implementation brief (single-shot, validator-first)

This is the authoritative `backend_cursor` implementation brief. Any brief
text under `docs/scoping/cursor-probe-evidence/implementation-brief.md` is a
pointer to this section, not a second source of truth. First implementation
should be single-shot and validator-first, mirroring the Codex/Droid path;
worker use waits for live soak.

Invoke via:

```
agent --print --output-format stream-json --workspace <dir> --model <id> [mode/force/sandbox flags] "<prompt>"
```

Prefer `stream-json` over `json` for a live backend so tool-call progress is
observable incrementally; fall back to `json` only for simple one-shot calls
where only the final result matters.

#### Event-to-`AgentEvent` mapping

Grounded in the committed fixture
`docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl` (prompt:
"Create hello.txt containing hi, then run cat hello.txt"), mirroring
`docs/scoping/codex-backend.md`'s table shape:

| stream-json event | shape | maps to `AgentEvent` |
|---|---|---|
| `{"type":"system","subtype":"init","apiKeySource":...,"cwd":...,"model":...,"permissionMode":...}` | first line of every run | `Init { session_id: session_id, model: <model display string from the wire>, raw }` |
| `{"type":"user","message":{"role":"user","content":[{"type":"text","text":...}]}}` | echoes the prompt sent | `Other { raw }` (no engine action; input echo) |
| `{"type":"tool_call","subtype":"started","tool_call":{"shellToolCall"\|"editToolCall"\|"readToolCall":{"args":{...}}}}` | tool about to run (one of `shellToolCall`, `editToolCall`, `readToolCall` observed in the fixture) | `ToolUse { tool: <key present under tool_call, e.g. "shellToolCall">, summary: <args.command or args.path>, raw }` |
| `{"type":"tool_call","subtype":"completed","tool_call":{...,"result":{"success":{...}}}}` | matching result, discriminated union: `result.success` (fixture: `shellToolCall.result.success` with `stdout`/`exitCode`; `editToolCall.result.success` with `diffString`; `readToolCall.result.success` with `content`) or `result.failure` (not in this fixture; seen in the deliberately-failing-command probe as `{command, exitCode, signal, stdout, stderr, aborted}`) | `ToolResult { tool: Some(<same key as started>), denied: false when result.success present (result.failure with a real exitCode is a normal failed command, not a denial), summary: <result.success.stdout/message or result.failure.stderr>, raw }` |
| `{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":...}]}}` | assistant narration/final text; may appear once (or more, per the no-tool-call no-op prompt) | `Text { text: message.content[0].text, raw }` |
| `{"type":"result","subtype":"success","result":...,"usage":{"inputTokens","outputTokens","cacheReadTokens","cacheWriteTokens"}}` | terminal event for the run; carries the full result text and token usage | `Result { text: result, is_error: false, usage: TokenUsage { input: inputTokens, output: outputTokens, cached: cacheReadTokens }, cost_usd: <computed client-side via a per-model price table keyed on --model id; not on the wire>, num_turns: Some(1), raw }` |

Model id validation: pass the id straight through; on exit code 1 with a
`Cannot use this model: <id>. Available models: ...` message (no
`session_id`/`usage` present, i.e. pre-billing), surface that as a
configuration error to the caller rather than retrying — no turn was billed.

#### Role mapping (validator vs. worker)

| Flag | Effect (observed unless noted) | validator (read-only) | worker (read-write) |
|---|---|---|---|
| `--mode ask` | model refuses file/shell writes at the turn level | yes | no |
| `--mode plan` | model produces a plan, no writes observed | yes | no |
| default mode (no `--mode`) + `--trust` | writes proceed with no interactive prompt in headless `--print` | no | yes |
| `--force` | writes/shell commands proceed unprompted (observed) | no | yes |
| `--yolo` | documented alias of `--force` (inferred from `--help`, not separately live-tested) | no | yes |

Use `--mode ask` or `--mode plan` for any kranz validator role that must
never mutate state. Use default mode or `--force`/`--yolo` only for a worker
role that is explicitly authorized to write.

#### Permission and auth requirements (headless deployment)

Two distinct concerns; both must be handled for a headless `backend_cursor`
worker or validator:

1. **Sandbox is not a kranz isolation boundary.** `--sandbox enabled` was
   tested live against two probes — outbound network access (`curl` to a
   public URL) and writing a file outside the declared `--workspace`
   directory (to `/tmp`) — and neither was blocked; `--sandbox disabled`
   produced identical results. This is an observed negative result in this
   environment/account/OS (macOS/darwin), not a claim that no sandbox
   mechanism exists anywhere in the CLI. **Consequence:** do not rely on
   `--sandbox enabled` to enforce kranz's no-push/no-publish/no-main-write
   invariants. Isolation must come from external controls independent of
   this flag: throwaway `--workspace` directories (never the mission repo),
   scoped credentials/tokens so a `git push` or publish step is unreachable
   even if attempted, and process/container-level sandboxing if stronger
   isolation is required.
2. **Auth state does not survive a relocated `$HOME`.** Cursor's CLI auth
   state lives under `$HOME/.cursor` and does not survive a relocated
   `$HOME` — running `HOME=/tmp/x agent status` reports `Not logged in`
   (verified: `git show 3d8f93e`). A headless `backend_cursor`
   worker/validator therefore needs one of:
   - the real `~/.cursor` directory carried into the process's `HOME` (e.g.
     bind-mounting or copying `~/.cursor` into whatever `HOME` the sandboxed
     process sees), or
   - a `CURSOR_API_KEY` (or equivalent bearer token, per `agent --help`'s
     `--api-key`/`--header` flags) present in the session's environment, so
     headless auth does not depend on `$HOME/.cursor` at all.

   Either path must be provisioned explicitly by whatever launches the
   `backend_cursor` process — do not assume a relocated-`HOME` sandbox will
   inherit a usable Cursor login for free.

#### Known gaps for whoever picks this up next

- `--worktree` was intentionally never exercised (spec required
  `--workspace` only, to avoid mutating `~/.cursor/worktrees` state); if a
  future feature wants worktree-based isolation instead of plain
  `--workspace`, that flag's behavior is still unobserved.
- `--yolo` was not separately live-tested (relied on `--help`'s explicit
  "alias for --force" documentation) — low risk, but flagged as inferred,
  not observed.
- `--sandbox`'s negative result covers exactly two probes (network egress,
  filesystem write outside workspace) — it is not an exhaustive audit of
  every possible sandbox-relevant action.
