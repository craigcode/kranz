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
use kranz_engine::cost;
use kranz_engine::event_log::EventLog;
use kranz_engine::events::Event;
use kranz_engine::merged::merged_bit;
use kranz_engine::orchestrator::render_plan_markdown;
use kranz_engine::paths::MissionPaths;
use kranz_engine::reducer;
use kranz_engine::types::{ControlCommand, MissionState, MissionStatus};
use serde::Deserialize;
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
    let index_contents = read_missions_index(&server.repo_root);
    let mut ids = MissionPaths::list_missions(&server.repo_root);
    for id in kranz_engine::orchestrator::mission_index_ids(&index_contents) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.sort();
    // Opened once for the whole list; a failure here just means every row's
    // `merged` degrades to null (no git ancestry to probe).
    let repo = kranz_engine::git_ops::GitRepo::open(&server.repo_root).ok();
    let mut rows = Vec::new();
    for id in ids {
        let paths = MissionPaths::new(&server.repo_root, &id);
        if !paths.events_file().is_file() {
            rows.push(json!({
                "id": id,
                "status": "deleted",
                "goal": "deleted mission (no data recorded)",
            }));
            continue;
        }
        let row = match fold_log(&paths) {
            Ok(state) => {
                let merged = repo
                    .as_ref()
                    .and_then(|repo| merged_bit(repo, &state.mission));
                json!({
                    "id": id,
                    "status": state.mission.status,
                    "goal": state.mission.goal,
                    "createdAt": state.mission.created_at,
                    "merged": merged,
                })
            }
            Err(error) => json!({ "id": id, "status": "failed", "error": error }),
        };
        rows.push(row);
    }
    Json(Value::Array(rows))
}

/// `<repo>/.kranz/missions/index.md` contents, or `""` if the file is absent
/// (never created here — callers only read the catalog).
fn read_missions_index(repo_root: &Path) -> String {
    std::fs::read_to_string(
        MissionPaths::new(repo_root, "_")
            .missions_dir()
            .join("index.md"),
    )
    .unwrap_or_default()
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

/// `GET /api/missions/:id/plan.md` — rendered plan markdown; 404 until
/// plan.md has been written (alongside plan.json, at approval time).
pub(crate) async fn mission_plan_md(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let paths = mission_paths(&server, &id)?;
    let markdown = read_file_or_404(&paths.plan_md_file(), || {
        format!("mission '{id}' has no approved plan yet")
    })?;
    Ok(Json(json!({ "markdown": markdown })))
}

/// `GET /api/missions/:id/revision-diff` — pending revision review artifact.
/// Returns 404 when no proposed revision is awaiting approval.
pub(crate) async fn mission_revision_diff(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let paths = mission_paths(&server, &id)?;
    if !paths.events_file().is_file() {
        return Err(unknown_mission(&id));
    }
    let state = fold_log(&paths).map_err(ApiError::internal)?;
    let Some(pending) = state.pending_revision.as_ref() else {
        return Err(ApiError::not_found(format!(
            "mission '{id}' has no pending revision"
        )));
    };
    let current = read_file_or_404(&paths.plan_md_file(), || {
        format!("mission '{id}' has no approved plan yet")
    })?;
    let estimate = cost::estimate(
        &pending.plan,
        &state.config,
        &cost::EstimateParams::default(),
    );
    let revised = render_plan_markdown(&pending.plan, &state.mission, &estimate, 0);
    Ok(Json(json!({
        "revision": pending.revision,
        "instructions": pending.instructions,
        "markdown": revised,
        "diff": simple_line_diff("plan.md", "revised-plan.md", &current, &revised),
    })))
}

/// `GET /api/missions/:id/report.md` — rendered mission report markdown;
/// 404 until the mission completes and report.md is written.
pub(crate) async fn mission_report_md(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let paths = mission_paths(&server, &id)?;
    let markdown = read_file_or_404(&paths.report_file(), || {
        format!("mission '{id}' has no report yet")
    })?;
    Ok(Json(json!({ "markdown": markdown })))
}

/// `GET /api/missions/:id/diff-stat` — `git diff --stat` of the pinned
/// `base_sha` against the mission branch tip; 404 until the plan is
/// approved (`base_sha` set) and the mission branch exists.
pub(crate) async fn mission_diff_stat(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let paths = mission_paths(&server, &id)?;
    if !paths.events_file().is_file() {
        return Err(unknown_mission(&id));
    }
    let state = fold_log(&paths).map_err(ApiError::internal)?;
    let Some(base_sha) = state.mission.base_sha else {
        return Err(ApiError::not_found(format!(
            "mission '{id}' has no pinned base yet"
        )));
    };
    let repo = kranz_engine::git_ops::GitRepo::open(&server.repo_root)?;
    if !repo.branch_exists(&state.mission.mission_branch)? {
        return Err(ApiError::not_found(format!(
            "mission '{id}' has no mission branch yet"
        )));
    }
    let tip = repo.rev_parse(&state.mission.mission_branch)?;
    let diff_stat = repo.diff_stat(&base_sha, &tip)?;
    Ok(Json(json!({
        "diffStat": diff_stat,
        "baseSha": base_sha,
        "tip": tip,
    })))
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReviseBody {
    instructions: String,
}

/// `POST /api/missions/:id/revise` — enqueue a mid-mission revision request.
pub(crate) async fn post_revise(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<ReviseBody>,
) -> Result<impl IntoResponse, ApiError> {
    let instructions = body.instructions.trim();
    if instructions.is_empty() {
        return Err(ApiError::bad_request(
            "revision instructions must not be empty",
        ));
    }
    let paths = require_revisable_mission(&server, &id)?;
    control::enqueue(
        &paths,
        &ControlCommand::RequestRevision {
            instructions: instructions.to_string(),
        },
    )?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "queued": true }))))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RevisionDecisionBody {
    revision: u32,
}

