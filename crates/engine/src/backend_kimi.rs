//! Kimi Code agent backend: drives the headless `kimi -p ... --output-format
//! stream-json` CLI.
//!
//! Ground truth is `crates/engine/tests/fixtures/kimi_exec_scrutiny.jsonl`
//! and `docs/scoping/kimi-cli-backend.md` (`f-1-1` probe). Kimi's `-p`
//! stdout wire is much thinner than Claude/Codex/Cursor's: it never emits an
//! `init`/`system` line and never emits a dedicated terminal result frame.
//! `backend_kimi` must *synthesize* both `Init` and the terminal `Result`:
//!
//! - `Init` is synthesized once the `role: "meta", type:
//!   "session.resume_hint"` line (the sole end-of-run signal on this wire)
//!   arrives, since that is the earliest point a session id is known.
//! - `Result` is synthesized from that same resume-hint line, with its text
//!   stitched from the last `role: "assistant"` line seen this run (mirrors
//!   `backend_codex::CodexStreamParser` stitching `agent_message` text into
//!   `turn.completed`).
//!
//! Token usage is not on this stdout wire at all (see docs/scoping/
//! kimi-cli-backend.md §5); the terminal `Result`'s usage is therefore
//! always the zero default here, and cost is computed client-side via
//! [`cost::usage_cost_usd`]. Tool-call/tool-result wire shapes are
//! unobserved (no tool-using capture exists yet); any unrecognized `role`
//! value is routed to [`AgentEvent::Other`] rather than guessed at.
//!
//! This module is single-shot only: unlike `backend_claude`, there is no
//! `--resume`/streaming-input mode, so [`KimiSession::send_user_message`]
//! and a `resume`d [`SessionSpec`] are both rejected at the seam rather than
//! translated into kimi flags.
//!
//! Several `SessionSpec` fields are claude-isms with no kimi equivalent and
//! are deliberately ignored when building argv: `json_schema`,
//! `max_budget_usd`, `resume`, `permission_mode`, `allowed_tools` /
//! `disallowed_tools`, `tools`, `settings_json`. `effort` is *not* ignored:
//! kimi has no `--effort`/`-m model:effort` flag (confirmed live in the
//! probe), so effort is instead forwarded as the `KIMI_MODEL_THINKING_EFFORT`
//! environment variable on the child process.

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

/// Env var carrying the thinking-effort level to the `kimi` child process
/// (highest-precedence effort selector per docs/scoping/kimi-cli-backend.md
/// §4 — there is no CLI flag for it).
const KIMI_EFFORT_ENV_VAR: &str = "KIMI_MODEL_THINKING_EFFORT";

/// The ambient var a kimi session may authenticate with (injected
/// explicitly, never via ambient inheritance).
const KIMI_AUTH_ENV: &str = "KIMI_API_KEY";

/// The minimal `.kimi-code` state seeded into a session's scratch HOME
/// (4th-pass review): auth does NOT survive a relocated `$HOME`
/// (docs/scoping/kimi-cli-backend.md) — the OAuth credential cache, device
/// id, oauth state, and provider/model config all live under `~/.kimi-code`,
/// and `KIMI_API_KEY` only authenticates an ALREADY-configured custom
/// provider (which lives in `config.toml`). An unseeded scratch HOME leaves
/// every kimi session with no provider at all. `sessions/` transcripts are
/// deliberately excluded (unbounded, and per-session state).
const KIMI_SEED_ENTRIES: &[&str] = &["credentials", "device_id", "oauth", "config.toml"];

