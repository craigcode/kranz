//! Cursor agent backend: drives the Cursor CLI headless
//! (`agent --print --output-format stream-json`).
//!
//! Ground truth is the decided route in `docs/scoping/cursor-cli-backend.md`
//! (decision revised 2026-07-09: `direct-parser`) and the committed wire
//! fixture `docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl`,
//! captured from a real write-capable run. The parser is built against THAT
//! shape first; the event mapping table in the scoping doc's implementation
//! brief is the authority for every arm below.
//!
//! This module is single-shot only: `--resume` exists on the CLI but was
//! never exercised by the probe, and there is no streaming-input mode, so
//! [`CursorSession::send_user_message`] and a `resume`d [`SessionSpec`] are
//! both rejected at the seam rather than translated into flags.
//!
//! Several `SessionSpec` fields are claude-isms with no cursor equivalent and
//! are deliberately ignored when building argv: `json_schema`,
//! `max_budget_usd`, `resume`, `permission_mode`, `allowed_tools` /
//! `disallowed_tools`, `tools`, `settings_json`, and `effort` — the CLI has
//! no `--effort` flag, and the `--model` bracket-override syntax
//! (`'model[effort=high]'`) is documented only for parameterized models and
//! was never probed, so effort is NOT munged into the model id.
//!
//! Permission posture (probe item 6, observed): read-only sessions map to
//! `--mode ask` (turn-level read-only, the validator role), writable sessions
//! to default mode + `--force` (writes/shell proceed unprompted, the worker
//! role; `--yolo` is only a documented alias of `--force`, never separately
//! live-tested, so `--force` is the emitted spelling). `--trust` rides every
//! session: headless `--print` otherwise prompts for workspace trust. The
//! CLI's own `--sandbox` flag is NEVER emitted — the probe observed it make
//! no difference to outbound network access or writes outside `--workspace`,
//! so it is not a kranz isolation boundary. The no-push/no-publish/
//! no-main-write invariants therefore hold exactly the way the scoping doc
//! prescribes: turn-level read-only modes for validators, throwaway
//! `--workspace` directories, scoped credentials, and (when requested) the
//! engine's external process sandbox — never this flag. Because the resolved
//! OS sandbox is not applied by this backend,
//! `BackendKind::Cursor::supports_sandbox_enforcement` is `false` and
//! `config::validate` fails closed on enforced-sandbox pairings.
//!
//! Auth posture (probe, verified): the CLI's login state does not survive a
//! relocated `$HOME` (`HOME=/tmp/x agent status` reports "Not logged in"),
//! and on macOS the credential itself is Keychain-backed (the sandboxed probe
//! crashed with `SecItemCopyMatching failed -50`) — no credential FILE exists
//! under `~/.cursor` to copy. The scratch-HOME seed therefore carries only
//! the small account-identity/CLI-config files ([`CURSOR_SEED_ENTRIES`]),
//! never transcripts or caches, and the one ambient var a cursor session may
//! authenticate with — `CURSOR_API_KEY`, the scoping doc's sanctioned
//! headless channel — is injected explicitly, never the ambient set. A
//! session whose seed+key is insufficient fails auth loudly
//! ("Authentication required", pre-billing), which the stream watcher turns
//! into an honest configuration-style failure rather than a retryable one
//! (probe item 5).
//!
//! Hook-status lane (ticket `agent-hooks-status-signals`,
//! [`crate::hook_status`]): when the runner seeds
//! [`SessionSpec::hook_status`] (mission config `hookStatus.enabled` AND
//! this hook-capable backend), [`CursorBackend::start`] installs the lane
//! into the session-private HOME BEFORE spawning: `<home>/.cursor/
//! hooks.json` (the CLI's documented user-level hook file — verified
//! 2026-08-06 against https://cursor.com/docs/hooks: `version: 1` with
//! per-event `[{command, timeout}]` handlers, payloads delivered on stdin,
//! exit 0 = ok / 2 = block / other = fail-open; there is NO HTTP hook
//! type, so delivery to kranz's endpoint is the installed
//! `kranz hook-status` relay) plus the per-session spec file the relay
//! reads. The install NEVER touches the workspace's tracked
//! `.cursor/hooks.json` — the project-level file is the operator's own,
//! and mutating it as a side effect of spawning would be exactly the
//! silent tracked-tree write the ticket forbids. Every install failure
//! degrades to NO lane (loud warning, ordinary session): hooks are
//! non-authoritative observability, and the lane disabled is the
//! byte-identical default. `SessionSpec::hook_status` is the one field
//! this backend consumes beyond argv/env; the claude-ism fields stay
//! ignored as documented above.

use crate::backend::{
    AgentBackend, AgentEvent, AgentSession, PromptMode, SessionExit, SessionSpec,
};
#[cfg(unix)]
use crate::backend_claude::kill_group;
#[cfg(windows)]
use crate::backend_claude::win_job;
use crate::cost;
use crate::error::{EngineError, Result};
use crate::stream_bounds::{drain_to_tail, BoundedLines, STDERR_TAIL_CAP};
use crate::types::TokenUsage;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::process::{Child, ChildStdout};
use tokio::task::JoinHandle;

/// Max characters kept in tool-use / tool-result summaries.
const SUMMARY_MAX_CHARS: usize = 200;
/// Max characters of captured stderr included in failure messages.
const STDERR_TAIL_CHARS: usize = 500;

/// The ambient var a cursor session may authenticate with (injected
/// explicitly, never via ambient inheritance). Confirmed by `agent --help`:
/// `--api-key <key>` "(can also use CURSOR_API_KEY env var)".
const CURSOR_AUTH_ENV: &str = "CURSOR_API_KEY";

