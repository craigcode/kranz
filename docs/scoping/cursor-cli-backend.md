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
