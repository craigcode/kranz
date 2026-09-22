//! REST handlers (docs/protocol.md "REST" table). Every handler re-reads
//! from disk — no caching; the engine process owns truth.

use crate::error::ApiError;
use crate::read_work::ReadWork;
use crate::ServerState;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{Extension, Json};
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
use serde::{Deserialize, Serialize};
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
pub(crate) async fn list_missions(
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
) -> Result<Json<Value>, ApiError> {
    reads
        .run(move || {
            let index_contents = read_missions_index_sync(&server.repo_root);
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
            Ok(Json(Value::Array(rows)))
        })
        .await
}

/// `GET /api/missions/outcomes` — flight-surgeon outcomes fold (autonomy
/// ratio, grant-latency distribution, escalation ledger), computed
/// per-request from the event logs by [`kranz_engine::outcomes::compute_outcomes`].
/// Reuses the engine log memo; no second source of truth.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OutcomeQuery {
    window_days: Option<u64>,
}

pub(crate) async fn mission_outcomes(
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
    Query(query): Query<OutcomeQuery>,
) -> Result<Json<kranz_engine::outcomes::Outcomes>, ApiError> {
    let days = query
        .window_days
        .unwrap_or(kranz_engine::outcomes::DEFAULT_MERGED_CHANGE_WINDOW_DAYS);
    if days > kranz_engine::outcomes::MAX_MERGED_CHANGE_WINDOW_DAYS {
        return Err(ApiError::bad_request("outcome reason window is too large"));
    }
    reads
        .run(move || {
            let mut options = kranz_engine::outcomes::OutcomesOptions::resolve(&server.repo_root);
            options.reason_window = Some((days, chrono::Utc::now()));
            let outcomes =
                kranz_engine::outcomes::compute_outcomes_with_options(&server.repo_root, &options)
                    .map_err(|e| ApiError::internal(e.to_string()))?;
            Ok(Json(outcomes))
        })
        .await
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
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
) -> Result<Json<kranz_engine::escalation_metrics::EscalationMetrics>, ApiError> {
    reads
        .run(move || {
            let metrics =
                kranz_engine::escalation_metrics::compute_escalation_metrics(&server.repo_root)
                    .map_err(|e| ApiError::internal(e.to_string()))?;
            Ok(Json(metrics))
        })
        .await
}

/// Cross-mission Flight Rules effectiveness fold (KRZ-348). Read-only and
/// recomputed from event logs plus traced defect ticket links on every call.
pub(crate) async fn standards_metrics(
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
) -> Result<Json<kranz_engine::standards_metrics::StandardsMetricsReport>, ApiError> {
    reads
        .run(move || {
            Ok(Json(kranz_engine::standards_metrics::compute(
                &server.repo_root,
            )?))
        })
        .await
}