/// The minimal `~/.cursor` state seeded into a session's scratch HOME:
/// `cli-config.json` carries the account identity (`authInfo`) and CLI
/// config, `agent-cli-state.json` the CLI's small state file. Both are a few
/// KB. The interactive-login CREDENTIAL is not here — on macOS it is
/// Keychain-backed (see module docs) — so this seed preserves account/config
/// context but cannot guarantee auth; `CURSOR_API_KEY` is the reliable
/// headless channel. Deliberately excluded: `chats/` (per-session
/// transcripts, hundreds of MB), `ai-tracking/`, `projects/`, `plugins/`,
/// `extensions/`, `prompt_history.json`, `statsig-cache.json` (unbounded or
/// per-session state).
const CURSOR_SEED_ENTRIES: &[&str] = &["cli-config.json", "agent-cli-state.json"];

/// Plain-text (non-JSON) stdout/stderr phrases the CLI emits when it rejects
/// a session BEFORE any billed turn starts (probe item 5): an invalid or
/// unentitled `--model` id exits 1 with `Cannot use this model: <id>.
/// Available models: ...`, and an unauthenticated `--print` fails with
/// `Authentication required`. Both are deterministic, user-readable,
/// pre-billing rejections — the session watcher surfaces them as
/// configuration-style failures (fix the model id / authenticate), never as
/// retryable transport errors.
const PRE_BILLING_FAILURE_PHRASES: &[&str] = &["cannot use this model", "authentication required"];

/// The cleared environment one `agent` session spawns with, mirroring
/// [`crate::backend_kimi`]'s seeding contract: a spec carrying a relocated
/// scratch `HOME` (worker relocation) is used verbatim; otherwise a fresh
/// per-session scratch HOME is seeded with [`CURSOR_SEED_ENTRIES`] so the
/// CLI's account-identity/config context survives. Seeding failure degrades
/// to an empty scratch home — the session then fails auth loudly rather than
/// silently inheriting the operator's real HOME. `CURSOR_API_KEY` is injected
/// explicitly when set (logged name-only).
fn cursor_child_env(spec: &SessionSpec) -> std::collections::HashMap<String, String> {
    if spec.env.contains_key("HOME") {
        return crate::agent_env::agent_session_env(
            &spec.env,
            &spec.session_id,
            Some(CURSOR_AUTH_ENV),
        );
    }
    let real_home = std::env::var_os("HOME").map(PathBuf::from);
    let scratch_root = crate::backend_claude::scratch_home_root(&spec.session_id);
    match seed_cursor_scratch_home(&scratch_root, real_home.as_deref()) {
        Ok(home) => {
            tracing::info!(
                session_id = %spec.session_id,
                decision = "scratch-seeded",
                "session spec carried no relocated HOME; spawning into a seeded scratch \
                 HOME (.cursor minimal account/config set)"
            );
            crate::agent_env::session_env_with_home(
                &spec.env,
                &spec.session_id,
                Some(CURSOR_AUTH_ENV),
                &home,
            )
        }
        Err(e) => {
            tracing::warn!(
                session_id = %spec.session_id,
                error = %e,
                "cursor scratch HOME seeding failed; session spawns into an empty scratch \
                 HOME and will fail auth loudly if CURSOR_API_KEY is not injected"
            );
            crate::agent_env::agent_session_env(&spec.env, &spec.session_id, Some(CURSOR_AUTH_ENV))
        }
    }
}

/// Seed `<scratch_root>/home/.cursor` with [`CURSOR_SEED_ENTRIES`], copied
/// opaquely (bytes only, no parsing/logging of contents) from the real
/// home's `.cursor` when present; a missing source yields an
/// empty-but-present `.cursor`. Returns the home dir the child should get as
/// `HOME`.
fn seed_cursor_scratch_home(
    scratch_root: &Path,
    real_home: Option<&Path>,
) -> std::io::Result<PathBuf> {
    let home = scratch_root.join("home");
    let cursor_dir = home.join(".cursor");
    std::fs::create_dir_all(&cursor_dir)?;
    if let Some(real_home) = real_home {
        let source = real_home.join(".cursor");
        for entry in CURSOR_SEED_ENTRIES {
            let src = source.join(entry);
            let dst = cursor_dir.join(entry);
            if src.is_file() {
                std::fs::copy(&src, &dst)?;
            } else if src.is_dir() {
                copy_dir_recursive(&src, &dst)?;
            }
        }
    }
    Ok(home)
}

/// Opaque recursive copy (files only; symlinks and other special entries
/// are skipped rather than followed).
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The HOME one session's child will actually receive, mirroring
/// [`cursor_child_env`]'s resolution exactly (both branches of
/// [`crate::agent_env::agent_session_env`] land on one of these two):
/// a spec-carried relocated HOME is used verbatim; otherwise the
/// per-session scratch home `<scratch_home_root>/home` — seeded by
/// [`seed_cursor_scratch_home`], or empty-but-present on seed failure.
/// The hook-status install resolves the same path so its
/// `<home>/.cursor/hooks.json` is always in the tree the child sees
/// (and never the primary checkout).
fn cursor_session_home(spec: &SessionSpec) -> PathBuf {
    if let Some(home) = spec.env.get("HOME") {
        return PathBuf::from(home);
    }
    crate::backend_claude::scratch_home_root(&spec.session_id).join("home")
}

/// Serializes the tests in this module that mutate the process-global env
/// vars consulted by [`discover_cursor_binary`] (`KRANZ_CURSOR_BIN`, `PATH`,
/// `HOME`), since `cargo test` runs tests in parallel threads within one
/// process (mirrors `KIMI_ENV_LOCK`; private because — unlike kimi — cursor's
/// env-mutating tests live in this one source file).
#[cfg(test)]
static CURSOR_ENV_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Binary discovery
// ---------------------------------------------------------------------------

