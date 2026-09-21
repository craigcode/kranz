//! ACP (Agent Client Protocol) worker backend: JSON-RPC 2.0 over NDJSON
//! stdin/stdout. Protocol version 1 is required; optional capabilities are
//! negotiated separately from that major version. Released adapter pins and
//! live-proof limits are recorded in `docs/acp-compatibility.md`.
//!
//! `initialize` and `session/new` synthesize `Init`. Its session ID is the
//! peer's ID; `AgentSession::session_id()` retains the engine ID. The raw
//! event records both IDs, capabilities and the configured model label.
//! Model attribution comes from the peer's model config option or legacy
//! `models.currentModelId`, else `unreported`. Kranz does not apply the
//! configured model/effort label as an ACP model-selection request.
//!
//! Existing role instructions and task text travel in the prompt, as with
//! the Codex/Droid backends. There is no native JSON-schema enforcement.
//! Message chunks become `Text`; tool announcements/results become
//! `ToolUse`/`ToolResult`; remaining valid notifications become `Other`.
//! A `session/prompt` response creates `Result` from the accumulated final
//! message. Only `end_turn` is a successful result: cancellation, truncation,
//! refusal and missing/unknown stop reasons fail honestly.
//!
//! Session updates and permissions must name the established peer session.
//! Malformed JSON-RPC, duplicate JSON keys, invalid UTF-8, oversized frames
//! and oversized retained message/tool state fail closed. A malformed frame
//! that can be retained is a diagnostic `Other`, never a later valid report.
//!
//! Context-window usage is not a billable input/output token split. Tokens
//! remain unavailable (the existing zero default). A finite nonnegative USD
//! session cost becomes a turn delta only when adjacent totals are known;
//! missing telemetry or a decreasing total yields no attributed turn cost.
//!
//! Permission decisions use current action identity, kind and raw arguments.
//! Deny patterns run before the read-only posture. Unclassified operations
//! and mode changes are refused. A display title cannot stand in for shell
//! arguments. Only an offered, unambiguous one-time option may be selected;
//! durable or malformed options cancel. The engine records each request and
//! resolution before its separately callable responder can send an answer.
//! Understood calls require one-time operator consent; prohibitions are denied
//! by policy. Output continues while a request waits, with a five-minute limit.
//! Delivery receipts are separate from the eventual tool outcome.
//! This is a cooperative permission policy, not shell parsing or containment:
//! an adapter can perform actions without asking. `allowed_tools` is not an
//! enforced allowlist here, and read-only roles may still execute commands.
//!
//! Client fs/terminal services and same-feature resume remain unsupported.
//! Unexpected client requests receive -32601. Released adapters may support
//! load/resume methods; that does not imply Kranz negotiates or uses them.
//! Configuration restricts ACP to opt-in worker use. Enforced missions require
//! an explicit qualified `acpProfile`; arbitrary commands, validator roles and
//! automatic backend promotion remain refused.
//! The direct backend API has a Docker containment proof path on macOS/Linux:
//! a pinned, trusted Linux image must supply /usr/local/bin/python3. It reuses
//! the container mount policy and a private host lease checked by guest PID 1.
//! Profile admission pins that image, startup policy and credential channel.
//!
//! Child environments are cleared through `agent_session_env`. ACP has no
//! implicit credential selection: direct callers supply SessionSpec.env, while
//! profiles seed only the operator-selected credential file in a private home.
//! Ambient HOME and provider credentials are never restored by this backend.
//!
//! Native single-shot completion closes stdin, allows a short exit grace and then
//! cleans up the owned process group. macOS/Linux observe exit with WNOWAIT
//! so group identity stays owned until cleanup, before reaping. Windows
//! requires Job Object assignment. Writes, cancellation, reap and diagnostic
//! drain have deadlines. Same-group cleanup is not containment against a
//! descendant that escapes its group; that requires the separate S6 proof.

use crate::backend::{
    AgentBackend, AgentEvent, AgentSession, PromptMode, SessionExit, SessionSpec,
};
#[cfg(windows)]
use crate::backend_claude::win_job;
use crate::error::{EngineError, Result};
use crate::stream_bounds::{drain_to_tail, BoundedLines, STDERR_TAIL_CAP, STDOUT_LINE_CAP};
use crate::types::TokenUsage;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::task::JoinHandle;

/// Max characters kept in tool-use / tool-result summaries.
const SUMMARY_MAX_CHARS: usize = 200;
/// Max characters of captured stderr included in failure messages.
const STDERR_TAIL_CHARS: usize = 500;

/// The only ACP protocol version this backend speaks (stable schema v1; see
/// module docs). A peer negotiating anything else is refused at `initialize`.
const ACP_PROTOCOL_VERSION: u64 = 1;

/// Deadline for one handshake request (`initialize`, `session/new`). Both
/// may involve adapter startup and authentication. The limit keeps
/// `start()` from parking the run loop forever. The prompt turn itself is
/// deliberately unbounded here: turn/stall budgets are the engine's call
/// (runner-level), not the transport's.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const CLEANUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);
const COMPLETION_GRACE: std::time::Duration = std::time::Duration::from_millis(200);
const CONTAINER_COMPLETION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const CANCEL_WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(100);

/// JSON-RPC method names, stable schema v1 (`meta.json`).
mod method {
    pub(crate) const INITIALIZE: &str = "initialize";
    pub(crate) const SESSION_NEW: &str = "session/new";
    pub(crate) const SESSION_PROMPT: &str = "session/prompt";
    pub(crate) const SESSION_CANCEL: &str = "session/cancel";
    pub(crate) const SESSION_UPDATE: &str = "session/update";
    pub(crate) const REQUEST_PERMISSION: &str = "session/request_permission";
}

/// Claude-style tool name → ACP `kind`s it matches, for interpreting
/// `SessionSpec::disallowed_tools` patterns against ACP tool calls. ACP
/// kinds are the only tool identity the wire carries (`title` is
/// free-form display text), so the mapping is necessarily coarse; it is
/// used ONLY for deny decisions, never to auto-approve.
const TOOL_NAME_KINDS: &[(&str, &[&str])] = &[
    ("Bash", &["execute"]),
    ("Read", &["read"]),
    ("Write", &["edit"]),
    ("Edit", &["edit"]),
    ("NotebookEdit", &["edit"]),
    ("Glob", &["search"]),
    ("Grep", &["search"]),
    ("WebSearch", &["search", "fetch"]),
    ("WebFetch", &["fetch"]),
];

/// ACP `kind`s that mutate the filesystem; refused outright in read-only
/// (`writable: false`) sessions. `execute` is deliberately not in this set —
/// see the module-docs permission section.
const MUTATING_KINDS: &[&str] = &["edit", "delete", "move"];

// ---------------------------------------------------------------------------
// JSON-RPC framing
// ---------------------------------------------------------------------------

/// One stdout line classified by JSON-RPC shape. The wire is symmetric
/// (both sides issue requests), so a line is one of: a response to a client
/// request, a peer request we must answer, or a notification.
#[derive(Debug)]
enum Frame {
    /// In-process broker messages; never constructible from peer JSON.
    PermissionAnswer(crate::live_permission::Answer),
    PermissionExpired,
    /// `result`/`error` for a client-issued request id.
    Response {
        id: u64,
        outcome: RpcOutcome,
    },
    /// Peer→client request (`session/request_permission`, or an unsupported
    /// client method). The `id` is echoed back verbatim — it may be a string
    /// or a number per JSON-RPC, so it is kept as a raw [`Value`].
    Request {
        id: Value,
        method: String,
        params: Value,
        raw: Value,
    },
    /// Peer→client notification (`session/update`, or anything else).
    Notification {
        method: String,
        params: Value,
        raw: Value,
    },
    /// Malformed protocol input: retained diagnostic, then session failure.
    Unrecognized(Value),
}

/// The payload of a JSON-RPC response: peer ids are always numbers in our
/// exchanges with the peer's client side, but the error path keeps the raw
/// object for the transcript.
#[derive(Debug)]
enum RpcOutcome {
    Result(Value),
    Error(Value),
}

/// Classify one stdout line. Unparseable lines become
/// [`Frame::Unrecognized`] with `raw = {"unparsed": <line>}` so nothing is
/// ever dropped from transcripts (mirrors `backend_codex::parse_codex_line`).
fn classify_line(line: &str) -> Frame {
    let value = match crate::strict_json::parse(line.as_bytes()) {
        Ok(value) => value,
        Err(_) => return Frame::Unrecognized(json!({ "unparsed": line })),
    };
    classify_value(value)
}

