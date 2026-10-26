//! `POST /api/hooks/github` — the GitHub webhook trigger endpoint (design
//! D-F, ticket `trigger-ci-pr-fix-mission`).
//!
//! The route authenticates with the per-repo `hooks.secret` HMAC
//! (`X-Hub-Signature-256`), NOT the mutation token — the token gate exempts
//! this path, and with no secret configured the route refuses ALL requests
//! closed. Accepted events draft ONE deduped ticket through the normal
//! pipeline (the [`kranz_engine::hooks`] core decides); the route's only
//! action set is draft / draft+queue, and a regression test scans this file
//! to keep it free of any git-mutation path. Every decision — accepted,
//! ignored, refused — emits one structured log line (never the secret, the
//! signature, or the raw body).

use crate::error::ApiError;
use crate::ServerState;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use kranz_engine::git_ops::GitRepo;
use kranz_engine::hooks::{self, Consent, Trigger, TriggerDraft};
use kranz_engine::signals::proposal::{ingest_verified_signal, IngestResult};
use kranz_engine::signals::sentry::SentrySignalSource;
use kranz_engine::signals::source::{DropReason, ReplayCache};
use kranz_engine::signals::triage::SignalsConfig;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

static SENTRY_REPLAY_CACHE: OnceLock<Mutex<ReplayCache>> = OnceLock::new();

fn sentry_replay_cache() -> &'static Mutex<ReplayCache> {
    SENTRY_REPLAY_CACHE.get_or_init(|| Mutex::new(ReplayCache::default()))
}

/// `POST /api/hooks/github` — see module docs. Always answers a JSON body;
/// refusals are honest status codes (401 bad signature, 403 wrong repo or
/// unconfigured, 400/422 malformed), non-trigger events are 202-ignored.
pub(crate) async fn github_hook(
    State(server): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let repo_root = server.host.repo_root().clone();
    let event = headers
        .get("x-github-event")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();

    let cfg = hooks::load_hooks(&repo_root).map_err(ApiError::from)?;

    // Absent secret ⇒ refuse closed, never open-accept.
    let Some(secret) = cfg.secret.as_deref().filter(|s| !s.is_empty()) else {
        decision(
            &event,
            None,
            None,
            "refused",
            "no hooks.secret configured; route closed",
        );
        return Err(ApiError::forbidden(
            "github hooks are not configured for this repository (no hooks.secret)",
        ));
    };

    let signature = headers
        .get("x-hub-signature-256")
        .and_then(|value| value.to_str().ok());
    if !hooks::verify_signature(secret, &body, signature) {
        decision(&event, None, None, "refused", "bad X-Hub-Signature-256");
        return Err(ApiError::unauthorized("invalid X-Hub-Signature-256"));
    }

    // Allowlisted event kinds only; everything else is 202-ignored.
    if !hooks::ACCEPTED_EVENTS.contains(&event.as_str()) {
        decision(&event, None, None, "ignored", "event kind not allowlisted");
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({ "outcome": "ignored", "reason": "event kind not allowlisted" })),
        ));
    }

    let payload: Value = serde_json::from_slice(&body)
        .map_err(|e| ApiError::bad_request(format!("webhook body is not valid JSON: {e}")))?;

    // No cross-repo triggers: the payload's repository.full_name must match
    // the served repo's origin identity.
    let served = served_full_name(&repo_root)?.ok_or_else(|| {
        ApiError::forbidden(
            "cannot establish the served repository's GitHub full_name (origin remote \
             missing or not a github.com URL); refusing webhooks closed",
        )
    })?;
    match hooks::repo_full_name_from_payload(&payload) {
        Some(claimed) if claimed.eq_ignore_ascii_case(&served) => {}
        other => {
            decision(
                &event,
                None,
                None,
                "refused",
                &format!(
                    "payload repository {:?} does not match served repository",
                    other.unwrap_or_default()
                ),
            );
            return Err(ApiError::forbidden(
                "webhook repository does not match the served repository",
            ));
        }
    }

    // The event still has to match a trigger rule (action, conclusion,
    // branch, label); anything else is 202-ignored.
    let Some(trigger) = hooks::parse_trigger(&event, &payload, &cfg) else {
        decision(&event, None, None, "ignored", "no trigger rule matched");
        return Ok((
            StatusCode::ACCEPTED,
            Json(json!({ "outcome": "ignored", "reason": "event did not match a trigger rule" })),
        ));
    };

    match hooks::draft_trigger_ticket(&repo_root, &trigger).map_err(ApiError::from)? {
        TriggerDraft::Duplicate { slug } => {
            decision_trigger(
                &trigger,
                "duplicate",
                "a ticket already exists for this trigger",
            );
            Ok((
                StatusCode::ACCEPTED,
                Json(json!({ "outcome": "duplicate", "ticketSlug": slug })),
            ))
        }
        TriggerDraft::Drafted { slug } => {
            // The existing draft pipeline — plan approval intact. Only the
            // queue step is pre-consented (queue label); nothing starts
            // running here: a queued mission still waits for `kranz work`.
            let then_enqueue = trigger.consent.then_enqueue();
            let mission_id = server.host.draft_async(&slug, then_enqueue).await?;
            decision_trigger(
                &trigger,
                "accepted",
                "ticket drafted through the normal pipeline",
            );
            Ok((
                StatusCode::ACCEPTED,
                Json(json!({
                    "outcome": "drafted",
                    "ticketSlug": slug,
                    "missionId": mission_id,
                    "queued": then_enqueue,
                })),
            ))
        }
    }
}

