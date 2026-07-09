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
