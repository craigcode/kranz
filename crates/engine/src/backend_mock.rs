//! Mock agent backend — scripted sessions so the whole engine tests without
//! model calls (plan §8: "the seam").
//!
//! [`MockBackend`] holds a FIFO queue of [`MockScript`]s; each
//! [`AgentBackend::start`] call consumes the front script and returns a
//! [`MockSession`] that replays the scripted [`AgentEvent`]s. The backend
//! records every [`SessionSpec`] it was started with and every user message
//! injected into its sessions, so tests can assert on exactly what the engine
//! asked for.
//!
//! Blocking semantics mirror the real CLI backend: a single-shot session's
//! stream closes after its scripted events; a streaming session with an empty
//! queue parks in [`AgentSession::next_event`] until `send_user_message`
//! supplies the next scripted batch or `abort` closes the stream. Because the
//! `AgentSession` trait takes `&mut self`, a parked `next_event` future must
//! be cancelled (e.g. via `tokio::time::timeout` or `select!`) before the
//! caller can send or abort — the same discipline a real child-process stream
//! requires.

use crate::backend::{AgentBackend, AgentEvent, AgentSession, SessionExit, SessionSpec};
use crate::error::{EngineError, Result};
use crate::types::TokenUsage;
use serde_json::json;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

// ---------------------------------------------------------------------------
// Script
// ---------------------------------------------------------------------------

/// One scripted agent session.
#[derive(Debug, Clone)]
pub struct MockScript {
    /// Yielded in order by `next_event`.
    pub events: Vec<AgentEvent>,
    /// Each `send_user_message` appends the next batch to the pending queue
    /// (streaming sessions).
    pub on_message: VecDeque<Vec<AgentEvent>>,
    /// Exit reported once the stream closes (ignored when aborted, and never
    /// reached by streaming sessions, which only end via `abort`).
    pub exit: SessionExit,
    /// When false, `send_user_message` errors (single-shot session).
    pub streaming: bool,
    /// Session id override; defaults to `spec.session_id`.
    pub session_id: Option<String>,
}

impl Default for MockScript {
    fn default() -> Self {
        MockScript {
            events: Vec::new(),
            on_message: VecDeque::new(),
            exit: SessionExit::Completed,
            streaming: false,
            session_id: None,
        }
    }
}

impl MockScript {
    /// A completed single-shot run: init → text → successful result carrying
    /// `final_text`, with the standard mock usage/cost.
    pub fn single_shot(final_text: &str) -> Self {
        MockScript {
            events: vec![
                mock_init("mock-session"),
                mock_text(final_text),
                mock_result_text(final_text),
            ],
            ..Default::default()
        }
    }

    /// Like [`single_shot`](Self::single_shot) but the text/result carry the
    /// serialized JSON — for worker-report / validator-report runs where the
    /// engine parses the result text.
    pub fn single_shot_json(value: &serde_json::Value) -> Self {
        let text = value.to_string();
        MockScript {
            events: vec![
                mock_init("mock-session"),
                mock_text(&text),
                mock_result_json(value),
            ],
            ..Default::default()
        }
    }

    /// A streaming-input session that yields `initial` and then waits for
    /// injected user messages. Chain [`responding`](Self::responding) to
    /// script the per-message batches.
    pub fn streaming(initial: Vec<AgentEvent>) -> Self {
        MockScript { events: initial, streaming: true, ..Default::default() }
    }

    /// Script the batches released by successive `send_user_message` calls.
    pub fn responding(mut self, batches: Vec<Vec<AgentEvent>>) -> Self {
        self.on_message = batches.into();
        self
    }

    /// Override the exit reported when the stream closes.
    pub fn with_exit(mut self, exit: SessionExit) -> Self {
        self.exit = exit;
        self
    }

