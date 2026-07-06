//! Real agent backend: drives the `claude` CLI headless.
//!
//! Ground truth is docs/design.md "Verified CLI behavior" (claude 2.1.198):
//! `claude -p --output-format stream-json --verbose` emits JSONL that this
//! module parses into [`AgentEvent`]s. Parsers MUST tolerate unknown line
//! types (`rate_limit_event`, `system/thinking_tokens`,
//! `system/post_turn_summary`, ...) — they map to [`AgentEvent::Other`] and
//! keep the raw line for transcript fidelity.
//!
//! The CLI dropped `--max-turns`, so turn budgets are engine-enforced here:
//! distinct assistant `message.id` values are counted and the session is
//! aborted once the count exceeds `SessionSpec::max_turns`.

use crate::backend::{
    AgentBackend, AgentEvent, AgentSession, PromptMode, SessionExit, SessionSpec,
};
use crate::error::{EngineError, Result};
use crate::types::TokenUsage;
use serde_json::{json, Value};
use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::task::JoinHandle;

/// Max characters kept in tool-use / tool-result summaries.
const SUMMARY_MAX_CHARS: usize = 200;
/// Max characters of captured stderr included in failure messages.
const STDERR_TAIL_CHARS: usize = 500;

// ---------------------------------------------------------------------------
// Windows process-tree kill via Job Objects
// ---------------------------------------------------------------------------

/// Windows process-tree kill, mirroring the unix process-group approach.
///
/// COMPILES AND RUNS ONLY UNDER `cfg(windows)`. This whole module is
/// `#[cfg(windows)]`, so it is absent from the macOS/Linux build entirely and
/// is validated exclusively by the `windows-latest` CI job — never by the dev
/// host. Keep the unsafe surface tiny and every `HANDLE` closed exactly once.
///
/// Windows has no process groups. The equivalent is a **Job Object** with
/// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`: every process assigned to the job —
/// and every descendant it spawns, which inherit job membership — is
/// terminated the moment the last handle to the job closes. So a `claude` CLI
/// (or `sh`/`cmd` wrapper) assigned to such a job takes its whole tool-child
/// tree (test runners, builds) down with it on abort, timeout, or a plain
/// drop of the job handle.
///
/// Usage: [`JobHandle::create_and_assign`] right after spawn, store the
/// returned guard alongside the child, then either call [`JobHandle::kill`]
/// (explicit `TerminateJobObject`) or just drop the guard (`CloseHandle` +
/// `KILL_ON_JOB_CLOSE`) — both kill the tree.
///
/// Assignment happens *after* `spawn()` (tokio's `Command` exposes no
/// `CREATE_SUSPENDED`), so there is a microsecond window in which the child
/// could `spawn` a grandchild before it is assigned — that grandchild would
/// escape the job. In practice `claude`/`cmd` has not forked a tool child in
/// the gap between `spawn()` and the assign, so this matches the unix
/// process-group approach (which has an analogous fork race) closely enough.
#[cfg(windows)]
pub(crate) mod win_job {
    use std::os::windows::io::RawHandle;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    // `windows` 0.58 gates its `CreateJobObjectW` wrapper behind the
    // `Win32_Security` feature (its signature names `SECURITY_ATTRIBUTES`),
    // and this crate's manifest deliberately does not enable that feature. We
    // only ever pass a null security descriptor and null name, so we declare
    // the raw kernel32 import ourselves — no `SECURITY_ATTRIBUTES` type is
    // needed. The ungated `SetInformationJobObject`/`AssignProcessToJobObject`/
    // `TerminateJobObject`/`CloseHandle` wrappers are used as-is above.
    #[link(name = "kernel32")]
    extern "system" {
        fn CreateJobObjectW(
            lpjobattributes: *const core::ffi::c_void,
            lpname: *const u16,
        ) -> *mut core::ffi::c_void;
    }

    /// RAII owner of a Job Object `HANDLE`. `Drop` calls `CloseHandle` exactly
    /// once, which (because the job was created with
    /// `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`) also terminates every process
    /// still assigned to the job.
    #[derive(Debug)]
    pub(crate) struct JobHandle {
        job: HANDLE,
    }

