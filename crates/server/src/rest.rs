//! REST handlers (docs/protocol.md "REST" table). Every handler re-reads
//! from disk — no caching; the engine process owns truth.

use crate::error::ApiError;
use crate::ServerState;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use kranz_engine::contract_lint::ContractLintReport;
use kranz_engine::cost;
use kranz_engine::event_log::EventLog;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::merged::merged_bit;
use kranz_engine::orchestrator::mission_worktree_path;
use kranz_engine::paths::MissionPaths;
use kranz_engine::preflight::PREFLIGHT_CLEAR_SUMMARY;
use kranz_engine::reducer;
use kranz_engine::report_render::render_plan_markdown;
use kranz_engine::types::{
    ControlCommand, MissionState, MissionStatus, RoleConfig, SandboxEnforce, WorkerIsolation,
};
use kranz_engine::{config, control};
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
    for id in kranz_engine::mission_catalog::mission_index_ids(&index_contents) {
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
        // The events file exists — but trust it only when no path component
        // is a symlink (P1 mission-path-no-follow): a symlinked mission dir
        // surfaces as a clear error row, never a read into another
        // repository's tree.
        if let Err(error) = paths.require_no_follow() {
            rows.push(json!({ "id": id, "status": "failed", "error": error.to_string() }));
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

/// `GET /api/missions/outcomes` — flight-surgeon outcomes fold (autonomy
/// ratio, grant-latency distribution, escalation ledger), computed
/// per-request from the event logs by [`kranz_engine::outcomes::compute_outcomes`].
/// No caching, no second source of truth.
pub(crate) async fn mission_outcomes(
    State(server): State<Arc<ServerState>>,
) -> Result<Json<kranz_engine::outcomes::Outcomes>, ApiError> {
    let outcomes = kranz_engine::outcomes::compute_outcomes(&server.repo_root)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(outcomes))
}

/// `GET /api/escalation-metrics` — the flight-surgeon console (ticket
/// `flight-surgeon-dashboard`): autonomy ratio split by outcome, the
/// rubber-stamp signal (park→grant p50/p90 + sub-10s count), false greens
/// (completed missions joined against `traced-from-mission` defect tickets),
/// and the escalation ledger. Computed per-request from the event logs and
/// ticket frontmatter by
/// [`kranz_engine::escalation_metrics::compute_escalation_metrics`]. Read-gated
/// like every other GET; no caching, no second source of truth.
pub(crate) async fn escalation_metrics(
    State(server): State<Arc<ServerState>>,
) -> Result<Json<kranz_engine::escalation_metrics::EscalationMetrics>, ApiError> {
    let metrics = kranz_engine::escalation_metrics::compute_escalation_metrics(&server.repo_root)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(metrics))
}

/// `GET /api/cost-per-merged-change?windowDays=30` — cost per merged change
/// for the served repo (ticket `cost-per-merged-change`, KRZ-329): the cost
/// fold over missions closed in the window beside the merged-change count
/// derived at fold time (merged.rs's landed/ancestry probe — never stored)
/// and the window's autonomy ratio. `windowDays` defaults to
/// [`kranz_engine::outcomes::DEFAULT_MERGED_CHANGE_WINDOW_DAYS`]; the same
/// folded JSON the CLI's `kranz outcomes --all` aggregates per catalog repo.
pub(crate) async fn cost_per_merged_change(
    State(server): State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<kranz_engine::outcomes::CostPerMergedChange>, ApiError> {
    let window_days = match params.get("windowDays") {
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|_| ApiError::bad_request("windowDays must be a non-negative integer"))?,
        None => kranz_engine::outcomes::DEFAULT_MERGED_CHANGE_WINDOW_DAYS,
    };
    let report = kranz_engine::outcomes::compute_cost_per_merged_change(
        &server.repo_root,
        window_days,
        chrono::Utc::now(),
    )
    .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(report))
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

