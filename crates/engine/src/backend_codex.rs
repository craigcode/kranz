//! Codex agent backend: drives `codex exec --json` headless.
//!
//! Ground truth is `crates/engine/tests/fixtures/codex_exec_scrutiny.jsonl`,
//! a recorded `codex exec --json` transcript. This module is single-shot
//! only: unlike `backend_claude`, there is no `--resume` and no
//! streaming-input mode, so [`CodexSession::send_user_message`] and a
//! `resume`d [`SessionSpec`] are both rejected at the seam rather than
//! translated into codex flags.
//!
//! Several `SessionSpec` fields are claude-isms with no codex equivalent and
//! are deliberately ignored when building argv: `json_schema`,
//! `max_budget_usd`, `resume`, `permission_mode`, `allowed_tools` /
//! `disallowed_tools`, `tools`, `settings_json`, `effort`.

use crate::backend::{
    AgentBackend, AgentEvent, AgentSession, PromptMode, SessionExit, SessionSpec,
};
#[cfg(unix)]
use crate::backend_claude::kill_group;
#[cfg(windows)]
use crate::backend_claude::win_job;
use crate::cost;
use crate::error::{EngineError, Result};
use crate::types::TokenUsage;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout};
use tokio::task::JoinHandle;

/// Max characters kept in tool-use / tool-result summaries.
const SUMMARY_MAX_CHARS: usize = 200;
/// Max characters of captured stderr included in failure messages.
const STDERR_TAIL_CHARS: usize = 500;

// ---------------------------------------------------------------------------
// Binary discovery
// ---------------------------------------------------------------------------