/// `POST /api/missions/:id/revision/approve` — approve a pending revision.
pub(crate) async fn post_revision_approve(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<RevisionDecisionBody>,
) -> Result<impl IntoResponse, ApiError> {
    let paths = require_pending_revision(&server, &id, body.revision)?;
    control::enqueue(
        &paths,
        &ControlCommand::ApproveRevision {
            revision: body.revision,
        },
    )?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "queued": true }))))
}

/// `POST /api/missions/:id/revision/reject` — reject a pending revision.
pub(crate) async fn post_revision_reject(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<RevisionDecisionBody>,
) -> Result<impl IntoResponse, ApiError> {
    let paths = require_pending_revision(&server, &id, body.revision)?;
    control::enqueue(
        &paths,
        &ControlCommand::RejectRevision {
            revision: body.revision,
        },
    )?;
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
    MissionPaths::is_safe_id(id)
}

fn unknown_mission(id: &str) -> ApiError {
    ApiError::not_found(format!("unknown mission '{id}'"))
}

fn fold_log(paths: &MissionPaths) -> Result<MissionState, String> {
    let events = EventLog::read_events(&paths.events_file()).map_err(|e| e.to_string())?;
    reducer::fold(&events).map_err(|e| e.to_string())
}

fn require_revisable_mission(server: &ServerState, id: &str) -> Result<MissionPaths, ApiError> {
    let paths = mission_paths(server, id)?;
    if !paths.events_file().is_file() {
        return Err(unknown_mission(id));
    }
    let state = fold_log(&paths).map_err(ApiError::internal)?;
    if state.mission.status == MissionStatus::Planning {
        return Err(ApiError::conflict(format!(
            "mission '{id}' has no approved plan to revise yet"
        )));
    }
    if kranz_engine::orchestrator::is_terminal_status(state.mission.status) {
        return Err(ApiError::conflict(format!(
            "mission '{id}' is {:?}; revision commands apply only to active missions",
            state.mission.status
        )));
    }
    Ok(paths)
}

fn require_pending_revision(
    server: &ServerState,
    id: &str,
    revision: u32,
) -> Result<MissionPaths, ApiError> {
    let paths = require_revisable_mission(server, id)?;
    let state = fold_log(&paths).map_err(ApiError::internal)?;
    match state.pending_revision {
        Some(pending) if pending.revision == revision => Ok(paths),
        Some(pending) => Err(ApiError::conflict(format!(
            "mission '{id}' is awaiting revision {}, not {revision}",
            pending.revision
        ))),
        None => Err(ApiError::conflict(format!(
            "mission '{id}' has no pending revision"
        ))),
    }
}

fn simple_line_diff(old_name: &str, new_name: &str, old: &str, new: &str) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut diff = format!("--- {old_name}\n+++ {new_name}\n");
    let max = old_lines.len().max(new_lines.len());
    for i in 0..max {
        match (old_lines.get(i), new_lines.get(i)) {
            (Some(a), Some(b)) if a == b => {
                diff.push(' ');
                diff.push_str(a);
                diff.push('\n');
            }
            (Some(a), Some(b)) => {
                diff.push('-');
                diff.push_str(a);
                diff.push('\n');
                diff.push('+');
                diff.push_str(b);
                diff.push('\n');
            }
            (Some(a), None) => {
                diff.push('-');
                diff.push_str(a);
                diff.push('\n');
            }
            (None, Some(b)) => {
                diff.push('+');
                diff.push_str(b);
                diff.push('\n');
            }
            (None, None) => {}
        }
    }
    diff
}

fn read_file_or_404(
    path: &Path,
    not_found_msg: impl FnOnce() -> String,
) -> Result<String, ApiError> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(e) if e.kind() == ErrorKind::NotFound => Err(ApiError::not_found(not_found_msg())),
        Err(e) => Err(ApiError::internal(format!(
            "failed to read {}: {e}",
            path.display()
        ))),
    }
}