    // The stored HANDLE is a kernel object handle owned solely by this guard;
    // it is safe to move across threads (the child is polled from tokio tasks).
    // SAFETY: a Job Object HANDLE is not tied to any thread; Win32 permits use
    // and close from any thread. We own it exclusively (closed once on Drop).
    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}

    impl JobHandle {
        /// Create a kill-on-close job, assign the process behind `child_handle`
        /// to it, and return the owning guard. The process's descendants inherit
        /// membership, so the whole tree dies when this guard is killed or
        /// dropped.
        ///
        /// `child_handle` is the child process's raw handle — on Windows,
        /// `tokio::process::Child::raw_handle()`. It is borrowed for the
        /// assignment only: it stays owned by the `Child` and is never closed
        /// here.
        ///
        /// Errors carry the failing Win32 call so a CI failure is diagnosable;
        /// the caller treats a job-setup failure as non-fatal (the child still
        /// runs, just without tree-kill — same as the pre-job behaviour).
        pub(crate) fn create_and_assign(child_handle: RawHandle) -> windows::core::Result<Self> {
            // SAFETY: CreateJobObjectW with a null SECURITY_ATTRIBUTES pointer
            // and a null name creates an unnamed, default-security job. It
            // returns a null handle on failure (GetLastError set), which we map
            // to a windows Error via `from_win32`.
            let raw_job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if raw_job.is_null() {
                return Err(windows::core::Error::from_win32());
            }
            let job = HANDLE(raw_job);
            // Wrap immediately so any early return below still closes the job.
            let guard = JobHandle { job };

            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: `job` is a valid job handle; we pass a pointer to a
            // correctly typed, fully initialized info struct together with its
            // exact byte length, as the API requires.
            unsafe {
                SetInformationJobObject(
                    guard.job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )?;
            }

            // `RawHandle` is already `*mut c_void`, exactly HANDLE's field type.
            // SAFETY: `guard.job` is valid; `child_handle` is the child's live
            // process handle (borrowed — not closed here). AssignProcessToJobObject
            // only reads it.
            unsafe {
                AssignProcessToJobObject(guard.job, HANDLE(child_handle))?;
            }
            Ok(guard)
        }

        /// Terminate every process in the job now (explicit kill path). Dropping
        /// the guard would achieve the same via `KILL_ON_JOB_CLOSE`, but the
        /// explicit call makes the kill deterministic even while the guard is
        /// still held.
        pub(crate) fn kill(&self) {
            // SAFETY: `self.job` is a valid job handle owned by this guard;
            // TerminateJobObject takes it plus an exit code and returns a
            // Result we deliberately ignore (best-effort kill).
            let _ = unsafe { TerminateJobObject(self.job, 1) };
        }
    }

    impl Drop for JobHandle {
        fn drop(&mut self) {
            // SAFETY: `self.job` was returned by CreateJobObjectW and is closed
            // exactly once, here. Closing the last handle to a
            // KILL_ON_JOB_CLOSE job also terminates any surviving members.
            let _ = unsafe { CloseHandle(self.job) };
        }
    }
}

// ---------------------------------------------------------------------------
// Binary discovery
// ---------------------------------------------------------------------------

/// Locate a working `claude` binary.
///
/// Order: `configured` → `KRANZ_CLAUDE_BIN` env var → `claude` on PATH →
/// well-known install locations. Each candidate is validated by running it
/// with `--version`; the first one that succeeds wins. Errors list every
/// attempt so the user can see what was tried.
pub fn discover_claude_binary(configured: Option<&str>) -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(configured) = configured {
        candidates.push(PathBuf::from(configured));
    }
    if let Some(env_bin) = std::env::var_os("KRANZ_CLAUDE_BIN") {
        if !env_bin.is_empty() {
            candidates.push(PathBuf::from(env_bin));
        }
    }
    // Bare names resolve through PATH (std::process handles .cmd/.exe lookup
    // rules per-platform).
    candidates.push(PathBuf::from("claude"));
    #[cfg(windows)]
    {
        candidates.push(PathBuf::from("claude.cmd"));
        candidates.push(PathBuf::from("claude.exe"));
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
        "no working claude binary found; tried: {}. Install Claude Code \
         (npm install -g @anthropic-ai/claude-code) or point kranz at it via \
         the claudeBinary config field or the KRANZ_CLAUDE_BIN environment \
         variable.",
        attempts.join(", ")
    )))
}

