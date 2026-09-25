//! ACP v1 wire framing and a single-session protocol client.
//!
//! The caller owns I/O scheduling, deadlines, credentials, process supervision,
//! permission decisions and event presentation. This crate never spawns a child
//! or answers a permission request. A cancel notification is not a process kill.
//! Filesystem and session loading are unsupported. Terminals default to false;
//! opt-in consumers must supply and supervise an admitted terminal provider.
//!
//! Use [`io::BoundedLines::new_strict`] before [`classify_line`]. Retain the reader
//! across cancelled reads: an incomplete frame must not race a permission answer.
//! Apply credential filtering before retaining raw frames in any log.

/// Provider-free fixtures shared by consumer conformance tests.
#[cfg(feature = "conformance")]
pub mod conformance {
    pub const TURN: &str = include_str!("../fixtures/turn.jsonl");
}

pub mod io;
pub mod strict_json;
pub mod terminal;
mod wire;

use serde_json::{json, Value};
pub use wire::{classify_line, Frame, RpcOutcome};

pub const PROTOCOL_VERSION: u64 = 1;

pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const SESSION_NEW: &str = "session/new";
    pub const SESSION_PROMPT: &str = "session/prompt";
    pub const SESSION_CANCEL: &str = "session/cancel";
    pub const SESSION_UPDATE: &str = "session/update";
    pub const REQUEST_PERMISSION: &str = "session/request_permission";
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for Error {}

/// Encode one bounded NDJSON frame. The caller bounds the write and flush.
pub fn encode_message(message: &Value) -> Result<String, Error> {
    let mut line = serde_json::to_string(message)
        .map_err(|e| Error(format!("failed to encode acp message: {e}")))?;
    line.push('\n');
    if line.len() > io::STDOUT_LINE_CAP {
        return Err(Error("acp request exceeds the frame byte limit".into()));
    }
    Ok(line)
}

/// A request reserved by the client. On write failure discard the client and
/// terminate its transport; never retry a partially written frame.
#[derive(Debug)]
pub struct Request {
    pub id: u64,
    pub message: Value,
}

#[derive(Debug, Default)]
enum Phase {
    #[default]
    Fresh,
    Initializing(u64),
    Initialized,
    Opening(u64),
    Ready,
}

/// One fresh peer session, with at most one outstanding client request.
/// Peer requests remain visible as [`Frame::Request`] while a prompt is active;
/// the consumer must keep reading while its permission policy waits for input.
#[derive(Debug)]
pub struct Client {
    terminal: bool,
    phase: Phase,
    next_request_id: u64,
    session_id: Option<String>,
    prompt_id: Option<u64>,
}

impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

impl Client {
    pub fn new() -> Self {
        Self {
            terminal: false,
            phase: Phase::Fresh,
            next_request_id: 1,
            session_id: None,
            prompt_id: None,
        }
    }

