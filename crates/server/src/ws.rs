//! WebSocket live view (docs/protocol.md "WebSocket" section).
//!
//! On connect: one `snapshot` frame (current fold + its seq) — unless a
//! `?since=` reconnect gap of ≤ 5000 events lets us replay `event` frames
//! from `since+1` instead (client keeps its state). Then a 250 ms poll tail
//! over `events.jsonl`, pushing `event` frames in seq order plus a `state`
//! frame after every non-`worker.message` (lifecycle) event. Client frames
//! are ignored except `{"type":"ping"}` → `{"type":"pong"}`.
//!
//! The fold for `state` frames is incremental: the session task holds the
//! [`MissionState`] and `reducer::apply`s each new event, falling back to a
//! full re-fold on error — cheap, and stays consistent with the log.

use crate::error::ApiError;
use crate::rest::mission_paths;
use crate::ServerState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use kranz_engine::event_log::EventLog;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::paths::MissionPaths;
use kranz_engine::reducer;
use kranz_engine::types::MissionState;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Largest `?since=` gap replayed as `event` frames; a larger gap (or `since`
/// ahead of head) gets a fresh snapshot instead.
const MAX_REPLAY_GAP: u64 = 5000;

/// Live-tail poll interval over events.jsonl.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// `GET /api/missions/:id/ws?since=<seq>`
pub(crate) async fn ws_handler(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    let origin = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok());
    if !crate::ws_origin_allowed(origin, server.bind_addr, server.bind_is_loopback) {
        return StatusCode::FORBIDDEN.into_response();
    }

    let paths = match mission_paths(&server, &id) {
        Ok(paths) => paths,
        Err(e) => return e.into_response(),
    };
    if !paths.events_file().is_file() {
        return ApiError::not_found(format!("unknown mission '{id}'")).into_response();
    }
    // Align with REST `GET .../events?since=`: reject unparsable values
    // instead of silently falling back to a full snapshot.
    let since = match params.get("since") {
        None => None,
        Some(raw) => match raw.parse::<u64>() {
            Ok(n) => Some(n),
            Err(_) => {
                return ApiError::bad_request(format!("invalid 'since' value: '{raw}'"))
                    .into_response();
            }
        },
    };
    ws.on_upgrade(move |socket| session(socket, paths, since))
}

async fn session(mut socket: WebSocket, paths: MissionPaths, since: Option<u64>) {
    let events_path = paths.events_file();

    // Initial fold. On a corrupt/unreadable log: close cleanly, never panic.
    let Ok(events) = EventLog::read_events(&events_path) else {
        close(&mut socket).await;
        return;
    };
    let Ok(mut state) = reducer::fold(&events) else {
        close(&mut socket).await;
        return;
    };
    let head = state.last_seq;

    match since {
        // Reconnect with a small gap: replay `event` frames from since+1 and
        // send no snapshot — the client keeps its state.
        Some(s) if s <= head && head - s <= MAX_REPLAY_GAP => {
            for event in events.iter().filter(|e| e.seq > s) {
                if send_json(&mut socket, &event_frame(event)).await.is_err() {
                    return;
                }
            }
        }
        // No `since`, gap too large, or `since` ahead of head: fresh snapshot.
        _ => {
            let frame = json!({ "type": "snapshot", "seq": head, "state": &state });
            if send_json(&mut socket, &frame).await.is_err() {
                return;
            }
        }
    }

    let mut last_seq = head;
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let new_events = match EventLog::read_events_after(&events_path, last_seq) {
                    Ok(events) => events,
                    // Mission dir disappeared or the log became unreadable.
                    Err(_) => {
                        close(&mut socket).await;
                        return;
                    }
                };
                for event in &new_events {
                    if send_json(&mut socket, &event_frame(event)).await.is_err() {
                        return;
                    }
                    // Incremental fold; full re-fold (up to this seq) on error.
                    if reducer::apply(&mut state, event).is_err() {
                        match refold_at(&events_path, event.seq) {
                            Ok(rebuilt) => state = rebuilt,
                            Err(_) => {
                                close(&mut socket).await;
                                return;
                            }
                        }
                    }
                    last_seq = event.seq;
                    if !matches!(event.kind, EventKind::WorkerMessage { .. }) {
                        let frame =
                            json!({ "type": "state", "seq": state.last_seq, "state": &state });
                        if send_json(&mut socket, &frame).await.is_err() {
                            return;
                        }
                    }
                }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if is_ping(&text)
                            && send_json(&mut socket, &json!({ "type": "pong" })).await.is_err()
                        {
                            return;
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                    Some(Ok(_)) => {} // binary / protocol ping-pong: ignored
                }
            }
        }
    }
}

fn event_frame(event: &Event) -> Value {
    json!({ "type": "event", "seq": event.seq, "event": event })
}

fn is_ping(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| v.get("type").map(|t| t == "ping"))
        .unwrap_or(false)
}

/// Rebuild state from the log prefix ending at `seq` (the log is validated
/// contiguous from 1, so that prefix is `events[..seq]`).
fn refold_at(events_path: &Path, seq: u64) -> kranz_engine::error::Result<MissionState> {
    let events = EventLog::read_events(events_path)?;
    let upto = usize::try_from(seq).unwrap_or(usize::MAX).min(events.len());
    reducer::fold(&events[..upto])
}

async fn send_json(socket: &mut WebSocket, value: &Value) -> Result<(), axum::Error> {
    socket.send(Message::Text(value.to_string().into())).await
}

/// Close cleanly: best-effort Close frame, then drop the socket.
async fn close(socket: &mut WebSocket) {
    let _ = socket.send(Message::Close(None)).await;
}
