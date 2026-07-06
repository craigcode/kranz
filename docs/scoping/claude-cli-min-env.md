# Minimal env for headless `claude -p --output-format stream-json`

Answers worker-sandboxing.md open question 1. Probed read-only against the
local `~/.claude` layout plus `claude --help` / `--bare` docs (claude CLI as
installed on this host, 2026-07-06). No secret values were read or recorded
here — only path/entry names.

## The two config roots

The CLI has **two** separate on-disk locations, and only one of them moves
with `CLAUDE_CONFIG_DIR`:

- **`CLAUDE_CONFIG_DIR`** (default `$HOME/.claude`) — the CLI's own
  directory: credentials, settings, caches, session state, plugins, skills,
  project history. Setting this env var relocates the *entire* directory;
  the CLI reads and writes everything below from there instead of
  `~/.claude`.
- **`$HOME/.claude.json`** — a separate top-level file (project trust state,
  onboarding flags, some account metadata) that sits next to `.claude`, not
  inside it. It is keyed off plain `$HOME`, **not** `CLAUDE_CONFIG_DIR` — a
  scratch-HOME strategy that only relocates `CLAUDE_CONFIG_DIR` and leaves
  `$HOME` untouched will still hit the real `~/.claude.json`. Getting
  isolation right requires relocating both: point `CLAUDE_CONFIG_DIR` at the
  scratch config dir *and* point `HOME` at a scratch home so `~/.claude.json`
  resolves inside the sandbox too.

## Minimal entry set (relative to `CLAUDE_CONFIG_DIR`)

To authenticate and run headless, the CLI needs at minimum:

| Entry | Purpose | Required? |
|---|---|---|
| `.credentials.json` | OAuth token (`claudeAiOauth.{accessToken,refreshToken,expiresAt}`) — file-based auth | Required unless using Keychain (macOS) or `ANTHROPIC_API_KEY` |
| `settings.json` | User settings: model, effort level, permissions, theme, etc. | Optional — CLI runs with defaults if absent |

Everything else under `~/.claude` (`projects/`, `sessions/`, `history.jsonl`,
`shell-snapshots/`, `ide/`, `plugins/`, `skills/`, `cache/`, `daemon/`,
`telemetry/`, `file-history/`, `backups/`, `debug/`, `paste-cache/`,
`session-env/`, `tasks/`, `mcp-needs-auth-cache.json`, `.last-cleanup`,
`.last-update-result.json`, `stats-cache.json`, `statusline-command.sh`) is
either write-created-on-demand state, IDE/plugin integration, or
non-essential telemetry/cache — the CLI runs headless (`-p
--output-format stream-json`) without any of it present. A worker's scratch
config dir does not need to seed these; it only needs to *tolerate* the CLI
creating them fresh (they're all writable, not read-required).

`$HOME/.claude.json` itself is not strictly required for a first-run
headless session (the CLI will create a default one), but if project-trust
prompts should be pre-answered for a given worktree path, that file is where
that state lives.

## Platform distinction: file vs Keychain credentials (macOS)

On macOS, the interactive CLI can store the OAuth token in the **Keychain**
instead of `.credentials.json`. This matters for scratch-HOME isolation:

- **Keychain-stored auth is HOME-independent.** It is looked up by service
  name via the macOS Keychain API, not by reading a path under
  `CLAUDE_CONFIG_DIR`/`HOME`. A worker given a scratch HOME/config dir will
  **still authenticate successfully** via Keychain even though no
  credentials file was seeded — because the lookup never touches the
  filesystem path at all. This is easy to mistake for "the scratch env
  carried the right files" when actually it carried nothing and Keychain
  filled the gap.
- **File-based auth (`.credentials.json`) follows `CLAUDE_CONFIG_DIR`/HOME.**
  When the token lives in `.credentials.json` (as it does on this host —
  confirmed present, JSON, containing only the `claudeAiOauth` key with no
  Keychain entry checked here), a scratch config dir *must* carry a copy of
  this file (or a variant with fresh/valid tokens) or the headless session
  will fail to authenticate.
- `--bare` mode explicitly documents this split: bare mode restricts auth to
  `ANTHROPIC_API_KEY` or `apiKeyHelper` only and states plainly that "OAuth
  and keychain are never read" in that mode — confirming Keychain and the
  OAuth-file path are the two non-API-key auth sources the CLI supports.

Practical implication for worker env hygiene: **do not assume the absence of
a credentials file means the worker can't auth** on macOS — check Keychain
too. The safest, most portable approach for a scratch worker HOME is to
carry an explicit `.credentials.json` copy (this repo's chosen minimal set,
see `crates/engine/src/backend_claude.rs::claude_min_config_entries`),
because that method works identically whether or not Keychain is present,
and is the only method available on Linux CI runners (no Keychain there at
all).

## `CLAUDE_CONFIG_DIR` relocation semantics

- Setting `CLAUDE_CONFIG_DIR=/some/scratch/dir` makes the CLI treat that
  path exactly as it would treat `$HOME/.claude` — all reads/writes for
  credentials, settings, caches, sessions, etc. go there instead.
- It does **not** relocate `$HOME/.claude.json` (see above) — that still
  resolves against `$HOME`.
- The directory need not pre-exist with every entry populated; the CLI
  creates missing subdirectories/files it needs at runtime. Only
  credentials must be pre-seeded for a non-interactive first run to
  authenticate without a login prompt.

## Bottom line for the next feature (scratch-HOME seeding)

A worker scratch environment needs:
1. A scratch dir set as `CLAUDE_CONFIG_DIR`, seeded with a copy of
   `.credentials.json` (entry name enumerated in
   `crates/engine/src/backend_claude.rs`).
2. A scratch dir set as `HOME` (so `~/.claude.json` resolves inside the
   sandbox, not the operator's real one).
3. On macOS, if Keychain-based auth is in play instead of a credentials
   file, scratch-HOME isolation alone will not block auth (Keychain lookup
   ignores HOME) — that's a feature for auth continuity, but a gap for
   containment purposes documented here, not silently assumed away.