    /// The embedding consumer must admit and supervise its provider before
    /// choosing this constructor. It enables wire capability only, not policy.
    pub fn with_terminal_support() -> Self {
        Self {
            terminal: true,
            ..Self::new()
        }
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Request, Error> {
        let id = self.next_request_id;
        self.next_request_id = id
            .checked_add(1)
            .ok_or_else(|| Error("acp request id exhausted".into()))?;
        Ok(Request {
            id,
            message: json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        })
    }

    pub fn initialize(&mut self, client_info: Value) -> Result<Request, Error> {
        if !matches!(self.phase, Phase::Fresh) {
            return Err(Error(
                "acp client already initialized or awaiting initialization".into(),
            ));
        }
        let request = self.request(
            method::INITIALIZE,
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": self.terminal,
                },
                "clientInfo": client_info,
            }),
        )?;
        self.phase = Phase::Initializing(request.id);
        Ok(request)
    }

    /// Validate the correlated result, retaining all capability data in the
    /// caller's original value. Optional capabilities do not enable themselves.
    pub fn accept_initialize(&mut self, id: u64, result: &Value) -> Result<(), Error> {
        if !matches!(self.phase, Phase::Initializing(expected) if expected == id) {
            return Err(Error("acp unexpected initialize response".into()));
        }
        let version = result
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if version != PROTOCOL_VERSION {
            return Err(Error(format!(
                "acp agent negotiated protocol version {version}, but this backend speaks \
                 only stable version {PROTOCOL_VERSION} (schema v1)"
            )));
        }
        self.phase = Phase::Initialized;
        Ok(())
    }

    pub fn new_session(&mut self, cwd: &str) -> Result<Request, Error> {
        if !matches!(self.phase, Phase::Initialized) {
            return Err(Error(
                "acp session/new requires completed initialization".into(),
            ));
        }
        let request = self.request(method::SESSION_NEW, json!({"cwd": cwd, "mcpServers": []}))?;
        self.phase = Phase::Opening(request.id);
        Ok(request)
    }

    pub fn accept_session(&mut self, id: u64, result: &Value) -> Result<&str, Error> {
        if !matches!(self.phase, Phase::Opening(expected) if expected == id) {
            return Err(Error("acp unexpected session/new response".into()));
        }
        let session_id = result
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty() && id.len() <= 256)
            .ok_or_else(|| Error("acp session/new response carried no sessionId".into()))?;
        self.session_id = Some(session_id.to_owned());
        self.phase = Phase::Ready;
        Ok(self.session_id.as_deref().expect("established session"))
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    pub fn prompt_in_flight(&self) -> bool {
        self.prompt_id.is_some()
    }

    pub fn prompt(&mut self, text: &str) -> Result<Request, Error> {
        let session_id = self
            .session_id
            .as_deref()
            .ok_or_else(|| Error("acp session not established yet".into()))?;
        if self.prompt_id.is_some() {
            return Err(Error(
                "acp session cannot accept an overlapping prompt".into(),
            ));
        }
        let request = self.request(
            method::SESSION_PROMPT,
            json!({
                "sessionId": session_id,
                "prompt": [{"type": "text", "text": text}],
            }),
        )?;
        self.prompt_id = Some(request.id);
        Ok(request)
    }

    /// Only the matching response completes a turn. Unmatched responses remain
    /// diagnostics; callers interpret the raw stop reason, error and cost.
    pub fn complete_prompt(&mut self, id: u64) -> bool {
        if self.prompt_id == Some(id) {
            self.prompt_id = None;
            true
        } else {
            false
        }
    }

    /// Cooperative cancellation only. The prompt stays outstanding until its
    /// response arrives or the supervisor tears down the entire connection.
    pub fn cancel(&self) -> Option<Value> {
        self.session_id.as_ref().map(|id| {
            json!({
                "jsonrpc": "2.0", "method": method::SESSION_CANCEL,
                "params": {"sessionId": id},
            })
        })
    }

    /// Classify an update without losing unknown fields. Pre-session notices
    /// are diagnostics; after setup a foreign or missing identity is an error.
    pub fn session_update<'a>(
        &self,
        params: &'a Value,
    ) -> Result<Option<SessionUpdate<'a>>, Error> {
        let Some(expected) = self.session_id() else {
            return Ok(None);
        };
        if params.get("sessionId").and_then(Value::as_str) != Some(expected) {
            return Err(Error(
                "acp update has a foreign or missing sessionId".into(),
            ));
        }
        let update = params.get("update").unwrap_or(&Value::Null);
        Ok(Some(SessionUpdate {
            session_id: params["sessionId"].as_str().expect("checked session id"),
            kind: match update.get("sessionUpdate").and_then(Value::as_str) {
                Some("agent_message_chunk") => UpdateKind::AgentMessageChunk,
                Some("tool_call") => UpdateKind::ToolCall,
                Some("tool_call_update") => UpdateKind::ToolCallUpdate,
                Some("usage_update") => UpdateKind::UsageUpdate,
                _ => UpdateKind::Other,
            },
            update,
        }))
    }
}

/// Variants currently interpreted by Kranz. Other variants remain raw, allowing
/// another consumer to interpret them without changing the shared wire format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateKind {
    AgentMessageChunk,
    ToolCall,
    ToolCallUpdate,
    UsageUpdate,
    Other,
}

#[derive(Debug)]
pub struct SessionUpdate<'a> {
    pub session_id: &'a str,
    pub kind: UpdateKind,
    pub update: &'a Value,
}