/// Locate a working cursor `agent` binary.
///
/// Order: `KRANZ_CURSOR_BIN` env var → `configured` → `agent` on PATH →
/// well-known install locations, ending with the Cursor-specific
/// `~/.local/bin/agent` (per docs/scoping/cursor-cli-backend.md, this is
/// where the real install lived on the probe host). Each candidate is
/// validated by running it with `--version`; the first one that succeeds
/// wins. Errors list every attempt so the user can see what was tried.
///
/// `KRANZ_CURSOR_BIN`, when set and non-empty, is an *exclusive* override:
/// only that path is probed, and a failure is returned immediately rather
/// than falling through to PATH or the well-known fallback locations. Naming
/// the binary explicitly and having it not work is an error, not a reason to
/// search elsewhere.
pub fn discover_cursor_binary(configured: Option<&str>) -> Result<PathBuf> {
    if let Some(env_bin) = std::env::var_os("KRANZ_CURSOR_BIN") {
        if !env_bin.is_empty() {
            let candidate = PathBuf::from(env_bin);
            return match probe_version(&candidate) {
                Ok(_version) => Ok(candidate),
                Err(why) => Err(EngineError::Config(format!(
                    "KRANZ_CURSOR_BIN points at {} which did not work: {why}",
                    candidate.display()
                ))),
            };
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(configured) = configured {
        candidates.push(PathBuf::from(configured));
    }
    // Bare names resolve through PATH (std::process handles .cmd/.exe lookup
    // rules per-platform). The Cursor CLI's binary is `agent`, not `cursor`
    // (`cursor` is the desktop wrapper).
    candidates.push(PathBuf::from("agent"));
    #[cfg(windows)]
    {
        candidates.push(PathBuf::from("agent.cmd"));
        candidates.push(PathBuf::from("agent.exe"));
    }
    candidates.extend(fallback_candidates());

    // Dedupe, preserving priority order.
    let mut deduped: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        if !deduped.contains(&candidate) {
            deduped.push(candidate);
        }
    }

    let mut attempts: Vec<String> = Vec::new();
    for candidate in deduped {
        match probe_version(&candidate) {
            Ok(_version) => return Ok(candidate),
            Err(why) => attempts.push(format!("{} ({why})", candidate.display())),
        }
    }
    Err(EngineError::Config(format!(
        "no working cursor agent binary found; tried: {}. Install the Cursor \
         CLI or point kranz at it via the KRANZ_CURSOR_BIN environment variable.",
        attempts.join(", ")
    )))
}

/// Well-known install locations checked after PATH, ending with the
/// Cursor-specific install dir (per docs/scoping/cursor-cli-backend.md).
#[cfg(not(windows))]
fn fallback_candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut out = Vec::new();
    if let Some(home) = &home {
        out.push(home.join(".npm-global").join("bin").join("agent"));
    }
    out.push(PathBuf::from("/opt/homebrew/bin/agent"));
    out.push(PathBuf::from("/usr/local/bin/agent"));
    if let Some(home) = &home {
        out.push(home.join(".local").join("bin").join("agent"));
    }
    out
}

/// Well-known install locations checked after PATH (Windows).
#[cfg(windows)]
fn fallback_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        for dir in [
            profile.join("AppData").join("Roaming").join("npm"),
            profile.join(".npm-global").join("bin"),
            profile.join(".local").join("bin"),
        ] {
            for name in ["agent.cmd", "agent.exe", "agent"] {
                out.push(dir.join(name));
            }
        }
    }
    out
}

/// Deadline for a `--version` probe. Generous for a healthy CLI, but bounds
/// a hung shim on PATH so binary discovery (`kranz ready`, session spawn)
/// can never block forever on a candidate.
const VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Validate a candidate by running `<candidate> --version`, draining both
/// output pipes concurrently while enforcing [`VERSION_PROBE_TIMEOUT`].
fn probe_version(binary: &Path) -> std::result::Result<String, String> {
    crate::backend_probe::probe_version(binary, VERSION_PROBE_TIMEOUT)
}

// ---------------------------------------------------------------------------
// Argument construction
// ---------------------------------------------------------------------------

/// The prompt text `agent` actually receives: `append_system_prompt` (if any)
/// concatenated ahead of the prompt text — the CLI has no
/// `--append-system-prompt` flag, so the engine folds it into the single
/// positional prompt argument instead.
fn effective_prompt(spec: &SessionSpec) -> String {
    let prompt_text = match &spec.prompt {
        PromptMode::SingleShot(text) => text.as_str(),
        PromptMode::Streaming(text) => text.as_str(),
    };
    match &spec.append_system_prompt {
        Some(system) if !system.is_empty() => format!("{system}\n\n{prompt_text}"),
        _ => prompt_text.to_string(),
    }
}

/// Build the argv (excluding the binary itself) for one session.
///
/// Public so tests can assert the exact CLI wire format without spawning.
/// Deliberately ignores every claude-only `SessionSpec` field: `json_schema`,
/// `max_budget_usd`, `resume`, `permission_mode`, `allowed_tools` /
/// `disallowed_tools`, `tools`, `settings_json`, `effort` (see module docs).
/// `--workspace` pins the CLI's workspace to the session cwd (probe item 4,
/// exercised live); `--worktree` is deliberately never emitted (its behavior
/// is unobserved by design — kranz does its own worktree isolation), and
/// neither is `--sandbox` (observed to be no isolation boundary) or
/// `--stream-partial-output` (complete assistant events are what the parser
/// consumes; the fixture was captured without it).
pub fn build_args(spec: &SessionSpec) -> Vec<String> {
    let mut args = vec![
        "--print".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--trust".into(),
        "--workspace".into(),
        spec.cwd.display().to_string(),
        "--model".into(),
        spec.model.clone(),
    ];
    // The permission mapping (probe item 6): read-only sessions get the
    // turn-level read-only `--mode ask`; writable sessions get default mode +
    // `--force` (the observed unprompted-writes spelling; `--yolo` is only an
    // inferred alias).
    if spec.writable {
        args.push("--force".into());
    } else {
        args.push("--mode".into());
        args.push("ask".into());
    }
    args.push(effective_prompt(spec));
    args
}

// ---------------------------------------------------------------------------
// `agent --print --output-format stream-json` line parsing
// ---------------------------------------------------------------------------

