//! Idempotent, egress-pinned status write-back to the external signal source
//! (`docs/scoping/signal-intake.md` §D-G, ticket `signal-intake-9-write-back`).
//!
//! Guarantees:
//! 1. Every outbound write-back is preceded by a durable `WriteBackPending`
//!    ledger entry and followed by `WriteBackConfirmed` (or `WriteBackFailed`).
//! 2. Outbound URLs are constructed strictly from the operator-configured
//!    `base_url` plus a validated `[a-zA-Z0-9_-]{1,128}` `external_id` —
//!    payload-supplied callback URLs are never followed.
//! 3. Uses `reqwest::redirect::Policy::none()` and `.no_proxy()`, refusing any
//!    `3xx` redirect response and verifying that the destination host matches
//!    the configured vendor `allowed_hosts`.
//! 4. Posts status and reference links only (never diffs, reports, or
//!    transcripts); `Failed` and `Declined` never resolve the source issue.
//! 5. Crash-safe idempotency: if a process dies (`kill -9`) after the vendor
//!    accepts the write but before `WriteBackConfirmed` is appended,
//!    [`resume_pending_write_backs`] queries the remote issue state for the
//!    deterministic `idempotency_key` before retrying, preventing double-posts.

use crate::error::{EngineError, Result};
use crate::signals::ledger::{SignalLedger, SignalRecordKind};
use crate::signals::source::{host_matches_allowlist, is_valid_external_id};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

/// Pluggable transport for deterministic write-back testing in environments
/// where loopback sockets are blocked by an outer host sandbox.
pub trait WriteBackTransport: Send + Sync {
    fn check_already_applied(
        &self,
        target_url: &reqwest::Url,
        idempotency_key: &str,
    ) -> Result<bool>;

    fn post_status(
        &self,
        target_url: &reqwest::Url,
        idempotency_key: &str,
        body: &serde_json::Value,
    ) -> Result<u16>;
}

/// Lifecycle milestones written back to the signal source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WriteBackStatus {
    ProposalCreated,
    Completed,
    Merged,
    Failed,
    Declined,
}

impl WriteBackStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            WriteBackStatus::ProposalCreated => "proposal_created",
            WriteBackStatus::Completed => "completed",
            WriteBackStatus::Merged => "merged",
            WriteBackStatus::Failed => "failed",
            WriteBackStatus::Declined => "declined",
        }
    }

    /// Only `Completed` and `Merged` may resolve the upstream issue; a failure,
    /// decline, or initial proposal never resolves it (`docs/scoping/signal-intake.md` §D-G).
    pub fn resolves_issue(self) -> bool {
        matches!(self, WriteBackStatus::Completed | WriteBackStatus::Merged)
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "proposal_created" => Some(WriteBackStatus::ProposalCreated),
            "completed" => Some(WriteBackStatus::Completed),
            "merged" => Some(WriteBackStatus::Merged),
            "failed" => Some(WriteBackStatus::Failed),
            "declined" => Some(WriteBackStatus::Declined),
            _ => None,
        }
    }
}

/// Parameters for one status write-back operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteBackRequest {
    pub signal_id: String,
    pub ticket_slug: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_id: Option<String>,
    pub external_id: String,
    pub status: WriteBackStatus,
    /// Short branch or dashboard reference (never diff, report, or transcript text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Optional payload-supplied callback URL — if a caller ever sets this,
    /// [`WriteBackClient`] explicitly refuses the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_callback_url: Option<String>,
}

impl WriteBackRequest {
    pub fn idempotency_key(&self) -> String {
        format!("{}-{}", self.signal_id, self.status.as_str())
    }
}

/// Outcome of a write-back execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WriteBackOutcome {
    Confirmed {
        idempotency_key: String,
        target_url: String,
        resolved_issue: bool,
    },
    AlreadyConfirmed {
        idempotency_key: String,
    },
    Failed {
        idempotency_key: String,
        error: String,
    },
    SkippedNoOp,
}

/// Egress-pinned HTTP client for vendor write-back.
#[derive(Clone)]
pub struct WriteBackClient {
    base_url: reqwest::Url,
    allowed_hosts: Vec<String>,
    bearer_token: String,
    transport: Option<Arc<dyn WriteBackTransport>>,
}