/// The served repository's `owner/repo`, derived from its `origin` remote.
/// `Ok(None)` when there is no origin or it is not a github.com URL — the
/// caller refuses closed.
fn served_full_name(repo_root: &Path) -> Result<Option<String>, ApiError> {
    let repo = GitRepo::open(repo_root)?;
    let url = repo.remote_url("origin")?;
    Ok(url.as_deref().and_then(hooks::github_full_name_from_remote))
}

/// One structured decision line per webhook: source event, actor, consent
/// state, outcome, and a short detail. NEVER the secret, the signature, or
/// the raw body.
fn decision(event: &str, actor: Option<&str>, consent: Option<&str>, outcome: &str, detail: &str) {
    tracing::info!(
        target: "kranz::hooks",
        event = event,
        actor = actor.unwrap_or("-"),
        consent = consent.unwrap_or("-"),
        outcome = outcome,
        detail = detail,
        "github hook decision"
    );
}

/// [`decision`] for a parsed trigger: actor and consent come from the
/// (already bounded/scrubbed) trigger itself.
fn decision_trigger(trigger: &Trigger, outcome: &str, detail: &str) {
    let event = match trigger.kind {
        hooks::TriggerKind::CiFailure => "workflow_run",
        hooks::TriggerKind::PrComment => "pr-comment",
    };
    let consent = match trigger.consent {
        Consent::DraftOnly => "draft-only",
        Consent::PreConsentedQueue => "fix-and-queue",
    };
    decision(event, Some(&trigger.actor), Some(consent), outcome, detail);
}

/// `POST /api/hooks/sentry` — the Sentry issue/alert webhook endpoint
/// (`docs/scoping/signal-intake.md`, `signal-intake-7-first-source`).
///
/// Authenticates via `Sentry-Hook-Signature` (HMAC-SHA256) +
/// `Sentry-Hook-Timestamp` (5-minute skew window) + bounded replay cache.
/// Never drafts, queues, or executes a mission automatically; produces at most
/// an undrafted proposal ticket with `signal-proposal: unconsented`.
pub(crate) async fn sentry_hook(
    State(server): State<Arc<ServerState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let repo_root = server.host.repo_root().clone();
    let signals_cfg = SignalsConfig::load(&repo_root).map_err(ApiError::from)?;
    let sentry_cfg = signals_cfg.sentry.clone().unwrap_or_default();
    let source = SentrySignalSource::new(sentry_cfg);

    let mut header_map = BTreeMap::new();
    for (name, value) in &headers {
        if let Ok(v) = value.to_str() {
            header_map.insert(name.as_str().to_ascii_lowercase(), v.to_string());
        }
    }

    let result = {
        let mut cache = sentry_replay_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        ingest_verified_signal(
            &repo_root,
            &source,
            &header_map,
            &body,
            &signals_cfg,
            chrono::Utc::now(),
            &mut cache,
        )
        .map_err(ApiError::from)?
    };

    match result {
        IngestResult::Dropped { reason } => {
            tracing::info!(
                target: "kranz::hooks",
                source = "sentry",
                reason = reason.as_str(),
                "sentry hook dropped"
            );
            match reason {
                DropReason::SecretNotConfigured => Err(ApiError::forbidden(
                    "sentry hooks are not configured for this repository (no signals.sentry.secret)",
                )),
                DropReason::MissingSignature
                | DropReason::InvalidSignature
                | DropReason::MissingTimestamp
                | DropReason::InvalidTimestamp
                | DropReason::TimestampSkew => Err(ApiError::unauthorized(reason.as_str())),
                DropReason::ReplayedDelivery => Err(ApiError::conflict(reason.as_str())),
                DropReason::BodyTooLarge | DropReason::MalformedPayload => {
                    Err(ApiError::bad_request(reason.as_str()))
                }
                DropReason::UnsupportedEvent => Ok((
                    StatusCode::ACCEPTED,
                    Json(json!({ "outcome": "ignored", "reason": reason.as_str() })),
                )),
            }
        }
        IngestResult::Parked {
            signal_id,
            rule,
            dedupe_count,
        } => Ok((
            StatusCode::ACCEPTED,
            Json(json!({
                "outcome": "parked",
                "signalId": signal_id,
                "rule": rule,
                "dedupeCount": dedupe_count,
            })),
        )),
        IngestResult::NotACodeChange {
            signal_id,
            rule,
            dedupe_count,
        } => Ok((
            StatusCode::ACCEPTED,
            Json(json!({
                "outcome": "not_a_code_change",
                "signalId": signal_id,
                "rule": rule,
                "dedupeCount": dedupe_count,
            })),
        )),
        IngestResult::Proposed(proposal) => Ok((
            StatusCode::ACCEPTED,
            Json(json!({
                "outcome": "proposed",
                "signalId": proposal.signal_id,
                "ticketSlug": proposal.ticket_slug,
                "created": proposal.created,
                "dedupeCount": proposal.dedupe_count,
                "candidateMissions": proposal.candidate_missions,
            })),
        )),
    }
}