/// The cleared environment one `kimi` session spawns with (ticket
/// `agent-env-clear`), mirroring [`crate::backend_claude`]'s seeding
/// contract: a spec carrying a relocated scratch `HOME` (worker relocation)
/// is used verbatim; otherwise a fresh per-session scratch HOME is seeded
/// with [`KIMI_SEED_ENTRIES`] so provider/model/OAuth state survives.
/// Seeding failure degrades to an empty scratch home — the session then
/// fails auth loudly rather than silently inheriting the operator's real
/// HOME. `KIMI_API_KEY` is injected explicitly when set (logged name-only).
fn kimi_child_env(spec: &SessionSpec) -> std::collections::HashMap<String, String> {
    if spec.env.contains_key("HOME") {
        return crate::agent_env::agent_session_env(
            &spec.env,
            &spec.session_id,
            Some(KIMI_AUTH_ENV),
        );
    }
    let real_home = std::env::var_os("HOME").map(PathBuf::from);
    let scratch_root = crate::backend_claude::scratch_home_root(&spec.session_id);
    match seed_kimi_scratch_home(&scratch_root, real_home.as_deref()) {
        Ok(home) => {
            tracing::info!(
                session_id = %spec.session_id,
                decision = "scratch-seeded",
                "session spec carried no relocated HOME; spawning into a seeded scratch \
                 HOME (.kimi-code minimal auth/config set)"
            );
            crate::agent_env::session_env_with_home(
                &spec.env,
                &spec.session_id,
                Some(KIMI_AUTH_ENV),
                &home,
            )
        }
        Err(e) => {
            tracing::warn!(
                session_id = %spec.session_id,
                error = %e,
                "kimi scratch HOME seeding failed; session spawns into an empty scratch \
                 HOME and will fail auth loudly if KIMI_API_KEY is not injected"
            );
            crate::agent_env::agent_session_env(&spec.env, &spec.session_id, Some(KIMI_AUTH_ENV))
        }
    }
}