    /// Override the session id reported by [`AgentSession::session_id`]
    /// (defaults to `spec.session_id`).
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Event helpers — the standard shapes scripts are built from
// ---------------------------------------------------------------------------

/// The token usage attached to every mock result event.
fn mock_usage() -> TokenUsage {
    TokenUsage { input: 1000, output: 200, cache_read: 0, cache_write: 0 }
}

/// System init event (first message of every session).
pub fn mock_init(session_id: &str) -> AgentEvent {
    AgentEvent::Init {
        session_id: session_id.to_string(),
        model: "mock-model".to_string(),
        raw: json!({
            "mock": true,
            "type": "system",
            "subtype": "init",
            "session_id": session_id,
            "model": "mock-model",
        }),
    }
}

/// Assistant text output.
pub fn mock_text(text: &str) -> AgentEvent {
    AgentEvent::Text {
        text: text.to_string(),
        raw: json!({
            "mock": true,
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": text }] },
        }),
    }
}

/// Assistant tool request.
pub fn mock_tool_use(tool: &str, summary: &str) -> AgentEvent {
    AgentEvent::ToolUse {
        tool: tool.to_string(),
        summary: summary.to_string(),
        raw: json!({
            "mock": true,
            "type": "assistant",
            "message": {
                "content": [{ "type": "tool_use", "name": tool, "input": { "summary": summary } }],
            },
        }),
    }
}

/// Successful tool result returned to the model.
pub fn mock_tool_result(tool: &str, summary: &str) -> AgentEvent {
    AgentEvent::ToolResult {
        tool: Some(tool.to_string()),
        denied: false,
        summary: summary.to_string(),
        raw: json!({
            "mock": true,
            "type": "user",
            "tool": tool,
            "content": summary,
            "is_error": false,
        }),
    }
}

/// Tool call blocked by permission rules (guardrail hit, §4.7).
pub fn mock_denied(tool: &str, summary: &str) -> AgentEvent {
    AgentEvent::ToolResult {
        tool: Some(tool.to_string()),
        denied: true,
        summary: summary.to_string(),
        raw: json!({
            "mock": true,
            "type": "user",
            "tool": tool,
            "content": summary,
            "is_error": true,
            "denied": true,
        }),
    }
}

/// Successful terminal result with `text`, standard mock usage and cost 0.01.
pub fn mock_result_text(text: &str) -> AgentEvent {
    AgentEvent::Result {
        text: text.to_string(),
        is_error: false,
        usage: mock_usage(),
        cost_usd: Some(0.01),
        num_turns: Some(1),
        raw: json!({
            "mock": true,
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "result": text,
            "total_cost_usd": 0.01,
            "num_turns": 1,
            "usage": {
                "input_tokens": 1000,
                "output_tokens": 200,
                "cache_read_input_tokens": 0,
                "cache_creation_input_tokens": 0,
            },
        }),
    }
}

/// Like [`mock_result_text`] but the result text is the serialized JSON value
/// (structured-output runs).
pub fn mock_result_json(value: &serde_json::Value) -> AgentEvent {
    let text = value.to_string();
    match mock_result_text(&text) {
        AgentEvent::Result { is_error, usage, cost_usd, num_turns, .. } => AgentEvent::Result {
            text,
            is_error,
            usage,
            cost_usd,
            num_turns,
            raw: json!({
                "mock": true,
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "result": value.to_string(),
                "structured_output": value,
                "total_cost_usd": 0.01,
                "num_turns": 1,
                "usage": {
                    "input_tokens": 1000,
                    "output_tokens": 200,
                    "cache_read_input_tokens": 0,
                    "cache_creation_input_tokens": 0,
                },
            }),
        },
        _ => unreachable!("mock_result_text always builds a Result event"),
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// Scripted [`AgentBackend`]: `start()` pops scripts FIFO and records every
/// spec it saw. Injected user messages are recorded per session, aligned with
/// start order.
#[derive(Default)]
pub struct MockBackend {
    scripts: Mutex<VecDeque<MockScript>>,
    started_specs: Mutex<Vec<SessionSpec>>,
    /// Shared with sessions: `injected[i]` are the messages injected into the
    /// i-th started session.
    injected: Arc<Mutex<Vec<Vec<String>>>>,
}

impl MockBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Backend pre-loaded with scripts (consumed FIFO by `start()`).
    pub fn with_scripts(scripts: Vec<MockScript>) -> Self {
        MockBackend { scripts: Mutex::new(scripts.into()), ..Default::default() }
    }

    /// Queue another script at the back.
    pub fn push_script(&self, script: MockScript) {
        self.scripts.lock().expect("mock scripts lock").push_back(script);
    }

    /// Clones of every spec passed to `start()`, in start order.
    pub fn started_specs(&self) -> Vec<SessionSpec> {
        self.started_specs.lock().expect("mock specs lock").clone()
    }

    /// User messages injected per session, aligned with start order.
    pub fn injected_messages(&self) -> Vec<Vec<String>> {
        self.injected.lock().expect("mock injected lock").clone()
    }
}

#[async_trait::async_trait]
impl AgentBackend for MockBackend {
    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>> {
        let script = self
            .scripts
            .lock()
            .expect("mock scripts lock")
            .pop_front()
            .ok_or_else(|| EngineError::Backend("mock: no script queued".to_string()))?;

        let slot = {
            let mut injected = self.injected.lock().expect("mock injected lock");
            injected.push(Vec::new());
            injected.len() - 1
        };

        let session_id =
            script.session_id.clone().unwrap_or_else(|| spec.session_id.clone());
        self.started_specs.lock().expect("mock specs lock").push(spec.clone());

        Ok(Box::new(MockSession {
            spec,
            session_id,
            streaming: script.streaming,
            pending: script.events.into(),
            on_message: script.on_message,
            script_exit: script.exit,
            exit: None,
            notify: Notify::new(),
            injected: Arc::clone(&self.injected),
            slot,
        }))
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A scripted session handed out by [`MockBackend::start`].
pub struct MockSession {
    /// Clone of the spec this session was started with (also recorded on the
    /// backend via [`MockBackend::started_specs`]).
    pub spec: SessionSpec,
    session_id: String,
    streaming: bool,
    pending: VecDeque<AgentEvent>,
    on_message: VecDeque<Vec<AgentEvent>>,
    script_exit: SessionExit,
    /// Set exactly when the stream closes (None returned, or abort).
    exit: Option<SessionExit>,
    /// Wakes a parked streaming `next_event` after send/abort. `notify_one`
    /// stores a permit, so a wake issued between a state check and the await
    /// is never lost.
    notify: Notify,
    injected: Arc<Mutex<Vec<Vec<String>>>>,
    slot: usize,
}

#[async_trait::async_trait]
impl AgentSession for MockSession {
    fn session_id(&self) -> String {
        self.session_id.clone()
    }

    async fn next_event(&mut self) -> Result<Option<AgentEvent>> {
        // Yield once per call so cooperative schedulers interleave sessions
        // realistically instead of draining one script synchronously.
        tokio::task::yield_now().await;
        loop {
            if self.exit.is_some() {
                // Closed (aborted or already finished). Abort drops any
                // still-pending events, matching process-kill semantics.
                return Ok(None);
            }
            if let Some(event) = self.pending.pop_front() {
                return Ok(Some(event));
            }
            if !self.streaming {
                self.exit = Some(self.script_exit.clone());
                return Ok(None);
            }
            // Streaming with an empty queue: park until send_user_message
            // pushes the next batch or abort() closes the stream. The state
            // is re-checked after every wakeup, so spurious wakes are safe.
            self.notify.notified().await;
        }
    }

    async fn send_user_message(&mut self, text: &str) -> Result<()> {
        if !self.streaming {
            return Err(EngineError::Backend(
                "mock: send_user_message on non-streaming session".to_string(),
            ));
        }
        if self.exit.is_some() {
            return Err(EngineError::Backend(
                "mock: send_user_message on closed session".to_string(),
            ));
        }
        self.injected.lock().expect("mock injected lock")[self.slot].push(text.to_string());
        if let Some(batch) = self.on_message.pop_front() {
            self.pending.extend(batch);
        }
        self.notify.notify_one();
        Ok(())
    }

    async fn abort(&mut self) -> Result<()> {
        if self.exit.is_none() {
            self.exit = Some(SessionExit::Aborted);
        }
        self.notify.notify_one();
        Ok(())
    }

    fn exit_status(&self) -> Option<SessionExit> {
        self.exit.clone()
    }
}