fn classify_value(value: Value) -> Frame {
    let obj = match value.as_object() {
        Some(obj) => obj,
        None => return Frame::Unrecognized(value),
    };
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Frame::Unrecognized(value);
    }
    let has_id = obj.contains_key("id");
    if obj.contains_key("method") && (obj.contains_key("result") || obj.contains_key("error")) {
        return Frame::Unrecognized(value);
    }
    if has_id && !(obj["id"].is_string() || obj["id"].is_i64() || obj["id"].is_u64()) {
        return Frame::Unrecognized(value);
    }
    let method = obj.get("method").and_then(Value::as_str);
    match (method, has_id) {
        // Request: method + id.
        (Some(method), true) => Frame::Request {
            id: obj.get("id").cloned().unwrap_or(Value::Null),
            method: method.to_string(),
            params: obj.get("params").cloned().unwrap_or(Value::Null),
            raw: value,
        },
        // Notification: method, no id.
        (Some(method), false) => Frame::Notification {
            method: method.to_string(),
            params: obj.get("params").cloned().unwrap_or(Value::Null),
            raw: value,
        },
        // Response: no method, carries result or error. Our request ids are
        // numbers; anything else is not a response to us.
        (None, true) => {
            let id = obj.get("id").and_then(Value::as_u64);
            match (id, obj.get("result"), obj.get("error")) {
                (Some(id), Some(result), None) => Frame::Response {
                    id,
                    outcome: RpcOutcome::Result(result.clone()),
                },
                (Some(id), None, Some(error)) => Frame::Response {
                    id,
                    outcome: RpcOutcome::Error(error.clone()),
                },
                _ => Frame::Unrecognized(value),
            }
        }
        (None, false) => Frame::Unrecognized(value),
    }
}

// ---------------------------------------------------------------------------
// Permission policy (pure; the seam the ticket pins)
// ---------------------------------------------------------------------------

/// What the seam decided about one `session/request_permission`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PermissionDecision {
    Allow,
    /// Human-readable reason; lands on the synthesized denial event's summary.
    Deny(String),
}

/// Everything known about the tool call a permission request covers: the
/// `tool_call`/`tool_call_update` fields tracked by [`AcpSession`], merged
/// with whatever the request itself carries (the request's embedded
/// `ToolCallUpdate` may be the first place a title/kind appears).
#[derive(Debug, Clone, Default)]
struct ToolCallInfo {
    kind: String,
    title: String,
    /// Command (`execute`) or path (file kinds) extracted from
    /// `rawInput`/`locations`; the subject glob patterns match against.
    subject: String,
}

/// Extract the matchable subject from a tool-call-shaped value
/// (`rawInput.command`/`cmd` for execute, `locations[0].path` or
/// `rawInput.path` for file kinds). Explicit raw input invalidates display
/// fallback; execute never trusts a title as its command.
fn tool_call_subject(kind: &str, title: &str, call: &Value) -> String {
    let raw_input = call.get("rawInput").cloned().unwrap_or(Value::Null);
    let str_at = |value: &Value, keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| value.get(*k).and_then(Value::as_str).map(str::to_string))
    };
    if kind == "execute" {
        // Display titles and file locations are not executable arguments.
        return str_at(&raw_input, &["command", "cmd"]).unwrap_or_default();
    }
    if let Some(path) = call
        .get("locations")
        .and_then(Value::as_array)
        .and_then(|locs| locs.first())
        .and_then(|loc| loc.get("path"))
        .and_then(Value::as_str)
    {
        return path.to_string();
    }
    if let Some(path) = str_at(&raw_input, &["path", "filePath", "file_path"]) {
        return path;
    }
    if call.get("rawInput").is_some() || call.get("locations").is_some() {
        return String::new();
    }
    title.to_string()
}

/// `*`-wildcard match (a bare `*` spans any text including empty; every
/// other character matches literally, case-sensitively — shell commands and
/// paths are case-sensitive on the platforms this guards).
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let parts = pattern.split('*');
    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');
    let mut rest = text;
    let mut first = true;
    for part in parts {
        if part.is_empty() {
            first = false;
            continue;
        }
        match rest.find(part) {
            Some(idx) if !first || !anchored_start || idx == 0 => {
                rest = &rest[idx + part.len()..];
            }
            _ => return false,
        }
        first = false;
    }
    // After the last literal, a pattern not ending in `*` must end exactly
    // there (nothing left over).
    !anchored_end || rest.is_empty()
}

/// Split one claude-shaped permission pattern (`Bash(git push*)`, `Write`,
/// …) into its tool name and subject glob. A bare name carries the glob `*`.
fn split_pattern(pattern: &str) -> (&str, &str) {
    match pattern.split_once('(') {
        Some((name, rest)) => (name.trim(), rest.strip_suffix(')').unwrap_or(rest)),
        None => (pattern.trim(), "*"),
    }
}

/// Whether one permission pattern's tool name maps to this ACP `kind` at
/// all, regardless of subject. Separate from [`pattern_matches`] because a
/// pattern that COVERS a call but cannot be evaluated against it is a
/// refusal, not a pass.
fn pattern_covers_kind(pattern: &str, kind: &str) -> bool {
    let (name, _) = split_pattern(pattern);
    TOOL_NAME_KINDS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .is_some_and(|(_, kinds)| kinds.contains(&kind))
}

/// Whether one claude-shaped permission pattern (`Bash(git push*)`,
/// `Write`, …) covers an ACP tool call of `kind` with `subject`.
fn pattern_matches(pattern: &str, kind: &str, subject: &str) -> bool {
    let (_, glob) = split_pattern(pattern);
    if pattern_covers_kind(pattern, kind) {
        wildcard_match(glob, subject)
    } else {
        false
    }
}

/// The seam: decide one permission request from the [`SessionSpec`]. Deny
/// rules are evaluated before the read-only posture so the recorded reason
/// names the most specific rule that fired.
///
/// Fails CLOSED on missing wire fields. ACP v1 leaves `title` optional and
/// `rawInput` free-form, so a peer can send a tool call with no subject at
/// all; every glob then matches nothing and the deny list silently vacates.
/// The same holds for the read-only posture when the peer omits `kind`
/// (it arrives as `"other"`, which `MUTATING_KINDS` cannot classify). In
/// both cases the guard cannot be evaluated, so the call is refused and the
/// reason names the field the peer left out.
fn decide_permission(spec: &SessionSpec, call: &ToolCallInfo) -> PermissionDecision {
    if call.kind == "switch_mode" {
        return PermissionDecision::Deny(
            "agent mode changes require an explicit human decision".into(),
        );
    }
    if ![
        "read",
        "edit",
        "delete",
        "move",
        "search",
        "execute",
        "think",
        "fetch",
        "switch_mode",
    ]
    .contains(&call.kind.as_str())
    {
        return PermissionDecision::Deny("tool call carries an unknown or missing ACP kind".into());
    }
    if call.subject.trim().is_empty() {
        if let Some(pattern) = spec
            .disallowed_tools
            .iter()
            .find(|pattern| pattern_covers_kind(pattern, &call.kind))
        {
            return PermissionDecision::Deny(format!(
                "tool call carries no subject (no rawInput command or path, no locations[0].path, \
                 no title), so deny pattern {pattern:?} for ACP kind {:?} cannot be evaluated",
                call.kind
            ));
        }
    }
    for pattern in &spec.disallowed_tools {
        if pattern_matches(pattern, &call.kind, &call.subject) {
            return PermissionDecision::Deny(format!(
                "matches SessionSpec.disallowed_tools pattern {pattern:?}"
            ));
        }
    }
    if !spec.writable {
        let kind = call.kind.trim();
        if kind.is_empty() || kind == "other" {
            return PermissionDecision::Deny(format!(
                "read-only session (writable: false): tool call carries no usable ACP kind \
                 ({kind:?}), so whether it mutates the filesystem cannot be decided"
            ));
        }
        if MUTATING_KINDS.contains(&kind) {
            return PermissionDecision::Deny(format!(
                "read-only session (writable: false): ACP kind {:?} mutates the filesystem",
                call.kind
            ));
        }
    }
    PermissionDecision::Allow
}

/// Select only a well-formed, unambiguous one-time option. Adapter option
/// IDs are opaque: the kind supplies semantics, never an ID spelling.
fn permission_response(decision: &PermissionDecision, options: &[Value]) -> Value {
    let mut ids = std::collections::HashSet::new();
    let valid = !options.is_empty()
        && options.len() <= 32
        && options.iter().all(|option| {
            option
                .get("optionId")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty() && id.len() <= 256)
                .is_some_and(|id| ids.insert(id))
        });
    let kind = match decision {
        PermissionDecision::Allow => "allow_once",
        PermissionDecision::Deny(_) => "reject_once",
    };
    let selected = valid
        .then(|| {
            let mut matching = options
                .iter()
                .filter(|option| option.get("kind").and_then(Value::as_str) == Some(kind));
            let first = matching.next()?;
            matching.next().is_none().then_some(first)
        })
        .flatten();
    match selected {
        Some(option) => {
            json!({ "outcome": { "outcome": "selected", "optionId": option["optionId"] } })
        }
        None => json!({ "outcome": { "outcome": "cancelled" } }),
    }
}

// ---------------------------------------------------------------------------
// Event mapping
// ---------------------------------------------------------------------------

/// Keep at most `max` characters (not bytes — never splits a code point).
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}

/// Last `max` characters of `text` (for stderr tails in failure messages).
fn last_chars(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(max);
    chars[start..].iter().collect()
}

