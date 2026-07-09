# Cursor CLI (`agent`) preflight probe log

## Historical: 2026-04-13 probe (binary `2026.04.13-a9d7fb5`)

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

## 2026-07-09 re-verification (binary `2026.07.08-0c04a8a`)

Binary: `~/.local/bin/agent`
CLI version: `2026.07.08-0c04a8a` (previously `2026.04.13-a9d7fb5` — **changed**)
Probe date: 2026-07-09
Scope: free, read-only re-verification only. `agent --print` was **not** run in
this pass (it is out of scope for this feature; the prior section's two
zero-cost `--print` auth-gate calls against the *old* binary/account state are
kept as historical evidence and are not re-run here).
All output below is verbatim except email/token/account identifiers, which are replaced with `<redacted>`.

### `agent --version`

```
2026.07.08-0c04a8a
```
Exit code: `0`

### `agent --help`

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
  --mode <mode>                Start in the given execution mode. plan:
                               read-only/planning (analyze, propose plans, no
                               edits). ask: Q&A style for explanations and
                               questions (read-only). (choices: "plan", "ask")
  --plan                       Start in plan mode (shorthand for --mode=plan).
                               (default: false)
  --resume [chatId]            Select a session to resume (default: false)
  --continue                   Continue previous session (default: false)
  --model <model>              Model to use (e.g., gpt-5, sonnet-4-thinking).
                               Parameterized models accept quoted bracket
                               overrides, e.g.
                               'claude-opus-4-8[context=1m,effort=high,fast=false]'
  --list-models                List available models and exit (default: false)
  -f, --force                  Force allow commands unless explicitly denied
                               (default: false)
  --yolo                       Alias for --force (Run Everything) (default:
                               false)
  --auto-review                Use Auto-review (Smart Auto): a server classifier
                               auto-runs safe tool calls and prompts for the
                               rest (default: false)
  --sandbox <mode>             Explicitly enable or disable sandbox mode
                               (overrides config) (choices: "enabled",
                               "disabled")
  --approve-mcps               Automatically approve all MCP servers (default:
                               false)
  --trust                      Trust the current workspace without prompting
                               (only works with --print/headless mode) (default:
                               false)
  --workspace <path-or-name>   Workspace directory or saved workspace name to
                               use (defaults to current working directory)
  --add-dir <path>             Add an additional workspace root directory (can
                               be specified multiple times)
  --plugin-dir <path>          Load a local plugin directory (can be specified
                               multiple times)
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
  worker [options]             Start a private cloud worker that connects to
                               Cursor to run agents in your environment
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
Exit code: `0`

### Flag-surface diff vs. the 2026-04-13 probe (do not trust the merged list — re-derived from this `--help` output)

- **Removed:** `-c`/`--cloud` (`Start in cloud mode (open composer picker on launch)`) — no longer present anywhere in `--help`.
- **Added:** `--auto-review` (server-classifier auto-run of safe tool calls), `--add-dir <path>` (additional workspace root, repeatable), `--plugin-dir <path>` (load a local plugin directory, repeatable).
- **Changed description only (same flag name, not a surface change):** `--workspace` now documents `<path-or-name>` (accepts a saved workspace name, not just a path); `--model` now documents parameterized/bracket overrides (e.g. `claude-opus-4-8[context=1m,effort=high,fast=false]`).
- **New top-level command (not a flag):** `worker [options]` — "Start a private cloud worker that connects to Cursor to run agents in your environment". Everything else in `Commands:` is unchanged.
- All other flags from the 2026-04-13 list are unchanged: `--api-key`, `--header`, `--print`, `--output-format`, `--stream-partial-output`, `--mode`, `--plan`, `--resume`, `--continue`, `--model`, `--list-models`, `--force`/`--yolo`, `--sandbox`, `--approve-mcps`, `--trust`, `--workspace`, `--worktree`, `--worktree-base`, `--skip-worktree-setup`.

### `agent status`

```
✓ Logged in as <redacted>
```
Exit code: `0`

### `agent about`

```
About Cursor CLI

CLI Version         2026.07.08-0c04a8a
Model               Auto
Subscription Tier   Ultra
OS                  darwin (arm64)
Terminal            unknown
Shell               zsh
User Email          <redacted>
```
Exit code: `0`

**Note:** unlike the 2026-04-13 probe, `agent status` and `agent about` now **agree** — both report a logged-in, identifiable account (`Subscription Tier: Ultra`), resolving the prior status/about inconsistency.