/// Well-known install locations checked after PATH.
#[cfg(not(windows))]
fn fallback_candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut out = Vec::new();
    if let Some(home) = &home {
        out.push(home.join(".npm-global").join("bin").join("claude"));
    }
    out.push(PathBuf::from("/opt/homebrew/bin/claude"));
    out.push(PathBuf::from("/usr/local/bin/claude"));
    if let Some(home) = &home {
        out.push(home.join(".local").join("bin").join("claude"));
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
            for name in ["claude.cmd", "claude.exe", "claude"] {
                out.push(dir.join(name));
            }
        }
    }
    out
}

/// Validate a candidate by running `<candidate> --version` and waiting for it
/// to exit. These invocations are fast, so a plain blocking wait suffices.
fn probe_version(binary: &Path) -> std::result::Result<String, String> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("could not run --version: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(format!("--version exited with {}", output.status))
    }
}

// ---------------------------------------------------------------------------
// Minimal config-dir entry set (worker-sandboxing open question 1)
// ---------------------------------------------------------------------------

/// The credential entry the `claude` CLI reads for file-based (non-Keychain)
/// OAuth auth, relative to `CLAUDE_CONFIG_DIR` (default `$HOME/.claude`).
pub const CLAUDE_CREDENTIALS_ENTRY: &str = ".credentials.json";

/// Single source of truth for the minimal `CLAUDE_CONFIG_DIR` entry set a
/// scratch worker HOME/config dir needs to carry so the `claude` CLI can
/// authenticate and run headless (`-p --output-format stream-json`).
///
/// See `docs/scoping/claude-cli-min-env.md` for the full probe: why
/// `.credentials.json` is required for file-based auth but irrelevant when
/// auth comes from the macOS Keychain or `ANTHROPIC_API_KEY`, and why
/// `CLAUDE_CONFIG_DIR` relocation does not also relocate `$HOME/.claude.json`
/// (a worker HOME must be set too for that file to land in the sandbox).
///
/// Names only — never actual secret values. Consumed by the (not-yet-built)
/// scratch-HOME-seeding feature; not wired into spawning here.
pub fn claude_min_config_entries() -> &'static [&'static str] {
    &[CLAUDE_CREDENTIALS_ENTRY]
}

// ---------------------------------------------------------------------------
// Argument construction
// ---------------------------------------------------------------------------

/// Build the argv (excluding the binary itself) for one session.
///
/// Public so tests can assert the exact CLI wire format without spawning.
pub fn build_args(spec: &SessionSpec) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-p".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--verbose".into(),
        "--model".into(),
        spec.model.clone(),
        "--effort".into(),
        spec.effort.clone(),
    ];
    if let Some(system) = &spec.append_system_prompt {
        args.push("--append-system-prompt".into());
        args.push(system.clone());
    }
    match &spec.resume {
        Some(previous) => {
            args.push("--resume".into());
            args.push(previous.clone());
        }
        None => {
            args.push("--session-id".into());
            args.push(spec.session_id.clone());
        }
    }
    if let Some(mode) = &spec.permission_mode {
        args.push("--permission-mode".into());
        args.push(mode.clone());
    }
    if !spec.allowed_tools.is_empty() {
        args.push("--allowedTools".into());
        args.extend(spec.allowed_tools.iter().cloned());
    }
    if !spec.disallowed_tools.is_empty() {
        args.push("--disallowedTools".into());
        args.extend(spec.disallowed_tools.iter().cloned());
    }
    if !spec.tools.is_empty() {
        args.push("--tools".into());
        args.extend(spec.tools.iter().cloned());
    }
    if let Some(settings) = &spec.settings_json {
        args.push("--settings".into());
        args.push(settings.to_string()); // compact JSON
    }
    if let Some(schema) = &spec.json_schema {
        args.push("--json-schema".into());
        args.push(schema.to_string()); // compact JSON
    }
    if let Some(budget) = spec.max_budget_usd {
        args.push("--max-budget-usd".into());
        args.push(budget.to_string());
    }
    match &spec.prompt {
        PromptMode::Streaming(_) => {
            // Initial prompt goes via stdin as a stream-json user message.
            args.push("--input-format".into());
            args.push("stream-json".into());
        }
        PromptMode::SingleShot(prompt) => {
            // Positional prompt must be the last argument.
            args.push(prompt.clone());
        }
    }
    args
}