/// Summarize a tool call's content for a [`AgentEvent::ToolResult`]: the
/// first text content block, else the updated title, else the status.
fn tool_result_summary(update: &Value, tracked: &ToolCallInfo, status: &str) -> String {
    if let Some(content) = update.get("content").and_then(Value::as_array) {
        for item in content {
            if item.get("type").and_then(Value::as_str) == Some("content") {
                if let Some(text) = item
                    .get("content")
                    .and_then(|c| c.get("text"))
                    .and_then(Value::as_str)
                {
                    return truncate_chars(text, SUMMARY_MAX_CHARS);
                }
            }
        }
    }
    if !tracked.title.is_empty() {
        return truncate_chars(&tracked.title, SUMMARY_MAX_CHARS);
    }
    status.to_string()
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// The [`AgentBackend`] for an ACP-speaking agent executable (KRZ-301).
///
/// There is no canonical ACP binary name and no `--version` convention, so
/// there is deliberately no discovery/probe: the configured command is
/// spawned directly and the `initialize` handshake IS the probe — a
/// non-ACP executable fails there, loudly, before any prompt turn.
#[derive(Debug, Clone)]
pub struct AcpBackend {
    program: PathBuf,
    args: Vec<String>,
    profile: Option<crate::acp_worker::AcpWorkerProfile>,
}

impl AcpBackend {
    /// Spawn `program args` as the ACP agent (no validation performed; the
    /// handshake at session start is the validation).
    pub fn new(program: impl Into<PathBuf>, args: Vec<String>) -> Self {
        AcpBackend {
            program: program.into(),
            args,
            profile: None,
        }
    }

    /// Ordinary-worker construction. Profile argv and startup policy are fixed;
    /// the credential is read only at session start, after boundary checks.
    pub fn for_worker(cfg: &crate::types::RoleConfig) -> Result<Self> {
        if let Some(profile) = &cfg.acp_profile {
            profile.validate_config(
                crate::types::Role::Worker,
                cfg,
                crate::types::WorkerIsolation::Worktree,
            )?;
            let definition = profile.definition()?;
            Ok(Self {
                program: definition.program.into(),
                args: definition.args.iter().map(|s| (*s).to_owned()).collect(),
                profile: Some(profile.clone()),
            })
        } else {
            let command = cfg.acp_command.as_ref().ok_or_else(|| {
                EngineError::Config("ACP worker requires acpCommand or acpProfile".into())
            })?;
            Ok(Self::new(command, cfg.acp_args.clone()))
        }
    }

    /// The program this backend spawns.
    pub fn program(&self) -> &std::path::Path {
        &self.program
    }
}

fn effective_prompt(spec: &SessionSpec) -> String {
    let text = match &spec.prompt {
        PromptMode::SingleShot(text) | PromptMode::Streaming(text) => text,
    };
    match &spec.append_system_prompt {
        Some(system) if !system.is_empty() => format!("{system}\n\n{text}"),
        _ => text.clone(),
    }
}

fn peer_reported_model(session: &Value) -> Option<&str> {
    session
        .get("configOptions")
        .and_then(Value::as_array)
        .and_then(|options| {
            options.iter().find_map(|option| {
                (option.get("category").and_then(Value::as_str) == Some("model"))
                    .then(|| option.get("currentValue").and_then(Value::as_str))
                    .flatten()
            })
        })
        .or_else(|| session.get("models")?.get("currentModelId")?.as_str())
        .filter(|model| !model.trim().is_empty() && model.len() <= 256)
}

#[async_trait::async_trait]
impl AgentBackend for AcpBackend {
    async fn start(&self, mut spec: SessionSpec) -> Result<Box<dyn AgentSession>> {
        // Configuration admission is not the only caller of this public
        // backend. Never silently discard an embedding caller's boundary.
        let container_requested = spec.sandbox.as_ref().is_some_and(|sandbox| {
            cfg!(any(target_os = "macos", target_os = "linux"))
                && sandbox.backend == crate::sandbox::SandboxBackend::Container
                && sandbox.container.is_some()
        });
        if spec.sandbox.is_some() && !container_requested {
            return Err(EngineError::Backend(
                "acp backend: enforced containment is not certified; refusing the supplied sandbox before spawn".into(),
            ));
        }
        if spec.resume.is_some() {
            return Err(EngineError::Backend(
                "acp backend: resume is unsupported (session/load is an optional v1 \
                 capability this backend does not negotiate)"
                    .to_string(),
            ));
        }
        let profile_home = self
            .profile
            .as_ref()
            .map(|profile| profile.prepare(&mut spec))
            .transpose()?;
        let model = spec.model.clone();

        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let (container, mut command) = if container_requested {
            let (container, command) =
                crate::acp_container::OwnedContainer::prepare(&spec, &self.program, &self.args)
                    .await?;
            (Some(container), command)
        } else {
            (None, self.native_command(&spec))
        };
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let mut command = self.native_command(&spec);
        command
            .current_dir(&spec.cwd)
            // ACP is bidirectional: stdin carries the client's requests.
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Unix: make the child the leader of a fresh process group so aborts
        // can kill the whole tree, mirroring `backend_claude::ClaudeBackend`.
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn().map_err(|e| {
            EngineError::Backend(format!(
                "failed to spawn acp agent {}: {e}",
                self.program.display()
            ))
        })?;

        // Windows: kill-on-close Job Object, mirroring `backend_claude`.
        #[cfg(windows)]
        let job = match child.raw_handle() {
            Some(handle) => match win_job::JobHandle::create_and_assign(handle) {
                Ok(job) => Some(job),
                Err(e) => {
                    let _ = child.kill().await;
                    return Err(EngineError::Backend(format!(
                        "acp Job Object assignment failed: {e}"
                    )));
                }
            },
            None => {
                let _ = child.kill().await;
                return Err(EngineError::Backend(
                    "acp child has no process handle".into(),
                ));
            }
        };

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| EngineError::Backend("acp child has no stdin pipe".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::Backend("acp child has no stdout pipe".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| EngineError::Backend("acp child has no stderr pipe".to_string()))?;

        // Capture stderr concurrently so a chatty child never blocks on a
        // full pipe and failure messages can include the tail. The stream is
        // drained to EOF but only a bounded tail is retained — a noisy or
        // malicious peer must not exhaust host memory (stream_bounds).
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_task = {
            let buf = Arc::clone(&stderr_buf);
            tokio::spawn(async move {
                let tail = drain_to_tail(stderr, STDERR_TAIL_CAP).await;
                *buf.lock().expect("stderr buffer lock") = tail;
            })
        };

        let (permission_responder, permission_answers) =
            crate::live_permission::PermissionResponder::channel();
        let mut session = AcpSession {
            session_id: spec.session_id.clone(),
            acp_session_id: None,
            model,
            spec,
            child,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            container,
            profile_home,
            #[cfg(windows)]
            job,
            stdin: Some(stdin),
            child_status: None,
            cleanup_failure: None,
            drain_deadline: None,
            lines: BoundedLines::new_strict(stdout),
            stderr_buf,
            stderr_task: Some(stderr_task),
            queue: VecDeque::new(),
            next_request_id: 1,
            prompt_request_id: None,
            tool_calls: HashMap::new(),
            permission_responder,
            permission_answers,
            deferred_permission_answer: None,
            pending_permissions: HashMap::new(),
            seen_permission_ids: std::collections::HashSet::new(),
            message_text: String::new(),
            message_id: None,
            last_usage: None,
            previous_cost_total: None,
            handshake_bytes: 0,
            handshake_complete: false,
            saw_result: false,
            exit: None,
        };

        // The handshake is eager: a non-ACP executable (or a hung peer)
        // fails start() loudly rather than mid-run. A handshake failure
        // kills the child before the error crosses back.
        if let Err(e) = session.handshake().await {
            session.kill_child().await;
            let message = match e {
                EngineError::Backend(message) => message,
                other => other.to_string(),
            };
            return Err(EngineError::Backend(format!(
                "{message}; startup stderr after cleanup: {}{}",
                session.stderr_tail(),
                session
                    .cleanup_failure
                    .map(|cause| format!("; cleanup unconfirmed: {cause}"))
                    .unwrap_or_default(),
            )));
        }
        Ok(Box::new(session))
    }
}

impl AcpBackend {
    fn native_command(&self, spec: &SessionSpec) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(&self.args)
            // agent-env-clear: ACP has no implicit credential channel.
            // Only explicit session variables cross the native boundary.
            .env_clear()
            .envs(crate::agent_env::agent_session_env(
                &spec.env,
                &spec.session_id,
                None,
            ));
        command
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A live ACP session (the [`AgentSession`] impl).
///
/// Reading is inline in `next_event`. Every write is bounded: a peer that
/// stops reading stdin cannot block cancellation indefinitely.
struct PendingPermission {
    proposal: crate::live_permission::Proposal,
    expires_at: tokio::time::Instant,
}

pub struct AcpSession {
    session_id: String,
    /// The peer-issued session id (`session/new` response).
    acp_session_id: Option<String>,
    model: String,
    /// Kept for permission decisions (`disallowed_tools`, `writable`).
    spec: SessionSpec,
    child: Child,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    container: Option<crate::acp_container::OwnedContainer>,
    profile_home: Option<crate::acp_worker::PreparedProfile>,
    #[cfg(windows)]
    job: Option<win_job::JobHandle>,
    stdin: Option<ChildStdin>,
    child_status: Option<ExitStatus>,
    cleanup_failure: Option<&'static str>,
    drain_deadline: Option<tokio::time::Instant>,
    lines: BoundedLines<ChildStdout>,
    stderr_buf: Arc<Mutex<String>>,
    stderr_task: Option<JoinHandle<()>>,
    /// Converted events not yet surfaced; popped one per `next_event`.
    queue: VecDeque<AgentEvent>,
    next_request_id: u64,
    /// The in-flight `session/prompt` request id; its response synthesizes
    /// the terminal `Result`. `None` outside a prompt turn.
    prompt_request_id: Option<u64>,
    /// Tracked tool calls by `toolCallId` (kind/title/subject), so a
    /// `tool_call_update` or permission request resolves to what is known
    /// about the call.
    tool_calls: HashMap<String, ToolCallInfo>,
    permission_responder: crate::live_permission::PermissionResponder,
    permission_answers: tokio::sync::mpsc::Receiver<crate::live_permission::Answer>,
    deferred_permission_answer: Option<crate::live_permission::Answer>,
    pending_permissions: HashMap<String, PendingPermission>,
    seen_permission_ids: std::collections::HashSet<String>,
    /// Accumulated text of the CURRENT assistant message (chunks with the
    /// same `messageId` concatenate; a changed/missing-`messageId` boundary
    /// starts a new message, and the last message wins the terminal Result,
    /// mirroring `backend_codex`'s last-agent_message rule).
    message_text: String,
    message_id: Option<String>,
    /// Latest `usage_update` (context state + optional cumulative USD cost);
    /// its cost lands on the terminal `Result` (see module docs).
    last_usage: Option<Value>,
    previous_cost_total: Option<f64>,
    handshake_bytes: usize,
    handshake_complete: bool,
    saw_result: bool,
    exit: Option<SessionExit>,
}

#[cfg(unix)]
impl Drop for AcpSession {
    fn drop(&mut self) {
        crate::backend_claude::kill_unreaped_group(&self.child);
    }
}

async fn wait_for_peer_exit(child: &mut Child) -> std::io::Result<()> {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let pid = child
            .id()
            .ok_or_else(|| std::io::Error::other("acp child already reaped"))?;
        crate::command_exec::control_leader_exited(pid).await
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        child.wait().await.map(|_| ())
    }
}

fn kill_owned_peer(child: &mut Child, #[cfg(windows)] job: Option<&win_job::JobHandle>) {
    #[cfg(unix)]
    crate::backend_claude::kill_unreaped_group(child);
    #[cfg(windows)]
    if let Some(job) = job {
        job.kill();
    }
    // Also target the still-owned leader if it changed its process group.
    if child.id().is_some() {
        let _ = child.start_kill();
    }
}

impl AcpSession {
    // -- wire helpers -------------------------------------------------------

    /// Serialize one JSON-RPC message as a single NDJSON line on the peer's
    /// stdin. Keys are inserted in sorted order by serde_json's default map,
    /// which also puts `"id"` first — mock peers and debugging tools rely on
    /// nothing more than JSON semantics, but stable key order keeps captured
    /// transcripts diffable.
    async fn write_message(&mut self, message: Value) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let mut line = serde_json::to_string(&message)
            .map_err(|e| EngineError::Backend(format!("failed to encode acp message: {e}")))?;
        line.push('\n');
        if line.len() > STDOUT_LINE_CAP {
            return Err(EngineError::Backend(
                "acp request exceeds the frame byte limit".into(),
            ));
        }
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| EngineError::Backend("acp stdin is closed".into()))?;
        tokio::time::timeout(WRITE_TIMEOUT, async {
            stdin.write_all(line.as_bytes()).await?;
            stdin.flush().await
        })
        .await
        .map_err(|_| EngineError::Backend("acp stdin write timed out".into()))?
        .map_err(|e| EngineError::Backend(format!("failed to write to acp agent stdin: {e}")))?;
        Ok(())
    }

    /// Send a client request and return its id (the caller either awaits the
    /// response via [`AcpSession::pump_until_response`] or, for
    /// `session/prompt`, leaves it to the `next_event` loop).
    async fn send_request(&mut self, method: &str, params: Value) -> Result<u64> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        self.write_message(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;
        Ok(id)
    }

    /// `initialize` + `session/new`, then the first `session/prompt` (its
    /// response streams in through `next_event` like any later turn).
    async fn handshake(&mut self) -> Result<()> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(container) = &mut self.container {
            container
                .write_launch(self.stdin.as_mut().expect("startup stdin"))
                .await?;
        }
        let init_id = self
            .send_request(
                method::INITIALIZE,
                json!({
                    "protocolVersion": ACP_PROTOCOL_VERSION,
                    "clientCapabilities": {
                        // fs/terminal unsupported: a conformant peer never
                        // calls fs/* or terminal/*; one that does is answered
                        // with -32601 rather than silently served.
                        "fs": { "readTextFile": false, "writeTextFile": false },
                        "terminal": false,
                    },
                    "clientInfo": {
                        "name": "kranz",
                        "title": "kranz mission engine",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            )
            .await?;
        let init_result = self.pump_until_response(init_id, HANDSHAKE_TIMEOUT).await?;
        let peer_version = init_result
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if peer_version != ACP_PROTOCOL_VERSION {
            return Err(EngineError::Backend(format!(
                "acp agent negotiated protocol version {peer_version}, but this backend speaks \
                 only stable version {ACP_PROTOCOL_VERSION} (schema v1)"
            )));
        }

        let new_id = self
            .send_request(
                method::SESSION_NEW,
                json!({
                    "cwd": self.spec.cwd.display().to_string(),
                    "mcpServers": [],
                }),
            )
            .await?;
        let new_result = self.pump_until_response(new_id, HANDSHAKE_TIMEOUT).await?;
        let acp_session_id = new_result
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty() && id.len() <= 256)
            .ok_or_else(|| {
                EngineError::Backend("acp session/new response carried no sessionId".to_string())
            })?
            .to_string();
        self.acp_session_id = Some(acp_session_id.clone());
        self.handshake_complete = true;
        let reported_model = peer_reported_model(&new_result).map(str::to_owned);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let containment = self.container.as_ref().map(|container| container.receipt());
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let containment: Option<Value> = None;
        // Configured attribution and peer-reported state are different facts.
        self.queue.push_back(AgentEvent::Init {
            session_id: acp_session_id,
            model: reported_model
                .clone()
                .unwrap_or_else(|| "unreported".into()),
            raw: json!({
                "initialize": init_result,
                "sessionNew": new_result,
                "engineSessionId": self.session_id,
                "configuredModel": self.model,
                "modelSource": if reported_model.is_some() { "peer" } else { "unreported" },
                "configuredModelSelectionApplied": false,
                "containment": containment,
                "workerProfile": self.profile_home.as_ref().map(|p| &p.receipt),
                "synthesizedBy": "backend_acp",
            }),
        });

        let prompt_text = effective_prompt(&self.spec);
        self.send_prompt(&prompt_text).await
    }

    /// Send one `session/prompt` request and mark its id as the turn whose
    /// response synthesizes the terminal `Result`.
    async fn send_prompt(&mut self, text: &str) -> Result<()> {
        let acp_session_id = self
            .acp_session_id
            .clone()
            .ok_or_else(|| EngineError::Backend("acp session not established yet".to_string()))?;
        let id = self
            .send_request(
                method::SESSION_PROMPT,
                json!({
                    "sessionId": acp_session_id,
                    "prompt": [ { "type": "text", "text": text } ],
                }),
            )
            .await?;
        self.prompt_request_id = Some(id);
        // A new turn begins: the terminal Result stitches only this turn's
        // last message.
        self.message_text.clear();
        self.message_id = None;
        self.last_usage = None;
        Ok(())
    }

    /// Read frames until the response to `id` arrives, converting everything
    /// else through the normal frame path (notifications become queued
    /// events; permission requests are answered). Used by the handshake —
    /// the only place a response is awaited synchronously.
    async fn pump_until_response(
        &mut self,
        id: u64,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        let pump = async {
            loop {
                let frame = match self.read_frame().await? {
                    Some(frame) => frame,
                    None => {
                        return Err(EngineError::Backend(format!(
                            "acp agent closed stdout before answering request id {id}; \
                             stderr tail: {}",
                            self.stderr_tail()
                        )))
                    }
                };
                match frame {
                    Frame::Response {
                        id: response_id,
                        outcome,
                    } if response_id == id => {
                        return match outcome {
                            RpcOutcome::Result(result) => Ok(result),
                            RpcOutcome::Error(error) => Err(EngineError::Backend(format!(
                                "acp request id {id} failed: {error}"
                            ))),
                        };
                    }
                    other => self.handle_frame(other).await?,
                }
            }
        };
        match tokio::time::timeout(timeout, pump).await {
            Ok(result) => result,
            Err(_) => Err(EngineError::Backend(format!(
                "acp agent did not answer request id {id} within {}s (handshake timeout)",
                timeout.as_secs()
            ))),
        }
    }

    /// Read and classify the next stdout line; `None` at EOF. Blank lines
    /// are skipped (NDJSON tolerates them; a peer's pretty-printing or
    /// keepalive must not fabricate events).
    async fn read_frame(&mut self) -> Result<Option<Frame>> {
        loop {
            let permission_wait = self
                .pending_permissions
                .values()
                .map(|p| {
                    p.expires_at
                        .saturating_duration_since(tokio::time::Instant::now())
                        .min(
                            (p.proposal.deadline - chrono::Utc::now())
                                .to_std()
                                .unwrap_or_default(),
                        )
                })
                .min()
                .unwrap_or(std::time::Duration::from_secs(86400));
            if permission_wait.is_zero() {
                return Ok(Some(Frame::PermissionExpired));
            }
            let can_answer = !self.lines.has_partial_line();
            let read = {
                let line = self.lines.next_line();
                tokio::pin!(line);
                let read = if let Some(deadline) = self.drain_deadline {
                    tokio::time::timeout_at(deadline, &mut line)
                        .await
                        .map_err(|_| {
                            EngineError::Backend(
                                "acp stdout remained open after peer cleanup".into(),
                            )
                        })?
                } else {
                    tokio::select! {
                        biased;
                        // Drain already-buffered action changes before applying
                        // a queued answer to the older invocation description.
                        read = &mut line => read,
                        answer = async {
                            if let Some(answer) = self.deferred_permission_answer.take() {
                                Some(answer)
                            } else {
                                self.permission_answers.recv().await
                            }
                        }, if can_answer => {
                            return Ok(answer.map(Frame::PermissionAnswer));
                        }
                        _ = tokio::time::sleep(permission_wait), if !self.pending_permissions.is_empty() => {
                            return Ok(Some(Frame::PermissionExpired));
                        }
                        exited = wait_for_peer_exit(&mut self.child) => {
                            exited.map_err(|e| EngineError::Backend(format!("acp process observation failed: {e}")))?;
                            kill_owned_peer(&mut self.child, #[cfg(windows)] self.job.as_ref());
                            self.child_status = Some(self.child.wait().await.map_err(|e| {
                                EngineError::Backend(format!("acp process reap failed: {e}"))
                            })?);
                            let deadline = tokio::time::Instant::now() + CLEANUP_TIMEOUT;
                            self.drain_deadline = Some(deadline);
                            tokio::time::timeout_at(deadline, &mut line).await
                                .map_err(|_| EngineError::Backend("acp stdout remained open after peer cleanup".into()))?
                        }
                    }
                };
                read
            };
            match read {
                Ok(Some(line)) if line.trim().is_empty() => continue,
                Ok(Some(line)) => {
                    if self.profile_home.is_some()
                        && crate::strict_json::parse(line.as_bytes()).is_err()
                    {
                        // Do not retain malformed credential-bearing peer input,
                        // including duplicate keys hiding an escaped token.
                        return Err(EngineError::Backend(
                            "acp profile peer emitted invalid unique-key JSON; frame refused"
                                .into(),
                        ));
                    }
                    if self
                        .profile_home
                        .as_ref()
                        .is_some_and(|p| p.contains_secret(&line))
                    {
                        return Err(EngineError::Backend(
                            "acp peer exposed a configured credential; frame refused".into(),
                        ));
                    }
                    if !self.handshake_complete {
                        self.handshake_bytes = self.handshake_bytes.saturating_add(line.len());
                        if self.handshake_bytes > STDOUT_LINE_CAP {
                            return Err(EngineError::Backend(
                                "acp handshake output exceeded its byte limit".into(),
                            ));
                        }
                    }
                    return Ok(Some(classify_line(&line)));
                }
                Ok(None) => return Ok(None),
                Err(e) => {
                    return Err(EngineError::Backend(format!(
                        "error reading acp agent stdout: {e}; stderr tail: {}",
                        self.stderr_tail()
                    )))
                }
            }
        }
    }

    /// Convert one frame into queued events and side effects (permission
    /// answers, tool tracking, usage capture, terminal-Result synthesis).
    async fn handle_frame(&mut self, frame: Frame) -> Result<()> {
        match frame {
            Frame::PermissionAnswer(answer) => self.answer_permission(answer).await?,
            Frame::PermissionExpired => {
                // Do not manufacture or replay a denial resolution. Terminating
                // the peer closes its requests without authorizing an effect.
                return Err(EngineError::Backend(
                    "live permission deadline expired".into(),
                ));
            }
            Frame::Notification {
                method,
                params,
                raw,
            } => {
                if method == method::SESSION_UPDATE {
                    self.handle_session_update(&params, raw)?;
                } else {
                    self.queue.push_back(AgentEvent::Other { raw });
                }
            }
            Frame::Request {
                id,
                method,
                params,
                raw,
            } => {
                if method == method::REQUEST_PERMISSION {
                    self.handle_permission_request(id, &params, raw).await?;
                } else {
                    // A client capability we did not advertise (fs/*,
                    // terminal/*, elicitation/*): refuse with JSON-RPC
                    // -32601 rather than silently serving or hanging.
                    self.write_message(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32601,
                            "message": format!("kranz acp backend does not support {method:?}"),
                        },
                    }))
                    .await?;
                    self.queue.push_back(AgentEvent::Other { raw });
                }
            }
            Frame::Response { id, outcome } => {
                if Some(id) == self.prompt_request_id {
                    self.prompt_request_id = None;
                    self.synthesize_result(outcome, id);
                } else {
                    // A response to nothing outstanding (a straggler from a
                    // cancelled turn, or a peer bug): transcript only.
                    self.queue.push_back(AgentEvent::Other {
                        raw: match outcome {
                            RpcOutcome::Result(result) => {
                                json!({ "unmatchedResponse": { "id": id, "result": result } })
                            }
                            RpcOutcome::Error(error) => {
                                json!({ "unmatchedResponse": { "id": id, "error": error } })
                            }
                        },
                    });
                }
            }
            Frame::Unrecognized(raw) => {
                self.queue.push_back(AgentEvent::Other { raw });
                return Err(EngineError::Backend(
                    "acp peer emitted malformed JSON-RPC".into(),
                ));
            }
        }
        Ok(())
    }

    fn track_tool_call(&mut self, id: String, info: ToolCallInfo) -> Result<()> {
        if id.trim().is_empty() || id.len() > 256 {
            return Err(EngineError::Backend(
                "acp tool update has no valid toolCallId".into(),
            ));
        }
        let bytes = info.kind.len() + info.title.len() + info.subject.len();
        let retained = self
            .tool_calls
            .iter()
            .filter(|(key, _)| *key != &id)
            .map(|(key, value)| {
                key.len() + value.kind.len() + value.title.len() + value.subject.len()
            })
            .sum::<usize>();
        if self.tool_calls.len() >= 1024
            || retained.saturating_add(bytes).saturating_add(id.len()) > STDOUT_LINE_CAP
        {
            return Err(EngineError::Backend(
                "acp tool-call tracking exceeded its limit".into(),
            ));
        }
        self.tool_calls.insert(id, info);
        Ok(())
    }

    /// Map one `session/update` notification onto events (see module docs).
    fn handle_session_update(&mut self, params: &Value, raw: Value) -> Result<()> {
        let Some(expected) = self.acp_session_id.as_deref() else {
            // No prompt has been sent: pre-session notices are diagnostic only.
            self.queue.push_back(AgentEvent::Other { raw });
            return Ok(());
        };
        if params.get("sessionId").and_then(Value::as_str) != Some(expected) {
            return Err(EngineError::Backend(
                "acp update has a foreign or missing sessionId".into(),
            ));
        }
        let update = params.get("update").cloned().unwrap_or(Value::Null);
        if let Some(call_id) = update.get("toolCallId").and_then(Value::as_str) {
            if let Some(pending) = self
                .pending_permissions
                .values()
                .map(|p| &p.proposal)
                .find(|p| p.tool_call_id == call_id)
            {
                // Compare complete effect-bearing fields, including content.
                // A same-title/same-path edit can still contain different bytes.
                let changed = ["kind", "rawInput", "locations", "content"]
                    .iter()
                    .any(|key| {
                        update
                            .get(*key)
                            .is_some_and(|value| pending.action.get(*key) != Some(value))
                    });
                let terminal = update
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s == "completed" || s == "failed" || s == "in_progress");
                if changed || terminal {
                    return Err(EngineError::Backend(
                        "ACP invocation changed or started while consent was pending".into(),
                    ));
                }
            }
        }
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("agent_message_chunk") => {
                let text = update
                    .get("content")
                    .and_then(|c| c.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if text.is_empty() {
                    self.queue.push_back(AgentEvent::Other { raw });
                    return Ok(());
                }
                // Message boundaries: a changed messageId starts a new
                // message; the LAST message wins the terminal Result.
                let chunk_id = update
                    .get("messageId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if chunk_id.is_some() && chunk_id != self.message_id {
                    self.message_text.clear();
                    self.message_id = chunk_id;
                }
                if self.message_text.len().saturating_add(text.len()) > STDOUT_LINE_CAP {
                    return Err(EngineError::Backend(
                        "acp assistant message exceeded its byte limit".into(),
                    ));
                }
                self.message_text.push_str(text);
                self.queue.push_back(AgentEvent::Text {
                    text: text.to_string(),
                    raw,
                });
            }
            Some("tool_call") => {
                let id = update
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let kind = update
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("other")
                    .to_string();
                let title = update
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let subject = tool_call_subject(&kind, &title, &update);
                self.track_tool_call(
                    id,
                    ToolCallInfo {
                        kind: kind.clone(),
                        title: title.clone(),
                        subject,
                    },
                )?;
                self.queue.push_back(AgentEvent::ToolUse {
                    tool: kind,
                    summary: truncate_chars(&title, SUMMARY_MAX_CHARS),
                    raw,
                });
            }
            Some("tool_call_update") => {
                let id = update
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let status = update
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let mut tracked = self.tool_calls.get(&id).cloned().unwrap_or_default();
                if let Some(kind) = update.get("kind").and_then(Value::as_str) {
                    tracked.kind = kind.to_string();
                }
                if let Some(title) = update.get("title").and_then(Value::as_str) {
                    tracked.title = title.to_string();
                }
                if update.get("kind").is_some()
                    || update.get("rawInput").is_some()
                    || update.get("locations").is_some()
                {
                    tracked.subject = tool_call_subject(&tracked.kind, &tracked.title, &update);
                }
                self.track_tool_call(id.clone(), tracked.clone())?;
                match status.as_str() {
                    // Terminal statuses surface as first-class ToolResult
                    // events; progress updates (pending/in_progress) are
                    // transcript-only. `failed` is a normal failure, NOT a
                    // denial (mirrors backend_codex) — denials are
                    // synthesized at the permission seam.
                    "completed" | "failed" => {
                        self.tool_calls.remove(&id);
                        self.queue.push_back(AgentEvent::ToolResult {
                            tool: Some(tracked.kind.clone()),
                            denied: false,
                            summary: tool_result_summary(&update, &tracked, &status),
                            raw,
                        });
                    }
                    _ => self.queue.push_back(AgentEvent::Other { raw }),
                }
            }
            Some("usage_update") => {
                // Remembered for the terminal Result's cost; the update
                // itself is transcript-only (no AgentEvent kind for
                // mid-stream usage, and `used`/`size` are context-window
                // state, not a billable token split).
                self.last_usage = Some(update.clone());
                self.queue.push_back(AgentEvent::Other { raw });
            }
            _ => self.queue.push_back(AgentEvent::Other { raw }),
        }
        Ok(())
    }

    /// Answer one `session/request_permission` at the seam; a refusal is
    /// synthesized into a `denied` ToolResult so the guardrail firing is a
    /// first-class event (the peer may report nothing itself).
    async fn handle_permission_request(
        &mut self,
        id: Value,
        params: &Value,
        raw: Value,
    ) -> Result<()> {
        if self.acp_session_id.is_none()
            || params.get("sessionId").and_then(Value::as_str) != self.acp_session_id.as_deref()
        {
            self.write_message(json!({
                "jsonrpc": "2.0", "id": id,
                "result": { "outcome": { "outcome": "cancelled" } },
            }))
            .await?;
            return Err(EngineError::Backend(
                "acp permission has a foreign or missing sessionId".into(),
            ));
        }
        let call_update = params.get("toolCall").cloned().unwrap_or(Value::Null);
        let call_id = call_update
            .get("toolCallId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty() && id.len() <= 256);
        let missing_id = call_id.is_none();
        let call_id = call_id.unwrap_or_default().to_string();
        // Merge what the request carries with what the tool_call/update
        // stream already told us about this call.
        let mut info = self.tool_calls.get(&call_id).cloned().unwrap_or_default();
        if let Some(kind) = call_update.get("kind").and_then(Value::as_str) {
            info.kind = kind.to_string();
        }
        if info.kind.is_empty() {
            info.kind = "other".to_string();
        }
        if let Some(title) = call_update.get("title").and_then(Value::as_str) {
            info.title = title.to_string();
        }
        if info.subject.is_empty()
            || call_update.get("kind").is_some()
            || call_update.get("rawInput").is_some()
            || call_update.get("locations").is_some()
        {
            info.subject = tool_call_subject(&info.kind, &info.title, &call_update);
        }
        if !missing_id {
            self.track_tool_call(call_id.clone(), info.clone())?;
        }

        let decision = if missing_id {
            PermissionDecision::Deny("tool call has no valid action identity (toolCallId)".into())
        } else {
            match decide_permission(&self.spec, &info) {
                PermissionDecision::Allow
                    if !call_update.get("rawInput").is_some_and(Value::is_object)
                        || info.subject.trim().is_empty() =>
                {
                    PermissionDecision::Deny(
                        "the complete action is unavailable for one-call consent".into(),
                    )
                }
                decision => decision,
            }
        };
        let peer_id = serde_json::to_string(&id)?;
        if self.pending_permissions.len() >= crate::live_permission::MAX_PENDING
            || self.seen_permission_ids.len() >= 1024
            || !self.seen_permission_ids.insert(peer_id)
            || self
                .pending_permissions
                .values()
                .any(|p| p.proposal.tool_call_id == call_id)
        {
            return Err(EngineError::Backend(
                "duplicate or excessive ACP permission requests".into(),
            ));
        }
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut action = call_update;
        if let Some(fields) = action.as_object_mut() {
            fields.insert("kind".into(), Value::String(info.kind));
        }
        let now = chrono::Utc::now();
        let mut proposal = crate::live_permission::Proposal {
            id: format!("permission-{}", uuid::Uuid::new_v4()),
            engine_session_id: self.session_id.clone(),
            peer_session_id: self.acp_session_id.clone().expect("validated above"),
            peer_request_id: id,
            tool_call_id: call_id,
            action_digest: crate::live_permission::digest(&action)?,
            options_digest: crate::live_permission::digest(&options)?,
            action,
            options,
            observed_at: now,
            deadline: now + chrono::Duration::seconds(crate::live_permission::REQUEST_TTL_SECS),
            prohibition: match decision {
                PermissionDecision::Deny(reason) => Some(reason),
                PermissionDecision::Allow => None,
            },
        };
        if proposal.option(true).is_none() && proposal.prohibition.is_none() {
            proposal.prohibition = Some("no unique certified allow_once option was offered".into());
        }
        proposal.validate()?;
        self.pending_permissions.insert(
            proposal.id.clone(),
            PendingPermission {
                proposal: proposal.clone(),
                expires_at: tokio::time::Instant::now()
                    + std::time::Duration::from_secs(
                        crate::live_permission::REQUEST_TTL_SECS as u64,
                    ),
            },
        );
        self.queue.push_back(AgentEvent::PermissionRequested {
            proposal: Box::new(proposal),
            raw,
        });
        Ok(())
    }

    async fn answer_permission(&mut self, answer: crate::live_permission::Answer) -> Result<()> {
        // The biased read may have consumed a fragment before yielding. Do
        // not authorize against an earlier description until that frame has
        // been classified. The existing permission deadline bounds this wait.
        if self.lines.has_partial_line() {
            self.deferred_permission_answer = Some(answer);
            return Ok(());
        }
        let Some(pending) = self.pending_permissions.get(&answer.proposal.id) else {
            return Err(EngineError::Backend(
                "permission response names no live request".into(),
            ));
        };
        if pending.proposal != answer.proposal
            || chrono::Utc::now() >= pending.proposal.deadline
            || tokio::time::Instant::now() >= pending.expires_at
            || (answer.allow
                && (pending.proposal.prohibition.is_some()
                    || pending.proposal.option(true).is_none()))
        {
            return Err(EngineError::Backend(
                "stale or prohibited permission response".into(),
            ));
        }
        let proposal = self
            .pending_permissions
            .remove(&answer.proposal.id)
            .expect("checked above")
            .proposal;
        let decision = if answer.allow {
            PermissionDecision::Allow
        } else {
            PermissionDecision::Deny("one-call consent refused".into())
        };
        let result = permission_response(&decision, &proposal.options);
        let sent = self
            .write_message(json!({
                "jsonrpc": "2.0", "id": proposal.peer_request_id, "result": result,
            }))
            .await;
        let delivery = if sent.is_ok() {
            crate::live_permission::Delivery::Sent
        } else {
            crate::live_permission::Delivery::Uncertain
        };
        self.queue.push_back(AgentEvent::PermissionResponded {
            request_id: proposal.id.clone(),
            delivery: delivery.clone(),
            raw: json!({"permissionResponse":proposal.id,"delivery":delivery}),
        });
        sent
    }

    /// Synthesize the terminal `Result` from a `session/prompt` response
    /// (see module docs): text stitched from the turn's last assistant
    /// message, `is_error` ⇔ `stopReason != "end_turn"`, cost only when the
    /// peer reported a USD amount — absent data stays absent.
    ///
    /// WHY non-`end_turn` is an error, not just `refusal` (12th-pass review):
    /// ACP v1's stop reasons are `end_turn` (natural completion), `refusal`,
    /// `max_tokens`, `max_turn_requests`, and `cancelled`. Mapping only
    /// `refusal` to `is_error` let a turn cut short by `max_tokens`/
    /// `max_turn_requests` — or answered `cancelled`, or carrying a missing
    /// or unrecognized reason — surface as a SUCCESSFUL result, so a
    /// truncated validator report could pass validation. Fail-closed is the
    /// only honest mapping: anything but a natural completion is an error,
    /// and the reason string rides the raw payload so the failure is
    /// diagnosable (`null` when the peer omitted the field entirely).
    fn synthesize_result(&mut self, outcome: RpcOutcome, request_id: u64) {
        let cumulative_cost_usd = self
            .last_usage
            .as_ref()
            .and_then(|u| u.get("cost"))
            .filter(|cost| {
                cost.get("currency").and_then(Value::as_str) == Some("USD")
                    && cost.get("amount").and_then(Value::as_f64).is_some()
            })
            .and_then(|cost| cost.get("amount").and_then(Value::as_f64))
            .filter(|amount| amount.is_finite() && *amount >= 0.0);
        let last_cost_usd = match (
            self.saw_result,
            self.previous_cost_total,
            cumulative_cost_usd,
        ) {
            (false, _, total) => total,
            (true, Some(previous), Some(total)) if total >= previous => Some(total - previous),
            _ => None,
        };
        self.previous_cost_total = cumulative_cost_usd;
        let (text, is_error, raw) = match outcome {
            RpcOutcome::Result(result) => {
                let stop_reason = result.get("stopReason").and_then(Value::as_str);
                (
                    std::mem::take(&mut self.message_text),
                    stop_reason != Some("end_turn"),
                    json!({
                        "promptResponse": result,
                        "usageUpdate": self.last_usage,
                        "costScope": "turn_delta_from_reported_session_total",
                        "synthesizedBy": "backend_acp",
                        // The classification input, verbatim: exactly what the
                        // peer sent, `null` when it sent nothing — the raw
                        // payload always explains WHY a non-end_turn failed.
                        "stopReason": stop_reason,
                    }),
                )
            }
            RpcOutcome::Error(error) => (
                format!("acp session/prompt failed: {error}"),
                true,
                json!({
                    "promptError": error,
                    "requestId": request_id,
                    "synthesizedBy": "backend_acp",
                }),
            ),
        };
        let event = AgentEvent::Result {
            text,
            is_error,
            usage: TokenUsage::default(),
            cost_usd: last_cost_usd,
            num_turns: Some(1),
            raw,
        };
        self.observe(&event);
        self.queue.push_back(event);
    }

    fn observe(&mut self, event: &AgentEvent) {
        if let AgentEvent::Result { .. } = event {
            self.saw_result = true;
        }
    }

    /// Kill only while the leader is still owned. Never signal a cached PID
    /// after reaping; the same-group cleanup precedes Child::wait.
    async fn kill_child(&mut self) {
        self.stdin.take();
        if self.child_status.is_none() {
            kill_owned_peer(
                &mut self.child,
                #[cfg(windows)]
                self.job.as_ref(),
            );
            if let Ok(Ok(status)) = tokio::time::timeout(CLEANUP_TIMEOUT, self.child.wait()).await {
                self.child_status = Some(status);
            }
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if let Some(container) = self.container.as_mut() {
            if let Err(error) = container.remove().await {
                self.cleanup_failure
                    .get_or_insert("container removal failed");
                tracing::error!(%error, "ACP cleanup requires recovery");
            }
        }
        if let Some(profile) = self.profile_home.as_mut() {
            if let Err(error) = profile.close() {
                self.cleanup_failure
                    .get_or_insert("private credential home cleanup failed");
                tracing::error!(%error, "ACP private home requires recovery");
            }
        }
        self.finish_stderr().await;
    }

    async fn finish_stderr(&mut self) {
        if let Some(mut task) = self.stderr_task.take() {
            match tokio::time::timeout(CLEANUP_TIMEOUT, &mut task).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    self.cleanup_failure
                        .get_or_insert("stderr drain task failed");
                }
                Err(_) => {
                    self.cleanup_failure.get_or_insert("stderr drain timed out");
                    task.abort();
                    let _ = task.await;
                }
            }
        }
    }

    /// A single-shot ACP turn ends with its response, even if the adapter is
    /// a long-lived server. Give it a short stdin-EOF grace, then terminate
    /// our owned process group. Contained peers terminate under the trusted
    /// supervisor; the Docker client's exit status must arrive before success.
    async fn finish_session(&mut self) {
        self.stdin.take();
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let contained = self.container.is_some();
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let contained = false;
        let completion_timeout = if contained {
            CONTAINER_COMPLETION_TIMEOUT
        } else {
            COMPLETION_GRACE
        };
        let mut forced = false;
        if self.child_status.is_none() {
            match tokio::time::timeout(completion_timeout, wait_for_peer_exit(&mut self.child))
                .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    self.kill_child().await;
                    self.exit = Some(SessionExit::Failed(format!(
                        "acp process observation failed: {error}"
                    )));
                    return;
                }
                Err(_) if contained => {
                    self.cleanup_failure
                        .get_or_insert("container completion deadline expired");
                }
                Err(_) => forced = true,
            }
            self.kill_child().await;
        } else {
            self.kill_child().await;
        }
        if let Some(cause) = self.cleanup_failure {
            self.exit = Some(SessionExit::Failed(format!(
                "acp cleanup could not be confirmed: {cause}"
            )));
            return;
        }
        self.exit = Some(match self.child_status {
            Some(status) if self.saw_result && (status.success() || forced) => {
                SessionExit::Completed
            }
            Some(status) => SessionExit::Failed(format!(
                "acp agent exited with {status}{}; stderr tail: {}",
                if self.saw_result {
                    ""
                } else {
                    " without answering session/prompt"
                },
                self.stderr_tail(),
            )),
            None => SessionExit::Failed("acp process did not reap within cleanup deadline".into()),
        });
    }

    fn stderr_tail(&self) -> String {
        let captured = self
            .stderr_buf
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let captured = self
            .profile_home
            .as_ref()
            .map_or_else(|| captured.clone(), |p| p.scrub(captured.clone()));
        last_chars(captured.trim_end(), STDERR_TAIL_CHARS)
    }
}

