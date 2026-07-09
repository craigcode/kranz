# Cursor CLI (`agent`) preflight probe log

Binary: `~/.local/bin/agent`
CLI version: `2026.04.13-a9d7fb5`
Probe date: 2026-07-09
Scope: primarily read-only commands. `agent --print` was run twice — once with
`--output-format json` and once with `--output-format stream-json`, both
`--mode ask --trust` — to observe whether it fails before a billed turn
starts; both failed free at the auth gate with `Authentication required` (see
"`agent --print` auth-gate check" below). Beyond those two zero-cost calls,
`--print` was intentionally not invoked further, to avoid risking a billed
turn against an account with no confirmed model access.
All output below is verbatim except email/token/account identifiers, which are replaced with `<redacted>`. Terminal spinner control codes (`\x1b[2K`, cursor moves) were stripped for readability; no textual content was altered.

## `agent --version`

```
2026.04.13-a9d7fb5
```

## `agent --help`

```
Usage: agent [options] [command] [prompt...]

Start the Cursor Agent

Arguments:
  prompt                       Initial prompt for the agent

Options:
  -v, --version                Output the version number
  --api-key <key>              API key for authentication (can also use
                               CURSOR_API_KEY env var)
  -H, --header <header>        Add custom header to agent requests (format:
                               'Name: Value', can be used multiple times)
  -p, --print                  Print responses to console (for scripts or
                               non-interactive use). Has access to all tools,
                               including write and shell. (default: false)
  --output-format <format>     Output format (only works with --print): text |
                               json | stream-json (default: "text")
  --stream-partial-output      Stream partial output as individual text deltas
                               (only works with --print and stream-json format)
                               (default: false)
  -c, --cloud                  Start in cloud mode (open composer picker on
                               launch) (default: false)
  --mode <mode>                Start in the given execution mode. plan:
                               read-only/planning (analyze, propose plans, no
                               edits). ask: Q&A style for explanations and
                               questions (read-only). (choices: "plan", "ask")
  --plan                       Start in plan mode (shorthand for --mode=plan).
                               Ignored if --cloud is passed. (default: false)
  --resume [chatId]            Select a session to resume (default: false)
  --continue                   Continue previous session (default: false)
  --model <model>              Model to use (e.g., gpt-5, sonnet-4,
                               sonnet-4-thinking)
  --list-models                List available models and exit (default: false)
  -f, --force                  Force allow commands unless explicitly denied
                               (default: false)
  --yolo                       Alias for --force (Run Everything) (default:
                               false)
  --sandbox <mode>             Explicitly enable or disable sandbox mode
                               (overrides config) (choices: "enabled",
                               "disabled")
  --approve-mcps               Automatically approve all MCP servers (default:
                               false)
  --trust                      Trust the current workspace without prompting
                               (only works with --print/headless mode) (default:
                               false)
  --workspace <path>           Workspace directory to use (defaults to current
                               working directory)
  -w, --worktree [name]        Start in an isolated git worktree at
                               ~/.cursor/worktrees/<reponame>/<name>. If
                               omitted, a name is generated.
  --worktree-base <branch>     Branch or ref to base the new worktree on
                               (default: current HEAD)
  --skip-worktree-setup        Skip running worktree setup scripts from
                               .cursor/worktrees.json (default: false)
  -h, --help                   Display help for command

Commands:
  install-shell-integration    Install shell integration to ~/.zshrc
  uninstall-shell-integration  Remove shell integration from ~/.zshrc
  login                        Authenticate with Cursor. Set NO_OPEN_BROWSER to
                               disable browser opening.
  logout                       Sign out and clear stored authentication
  mcp                          Manage MCP servers
  status|whoami [options]      View authentication status
  models                       List available models for this account
  about [options]              Display version, system, and account information
  update                       Update Cursor Agent to the latest version
  create-chat                  Create a new empty chat and return its ID
  generate-rule|rule           Generate a new Cursor rule with interactive
                               prompts
  agent [prompt...]            Start the Cursor Agent
  ls                           Resume a chat session
  resume                       Resume the latest chat session
  help [command]               Display help for command
```