/// One stdin line injecting a user message into a streaming-input session.
pub fn user_message_line(text: &str) -> String {
    let value = json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{ "type": "text", "text": text }],
        },
    });
    format!("{value}\n")
}

// ---------------------------------------------------------------------------
// Stream-json line parsing
// ---------------------------------------------------------------------------

/// Parse one stdout line into zero or more [`AgentEvent`]s.
///
/// Unparseable lines become [`AgentEvent::Other`] with
/// `raw = {"unparsed": <line>}` so nothing is ever dropped from transcripts.
pub fn parse_stream_line(line: &str) -> Vec<AgentEvent> {
    match serde_json::from_str::<Value>(line) {
        Ok(value) => parse_stream_value(value),
        Err(_) => vec![AgentEvent::Other {
            raw: json!({ "unparsed": line }),
        }],
    }
}

/// Map one parsed stream-json value to events (see module docs / fixture).
pub fn parse_stream_value(value: Value) -> Vec<AgentEvent> {
    let line_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    match line_type {
        "system" if value.get("subtype").and_then(Value::as_str) == Some("init") => {
            vec![AgentEvent::Init {
                session_id: str_field(&value, "session_id"),
                model: str_field(&value, "model"),
                raw: value,
            }]
        }
        "assistant" => parse_assistant(value),
        "user" => parse_user(value),
        "result" => vec![parse_result(value)],
        _ => vec![AgentEvent::Other { raw: value }],
    }
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One event per content block: text → `Text` (empty skipped), tool_use →
/// `ToolUse`, anything else (thinking, ...) → `Other`. Every event carries
/// the full raw line.
fn parse_assistant(value: Value) -> Vec<AgentEvent> {
    let Some(blocks) = value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .cloned()
    else {
        return vec![AgentEvent::Other { raw: value }];
    };
    let mut events = Vec::new();
    for block in &blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                if !text.is_empty() {
                    events.push(AgentEvent::Text {
                        text: text.to_string(),
                        raw: value.clone(),
                    });
                }
            }
            Some("tool_use") => {
                let tool = block
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let summary = tool_use_summary(&tool, block.get("input"));
                events.push(AgentEvent::ToolUse {
                    tool,
                    summary,
                    raw: value.clone(),
                });
            }
            _ => events.push(AgentEvent::Other { raw: value.clone() }),
        }
    }
    events
}

/// Human-readable summary of a tool invocation: the command for Bash, the
/// file path for Edit/Write/Read, else the compact input JSON (truncated).
fn tool_use_summary(tool: &str, input: Option<&Value>) -> String {
    let null = Value::Null;
    let input = input.unwrap_or(&null);
    let picked = match tool {
        "Bash" => input.get("command").and_then(Value::as_str),
        "Edit" | "Write" | "Read" => input.get("file_path").and_then(Value::as_str),
        _ => None,
    };
    match picked {
        Some(text) => text.to_string(),
        None => truncate_chars(&input.to_string(), SUMMARY_MAX_CHARS),
    }
}