/// `GET /api/missions/:id/workspace` — effective local execution workspace,
/// sandbox tiers, and the latest already-recorded environment-preflight
/// outcome. This is a derived read model: no new durable state or events.
///
/// Additive workspace-provider fields (design D-B/D-E, ticket
/// workspace-provider-pin-at-approval):
/// - `pin` — the provider identity pinned at plan approval, mirroring
///   `MissionState.workspace_pin` (`{provider, template, version}`); `null`
///   on missions approved before pinning existed.
/// - `previews` — the contract's preview URL templates `[{name,
///   urlTemplate}]`, placeholders UNFILLED, and only once a readiness pass is
///   on the log (never a fabricated URL for unproven services); `null`
///   otherwise.
/// - `takeover` — how a human takes over the workspace. For local-worktree
///   this is the plain truth (work locally in the workspace cwd) — no SSH/
///   remote fiction; for `remote` it is the substrate-reported takeover URL
///   (SSH/web) from the latest `workspace.provisioned` event; `null` when no
///   pin names the provider or a remote mission has not provisioned yet.
/// - `workspaceLifecycle` — the last known PROVIDER lifecycle transition
///   (ticket `workspace-idle-hibernate`), folded from `workspace.teardown`
///   events carrying a `state`: `{state: "kept"|"stopped"|"destroyed"|
///   "failed", ts}` with the transition event's own timestamp — the
///   workspace-hours anchor for cost tooling. `null` on logs without a
///   state-carrying teardown (consumers must degrade on null). DISTINCT
///   from the top-level `lifecycle`, which describes the local execution
///   cwd (worktree active/pending/removed), not the provider workspace.
pub(crate) async fn mission_workspace(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let paths = mission_paths(&server, &id)?;
    if !paths.events_file().is_file() {
        return Err(unknown_mission(&id));
    }
    let events = EventLog::read_events(&paths.events_file())?;
    let state = reducer::fold(&events)?;
    let isolation = state.config.isolation();
    let cwd = match isolation {
        WorkerIsolation::Worktree => mission_worktree_path(&server.repo_root, &id),
        WorkerIsolation::Checkout => server.repo_root.clone(),
    };
    let worktree_active = isolation == WorkerIsolation::Worktree && cwd.is_dir();
    let lifecycle = match isolation {
        WorkerIsolation::Checkout => "primary-checkout",
        WorkerIsolation::Worktree if worktree_active => "active",
        WorkerIsolation::Worktree
            if matches!(
                state.mission.status,
                MissionStatus::Planning | MissionStatus::Approved
            ) =>
        {
            "pending"
        }
        WorkerIsolation::Worktree => "removed",
    };
    let preflight = events.iter().rev().find_map(|event| match &event.kind {
        EventKind::OrchestratorDecision { summary, .. } if summary.starts_with("preflight:") => {
            Some(json!({
                "status": if summary == PREFLIGHT_CLEAR_SUMMARY { "clear" } else { "issues" },
                "summary": summary,
                "eventSeq": event.seq,
            }))
        }
        _ => None,
    });
    let preflight = preflight.unwrap_or_else(|| {
        let pending = matches!(
            state.mission.status,
            MissionStatus::Planning | MissionStatus::Approved
        );
        json!({
            "status": if pending { "pending" } else { "clear" },
            "summary": if pending {
                "environment preflight has not run yet"
            } else {
                "no advisory preflight issues recorded"
            },
            "eventSeq": Value::Null,
        })
    });

    // Workspace contract presence (D-H): base-branch-owned `.kranz/
    // workspace.json` read from the repo root — a derived read like the rest
    // of this projection; an unreadable/invalid contract degrades to absent
    // here (draft/approve is the fail-closed surface).
    let workspace_contract =
        kranz_engine::workspace_contract::load_workspace_contract(&server.repo_root)
            .ok()
            .flatten();

    // Bootstrap/readiness gate outcomes (D-C/D-H), derived from the gate's
    // orchestrator.decision events — ADDITIVE fields: null when there is no
    // contract or the gate has not run yet, and consumers must degrade on
    // null/absent. A later run's outcome supersedes an earlier one (same
    // latest-wins derivation as `preflight` above).
    let gate_outcome = |prefix: &str| {
        events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::OrchestratorDecision { summary, .. } if summary.starts_with(prefix) => {
                    Some(json!({
                        "summary": summary,
                        "eventSeq": event.seq,
                    }))
                }
                _ => None,
            })
            .unwrap_or(Value::Null)
    };
    let (bootstrap, readiness) = if workspace_contract.is_some() {
        (
            gate_outcome(kranz_engine::workspace_gate::BOOTSTRAP_SUMMARY_PREFIX),
            gate_outcome(kranz_engine::workspace_gate::READINESS_SUMMARY_PREFIX),
        )
    } else {
        (Value::Null, Value::Null)
    };

    // Approval-time provider pin (D-B), mirrored from folded state; null on
    // missions approved before pinning existed — consumers must degrade on
    // null, same as the gate outcome fields above.
    let pin = match &state.workspace_pin {
        Some(pin) => json!(pin),
        None => Value::Null,
    };

    // Preview placeholders (D-E): the contract's URL templates, UNFILLED —
    // surfaced only once a readiness PASS is on the log, never implying a
    // reachable URL while the services behind it are unproven. The REMOTE
    // kind instead surfaces the substrate-reported URLs recorded on
    // `workspace.provisioned` (name-matched, with the substrate's auth
    // report) — same readiness-pass gate, never a fabricated URL.
    let readiness_passed = events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::WorkspaceReadinessReport { outcome, .. } if outcome == "ready"
        )
    });
    // The latest remote-kind provisioned event (latest-wins, the same
    // derivation idiom as preflight/gate outcomes above).
    let remote_provision = events.iter().rev().find_map(|event| match &event.kind {
        EventKind::WorkspaceProvisioned {
            provider,
            takeover,
            previews,
            ..
        } if provider == "remote" => Some((takeover.clone(), previews.clone())),
        _ => None,
    });
    let remote_pinned = matches!(
        &state.workspace_pin,
        Some(pin) if pin.provider == "remote"
    );
    let previews = if remote_pinned {
        match (&remote_provision, readiness_passed) {
            (Some((_, Some(previews))), true) if !previews.is_empty() => json!(previews),
            _ => Value::Null,
        }
    } else {
        match (&workspace_contract, readiness_passed) {
            (Some(contract), true) if !contract.previews.is_empty() => {
                json!(contract
                    .previews
                    .iter()
                    .map(|p| json!({
                        "name": p.name,
                        "urlTemplate": p.url_template,
                    }))
                    .collect::<Vec<_>>())
            }
            _ => Value::Null,
        }
    };

    // Human takeover (D-E): for local-worktree the plain truth — work locally
    // in the workspace cwd, no SSH/remote fiction (the provider isolates
    // source only, D-H). For remote, the substrate's reported SSH/web URL —
    // honest and substrate-sourced — null until a remote provision lands.
    // Keyed to the pin; null when no pin names the provider (older missions)
    // or a future provider has no line yet.
    let takeover = match &state.workspace_pin {
        Some(pin) if pin.provider == "local-worktree" => json!(format!(
            "work locally in the workspace cwd ({})",
            cwd.to_string_lossy()
        )),
        Some(pin) if pin.provider == "remote" => match &remote_provision {
            Some((Some(takeover), _)) => json!(takeover),
            _ => Value::Null,
        },
        _ => Value::Null,
    };

    // The last known provider lifecycle transition (ticket
    // workspace-idle-hibernate), folded from `workspace.teardown` state —
    // additive: null when no teardown carried an outcome; consumers must
    // degrade on null, same as the fields above.
    let workspace_lifecycle = match &state.workspace_lifecycle {
        Some(lifecycle) => json!(lifecycle),
        None => Value::Null,
    };

    Ok(Json(json!({
        "isolation": isolation,
        "cwd": cwd.to_string_lossy(),
        "lifecycle": lifecycle,
        "worktreeActive": worktree_active,
        "sandboxes": [
            sandbox_summary("worker", &state.config.worker),
            sandbox_summary("scrutiny", &state.config.validator_scrutiny),
            sandbox_summary("functional", &state.config.validator_functional),
        ],
        "preflight": preflight,
        "pin": pin,
        "previews": previews,
        "takeover": takeover,
        "workspaceLifecycle": workspace_lifecycle,
        "contract": {
            "present": workspace_contract.is_some(),
            "services": workspace_contract.as_ref().map_or(0, |c| c.services.len()),
            "previews": workspace_contract.as_ref().map_or(0, |c| c.previews.len()),
            "bootstrap": bootstrap,
            "readiness": readiness,
        },
    })))
}

