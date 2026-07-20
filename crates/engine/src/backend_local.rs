//! Local OpenAI-compatible HTTP backend (`f-2-1`): the first `AgentBackend`
//! that drives an in-engine `reqwest` client rather than a spawned CLI
//! subprocess. Talks to `{base_url}/v1/chat/completions` in non-streaming
//! mode.
//!
//! Single-shot only, mirroring `backend_kimi`: [`LocalSession::send_user_message`]
//! always errors and a `resume`d [`SessionSpec`] is rejected at the seam.
//! Unlike every other backend, the entire request/response round trip
//! happens synchronously inside [`LocalBackend::start`] (there is no child
//! process stdout to poll), so the resulting [`LocalSession`] is just a
//! pre-computed queue of events plus a final [`SessionExit`].
//!
//! Because this HTTP call runs inside the engine process rather than the
//! sandboxed worker subprocess, it needs no localhost egress-allowlist
//! entry.

use crate::backend::{
    AgentBackend, AgentEvent, AgentSession, PromptMode, SessionExit, SessionSpec,
};
use crate::error::{EngineError, Result};
use crate::types::TokenUsage;
use serde_json::{json, Value};
use std::collections::VecDeque;

/// Max characters of a response/error body included in failure messages.
const BODY_TAIL_CHARS: usize = 500;

/// Deliberately crude token estimate (docs/scoping don't yet have a
/// tokenizer dependency for arbitrary local models): 4 chars/token over the
/// assembled message contents. Only used to enforce `context_budget` before
/// spending an HTTP round trip; never used for cost accounting (cost is
/// always `$0.0` for a local run).
const CHARS_PER_TOKEN: usize = 4;

/// Last `max` characters of `text`.
fn last_chars(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(max);
    chars[start..].iter().collect()
}

fn prompt_text(spec: &SessionSpec) -> &str {
    match &spec.prompt {
        PromptMode::SingleShot(text) => text.as_str(),
        PromptMode::Streaming(text) => text.as_str(),
    }
}

/// Assemble the OpenAI-style `messages` array: an optional system message
/// from `append_system_prompt` (when non-empty) followed by the user prompt.
fn build_messages(spec: &SessionSpec) -> Vec<Value> {
    let mut messages = Vec::new();
    if let Some(system) = &spec.append_system_prompt {
        if !system.is_empty() {
            messages.push(json!({"role": "system", "content": system}));
        }
    }
    messages.push(json!({"role": "user", "content": prompt_text(spec)}));
    messages
}

/// `chars/4`, rounded up, summed over every message's `content`.
fn estimate_tokens(messages: &[Value]) -> u32 {
    let total_chars: usize = messages
        .iter()
        .filter_map(|m| m.get("content").and_then(Value::as_str))
        .map(|s| s.chars().count())
        .sum();
    total_chars.div_ceil(CHARS_PER_TOKEN) as u32
}