/// Locate a working `codex` binary.
///
/// Order: `configured` → `KRANZ_CODEX_BIN` env var → `codex` on PATH →
/// well-known install locations. Each candidate is validated by running it
/// with `--version`; the first one that succeeds wins. Errors list every
/// attempt so the user can see what was tried.
///
/// `KRANZ_CODEX_BIN`, when set and non-empty, is an *exclusive* override: only
/// that path is probed, and a failure is returned immediately rather than
/// falling through to PATH or the well-known fallback locations. Naming the
/// binary explicitly and having it not work is an error, not a reason to
/// search elsewhere.
pub fn discover_codex_binary(configured: Option<&str>) -> Result<PathBuf> {
    if let Some(env_bin) = std::env::var_os("KRANZ_CODEX_BIN") {
        if !env_bin.is_empty() {
            let candidate = PathBuf::from(env_bin);
            return match probe_version(&candidate) {
                Ok(_version) => Ok(candidate),
                Err(why) => Err(EngineError::Config(format!(
                    "KRANZ_CODEX_BIN points at {} which did not work: {why}",
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
    // rules per-platform).
    candidates.push(PathBuf::from("codex"));
    #[cfg(windows)]
    {
        candidates.push(PathBuf::from("codex.cmd"));
        candidates.push(PathBuf::from("codex.exe"));
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
        "no working codex binary found; tried: {}. Install Codex CLI \
         (npm install -g @openai/codex) or point kranz at it via the \
         validatorScrutiny.codexBinary config field or the KRANZ_CODEX_BIN \
         environment variable.",
        attempts.join(", ")
    )))
}

/// Well-known install locations checked after PATH.
#[cfg(not(windows))]
fn fallback_candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut out = Vec::new();
    if let Some(home) = &home {
        out.push(home.join(".npm-global").join("bin").join("codex"));
    }
    out.push(PathBuf::from("/opt/homebrew/bin/codex"));
    out.push(PathBuf::from("/usr/local/bin/codex"));
    if let Some(home) = &home {
        out.push(home.join(".local").join("bin").join("codex"));
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
            for name in ["codex.cmd", "codex.exe", "codex"] {
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

/// The prompt text codex actually receives: `append_system_prompt` (if any)
/// concatenated ahead of the prompt text — codex has no
/// `--append-system-prompt` flag, so the engine folds it into the single
/// positional PROMPT argument instead.
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

/// Build a TOML basic string literal (double-quoted with escapes) for a
/// path that may contain spaces, backslashes, or single quotes.
fn toml_basic_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Build the argv (excluding the binary itself) for one session.
///
/// Public so tests can assert the exact CLI wire format without spawning.
/// Deliberately ignores every claude-only `SessionSpec` field: `json_schema`,
/// `max_budget_usd`, `resume`, `permission_mode`, `allowed_tools` /
/// `disallowed_tools`, `tools`, `settings_json`, `effort`.
pub fn build_args(spec: &SessionSpec) -> Vec<String> {
    let sandbox = if spec.writable {
        "workspace-write"
    } else {
        "read-only"
    };
    let mut args = vec![
        "exec".into(),
        "--json".into(),
        "--sandbox".into(),
        sandbox.into(),
    ];
    // Temp-dir worktrees (macOS /var/folders → /private/var) are outside
    // Codex's default workspace-write roots. Pin the session cwd explicitly
    // so workers can land deliverables (fix-codex-sandbox-writable-roots-worktree).
    if spec.writable {
        // TOML basic string (double-quoted) — literal `'…'` has no escapes
        // and breaks on paths containing `'`.
        let root = toml_basic_string(&spec.cwd.display().to_string());
        args.push("-c".into());
        args.push(format!("sandbox_workspace_write.writable_roots=[{root}]"));
    }
    args.push("--model".into());
    args.push(spec.model.clone());
    args.push(effective_prompt(spec));
    args
}

// ---------------------------------------------------------------------------
// codex exec --json line parsing
// ---------------------------------------------------------------------------

/// Parse one stdout line into zero or more [`AgentEvent`]s. `model` is the
/// configured model, used both as the `Init` fallback (codex's
/// `thread.started` carries no model field in observed output) and as the
/// pricing key when a terminal event has no CLI-reported dollar cost.
///
/// Unparseable lines become [`AgentEvent::Other`] with
/// `raw = {"unparsed": <line>}` so nothing is ever dropped from transcripts.
pub fn parse_codex_line(line: &str, model: &str) -> Vec<AgentEvent> {
    match serde_json::from_str::<Value>(line) {
        Ok(value) => parse_codex_value(value, model),
        Err(_) => vec![AgentEvent::Other {
            raw: json!({ "unparsed": line }),
        }],
    }
}

/// Map one parsed `codex exec --json` value to events (see module docs /
/// fixture).
pub fn parse_codex_value(value: Value, model: &str) -> Vec<AgentEvent> {
    let line_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    match line_type {
        "thread.started" => vec![AgentEvent::Init {
            session_id: str_field(&value, "thread_id"),
            model: value
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(model)
                .to_string(),
            raw: value,
        }],
        "item.started" if item_type(&value) == "command_execution" => {
            let command = value
                .pointer("/item/command")
                .and_then(Value::as_str)
                .unwrap_or("");
            vec![AgentEvent::ToolUse {
                tool: "command_execution".to_string(),
                summary: truncate_chars(command, SUMMARY_MAX_CHARS),
                raw: value,
            }]
        }
        "item.completed" if item_type(&value) == "command_execution" => {
            let output = value
                .pointer("/item/aggregated_output")
                .and_then(Value::as_str)
                .unwrap_or("");
            // A sandbox refusal reports a null exit_code alongside
            // status == "failed" (see docs/scoping/codex-backend.md); a
            // command that merely exits non-zero has a real exit_code and is
            // a normal failure, not a denial.
            let exit_code_is_null = value
                .pointer("/item/exit_code")
                .map(Value::is_null)
                .unwrap_or(true);
            let status = value
                .pointer("/item/status")
                .and_then(Value::as_str)
                .unwrap_or("");
            let denied = exit_code_is_null && status == "failed";
            vec![AgentEvent::ToolResult {
                tool: Some("command_execution".to_string()),
                denied,
                summary: truncate_chars(output, SUMMARY_MAX_CHARS),
                raw: value,
            }]
        }
        "item.completed" if item_type(&value) == "agent_message" => {
            let text = value
                .pointer("/item/text")
                .and_then(Value::as_str)
                .unwrap_or("");
            if text.is_empty() {
                vec![AgentEvent::Other { raw: value }]
            } else {
                vec![AgentEvent::Text {
                    text: text.to_string(),
                    raw: value,
                }]
            }
        }
        "turn.completed" => vec![parse_terminal(value, model)],
        _ => vec![AgentEvent::Other { raw: value }],
    }
}

fn item_type(value: &Value) -> &str {
    value
        .pointer("/item/type")
        .and_then(Value::as_str)
        .unwrap_or("")
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

// ---------------------------------------------------------------------------
// Stateful stream parsing (stitches agent_message text into the terminal
// Result — see module docs / docs/scoping/codex-backend.md)
// ---------------------------------------------------------------------------

/// Stateful wrapper around [`parse_codex_line`] that remembers the most
/// recent `agent_message` [`AgentEvent::Text`] and stitches it into the
/// terminal [`AgentEvent::Result`] when a `turn.completed` line arrives.
///
/// `turn.completed` carries no text of its own; the final `agent_message` of
/// the turn is the codex analogue of Claude's terminal result text (the
/// validator report JSON), so "last agent_message wins".
#[derive(Debug, Default)]
pub struct CodexStreamParser {
    last_text: Option<String>,
}

impl CodexStreamParser {
    pub fn new() -> Self {
        CodexStreamParser::default()
    }

    /// Parse one stdout line, filling in any remembered `agent_message` text
    /// on a terminal `Result` event.
    pub fn push(&mut self, line: &str, model: &str) -> Vec<AgentEvent> {
        parse_codex_line(line, model)
            .into_iter()
            .map(|event| self.observe(event))
            .collect()
    }

    fn observe(&mut self, event: AgentEvent) -> AgentEvent {
        match event {
            AgentEvent::Text { text, raw } => {
                self.last_text = Some(text.clone());
                AgentEvent::Text { text, raw }
            }
            AgentEvent::Result {
                text,
                is_error,
                usage,
                cost_usd,
                num_turns,
                raw,
            } if text.is_empty() => AgentEvent::Result {
                text: self.last_text.take().unwrap_or_default(),
                is_error,
                usage,
                cost_usd,
                num_turns,
                raw,
            },
            other => other,
        }
    }
}

fn parse_terminal(value: Value, model: &str) -> AgentEvent {
    let usage_field = |key: &str| {
        value
            .pointer(&format!("/usage/{key}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    // Reasoning tokens are output tokens for billing purposes; codex reports
    // them as a separate `reasoning_output_tokens` field alongside
    // `output_tokens`.
    let usage = TokenUsage {
        input: usage_field("input_tokens"),
        output: usage_field("output_tokens") + usage_field("reasoning_output_tokens"),
        cache_read: usage_field("cached_input_tokens"),
        cache_write: 0,
    };
    let cost_usd = value
        .get("total_cost_usd")
        .and_then(Value::as_f64)
        .or_else(|| value.get("cost_usd").and_then(Value::as_f64))
        .or_else(|| Some(cost::usage_cost_usd(&usage, model)));
    AgentEvent::Result {
        text: String::new(),
        is_error: value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        usage,
        cost_usd,
        num_turns: Some(1),
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

/// The [`AgentBackend`] for `codex exec --json`: single-shot with sandbox
/// mode selected from the session role.
#[derive(Debug, Clone)]
pub struct CodexBackend {
    binary: PathBuf,
}

impl CodexBackend {
    /// Use an explicit binary path (no validation performed).
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        CodexBackend {
            binary: binary.into(),
        }
    }

    /// Discover the binary via [`discover_codex_binary`].
    pub fn discover(configured: Option<&str>) -> Result<Self> {
        Ok(CodexBackend {
            binary: discover_codex_binary(configured)?,
        })
    }

    /// The binary this backend spawns.
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

#[async_trait::async_trait]
impl AgentBackend for CodexBackend {
    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>> {
        if spec.resume.is_some() {
            return Err(EngineError::Backend(
                "codex backend is single-shot only; resume is unsupported".to_string(),
            ));
        }
        let model = spec.model.clone();
        let args = build_args(&spec);

        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(&args)
            .current_dir(&spec.cwd)
            .envs(&spec.env)
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
                    tracing::warn!(error = %e, "failed to create Job Object for codex child; \
                        tree-kill on abort will be unavailable");
                    None
                }
            },
            None => None,
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::Backend("codex child has no stdout pipe".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| EngineError::Backend("codex child has no stderr pipe".to_string()))?;

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

        Ok(Box::new(CodexSession {
            session_id: spec.session_id.clone(),
            model,
            child,
            #[cfg(windows)]
            job,
            lines: BufReader::new(stdout).lines(),
            stderr_buf,
            stderr_task: Some(stderr_task),
            queue: VecDeque::new(),
            stream_parser: CodexStreamParser::new(),
            saw_result: false,
            saw_success_result: false,
            exit: None,
        }))
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A live `codex exec --json` session (the [`AgentSession`] impl).
///
/// Single-shot only: [`send_user_message`](AgentSession::send_user_message)
/// always errors, and there is no streaming stdin to hold open.
pub struct CodexSession {
    session_id: String,
    model: String,
    child: Child,
    #[cfg(windows)]
    job: Option<win_job::JobHandle>,
    lines: Lines<BufReader<ChildStdout>>,
    stderr_buf: Arc<Mutex<String>>,
    stderr_task: Option<JoinHandle<()>>,
    /// Multi-block lines queue several events; popped one per `next_event`.
    queue: VecDeque<AgentEvent>,
    stream_parser: CodexStreamParser,
    saw_result: bool,
    saw_success_result: bool,
    exit: Option<SessionExit>,
}

impl CodexSession {
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
        let exit = match status {
            Ok(status) if status.success() && self.saw_result => SessionExit::Completed,
            Ok(status) => SessionExit::Failed(format!(
                "codex exited with {status}{}; stderr tail: {}",
                if self.saw_result {
                    ""
                } else {
                    " without emitting a terminal event"
                },
                self.stderr_tail(),
            )),
            Err(e) => SessionExit::Failed(format!(
                "failed to reap codex process: {e}; stderr tail: {}",
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
impl AgentSession for CodexSession {
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
                        "error reading codex stdout: {e}; stderr tail: {}",
                        self.stderr_tail(),
                    )));
                    return Ok(None);
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let events = self.stream_parser.push(&line, &self.model);
            for event in &events {
                self.observe(event);
            }
            self.queue.extend(events);
        }
    }

    async fn send_user_message(&mut self, _text: &str) -> Result<()> {
        Err(EngineError::Backend(
            "codex backend is single-shot only; send_user_message is unsupported".to_string(),
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
    use crate::cost::DEFAULT_CODEX_MODEL;

    #[test]
    #[cfg(unix)]
    fn probe_version_kills_a_hung_binary_within_the_deadline() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("hung-codex");
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

    fn fixture_lines() -> Vec<String> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("codex_exec_scrutiny.jsonl");
        std::fs::read_to_string(path)
            .expect("read fixture")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.to_string())
            .collect()
    }

    #[test]
    fn backend_codex_parse_fixture() {
        let mut events: Vec<AgentEvent> = Vec::new();
        for line in fixture_lines() {
            events.extend(parse_codex_line(&line, DEFAULT_CODEX_MODEL));
        }

        assert!(
            events.iter().any(
                |e| matches!(e, AgentEvent::Init { session_id, .. } if !session_id.is_empty())
            ),
            "expected an Init event with a non-empty session id"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Text { text, .. } if !text.is_empty())),
            "expected at least one Text event"
        );
        assert!(
            events.iter().any(
                |e| matches!(e, AgentEvent::ToolUse { tool, .. } if tool == "command_execution")
            ),
            "expected a ToolUse event with tool == \"command_execution\""
        );
        assert!(
            events.iter().any(
                |e| matches!(e, AgentEvent::ToolResult { tool, .. } if tool.as_deref() == Some("command_execution"))
            ),
            "expected a ToolResult event with tool == Some(\"command_execution\")"
        );

        let terminal = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Result {
                    usage,
                    cost_usd,
                    num_turns,
                    ..
                } => Some((usage, cost_usd, num_turns)),
                _ => None,
            })
            .expect("expected a terminal Result event");
        let (usage, cost_usd, num_turns) = terminal;
        assert!(
            usage.input > 0 || usage.output > 0 || usage.cache_read > 0,
            "expected non-zero usage on the terminal Result"
        );
        assert!(cost_usd.is_some(), "expected cost_usd to be Some");
        assert_eq!(
            *num_turns,
            Some(1),
            "expected the terminal Result's num_turns to be Some(1)"
        );
    }

    #[test]
    fn command_execution_denied_derives_from_structured_fields_not_output_text() {
        let completed = json!({
            "type": "item.completed",
            "item": {
                "type": "command_execution",
                "command": "grep foo bar.txt",
                "aggregated_output": "",
                "exit_code": 0,
                "status": "completed"
            }
        });
        let events = parse_codex_value(completed, DEFAULT_CODEX_MODEL);
        match &events[0] {
            AgentEvent::ToolResult { denied, .. } => {
                assert!(
                    !denied,
                    "a real exit_code with status completed must not be denied"
                )
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }

        let refused = json!({
            "type": "item.completed",
            "item": {
                "type": "command_execution",
                "command": "rm -rf /",
                "aggregated_output": "",
                "exit_code": null,
                "status": "failed"
            }
        });
        let events = parse_codex_value(refused, DEFAULT_CODEX_MODEL);
        match &events[0] {
            AgentEvent::ToolResult { denied, .. } => {
                assert!(
                    *denied,
                    "a null exit_code with status failed must be denied"
                )
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn backend_codex_stream_parser_stitches_terminal_text() {
        let mut parser = CodexStreamParser::new();
        let mut events: Vec<AgentEvent> = Vec::new();
        for line in fixture_lines() {
            events.extend(parser.push(&line, DEFAULT_CODEX_MODEL));
        }

        let terminal_text = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Result { text, .. } => Some(text.clone()),
                _ => None,
            })
            .expect("expected a terminal Result event");
        assert!(
            !terminal_text.is_empty(),
            "expected the terminal Result text to be stitched from the last agent_message"
        );

        let report = crate::runner::parse_validator_report(&terminal_text)
            .expect("terminal text should parse as a ValidatorReport");
        assert!(
            !report.findings.is_empty(),
            "expected the fixture's ValidatorReport to have findings"
        );
    }

    #[test]
    fn build_args_ignores_claude_only_fields() {
        let spec = SessionSpec {
            cwd: PathBuf::from("."),
            prompt: PromptMode::SingleShot("do the thing".to_string()),
            append_system_prompt: Some("be terse".to_string()),
            model: "gpt-5-codex".to_string(),
            effort: "high".to_string(),
            session_id: "sess-1".to_string(),
            resume: None,
            permission_mode: Some("acceptEdits".to_string()),
            allowed_tools: vec!["Bash(npm test*)".to_string()],
            disallowed_tools: vec!["Bash(git push*)".to_string()],
            tools: vec!["Bash".to_string()],
            writable: false,
            settings_json: Some(json!({"hooks": {}})),
            json_schema: Some(json!({"type": "object"})),
            max_budget_usd: Some(5.0),
            max_turns: Some(10),
            env: Default::default(),
            sandbox: None,
        };
        let args = build_args(&spec);
        assert_eq!(
            args,
            vec![
                "exec".to_string(),
                "--json".to_string(),
                "--sandbox".to_string(),
                "read-only".to_string(),
                "--model".to_string(),
                "gpt-5-codex".to_string(),
                "be terse\n\ndo the thing".to_string(),
            ]
        );
    }

    #[test]
    fn build_args_uses_workspace_write_for_writable_sessions() {
        let spec = SessionSpec {
            cwd: PathBuf::from("."),
            prompt: PromptMode::SingleShot("do the thing".to_string()),
            append_system_prompt: None,
            model: "gpt-5-codex".to_string(),
            effort: "high".to_string(),
            session_id: "sess-1".to_string(),
            resume: None,
            permission_mode: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
            tools: vec![],
            writable: true,
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: Default::default(),
            sandbox: None,
        };
        let args = build_args(&spec);
        assert_eq!(
            args,
            vec![
                "exec".to_string(),
                "--json".to_string(),
                "--sandbox".to_string(),
                "workspace-write".to_string(),
                "-c".to_string(),
                "sandbox_workspace_write.writable_roots=[\".\"]".to_string(),
                "--model".to_string(),
                "gpt-5-codex".to_string(),
                "do the thing".to_string(),
            ]
        );
    }
}