/// `GET /api/cost-per-merged-change?windowDays=30` — cost per merged change
/// for the served repo (ticket `cost-per-merged-change`, KRZ-329): the cost
/// fold over missions closed in the window beside the merged-change count
/// derived at fold time (merged.rs's landed/ancestry probe — never stored)
/// and the window's autonomy ratio. `windowDays` defaults to
/// [`kranz_engine::outcomes::DEFAULT_MERGED_CHANGE_WINDOW_DAYS`] and is
/// capped at [`kranz_engine::outcomes::MAX_MERGED_CHANGE_WINDOW_DAYS`] —
/// over the cap is a 400, never a wrapped/panicked handler (12th-pass
/// review); the same folded JSON the CLI's `kranz outcomes --all`
/// aggregates per catalog repo.
pub(crate) async fn cost_per_merged_change(
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<kranz_engine::outcomes::CostPerMergedChange>, ApiError> {
    reads
        .run(move || {
            let window_days = match params.get("windowDays") {
                Some(raw) => {
                    let parsed = raw.parse::<u64>().map_err(|_| {
                        ApiError::bad_request("windowDays must be a non-negative integer")
                    })?;
                    if parsed > kranz_engine::outcomes::MAX_MERGED_CHANGE_WINDOW_DAYS {
                        return Err(ApiError::bad_request(format!(
                            "windowDays must be at most {} days",
                            kranz_engine::outcomes::MAX_MERGED_CHANGE_WINDOW_DAYS
                        )));
                    }
                    parsed
                }
                None => kranz_engine::outcomes::DEFAULT_MERGED_CHANGE_WINDOW_DAYS,
            };
            let report = kranz_engine::outcomes::compute_cost_per_merged_change(
                &server.repo_root,
                window_days,
                chrono::Utc::now(),
            )
            .map_err(|e| ApiError::internal(e.to_string()))?;
            Ok(Json(report))
        })
        .await
}

/// `<repo>/.kranz/missions/index.md` contents, or `""` if the file is absent
/// (never created here — callers only read the catalog).
///
/// No-follow, like every other repo file this crate reads: the catalog lives
/// in a worker-writable tree and a symlinked `index.md` must read as absent,
/// not as whatever it points at.
fn read_missions_index_sync(repo_root: &Path) -> String {
    let path = MissionPaths::new(repo_root, "_")
        .missions_dir()
        .join("index.md");
    read_file_or_404_sync(&path, "no mission index".into()).unwrap_or_default()
}

/// `GET /api/missions/:id/state` — full [`MissionState`], folded from
/// events.jsonl (NOT the state.json cache). 404 for an unknown mission.
pub(crate) async fn mission_state(
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<MissionState>, ApiError> {
    reads
        .run(move || {
            let paths = mission_paths(&server, &id)?;
            let events_path = paths.events_file();
            if !events_path.is_file() {
                return Err(unknown_mission(&id));
            }
            let events = EventLog::read_events(&events_path)?;
            Ok(Json(reducer::fold(&events)?))
        })
        .await
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StandardsWaiverCandidate {
    rule: kranz_engine::types::PinnedRule,
    finding_subject: String,
    finding_evidence: String,
    run_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MissionStandardsView {
    #[serde(skip_serializing_if = "Option::is_none")]
    manifest: Option<kranz_engine::types::StandardsPin>,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage: Option<kranz_engine::standards_coverage::StandardsCoverage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    waiver_candidates: Vec<StandardsWaiverCandidate>,
}

fn fold_standards_view(
    id: &str,
    events: &[Event],
) -> kranz_engine::error::Result<MissionStandardsView> {
    let state = reducer::fold(events)?;
    let manifest = state.mission.standards_manifest.clone();
    let coverage = kranz_engine::standards_coverage::standards_coverage(id, events);
    let mut waiver_candidates = Vec::new();
    if let (Some(pin), Some(coverage)) = (manifest.as_ref(), coverage.as_ref()) {
        for row in coverage.rules.iter().filter(|row| {
            row.disposition == kranz_engine::standards_coverage::RuleDisposition::Failed
        }) {
            let Some(rule) = pin
                .rules
                .iter()
                .find(|rule| rule.id == row.id && rule.revision == row.revision && rule.waivable)
            else {
                continue;
            };
            if let Some((finding_subject, finding_evidence, run_id)) = events
                .iter()
                .filter(|event| event.mission_id == id)
                .rev()
                .find_map(|event| match &event.kind {
                    EventKind::ValidationFinding {
                        finding, run_id, ..
                    } if finding.rule.as_ref().is_some_and(|citation| {
                        citation.id == rule.id
                            && citation.revision == rule.revision
                            && citation.digest == pin.digest
                    }) =>
                    {
                        Some((
                            finding.subject.clone(),
                            finding.evidence.clone(),
                            run_id.clone(),
                        ))
                    }
                    _ => None,
                })
            {
                waiver_candidates.push(StandardsWaiverCandidate {
                    rule: rule.clone(),
                    finding_subject,
                    finding_evidence,
                    run_id,
                });
            }
        }
    }
    Ok(MissionStandardsView {
        manifest,
        coverage,
        waiver_candidates,
    })
}

/// Typed Flight Rules read model. No configured standards is represented by
/// `{}` so old missions add no empty warning surface.
pub(crate) async fn mission_standards(
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<MissionStandardsView>, ApiError> {
    reads
        .run(move || {
            let paths = mission_paths(&server, &id)?;
            if !paths.events_file().is_file() {
                return Err(unknown_mission(&id));
            }
            let events = EventLog::read_events(&paths.events_file())?;
            Ok(Json(fold_standards_view(&id, &events)?))
        })
        .await
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StandardsWaiverBody {
    rule_id: String,
    #[serde(default)]
    revision: Option<u64>,
    #[serde(default)]
    finding_subject: Option<String>,
    reason: String,
    expires_at: String,
}

/// Authenticated REST twin of `kranz standards waive`. It intentionally
/// refuses while the mission engine holds the append lock; the UI keeps the
/// evidence visible and tells the operator to pause/stop before retrying.
pub(crate) async fn post_standards_waiver(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<StandardsWaiverBody>,
) -> Result<Json<Value>, ApiError> {
    mission_paths(&server, &id)?;
    let expires_at = chrono::DateTime::parse_from_rfc3339(&body.expires_at)
        .map_err(|error| ApiError::bad_request(format!("expiresAt must be RFC 3339: {error}")))?
        .with_timezone(&chrono::Utc);
    let request = kranz_engine::standards_waiver::WaiverRequest {
        rule_id: body.rule_id,
        revision: body.revision,
        finding_subject: body.finding_subject,
        reason: body.reason,
        expires_at,
    };
    let outcome = kranz_engine::standards_waiver::approve_standards_waiver(
        &server.repo_root,
        &id,
        &request,
        "rest",
        kranz_engine::event_log::LockForce::No,
    )
    .map_err(|error| match error {
        kranz_engine::error::EngineError::LockHeld(_) => ApiError::conflict(format!(
            "waiver refused: mission '{id}' is still running; pause or stop it before approving this exception"
        )),
        other => ApiError::unprocessable(format!("waiver refused: {other}")),
    })?;
    Ok(Json(json!({
        "recorded": true,
        "seq": outcome.event.seq,
        "rule": outcome.rule,
        "findingSubject": outcome.finding_subject,
        "findingEvidence": outcome.finding_evidence,
        "runId": outcome.run_id,
        "affectedPaths": outcome.affected_paths,
        "diffDigest": outcome.diff_digest,
        "findingFingerprint": outcome.finding_fingerprint,
    })))
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
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    reads.run(move || {
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
    })
    .await
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

/// Human-only observation; never persists evidence or queues a decision.
pub(crate) async fn mission_review_packet(
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    reads
        .run(move || {
            let paths = mission_paths(&server, &id)?;
            if !paths.events_file().is_file() {
                return Err(unknown_mission(&id));
            }
            let packet =
                kranz_engine::review_packet::compute_review_packet(&server.repo_root, &id)?;
            let markdown = kranz_engine::review_packet::render_markdown(&packet);
            Ok(Json(json!({ "packet": packet, "markdown": markdown })))
        })
        .await
}

/// `GET /api/missions/:id/events?since=<seq>` — events with `seq > since`
/// (all events when `since` is omitted).
pub(crate) async fn mission_events(
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<Event>>, ApiError> {
    reads
        .run(move || {
            let paths = mission_paths(&server, &id)?;
            let events_path = paths.events_file();
            if !events_path.is_file() {
                return Err(unknown_mission(&id));
            }
            let since = match params.get("since") {
                None => 0,
                Some(raw) => raw.parse::<u64>().map_err(|_| {
                    ApiError::bad_request(format!("invalid 'since' value: '{raw}'"))
                })?,
            };
            Ok(Json(EventLog::read_events_after(&events_path, since)?))
        })
        .await
}

/// `GET /api/missions/:id/plan` — contents of plan.json; 404 until the plan
/// has been approved (i.e. the file exists).
pub(crate) async fn mission_plan(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    crate::read_work::run(move || {
        let paths = mission_paths(&server, &id)?;
        let content = read_file_or_404_sync(
            &paths.plan_file(),
            format!("mission '{id}' has no approved plan yet"),
        )?;
        let plan: Value = serde_json::from_str(&content)
            .map_err(|e| ApiError::internal(format!("plan.json is not valid JSON: {e}")))?;
        Ok(Json(plan))
    })
    .await
}

/// `GET /api/missions/:id/plan.md` — rendered plan markdown; 404 until
/// plan.md has been written (alongside plan.json, at approval time).
pub(crate) async fn mission_plan_md(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    crate::read_work::run(move || {
        let paths = mission_paths(&server, &id)?;
        let markdown = read_file_or_404_sync(
            &paths.plan_md_file(),
            format!("mission '{id}' has no approved plan yet"),
        )?;
        Ok(Json(json!({ "markdown": markdown })))
    })
    .await
}

/// `GET /api/missions/:id/revision-diff` — pending revision review artifact.
/// Returns 404 when no proposed revision is awaiting approval.
pub(crate) async fn mission_revision_diff(
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    reads
        .run(move || {
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
            let current = read_file_or_404_sync(
                &paths.plan_md_file(),
                format!("mission '{id}' has no approved plan yet"),
            )?;
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
                &state.config.worker_candidates,
            );
            Ok(Json(json!({
                "revision": pending.revision,
                "instructions": pending.instructions,
                "markdown": revised,
                "diff": simple_line_diff("plan.md", "revised-plan.md", &current, &revised),
            })))
        })
        .await
}

#[derive(Default, Deserialize)]
pub(crate) struct ReportQuery {
    #[serde(default)]
    pub(crate) review: bool,
}

/// Report markdown; the authenticated `review=true` view adds an ephemeral
/// human packet without writing it into the source tree.
pub(crate) async fn mission_report_md(
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    Query(query): Query<ReportQuery>,
) -> Result<Json<Value>, ApiError> {
    reads
        .run(move || {
            let paths = mission_paths(&server, &id)?;
            let mut markdown = read_file_or_404_sync(
                &paths.report_file(),
                format!("mission '{id}' has no report yet"),
            )?;
            if query.review {
                let packet =
                    kranz_engine::review_packet::compute_review_packet(&server.repo_root, &id)?;
                markdown.push_str(&kranz_engine::review_packet::render_markdown(&packet));
            }
            Ok(Json(json!({ "markdown": markdown })))
        })
        .await
}

/// `GET /api/missions/:id/diff-stat` — `git diff --stat` of the pinned
/// `base_sha` against the mission branch tip; 404 until the plan is
/// approved (`base_sha` set) and the mission branch exists.
pub(crate) async fn mission_diff_stat(
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    reads
        .run(move || {
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
        })
        .await
}

/// `GET /api/missions/:id/pr-handoff` — optional GitHub PR handoff for a
/// COMPLETE-but-unmerged mission. Never pushes; may return a copyable
/// `git push` or a `gh pr create` command.
pub(crate) async fn mission_pr_handoff(
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    reads
        .run(move || {
            let paths = mission_paths(&server, &id)?;
            if !paths.events_file().is_file() {
                return Err(unknown_mission(&id));
            }
            let handoff = kranz_engine::pr_handoff::assess_mission(&server.repo_root, &id)
                .map_err(|e| ApiError::internal(e.to_string()))?;
            Ok(Json(
                serde_json::to_value(handoff).map_err(|e| ApiError::internal(e.to_string()))?,
            ))
        })
        .await
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
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    reads
        .run(move || {
            let _ = mission_paths(&server, &id)?;
            let report = kranz_engine::backend_readiness::probe_mission(&server.repo_root, &id)
                .map_err(|e| ApiError::internal(e.to_string()))?;
            Ok(Json(
                serde_json::to_value(report).map_err(|e| ApiError::internal(e.to_string()))?,
            ))
        })
        .await
}

/// `POST /api/hook-status` — the hook-status lane's ONLY write (ticket
/// `agent-hooks-status-signals`, [`kranz_engine::hook_status`]). Receives
/// one mapped lifecycle signal from a session's `kranz hook-status` relay
/// and records it in the ephemeral `.kranz/hook-status/` projection.
///
/// Authenticates with the per-RUN capability token in the body — never the
/// serve mutation token (a worker-readable file can only ever carry a
/// token whose forgery ceiling is lying about its own run's status), so
/// the route is exempt from the mutation-token gate exactly like the
/// GitHub webhook's HMAC route. The payload is untrusted even on loopback:
/// the body is route-limited to
/// [`kranz_engine::hook_status::SIGNAL_BODY_MAX_BYTES`], ids are safe-id
/// checked (path traversal), the token is constant-time compared against
/// the registered hash, stale registrations reject, and the handler's only
/// write is the projection file — no event, no state mutation, no grant
/// path exists here.
pub(crate) async fn post_hook_status(
    State(server): State<Arc<ServerState>>,
    Json(body): Json<kranz_engine::hook_status::SignalPost>,
) -> Result<impl IntoResponse, ApiError> {
    use kranz_engine::hook_status::RecordRejection;
    match kranz_engine::hook_status::record_signal(
        &server.repo_root,
        &body.mission_id,
        &body.run_id,
        &body.token,
        body.signal,
        body.detail.as_deref(),
        chrono::Utc::now(),
    ) {
        Ok(_) => Ok((StatusCode::ACCEPTED, Json(json!({ "recorded": true })))),
        Err(RecordRejection::UnsafeId) | Err(RecordRejection::UnknownRun) => Err(
            ApiError::not_found(format!("unknown hook-status run '{}'", body.run_id)),
        ),
        Err(RecordRejection::TokenMismatch) => Err(ApiError::unauthorized(
            "hook-status token does not match this run's registration",
        )),
        Err(RecordRejection::Stale) => Err(ApiError::unauthorized(
            "hook-status registration is stale (past its acceptance TTL)",
        )),
        Err(RecordRejection::RegistrationUnreadable) => Err(ApiError::internal(
            "hook-status projection entry could not be read or written",
        )),
    }
}

/// `GET /api/missions/:id/hook-status` — the ephemeral hook-signal
/// projection for one mission, re-read from disk per request like every
/// other derived read. Additive and explicitly NON-authoritative: the
/// payload carries `authoritative: false` so no consumer can mistake
/// hook-derived signals for folded mission state (the fold is untouched —
/// nothing in this lane can change a mission's terminal state).
pub(crate) async fn mission_hook_status(
    Extension(reads): Extension<ReadWork>,
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    reads.run(move || {
        let paths = mission_paths(&server, &id)?;
        if !paths.events_file().is_file() {
            return Err(unknown_mission(&id));
        }
        let runs = kranz_engine::hook_status::read_mission_signals(&server.repo_root, &id);
        Ok(Json(json!({
            "missionId": id,
            "authoritative": false,
            "note": "hook-derived lifecycle signals; observability only, never folded mission state",
            "runs": runs,
        })))
    })
    .await
}

/// `GET /api/missions/:id/runs/:runId/transcript` — the run's JSONL parsed
/// into a JSON array of raw stream values; 404 if the file is missing.
pub(crate) async fn run_transcript(
    State(server): State<Arc<ServerState>>,
    UrlPath((id, run_id)): UrlPath<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    crate::read_work::run(move || {
        let paths = mission_paths(&server, &id)?;
        if !safe_id(&run_id) {
            return Err(ApiError::not_found(format!("unknown run '{run_id}'")));
        }
        let content = read_file_or_404_sync(
            &paths.transcript_file(&run_id),
            format!("no transcript for run '{run_id}'"),
        )?;
        // Tolerate torn/garbage lines (a live transcript may end mid-write).
        let values: Vec<Value> = content
            .lines()
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect();
        Ok(Json(Value::Array(values)))
    })
    .await
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
    if matches!(command, ControlCommand::ResolvePermission { .. }) {
        return Err(ApiError::bad_request(
            "use the permission/answer route; permission actor is assigned by the server",
        ));
    }
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PermissionAnswerBody {
    request_id: String,
    binding_digest: String,
    allow: bool,
}

pub(crate) async fn post_permission_answer(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<PermissionAnswerBody>,
) -> Result<impl IntoResponse, ApiError> {
    let paths = require_revisable_mission(&server, &id)?;
    let state = fold_log(&paths).map_err(ApiError::internal)?;
    let record = state
        .permissions
        .get(&body.request_id)
        .ok_or_else(|| ApiError::conflict("unknown live permission"))?;
    record
        .validate_answer(&body.binding_digest, body.allow, chrono::Utc::now())
        .map_err(|e| ApiError::conflict(e.to_string()))?;
    control::enqueue(
        &paths,
        &ControlCommand::ResolvePermission {
            resolution: kranz_engine::live_permission::Resolution {
                request_id: body.request_id,
                binding_digest: body.binding_digest,
                allow: body.allow,
                actor: kranz_engine::live_permission::Actor::LocalMutationCapability,
                reason: "operator answered through the authenticated local API".into(),
            },
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

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct QuestionAnswerBody {
    /// The open question's engine-minted id (`q-<n>`).
    question_id: String,
    /// The chosen option's text verbatim, or free text.
    answer: String,
    /// 0-based option index when an offered option was picked; absent for
    /// free-text answers.
    #[serde(default)]
    option: Option<u32>,
}

/// `POST /api/missions/:id/question/answer` — answer an open structured
/// question (ticket `structured-human-question-events`): the pending-decision
/// projection's input edge, enqueued onto the EXISTING control path (D-X: no
/// new server). The engine lands it as `question.answered`, which the reducer
/// routes onto the mission's user-message consult.
pub(crate) async fn post_question_answer(
    State(server): State<Arc<ServerState>>,
    UrlPath(id): UrlPath<String>,
    Json(body): Json<QuestionAnswerBody>,
) -> Result<impl IntoResponse, ApiError> {
    let paths =
        require_pending_question(&server, &id, &body.question_id, body.option, &body.answer)?;
    control::enqueue(
        &paths,
        &ControlCommand::AnswerQuestion {
            question_id: body.question_id,
            answer: body.answer,
            option: body.option,
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

/// Confirm question `question_id` is open (and an option-index answer is in
/// range and matches the offered option), so the enqueued answer can't land
/// on a different (or absent) question than the operator saw — the same
/// stale-decision discipline as [`require_pending_grant`]. The engine
/// re-validates at drain time; this pre-check is what lets the caller get an
/// honest 409 instead of a silently ignored 202.
fn require_pending_question(
    server: &ServerState,
    id: &str,
    question_id: &str,
    option: Option<u32>,
    answer: &str,
) -> Result<MissionPaths, ApiError> {
    let paths = require_revisable_mission(server, id)?;
    let state = fold_log(&paths).map_err(ApiError::internal)?;
    let Some(pending) = state
        .pending_questions
        .iter()
        .find(|q| q.question_id == question_id)
    else {
        return Err(ApiError::conflict(format!(
            "mission '{id}' has no open question '{question_id}'"
        )));
    };
    if let Some(index) = option {
        match pending.options.get(index as usize) {
            Some(expected) if expected == answer => {}
            Some(expected) => {
                return Err(ApiError::conflict(format!(
                    "answer `{answer}` does not match option {index} (`{expected}`) of question '{question_id}'"
                )))
            }
            None => {
                return Err(ApiError::conflict(format!(
                    "question '{question_id}' has no option {index} (it offered {})",
                    pending.options.len()
                )))
            }
        }
    }
    Ok(paths)
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

/// Read one mission leaf file (`plan.json`, `plan.md`, `report.md`, a run
/// transcript) NO-FOLLOW.
///
/// `mission_paths` pins the directory chain, but the leaf used to be a plain
/// `read_to_string`, which follows symlinks: under checkout isolation a
/// session could replace `plan.md` with a link to `.kranz/serve.token` and
/// read the mutation token back over a tokenless loopback GET, defeating the
/// sandbox's own read-deny on that file. Every leaf read goes through the
/// engine's no-follow open now — the same helper `EventLog` uses. A refusal
/// is a 404, like a missing file: the API says nothing about what the link
/// pointed at.
// Cap concurrent file work as well as bytes per response. The permit moves
// into the blocking task so cancellation cannot release capacity prematurely.
const MAX_ARTIFACT_BYTES: u64 = 8 * 1024 * 1024;

#[cfg(test)]
async fn read_file_or_404(
    path: &Path,
    not_found_msg: impl FnOnce() -> String,
) -> Result<String, ApiError> {
    let path = path.to_path_buf();
    let missing = not_found_msg();
    crate::read_work::run(move || read_file_or_404_sync(&path, missing)).await
}

fn read_file_or_404_sync(path: &Path, missing: String) -> Result<String, ApiError> {
    let file = match kranz_engine::paths::open_read_nofollow(path) {
        Ok(file) => file,
        Err(kranz_engine::error::EngineError::Io(e)) if e.kind() == ErrorKind::NotFound => {
            return Err(ApiError::not_found(missing));
        }
        Err(kranz_engine::error::EngineError::InvalidState(_)) => {
            return Err(ApiError::not_found(missing));
        }
        Err(e) => {
            return Err(ApiError::internal(format!(
                "failed to read {}: {e}",
                path.display()
            )))
        }
    };
    kranz_engine::paths::read_regular_file_bounded(file, MAX_ARTIFACT_BYTES).map_err(|e| {
        if e.kind() == ErrorKind::FileTooLarge {
            ApiError {
                status: axum::http::StatusCode::PAYLOAD_TOO_LARGE,
                code: None,
                message: format!("artifact exceeds the {MAX_ARTIFACT_BYTES}-byte read limit"),
            }
        } else {
            ApiError::internal(format!("failed to read {}: {e}", path.display()))
        }
    })
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use kranz_engine::event_log::{EventLog, LockForce};
    use kranz_engine::events::EventKind;
    use kranz_engine::paths::MissionPaths;
    use kranz_engine::types::{
        Finding, GrantKind, MissionConfig, PinnedRule, Plan, PlanFeature, PlanMilestone,
        RuleCitation, StandardsPin, StandardsPinSource,
    };
    use serde_json::Value;
    use std::time::Duration;
    use tempfile::TempDir;
    use tower::ServiceExt;

    #[tokio::test]
    async fn artifact_reads_reject_oversized_and_nonregular_inputs() {
        let repo = TempDir::new().unwrap();
        let paths = MissionPaths::new(repo.path(), "m-bounded");
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        let path = paths.report_file();
        std::fs::File::create(&path)
            .unwrap()
            .set_len(super::MAX_ARTIFACT_BYTES + 1)
            .unwrap();
        let error = super::read_file_or_404(&path, || "missing".into())
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::PAYLOAD_TOO_LARGE);
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        let error = super::read_file_or_404(&path, || "missing".into())
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::NOT_FOUND);
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, "complete report").unwrap();
        assert_eq!(
            super::read_file_or_404(&path, || "missing".into())
                .await
                .unwrap(),
            "complete report"
        );
    }

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

    fn post_json(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn flight_rules_dashboard_standards_view_is_typed_and_hides_nonwaivable_actions() {
        let tmp = TempDir::new().unwrap();
        let digest = "ab".repeat(32);
        let rule = |id: &str, waivable: bool| PinnedRule {
            id: id.to_string(),
            revision: 2,
            rfc: "RFC-001".to_string(),
            level: "must".to_string(),
            effective_status: "enforced".to_string(),
            statement: format!("statement for {id}"),
            domains: vec!["security".to_string()],
            stages: vec!["validation".to_string()],
            when_paths: Vec::new(),
            task_classes: Vec::new(),
            checker: Some("gate:secure".to_string()),
            waivable,
        };
        let rules = vec![rule("ZZ-WAIVE-001", true), rule("ZZ-LOCKED-001", false)];
        let pin = StandardsPin {
            pack_name: "zz-pack".to_string(),
            pack_dir: "vendor/pack".to_string(),
            standards_root: "standards".to_string(),
            digest: digest.clone(),
            source: StandardsPinSource::RepoTracked,
            task_class: None,
            touch_set: vec!["src/**".to_string()],
            context_paths: Vec::new(),
            gates: Vec::new(),
            rules: rules.clone(),
        };
        let plan = Plan {
            goal: "governed change".to_string(),
            validation_contract: Vec::new(),
            milestones: vec![PlanMilestone {
                title: "one".to_string(),
                features: vec![PlanFeature {
                    title: "change".to_string(),
                    spec: "implement".to_string(),
                    validation_criteria: Vec::new(),
                }],
            }],
            considered_alternatives: None,
            command_grants: Vec::new(),
            touch_set: vec!["src/**".to_string()],
            standards_manifest: Some(Box::new(pin)),
            reviewer_independence: None,
        };
        let finding = |rule: &PinnedRule| Finding {
            subject: format!("flight-rule:{}", rule.id),
            severity: "critical".to_string(),
            evidence: format!("{} failed with exact evidence", rule.id),
            suggested_fix: "fix it".to_string(),
            class: "standards-authoritative".to_string(),
            rule: Some(RuleCitation {
                id: rule.id.clone(),
                revision: rule.revision,
                source: "zz-pack standards".to_string(),
                digest: digest.clone(),
                lifecycle: rule.effective_status.clone(),
                level: rule.level.clone(),
                checker: rule.checker.clone(),
            }),
        };
        seed_mission(
            tmp.path(),
            "m-1",
            vec![
                created("governed change"),
                EventKind::PlanApproved {
                    plan,
                    base_sha: Some("deadbeef".to_string()),
                },
                EventKind::ValidationFinding {
                    milestone_id: "ms-1".to_string(),
                    run_id: kranz_engine::reducer::ENGINE_RUN_ID.to_string(),
                    finding: finding(&rules[0]),
                },
                EventKind::ValidationFinding {
                    milestone_id: "ms-1".to_string(),
                    run_id: kranz_engine::reducer::ENGINE_RUN_ID.to_string(),
                    finding: finding(&rules[1]),
                },
            ],
        );
        let app = crate::router(tmp.path().to_path_buf(), None);
        let response = app
            .oneshot(get("/api/missions/m-1/standards"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["manifest"]["digest"], digest);
        assert_eq!(body["coverage"]["rules"][0]["disposition"], "failed");
        assert_eq!(body["waiverCandidates"].as_array().unwrap().len(), 1);
        assert_eq!(body["waiverCandidates"][0]["rule"]["id"], "ZZ-WAIVE-001");
        assert!(body["waiverCandidates"][0]["findingEvidence"]
            .as_str()
            .unwrap()
            .contains("exact evidence"));
    }

    /// The lane end to end over HTTP: a registered run's signal POST lands
    /// in the ephemeral projection and is served by the per-mission GET —
    /// labelled non-authoritative — while the mission's folded state and
    /// its event log stay byte-identical (hooks never become mission
    /// state; a terminal mission stays terminal).
    #[tokio::test]
    async fn hook_status_signal_endpoint_records_serves_and_never_touches_state() {
        let tmp = TempDir::new().unwrap();
        seed_mission(
            tmp.path(),
            "m-1",
            vec![created("terminal"), EventKind::MissionCompleted {}],
        );
        kranz_engine::hook_status::register(tmp.path(), "m-1", "r-1", "tok-1", chrono::Utc::now())
            .unwrap();
        let events_before =
            std::fs::read(MissionPaths::new(tmp.path(), "m-1").events_file()).unwrap();
        let app = crate::router(tmp.path().to_path_buf(), None);

        let response = app
            .clone()
            .oneshot(post_json(
                "/api/hook-status",
                &serde_json::json!({
                    "token": "tok-1",
                    "missionId": "m-1",
                    "runId": "r-1",
                    "signal": "needs-input",
                    "detail": "Shell was refused",
                })
                .to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);

        let response = app
            .clone()
            .oneshot(get("/api/missions/m-1/hook-status"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["authoritative"], false);
        let runs = body["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["runId"], "r-1");
        assert_eq!(runs[0]["signal"]["signal"], "needs-input");
        assert_eq!(runs[0]["signal"]["detail"], "Shell was refused");
        assert!(
            runs[0]["signal"]["receivedAt"].as_str().is_some(),
            "{runs:?}"
        );

        // Folded mission state is untouched by the lane: the terminal
        // mission still folds Complete and the event log is byte-identical.
        let response = app.oneshot(get("/api/missions/m-1/state")).await.unwrap();
        let state = body_json(response).await;
        assert_eq!(state["mission"]["status"], "complete");
        let events_after =
            std::fs::read(MissionPaths::new(tmp.path(), "m-1").events_file()).unwrap();
        assert_eq!(events_before, events_after);
    }

    /// Untrusted-payload discipline over HTTP: wrong tokens, unknown runs,
    /// traversal ids, malformed bodies, and oversized bodies are all
    /// rejected; nothing is written for any of them.
    #[tokio::test]
    async fn hook_status_signal_endpoint_rejects_untrusted_payloads() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-1", vec![created("x")]);
        kranz_engine::hook_status::register(tmp.path(), "m-1", "r-1", "tok-1", chrono::Utc::now())
            .unwrap();
        let app = crate::router(tmp.path().to_path_buf(), None);

        // Wrong capability token → 401.
        let response = app
            .clone()
            .oneshot(post_json(
                "/api/hook-status",
                &serde_json::json!({
                    "token": "tok-WRONG",
                    "missionId": "m-1",
                    "runId": "r-1",
                    "signal": "running",
                })
                .to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // Unknown run → 404 (no oracle about neighboring runs).
        let response = app
            .clone()
            .oneshot(post_json(
                "/api/hook-status",
                &serde_json::json!({
                    "token": "tok-1",
                    "missionId": "m-1",
                    "runId": "r-9",
                    "signal": "running",
                })
                .to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // Path traversal in the mission id → 404, never a joined path.
        let response = app
            .clone()
            .oneshot(post_json(
                "/api/hook-status",
                &serde_json::json!({
                    "token": "tok-1",
                    "missionId": "../m-1",
                    "runId": "r-1",
                    "signal": "running",
                })
                .to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // A signal outside the vocabulary is a 4xx — the lane can never
        // spell a state transition ("complete", "blocked", ...).
        let response = app
            .clone()
            .oneshot(post_json(
                "/api/hook-status",
                &serde_json::json!({
                    "token": "tok-1",
                    "missionId": "m-1",
                    "runId": "r-1",
                    "signal": "complete",
                })
                .to_string(),
            ))
            .await
            .unwrap();
        assert!(
            response.status().is_client_error(),
            "an out-of-vocabulary signal must be rejected: {}",
            response.status()
        );

        // Malformed JSON → 4xx.
        let response = app
            .clone()
            .oneshot(post_json("/api/hook-status", "{not json"))
            .await
            .unwrap();
        assert!(response.status().is_client_error());

        // Oversized body → 413 (the route's own 16 KiB limit).
        let oversized = format!(
            "{{\"token\":\"tok-1\",\"missionId\":\"m-1\",\"runId\":\"r-1\",\"signal\":\"running\",\"detail\":\"{}\"}}",
            "x".repeat(kranz_engine::hook_status::SIGNAL_BODY_MAX_BYTES)
        );
        let response = app
            .clone()
            .oneshot(post_json("/api/hook-status", &oversized))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        // None of the rejections recorded anything.
        let views = kranz_engine::hook_status::read_mission_signals(tmp.path(), "m-1");
        assert!(views.iter().all(|v| v.signal.is_none()), "{views:?}");
    }

    /// The POST authenticates with the per-run capability token, NOT the
    /// serve mutation token: with the gate armed, the signal POST goes
    /// through WITHOUT `x-kranz-token` while an ordinary mutation POST is
    /// still rejected.
    #[tokio::test]
    async fn hook_status_signal_post_is_exempt_from_the_mutation_token_gate() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-1", vec![created("x")]);
        kranz_engine::hook_status::register(tmp.path(), "m-1", "r-1", "tok-1", chrono::Utc::now())
            .unwrap();
        let app = crate::router_with_token(
            tmp.path().to_path_buf(),
            None,
            crate::MutationAuthority::new("serve-secret").unwrap(),
        );

        let response = app
            .clone()
            .oneshot(post_json(
                "/api/hook-status",
                &serde_json::json!({
                    "token": "tok-1",
                    "missionId": "m-1",
                    "runId": "r-1",
                    "signal": "running",
                })
                .to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::ACCEPTED,
            "the per-run capability token authenticates the lane, not the serve token"
        );

        // An ordinary mutation without the serve token is still refused.
        let response = app
            .oneshot(post_json(
                "/api/missions/m-1/revise",
                "{\"instructions\":\"x\"}",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    /// Install hygiene at the server seam: serving and recording never
    /// writes hook config into the repo's tracked tree — the only tree the
    /// lane writes is the gitignored `.kranz/hook-status/` projection.
    #[tokio::test]
    async fn hook_status_signal_server_writes_nothing_into_the_tracked_tree() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-1", vec![created("x")]);
        kranz_engine::hook_status::register(tmp.path(), "m-1", "r-1", "tok-1", chrono::Utc::now())
            .unwrap();
        let app = crate::router(tmp.path().to_path_buf(), None);
        let response = app
            .clone()
            .oneshot(post_json(
                "/api/hook-status",
                &serde_json::json!({
                    "token": "tok-1",
                    "missionId": "m-1",
                    "runId": "r-1",
                    "signal": "turn-finished",
                })
                .to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let response = app
            .oneshot(get("/api/missions/m-1/hook-status"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        assert!(
            !tmp.path().join(".cursor").exists(),
            "no cursor hook config may appear in the repo tree"
        );
        assert!(
            kranz_engine::hook_status::hook_status_dir(tmp.path()).is_dir(),
            "the projection is the lane's only write"
        );
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
    async fn outcome_reasons_endpoint_uses_the_engine_fold_and_validates_window() {
        let tmp = TempDir::new().unwrap();
        let mut plan = sample_plan();
        plan.milestones.push(PlanMilestone {
            title: "unit".into(),
            features: vec![],
        });
        seed_mission(
            tmp.path(),
            "m-1",
            vec![
                created("seeded"),
                EventKind::PlanApproved {
                    plan,
                    base_sha: None,
                },
                EventKind::MilestoneBlocked {
                    milestone_id: "ms-1".into(),
                    reason: "Authentication unavailable".into(),
                    block_context: Some(kranz_engine::types::BlockContext::engine(
                        kranz_engine::types::BlockCause::Authentication,
                    )),
                },
            ],
        );
        let paths = MissionPaths::new(tmp.path(), "m-1");
        let recorded = std::fs::read(paths.events_file()).unwrap();
        assert_eq!(std::fs::read_dir(paths.control_dir()).unwrap().count(), 0);
        let app = crate::router(tmp.path().to_path_buf(), None);
        let response = app
            .clone()
            .oneshot(get("/api/missions/outcomes?windowDays=7"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        let report: kranz_engine::outcomes::reasons::Report =
            serde_json::from_value(body["outcomeReasons"].clone()).unwrap();
        assert_eq!(
            report.through.unwrap() - report.from.unwrap(),
            chrono::Duration::days(7)
        );
        let options = kranz_engine::outcomes::OutcomesOptions {
            reason_window: Some((7, report.through.unwrap())),
            ..Default::default()
        };
        let engine =
            kranz_engine::outcomes::compute_outcomes_with_options(tmp.path(), &options).unwrap();
        assert_eq!(Some(report), engine.outcome_reasons);
        assert_eq!(body["outcomeReasons"]["taskClasses"][0]["missions"], 1);
        assert_eq!(
            body["outcomeReasons"]["missions"][0]["observations"][0]["category"],
            "environment-prerequisite"
        );
        for value in ["-1", "nonsense", "36526", "18446744073709551615"] {
            let response = app
                .clone()
                .oneshot(get(&format!("/api/missions/outcomes?windowDays={value}")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{value}");
        }
        assert_eq!(std::fs::read(paths.events_file()).unwrap(), recorded);
        assert_eq!(std::fs::read_dir(paths.control_dir()).unwrap().count(), 0);
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
    async fn flight_rules_metrics_endpoint_empty_repo_is_machine_readable() {
        let tmp = TempDir::new().unwrap();
        let app = crate::router(tmp.path().to_path_buf(), None);

        let response = app.oneshot(get("/api/standards-metrics")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;

        assert_eq!(body["minimumSamples"], 5);
        assert!(body["definitions"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty()));
        assert_eq!(body["rules"].as_array().unwrap().len(), 0);
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

    /// KRZ-333: the same endpoint also serves the industry-comparison set as
    /// a structurally separate section with its inline definitions — the CLI
    /// and the served payload stay one wire shape. The tempdir is no git
    /// repo, so the git-derived slots read absent naming their dependency.
    #[tokio::test]
    async fn comparison_metrics_outcomes_endpoint_serves_the_section() {
        let tmp = TempDir::new().unwrap();
        seed_mission(
            tmp.path(),
            "m-1",
            vec![created("seeded"), EventKind::MissionCompleted {}],
        );
        let app = crate::router(tmp.path().to_path_buf(), None);

        let response = app.oneshot(get("/api/missions/outcomes")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;

        let comparison = &body["comparison"];
        assert_eq!(comparison["windowDays"], 30);
        assert!(comparison["assistedChangeShare"]["definition"]
            .as_str()
            .unwrap()
            .contains("agent-involved by construction"));
        assert!(comparison["defectDensity"]["definition"]
            .as_str()
            .unwrap()
            .contains("traced-from-mission frontmatter"));
        // The empty resolution slot names its missing lifecycle timestamps.
        assert!(comparison["defectResolutionTime"]["dependency"]
            .as_str()
            .unwrap()
            .contains("open/close timestamps"));
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
            standards_manifest: None,
            reviewer_independence: None,
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

    /// Audit (server MEDIUM): the mission-dir walk is no-follow but the LEAF
    /// read was not, so a session under checkout isolation could point
    /// `plan.md` at `.kranz/serve.token` and read the mutation token back
    /// over a tokenless loopback GET. The leaf read is no-follow now: the
    /// route refuses instead of returning the target's bytes.
    #[cfg(unix)]
    #[tokio::test]
    async fn mission_leaf_reads_refuse_a_symlinked_file() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-1", vec![created("goal")]);
        let token_file = tmp.path().join(".kranz").join("serve.token");
        std::fs::write(&token_file, "super-secret-mutation-token").unwrap();
        let paths = MissionPaths::new(tmp.path(), "m-1");
        symlink(&token_file, paths.plan_md_file()).unwrap();
        symlink(&token_file, paths.report_file()).unwrap();
        symlink(&token_file, paths.plan_file()).unwrap();

        for uri in [
            "/api/missions/m-1/plan.md",
            "/api/missions/m-1/report.md",
            "/api/missions/m-1/plan",
        ] {
            let app = crate::router(tmp.path().to_path_buf(), None);
            let response = app.oneshot(get(uri)).await.unwrap();
            assert_ne!(
                response.status(),
                StatusCode::OK,
                "{uri} must refuse a symlinked leaf"
            );
            let body = body_json(response).await;
            assert!(
                !body.to_string().contains("super-secret-mutation-token"),
                "{uri} leaked the symlink target: {body}"
            );
        }
    }

    /// The same leaf rule for the tickets surface, which reads through
    /// `Ticket::load`.
    #[cfg(unix)]
    #[tokio::test]
    async fn ticket_reads_refuse_a_symlinked_file() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let token_file = tmp.path().join(".kranz").join("serve.token");
        std::fs::create_dir_all(tmp.path().join(".kranz")).unwrap();
        std::fs::write(&token_file, "super-secret-mutation-token").unwrap();
        let tickets = tmp.path().join(".kranz").join("tickets");
        std::fs::create_dir_all(&tickets).unwrap();
        symlink(&token_file, tickets.join("leak.md")).unwrap();

        let app = crate::router(tmp.path().to_path_buf(), None);
        let response = app.oneshot(get("/api/tickets/leak")).await.unwrap();
        assert_ne!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert!(
            !body.to_string().contains("super-secret-mutation-token"),
            "ticket read leaked the symlink target: {body}"
        );

        let app = crate::router(tmp.path().to_path_buf(), None);
        let response = app.oneshot(get("/api/tickets")).await.unwrap();
        let body = body_json(response).await;
        assert!(
            !body.to_string().contains("super-secret-mutation-token"),
            "ticket listing leaked the symlink target: {body}"
        );
    }

    /// The missions catalog (`.kranz/missions/index.md`) is read the same
    /// way: a symlinked index yields an empty catalog, never the target.
    #[cfg(unix)]
    #[tokio::test]
    async fn missions_index_read_refuses_a_symlink() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let missions_dir = tmp.path().join(".kranz").join("missions");
        std::fs::create_dir_all(&missions_dir).unwrap();
        let token_file = tmp.path().join(".kranz").join("serve.token");
        std::fs::write(&token_file, "super-secret-mutation-token").unwrap();
        symlink(&token_file, missions_dir.join("index.md")).unwrap();

        let app = crate::router(tmp.path().to_path_buf(), None);
        let response = app.oneshot(get("/api/missions")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert!(
            !body.to_string().contains("super-secret-mutation-token"),
            "the catalog read followed a symlink: {body}"
        );
    }

    /// 12th-pass review: an unbounded `windowDays` once reached the engine's
    /// `as i64` cast / chrono arithmetic and could wrap or panic the handler
    /// — a read-authorized request crashing its own endpoint. Over the
    /// documented maximum is a 400 now; the bound itself still computes.
    #[tokio::test]
    async fn cost_per_merged_change_window_days_bound_over_max_gets_400() {
        let tmp = TempDir::new().unwrap();
        let app = crate::router(tmp.path().to_path_buf(), None);

        for raw in [
            format!(
                "{}",
                kranz_engine::outcomes::MAX_MERGED_CHANGE_WINDOW_DAYS + 1
            ),
            u64::MAX.to_string(),
        ] {
            let response = app
                .clone()
                .oneshot(get(&format!(
                    "/api/cost-per-merged-change?windowDays={raw}"
                )))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "windowDays={raw} must be a 400, never a crash"
            );
        }
        // At the bound the endpoint computes normally (empty repo zeroes).
        let response = app
            .oneshot(get(&format!(
                "/api/cost-per-merged-change?windowDays={}",
                kranz_engine::outcomes::MAX_MERGED_CHANGE_WINDOW_DAYS
            )))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(
            body["windowDays"],
            kranz_engine::outcomes::MAX_MERGED_CHANGE_WINDOW_DAYS
        );
    }
}