/// `type: "user"` lines carry tool results echoed back to the model. Each
/// `tool_result` block becomes a `ToolResult`; the `denied` heuristic flags
/// permission-rule and hook blocks (§4.7 guardrail surfacing).
fn parse_user(value: Value) -> Vec<AgentEvent> {
    let blocks = value
        .pointer("/message/content")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut events = Vec::new();
    for block in &blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let text = tool_result_text(block);
        let is_error = block
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let lower = text.to_lowercase();
        let denied = (is_error && lower.contains("permission")) || lower.contains("hook");
        events.push(AgentEvent::ToolResult {
            tool: None,
            denied,
            summary: truncate_chars(&text, SUMMARY_MAX_CHARS),
            raw: value.clone(),
        });
    }
    if events.is_empty() {
        return vec![AgentEvent::Other { raw: value }];
    }
    events
}

/// A tool_result `content` is either a plain string or an array of
/// `{type:"text", text}` parts.
fn tool_result_text(block: &Value) -> String {
    match block.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    part.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn parse_result(value: Value) -> AgentEvent {
    let usage_field = |key: &str| {
        value
            .pointer(&format!("/usage/{key}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    AgentEvent::Result {
        text: value
            .get("result")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        is_error: value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        usage: TokenUsage {
            input: usage_field("input_tokens"),
            output: usage_field("output_tokens"),
            cache_read: usage_field("cache_read_input_tokens"),
            cache_write: usage_field("cache_creation_input_tokens"),
        },
        cost_usd: value.get("total_cost_usd").and_then(Value::as_f64),
        num_turns: value
            .get("num_turns")
            .and_then(Value::as_u64)
            .map(|n| n as u32),
        raw: value,
    }
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

/// The real [`AgentBackend`]: spawns the `claude` CLI headless.
#[derive(Debug, Clone)]
pub struct ClaudeBackend {
    binary: PathBuf,
}

impl ClaudeBackend {
    /// Use an explicit binary path (no validation performed).
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        ClaudeBackend {
            binary: binary.into(),
        }
    }

    /// Discover the binary via [`discover_claude_binary`].
    pub fn discover(configured: Option<&str>) -> Result<Self> {
        Ok(ClaudeBackend {
            binary: discover_claude_binary(configured)?,
        })
    }

    /// The binary this backend spawns.
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

#[async_trait::async_trait]
impl AgentBackend for ClaudeBackend {
    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>> {
        let streaming = matches!(spec.prompt, PromptMode::Streaming(_));
        let args = build_args(&spec);

        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(&args)
            .current_dir(&spec.cwd)
            .envs(&spec.env)
            .stdin(if streaming {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Unix: make the child the leader of a fresh process group so aborts
        // can kill the whole tree — tool subprocesses (test runners, builds)
        // die with the CLI instead of surviving an interrupt/turn-budget
        // abort. See [`ClaudeSession::kill_child`].
        // Windows has no process groups; the equivalent (a Job Object with
        // KILL_ON_JOB_CLOSE) is created *after* spawn, below.
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn().map_err(|e| {
            EngineError::Backend(format!("failed to spawn {}: {e}", self.binary.display()))
        })?;

        // Windows: assign the child to a kill-on-close Job Object so its whole
        // descendant tree (tool children — test runners, builds) dies on
        // abort/turn-budget kill, mirroring the unix process-group behaviour.
        // Job setup failure is non-fatal: the child still runs, just without
        // tree-kill (identical to the pre-Job-Object behaviour). Behind
        // cfg(windows); compiled and validated only on windows-latest CI.
        #[cfg(windows)]
        let job = match child.raw_handle() {
            Some(handle) => match win_job::JobHandle::create_and_assign(handle) {
                Ok(job) => Some(job),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to create Job Object for claude child; \
                        tree-kill on abort will be unavailable");
                    None
                }
            },
            // The child already exited between spawn and here — nothing to
            // assign; kill_child falls back to the direct reap.
            None => None,
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::Backend("claude child has no stdout pipe".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| EngineError::Backend("claude child has no stderr pipe".to_string()))?;
        let mut stdin = if streaming { child.stdin.take() } else { None };

        // Capture stderr concurrently so a chatty child never blocks on a
        // full pipe and failure messages can include the tail.
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_task = {
            let buf = Arc::clone(&stderr_buf);
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut guard = buf.lock().expect("stderr buffer lock");
                    guard.push_str(&line);
                    guard.push('\n');
                }
            })
        };

        if let PromptMode::Streaming(initial) = &spec.prompt {
            let Some(handle) = stdin.as_mut() else {
                return Err(EngineError::Backend(
                    "claude child has no stdin pipe for streaming input".to_string(),
                ));
            };
            handle
                .write_all(user_message_line(initial).as_bytes())
                .await?;
            handle.flush().await?;
        }

        Ok(Box::new(ClaudeSession {
            session_id: spec.session_id.clone(),
            streaming,
            max_turns: spec.max_turns,
            child,
            #[cfg(windows)]
            job,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr_buf,
            stderr_task: Some(stderr_task),
            queue: VecDeque::new(),
            assistant_ids: HashSet::new(),
            saw_result: false,
            saw_success_result: false,
            exit: None,
        }))
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// Send SIGKILL to the process group `pgid`. Returns whether the signal was
/// delivered to at least one process (false means the group is gone).
#[cfg(unix)]
pub(crate) fn kill_group(pgid: i32) -> bool {
    debug_assert!(pgid > 0, "kill_group needs a positive group id");
    // SAFETY: kill(2) takes a pid and a signal number; no pointers or shared
    // state are involved. A negative pid targets the whole process group.
    unsafe { libc::kill(-pgid, libc::SIGKILL) == 0 }
}

/// A live `claude` CLI session (the [`AgentSession`] impl).
pub struct ClaudeSession {
    /// Updated by the last `system/init` seen; defaults to the spec value.
    session_id: String,
    streaming: bool,
    max_turns: Option<u32>,
    child: Child,
    /// Windows only: the kill-on-close Job Object owning the child's process
    /// tree. Ordered *after* `child` so `child` drops first (Rust drops fields
    /// top-to-bottom); either order is safe, but killing the tree after the
    /// child's own `kill_on_drop` is the tidier sequence. Dropping this guard
    /// closes the job handle, which (via `KILL_ON_JOB_CLOSE`) also terminates
    /// any surviving descendants. `None` if job setup failed at spawn.
    /// Compiled and validated only on windows-latest CI.
    #[cfg(windows)]
    job: Option<win_job::JobHandle>,
    /// Held open for streaming-input sessions; dropped to close stdin.
    stdin: Option<ChildStdin>,
    lines: Lines<BufReader<ChildStdout>>,
    stderr_buf: Arc<Mutex<String>>,
    stderr_task: Option<JoinHandle<()>>,
    /// Multi-block lines queue several events; popped one per `next_event`.
    queue: VecDeque<AgentEvent>,
    /// Distinct assistant message ids seen (engine-enforced turn budget).
    assistant_ids: HashSet<String>,
    saw_result: bool,
    saw_success_result: bool,
    exit: Option<SessionExit>,
}

impl ClaudeSession {
    /// Record bookkeeping the session derives from its own event stream.
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
            _ => {}
        }
    }

    /// True when this line pushes the distinct-assistant-id count over the
    /// turn budget.
    fn over_turn_budget(&mut self, value: &Value) -> bool {
        let Some(max_turns) = self.max_turns else {
            return false;
        };
        if value.get("type").and_then(Value::as_str) != Some("assistant") {
            return false;
        }
        let Some(id) = value.pointer("/message/id").and_then(Value::as_str) else {
            return false;
        };
        if self.assistant_ids.insert(id.to_string()) {
            self.assistant_ids.len() > max_turns as usize
        } else {
            false
        }
    }

    /// Kill the child and reap it, best-effort; also closes stdin and joins
    /// the stderr capture task.
    ///
    /// Unix: the child was spawned as the leader of its own process group
    /// (`process_group(0)` in [`ClaudeBackend::start`]), so SIGKILL is sent
    /// to the whole group via `kill(-pid, SIGKILL)` — tool subprocesses
    /// (test runners, builds) die with the CLI. A first group kill can race
    /// a concurrent `fork` inside the group (the mid-fork child misses the
    /// signal), so after reaping the leader — membership is stable then —
    /// the group is swept with a second SIGKILL. When the group kill fails
    /// (e.g. the child is already reaped), the direct `start_kill` is the
    /// fallback.
    ///
    /// Windows: the child was assigned to a kill-on-close Job Object at spawn
    /// (see [`ClaudeBackend::start`]). `TerminateJobObject` kills every process
    /// in the job — the CLI and its whole tool-child tree — then the child is
    /// reaped. If job setup had failed (`job == None`) this degrades to the
    /// old direct-child `start_kill`. The Job Object block compiles and is
    /// validated only on windows-latest CI, never on the dev host.
    async fn kill_child(&mut self) {
        self.stdin = None;
        #[cfg(unix)]
        {
            // `id()` is None once the child has been reaped; the leader's
            // pid doubles as the group id (`process_group(0)` at spawn).
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
                    // Sweep stragglers that raced the first kill mid-fork.
                    let _ = kill_group(pgid);
                }
            }
        }
        #[cfg(windows)]
        {
            // Kill the whole tree via the job; fall back to the direct child
            // if job setup had failed at spawn. Then reap the CLI so its pipes
            // (and the stderr capture task below) close.
            match &self.job {
                Some(job) => job.kill(),
                None => {
                    let _ = self.child.start_kill();
                }
            }
            let _ = self.child.wait().await;
        }
        // Any other (hypothetical) non-unix, non-windows target: direct child
        // kill only, no tree semantics available.
        #[cfg(all(not(unix), not(windows)))]
        {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
        }
        if let Some(task) = self.stderr_task.take() {
            let _ = task.await;
        }
    }

    /// stdout hit EOF: reap the child and classify the exit.
    async fn finish_at_eof(&mut self) {
        self.stdin = None;
        let status = self.child.wait().await;
        // The stderr pipe closes with the process, so the capture task is
        // about to finish; join it before reading the buffer.
        if let Some(task) = self.stderr_task.take() {
            let _ = task.await;
        }
        let exit = match status {
            Ok(status) if status.success() && self.saw_result => SessionExit::Completed,
            Ok(status) => SessionExit::Failed(format!(
                "claude exited with {status}{}; stderr tail: {}",
                if self.saw_result {
                    ""
                } else {
                    " without emitting a result message"
                },
                self.stderr_tail(),
            )),
            Err(e) => SessionExit::Failed(format!(
                "failed to reap claude process: {e}; stderr tail: {}",
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
impl AgentSession for ClaudeSession {
    fn session_id(&self) -> String {
        self.session_id.clone()
    }

    async fn next_event(&mut self) -> Result<Option<AgentEvent>> {
        loop {
            // Drain queued events first — even after an abort, so multi-block
            // lines already parsed are never lost.
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
                        "error reading claude stdout: {e}; stderr tail: {}",
                        self.stderr_tail(),
                    )));
                    return Ok(None);
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(_) => {
                    self.queue.push_back(AgentEvent::Other {
                        raw: json!({ "unparsed": line }),
                    });
                    continue;
                }
            };
            if self.over_turn_budget(&value) {
                // Engine-enforced turn budget (the CLI has no --max-turns):
                // abort internally; the over-budget message is not emitted.
                self.kill_child().await;
                self.exit = Some(SessionExit::Aborted);
                continue; // queue is empty here → next iteration returns None
            }
            let events = parse_stream_value(value);
            for event in &events {
                self.observe(event);
            }
            self.queue.extend(events);
        }
    }

    async fn send_user_message(&mut self, text: &str) -> Result<()> {
        if !self.streaming {
            return Err(EngineError::Backend(
                "send_user_message on a single-shot session".to_string(),
            ));
        }
        let Some(stdin) = self.stdin.as_mut() else {
            return Err(EngineError::Backend(
                "send_user_message on a closed session (stdin dropped)".to_string(),
            ));
        };
        stdin.write_all(user_message_line(text).as_bytes()).await?;
        stdin.flush().await?;
        Ok(())
    }

    async fn abort(&mut self) -> Result<()> {
        // Whether the process had already exited on its own before we killed
        // it (an abort after natural completion keeps Completed).
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