impl WriteBackClient {
    pub fn new(base_url: &str, allowed_hosts: &[String], bearer_token: String) -> Result<Self> {
        if base_url.contains('\\') {
            return Err(EngineError::Config(
                "write-back base_url must not contain backslashes".into(),
            ));
        }
        let parsed = reqwest::Url::parse(base_url).map_err(|e| {
            EngineError::Config(format!("invalid write-back base_url '{base_url}': {e}"))
        })?;
        if !matches!(parsed.scheme(), "https" | "http") {
            return Err(EngineError::Config(format!(
                "unsupported write-back scheme '{}'",
                parsed.scheme()
            )));
        }
        if !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(EngineError::Config(
                "write-back base_url must not contain credentials, query, or fragment".into(),
            ));
        }
        let host = parsed
            .host_str()
            .ok_or_else(|| EngineError::Config("write-back base_url is missing a host".into()))?;
        if !host_matches_allowlist(host, allowed_hosts) {
            return Err(EngineError::Config(format!(
                "write-back base_url host '{host}' is not in allowed_hosts {:?}",
                allowed_hosts
            )));
        }
        if parsed.scheme() == "http" && !matches!(host, "127.0.0.1" | "localhost" | "::1") {
            return Err(EngineError::Config(format!(
                "non-loopback write-back base_url '{base_url}' must use https://"
            )));
        }
        Ok(Self {
            base_url: parsed,
            allowed_hosts: allowed_hosts.to_vec(),
            bearer_token,
            transport: None,
        })
    }

    pub fn with_transport(mut self, transport: Arc<dyn WriteBackTransport>) -> Self {
        self.transport = Some(transport);
        self
    }

    /// Construct the pinned issue endpoint URL from `base_url` + validated `external_id`.
    pub fn build_issue_url(&self, req: &WriteBackRequest) -> Result<reqwest::Url> {
        if req.payload_callback_url.is_some() {
            return Err(EngineError::Config(
                "refusing payload-supplied callback URL in write-back request".into(),
            ));
        }
        if !is_valid_external_id(&req.external_id) {
            return Err(EngineError::Config(format!(
                "invalid write-back external_id '{}': must match [a-zA-Z0-9_-]{{1,128}}",
                crate::presentation::visible(&req.external_id)
            )));
        }
        let mut url = self.base_url.clone();
        let base_path = url.path().trim_end_matches('/');
        let new_path = format!("{base_path}/api/0/issues/{}/", req.external_id);
        url.set_path(&new_path);
        let host = url.host_str().unwrap_or_default();
        if !host_matches_allowlist(host, &self.allowed_hosts) {
            return Err(EngineError::Config(format!(
                "write-back target host '{host}' does not match allowed_hosts"
            )));
        }
        Ok(url)
    }

    fn build_http_client() -> Result<reqwest::Client> {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| EngineError::Other(format!("failed to build write-back HTTP client: {e}")))
    }

    /// Query the remote issue endpoint to check if `idempotency_key` was already
    /// applied before a crash (`kill -9` between POST and local ledger confirmation).
    pub async fn check_already_applied(
        &self,
        target_url: &reqwest::Url,
        idempotency_key: &str,
    ) -> Result<bool> {
        if let Some(ref transport) = self.transport {
            return transport.check_already_applied(target_url, idempotency_key);
        }
        let client = Self::build_http_client()?;
        let mut check_url = target_url.clone();
        check_url.set_query(Some(&format!("idempotencyKey={idempotency_key}")));
        let mut req_builder = client
            .get(check_url)
            .header("Idempotency-Key", idempotency_key);
        if !self.bearer_token.is_empty() {
            req_builder = req_builder.bearer_auth(&self.bearer_token);
        }
        let Ok(resp) = req_builder.send().await else {
            return Ok(false);
        };
        if resp.status().is_redirection() {
            return Err(EngineError::InvalidState(format!(
                "write-back precheck refused HTTP redirect ({})",
                resp.status()
            )));
        }
        if !resp.status().is_success() {
            return Ok(false);
        }
        let text = resp.text().await.unwrap_or_default();
        Ok(text.contains(idempotency_key))
    }

    /// Send the status write-back over HTTP with redirect and proxy disabled.
    pub async fn send(&self, req: &WriteBackRequest) -> Result<WriteBackOutcome> {
        let target_url = self.build_issue_url(req)?;
        let idempotency_key = req.idempotency_key();
        let resolves = req.status.resolves_issue();
        let issue_state = if resolves { "resolved" } else { "unresolved" };
        let reference = req
            .reference
            .as_deref()
            .map(crate::presentation::visible)
            .unwrap_or_default();

        // Status and links only: no diffs, reports, or transcripts.
        let body = json!({
            "status": issue_state,
            "kranzStatus": req.status.as_str(),
            "signalId": req.signal_id,
            "ticketSlug": req.ticket_slug,
            "missionId": req.mission_id,
            "reference": reference,
            "idempotencyKey": idempotency_key,
            "note": format!(
                "kranz signal {} status={} ticket={} mission={} ref={} [kranz-idempotency:{}]",
                req.signal_id,
                req.status.as_str(),
                req.ticket_slug,
                req.mission_id.as_deref().unwrap_or("none"),
                reference,
                idempotency_key
            )
        });

        if let Some(ref transport) = self.transport {
            let status = transport.post_status(&target_url, &idempotency_key, &body)?;
            if (300..400).contains(&status) {
                return Err(EngineError::InvalidState(format!(
                    "write-back refused HTTP redirect status {status}"
                )));
            }
            if !(200..300).contains(&status) {
                return Err(EngineError::Other(format!(
                    "write-back HTTP error status {status}"
                )));
            }
            return Ok(WriteBackOutcome::Confirmed {
                idempotency_key,
                target_url: target_url.to_string(),
                resolved_issue: resolves,
            });
        }

        let client = Self::build_http_client()?;
        let mut http_req = client
            .post(target_url.clone())
            .header("Idempotency-Key", &idempotency_key)
            .json(&body);
        if !self.bearer_token.is_empty() {
            http_req = http_req.bearer_auth(&self.bearer_token);
        }

        let resp = http_req
            .send()
            .await
            .map_err(|e| EngineError::Other(format!("write-back HTTP request failed: {e}")))?;
        let status = resp.status();
        if status.is_redirection() {
            return Err(EngineError::InvalidState(format!(
                "write-back refused HTTP redirect status {status}"
            )));
        }
        if !status.is_success() {
            return Err(EngineError::Other(format!(
                "write-back HTTP error status {status}"
            )));
        }

        Ok(WriteBackOutcome::Confirmed {
            idempotency_key,
            target_url: target_url.to_string(),
            resolved_issue: resolves,
        })
    }

    /// Synchronous wrapper around [`Self::send`] safe to call from inside or
    /// outside a Tokio runtime.
    pub fn send_blocking(&self, req: &WriteBackRequest) -> Result<WriteBackOutcome> {
        let client = self.clone();
        let req = req.clone();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| EngineError::Other(e.to_string()))?;
            rt.block_on(client.send(&req))
        })
        .join()
        .map_err(|_| EngineError::Other("write-back worker thread panicked".into()))?
    }
}