fn sandbox_summary(role: &str, config: &RoleConfig) -> Value {
    json!({
        "role": role,
        "enforce": sandbox_enforce_label(config.sandbox.enforce),
        "extraWriteCount": config.sandbox.extra_write.len(),
        "egressCount": config.sandbox.egress.len(),
    })
}

fn sandbox_enforce_label(enforce: SandboxEnforce) -> &'static str {
    match enforce {
        SandboxEnforce::Off => "off",
        SandboxEnforce::Fs => "fs",
        SandboxEnforce::FsNet => "fs+net",
    }
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
    // Preview the same calibrated estimate approval will commit, so the
    // dashboard's revision diff shows the range that actually lands (M1).
    let calibration = cost::calibrate(&paths.repo_root);
    let estimate = cost::apply_shape(
        cost::estimate(&pending.plan, &state.config, &calibration.params),
        &pending.plan,
        &calibration,
    );
    // Preview only: this endpoint does not re-lint the contract against the
    // base (mid-mission, the base tree is no longer necessarily pristine).
    let no_lint = ContractLintReport {
        results: Vec::new(),
        tree_clean_at_base: true,
    };
    let revised = render_plan_markdown(
        &pending.plan,
        &state.mission,
        &estimate,
        kranz_engine::cost::estimate_two_path(estimate, &state.config, &calibration.params)
            .as_ref(),
        None,
        calibration.missions_used,
        &no_lint,
        &[],
    );
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