### `agent models`

```
Available models

auto - Auto (current, default)
gpt-5.3-codex-low - Codex 5.3 Low
gpt-5.3-codex-low-fast - Codex 5.3 Low Fast
gpt-5.3-codex - Codex 5.3
gpt-5.3-codex-fast - Codex 5.3 Fast
gpt-5.3-codex-high - Codex 5.3 High
gpt-5.3-codex-high-fast - Codex 5.3 High Fast
gpt-5.3-codex-xhigh - Codex 5.3 Extra High
gpt-5.3-codex-xhigh-fast - Codex 5.3 Extra High Fast
gpt-5.2 - GPT-5.2
gpt-5.2-codex-low - Codex 5.2 Low
gpt-5.2-codex-low-fast - Codex 5.2 Low Fast
gpt-5.2-codex - Codex 5.2
gpt-5.2-codex-fast - Codex 5.2 Fast
gpt-5.2-codex-high - Codex 5.2 High
gpt-5.2-codex-high-fast - Codex 5.2 High Fast
gpt-5.2-codex-xhigh - Codex 5.2 Extra High
gpt-5.2-codex-xhigh-fast - Codex 5.2 Extra High Fast
gpt-5.1-codex-max-low - Codex 5.1 Max Low
gpt-5.1-codex-max-low-fast - Codex 5.1 Max Low Fast
gpt-5.1-codex-max-medium - Codex 5.1 Max
gpt-5.1-codex-max-medium-fast - Codex 5.1 Max Medium Fast
gpt-5.1-codex-max-high - Codex 5.1 Max High
gpt-5.1-codex-max-high-fast - Codex 5.1 Max High Fast
gpt-5.1-codex-max-xhigh - Codex 5.1 Max Extra High
gpt-5.1-codex-max-xhigh-fast - Codex 5.1 Max Extra High Fast
grok-4.5-xhigh - Cursor Grok 4.5
grok-4.5-fast-xhigh - Cursor Grok 4.5 Fast
composer-2.5 - Composer 2.5
claude-opus-4-8-thinking-high - Opus 4.8 1M Thinking
claude-opus-4-8-thinking-high-fast - Opus 4.8 1M Thinking Fast
gpt-5.6-sol-high - GPT-5.6 Sol 1M High
gpt-5.6-sol-high-fast - GPT-5.6 Sol High Fast
gpt-5.6-sol-xhigh - GPT-5.6 Sol 1M Extra High
gpt-5.6-sol-xhigh-fast - GPT-5.6 Sol Extra High Fast
gpt-5.5-high - GPT-5.5 1M High
gpt-5.5-high-fast - GPT-5.5 High Fast
claude-fable-5-thinking-high - Fable 5 1M Thinking (NO ZDR)
claude-fable-5-thinking-xhigh - Fable 5 1M Extra High Thinking (NO ZDR)
claude-opus-4-7-thinking-high - Opus 4.7 1M High Thinking
claude-opus-4-7-thinking-high-fast - Opus 4.7 1M High Thinking Fast
gpt-5.4-high - GPT-5.4 1M High
gpt-5.4-high-fast - GPT-5.4 High Fast
grok-4.5-medium - Cursor Grok 4.5 Low
grok-4.5-fast-medium - Cursor Grok 4.5 Low Fast
grok-4.5-high - Cursor Grok 4.5 Medium
grok-4.5-fast-high - Cursor Grok 4.5 Medium Fast
composer-2.5-fast - Composer 2.5 Fast
claude-opus-4-8-low - Opus 4.8 1M Low
claude-opus-4-8-low-fast - Opus 4.8 1M Low Fast
claude-opus-4-8-medium - Opus 4.8 1M Medium
claude-opus-4-8-medium-fast - Opus 4.8 1M Medium Fast
claude-opus-4-8-high - Opus 4.8 1M
claude-opus-4-8-high-fast - Opus 4.8 1M Fast
claude-opus-4-8-xhigh - Opus 4.8 1M Extra High
claude-opus-4-8-xhigh-fast - Opus 4.8 1M Extra High Fast
claude-opus-4-8-max - Opus 4.8 1M Max
claude-opus-4-8-max-fast - Opus 4.8 1M Max Fast
claude-opus-4-8-thinking-low - Opus 4.8 1M Low Thinking
claude-opus-4-8-thinking-low-fast - Opus 4.8 1M Low Thinking Fast
claude-opus-4-8-thinking-medium - Opus 4.8 1M Medium Thinking
claude-opus-4-8-thinking-medium-fast - Opus 4.8 1M Medium Thinking Fast
claude-opus-4-8-thinking-xhigh - Opus 4.8 1M Extra High Thinking
claude-opus-4-8-thinking-xhigh-fast - Opus 4.8 1M Extra High Thinking Fast
claude-opus-4-8-thinking-max - Opus 4.8 1M Max Thinking
claude-opus-4-8-thinking-max-fast - Opus 4.8 1M Max Thinking Fast
gpt-5.6-sol-none - GPT-5.6 Sol 1M None
gpt-5.6-sol-none-fast - GPT-5.6 Sol None Fast
gpt-5.6-sol-low - GPT-5.6 Sol 1M Low
gpt-5.6-sol-low-fast - GPT-5.6 Sol Low Fast
gpt-5.6-sol-medium - GPT-5.6 Sol 1M
gpt-5.6-sol-medium-fast - GPT-5.6 Sol Fast
gpt-5.6-sol-max - GPT-5.6 Sol 1M Max
gpt-5.6-sol-max-fast - GPT-5.6 Sol Max Fast
gpt-5.5-none - GPT-5.5 1M None
gpt-5.5-none-fast - GPT-5.5 None Fast
gpt-5.5-low - GPT-5.5 1M Low
gpt-5.5-low-fast - GPT-5.5 Low Fast
gpt-5.5-medium - GPT-5.5 1M
gpt-5.5-medium-fast - GPT-5.5 Fast
gpt-5.5-extra-high - GPT-5.5 1M Extra High
gpt-5.5-extra-high-fast - GPT-5.5 Extra High Fast
claude-fable-5-low - Fable 5 1M Low (NO ZDR)
claude-fable-5-medium - Fable 5 1M Medium (NO ZDR)
claude-fable-5-high - Fable 5 1M (NO ZDR)
claude-fable-5-xhigh - Fable 5 1M Extra High (NO ZDR)
claude-fable-5-max - Fable 5 1M Max (NO ZDR)
claude-fable-5-thinking-low - Fable 5 1M Low Thinking (NO ZDR)
claude-fable-5-thinking-medium - Fable 5 1M Medium Thinking (NO ZDR)
claude-fable-5-thinking-max - Fable 5 1M Max Thinking (NO ZDR)
claude-sonnet-5-low - Sonnet 5 1M Low
claude-sonnet-5-medium - Sonnet 5 1M Medium
claude-sonnet-5-high - Sonnet 5 1M
claude-sonnet-5-xhigh - Sonnet 5 1M Extra High
claude-sonnet-5-max - Sonnet 5 1M Max
claude-sonnet-5-thinking-low - Sonnet 5 1M Low Thinking
claude-sonnet-5-thinking-medium - Sonnet 5 1M Medium Thinking
claude-sonnet-5-thinking-high - Sonnet 5 1M Thinking
claude-sonnet-5-thinking-xhigh - Sonnet 5 1M Extra High Thinking
claude-sonnet-5-thinking-max - Sonnet 5 1M Max Thinking
gpt-5.6-terra-none - GPT-5.6 Terra 1M None
gpt-5.6-terra-none-fast - GPT-5.6 Terra None Fast
gpt-5.6-terra-low - GPT-5.6 Terra 1M Low
gpt-5.6-terra-low-fast - GPT-5.6 Terra Low Fast
gpt-5.6-terra-medium - GPT-5.6 Terra 1M
gpt-5.6-terra-medium-fast - GPT-5.6 Terra Fast
gpt-5.6-terra-high - GPT-5.6 Terra 1M High
gpt-5.6-terra-high-fast - GPT-5.6 Terra High Fast
gpt-5.6-terra-xhigh - GPT-5.6 Terra 1M Extra High
gpt-5.6-terra-xhigh-fast - GPT-5.6 Terra Extra High Fast
gpt-5.6-terra-max - GPT-5.6 Terra 1M Max
gpt-5.6-terra-max-fast - GPT-5.6 Terra Max Fast
claude-4.6-sonnet-medium - Sonnet 4.6 1M
claude-4.6-sonnet-medium-thinking - Sonnet 4.6 1M Thinking
claude-opus-4-7-low - Opus 4.7 1M Low
claude-opus-4-7-low-fast - Opus 4.7 1M Low Fast
claude-opus-4-7-medium - Opus 4.7 1M Medium
claude-opus-4-7-medium-fast - Opus 4.7 1M Medium Fast
claude-opus-4-7-high - Opus 4.7 1M High
claude-opus-4-7-high-fast - Opus 4.7 1M High Fast
claude-opus-4-7-xhigh - Opus 4.7 1M
claude-opus-4-7-xhigh-fast - Opus 4.7 1M Fast
claude-opus-4-7-max - Opus 4.7 1M Max
claude-opus-4-7-max-fast - Opus 4.7 1M Max Fast
claude-opus-4-7-thinking-low - Opus 4.7 1M Low Thinking
claude-opus-4-7-thinking-low-fast - Opus 4.7 1M Low Thinking Fast
claude-opus-4-7-thinking-medium - Opus 4.7 1M Medium Thinking
claude-opus-4-7-thinking-medium-fast - Opus 4.7 1M Medium Thinking Fast
claude-opus-4-7-thinking-xhigh - Opus 4.7 1M Thinking
claude-opus-4-7-thinking-xhigh-fast - Opus 4.7 1M Thinking Fast
claude-opus-4-7-thinking-max - Opus 4.7 1M Max Thinking
claude-opus-4-7-thinking-max-fast - Opus 4.7 1M Max Thinking Fast
gpt-5.4-low - GPT-5.4 1M Low
gpt-5.4-medium - GPT-5.4 1M
gpt-5.4-medium-fast - GPT-5.4 Fast
gpt-5.4-xhigh - GPT-5.4 1M Extra High
gpt-5.4-xhigh-fast - GPT-5.4 Extra High Fast
claude-4.6-opus-high - Opus 4.6 1M
claude-4.6-opus-max - Opus 4.6 1M Max
claude-4.6-opus-high-thinking - Opus 4.6 1M Thinking
claude-4.6-opus-max-thinking - Opus 4.6 1M Max Thinking
claude-4.5-opus-high - Opus 4.5
claude-4.5-opus-high-thinking - Opus 4.5 Thinking
gpt-5.2-low - GPT-5.2 Low
gpt-5.2-low-fast - GPT-5.2 Low Fast
gpt-5.2-fast - GPT-5.2 Fast
gpt-5.2-high - GPT-5.2 High
gpt-5.2-high-fast - GPT-5.2 High Fast
gpt-5.2-xhigh - GPT-5.2 Extra High
gpt-5.2-xhigh-fast - GPT-5.2 Extra High Fast
gpt-5.6-luna-none - GPT-5.6 Luna 1M None
gpt-5.6-luna-none-fast - GPT-5.6 Luna None Fast
gpt-5.6-luna-low - GPT-5.6 Luna 1M Low
gpt-5.6-luna-low-fast - GPT-5.6 Luna Low Fast
gpt-5.6-luna-medium - GPT-5.6 Luna 1M
gpt-5.6-luna-medium-fast - GPT-5.6 Luna Fast
gpt-5.6-luna-high - GPT-5.6 Luna 1M High
gpt-5.6-luna-high-fast - GPT-5.6 Luna High Fast
gpt-5.6-luna-xhigh - GPT-5.6 Luna 1M Extra High
gpt-5.6-luna-xhigh-fast - GPT-5.6 Luna Extra High Fast
gpt-5.6-luna-max - GPT-5.6 Luna 1M Max
gpt-5.6-luna-max-fast - GPT-5.6 Luna Max Fast
gemini-3.1-pro - Gemini 3.1 Pro
gpt-5.4-mini-none - GPT-5.4 Mini None
gpt-5.4-mini-low - GPT-5.4 Mini Low
gpt-5.4-mini-medium - GPT-5.4 Mini
gpt-5.4-mini-high - GPT-5.4 Mini High
gpt-5.4-mini-xhigh - GPT-5.4 Mini Extra High
gpt-5.4-nano-none - GPT-5.4 Nano None
gpt-5.4-nano-low - GPT-5.4 Nano Low
gpt-5.4-nano-medium - GPT-5.4 Nano
gpt-5.4-nano-high - GPT-5.4 Nano High
gpt-5.4-nano-xhigh - GPT-5.4 Nano Extra High
claude-4.5-sonnet - Sonnet 4.5
claude-4.5-sonnet-thinking - Sonnet 4.5 Thinking
gpt-5.1-low - GPT-5.1 Low
gpt-5.1 - GPT-5.1
gpt-5.1-high - GPT-5.1 High
gemini-3-flash - Gemini 3 Flash
gemini-3.5-flash - Gemini 3.5 Flash
gpt-5.1-codex-mini-low - Codex 5.1 Mini Low
gpt-5.1-codex-mini - Codex 5.1 Mini
gpt-5.1-codex-mini-high - Codex 5.1 Mini High
claude-4-sonnet - Sonnet 4
claude-4-sonnet-thinking - Sonnet 4 Thinking
gpt-5-mini - GPT-5 Mini
kimi-k2.7-code - Kimi K2.7 Code
glm-5.2-high - GLM 5.2
glm-5.2-max - GLM 5.2 Max

Tip: use --model <id> (or /model <id> in interactive mode) to switch. Parameterized models also accept quoted overrides, e.g. --model 'claude-opus-4-8[context=1m,effort=high,fast=false]'.
```
Exit code: `0`

