//! ACP (Agent Client Protocol) agent backend: spawns and supervises an
//! external agent process speaking JSON-RPC 2.0 over NDJSON stdio (KRZ-301 —
//! the vendor-neutrality seam: any agent that speaks ACP can drive a kranz
//! mission role without a per-vendor CLI parser).
//!
//! **Schema version targeted:** ACP protocol version **1** — the stable
//! `schema/v1/schema.json` of `agentclientprotocol/agent-client-protocol`
//! (meta.json `"version": 1`, crate release v1.6.0, 2026-07-21). v2 shapes
//! (schema.unstable.json) are deliberately NOT used: unstable discriminators
//! would tie the seam to a moving target. Protocol versions are MAJOR-only,
//! so a peer answering `initialize` with anything other than 1 is rejected.
//!
//! ## Wire → event mapping
//!
//! | ACP message | maps to |
//! |---|---|
//! | `initialize` + `session/new` responses | synthesized [`AgentEvent::Init`] (the peer's `sessionId`; model recorded from config — ACP v1 has no standard model-reporting field) |
//! | `session/update` `agent_message_chunk` (text) | [`AgentEvent::Text`] per chunk (deltas) |
//! | `session/update` `tool_call` | [`AgentEvent::ToolUse`] (`tool` = ACP `kind`, `summary` = `title`) |
//! | `session/update` `tool_call_update` with terminal status (`completed`/`failed`) | [`AgentEvent::ToolResult`] (`denied: false` — a failed tool is a normal failure, not a denial, mirroring `backend_codex`) |
//! | `session/request_permission` refused at the seam | synthesized [`AgentEvent::ToolResult`] with `denied: true` (the refusal IS the event — the peer may report nothing itself) |
//! | `session/update` `usage_update` | remembered; its `cost` (USD only) lands on the terminal `Result.cost_usd` |
//! | `session/prompt` response (`stopReason`) | synthesized terminal [`AgentEvent::Result`]: text stitched from the LAST assistant message's chunks (mirrors `backend_codex` "last agent_message wins"; `messageId` changes delimit messages), `is_error` ⇔ `stopReason != "end_turn"` — ACP v1 names `end_turn` as the ONLY natural completion, so `refusal`, `max_tokens`, `max_turn_requests`, `cancelled`, and any missing/unknown reason all fail honestly (12th-pass review: a truncated or cancelled validator report must never read as a pass) |
//! | everything else (`plan`, `agent_thought_chunk`, `available_commands_update`, unknown kinds, unparseable lines) | [`AgentEvent::Other`] — kept for transcripts, never dropped |
//!
//! ## What is NOT on this wire (absent, never fabricated)
//!
//! ACP v1's `usage_update` reports context-window state (`used`/`size`
//! tokens) and an optional cumulative `cost` — NOT an input/output/cache
//! token split. [`AgentEvent::Result.usage`] therefore stays the zero
//! default (a fabricated split would be invented data), and `cost_usd` is
//! `Some` only when the peer reported a USD amount. There is likewise no
//! client-side price-table fallback: kranz cannot price an arbitrary ACP
//! peer's model, so unreported cost stays `None`. Model selection is the
//! peer's own concern (encoded in the configured command/args); the
//! configured model string is recorded on `Init` for attribution only.
//!
//! ## Permission mapping (the allow/deny seam)
//!
//! The peer asks before acting via `session/request_permission`; the answer
//! is computed from the [`SessionSpec`] (see [`decide_permission`]):
//!
//! - **Disallowed patterns** (`spec.disallowed_tools`, claude-shaped strings
//!   like `Bash(git push*)`) are matched against the ACP `kind` via
//!   [`TOOL_NAME_KINDS`] (`Bash`↔`execute`, `Edit`/`Write`↔`edit`, …) and a
//!   `*`-wildcard glob against the call's subject (command for `execute`,
//!   path for the file kinds, title otherwise). A match refuses the call —
//!   this is where the no-push/no-publish invariants land.
//! - **Read-only sessions** (`writable: false`) refuse the filesystem-
//!   mutating kinds `edit`/`delete`/`move` outright; `execute` stays allowed
//!   unless disallowed-matched, because a read-only role still runs
//!   read-only commands (`git diff`, `cargo test`) and ACP's `execute` kind
//!   does not split reads from writes. This is the ACP equivalent posture;
//!   what it CANNOT do is constrain a peer that never asks permission —
//!   there is no client-side fs proxy in v1, and `BackendKind::Acp` reports
//!   `supports_sandbox_enforcement() == false`, so `config::validate`
//!   refuses an enforced OS sandbox on this backend rather than letting the
//!   gap go silent. `spec.allowed_tools` is not interpreted yet (the peer's
//!   permission request IS the ask; auto-approving without one would weaken
//!   the seam).
//!
//! A refusal picks the first `reject_once` (else `reject_always`) option the
//! peer offered, falling back to the `cancelled` outcome when it offered
//! none; an approval picks the first `allow_once` (else `allow_always`,
//! else first) option.
//!
//! ## Process supervision
//!
//! House discipline, mirroring `backend_kimi`/`backend_codex`: env-cleared
//! spawn ([`crate::agent_env::agent_session_env`] — ACP has no canonical
//! auth env var, so NO ambient credential crosses; the peer authenticates
//! from its own config), unix process-group + post-reap sweep /
//! windows Job Object tree kill, `kill_on_drop`, bounded stdout lines and a
//! bounded stderr tail ([`crate::stream_bounds`]). A killed peer's torn
//! final NDJSON line is never a parse failure: [`BoundedLines`] returns it
//! as one last unterminated line, which routes to [`AgentEvent::Other`]
//! like any unparseable line, so the events already surfaced stay complete
//! and the session simply ends `Aborted`/`Failed`.
//!
//! `resume` is rejected at the seam (`session/load` is an optional v1
//! capability this backend does not negotiate; `session/resume` is
//! unstable-v2 only). Client capabilities advertise `fs`/`terminal` as
//! unsupported, so a conformant peer never calls `fs/*`/`terminal/*`; one
//! that does gets a JSON-RPC `-32601` error response, not silent service.

