//! REST handlers (docs/protocol.md "REST" table). Every handler re-reads
//! from disk — no caching; the engine process owns truth.

use crate::error::ApiError;
use crate::ServerState;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use kranz_engine::control;
use kranz_engine::event_log::EventLog;
use kranz_engine::events::Event;
use kranz_engine::paths::MissionPaths;
use kranz_engine::reducer;
use kranz_engine::types::{ControlCommand, MissionState};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;

/// `GET /api/health` → `{"ok":true,"version":"<crate version>"}`.
pub(crate) async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

/// `GET /api/missions` — fold each mission's log into a summary row. A
/// corrupt (or unreadable) log yields that entry with `"status": "failed"`
/// and an `"error"` field instead of failing the whole list.
pub(crate) async fn list_missions(State(server): State<Arc<ServerState>>) -> Json<Value> {
    let mut rows = Vec::new();
    for id in MissionPaths::list_missions(&server.repo_root) {
        let paths = MissionPaths::new(&server.repo_root, &id);
        let row = match fold_log(&paths) {
            Ok(state) => json!({
                "id": id,
                "status": state.mission.status,
                "goal": state.mission.goal,
                "createdAt": state.mission.created_at,
            }),
            Err(error) => json!({ "id": id, "status": "failed", "error": error }),
        };
        rows.push(row);
    }
    Json(Value::Array(rows))
}

/// `GET /api/missions/:id/state` — full [`MissionState`], folded from
/// events.jsonl (NOT the state.json cache). 404 for an unknown mission.
pub(crate) async fn mission_state(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<MissionState>, ApiError> {
    let paths = mission_paths(&server, &id)?;
    let events_path = paths.events_file();
    if !events_path.is_file() {
        return Err(unknown_mission(&id));
    }
    let events = EventLog::read_events(&events_path)?;
    Ok(Json(reducer::fold(&events)?))
}

/// `GET /api/missions/:id/events?since=<seq>` — events with `seq > since`
/// (all events when `since` is omitted).
pub(crate) async fn mission_events(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<Event>>, ApiError> {
    let paths = mission_paths(&server, &id)?;
    let events_path = paths.events_file();
    if !events_path.is_file() {
        return Err(unknown_mission(&id));
    }
    let since = match params.get("since") {
        None => 0,
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|_| ApiError::bad_request(format!("invalid 'since' value: '{raw}'")))?,
    };
    Ok(Json(EventLog::read_events_after(&events_path, since)?))
}

/// `GET /api/missions/:id/plan` — contents of plan.json; 404 until the plan
/// has been approved (i.e. the file exists).
pub(crate) async fn mission_plan(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let paths = mission_paths(&server, &id)?;
    let content = read_file_or_404(&paths.plan_file(), || {
        format!("mission '{id}' has no approved plan yet")
    })?;
    let plan: Value = serde_json::from_str(&content)
        .map_err(|e| ApiError::internal(format!("plan.json is not valid JSON: {e}")))?;
    Ok(Json(plan))
}

/// `GET /api/missions/:id/runs/:runId/transcript` — the run's JSONL parsed
/// into a JSON array of raw stream values; 404 if the file is missing.
pub(crate) async fn run_transcript(
    State(server): State<Arc<ServerState>>,
    UrlPath((id, run_id)): UrlPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let paths = mission_paths(&server, &id)?;
    if !safe_id(&run_id) {
        return Err(ApiError::not_found(format!("unknown run '{run_id}'")));
    }
    let content = read_file_or_404(&paths.transcript_file(&run_id), || {
        format!("no transcript for run '{run_id}'")
    })?;
    // Tolerate torn/garbage lines (a live transcript may end mid-write).
    let values: Vec<Value> = content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    Ok(Json(Value::Array(values)))
}

/// `POST /api/missions/:id/control` — enqueue a [`ControlCommand`] into the
/// mission's control inbox (the engine drains it). `202 {"queued":true}`;
/// 400 on a body that is not a valid command.
pub(crate) async fn post_control(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let paths = mission_paths(&server, &id)?;
    if !paths.mission_dir().is_dir() {
        return Err(unknown_mission(&id));
    }
    let command: ControlCommand = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid ControlCommand body: {e}")))?;
    control::enqueue(&paths, &command)?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "queued": true }))))
}

// ---------------------------------------------------------------------------
// Shared helpers (also used by the WS handler)
// ---------------------------------------------------------------------------

/// Build [`MissionPaths`] for a URL-supplied mission id, rejecting anything
/// that could traverse outside the missions dir.
pub(crate) fn mission_paths(server: &ServerState, id: &str) -> Result<MissionPaths, ApiError> {
    if !safe_id(id) {
        return Err(unknown_mission(id));
    }
    Ok(MissionPaths::new(&server.repo_root, id))
}

/// Ids from the URL are joined into filesystem paths, so separators, `..`
/// and drive designators are rejected (axum percent-decodes path segments,
/// so `%2e%2e%2f` would otherwise sneak through).
fn safe_id(id: &str) -> bool {
    !id.is_empty() && !id.contains(['/', '\\', ':']) && !id.contains("..")
}

fn unknown_mission(id: &str) -> ApiError {
    ApiError::not_found(format!("unknown mission '{id}'"))
}

fn fold_log(paths: &MissionPaths) -> Result<MissionState, String> {
    let events = EventLog::read_events(&paths.events_file()).map_err(|e| e.to_string())?;
    reducer::fold(&events).map_err(|e| e.to_string())
}

fn read_file_or_404(
    path: &Path,
    not_found_msg: impl FnOnce() -> String,
) -> Result<String, ApiError> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(e) if e.kind() == ErrorKind::NotFound => Err(ApiError::not_found(not_found_msg())),
        Err(e) => Err(ApiError::internal(format!("failed to read {}: {e}", path.display()))),
    }
}