### `agent --list-models`

Byte-identical output and exit code (`0`) to `agent models` above.

### Auth usability determination (2026-07-09)

`auth_usable = true`.

Rationale: `agent status` and `agent about` now agree — logged in as
`<redacted>`, `Subscription Tier: Ultra` — and `agent models` /
`agent --list-models` return a large, populated model catalog (not "No models
available for this account."). This **reverses** the 2026-04-13 determination
(`auth_usable = false`). Auth is usable in this worker's environment as of
this probe. This is the gate for the billed features that follow; those
features may now proceed to exercise `--print` under their own scoped,
explicitly-authorized billing.

### Grok tier id-vs-display-name off-by-one trap

The `agent models` / `agent --list-models` catalog exposes a real, observed
off-by-one between a Grok tier's CLI **id** and its **display name** — the id
suffix names one severity/effort level, while the printed display name for
that same line names the tier *below* it:

```
grok-4.5-medium - Cursor Grok 4.5 Low
grok-4.5-high   - Cursor Grok 4.5 Medium
```

i.e. `grok-4.5-medium` displays as "Grok 4.5 **Low**", and `grok-4.5-high`
displays as "Grok 4.5 **Medium**" — the id's tier word is one step higher than
what the human-readable name shows for that same catalog line. (`grok-4.5-xhigh`
displays as "Cursor Grok 4.5" with no tier suffix at all, and `grok-4.5-fast-xhigh`
similarly.) Any backend or UI that renders a *human-readable* label by
naively re-deriving it from the `--model` id string (rather than reading the
catalog's own display-name field) will misreport the tier by one level for
these Grok entries. Always source the display name from the catalog, never
infer it from the id.

### Summary for backend decision — 2026-07-09 update

- CLI version has moved from `2026.04.13-a9d7fb5` to `2026.07.08-0c04a8a`.
- Auth is now usable in this environment (`auth_usable = true`), reversing the
  prior blocker. The account is logged in, `about`/`status` agree, and the
  model catalog is populated (~190 entries).
- Flag surface changed since the last probe: `--cloud` was removed; `--auto-review`,
  `--add-dir`, and `--plugin-dir` were added; a new `worker` subcommand appeared.
  All flags load-bearing for a direct-parser/ACP backend decision
  (`--print`, `--output-format`, `--model`, `--workspace`, `--sandbox`,
  `--trust`, `--worktree`) are unchanged and still present.
- `agent --print` was intentionally **not** run in this feature (out of scope —
  free/read-only re-verification only). The `--print` auth-gate/output-shape
  questions the 2026-04-13 probe left unresolved remain unresolved by this
  pass; they can now be pursued by a later, explicitly-billed feature since
  auth is confirmed usable.
- The grok id-vs-display-name off-by-one (documented above) is new,
  previously-unrecorded information relevant to any UI/backend that surfaces
  model display names.

## 2026-07-09 write-capable capture (billed feature, binary `2026.07.08-0c04a8a`)

Binary: `~/.local/bin/agent`. Gate check re-confirmed live before spending money:
`agent status` -> `✓ Logged in as <redacted>` (exit 0) -- auth usable, matching
this file's prior `auth_usable = true` determination. Proceeded to run
`--print` for the first time in this probe's history.

All work below ran against a **throwaway** git repo created with
`mktemp -d` + `git init` under `$TMPDIR`, never against this repository, and
`--worktree` was never passed.

### Command

```
agent --print --output-format stream-json --force --trust \
  --workspace <tmpdir> --model gpt-5.6-luna-low \
  "Create hello.txt containing hi, then run cat hello.txt"
```

Exit code: `0`. Output captured as 10 newline-delimited JSON events, redacted
(session/call/tool-call/request/model-call ids, `$HOME` path prefix, and any
email-shaped strings replaced with `<redacted>`; JSON structure and event
`type`/`subtype` values left intact) and committed verbatim as
`docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl`.

**Event sequence:** `system/init` -> `user` -> `tool_call` started/completed
(`shellToolCall`, command `ls` -- the model's own workspace-verification step)
-> `tool_call` started/completed (`editToolCall`, writes `hello.txt`) ->
`tool_call` started/completed (`readToolCall`, reads `hello.txt` back --
the model satisfied "run a command and show me the output" via its Read tool
rather than literally shelling out to `cat`) -> `assistant` (text summarizing
the created file) -> `result` (`subtype: success`, carries
`usage: {inputTokens, outputTokens, cacheReadTokens, cacheWriteTokens}`; no
separate dollar-cost field was observed on the wire in this CLI version).

This satisfies the fixture acceptance bar: assistant text present, at least
one tool-use event present (three, in fact: shell/edit/read), at least one
tool-result event present (three matching `completed` events, each carrying a
`result.success` payload), and a terminal event carrying token usage.

**Temp-repo `git diff` after this run:** `hello.txt` appears as an untracked
file (`git status`); `git diff` against the tracked tree is empty -- the
agent's edit tool wrote the file to disk but did not stage or commit it, and
the pre-existing tracked `README.md` was untouched.

### Deliberately-failing command

Same workspace, same model, prompt: `"Run the shell command 'exit 1' and
report what happened. Do not create or edit any files."` Exit code: `0` (the
CLI invocation itself succeeds; only the shell command inside it fails).
Stream shape: `system/init` -> `user` -> ~78 `thinking`/`delta` events (this
model reasons before acting) -> `thinking/completed` -> `tool_call`
started/completed (`shellToolCall`, command `exit 1`) -> `assistant` (text:
reports exit code 1, no output, no files changed) -> `result/success`.
Notably, the failing shell command's `tool_call/completed` event nests its
outcome under `result.failure` (`{command:"exit 1", exitCode:1, stdout:"",
stderr:"", aborted:false}`) rather than `result.success` -- a distinct,
structurally-typed failure variant of the same tool-result event, not an
error thrown up to the top level. The top-level `result` event still reports
`is_error:false` and `subtype:"success"`, since the *agent run* succeeded
even though the *shell command it ran* did not. Temp-repo `git diff` after:
unchanged (only the untracked `hello.txt` from the previous run remains).
Raw output was not committed as a separate fixture per spec; its shape is
recorded here only.

### No-op prompt

Same workspace, same model, prompt: `"Do nothing. Do not use any tools. Just
reply with the single word: OK"`. Exit code: `0`. Stream shape: `system/init`
-> `user` -> `assistant` (text: `"OK"`) -> `result/success` (usage tokens) --
no `tool_call` events at all, confirming the stream omits tool-use/tool-result
events entirely when the model performs no tool calls. Temp-repo `git diff`
after: unchanged. Raw output was not committed as a separate fixture per
spec; its shape is recorded here only.

### Summary

- `probe-result.json.fixture` now points at
  `docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl`, resolving
  the `fixture_offline_parser_test` acceptance-bar item that was previously
  unresolved (see `probe-result.json`'s `acceptance_bar` and new
  `fixture_capture`/`prompt_shape_matrix` fields for full detail).
  `tool_use_and_results_observable` and `usage_cost_on_wire_or_priceable` are
  now resolved for the token-usage half (usage is present; no separate cost
  field was seen on the wire in this CLI version). `cwd_workspace_worktree_isolation`
  is now exercised: `--workspace <tmpdir>` correctly scoped all file/shell
  activity to the throwaway repo.
- Three billed `--print` calls were made in this feature (write-capable,
  failing-command, no-op), each a single short turn against a throwaway
  workspace -- no money was spent against this repository or any file outside
  `$TMPDIR`.