/// Seed `<scratch_root>/home/.kimi-code` with [`KIMI_SEED_ENTRIES`], copied
/// opaquely (bytes only, no parsing/logging of contents) from the real
/// home's `.kimi-code` when present; a missing source yields an
/// empty-but-present `.kimi-code`. Returns the home dir the child should
/// get as `HOME`.
fn seed_kimi_scratch_home(
    scratch_root: &Path,
    real_home: Option<&Path>,
) -> std::io::Result<PathBuf> {
    let home = scratch_root.join("home");
    let kimi_dir = home.join(".kimi-code");
    std::fs::create_dir_all(&kimi_dir)?;
    if let Some(real_home) = real_home {
        let source = real_home.join(".kimi-code");
        for entry in KIMI_SEED_ENTRIES {
            let src = source.join(entry);
            let dst = kimi_dir.join(entry);
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

/// Serializes every test (in this module and in `orchestrator.rs`) that
/// mutates the process-global env vars consulted by [`discover_kimi_binary`]
/// (`KRANZ_KIMI_BIN`, `PATH`, `HOME`), since `cargo test` runs tests in
/// parallel threads within one process and a second, independent mutex would
/// not mutually exclude against this one (mirrors `DROID_ENV_LOCK`, but must
/// be `pub(crate)` — unlike droid, kimi's env-mutating tests are split across
/// two source files and both must lock the SAME mutex).
#[cfg(test)]
pub(crate) static KIMI_ENV_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Binary discovery
// ---------------------------------------------------------------------------

/// Locate a working `kimi` binary.
///
/// Order: `KRANZ_KIMI_BIN` env var → `configured` → `kimi` on PATH →
/// well-known install locations, ending with the Kimi-specific
/// `~/.kimi-code/bin/kimi` (per docs/scoping/kimi-cli-backend.md §1, this is
/// where the real install lives on a machine that never put it on PATH).
/// Each candidate is validated by running it with `--version`; the first one
/// that succeeds wins. Errors list every attempt so the user can see what
/// was tried.
///
/// `KRANZ_KIMI_BIN`, when set and non-empty, is an *exclusive* override: only
/// that path is probed, and a failure is returned immediately rather than
/// falling through to PATH or the well-known fallback locations. Naming the
/// binary explicitly and having it not work is an error, not a reason to
/// search elsewhere.
pub fn discover_kimi_binary(configured: Option<&str>) -> Result<PathBuf> {
    if let Some(env_bin) = std::env::var_os("KRANZ_KIMI_BIN") {
        if !env_bin.is_empty() {
            let candidate = PathBuf::from(env_bin);
            return match probe_version(&candidate) {
                Ok(_version) => Ok(candidate),
                Err(why) => Err(EngineError::Config(format!(
                    "KRANZ_KIMI_BIN points at {} which did not work: {why}",
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
    candidates.push(PathBuf::from("kimi"));
    #[cfg(windows)]
    {
        candidates.push(PathBuf::from("kimi.cmd"));
        candidates.push(PathBuf::from("kimi.exe"));
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
        "no working kimi binary found; tried: {}. Install the Kimi Code CLI \
         or point kranz at it via the validatorScrutiny.kimiBinary config \
         field or the KRANZ_KIMI_BIN environment variable.",
        attempts.join(", ")
    )))
}

/// Well-known install locations checked after PATH, ending with the
/// Kimi-specific install dir (per docs/scoping/kimi-cli-backend.md §1).
#[cfg(not(windows))]
fn fallback_candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut out = Vec::new();
    if let Some(home) = &home {
        out.push(home.join(".npm-global").join("bin").join("kimi"));
    }
    out.push(PathBuf::from("/opt/homebrew/bin/kimi"));
    out.push(PathBuf::from("/usr/local/bin/kimi"));
    if let Some(home) = &home {
        out.push(home.join(".local").join("bin").join("kimi"));
        out.push(home.join(".kimi-code").join("bin").join("kimi"));
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
            for name in ["kimi.cmd", "kimi.exe", "kimi"] {
                out.push(dir.join(name));
            }
        }
        for name in ["kimi.cmd", "kimi.exe", "kimi"] {
            out.push(profile.join(".kimi-code").join("bin").join(name));
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

/// The prompt text kimi actually receives: `append_system_prompt` (if any)
/// concatenated ahead of the prompt text — kimi has no
/// `--append-system-prompt` flag, so the engine folds it into the single
/// `-p` argument instead.
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
/// `disallowed_tools`, `tools`, `settings_json`. `effort` is handled
/// separately via the `KIMI_MODEL_THINKING_EFFORT` env var (see
/// [`effort_env_value`]) — `-m` has no `model:effort` suffix syntax
/// (confirmed live in docs/scoping/kimi-cli-backend.md §4).
pub fn build_args(spec: &SessionSpec) -> Vec<String> {
    let permission_flag = if spec.writable { "--yolo" } else { "--plan" };
    vec![
        "-p".into(),
        effective_prompt(spec),
        "-m".into(),
        spec.model.clone(),
        "--output-format".into(),
        "stream-json".into(),
        permission_flag.into(),
    ]
}

/// The value to set `KIMI_MODEL_THINKING_EFFORT` to for this session, if any
/// (docs/scoping/kimi-cli-backend.md §4: highest-precedence effort selector,
/// forwarded straight to the provider rather than validated client-side).
fn effort_env_value(spec: &SessionSpec) -> Option<&str> {
    if spec.effort.is_empty() {
        None
    } else {
        Some(spec.effort.as_str())
    }
}

// ---------------------------------------------------------------------------
// `kimi -p --output-format stream-json` line parsing
// ---------------------------------------------------------------------------

/// Parse one stdout line into zero or more [`AgentEvent`]s. Does not
/// synthesize `Init`/terminal-text-stitching — that requires cross-line
/// state and lives in [`KimiStreamParser`].
///
/// Unparseable lines become [`AgentEvent::Other`] with
/// `raw = {"unparsed": <line>}` so nothing is ever dropped from transcripts.
pub fn parse_kimi_line(line: &str, model: &str) -> Vec<AgentEvent> {
    match serde_json::from_str::<Value>(line) {
        Ok(value) => parse_kimi_value(value, model),
        Err(_) => vec![AgentEvent::Other {
            raw: json!({ "unparsed": line }),
        }],
    }
}

/// Map one parsed `kimi -p --output-format stream-json` value to events (see
/// module docs / docs/scoping/kimi-cli-backend.md §3). Unrecognized `role`
/// values (e.g. an as-yet-unobserved tool-call role) route to
/// [`AgentEvent::Other`] rather than being guessed at.
pub fn parse_kimi_value(value: Value, model: &str) -> Vec<AgentEvent> {
    let role = value.get("role").and_then(Value::as_str).unwrap_or("");
    match role {
        "assistant" => {
            let text = value.get("content").and_then(Value::as_str).unwrap_or("");
            if text.is_empty() {
                vec![AgentEvent::Other { raw: value }]
            } else {
                vec![AgentEvent::Text {
                    text: text.to_string(),
                    raw: value,
                }]
            }
        }
        "meta" if value.get("type").and_then(Value::as_str) == Some("session.resume_hint") => {
            vec![parse_terminal(value, model)]
        }
        _ => vec![AgentEvent::Other { raw: value }],
    }
}

/// Synthesize the terminal `Result` from a `session.resume_hint` line. No
/// usage/cost field exists on this wire (docs/scoping/kimi-cli-backend.md
/// §5), so usage is always the zero default and cost is always computed
/// client-side via [`cost::usage_cost_usd`].
fn parse_terminal(value: Value, model: &str) -> AgentEvent {
    let usage = TokenUsage::default();
    let cost_usd = Some(cost::usage_cost_usd(&usage, model));
    AgentEvent::Result {
        text: String::new(),
        is_error: false,
        usage,
        cost_usd,
        num_turns: Some(1),
        raw: value,
    }
}

/// Last `max` characters of `text` (for stderr tails in error messages).
fn last_chars(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(max);
    chars[start..].iter().collect()
}

// ---------------------------------------------------------------------------
// Stateful stream parsing (synthesizes `Init` and stitches terminal text —
// see module docs / docs/scoping/kimi-cli-backend.md §3)
// ---------------------------------------------------------------------------

/// Stateful wrapper around [`parse_kimi_line`] that remembers the most
/// recent `role: "assistant"` [`AgentEvent::Text`] and, on the terminal
/// `session.resume_hint` line, synthesizes an [`AgentEvent::Init`] (the
/// session id first becomes known on this line) followed by the
/// [`AgentEvent::Result`] with text stitched from the last assistant
/// message.
#[derive(Debug, Default)]
pub struct KimiStreamParser {
    last_text: Option<String>,
    init_emitted: bool,
}

impl KimiStreamParser {
    pub fn new() -> Self {
        KimiStreamParser::default()
    }

    /// Parse one stdout line, synthesizing `Init` (once) and stitching
    /// remembered assistant text into the terminal `Result`.
    pub fn push(&mut self, line: &str, model: &str) -> Vec<AgentEvent> {
        parse_kimi_line(line, model)
            .into_iter()
            .flat_map(|event| self.observe(event, model))
            .collect()
    }

    fn observe(&mut self, event: AgentEvent, model: &str) -> Vec<AgentEvent> {
        match event {
            AgentEvent::Text { text, raw } => {
                self.last_text = Some(text.clone());
                vec![AgentEvent::Text { text, raw }]
            }
            AgentEvent::Result {
                is_error,
                usage,
                cost_usd,
                num_turns,
                raw,
                ..
            } => {
                let session_id = raw
                    .get("session_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let mut out = Vec::new();
                if !self.init_emitted {
                    self.init_emitted = true;
                    out.push(AgentEvent::Init {
                        session_id,
                        model: model.to_string(),
                        raw: raw.clone(),
                    });
                }
                out.push(AgentEvent::Result {
                    text: self.last_text.take().unwrap_or_default(),
                    is_error,
                    usage,
                    cost_usd,
                    num_turns,
                    raw,
                });
                out
            }
            other => vec![other],
        }
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// The [`AgentBackend`] for `kimi -p --output-format stream-json`:
/// single-shot with the `--yolo`/`--plan` permission mode selected from the
/// session role.
#[derive(Debug, Clone)]
pub struct KimiBackend {
    binary: PathBuf,
}

impl KimiBackend {
    /// Use an explicit binary path (no validation performed).
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        KimiBackend {
            binary: binary.into(),
        }
    }

    /// Discover the binary via [`discover_kimi_binary`].
    pub fn discover(configured: Option<&str>) -> Result<Self> {
        Ok(KimiBackend {
            binary: discover_kimi_binary(configured)?,
        })
    }

    /// The binary this backend spawns.
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

#[async_trait::async_trait]
impl AgentBackend for KimiBackend {
    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>> {
        if spec.resume.is_some() {
            return Err(EngineError::Backend(
                "kimi backend is single-shot only; resume is unsupported".to_string(),
            ));
        }
        let model = spec.model.clone();
        let args = build_args(&spec);

        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(&args)
            .current_dir(&spec.cwd)
            // agent-env-clear: CLEARED env from the minimal allowlist; the
            // scratch HOME is SEEDED with the minimal .kimi-code auth/config
            // set (auth does not survive a relocated HOME), and the one
            // ambient var a kimi session may authenticate with is injected
            // explicitly, never the whole ambient set.
            .env_clear()
            .envs(kimi_child_env(&spec))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(authority) = effort_env_value(&spec) {
            command.env(KIMI_EFFORT_ENV_VAR, authority);
        }
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
                    tracing::warn!(error = %e, "failed to create Job Object for kimi child; \
                        tree-kill on abort will be unavailable");
                    None
                }
            },
            None => None,
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::Backend("kimi child has no stdout pipe".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| EngineError::Backend("kimi child has no stderr pipe".to_string()))?;

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

        Ok(Box::new(KimiSession {
            session_id: spec.session_id.clone(),
            model,
            child,
            #[cfg(windows)]
            job,
            lines: BoundedLines::new(stdout),
            stderr_buf,
            stderr_task: Some(stderr_task),
            queue: VecDeque::new(),
            stream_parser: KimiStreamParser::new(),
            saw_result: false,
            saw_success_result: false,
            exit: None,
        }))
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A live `kimi -p --output-format stream-json` session (the
/// [`AgentSession`] impl).
///
/// Single-shot only: [`send_user_message`](AgentSession::send_user_message)
/// always errors, and there is no streaming stdin to hold open.
pub struct KimiSession {
    session_id: String,
    model: String,
    child: Child,
    #[cfg(windows)]
    job: Option<win_job::JobHandle>,
    lines: BoundedLines<ChildStdout>,
    stderr_buf: Arc<Mutex<String>>,
    stderr_task: Option<JoinHandle<()>>,
    /// Multi-block lines queue several events (an `Init`+`Result` pair on
    /// the resume-hint line); popped one per `next_event`.
    queue: VecDeque<AgentEvent>,
    stream_parser: KimiStreamParser,
    saw_result: bool,
    saw_success_result: bool,
    exit: Option<SessionExit>,
}

impl KimiSession {
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
                "kimi exited with {status}{}; stderr tail: {}",
                if self.saw_result {
                    ""
                } else {
                    " without emitting a terminal event"
                },
                self.stderr_tail(),
            )),
            Err(e) => SessionExit::Failed(format!(
                "failed to reap kimi process: {e}; stderr tail: {}",
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
impl AgentSession for KimiSession {
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
                        "error reading kimi stdout: {e}; stderr tail: {}",
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
            "kimi backend is single-shot only; send_user_message is unsupported".to_string(),
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

    const TEST_MODEL: &str = "kimi-code/k3";

    /// 4th-pass review: the scratch seed carries the minimal `.kimi-code`
    /// auth/config set (auth does not survive a relocated HOME) and never
    /// the unbounded per-session transcripts.
    #[test]
    fn seed_kimi_scratch_home_copies_the_minimal_auth_config_set() {
        let real_home = tempfile::tempdir().unwrap();
        let kimi = real_home.path().join(".kimi-code");
        std::fs::create_dir_all(kimi.join("credentials")).unwrap();
        std::fs::write(kimi.join("credentials").join("kimi-code.json"), "{}").unwrap();
        std::fs::write(kimi.join("device_id"), "dev-1").unwrap();
        std::fs::create_dir_all(kimi.join("oauth")).unwrap();
        std::fs::write(kimi.join("oauth").join("state"), "state").unwrap();
        std::fs::write(kimi.join("config.toml"), "model = \"kimi-code/k3\"\n").unwrap();
        std::fs::create_dir_all(kimi.join("sessions")).unwrap();
        std::fs::write(kimi.join("sessions").join("big.jsonl"), "transcript").unwrap();

        let scratch = tempfile::tempdir().unwrap();
        let home = seed_kimi_scratch_home(scratch.path(), Some(real_home.path())).unwrap();

        let seeded = home.join(".kimi-code");
        assert!(seeded.join("credentials").join("kimi-code.json").is_file());
        assert!(seeded.join("device_id").is_file());
        assert!(seeded.join("oauth").join("state").is_file());
        assert!(seeded.join("config.toml").is_file());
        assert!(
            !seeded.join("sessions").exists(),
            "per-session transcripts are never seeded"
        );
    }

    /// A missing real `.kimi-code` yields an empty-but-present seed (the
    /// session then fails auth loudly rather than inheriting).
    #[test]
    fn seed_kimi_scratch_home_without_a_source_yields_an_empty_seed() {
        let real_home = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let home = seed_kimi_scratch_home(scratch.path(), Some(real_home.path())).unwrap();

        let seeded = home.join(".kimi-code");
        assert!(seeded.is_dir());
        assert_eq!(std::fs::read_dir(&seeded).unwrap().count(), 0);
    }

    fn fixture_lines() -> Vec<String> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("kimi_exec_scrutiny.jsonl");
        std::fs::read_to_string(path)
            .expect("read fixture")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.to_string())
            .collect()
    }

    #[test]
    #[cfg(unix)]
    fn kimi_probe_version_kills_a_hung_binary_within_the_deadline() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("hung-kimi");
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
    fn kimi_stream_parser_synthesizes_init_and_terminal_result_from_fixture() {
        let mut parser = KimiStreamParser::new();
        let mut events: Vec<AgentEvent> = Vec::new();
        for line in fixture_lines() {
            events.extend(parser.push(&line, TEST_MODEL));
        }

        let init = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Init { session_id, .. } => Some(session_id.clone()),
                _ => None,
            })
            .expect("expected a synthesized Init event");
        assert!(!init.is_empty(), "expected a non-empty Init session id");

        let terminal = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Result {
                    text,
                    usage,
                    num_turns,
                    ..
                } => Some((text.clone(), usage.clone(), *num_turns)),
                _ => None,
            })
            .expect("expected a terminal Result event");
        let (text, usage, num_turns) = terminal;
        assert!(
            !text.is_empty(),
            "expected the terminal Result text to be stitched from the last assistant line"
        );
        assert_eq!(
            usage,
            TokenUsage::default(),
            "the fixture's terminal line carries no usage field, so usage must stay the zero \
             default (populated iff the wire carries it)"
        );
        assert_eq!(num_turns, Some(1));
    }

    #[test]
    fn kimi_discovery_honors_env_override_exclusively() {
        // ENV_TEST_LOCK first: tempfile resolves its parent from ambient
        // TMP/TEMP, and env-poisoning tests elsewhere in this binary (e.g.
        // the cfg(windows) TEMP=C:\operator-tmp fixture) hold the same lock —
        // without it a parallel windows test's poisoned TEMP makes tempdir()
        // fail with NotFound (windows-latest CI, run 30935850957).
        let _env_lock = crate::agent_env::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = super::KIMI_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let working = dir.path().join("working-kimi");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&working, "#!/bin/sh\necho kimi-code 0.27.0\n").unwrap();
            std::fs::set_permissions(&working, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let bogus = dir.path().join("does-not-exist-kimi");

        std::env::set_var("KRANZ_KIMI_BIN", &bogus);
        let result = discover_kimi_binary(Some(working.to_str().unwrap()));
        std::env::remove_var("KRANZ_KIMI_BIN");

        let error = result.expect_err("a broken KRANZ_KIMI_BIN must fail immediately");
        assert!(
            error.to_string().contains("KRANZ_KIMI_BIN"),
            "expected the error to name the exclusive override, got: {error}"
        );
        assert!(
            !error.to_string().contains("working-kimi"),
            "the exclusive override must not fall through to `configured`, got: {error}"
        );
    }

    #[test]
    #[cfg(unix)]
    fn kimi_discovery_falls_through_configured_to_path_then_well_known() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = super::KIMI_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved_env_override = std::env::var_os("KRANZ_KIMI_BIN");
        std::env::remove_var("KRANZ_KIMI_BIN");
        let saved_path = std::env::var_os("PATH");
        let saved_home = std::env::var_os("HOME");

        let write_stub = |path: &Path| {
            std::fs::write(path, "#!/bin/sh\necho kimi-code 0.27.0\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };

        let dir = tempfile::tempdir().unwrap();
        let bogus_configured = dir.path().join("does-not-exist-kimi");

        // Prepend (never replace) PATH so unrelated tests spawning real
        // system binaries (`true`, `sh`, ...) concurrently on other threads
        // keep resolving them.
        let prepend_path = |extra: &Path| {
            let mut dirs = vec![extra.to_path_buf()];
            if let Some(existing) = std::env::var_os("PATH") {
                dirs.extend(std::env::split_paths(&existing));
            }
            std::env::set_var("PATH", std::env::join_paths(dirs).unwrap());
        };

        // Stage 1: `configured` is broken, but a working `kimi` sits on
        // PATH. Discovery must fall through configured -> PATH and pick it
        // up (proving the `configured -> PATH` half of the ordering, not
        // just the exclusive-env-override branch already covered above).
        let path_dir = dir.path().join("path-bin");
        std::fs::create_dir_all(&path_dir).unwrap();
        let path_stub = path_dir.join("kimi");
        write_stub(&path_stub);
        prepend_path(&path_dir);

        let path_result = discover_kimi_binary(Some(bogus_configured.to_str().unwrap()));

        // Stage 2: PATH is pinned to a minimal, known-safe set of standard
        // dirs (still enough for unrelated concurrent tests to spawn `true`
        // / `sh`, but guaranteed to carry no `kimi`, unlike the developer's
        // real PATH which may well have one installed). HOME points at a
        // dir with a working `~/.kimi-code/bin/kimi`. Discovery must fall
        // through PATH -> well-known and pick it up.
        std::env::set_var("PATH", "/usr/bin:/bin");
        let home_dir = dir.path().join("home");
        let well_known_dir = home_dir.join(".kimi-code").join("bin");
        std::fs::create_dir_all(&well_known_dir).unwrap();
        let well_known_stub = well_known_dir.join("kimi");
        write_stub(&well_known_stub);
        std::env::set_var("HOME", &home_dir);

        let well_known_result = discover_kimi_binary(Some(bogus_configured.to_str().unwrap()));

        match saved_path {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        match saved_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        match saved_env_override {
            Some(v) => std::env::set_var("KRANZ_KIMI_BIN", v),
            None => std::env::remove_var("KRANZ_KIMI_BIN"),
        }

        assert_eq!(
            path_result.expect("PATH fallback candidate should be found"),
            PathBuf::from("kimi"),
            "discovery should fall through configured -> PATH (bare name, resolved via PATH)"
        );
        assert_eq!(
            well_known_result.expect("well-known fallback candidate should be found"),
            well_known_stub,
            "discovery should fall through PATH -> well-known ~/.kimi-code/bin/kimi"
        );
    }

    #[test]
    fn kimi_build_args_ignores_claude_only_fields_and_uses_plan_for_read_only() {
        let spec = SessionSpec {
            cwd: PathBuf::from("."),
            prompt: PromptMode::SingleShot("do the thing".to_string()),
            append_system_prompt: Some("be terse".to_string()),
            model: "kimi-code/k3".to_string(),
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
                "-p".to_string(),
                "be terse\n\ndo the thing".to_string(),
                "-m".to_string(),
                "kimi-code/k3".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
                "--plan".to_string(),
            ]
        );
        assert_eq!(effort_env_value(&spec), Some("high"));
    }

    #[test]
    fn kimi_build_args_uses_yolo_for_writable_sessions() {
        let spec = SessionSpec {
            cwd: PathBuf::from("."),
            prompt: PromptMode::SingleShot("do the thing".to_string()),
            append_system_prompt: None,
            model: "kimi-code/k3".to_string(),
            effort: String::new(),
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
                "-p".to_string(),
                "do the thing".to_string(),
                "-m".to_string(),
                "kimi-code/k3".to_string(),
                "--output-format".to_string(),
                "stream-json".to_string(),
                "--yolo".to_string(),
            ]
        );
        assert_eq!(effort_env_value(&spec), None);
    }

    #[test]
    fn kimi_backend_rejects_resumed_spec() {
        use crate::backend::AgentBackend;
        let backend = KimiBackend::new("kimi");
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
        };
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(backend.start(spec));
        assert!(result.is_err(), "expected resume to be rejected");
    }

    #[tokio::test]
    async fn kimi_session_rejects_send_user_message() {
        let mut child = tokio::process::Command::new("true")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn `true`");
        let stdout = child.stdout.take().expect("stdout pipe");
        let mut session = KimiSession {
            session_id: "sess-1".to_string(),
            model: TEST_MODEL.to_string(),
            lines: BoundedLines::new(stdout),
            #[cfg(windows)]
            job: None,
            stderr_buf: Arc::new(Mutex::new(String::new())),
            stderr_task: None,
            queue: VecDeque::new(),
            stream_parser: KimiStreamParser::new(),
            saw_result: false,
            saw_success_result: false,
            exit: None,
            child,
        };
        let result = session.send_user_message("nope").await;
        assert!(result.is_err(), "expected send_user_message to be rejected");
    }
}