use crate::backend::{
    AgentBackend, AgentEvent, AgentSession, PromptMode, SessionExit, SessionSpec,
};
#[cfg(unix)]
use crate::backend_claude::kill_group;
#[cfg(windows)]
use crate::backend_claude::win_job;
use crate::error::{EngineError, Result};
use crate::stream_bounds::{drain_to_tail, BoundedLines, STDERR_TAIL_CAP};
use crate::types::TokenUsage;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
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
/// are capability negotiation — no model call — so a peer that cannot answer
/// within this window is hung or not an ACP agent; bounding it keeps
/// `start()` from parking the run loop forever. The prompt turn itself is
/// deliberately unbounded here: turn/stall budgets are the engine's call
/// (runner-level), not the transport's.
const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
    /// `result`/`error` for a client-issued request id.
    Response { id: u64, outcome: RpcOutcome },
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
    /// Not JSON-RPC-shaped (or not JSON at all) — transcript only.
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
    let value = match serde_json::from_str::<Value>(line) {
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
    let has_id = obj.contains_key("id");
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
                (Some(id), Some(result), _) => Frame::Response {
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
/// `rawInput.path` for the file kinds, `title` as the last resort).
fn tool_call_subject(kind: &str, title: &str, call: &Value) -> String {
    let raw_input = call.get("rawInput").cloned().unwrap_or(Value::Null);
    let str_at = |value: &Value, keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| value.get(*k).and_then(Value::as_str).map(str::to_string))
    };
    if kind == "execute" {
        if let Some(command) = str_at(&raw_input, &["command", "cmd"]) {
            return command;
        }
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

/// Whether one claude-shaped permission pattern (`Bash(git push*)`,
/// `Write`, …) covers an ACP tool call of `kind` with `subject`.
fn pattern_matches(pattern: &str, kind: &str, subject: &str) -> bool {
    let (name, glob) = match pattern.split_once('(') {
        Some((name, rest)) => (name.trim(), rest.strip_suffix(')').unwrap_or(rest)),
        None => (pattern.trim(), "*"),
    };
    let kinds = TOOL_NAME_KINDS
        .iter()
        .find(|(n, _)| n.eq_ignore_ascii_case(name))
        .map(|(_, kinds)| *kinds);
    match kinds {
        Some(kinds) if kinds.contains(&kind) => wildcard_match(glob, subject),
        _ => false,
    }
}

/// The seam: decide one permission request from the [`SessionSpec`]. Deny
/// rules are evaluated before the read-only posture so the recorded reason
/// names the most specific rule that fired.
fn decide_permission(spec: &SessionSpec, call: &ToolCallInfo) -> PermissionDecision {
    for pattern in &spec.disallowed_tools {
        if pattern_matches(pattern, &call.kind, &call.subject) {
            return PermissionDecision::Deny(format!(
                "matches SessionSpec.disallowed_tools pattern {pattern:?}"
            ));
        }
    }
    if !spec.writable && MUTATING_KINDS.contains(&call.kind.as_str()) {
        return PermissionDecision::Deny(format!(
            "read-only session (writable: false): ACP kind {:?} mutates the filesystem",
            call.kind
        ));
    }
    PermissionDecision::Allow
}

/// Build the JSON-RPC result answering a permission request. `Allow` picks
/// the first `allow_once` option, then `allow_always`, then the first
/// option at all; `Deny` picks the first `reject_once`, then
/// `reject_always`, and falls back to the `cancelled` outcome when the peer
/// offered no reject option (the only refusal the stable schema guarantees).
fn permission_response(decision: &PermissionDecision, options: &[Value]) -> Value {
    let option_kind = |opt: &Value| -> String {
        opt.get("kind")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let option_id = |opt: &Value| opt.get("optionId").cloned().unwrap_or(Value::Null);
    let pick = |kinds: &[&str]| -> Option<Value> {
        options
            .iter()
            .find(|opt| kinds.contains(&option_kind(opt).as_str()))
            .map(option_id)
    };
    let selected = match decision {
        PermissionDecision::Allow => pick(&["allow_once"])
            .or_else(|| pick(&["allow_always"]))
            .or_else(|| options.first().map(option_id)),
        PermissionDecision::Deny(_) => pick(&["reject_once"]).or_else(|| pick(&["reject_always"])),
    };
    match selected {
        Some(option_id) => json!({ "outcome": { "outcome": "selected", "optionId": option_id } }),
        None => match decision {
            PermissionDecision::Allow => {
                // No allow option at all: the only safe answer is refusal.
                json!({ "outcome": { "outcome": "cancelled" } })
            }
            PermissionDecision::Deny(_) => json!({ "outcome": { "outcome": "cancelled" } }),
        },
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
}

impl AcpBackend {
    /// Spawn `program args` as the ACP agent (no validation performed; the
    /// handshake at session start is the validation).
    pub fn new(program: impl Into<PathBuf>, args: Vec<String>) -> Self {
        AcpBackend {
            program: program.into(),
            args,
        }
    }

    /// The program this backend spawns.
    pub fn program(&self) -> &std::path::Path {
        &self.program
    }
}

#[async_trait::async_trait]
impl AgentBackend for AcpBackend {
    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>> {
        if spec.resume.is_some() {
            return Err(EngineError::Backend(
                "acp backend: resume is unsupported (session/load is an optional v1 \
                 capability this backend does not negotiate)"
                    .to_string(),
            ));
        }
        let model = spec.model.clone();

        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(&spec.cwd)
            // agent-env-clear: CLEARED env from the minimal allowlist. ACP
            // defines no canonical auth env var, so NO ambient credential is
            // injected (auth_env_name None) — the peer authenticates from
            // its own config or fails loudly, never by inheritance.
            .env_clear()
            .envs(crate::agent_env::agent_session_env(
                &spec.env,
                &spec.session_id,
                None,
            ))
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
                    tracing::warn!(error = %e, "failed to create Job Object for acp child; \
                        tree-kill on abort will be unavailable");
                    None
                }
            },
            None => None,
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

        let mut session = AcpSession {
            session_id: spec.session_id.clone(),
            acp_session_id: None,
            model,
            spec,
            child,
            #[cfg(windows)]
            job,
            stdin,
            lines: BoundedLines::new(stdout),
            stderr_buf,
            stderr_task: Some(stderr_task),
            queue: VecDeque::new(),
            next_request_id: 1,
            prompt_request_id: None,
            tool_calls: HashMap::new(),
            message_text: String::new(),
            message_id: None,
            last_usage: None,
            saw_result: false,
            saw_success_result: false,
            exit: None,
        };

        // The handshake is eager: a non-ACP executable (or a hung peer)
        // fails start() loudly rather than mid-run. A handshake failure
        // kills the child before the error crosses back.
        if let Err(e) = session.handshake().await {
            session.kill_child().await;
            return Err(e);
        }
        Ok(Box::new(session))
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A live ACP session (the [`AgentSession`] impl).
///
/// Reading is INLINE in [`AcpSession::next_event`] (no reader task), which
/// keeps memory bounded by [`BoundedLines`] and is deadlock-free for this
/// protocol: the peer blocks waiting for each permission answer, and the
/// read loop answers permission requests synchronously as they arrive, so
/// stdin writes never wait on a peer that is itself waiting on a full
/// stdout pipe.
pub struct AcpSession {
    session_id: String,
    /// The peer-issued session id (`session/new` response).
    acp_session_id: Option<String>,
    model: String,
    /// Kept for permission decisions (`disallowed_tools`, `writable`).
    spec: SessionSpec,
    child: Child,
    #[cfg(windows)]
    job: Option<win_job::JobHandle>,
    stdin: ChildStdin,
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
    /// Accumulated text of the CURRENT assistant message (chunks with the
    /// same `messageId` concatenate; a changed/missing-`messageId` boundary
    /// starts a new message, and the last message wins the terminal Result,
    /// mirroring `backend_codex`'s last-agent_message rule).
    message_text: String,
    message_id: Option<String>,
    /// Latest `usage_update` (context state + optional cumulative USD cost);
    /// its cost lands on the terminal `Result` (see module docs).
    last_usage: Option<Value>,
    saw_result: bool,
    saw_success_result: bool,
    exit: Option<SessionExit>,
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
        self.stdin.write_all(line.as_bytes()).await.map_err(|e| {
            EngineError::Backend(format!("failed to write to acp agent stdin: {e}"))
        })?;
        self.stdin
            .flush()
            .await
            .map_err(|e| EngineError::Backend(format!("failed to flush acp agent stdin: {e}")))?;
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
            .ok_or_else(|| {
                EngineError::Backend("acp session/new response carried no sessionId".to_string())
            })?
            .to_string();
        self.acp_session_id = Some(acp_session_id.clone());
        self.session_id = acp_session_id.clone();
        // Init is synthesized from the handshake (no wire line carries it),
        // mirroring backend_kimi: the configured model is recorded for
        // attribution — ACP v1 has no model-reporting field.
        self.queue.push_back(AgentEvent::Init {
            session_id: acp_session_id,
            model: self.model.clone(),
            raw: json!({
                "initialize": init_result,
                "sessionNew": new_result,
                "synthesizedBy": "backend_acp",
            }),
        });

        let prompt_text = match &self.spec.prompt {
            PromptMode::SingleShot(text) | PromptMode::Streaming(text) => text.clone(),
        };
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
            match self.lines.next_line().await {
                Ok(Some(line)) if line.trim().is_empty() => continue,
                Ok(Some(line)) => return Ok(Some(classify_line(&line))),
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
            Frame::Notification {
                method,
                params,
                raw,
            } => {
                if method == method::SESSION_UPDATE {
                    self.handle_session_update(&params, raw);
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
            }
        }
        Ok(())
    }

    /// Map one `session/update` notification onto events (see module docs).
    fn handle_session_update(&mut self, params: &Value, raw: Value) {
        let update = params.get("update").cloned().unwrap_or(Value::Null);
        match update.get("sessionUpdate").and_then(Value::as_str) {
            Some("agent_message_chunk") => {
                let text = update
                    .get("content")
                    .and_then(|c| c.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if text.is_empty() {
                    self.queue.push_back(AgentEvent::Other { raw });
                    return;
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
                self.tool_calls.insert(
                    id,
                    ToolCallInfo {
                        kind: kind.clone(),
                        title: title.clone(),
                        subject,
                    },
                );
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
                {
                    let tracked = self.tool_calls.entry(id).or_default();
                    if let Some(kind) = update.get("kind").and_then(Value::as_str) {
                        tracked.kind = kind.to_string();
                    }
                    if let Some(title) = update.get("title").and_then(Value::as_str) {
                        tracked.title = title.to_string();
                    }
                }
                match status.as_str() {
                    // Terminal statuses surface as first-class ToolResult
                    // events; progress updates (pending/in_progress) are
                    // transcript-only. `failed` is a normal failure, NOT a
                    // denial (mirrors backend_codex) — denials are
                    // synthesized at the permission seam.
                    "completed" | "failed" => {
                        let tracked = self
                            .tool_calls
                            .get(
                                update
                                    .get("toolCallId")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default(),
                            )
                            .cloned()
                            .unwrap_or_default();
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
        let call_update = params.get("toolCall").cloned().unwrap_or(Value::Null);
        let call_id = call_update
            .get("toolCallId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
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
        if info.subject.is_empty() {
            info.subject = tool_call_subject(&info.kind, &info.title, &call_update);
        }
        self.tool_calls.insert(call_id.clone(), info.clone());

        let decision = decide_permission(&self.spec, &info);
        let options = params
            .get("options")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let result = permission_response(&decision, &options);
        self.write_message(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
        .await?;

        if let PermissionDecision::Deny(reason) = &decision {
            tracing::info!(
                session_id = %self.session_id,
                tool_call_id = %call_id,
                kind = %info.kind,
                decision = "deny",
                reason = %reason,
                "acp permission request refused at the kranz seam"
            );
            self.queue.push_back(AgentEvent::ToolResult {
                tool: Some(info.kind.clone()),
                denied: true,
                summary: truncate_chars(
                    &format!("refused by kranz permission seam: {reason}"),
                    SUMMARY_MAX_CHARS,
                ),
                raw,
            });
        } else {
            // The approval itself is not an event — the peer's own
            // tool_call_update reports the outcome. The request line stays
            // in the transcript.
            self.queue.push_back(AgentEvent::Other { raw });
        }
        Ok(())
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
        let last_cost_usd = self
            .last_usage
            .as_ref()
            .and_then(|u| u.get("cost"))
            .filter(|cost| {
                cost.get("currency").and_then(Value::as_str) == Some("USD")
                    && cost.get("amount").and_then(Value::as_f64).is_some()
            })
            .and_then(|cost| cost.get("amount").and_then(Value::as_f64));
        let (text, is_error, raw) = match outcome {
            RpcOutcome::Result(result) => {
                let stop_reason = result.get("stopReason").and_then(Value::as_str);
                (
                    std::mem::take(&mut self.message_text),
                    stop_reason != Some("end_turn"),
                    json!({
                        "promptResponse": result,
                        "usageUpdate": self.last_usage,
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
        if let AgentEvent::Result { is_error, .. } = event {
            self.saw_result = true;
            if !is_error {
                self.saw_success_result = true;
            }
        }
    }

    /// Kill the child and reap it, best-effort; also joins the stderr
    /// capture task. Mirrors `backend_kimi::KimiSession::kill_child`
    /// exactly: unix process-group SIGKILL (with a post-reap sweep for
    /// stragglers that raced a mid-fork), windows kill-on-close Job Object.
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
                "acp agent exited with {status}{}; stderr tail: {}",
                if self.saw_result {
                    ""
                } else {
                    " without answering session/prompt"
                },
                self.stderr_tail(),
            )),
            Err(e) => SessionExit::Failed(format!(
                "failed to reap acp agent process: {e}; stderr tail: {}",
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
impl AgentSession for AcpSession {
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
            let frame = match self.read_frame().await {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    self.finish_at_eof().await;
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
                return Ok(None);
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
        self.send_prompt(text).await
    }

    async fn abort(&mut self) -> Result<()> {
        // Best-effort graceful cancel first (the peer MAY stop its turn
        // cleanly and answer the prompt with stopReason "cancelled"), then
        // the house tree-kill regardless — abort must never depend on the
        // peer honoring the notification.
        if let Some(acp_session_id) = self.acp_session_id.clone() {
            let _ = self
                .write_message(json!({
                    "jsonrpc": "2.0",
                    "method": method::SESSION_CANCEL,
                    "params": { "sessionId": acp_session_id },
                }))
                .await;
        }
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
        // Garbage is never a hard failure — transcript only.
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
    }
}