/// Execute an idempotent write-back recorded in `.kranz/signals/ledger.jsonl`.
///
/// Never blocks the caller on network/vendor errors: if the HTTP write fails,
/// a `WriteBackFailed` record is appended and `Ok(WriteBackOutcome::Failed { .. })`
/// is returned so the mission can complete normally and retry on resume.
pub async fn execute_write_back(
    repo_root: &Path,
    client: &WriteBackClient,
    req: &WriteBackRequest,
) -> Result<WriteBackOutcome> {
    // Validate URL and external_id up front (hard config errors fail closed).
    let target_url = client.build_issue_url(req)?;
    let idempotency_key = req.idempotency_key();

    let entries = SignalLedger::read_entries(repo_root)?;
    let already_confirmed = entries.iter().any(|e| {
        matches!(
            &e.record,
            SignalRecordKind::WriteBackConfirmed { idempotency_key: k, .. } if k == &idempotency_key
        )
    });
    if already_confirmed {
        return Ok(WriteBackOutcome::AlreadyConfirmed { idempotency_key });
    }

    let had_pending = entries.iter().any(|e| {
        matches!(
            &e.record,
            SignalRecordKind::WriteBackPending { idempotency_key: k, .. } if k == &idempotency_key
        )
    });

    if had_pending {
        // A prior process recorded WriteBackPending before crashing or failing.
        // Check whether the vendor already applied this idempotency_key!
        if client
            .check_already_applied(&target_url, &idempotency_key)
            .await
            .unwrap_or(false)
        {
            SignalLedger::append(
                repo_root,
                SignalRecordKind::WriteBackConfirmed {
                    signal_id: req.signal_id.clone(),
                    ticket_slug: req.ticket_slug.clone(),
                    mission_id: req.mission_id.clone(),
                    status: req.status.as_str().to_string(),
                    idempotency_key: idempotency_key.clone(),
                    external_id: req.external_id.clone(),
                },
            )?;
            return Ok(WriteBackOutcome::Confirmed {
                idempotency_key,
                target_url: target_url.to_string(),
                resolved_issue: req.status.resolves_issue(),
            });
        }
    } else {
        SignalLedger::append(
            repo_root,
            SignalRecordKind::WriteBackPending {
                signal_id: req.signal_id.clone(),
                ticket_slug: req.ticket_slug.clone(),
                mission_id: req.mission_id.clone(),
                status: req.status.as_str().to_string(),
                idempotency_key: idempotency_key.clone(),
                target_url: target_url.to_string(),
                external_id: req.external_id.clone(),
            },
        )?;
    }

    match client.send(req).await {
        Ok(outcome) => {
            SignalLedger::append(
                repo_root,
                SignalRecordKind::WriteBackConfirmed {
                    signal_id: req.signal_id.clone(),
                    ticket_slug: req.ticket_slug.clone(),
                    mission_id: req.mission_id.clone(),
                    status: req.status.as_str().to_string(),
                    idempotency_key: idempotency_key.clone(),
                    external_id: req.external_id.clone(),
                },
            )?;
            Ok(outcome)
        }
        Err(err) => {
            let error_msg = crate::scrub::scrub(&err.to_string());
            SignalLedger::append(
                repo_root,
                SignalRecordKind::WriteBackFailed {
                    signal_id: req.signal_id.clone(),
                    ticket_slug: req.ticket_slug.clone(),
                    mission_id: req.mission_id.clone(),
                    status: req.status.as_str().to_string(),
                    idempotency_key: idempotency_key.clone(),
                    external_id: req.external_id.clone(),
                    error: error_msg.clone(),
                },
            )?;
            Ok(WriteBackOutcome::Failed {
                idempotency_key,
                error: error_msg,
            })
        }
    }
}

