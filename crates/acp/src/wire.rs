use serde_json::{json, Value};

/// One stdout line classified by JSON-RPC shape. The wire is symmetric
/// (both sides issue requests), so a line is one of: a response to a client
/// request, a peer request we must answer, or a notification.
#[derive(Debug)]
pub enum Frame {
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
    /// Malformed protocol input: retained diagnostic, then session failure.
    Unrecognized(Value),
}

/// The payload of a JSON-RPC response: peer ids are always numbers in our
/// exchanges with the peer's client side, but the error path keeps the raw
/// object for the transcript.
#[derive(Debug)]
pub enum RpcOutcome {
    Result(Value),
    Error(Value),
}

/// Classify one stdout line. Unparseable lines become
/// [`Frame::Unrecognized`] with `raw = {"unparsed": <line>}` so nothing is
/// ever dropped from transcripts (mirrors `backend_codex::parse_codex_line`).
pub fn classify_line(line: &str) -> Frame {
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