#[async_trait::async_trait]
impl AgentSession for AcpSession {
    fn permission_responder(&self) -> Option<crate::live_permission::PermissionResponder> {
        Some(self.permission_responder.clone())
    }

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
            if self.saw_result && matches!(self.spec.prompt, PromptMode::SingleShot(_)) {
                self.finish_session().await;
                return Ok(None);
            }
            let frame = match self.read_frame().await {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    self.finish_session().await;
                    return Ok(None);
                }
                Err(e) => {
                    self.kill_child().await;
                    self.exit = Some(SessionExit::Failed(e.to_string()));
                    return Ok(None);
                }
            };
            if let Err(e) = self.handle_frame(frame).await {
                // A transport error mid-session (stdin write failed — the
                // peer is gone): fail the session honestly rather than hang.
                self.kill_child().await;
                self.exit = Some(SessionExit::Failed(e.to_string()));
                // Preserve the rejected frame's diagnostic event.
                continue;
            }
        }
    }

    async fn send_user_message(&mut self, text: &str) -> Result<()> {
        if self.exit.is_some() {
            return Err(EngineError::Backend(
                "acp session is closed; cannot send further messages".to_string(),
            ));
        }
        if self.acp_session_id.is_none() {
            return Err(EngineError::Backend(
                "acp session not established yet; cannot send a message".to_string(),
            ));
        }
        if matches!(self.spec.prompt, PromptMode::SingleShot(_)) || self.prompt_request_id.is_some()
        {
            return Err(EngineError::Backend(
                "acp session cannot accept an overlapping or single-shot follow-up".into(),
            ));
        }
        self.send_prompt(text).await
    }

    async fn abort(&mut self) -> Result<()> {
        if self.exit.is_some() {
            return Ok(());
        }
        // Best-effort graceful cancel first (the peer MAY stop its turn
        // cleanly and answer the prompt with stopReason "cancelled"), then
        // the house tree-kill regardless — abort must never depend on the
        // peer honoring the notification.
        if let Some(acp_session_id) = self.acp_session_id.clone() {
            let _ = tokio::time::timeout(
                CANCEL_WRITE_TIMEOUT,
                self.write_message(json!({
                    "jsonrpc": "2.0",
                    "method": method::SESSION_CANCEL,
                    "params": { "sessionId": acp_session_id },
                })),
            )
            .await;
        }
        self.kill_child().await;
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let container_cleanup_failed = self.container.is_some()
            && (self.cleanup_failure.is_some() || self.child_status.is_none());
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        let container_cleanup_failed = false;
        if container_cleanup_failed {
            let cause = self
                .cleanup_failure
                .unwrap_or("host process did not reap within cleanup deadline");
            let message = format!("acp abort cleanup could not be confirmed: {cause}");
            self.exit = Some(SessionExit::Failed(message.clone()));
            return Err(EngineError::Backend(message));
        }
        self.exit = Some(SessionExit::Aborted);
        Ok(())
    }

    fn exit_status(&self) -> Option<SessionExit> {
        self.exit.clone()
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_acp_classify_distinguishes_response_request_notification() {
        let response =
            classify_line(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#);
        assert!(matches!(
            response,
            Frame::Response {
                id: 3,
                outcome: RpcOutcome::Result(_)
            }
        ));
        let error =
            classify_line(r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32603,"message":"boom"}}"#);
        assert!(matches!(
            error,
            Frame::Response {
                id: 4,
                outcome: RpcOutcome::Error(_)
            }
        ));
        let request = classify_line(
            r#"{"jsonrpc":"2.0","id":100,"method":"session/request_permission","params":{}}"#,
        );
        assert!(matches!(
            request,
            Frame::Request { ref method, .. } if method == "session/request_permission"
        ));
        let notification = classify_line(
            r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"plan"}}}"#,
        );
        assert!(matches!(
            notification,
            Frame::Notification { ref method, .. } if method == "session/update"
        ));
        // Classification retains the diagnostic; the session fails on it.
        let torn = classify_line(r#"{"jsonrpc":"2.0","method":"session/upda"#);
        assert!(matches!(torn, Frame::Unrecognized(_)));
    }

    #[test]
    fn backend_acp_wildcard_match_anchors_like_a_shell_glob() {
        assert!(wildcard_match("git push*", "git push origin main"));
        assert!(wildcard_match("git push*", "git push"));
        assert!(!wildcard_match("git push*", "git pull"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match(
            "cargo * --workspace",
            "cargo test --workspace"
        ));
        assert!(!wildcard_match(
            "cargo * --workspace",
            "cargo test --package x"
        ));
        assert!(wildcard_match("*/etc/passwd", "/etc/passwd"));
        assert!(!wildcard_match("*/etc/passwd", "/etc/passwd.bak"));
    }

    #[test]
    fn backend_acp_pattern_matches_maps_claude_names_to_acp_kinds() {
        assert!(pattern_matches(
            "Bash(git push*)",
            "execute",
            "git push origin main"
        ));
        assert!(!pattern_matches("Bash(git push*)", "execute", "git pull"));
        assert!(!pattern_matches(
            "Bash(git push*)",
            "edit",
            "git push origin main"
        ));
        assert!(pattern_matches("Write", "edit", "/repo/src/main.rs"));
        assert!(!pattern_matches("Write", "read", "/repo/src/main.rs"));
        assert!(pattern_matches("Edit(/etc/*)", "edit", "/etc/hosts"));
        assert!(pattern_matches("Read", "read", "/anywhere"));
        // Unknown tool names match nothing (deny-only mapping; never widens).
        assert!(!pattern_matches("NotAClaudeTool(*)", "execute", "x"));
    }

    fn spec_with(writable: bool, disallowed: &[&str]) -> SessionSpec {
        SessionSpec {
            cwd: PathBuf::from("."),
            prompt: PromptMode::SingleShot("do the thing".to_string()),
            append_system_prompt: None,
            model: "acp-model".to_string(),
            effort: "high".to_string(),
            session_id: "sess-1".to_string(),
            resume: None,
            permission_mode: None,
            allowed_tools: vec![],
            disallowed_tools: disallowed.iter().map(|s| s.to_string()).collect(),
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

    #[test]
    fn backend_acp_permission_denies_disallowed_and_mutating_kinds() {
        let spec = spec_with(true, &["Bash(git push*)"]);
        let push = ToolCallInfo {
            kind: "execute".to_string(),
            title: "git push origin main".to_string(),
            subject: "git push origin main".to_string(),
        };
        assert!(matches!(
            decide_permission(&spec, &push),
            PermissionDecision::Deny(reason) if reason.contains("Bash(git push*)")
        ));
        let test = ToolCallInfo {
            kind: "execute".to_string(),
            title: "cargo test".to_string(),
            subject: "cargo test".to_string(),
        };
        assert_eq!(decide_permission(&spec, &test), PermissionDecision::Allow);

        // Read-only posture: mutating kinds refused, execute/read allowed.
        let ro = spec_with(false, &[]);
        let edit = ToolCallInfo {
            kind: "edit".to_string(),
            title: "write src/main.rs".to_string(),
            subject: "/repo/src/main.rs".to_string(),
        };
        assert!(matches!(
            decide_permission(&ro, &edit),
            PermissionDecision::Deny(reason) if reason.contains("writable: false")
        ));
        assert_eq!(decide_permission(&ro, &test), PermissionDecision::Allow);
        let read = ToolCallInfo {
            kind: "read".to_string(),
            title: "read src/main.rs".to_string(),
            subject: "/repo/src/main.rs".to_string(),
        };
        assert_eq!(decide_permission(&ro, &read), PermissionDecision::Allow);
    }

    /// The permission seam used to fail OPEN when the peer omitted the
    /// subject: `wildcard_match("git push*", "")` is false, so no deny fired
    /// and the decision was Allow. Every glob-carrying deny rule was
    /// bypassable that way, by a peer that need not even be hostile.
    #[test]
    fn backend_acp_permission_denies_when_the_subject_is_missing() {
        let spec = spec_with(true, &["Bash(git push*)"]);
        let no_subject = ToolCallInfo {
            kind: "execute".to_string(),
            title: String::new(),
            subject: String::new(),
        };
        assert!(
            matches!(
                decide_permission(&spec, &no_subject),
                PermissionDecision::Deny(ref reason)
                    if reason.contains("no subject") && reason.contains("Bash(git push*)")
            ),
            "got {:?}",
            decide_permission(&spec, &no_subject)
        );

        // Whitespace is no subject either.
        let blank_subject = ToolCallInfo {
            subject: "   ".to_string(),
            ..no_subject.clone()
        };
        assert!(matches!(
            decide_permission(&spec, &blank_subject),
            PermissionDecision::Deny(_)
        ));

        // Precise: a deny list that does not cover this call's kind is not
        // made to fire by a missing subject.
        let read_no_subject = ToolCallInfo {
            kind: "read".to_string(),
            ..no_subject.clone()
        };
        assert_eq!(
            decide_permission(&spec_with(true, &["Bash(git push*)"]), &read_no_subject),
            PermissionDecision::Allow
        );
    }

    /// Read-only posture: `MUTATING_KINDS` cannot classify a call whose kind
    /// the peer omitted (it arrives as `"other"`), so the containment claim
    /// cannot be checked and the call is refused.
    #[test]
    fn backend_acp_read_only_denies_an_unclassifiable_kind() {
        let ro = spec_with(false, &[]);
        for kind in ["", "other"] {
            let call = ToolCallInfo {
                kind: kind.to_string(),
                title: "do something".to_string(),
                subject: "/repo/src/main.rs".to_string(),
            };
            assert!(
                matches!(
                    decide_permission(&ro, &call),
                    PermissionDecision::Deny(ref reason) if reason.contains("kind")
                ),
                "kind {kind:?} got {:?}",
                decide_permission(&ro, &call)
            );
        }
        // Unknown kinds cannot bypass deny rules in writable sessions either.
        let writable = spec_with(true, &[]);
        let other = ToolCallInfo {
            kind: "other".to_string(),
            title: "think".to_string(),
            subject: "think".to_string(),
        };
        assert!(matches!(
            decide_permission(&writable, &other),
            PermissionDecision::Deny(_)
        ));
    }

    #[test]
    fn backend_acp_permission_response_picks_options_or_cancels() {
        let options = vec![
            json!({ "optionId": "allow-1", "name": "Allow", "kind": "allow_once" }),
            json!({ "optionId": "reject-1", "name": "Reject", "kind": "reject_once" }),
        ];
        let allow = permission_response(&PermissionDecision::Allow, &options);
        assert_eq!(allow["outcome"]["optionId"], json!("allow-1"));
        let deny = permission_response(&PermissionDecision::Deny("nope".to_string()), &options);
        assert_eq!(deny["outcome"]["optionId"], json!("reject-1"));
        // No reject option offered: refusal degrades to the cancelled
        // outcome — never an invented selection.
        let deny_no_reject = permission_response(
            &PermissionDecision::Deny("nope".to_string()),
            &[options[0].clone()],
        );
        assert_eq!(deny_no_reject["outcome"]["outcome"], json!("cancelled"));
        for (kind, decision) in [
            ("allow_once", PermissionDecision::Allow),
            ("reject_once", PermissionDecision::Deny("refused".into())),
        ] {
            let ambiguous = vec![
                json!({"optionId":"a","kind":kind}),
                json!({"optionId":"b","kind":kind}),
            ];
            assert_eq!(
                permission_response(&decision, &ambiguous)["outcome"]["outcome"],
                "cancelled"
            );
        }
    }
}
