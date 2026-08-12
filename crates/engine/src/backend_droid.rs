//! Droid agent backend: drives Factory `droid exec -o json` headless.
//!
//! Ground truth is `crates/engine/tests/fixtures/droid_exec_scrutiny.json`, a
//! recorded `droid exec -o json` result object. Unlike codex/claude, droid
//! `-o json` emits a SINGLE result object rather than a stream — there is no
//! stitching required. This module is single-shot only:
//! [`DroidSession::send_user_message`] and a `resume`d [`SessionSpec`] are
//! both rejected at the seam rather than translated into droid flags.
//!
//! Several `SessionSpec` fields are claude-isms with no droid equivalent and
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
use crate::stream_bounds::{drain_to_tail, BoundedLines, STDERR_TAIL_CAP};
use crate::types::TokenUsage;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::process::{Child, ChildStdout};
use tokio::task::JoinHandle;

/// Max characters of captured stderr included in failure messages.
const STDERR_TAIL_CHARS: usize = 500;

// ---------------------------------------------------------------------------
// Binary discovery
// ---------------------------------------------------------------------------

/// Locate a working `droid` binary.
///
/// Order: `configured` → `KRANZ_DROID_BIN` env var → `droid` on PATH →
/// well-known install locations. Each candidate is validated by running it
/// with `--version`; the first one that succeeds wins. Errors list every
/// attempt so the user can see what was tried.
///
/// `KRANZ_DROID_BIN`, when set and non-empty, is an *exclusive* override:
/// only that path is probed, and a failure is returned immediately rather
/// than falling through to PATH or the well-known fallback locations. Naming
/// the binary explicitly and having it not work is an error, not a reason to
/// search elsewhere.
pub fn discover_droid_binary(configured: Option<&str>) -> Result<PathBuf> {
    if let Some(env_bin) = std::env::var_os("KRANZ_DROID_BIN") {
        if !env_bin.is_empty() {
            let candidate = PathBuf::from(env_bin);
            return match probe_version(&candidate) {
                Ok(_version) => Ok(candidate),
                Err(why) => Err(EngineError::Config(format!(
                    "KRANZ_DROID_BIN points at {} which did not work: {why}",
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
    candidates.push(PathBuf::from("droid"));
    #[cfg(windows)]
    {
        candidates.push(PathBuf::from("droid.cmd"));
        candidates.push(PathBuf::from("droid.exe"));
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
        "no working droid binary found; tried: {}. Install Factory droid \
         or point kranz at it via the validatorScrutiny.droidBinary config \
         field or the KRANZ_DROID_BIN environment variable.",
        attempts.join(", ")
    )))
}

/// Well-known install locations checked after PATH.
#[cfg(not(windows))]
fn fallback_candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut out = Vec::new();
    if let Some(home) = &home {
        out.push(home.join(".factory").join("bin").join("droid"));
        out.push(home.join(".local").join("bin").join("droid"));
    }
    out.push(PathBuf::from("/opt/homebrew/bin/droid"));
    out.push(PathBuf::from("/usr/local/bin/droid"));
    if let Some(home) = &home {
        out.push(home.join(".npm-global").join("bin").join("droid"));
    }
    out
}

/// Well-known install locations checked after PATH (Windows).
#[cfg(windows)]
fn fallback_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        for dir in [
            profile.join(".factory").join("bin"),
            profile.join(".local").join("bin"),
            profile.join("AppData").join("Roaming").join("npm"),
            profile.join(".npm-global").join("bin"),
        ] {
            for name in ["droid.cmd", "droid.exe", "droid"] {
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

/// The prompt text droid actually receives: `append_system_prompt` (if any)
/// concatenated ahead of the prompt text — droid has no
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

/// Build the argv (excluding the binary itself) for one session.
///
/// Public so tests can assert the exact CLI wire format without spawning.
/// Deliberately ignores every claude-only `SessionSpec` field: `json_schema`,
/// `max_budget_usd`, `resume`, `permission_mode`, `allowed_tools` /
/// `disallowed_tools`, `tools`, `settings_json`, `effort`.
pub fn build_args(spec: &SessionSpec) -> Vec<String> {
    let auto = if spec.writable { "high" } else { "low" };
    vec![
        "exec".into(),
        "-o".into(),
        "json".into(),
        "--auto".into(),
        auto.into(),
        "-m".into(),
        spec.model.clone(),
        effective_prompt(spec),
    ]
}

// ---------------------------------------------------------------------------
// droid exec -o json result parsing
// ---------------------------------------------------------------------------

/// Parse one stdout line (a single JSON result object) into
/// [`AgentEvent`]s: an [`AgentEvent::Init`] followed by a terminal
/// [`AgentEvent::Result`]. `model` is the configured model, used both as the
/// `Init` model (droid's result object carries no model field) and as the
/// pricing key when the result has no CLI-reported dollar cost.
///
/// Unparseable lines become [`AgentEvent::Other`] with
/// `raw = {"unparsed": <line>}` so nothing is ever dropped from transcripts.
pub fn parse_droid_result(line: &str, model: &str) -> Vec<AgentEvent> {
    match serde_json::from_str::<Value>(line) {
        Ok(value) => parse_droid_value(value, model),
        Err(_) => vec![AgentEvent::Other {
            raw: json!({ "unparsed": line }),
        }],
    }
}

/// Map one parsed `droid exec -o json` value to events (see module docs /
/// fixture).
fn parse_droid_value(value: Value, model: &str) -> Vec<AgentEvent> {
    let line_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    if line_type != "result" {
        return vec![AgentEvent::Other { raw: value }];
    }

    let session_id = str_field(&value, "session_id");
    let init = AgentEvent::Init {
        session_id,
        model: model.to_string(),
        raw: value.clone(),
    };

    let usage_field = |key: &str| {
        value
            .pointer(&format!("/usage/{key}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let usage = TokenUsage {
        input: usage_field("input_tokens"),
        output: usage_field("output_tokens"),
        cache_read: usage_field("cache_read_input_tokens"),
        cache_write: usage_field("cache_creation_input_tokens"),
    };
    let cost_usd = value
        .get("cost_usd")
        .and_then(Value::as_f64)
        .or_else(|| Some(cost::usage_cost_usd(&usage, model)));
    let result_text = value
        .get("result")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let num_turns = value
        .get("num_turns")
        .and_then(Value::as_u64)
        .map(|n| n as u32);

    let terminal = AgentEvent::Result {
        text: result_text,
        is_error: value
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        usage,
        cost_usd,
        num_turns,
        raw: value,
    };

    vec![init, terminal]
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
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

/// The [`AgentBackend`] for `droid exec -o json`: single-shot with autonomy
/// selected from the session role.
#[derive(Debug, Clone)]
pub struct DroidBackend {
    binary: PathBuf,
}

impl DroidBackend {
    /// Use an explicit binary path (no validation performed).
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        DroidBackend {
            binary: binary.into(),
        }
    }

    /// Discover the binary via [`discover_droid_binary`].
    pub fn discover(configured: Option<&str>) -> Result<Self> {
        Ok(DroidBackend {
            binary: discover_droid_binary(configured)?,
        })
    }

    /// The binary this backend spawns.
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

#[async_trait::async_trait]
impl AgentBackend for DroidBackend {
    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>> {
        if spec.resume.is_some() {
            return Err(EngineError::Backend(
                "droid backend is single-shot only; resume is unsupported".to_string(),
            ));
        }
        let model = spec.model.clone();
        let args = build_args(&spec);

        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(&args)
            .current_dir(&spec.cwd)
            // agent-env-clear: CLEARED env from the minimal allowlist; the
            // one ambient var a droid session may authenticate with is
            // injected explicitly, never the whole ambient set.
            .env_clear()
            .envs(crate::agent_env::agent_session_env(
                &spec.env,
                &spec.session_id,
                Some("FACTORY_API_KEY"),
            ))
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
                    tracing::warn!(error = %e, "failed to create Job Object for droid child; \
                        tree-kill on abort will be unavailable");
                    None
                }
            },
            None => None,
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::Backend("droid child has no stdout pipe".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| EngineError::Backend("droid child has no stderr pipe".to_string()))?;

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

        Ok(Box::new(DroidSession {
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
            exit: None,
        }))
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A live `droid exec -o json` session (the [`AgentSession`] impl).
///
/// Single-shot only: [`send_user_message`](AgentSession::send_user_message)
/// always errors, and there is no streaming stdin to hold open. Only one
/// JSON line is ever expected, but the read loop is kept anyway to mirror
/// `CodexSession` and to tolerate stray blank lines.
pub struct DroidSession {
    session_id: String,
    model: String,
    child: Child,
    #[cfg(windows)]
    job: Option<win_job::JobHandle>,
    lines: BoundedLines<ChildStdout>,
    stderr_buf: Arc<Mutex<String>>,
    stderr_task: Option<JoinHandle<()>>,
    /// The single result line queues an Init + terminal Result; popped one
    /// per `next_event`.
    queue: VecDeque<AgentEvent>,
    saw_result: bool,
    saw_success_result: bool,
    exit: Option<SessionExit>,
}

impl DroidSession {
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
                "droid exited with {status}{}; stderr tail: {}",
                if self.saw_result {
                    ""
                } else {
                    " without emitting a terminal event"
                },
                self.stderr_tail(),
            )),
            Err(e) => SessionExit::Failed(format!(
                "failed to reap droid process: {e}; stderr tail: {}",
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
impl AgentSession for DroidSession {
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
                        "error reading droid stdout: {e}; stderr tail: {}",
                        self.stderr_tail(),
                    )));
                    return Ok(None);
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let events = parse_droid_result(&line, &self.model);
            for event in &events {
                self.observe(event);
            }
            self.queue.extend(events);
        }
    }

    async fn send_user_message(&mut self, _text: &str) -> Result<()> {
        Err(EngineError::Backend(
            "droid backend is single-shot only; send_user_message is unsupported".to_string(),
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

    const TEST_MODEL: &str = "accounts/fireworks/models/glm-5p2";

    #[test]
    #[cfg(unix)]
    fn probe_version_kills_a_hung_binary_within_the_deadline() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("hung-droid");
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

    fn fixture(name: &str) -> String {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name);
        std::fs::read_to_string(path).expect("read fixture")
    }

    #[test]
    fn backend_droid_parse_fixture() {
        let line = fixture("droid_exec_scrutiny.json");
        let events = parse_droid_result(line.trim(), TEST_MODEL);

        assert!(
            events.iter().any(
                |e| matches!(e, AgentEvent::Init { session_id, .. } if !session_id.is_empty())
            ),
            "expected an Init event with a non-empty session id"
        );

        let terminal = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Result {
                    text,
                    usage,
                    cost_usd,
                    num_turns,
                    ..
                } => Some((text, usage, cost_usd, num_turns)),
                _ => None,
            })
            .expect("expected a terminal Result event");
        let (text, usage, cost_usd, num_turns) = terminal;
        assert!(!text.is_empty(), "expected non-empty terminal text");
        assert!(
            usage.input > 0 || usage.output > 0 || usage.cache_read > 0,
            "expected non-zero usage on the terminal Result"
        );
        assert!(cost_usd.is_some(), "expected cost_usd to be Some");
        assert!(num_turns.is_some(), "expected num_turns to be Some");
    }

    #[test]
    fn backend_droid_terminal_text_parses_report() {
        let line = fixture("droid_exec_scrutiny.json");
        let events = parse_droid_result(line.trim(), TEST_MODEL);

        let terminal_text = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Result { text, .. } => Some(text.clone()),
                _ => None,
            })
            .expect("expected a terminal Result event");

        let report = crate::runner::parse_validator_report(&terminal_text)
            .expect("terminal text should parse as a ValidatorReport");
        assert!(
            !report.findings.is_empty(),
            "expected the fixture's ValidatorReport to have findings"
        );
    }

    #[test]
    fn build_args_droid_read_only() {
        let spec = SessionSpec {
            cwd: PathBuf::from("."),
            prompt: PromptMode::SingleShot("do the thing".to_string()),
            append_system_prompt: None,
            model: TEST_MODEL.to_string(),
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
            hook_status: None,
        };
        let args = build_args(&spec);
        assert_eq!(
            args,
            vec![
                "exec".to_string(),
                "-o".to_string(),
                "json".to_string(),
                "--auto".to_string(),
                "low".to_string(),
                "-m".to_string(),
                TEST_MODEL.to_string(),
                "do the thing".to_string(),
            ]
        );
    }

    #[test]
    fn build_args_droid_writable_uses_high_auto() {
        let spec = SessionSpec {
            cwd: PathBuf::from("."),
            prompt: PromptMode::SingleShot("do the thing".to_string()),
            append_system_prompt: Some("be terse".to_string()),
            model: TEST_MODEL.to_string(),
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
            hook_status: None,
        };
        let args = build_args(&spec);
        assert_eq!(
            args,
            vec![
                "exec".to_string(),
                "-o".to_string(),
                "json".to_string(),
                "--auto".to_string(),
                "high".to_string(),
                "-m".to_string(),
                TEST_MODEL.to_string(),
                "be terse\n\ndo the thing".to_string(),
            ]
        );
    }

    #[test]
    fn build_args_droid_folds_append_system_prompt() {
        let spec = SessionSpec {
            cwd: PathBuf::from("."),
            prompt: PromptMode::SingleShot("do the thing".to_string()),
            append_system_prompt: Some("be terse".to_string()),
            model: TEST_MODEL.to_string(),
            effort: "high".to_string(),
            session_id: "sess-1".to_string(),
            resume: None,
            permission_mode: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
            tools: vec![],
            writable: false,
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: Default::default(),
            sandbox: None,
            hook_status: None,
        };
        let args = build_args(&spec);
        assert_eq!(args.last().unwrap(), "be terse\n\ndo the thing");
    }

    #[test]
    fn droid_backend_rejects_resumed_spec() {
        use crate::backend::AgentBackend;
        let backend = DroidBackend::new("droid");
        let spec = SessionSpec {
            cwd: PathBuf::from("."),
            prompt: PromptMode::SingleShot("do the thing".to_string()),
            append_system_prompt: None,
            model: TEST_MODEL.to_string(),
            effort: "high".to_string(),
            session_id: "sess-1".to_string(),
            resume: Some("sess-0".to_string()),
            permission_mode: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
            tools: vec![],
            writable: false,
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: Default::default(),
            sandbox: None,
            hook_status: None,
        };
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(backend.start(spec));
        assert!(result.is_err(), "expected resume to be rejected");
    }
}