/// Parse one stdout line into zero or more [`AgentEvent`]s. `model` is the
/// configured model id, used as the `Init` fallback (the wire's init event
/// carries a model DISPLAY string, e.g. "GPT-5.6 Luna 272K Low", which is
/// preferred when present) and as the pricing key for the terminal event's
/// client-side cost computation.
///
/// Unparseable lines become [`AgentEvent::Other`] with
/// `raw = {"unparsed": <line>}` so nothing is ever dropped from transcripts
/// — this is also what the pre-billing plain-text rejections
/// (`Cannot use this model`, `Authentication required`) arrive as.
pub fn parse_cursor_line(line: &str, model: &str) -> Vec<AgentEvent> {
    match serde_json::from_str::<Value>(line) {
        Ok(value) => parse_cursor_value(value, model),
        Err(_) => vec![AgentEvent::Other {
            raw: json!({ "unparsed": line }),
        }],
    }
}

/// Map one parsed `stream-json` value to events (see module docs /
/// docs/scoping/cursor-cli-backend.md's event-to-`AgentEvent` table).
/// Unrecognized `type`/`subtype` combinations route to [`AgentEvent::Other`]
/// rather than being guessed at.
pub fn parse_cursor_value(value: Value, model: &str) -> Vec<AgentEvent> {
    let line_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    match line_type {
        "system" if str_field(&value, "subtype") == "init" => vec![AgentEvent::Init {
            session_id: str_field(&value, "session_id"),
            model: value
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(model)
                .to_string(),
            raw: value,
        }],
        // The `user` event echoes the prompt back; any other `system`
        // subtype is unobserved. Both are transcript-only.
        "user" | "system" => vec![AgentEvent::Other { raw: value }],
        "assistant" => {
            let text = assistant_text(&value);
            if text.is_empty() {
                vec![AgentEvent::Other { raw: value }]
            } else {
                vec![AgentEvent::Text { text, raw: value }]
            }
        }
        "tool_call" => match str_field(&value, "subtype").as_str() {
            "started" => vec![parse_tool_use(value)],
            "completed" => parse_tool_result(value),
            _ => vec![AgentEvent::Other { raw: value }],
        },
        "result" => vec![parse_terminal(value, model)],
        _ => vec![AgentEvent::Other { raw: value }],
    }
}

/// The joined text blocks of an `assistant` event's `message.content`
/// (the fixture carries exactly one `{"type":"text","text":...}` block;
/// multiple blocks concatenate so none are dropped).
fn assistant_text(value: &Value) -> String {
    let mut out = String::new();
    if let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array) {
        for block in blocks {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                }
            }
        }
    }
    out
}

/// The tool kind key under `/tool_call` (`shellToolCall`, `editToolCall`,
/// `readToolCall` in the fixture; the discriminated union's member is the
/// one key ending in `ToolCall` — the object also carries bookkeeping keys
/// like `toolCallId`/`hookAdditionalContexts` that must not be mistaken for
/// it). Falls back to `"tool"` when the shape is unobserved.
fn tool_kind(value: &Value) -> String {
    value
        .get("tool_call")
        .and_then(Value::as_object)
        .and_then(|obj| obj.keys().find(|k| k.ends_with("ToolCall")).cloned())
        .unwrap_or_else(|| "tool".to_string())
}