/// `GET /api/missions/:id/pr-handoff` — optional GitHub PR handoff for a
/// COMPLETE-but-unmerged mission. Never pushes; may return a copyable
/// `git push` or a `gh pr create` command.
pub(crate) async fn mission_pr_handoff(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let paths = mission_paths(&server, &id)?;
    if !paths.events_file().is_file() {
        return Err(unknown_mission(&id));
    }
    let handoff = kranz_engine::pr_handoff::assess_mission(&server.repo_root, &id)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(
        serde_json::to_value(handoff).map_err(|e| ApiError::internal(e.to_string()))?,
    ))
}

/// `POST /api/missions/:id/pr-handoff/create` — run `gh pr create` only when
/// the handoff is `readyToCreate` (remote branch already present). Never pushes.
pub(crate) async fn mission_pr_create(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let paths = mission_paths(&server, &id)?;
    if !paths.events_file().is_file() {
        return Err(unknown_mission(&id));
    }
    let handoff = kranz_engine::pr_handoff::assess_mission(&server.repo_root, &id)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    let url = kranz_engine::pr_handoff::create_pull_request(&server.repo_root, &handoff)
        .map_err(|e| ApiError::conflict(e.to_string()))?;
    Ok(Json(json!({ "url": url })))
}

/// `GET /api/missions/:id/readiness` — backend readiness probe (same enum as
/// pre-drain gating). Tokenless read.
pub(crate) async fn mission_readiness(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let _ = mission_paths(&server, &id)?;
    let report = kranz_engine::backend_readiness::probe_mission(&server.repo_root, &id)
        .map_err(|e| ApiError::internal(e.to_string()))?;
    Ok(Json(
        serde_json::to_value(report).map_err(|e| ApiError::internal(e.to_string()))?,
    ))
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
/// 400 on a body that is not a valid command; 409 when the mission is
/// terminal (mirrors [`control::resolve_active_mission`]).
pub(crate) async fn post_control(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Result<impl IntoResponse, ApiError> {
    let paths = mission_paths(&server, &id)?;
    if !paths.mission_dir().is_dir() {
        return Err(unknown_mission(&id));
    }
    if paths.events_file().is_file() {
        if let Some(status) =
            terminal_status_from_tail(&paths).map_err(|e| ApiError::internal(e.to_string()))?
        {
            return Err(ApiError::conflict(format!(
                "mission '{id}' is {status:?}; control commands apply only to active missions"
            )));
        }
    }
    let command: ControlCommand = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("invalid ControlCommand body: {e}")))?;
    if let ControlCommand::ConfigChange { patch } = &command {
        let events = EventLog::read_events(&paths.events_file())?;
        let state = reducer::fold(&events)?;
        config::apply_validated_patch(&state.config, patch)?;
    }
    control::enqueue(&paths, &command)?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "queued": true }))))
}

/// O(tail) terminal probe for the control route — the hottest write path
/// only needs "is the mission over?", not a full-log fold. Sound because a
/// terminal lifecycle event always sits in the trailing window: the engine
/// stops appending after emitting it (the report and any lesson land BEFORE
/// `mission.completed`; only the redaction audits attached to the same
/// append can follow `mission.abandoned`), and every mutation surface
/// refuses terminal missions.
fn terminal_status_from_tail(
    paths: &MissionPaths,
) -> kranz_engine::error::Result<Option<MissionStatus>> {
    const TAIL_WINDOW_BYTES: u64 = 64 * 1024;
    let events = EventLog::read_tail_events(&paths.events_file(), TAIL_WINDOW_BYTES)?;
    Ok(events.iter().rev().find_map(|e| match e.kind {
        EventKind::MissionCompleted {} => Some(MissionStatus::Complete),
        EventKind::MissionFailed { .. } => Some(MissionStatus::Failed),
        EventKind::MissionAbandoned { .. } => Some(MissionStatus::Abandoned),
        _ => None,
    }))
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GrantApproveBody {
    command: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GrantDenyBody {
    command: String,
    #[serde(default = "default_grant_deny_reason")]
    reason: String,
}

fn default_grant_deny_reason() -> String {
    "denied by operator".to_string()
}

/// `POST /api/missions/:id/grant/approve` — approve the parked grant request.
pub(crate) async fn post_grant_approve(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<GrantApproveBody>,
) -> Result<impl IntoResponse, ApiError> {
    let paths = require_pending_grant(&server, &id, &body.command)?;
    control::enqueue(
        &paths,
        &ControlCommand::ApproveGrant {
            command: body.command,
        },
    )?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "queued": true }))))
}