/// Resume and retry all pending (unconfirmed) write-backs in `.kranz/signals/ledger.jsonl`.
pub async fn resume_pending_write_backs(
    repo_root: &Path,
    client: &WriteBackClient,
) -> Result<Vec<WriteBackOutcome>> {
    let entries = SignalLedger::read_entries(repo_root)?;
    let confirmed_keys: std::collections::BTreeSet<String> = entries
        .iter()
        .filter_map(|e| match &e.record {
            SignalRecordKind::WriteBackConfirmed {
                idempotency_key, ..
            } => Some(idempotency_key.clone()),
            _ => None,
        })
        .collect();

    let mut pending_requests: Vec<WriteBackRequest> = Vec::new();
    let mut seen_keys = std::collections::BTreeSet::new();

    for entry in &entries {
        if let SignalRecordKind::WriteBackPending {
            signal_id,
            ticket_slug,
            mission_id,
            status,
            idempotency_key,
            external_id,
            ..
        } = &entry.record
        {
            if !confirmed_keys.contains(idempotency_key)
                && seen_keys.insert(idempotency_key.clone())
            {
                if let Some(parsed_status) = WriteBackStatus::parse(status) {
                    pending_requests.push(WriteBackRequest {
                        signal_id: signal_id.clone(),
                        ticket_slug: ticket_slug.clone(),
                        mission_id: mission_id.clone(),
                        external_id: external_id.clone(),
                        status: parsed_status,
                        reference: None,
                        payload_callback_url: None,
                    });
                }
            }
        }
    }

    let mut outcomes = Vec::new();
    for req in pending_requests {
        outcomes.push(execute_write_back(repo_root, client, &req).await?);
    }
    Ok(outcomes)
}