/// A `tool_call/started` event maps to [`AgentEvent::ToolUse`]; the summary
/// is the shell command, the edited/read path, or the call's description,
/// whichever the tool's args carry first.
fn parse_tool_use(value: Value) -> AgentEvent {
    let kind = tool_kind(&value);
    let args = value.pointer(&format!("/tool_call/{kind}/args"));
    let summary = args
        .and_then(|args| {
            args.get("command")
                .or_else(|| args.get("path"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .pointer(&format!("/tool_call/{kind}/description"))
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    AgentEvent::ToolUse {
        tool: kind,
        summary: truncate_chars(summary, SUMMARY_MAX_CHARS),
        raw: value,
    }
}

/// A `tool_call/completed` event maps to [`AgentEvent::ToolResult`]. The
/// result union discriminates on `success`/`failure`; a `failure` carrying a
/// real `exitCode` is a normal failed command, NOT a kranz guardrail denial
/// — no in-band permission-denial frame was observed on this wire (kranz's
/// no-push invariant for cursor is enforced externally: read-only turn modes
/// and scoped credentials), so `denied` is always `false` here rather than
/// guessed from text. A completed event with no recognizable result member
/// routes to [`AgentEvent::Other`] (unobserved shape).
fn parse_tool_result(value: Value) -> Vec<AgentEvent> {
    let kind = tool_kind(&value);
    let result = value.pointer(&format!("/tool_call/{kind}/result"));
    let Some(result) = result else {
        return vec![AgentEvent::Other { raw: value }];
    };
    let summary = if let Some(success) = result.get("success") {
        success
            .get("stdout")
            .or_else(|| success.get("message"))
            .or_else(|| success.get("content"))
            .or_else(|| success.get("diffString"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| success.to_string())
    } else if let Some(failure) = result.get("failure") {
        failure
            .get("stderr")
            .or_else(|| failure.get("stdout"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                failure
                    .get("exitCode")
                    .and_then(Value::as_i64)
                    .map(|code| format!("exit code {code}"))
            })
            .unwrap_or_else(|| failure.to_string())
    } else {
        return vec![AgentEvent::Other { raw: value }];
    };
    vec![AgentEvent::ToolResult {
        tool: Some(kind),
        denied: false,
        summary: truncate_chars(&summary, SUMMARY_MAX_CHARS),
        raw: value,
    }]
}

/// The terminal `result` event: full result text in `.result` (acceptance
/// item 1 — no cross-line stitching is needed on this wire; the final
/// `assistant` event repeats the same text), usage from the `.usage` object
/// (`inputTokens`/`outputTokens`/`cacheReadTokens`/`cacheWriteTokens`).
///
/// Absent stays absent, never fabricated: a result with NO `usage` object
/// records the zero default and `cost_usd: None` — the wire carries no
/// dollar cost (probe item 3), so cost is only ever computed client-side
/// from REAL usage tokens via [`cost::usage_cost_usd`].
fn parse_terminal(value: Value, model: &str) -> AgentEvent {
    let usage_present = value.get("usage").is_some();
    let usage_field = |key: &str| {
        value
            .pointer(&format!("/usage/{key}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let usage = TokenUsage {
        input: usage_field("inputTokens"),
        output: usage_field("outputTokens"),
        cache_read: usage_field("cacheReadTokens"),
        cache_write: usage_field("cacheWriteTokens"),
    };
    let cost_usd = usage_present.then(|| cost::usage_cost_usd(&usage, model));
    let is_error = value
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || str_field(&value, "subtype") == "error";
    AgentEvent::Result {
        text: str_field(&value, "result"),
        is_error,
        usage,
        cost_usd,
        num_turns: Some(1),
        raw: value,
    }
}

/// Whether an unparsed stdout line (or a stderr tail) names a known
/// pre-billing rejection ([`PRE_BILLING_FAILURE_PHRASES`], probe item 5) —
/// deterministic, user-readable, and never retried because no turn was
/// billed.
fn names_pre_billing_failure(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    PRE_BILLING_FAILURE_PHRASES
        .iter()
        .any(|phrase| lower.contains(phrase))
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Keep at most `max` characters (not bytes — never splits a code point).
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}

/// Last `max` characters of `text` (for stderr tails in error messages).
fn last_chars(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(max);
    chars[start..].iter().collect()
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// The [`AgentBackend`] for `agent --print --output-format stream-json`:
/// single-shot with the `--mode ask` / `--force` permission posture selected
/// from the session role.
#[derive(Debug, Clone)]
pub struct CursorBackend {
    binary: PathBuf,
}

impl CursorBackend {
    /// Use an explicit binary path (no validation performed).
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        CursorBackend {
            binary: binary.into(),
        }
    }

    /// Discover the binary via [`discover_cursor_binary`].
    pub fn discover(configured: Option<&str>) -> Result<Self> {
        Ok(CursorBackend {
            binary: discover_cursor_binary(configured)?,
        })
    }

    /// The binary this backend spawns.
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

#[async_trait::async_trait]
impl AgentBackend for CursorBackend {
    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>> {
        if spec.resume.is_some() {
            return Err(EngineError::Backend(
                "cursor backend is single-shot only; resume is unsupported".to_string(),
            ));
        }
        let model = spec.model.clone();
        let args = build_args(&spec);

        // agent-env-clear: CLEARED env from the minimal allowlist; the
        // scratch HOME is SEEDED with the minimal .cursor account/config
        // set (login state does not survive a relocated HOME), and the
        // one ambient var a cursor session may authenticate with is
        // injected explicitly, never the whole ambient set.
        let child_env = cursor_child_env(&spec);

        // Ticket agent-hooks-status-signals: install the OPTIONAL hook
        // lane into the session-private HOME (the seeded `.cursor` now
        // exists, and the tracked project `.cursor/hooks.json` is never
        // touched — module docs). The seed/env are unaffected; a failure
        // degrades to NO lane with a loud warning, never a spawn error.
        if let Some(seed) = &spec.hook_status {
            if let Err(e) = crate::hook_status::install_cursor_hook_status(
                &cursor_session_home(&spec),
                seed,
                &spec.session_id,
            ) {
                tracing::warn!(
                    session_id = %spec.session_id,
                    error = %e,
                    "hook-status install failed; the session spawns without the lane \
                     (mission state is unaffected — the lane is observational)"
                );
            }
        }

        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(&args)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(child_env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Unix: make the child the leader of a fresh process group so aborts
        // can kill the whole tree, mirroring `backend_claude::ClaudeBackend`.
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn().map_err(|e| {
            EngineError::Backend(format!("failed to spawn {}: {e}", self.binary.display()))
        })?;

        // Windows: kill-on-close Job Object, mirroring `backend_claude`.
        #[cfg(windows)]
        let job = match child.raw_handle() {
            Some(handle) => match win_job::JobHandle::create_and_assign(handle) {
                Ok(job) => Some(job),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to create Job Object for cursor child; \
                        tree-kill on abort will be unavailable");
                    None
                }
            },
            None => None,
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::Backend("cursor child has no stdout pipe".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| EngineError::Backend("cursor child has no stderr pipe".to_string()))?;

        // Capture stderr concurrently so a chatty child never blocks on a
        // full pipe and failure messages can include the tail. The stream is
        // drained to EOF but only a bounded tail is retained — a noisy or
        // malicious CLI must not exhaust host memory (stream_bounds).
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_task = {
            let buf = Arc::clone(&stderr_buf);
            tokio::spawn(async move {
                let tail = drain_to_tail(stderr, STDERR_TAIL_CAP).await;
                *buf.lock().expect("stderr buffer lock") = tail;
            })
        };

        Ok(Box::new(CursorSession {
            session_id: spec.session_id.clone(),
            model,
            child,
            #[cfg(windows)]
            job,
            lines: BoundedLines::new(stdout),
            stderr_buf,
            stderr_task: Some(stderr_task),
            queue: VecDeque::new(),
            saw_result: false,
            saw_success_result: false,
            pre_billing_failure: None,
            exit: None,
        }))
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A live `agent --print --output-format stream-json` session (the
/// [`AgentSession`] impl).
///
/// Single-shot only: [`send_user_message`](AgentSession::send_user_message)
/// always errors, and there is no streaming stdin to hold open.
pub struct CursorSession {
    session_id: String,
    model: String,
    child: Child,
    #[cfg(windows)]
    job: Option<win_job::JobHandle>,
    lines: BoundedLines<ChildStdout>,
    stderr_buf: Arc<Mutex<String>>,
    stderr_task: Option<JoinHandle<()>>,
    /// Multi-block lines queue several events; popped one per `next_event`.
    queue: VecDeque<AgentEvent>,
    saw_result: bool,
    saw_success_result: bool,
    /// The first unparsed stdout line naming a known pre-billing rejection
    /// (`Cannot use this model`, `Authentication required` — probe item 5):
    /// recorded so EOF can word the failure as the configuration error it is
    /// rather than a retryable transport failure.
    pre_billing_failure: Option<String>,
    exit: Option<SessionExit>,
}

impl CursorSession {
    fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Init { session_id, .. } => {
                self.session_id = session_id.clone();
            }
            AgentEvent::Result { is_error, .. } => {
                self.saw_result = true;
                if !is_error {
                    self.saw_success_result = true;
                }
            }
            AgentEvent::Other { raw } if self.pre_billing_failure.is_none() => {
                if let Some(line) = raw.get("unparsed").and_then(Value::as_str) {
                    if names_pre_billing_failure(line) {
                        self.pre_billing_failure = Some(truncate_chars(line, STDERR_TAIL_CHARS));
                    }
                }
            }
            _ => {}
        }
    }

    /// Kill the child and reap it, best-effort; also joins the stderr capture
    /// task. Mirrors `backend_claude::ClaudeSession::kill_child` exactly:
    /// unix process-group SIGKILL (with a post-reap sweep for stragglers that
    /// raced a mid-fork), windows kill-on-close Job Object.
    async fn kill_child(&mut self) {
        #[cfg(unix)]
        {
            let pgid = self
                .child
                .id()
                .and_then(|pid| i32::try_from(pid).ok())
                .filter(|pid| *pid > 0);
            let group_killed = matches!(pgid, Some(pgid) if kill_group(pgid));
            if !group_killed {
                let _ = self.child.start_kill();
            }
            let _ = self.child.wait().await;
            if group_killed {
                if let Some(pgid) = pgid {
                    let _ = kill_group(pgid);
                }
            }
        }
        #[cfg(windows)]
        {
            match &self.job {
                Some(job) => job.kill(),
                None => {
                    let _ = self.child.start_kill();
                }
            }
            let _ = self.child.wait().await;
        }
        #[cfg(all(not(unix), not(windows)))]
        {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
        }
        if let Some(task) = self.stderr_task.take() {
            let _ = task.await;
        }
    }

    async fn finish_at_eof(&mut self) {
        let status = self.child.wait().await;
        if let Some(task) = self.stderr_task.take() {
            let _ = task.await;
        }
        // A known pre-billing rejection (probe item 5) is reported as the
        // configuration error it is — the caller fixes the model id or
        // authenticates; nothing was billed and there is nothing to retry.
        // The stdout capture wins; the stderr tail is the fallback for a CLI
        // that prints the rejection there instead. The guard matters: a
        // COMPLETED turn (exit 0 with a success result) is never re-labeled —
        // a tool's own stderr can legitimately contain one of the phrases
        // mid-turn (e.g. a remote's "Authentication required").
        let completed = matches!(status, Ok(ref s) if s.success()) && self.saw_result;
        let pre_billing = if completed {
            None
        } else {
            self.pre_billing_failure.clone().or_else(|| {
                let tail = self.stderr_tail();
                names_pre_billing_failure(&tail).then_some(tail)
            })
        };
        let exit = match (status, pre_billing) {
            (Ok(status), Some(detail)) => SessionExit::Failed(format!(
                "cursor rejected the session before any billed turn (exit {status}): {detail} — \
                 fix the configured model id or authenticate the cursor CLI; this is not a \
                 retryable failure"
            )),
            (Ok(status), None) if status.success() && self.saw_result => SessionExit::Completed,
            (Ok(status), None) => SessionExit::Failed(format!(
                "cursor exited with {status}{}; stderr tail: {}",
                if self.saw_result {
                    ""
                } else {
                    " without emitting a terminal event"
                },
                self.stderr_tail(),
            )),
            (Err(e), _) => SessionExit::Failed(format!(
                "failed to reap cursor process: {e}; stderr tail: {}",
                self.stderr_tail(),
            )),
        };
        self.exit = Some(exit);
    }

    fn stderr_tail(&self) -> String {
        let captured = self
            .stderr_buf
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        last_chars(captured.trim_end(), STDERR_TAIL_CHARS)
    }
}

#[async_trait::async_trait]
impl AgentSession for CursorSession {
    fn session_id(&self) -> String {
        self.session_id.clone()
    }

    async fn next_event(&mut self) -> Result<Option<AgentEvent>> {
        loop {
            if let Some(event) = self.queue.pop_front() {
                return Ok(Some(event));
            }
            if self.exit.is_some() {
                return Ok(None);
            }
            let line = match self.lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => {
                    self.finish_at_eof().await;
                    return Ok(None);
                }
                Err(e) => {
                    self.kill_child().await;
                    self.exit = Some(SessionExit::Failed(format!(
                        "error reading cursor stdout: {e}; stderr tail: {}",
                        self.stderr_tail(),
                    )));
                    return Ok(None);
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let events = parse_cursor_line(&line, &self.model);
            for event in &events {
                self.observe(event);
            }
            self.queue.extend(events);
        }
    }

    async fn send_user_message(&mut self, _text: &str) -> Result<()> {
        Err(EngineError::Backend(
            "cursor backend is single-shot only; send_user_message is unsupported".to_string(),
        ))
    }

    async fn abort(&mut self) -> Result<()> {
        let already_exited = matches!(self.child.try_wait(), Ok(Some(_)));
        self.kill_child().await;
        if self.saw_success_result && already_exited {
            self.exit = Some(SessionExit::Completed);
        } else {
            self.exit = Some(SessionExit::Aborted);
        }
        Ok(())
    }

    fn exit_status(&self) -> Option<SessionExit> {
        self.exit.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MODEL: &str = "gpt-5";

    fn fixture_lines() -> Vec<String> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("docs")
            .join("scoping")
            .join("cursor-probe-evidence")
            .join("fixture-stream-json.jsonl");
        std::fs::read_to_string(path)
            .expect("read fixture")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.to_string())
            .collect()
    }

    fn spec(cwd: &Path, writable: bool) -> SessionSpec {
        SessionSpec {
            cwd: cwd.to_path_buf(),
            prompt: PromptMode::SingleShot("do the thing".to_string()),
            append_system_prompt: None,
            model: TEST_MODEL.to_string(),
            effort: "high".to_string(),
            session_id: "sess-1".to_string(),
            resume: None,
            permission_mode: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
            tools: vec![],
            writable,
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: Default::default(),
            sandbox: None,
            hook_status: None,
        }
    }

    /// The scratch seed carries the minimal `.cursor` account/config set
    /// (login state does not survive a relocated HOME) and never the
    /// unbounded transcripts/caches.
    #[test]
    fn seed_cursor_scratch_home_copies_the_minimal_state_set() {
        let real_home = tempfile::tempdir().unwrap();
        let cursor = real_home.path().join(".cursor");
        std::fs::create_dir_all(cursor.join("chats")).unwrap();
        std::fs::write(cursor.join("cli-config.json"), "{}").unwrap();
        std::fs::write(cursor.join("agent-cli-state.json"), "{}").unwrap();
        std::fs::write(cursor.join("chats").join("big.jsonl"), "transcript").unwrap();
        std::fs::write(cursor.join("prompt_history.json"), "[]").unwrap();

        let scratch = tempfile::tempdir().unwrap();
        let home = seed_cursor_scratch_home(scratch.path(), Some(real_home.path())).unwrap();

        let seeded = home.join(".cursor");
        assert!(seeded.join("cli-config.json").is_file());
        assert!(seeded.join("agent-cli-state.json").is_file());
        assert!(
            !seeded.join("chats").exists(),
            "per-session transcripts are never seeded"
        );
        assert!(
            !seeded.join("prompt_history.json").exists(),
            "unbounded history is never seeded"
        );
    }

    /// A missing real `.cursor` yields an empty-but-present seed (the session
    /// then fails auth loudly rather than inheriting).
    #[test]
    fn seed_cursor_scratch_home_without_a_source_yields_an_empty_seed() {
        let real_home = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let home = seed_cursor_scratch_home(scratch.path(), Some(real_home.path())).unwrap();

        let seeded = home.join(".cursor");
        assert!(seeded.is_dir());
        assert_eq!(std::fs::read_dir(&seeded).unwrap().count(), 0);
    }

    /// The one sanctioned auth var crosses when set; ambient secrets never do.
    #[test]
    fn cursor_child_env_injects_the_sanctioned_api_key_and_never_ambient_secrets() {
        let _poison = crate::agent_env::EnvTestGuard::engage(&[
            ("CURSOR_API_KEY", "hunter2"),
            ("GH_TOKEN", "ghp-poison"),
            ("SLACK_BOT_TOKEN", "xoxb-poison"),
        ]);
        let session_spec = spec(Path::new("."), false);

        let env = cursor_child_env(&session_spec);

        assert_eq!(
            env.get("CURSOR_API_KEY").map(String::as_str),
            Some("hunter2"),
            "the sanctioned auth var must be injected explicitly"
        );
        for secret in ["GH_TOKEN", "SLACK_BOT_TOKEN", "ANTHROPIC_API_KEY"] {
            assert!(!env.contains_key(secret), "child env leaked {secret}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn cursor_probe_version_kills_a_hung_binary_within_the_deadline() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("hung-agent");
        std::fs::write(&stub, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let start = std::time::Instant::now();
        let result = probe_version(&stub);

        let error = result.expect_err("a hung probe must be reported as broken");
        assert!(error.contains("did not exit"), "{error}");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "probe returned within the deadline, not after the stub's sleep"
        );
    }

    #[test]
    fn cursor_discovery_honors_env_override_exclusively() {
        // ENV_TEST_LOCK first: tempfile resolves its parent from ambient
        // TMP/TEMP, and env-poisoning tests elsewhere in this binary hold the
        // same lock (see `KIMI_ENV_LOCK`'s note in backend_kimi.rs).
        let _env_lock = crate::agent_env::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = CURSOR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let working = dir.path().join("working-agent");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&working, "#!/bin/sh\necho 2026.07.08-test\n").unwrap();
            std::fs::set_permissions(&working, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let bogus = dir.path().join("does-not-exist-agent");

        std::env::set_var("KRANZ_CURSOR_BIN", &bogus);
        let result = discover_cursor_binary(Some(working.to_str().unwrap()));
        std::env::remove_var("KRANZ_CURSOR_BIN");

        let error = result.expect_err("a broken KRANZ_CURSOR_BIN must fail immediately");
        assert!(
            error.to_string().contains("KRANZ_CURSOR_BIN"),
            "expected the error to name the exclusive override, got: {error}"
        );
        assert!(
            !error.to_string().contains("working-agent"),
            "the exclusive override must not fall through to `configured`, got: {error}"
        );
    }

    #[test]
    fn backend_cursor_parse_fixture() {
        let mut events: Vec<AgentEvent> = Vec::new();
        for line in fixture_lines() {
            events.extend(parse_cursor_line(&line, TEST_MODEL));
        }

        assert!(
            events.iter().any(
                |e| matches!(e, AgentEvent::Init { session_id, model, .. }
                    if !session_id.is_empty() && model == "GPT-5.6 Luna 272K Low")
            ),
            "expected an Init event with a non-empty session id and the wire's model display string"
        );
        for kind in ["shellToolCall", "editToolCall", "readToolCall"] {
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, AgentEvent::ToolUse { tool, .. } if tool == kind)),
                "expected a ToolUse event with tool == {kind:?}"
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, AgentEvent::ToolResult { tool, denied, .. }
                        if tool.as_deref() == Some(kind) && !denied)),
                "expected a non-denied ToolResult event with tool == {kind:?}"
            );
        }
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Text { text, .. } if !text.is_empty())),
            "expected at least one Text event"
        );

        let terminal = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Result {
                    text,
                    is_error,
                    usage,
                    cost_usd,
                    num_turns,
                    ..
                } => Some((text, is_error, usage, cost_usd, num_turns)),
                _ => None,
            })
            .expect("expected a terminal Result event");
        let (text, is_error, usage, cost_usd, num_turns) = terminal;
        assert!(
            !text.is_empty(),
            "the terminal result event carries the full result text (no stitching needed)"
        );
        assert!(!is_error);
        assert_eq!(
            *usage,
            TokenUsage {
                input: 32473,
                output: 305,
                cache_read: 96675,
                cache_write: 0,
            },
            "the fixture's usage object must map verbatim onto TokenUsage"
        );
        assert!(
            cost_usd.is_some(),
            "usage is on the wire, so a client-side computed cost must be present"
        );
        assert_eq!(*num_turns, Some(1));
    }

    #[test]
    fn build_args_maps_read_only_to_mode_ask_and_writable_to_force() {
        let read_only = build_args(&spec(Path::new("/tmp/ws"), false));
        assert_eq!(
            read_only,
            vec![
                "--print",
                "--output-format",
                "stream-json",
                "--trust",
                "--workspace",
                "/tmp/ws",
                "--model",
                TEST_MODEL,
                "--mode",
                "ask",
                "do the thing",
            ]
        );
        let writable = build_args(&spec(Path::new("/tmp/ws"), true));
        assert_eq!(
            writable,
            vec![
                "--print",
                "--output-format",
                "stream-json",
                "--trust",
                "--workspace",
                "/tmp/ws",
                "--model",
                TEST_MODEL,
                "--force",
                "do the thing",
            ]
        );
    }

    #[test]
    fn build_args_ignores_claude_only_fields_and_folds_the_system_prompt() {
        let mut session_spec = spec(Path::new("."), false);
        session_spec.append_system_prompt = Some("be terse".to_string());
        session_spec.permission_mode = Some("acceptEdits".to_string());
        session_spec.allowed_tools = vec!["Bash(npm test*)".to_string()];
        session_spec.disallowed_tools = vec!["Bash(git push*)".to_string()];
        session_spec.tools = vec!["Bash".to_string()];
        session_spec.settings_json = Some(json!({"hooks": {}}));
        session_spec.json_schema = Some(json!({"type": "object"}));
        session_spec.max_budget_usd = Some(5.0);

        let args = build_args(&session_spec);
        assert_eq!(
            args.last().map(String::as_str),
            Some("be terse\n\ndo the thing")
        );
        for forbidden in [
            "--effort",
            "--permission-mode",
            "--allowedTools",
            "--disallowedTools",
            "--tools",
            "--settings",
            "--json-schema",
            "--max-budget-usd",
            "--sandbox",
            "--worktree",
            "--yolo",
        ] {
            assert!(
                !args.iter().any(|a| a == forbidden),
                "argv must not contain {forbidden}: {args:?}"
            );
        }
    }

    /// Probe item 2 / the brief's table: a `result.failure` with a real
    /// exitCode is a normal failed command, never a guardrail denial.
    #[test]
    fn tool_result_failure_is_a_normal_failure_not_a_denial() {
        let completed = json!({
            "type": "tool_call",
            "subtype": "completed",
            "call_id": "c1",
            "tool_call": {
                "shellToolCall": {
                    "args": {"command": "git push origin main"},
                    "result": {"failure": {
                        "command": "git push origin main",
                        "exitCode": 1,
                        "signal": "",
                        "stdout": "",
                        "stderr": "denied by policy",
                        "aborted": false
                    }}
                }
            }
        });
        let events = parse_cursor_value(completed, TEST_MODEL);
        match &events[0] {
            AgentEvent::ToolResult {
                tool,
                denied,
                summary,
                ..
            } => {
                assert_eq!(tool.as_deref(), Some("shellToolCall"));
                assert!(
                    !denied,
                    "a failed command with a real exit code is not a denial"
                );
                assert_eq!(summary, "denied by policy");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    /// Absent stays absent: a terminal result with no `usage` object records
    /// the zero default and `cost_usd: None` — cost is computed client-side
    /// only from REAL usage tokens, never fabricated from nothing.
    #[test]
    fn result_without_usage_keeps_usage_and_cost_absent() {
        let result = json!({
            "type": "result",
            "subtype": "success",
            "duration_ms": 10,
            "is_error": false,
            "result": "done",
        });
        let events = parse_cursor_value(result, TEST_MODEL);
        match &events[0] {
            AgentEvent::Result {
                usage, cost_usd, ..
            } => {
                assert_eq!(*usage, TokenUsage::default(), "usage is never fabricated");
                assert_eq!(*cost_usd, None, "unreported usage means no cost either");
            }
            other => panic!("expected Result, got {other:?}"),
        }
    }

    /// A torn/unparseable line is never a parse failure: it rides the
    /// transcript as `Other { raw: {"unparsed": ... } }`.
    #[test]
    fn unparseable_lines_become_other_transcript_entries() {
        let events = parse_cursor_line("{\"type\":\"resu", TEST_MODEL);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::Other { raw } => {
                assert_eq!(raw["unparsed"], "{\"type\":\"resu");
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }

    /// Probe item 5: the pre-billing rejection phrases are detected
    /// case-insensitively; ordinary output never trips the detector.
    #[test]
    fn pre_billing_failure_detection_names_only_known_rejections() {
        assert!(names_pre_billing_failure(
            "Cannot use this model: bogus-id. Available models: gpt-5"
        ));
        assert!(names_pre_billing_failure("Authentication required"));
        assert!(!names_pre_billing_failure("README.md"));
        assert!(!names_pre_billing_failure(""));
    }
}