/// `POST /api/missions/:id/grant/deny` — deny the parked grant request.
pub(crate) async fn post_grant_deny(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<GrantDenyBody>,
) -> Result<impl IntoResponse, ApiError> {
    let paths = require_pending_grant(&server, &id, &body.command)?;
    control::enqueue(
        &paths,
        &ControlCommand::DenyGrant {
            command: body.command,
            reason: body.reason,
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
    let paths = MissionPaths::new(&server.repo_root, id);
    // A symlinked component in the mission path is refused (P1
    // mission-path-no-follow), never followed into another repository's
    // tree. An absent component is NOT a refusal here — handlers keep their
    // own unknown-mission behavior for missions that do not exist.
    if paths.require_no_follow().is_err() {
        return Err(unknown_mission(id));
    }
    Ok(paths)
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
    if kranz_engine::mission_catalog::is_terminal_status(state.mission.status) {
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

/// Confirm a grant request for exactly `command` is parked, so the enqueued
/// approve/deny can't target a different (or absent) request than the operator
/// saw. Same active-mission preconditions as the revision gate.
fn require_pending_grant(
    server: &ServerState,
    id: &str,
    command: &str,
) -> Result<MissionPaths, ApiError> {
    let paths = require_revisable_mission(server, id)?;
    let state = fold_log(&paths).map_err(ApiError::internal)?;
    match state.pending_grant_request {
        Some(pending) if pending.command == command => Ok(paths),
        Some(pending) => Err(ApiError::conflict(format!(
            "mission '{id}' is awaiting a grant for `{}`, not `{command}`",
            pending.command
        ))),
        None => Err(ApiError::conflict(format!(
            "mission '{id}' has no pending grant request"
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

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use kranz_engine::event_log::{EventLog, LockForce};
    use kranz_engine::events::EventKind;
    use kranz_engine::paths::MissionPaths;
    use kranz_engine::types::{GrantKind, MissionConfig};
    use serde_json::Value;
    use std::time::Duration;
    use tempfile::TempDir;
    use tower::ServiceExt;

    async fn body_json(response: axum::response::Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn get(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn seed_mission(repo_root: &std::path::Path, id: &str, kinds: Vec<EventKind>) {
        let paths = MissionPaths::new(repo_root, id);
        let mut log = EventLog::acquire(&paths, id, Duration::ZERO, LockForce::No).unwrap();
        for kind in kinds {
            log.append(kind).unwrap();
        }
    }

    fn created(goal: &str) -> EventKind {
        EventKind::MissionCreated {
            goal: goal.into(),
            base_branch: "main".into(),
            mission_branch: "kranz/mission-x".into(),
            config: MissionConfig::default(),
        }
    }

    #[tokio::test]
    async fn outcomes_endpoint_empty_repo_returns_zeroed_defaults() {
        let tmp = TempDir::new().unwrap();
        let app = crate::router(tmp.path().to_path_buf(), None);

        let response = app.oneshot(get("/api/missions/outcomes")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;

        assert_eq!(body["autonomyRatio"]["closedMissions"], 0);
        let buckets = body["grantLatency"]["buckets"].as_array().unwrap();
        assert_eq!(buckets.len(), 4);
        assert!(buckets.iter().all(|b| b["count"] == 0));
        assert_eq!(body["escalations"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn outcomes_endpoint_route_is_not_swallowed_by_mission_id_routes() {
        let tmp = TempDir::new().unwrap();
        let app = crate::router(tmp.path().to_path_buf(), None);

        // If `outcomes` were captured as a mission id by `/missions/:id/state`
        // style routes, this would 404 as an unknown mission instead of
        // resolving to the outcomes handler.
        let response = app.oneshot(get("/api/missions/outcomes")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert!(body.get("autonomyRatio").is_some());
        assert!(body.get("error").is_none());
    }

    #[tokio::test]
    async fn outcomes_endpoint_seeded_repo_populates_buckets_and_escalations() {
        let tmp = TempDir::new().unwrap();
        seed_mission(
            tmp.path(),
            "m-1",
            vec![
                created("seeded"),
                EventKind::GrantRequested {
                    milestone_id: "ms-1".into(),
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
                EventKind::GrantApproved {
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
                EventKind::PlanRevisionProposed {
                    revision: 1,
                    plan: sample_plan(),
                    instructions: "add tests".into(),
                },
                EventKind::PlanRevised {
                    revision: 1,
                    plan: sample_plan(),
                },
                EventKind::MissionCompleted {},
            ],
        );

        let app = crate::router(tmp.path().to_path_buf(), None);
        let response = app.oneshot(get("/api/missions/outcomes")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;

        assert_eq!(body["autonomyRatio"]["closedMissions"], 1);
        assert_eq!(body["autonomyRatio"]["totalInterventions"], 2);

        let buckets = body["grantLatency"]["buckets"].as_array().unwrap();
        assert_eq!(buckets.len(), 4);
        let total_bucketed: i64 = buckets.iter().map(|b| b["count"].as_i64().unwrap()).sum();
        assert_eq!(total_bucketed, 1);
        assert_eq!(body["grantLatency"]["totalDecided"], 1);

        let escalations = body["escalations"].as_array().unwrap();
        assert!(!escalations.is_empty());
        let grant_row = escalations
            .iter()
            .find(|e| e["kind"] == "grant")
            .expect("grant escalation row present");
        assert_eq!(grant_row["missionId"], "m-1");
        assert_eq!(grant_row["summary"], "cargo test");
        assert_eq!(grant_row["decision"], "approved");
        assert!(grant_row["latencyMs"].is_number());

        let revision_row = escalations
            .iter()
            .find(|e| e["kind"] == "revision")
            .expect("revision escalation row present");
        assert_eq!(revision_row["missionId"], "m-1");
        assert_eq!(revision_row["summary"], "add tests");
        assert_eq!(revision_row["decision"], "accepted (rev 1)");
    }

    #[tokio::test]
    async fn escalation_metrics_endpoint_empty_repo_returns_none_rates() {
        let tmp = TempDir::new().unwrap();
        let app = crate::router(tmp.path().to_path_buf(), None);

        let response = app.oneshot(get("/api/escalation-metrics")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;

        assert_eq!(body["autonomy"]["closedMissions"], 0);
        assert!(body["autonomy"]["zeroInterventionShare"].is_null());
        assert_eq!(body["rubberStamp"]["decidedGrants"], 0);
        assert!(body["rubberStamp"]["p50Ms"].is_null());
        assert_eq!(body["falseGreens"]["completedMissions"], 0);
        assert!(body["falseGreens"]["falseGreenRate"].is_null());
        assert_eq!(body["ledger"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn escalation_metrics_endpoint_seeded_repo_joins_traced_defects() {
        let tmp = TempDir::new().unwrap();
        seed_mission(
            tmp.path(),
            "m-1",
            vec![
                created("seeded"),
                EventKind::GrantRequested {
                    milestone_id: "ms-1".into(),
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
                EventKind::GrantApproved {
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
                EventKind::MissionCompleted {},
            ],
        );
        seed_mission(
            tmp.path(),
            "m-2",
            vec![created("clean"), EventKind::MissionCompleted {}],
        );
        // A hand-edited defect ticket traces back to m-1 (the grant-steered
        // completion); the clean m-2 stays out of the false-green count.
        let tickets = kranz_engine::ticket::Ticket::tickets_dir(tmp.path());
        std::fs::create_dir_all(&tickets).unwrap();
        std::fs::write(
            tickets.join("defect-regression.md"),
            "---\ntitle: Regression\ntraced-from-mission: m-1\n---\n\n## Goal\nfix\n",
        )
        .unwrap();

        let app = crate::router(tmp.path().to_path_buf(), None);
        let response = app.oneshot(get("/api/escalation-metrics")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;

        assert_eq!(body["autonomy"]["closedMissions"], 2);
        assert_eq!(body["autonomy"]["zeroInterventionMissions"], 1);
        assert_eq!(body["autonomy"]["zeroInterventionShare"], 0.5);
        assert_eq!(body["autonomy"]["completed"]["missions"], 2);
        assert_eq!(body["autonomy"]["completed"]["zeroIntervention"], 1);

        assert_eq!(body["rubberStamp"]["decidedGrants"], 1);
        assert_eq!(body["rubberStamp"]["underTenSeconds"], 1);
        assert!(body["rubberStamp"]["p50Ms"].is_number());

        assert_eq!(body["falseGreens"]["completedMissions"], 2);
        assert_eq!(body["falseGreens"]["falseGreens"], 1);
        assert_eq!(body["falseGreens"]["falseGreenRate"], 0.5);
        assert_eq!(body["falseGreens"]["withInterventions"]["falseGreens"], 1);
        assert_eq!(body["falseGreens"]["zeroIntervention"]["falseGreens"], 0);
        assert_eq!(
            body["falseGreens"]["tracedDefects"][0]["ticket"],
            "defect-regression"
        );
        assert_eq!(body["falseGreens"]["tracedDefects"][0]["missionId"], "m-1");

        let ledger = body["ledger"].as_array().unwrap();
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0]["kind"], "grant");
        assert_eq!(ledger[0]["missionId"], "m-1");
        assert_eq!(ledger[0]["milestoneId"], "ms-1");
        assert_eq!(ledger[0]["ask"], "command: cargo test");
        assert_eq!(ledger[0]["decision"], "approved");
        assert!(ledger[0]["latencyMs"].is_number());
    }

    #[tokio::test]
    async fn outcomes_report_cost_per_merged_endpoint_defaults_and_validates_window() {
        let tmp = TempDir::new().unwrap();
        seed_mission(
            tmp.path(),
            "m-1",
            vec![created("seeded"), EventKind::MissionCompleted {}],
        );
        let app = crate::router(tmp.path().to_path_buf(), None);

        // Default window: 30 days; a repo that merged nothing reads absent,
        // never zero.
        let response = app
            .clone()
            .oneshot(get("/api/cost-per-merged-change"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["windowDays"], 30);
        assert_eq!(body["closedInWindow"], 1);
        assert_eq!(body["mergedChanges"], 0);
        assert!(body["usdPerMergedChange"].is_null());
        assert_eq!(body["zeroInterventionShare"], 1.0);

        // The window is a parameter.
        let response = app
            .clone()
            .oneshot(get("/api/cost-per-merged-change?windowDays=7"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["windowDays"], 7);

        // A malformed window is a 400, never a silent default.
        let response = app
            .oneshot(get("/api/cost-per-merged-change?windowDays=abc"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn outcomes_report_outcomes_endpoint_carries_the_new_fold_sections() {
        let tmp = TempDir::new().unwrap();
        seed_mission(
            tmp.path(),
            "m-1",
            vec![
                created("do the thing\n\n## Task class\nexecution-class\n"),
                EventKind::GrantRequested {
                    milestone_id: "ms-1".into(),
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
                EventKind::GrantApproved {
                    kind: GrantKind::Command,
                    command: "cargo test".into(),
                },
                EventKind::MissionCompleted {},
            ],
        );
        let app = crate::router(tmp.path().to_path_buf(), None);

        let response = app.oneshot(get("/api/missions/outcomes")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;

        // KRZ-321: the per-task-class row (same fold, class grouping).
        let classes = body["taskClasses"].as_array().unwrap();
        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0]["taskClass"], "execution-class");
        assert_eq!(classes[0]["missions"], 1);
        assert_eq!(classes[0]["advisorInvocations"], 1);
        // KRZ-323: the flag row plus the per-grant marker (seed_mission's
        // request→approval gap lands far under the 10s default threshold).
        assert_eq!(body["rubberStamp"]["thresholdMs"], 10_000);
        assert_eq!(body["rubberStamp"]["flagged"], 1);
        assert_eq!(body["rubberStamp"]["approvedDecisions"], 1);
        let grant = body["escalations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "grant")
            .unwrap()
            .clone();
        assert_eq!(grant["rubberStamp"], true);
    }

    /// `workspaceLifecycle` (ticket workspace-idle-hibernate): present with
    /// the folded transition state + its event ts when a teardown carried an
    /// outcome, null when none did (v1 keep-only logs) — consumers degrade
    /// on null.
    #[tokio::test]
    async fn workspace_endpoint_surfaces_workspace_lifecycle_present_and_absent() {
        let tmp = TempDir::new().unwrap();
        seed_mission(
            tmp.path(),
            "m-1",
            vec![
                created("lifecycle"),
                EventKind::WorkspaceTeardown {
                    mode: "hibernate".into(),
                    state: Some("stopped".into()),
                },
            ],
        );
        let app = crate::router(tmp.path().to_path_buf(), None);
        let response = app
            .oneshot(get("/api/missions/m-1/workspace"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["workspaceLifecycle"]["state"], "stopped");
        assert!(
            body["workspaceLifecycle"]["ts"].as_str().is_some(),
            "the transition ts rides the field (the workspace-hours anchor): {body}"
        );

        seed_mission(
            tmp.path(),
            "m-2",
            vec![
                created("no-lifecycle"),
                EventKind::WorkspaceTeardown {
                    mode: "keep".into(),
                    state: None,
                },
            ],
        );
        let app = crate::router(tmp.path().to_path_buf(), None);
        let response = app
            .oneshot(get("/api/missions/m-2/workspace"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert!(
            body["workspaceLifecycle"].is_null(),
            "null when no teardown carried an outcome: {body}"
        );
    }

    fn sample_plan() -> kranz_engine::types::Plan {
        kranz_engine::types::Plan {
            goal: "g".into(),
            validation_contract: vec![],
            milestones: vec![],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
        }
    }

    /// P1 mission-path-no-follow: a catalog line whose id traverses out of
    /// the missions dir is rejected before any filesystem access — it never
    /// becomes a row (and never resolves a path).
    #[tokio::test]
    async fn list_missions_rejects_catalog_ids_with_traversal() {
        let tmp = TempDir::new().unwrap();
        let missions_dir = tmp.path().join(".kranz").join("missions");
        std::fs::create_dir_all(&missions_dir).unwrap();
        std::fs::write(
            missions_dir.join("index.md"),
            "# Kranz missions\n\n\
             - 2026-07-28 · [../../../tmp/evil](../../../tmp/evil/plan.md) — traversal\n\
             - 2026-07-28 · [m-ghost](m-ghost/plan.md) — ghost\n",
        )
        .unwrap();
        let app = crate::router(tmp.path().to_path_buf(), None);
        let response = app.oneshot(get("/api/missions")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let ids: Vec<&str> = body
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["id"].as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["m-ghost"],
            "traversal catalog id rejected, legit ghost kept: {ids:?}"
        );
    }

    // Symlink-creating tests are unix-only, exactly like the lessons guard's
    // tests in the engine; Windows needs privileges to create symlinks.

    /// P1 mission-path-no-follow: a symlinked `.kranz/missions/<id>` is a
    /// clear error row on enumerate — never a read into the target's tree.
    #[cfg(unix)]
    #[tokio::test]
    async fn list_missions_surfaces_a_symlinked_mission_dir_as_an_error_row() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        // The symlink target is another tree's real mission, with data that
        // must not leak into this repo's listing.
        let elsewhere = TempDir::new().unwrap();
        seed_mission(elsewhere.path(), "m-evil", vec![created("other repo goal")]);
        let missions_dir = tmp.path().join(".kranz").join("missions");
        std::fs::create_dir_all(&missions_dir).unwrap();
        symlink(
            elsewhere
                .path()
                .join(".kranz")
                .join("missions")
                .join("m-evil"),
            missions_dir.join("m-evil"),
        )
        .unwrap();
        // A catalog line keeps the symlinked id in the union (the on-disk
        // scan already excludes it as not-a-real-dir).
        std::fs::write(
            missions_dir.join("index.md"),
            "# Kranz missions\n\n- 2026-07-28 · [m-evil](m-evil/plan.md) — evil\n",
        )
        .unwrap();
        let app = crate::router(tmp.path().to_path_buf(), None);
        let response = app.oneshot(get("/api/missions")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let row = body
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == "m-evil")
            .expect("the symlinked mission surfaces as an error row");
        assert_eq!(row["status"], "failed", "{row}");
        assert!(
            row["error"].as_str().unwrap().contains("refusing"),
            "clear refusal: {row}"
        );
        assert!(
            row.get("goal").is_none(),
            "nothing read through the symlink: {row}"
        );
    }

    /// P1 mission-path-no-follow: direct mission-path resolution (every
    /// `/api/missions/:id/...` handler goes through `mission_paths`) refuses
    /// a symlinked mission dir as an unknown mission.
    #[cfg(unix)]
    #[tokio::test]
    async fn mission_state_refuses_a_symlinked_mission_dir() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let elsewhere = TempDir::new().unwrap();
        seed_mission(elsewhere.path(), "m-evil", vec![created("other repo goal")]);
        let missions_dir = tmp.path().join(".kranz").join("missions");
        std::fs::create_dir_all(&missions_dir).unwrap();
        symlink(
            elsewhere
                .path()
                .join(".kranz")
                .join("missions")
                .join("m-evil"),
            missions_dir.join("m-evil"),
        )
        .unwrap();
        let app = crate::router(tmp.path().to_path_buf(), None);
        let response = app
            .oneshot(get("/api/missions/m-evil/state"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