Flag surface relevant to a backend (all present, matches the spec's expected set plus extras):
`--print`, `--output-format text|json|stream-json`, `--model`, `--list-models`,
`--mode plan|ask`, `--plan`, `--force`/`--yolo`, `--sandbox enabled|disabled`,
`--trust`, `--workspace <path>`, `--worktree [name]`, `--worktree-base <branch>`,
`--skip-worktree-setup`, `--stream-partial-output`, `--resume [chatId]`,
`--continue`, `--api-key <key>`, `--header <header>`, `--approve-mcps`.

## `agent status`

```
Starting login process...
Checking authentication status...
✓ Login successful!
Logged in (unable to fetch user details)
```
Exit code: `0`

Note: reports login success but cannot fetch user/account details — an inconsistent/degraded auth state.

## `agent models`

```
Loading models…
No models available for this account.
```
Exit code: `0`

## `agent --list-models`

```
Loading models…
No models available for this account.
```
Exit code: `0`

## `agent about` (extra context, not in the required command list but informative)

```
About Cursor CLI

CLI Version         2026.04.13-a9d7fb5
Model               Composer 2 Fast
Subscription Tier   Unknown
OS                  darwin (arm64)
Terminal            unknown
Shell               zsh
User Email          Not logged in
```

Note: `agent about` reports "User Email: Not logged in", directly contradicting `agent status`'s "Login successful!". This is evidence the local auth/session state is inconsistent or degraded, not a clean logged-out state.

## `agent --print` auth-gate check (free — failed before any billed turn)

Run once with `--output-format json` and once with `--output-format
stream-json` (both `--mode ask --trust`, read-only mode, in a scratch
directory) to determine whether `--print` fails deterministically before
billing when auth/model access is unusable. Both failed identically:

```
Authentication required
```
Exit code: non-zero (auth-gate rejection, no billed turn started)

**Observation:** this is a deterministic, user-readable, post-arg-parse
`--print` runtime failure — evidence for acceptance-bar item 5
(`model_availability_failures_deterministic_readable`), even though no
*authenticated* turn was observed. Beyond these two zero-cost calls,
`--print` was intentionally not invoked further, since a call that got past
the auth gate could trigger a billed turn against an account with no
confirmed model access.

## Model-selection arg/preflight layer (no further `--print` run; no money spent)

Since `--print` cannot be run without cost, model selection was probed by combining `--model <id>` with the read-only `--list-models` flag, which exits before any billed agent turn.

### `agent --list-models --model gpt-5` (default/plausible id)

```
Loading models…
No models available for this account.
```
Exit code: `0`

### `agent --list-models --model grok-4.5` (plausible id for the target model; public display name may differ from CLI id — unconfirmed)

```
Loading models…
No models available for this account.
```
Exit code: `0`

### `agent --list-models --model definitely-not-a-real-model` (deliberately invalid id)

```
Loading models…
No models available for this account.
```
Exit code: `0`

**Observation:** all three `--model` values produce byte-identical output and exit code. The CLI performs no client-side (arg-parse) validation of the `--model` value — an obviously-invalid id is accepted syntactically just like a plausible one. Because this account currently has zero models available, the "no models available for this account" response masks whatever server-side model-id validation exists; it is impossible from this probe to tell whether `grok-4.5` is a real CLI model id, or whether an invalid id would eventually produce a different error once a model becomes available.

## Auth usability determination

`auth_usable = false`.

Rationale: `agent status` claims "Login successful!" but cannot report user details, and `agent about` claims "Not logged in" — the two commands disagree. Regardless of that discrepancy, the decisive signal per the probe's own bar is `agent models` / `agent --list-models`, both of which report **"No models available for this account."** with zero models listed. A headless `--print` invocation could not plausibly authenticate and complete a paid turn in this state.

## Summary for backend decision (evidence only; final recommendation to be made in a later feature)

- Full flag surface confirmed, including `--print`, `--output-format stream-json`, `--model`, `--workspace`, `--sandbox`, `--trust`, `--worktree` — a direct-parser or ACP-backed backend both look structurally feasible from the flag surface alone.
- Headless auth is currently unusable on this machine (no models available for this account), which blocks exercising `--print --output-format stream-json` output shape entirely.
- `--model` argument is not client-side validated; real behavior (valid/invalid/unavailable model) can only be observed once auth is restored and at least one model is available.
- No commands in this probe spent money. `agent --print` was invoked twice
  (`json` and `stream-json`) and both failed free at the auth gate with
  `Authentication required` before any billed turn could start; beyond that,
  `--print` was intentionally not invoked further to avoid risking a billed
  turn against an account with no confirmed model access.