/// Pull the assistant message text and token usage out of an OpenAI-shaped
/// chat-completions response body.
fn extract_completion(body: &Value) -> std::result::Result<(String, TokenUsage), String> {
    let content = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .ok_or_else(|| {
            format!(
                "response missing choices[0].message.content; body tail: {}",
                last_chars(&body.to_string(), BODY_TAIL_CHARS)
            )
        })?
        .to_string();
    let usage = body.get("usage");
    let input = usage
        .and_then(|u| u.get("prompt_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let output = usage
        .and_then(|u| u.get("completion_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok((
        content,
        TokenUsage {
            input,
            output,
            cache_read: 0,
            cache_write: 0,
        },
    ))
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// The [`AgentBackend`] for an OpenAI-compatible `/v1/chat/completions`
/// endpoint, constructed from a role's `base_url`/`temperature`/
/// `contextBudget` config fields.
#[derive(Debug, Clone)]
pub struct LocalBackend {
    base_url: String,
    temperature: Option<f64>,
    context_budget: u32,
    client: reqwest::Client,
}

impl LocalBackend {
    pub fn new(base_url: String, temperature: Option<f64>, context_budget: u32) -> Self {
        LocalBackend {
            base_url,
            temperature,
            context_budget,
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait::async_trait]
impl AgentBackend for LocalBackend {
    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>> {
        if spec.resume.is_some() {
            return Err(EngineError::Backend(
                "local backend is single-shot only; resume is unsupported".to_string(),
            ));
        }

        let session_id = spec.session_id.clone();
        let model = spec.model.clone();
        let messages = build_messages(&spec);

        let estimated_tokens = estimate_tokens(&messages);
        if estimated_tokens > self.context_budget {
            return Ok(Box::new(LocalSession::context_budget_exceeded(
                session_id,
                model,
                estimated_tokens,
                self.context_budget,
            )));
        }

        let mut request_body = json!({
            "model": model,
            "messages": messages,
        });
        if let Some(temperature) = self.temperature {
            request_body["temperature"] = json!(temperature);
        }

        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        let outcome = match self.client.post(&url).json(&request_body).send().await {
            Ok(response) => {
                let status = response.status();
                let body_text = response.text().await.unwrap_or_default();
                if status.is_success() {
                    match serde_json::from_str::<Value>(&body_text) {
                        Ok(parsed) => Ok(parsed),
                        Err(e) => Err(format!(
                            "failed to parse local backend response as JSON: {e}; body tail: {}",
                            last_chars(&body_text, BODY_TAIL_CHARS)
                        )),
                    }
                } else {
                    Err(format!(
                        "local backend request failed with HTTP {status}; body tail: {}",
                        last_chars(&body_text, BODY_TAIL_CHARS)
                    ))
                }
            }
            Err(e) => Err(format!("local backend request failed: {e}")),
        };

        Ok(Box::new(LocalSession::from_response(
            session_id, model, outcome,
        )))
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A "session" over a single already-completed HTTP round trip: the request
/// happens synchronously in [`LocalBackend::start`], so this is just a
/// pre-computed event queue plus a terminal [`SessionExit`], drained by
/// `next_event`.
pub struct LocalSession {
    session_id: String,
    queue: VecDeque<AgentEvent>,
    pending_exit: Option<SessionExit>,
    exit: Option<SessionExit>,
}

impl LocalSession {
    /// Build the event queue from the outcome of the HTTP round trip: `Ok`
    /// carries the parsed JSON body of a 2xx response, `Err` carries a
    /// descriptive failure message (transport error, non-2xx status, or an
    /// unparseable body).
    fn from_response(
        session_id: String,
        model: String,
        outcome: std::result::Result<Value, String>,
    ) -> Self {
        match outcome {
            Ok(body) => match extract_completion(&body) {
                Ok((content, usage)) => {
                    let mut queue = VecDeque::new();
                    queue.push_back(AgentEvent::Init {
                        session_id: session_id.clone(),
                        model,
                        raw: body.clone(),
                    });
                    queue.push_back(AgentEvent::Text {
                        text: content.clone(),
                        raw: body.clone(),
                    });
                    queue.push_back(AgentEvent::Result {
                        text: content,
                        is_error: false,
                        usage,
                        cost_usd: Some(0.0),
                        num_turns: Some(1),
                        raw: body,
                    });
                    LocalSession {
                        session_id,
                        queue,
                        pending_exit: Some(SessionExit::Completed),
                        exit: None,
                    }
                }
                Err(message) => LocalSession {
                    session_id,
                    queue: VecDeque::new(),
                    pending_exit: Some(SessionExit::Failed(message)),
                    exit: None,
                },
            },
            Err(message) => LocalSession {
                session_id,
                queue: VecDeque::new(),
                pending_exit: Some(SessionExit::Failed(message)),
                exit: None,
            },
        }
    }

    /// The context-budget-exceeded path: no HTTP request is ever sent. A
    /// synthesized `Init` is still emitted (mirrors every other backend
    /// always producing one) before the session ends `Failed`.
    fn context_budget_exceeded(
        session_id: String,
        model: String,
        estimated_tokens: u32,
        context_budget: u32,
    ) -> Self {
        let mut queue = VecDeque::new();
        queue.push_back(AgentEvent::Init {
            session_id: session_id.clone(),
            model,
            raw: json!({}),
        });
        LocalSession {
            session_id,
            queue,
            pending_exit: Some(SessionExit::Failed(format!(
                "prompt estimated at {estimated_tokens} tokens exceeds context budget of \
                 {context_budget} tokens; no request was sent"
            ))),
            exit: None,
        }
    }
}

#[async_trait::async_trait]
impl AgentSession for LocalSession {
    fn session_id(&self) -> String {
        self.session_id.clone()
    }

    async fn next_event(&mut self) -> Result<Option<AgentEvent>> {
        if let Some(event) = self.queue.pop_front() {
            return Ok(Some(event));
        }
        if self.exit.is_none() {
            self.exit = self.pending_exit.take();
        }
        Ok(None)
    }

    async fn send_user_message(&mut self, _text: &str) -> Result<()> {
        Err(EngineError::Backend(
            "local backend is single-shot only; send_user_message is unsupported".to_string(),
        ))
    }

    async fn abort(&mut self) -> Result<()> {
        self.queue.clear();
        if self.exit.is_none() {
            self.exit = Some(SessionExit::Aborted);
        }
        self.pending_exit = None;
        Ok(())
    }

    fn exit_status(&self) -> Option<SessionExit> {
        self.exit.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    const TEST_MODEL: &str = "local-test-model";

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// Read one HTTP/1.1 request off `socket`: headers plus (if present) a
    /// `Content-Length`-sized body. Good enough for the small JSON requests
    /// this backend sends.
    async fn read_http_request(socket: &mut TcpStream) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let header_end = find_subslice(&buf, b"\r\n\r\n");
            if let Some(header_end) = header_end {
                let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let content_length: usize = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        if name.eq_ignore_ascii_case("content-length") {
                            value.trim().parse().ok()
                        } else {
                            None
                        }
                    })
                    .unwrap_or(0);
                let body_start = header_end + 4;
                if buf.len() >= body_start + content_length {
                    break;
                }
            }
            match socket.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => break,
            }
        }
        buf
    }

    /// Spawn an in-process stub `/v1/chat/completions` server that always
    /// replies with `status_line`/`body`, and hands back its base URL plus a
    /// counter of requests actually received (so the context-budget test can
    /// assert zero HTTP traffic).
    async fn spawn_stub(status_line: &'static str, body: String) -> (String, Arc<AtomicUsize>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind stub");
        let addr: SocketAddr = listener.local_addr().expect("stub addr");
        let count = Arc::new(AtomicUsize::new(0));
        let count_for_task = Arc::clone(&count);
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => break,
                };
                count_for_task.fetch_add(1, Ordering::SeqCst);
                let _ = read_http_request(&mut socket).await;
                let response = format!(
                    "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        (format!("http://{addr}"), count)
    }

    fn base_spec(session_id: &str, prompt: &str, context_budget_prompt: bool) -> SessionSpec {
        let _ = context_budget_prompt;
        SessionSpec {
            cwd: PathBuf::from("."),
            prompt: PromptMode::SingleShot(prompt.to_string()),
            append_system_prompt: Some("be terse".to_string()),
            model: TEST_MODEL.to_string(),
            effort: String::new(),
            session_id: session_id.to_string(),
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
            env: HashMap::new(),
            sandbox: None,
        }
    }

    async fn drain(session: &mut dyn AgentSession) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Some(event) = session.next_event().await.expect("next_event") {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn local_http_roundtrip_yields_init_text_result_with_usage_and_zero_cost() {
        let stub_body = json!({
            "id": "chatcmpl-1",
            "choices": [{"message": {"role": "assistant", "content": "hello from stub"}}],
            "usage": {"prompt_tokens": 12, "completion_tokens": 34, "total_tokens": 46}
        })
        .to_string();
        let (base_url, requests) = spawn_stub("HTTP/1.1 200 OK", stub_body).await;

        let backend = LocalBackend::new(base_url, Some(0.2), 100_000);
        let spec = base_spec("sess-1", "do the thing", false);
        let mut session = backend.start(spec).await.expect("start");

        let events = drain(session.as_mut()).await;
        assert_eq!(requests.load(Ordering::SeqCst), 1);

        assert!(
            matches!(&events[0], AgentEvent::Init { session_id, model, .. }
                if session_id == "sess-1" && model == TEST_MODEL)
        );
        assert!(matches!(&events[1], AgentEvent::Text { text, .. } if text == "hello from stub"));
        match &events[2] {
            AgentEvent::Result {
                text,
                is_error,
                usage,
                cost_usd,
                num_turns,
                ..
            } => {
                assert_eq!(text, "hello from stub");
                assert!(!is_error);
                assert_eq!(usage.input, 12);
                assert_eq!(usage.output, 34);
                assert_eq!(*cost_usd, Some(0.0));
                assert_eq!(*num_turns, Some(1));
            }
            other => panic!("expected terminal Result, got {other:?}"),
        }
        assert_eq!(events.len(), 3);
        assert_eq!(session.exit_status(), Some(SessionExit::Completed));
    }

    #[tokio::test]
    async fn local_http_rejects_resumed_spec() {
        let backend = LocalBackend::new("http://127.0.0.1:1".to_string(), None, 100_000);
        let mut spec = base_spec("sess-1", "do the thing", false);
        spec.resume = Some("sess-0".to_string());

        let result = backend.start(spec).await;
        assert!(result.is_err(), "expected resume to be rejected");
    }

    #[tokio::test]
    async fn local_http_send_user_message_errors() {
        let stub_body = json!({
            "choices": [{"message": {"role": "assistant", "content": "hi"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        })
        .to_string();
        let (base_url, _requests) = spawn_stub("HTTP/1.1 200 OK", stub_body).await;

        let backend = LocalBackend::new(base_url, None, 100_000);
        let spec = base_spec("sess-1", "do the thing", false);
        let mut session = backend.start(spec).await.expect("start");

        let result = session.send_user_message("nope").await;
        assert!(result.is_err(), "expected send_user_message to be rejected");
    }

    #[tokio::test]
    async fn local_http_500_fails_cleanly() {
        let (base_url, requests) =
            spawn_stub("HTTP/1.1 500 Internal Server Error", "boom".to_string()).await;

        let backend = LocalBackend::new(base_url, None, 100_000);
        let spec = base_spec("sess-1", "do the thing", false);
        let mut session = backend.start(spec).await.expect("start");

        let events = drain(session.as_mut()).await;
        assert!(events.is_empty(), "expected no events on a failed response");
        assert_eq!(requests.load(Ordering::SeqCst), 1);

        match session.exit_status() {
            Some(SessionExit::Failed(message)) => {
                assert!(
                    message.contains("500"),
                    "expected the failure message to include the HTTP status, got: {message}"
                );
            }
            other => panic!("expected SessionExit::Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn local_http_context_budget_exceeds_fails_cleanly() {
        let (base_url, requests) = spawn_stub("HTTP/1.1 200 OK", "{}".to_string()).await;

        // context_budget of 1 token; any real prompt blows past it.
        let backend = LocalBackend::new(base_url, None, 1);
        let spec = base_spec(
            "sess-1",
            "this prompt is far too long for a one-token budget",
            false,
        );
        let mut session = backend.start(spec).await.expect("start");

        let events = drain(session.as_mut()).await;
        assert_eq!(
            requests.load(Ordering::SeqCst),
            0,
            "must not send an HTTP request when the context budget is exceeded"
        );
        assert_eq!(events.len(), 1, "expected only a synthesized Init event");
        assert!(matches!(&events[0], AgentEvent::Init { .. }));

        match session.exit_status() {
            Some(SessionExit::Failed(message)) => {
                assert!(
                    message.contains("context budget"),
                    "expected the failure message to name the context budget, got: {message}"
                );
            }
            other => panic!("expected SessionExit::Failed, got {other:?}"),
        }
    }
}
