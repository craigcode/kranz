//! Sentry Issue Alert / Event Alert / Metric Alert webhook adapter
//! (`docs/scoping/signal-intake.md` §D-C, ticket `signal-intake-7-first-source`).
//!
//! Built against Sentry's Integration Platform Webhooks specification
//! (<https://docs.sentry.io/organization/integrations/integration-platform/webhooks/>
//! and <https://docs.sentry.io/organization/integrations/integration-platform/webhooks/issue-alerts/>,
//! verified 2026-10):
//! - Signature header: `Sentry-Hook-Signature` = lowercase hex `HMAC-SHA256(client_secret, raw_body)`
//!   verified in constant time.
//! - Timestamp header: `Sentry-Hook-Timestamp` (Unix epoch seconds or RFC 3339),
//!   checked against the 5-minute (`300s`) skew window.
//! - Resource header: `Sentry-Hook-Resource` (`issue`, `event_alert`, `metric_alert`).
//! - Replay protection: `(source, request_id_or_sig_hash, timestamp)` in [`ReplayCache`].
//! - Domain-free: no project names or repository names in core.

use crate::error::{EngineError, Result};
use crate::hooks;
use crate::signals::ledger::MAX_RAW_SIGNAL_BYTES;
use crate::signals::source::{
    header_value, parse_timestamp_header, DropReason, NormalisedSignal, ReplayCache,
    SignalSeverity, SignalSource, VerifiedDelivery,
};
use crate::signals::write_back::{WriteBackClient, WriteBackOutcome, WriteBackRequest};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use subtle::ConstantTimeEq as _;

/// Operator configuration for the Sentry signal adapter (`signals.sentry` in
/// `.kranz/config.json` / `~/.kranz/config.json`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentryConfig {
    /// Client secret used to verify `Sentry-Hook-Signature`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// Allowed hostnames for Sentry evidence links and write-back (defaults to `["sentry.io"]`).
    #[serde(default = "default_sentry_hosts")]
    pub allowed_hosts: Vec<String>,
    /// Optional base URL for outbound status write-back (e.g. `https://sentry.io`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,
    /// Optional environment variable name holding the Sentry API bearer token for write-back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_env: Option<String>,
}

fn default_sentry_hosts() -> Vec<String> {
    vec!["sentry.io".to_string()]
}

impl Default for SentryConfig {
    fn default() -> Self {
        Self {
            secret: None,
            allowed_hosts: default_sentry_hosts(),
            api_base_url: None,
            token_env: None,
        }
    }
}

impl std::fmt::Debug for SentryConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SentryConfig")
            .field("secret", &self.secret.as_ref().map(|_| "[REDACTED]"))
            .field("allowed_hosts", &self.allowed_hosts)
            .field("api_base_url", &self.api_base_url)
            .field("token_env", &self.token_env)
            .finish()
    }
}

/// Sentry webhook adapter implementing [`SignalSource`].
pub struct SentrySignalSource {
    pub config: SentryConfig,
}

impl SentrySignalSource {
    pub fn new(config: SentryConfig) -> Self {
        Self { config }
    }

    /// Compute the expected `Sentry-Hook-Signature` hex digest over `raw_body`.
    pub fn sign_body(secret: &str, raw_body: &[u8]) -> String {
        hooks::hmac_sha256_hex(secret.as_bytes(), raw_body)
    }

    pub fn verify_sentry_signature(secret: &str, raw_body: &[u8], header_sig: &str) -> bool {
        let trimmed = header_sig.trim().trim_start_matches("sha256=");
        if trimmed.len() != 64 || !trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
        let expected = Self::sign_body(secret, raw_body);
        let candidate = trimmed.to_ascii_lowercase();
        candidate.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() == 1
    }
}

impl SignalSource for SentrySignalSource {
    fn source_id(&self) -> &'static str {
        "sentry"
    }

    fn verify(
        &self,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        now: DateTime<Utc>,
        replay_cache: &mut ReplayCache,
    ) -> std::result::Result<VerifiedDelivery, DropReason> {
        if body.len() > MAX_RAW_SIGNAL_BYTES {
            return Err(DropReason::BodyTooLarge);
        }
        let secret = self
            .config
            .secret
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(DropReason::SecretNotConfigured)?;

        let resource = header_value(headers, "sentry-hook-resource").unwrap_or("issue");
        if !matches!(resource, "issue" | "event_alert" | "metric_alert" | "error") {
            return Err(DropReason::UnsupportedEvent);
        }

        let ts_raw =
            header_value(headers, "sentry-hook-timestamp").ok_or(DropReason::MissingTimestamp)?;
        let timestamp = parse_timestamp_header(ts_raw)?;

        let sig =
            header_value(headers, "sentry-hook-signature").ok_or(DropReason::MissingSignature)?;
        if !Self::verify_sentry_signature(secret, body, sig) {
            return Err(DropReason::InvalidSignature);
        }

        let delivery_id = header_value(headers, "request-id")
            .or_else(|| header_value(headers, "x-sentry-delivery"))
            .map(str::to_string)
            .unwrap_or_else(|| crate::standards_waiver::sha256_hex(sig.as_bytes()));

        replay_cache.check_and_insert(self.source_id(), &delivery_id, timestamp, now)?;

        Ok(VerifiedDelivery {
            source: self.source_id().to_string(),
            delivery_id,
            timestamp,
        })
    }

    fn normalise(
        &self,
        headers: &BTreeMap<String, String>,
        body: &[u8],
    ) -> Result<NormalisedSignal> {
        let resource = header_value(headers, "sentry-hook-resource").unwrap_or("issue");
        let payload: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| EngineError::Config(format!("invalid Sentry JSON payload: {e}")))?;

        let action = payload
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("triggered");
        if !matches!(
            (resource, action),
            ("issue", "created" | "unresolved" | "assigned" | "triggered")
                | ("event_alert", "triggered")
                | ("metric_alert", "critical" | "warning" | "triggered")
                | ("error", "created" | "triggered")
        ) {
            return Err(EngineError::Config(format!(
                "unsupported Sentry resource/action pair '{resource}/{action}'"
            )));
        }

        let data = payload.get("data").unwrap_or(&payload);
        let issue = data
            .get("issue")
            .or_else(|| data.get("event"))
            .or_else(|| data.get("metric_alert"))
            .or_else(|| data.get("error"))
            .unwrap_or(data);

        // Fingerprint MUST be the vendor's issue or incident identity, never message text.
        let external_id = issue
            .get("id")
            .and_then(value_id_string)
            .or_else(|| {
                data.get("event")
                    .and_then(|e| e.get("issue_id"))
                    .and_then(value_id_string)
            })
            .or_else(|| issue.get("shortId").and_then(value_id_string))
            .ok_or_else(|| {
                EngineError::Config("Sentry payload missing issue/incident id".into())
            })?;
        let fingerprint = issue
            .get("shortId")
            .and_then(value_id_string)
            .unwrap_or_else(|| format!("sentry-issue-{external_id}"));

        let project = issue
            .get("project")
            .and_then(|p| {
                p.get("slug")
                    .and_then(|v| v.as_str())
                    .or_else(|| p.as_str())
            })
            .or_else(|| {
                data.get("event")
                    .and_then(|e| e.get("project"))
                    .and_then(value_id_string_ref)
            })
            .or_else(|| payload.get("project_slug").and_then(|v| v.as_str()))
            .or_else(|| payload.get("project").and_then(|v| v.as_str()))
            .unwrap_or("unknown")
            .to_string();

        let level_raw = issue
            .get("level")
            .and_then(|v| v.as_str())
            .or_else(|| {
                data.get("event")
                    .and_then(|e| e.get("level"))
                    .and_then(|v| v.as_str())
            })
            .unwrap_or(action);
        let mut demoted_fields = Vec::new();
        let severity = match SignalSeverity::parse_strict(level_raw) {
            Some(s) => s,
            None => {
                if level_raw != "created" && level_raw != "triggered" && level_raw != "unresolved" {
                    demoted_fields.push(format!("severity: {level_raw}"));
                }
                SignalSeverity::High
            }
        };

        let environment = extract_sentry_environment(data, issue);
        let evidence_links = extract_sentry_links(data, issue);
        let frames = extract_sentry_frames(data, issue);
        let tags = extract_sentry_tags(data, issue, resource);
        let raw_excerpt = build_sentry_excerpt(resource, action, data, issue);

        let allowed_hosts = if self.config.allowed_hosts.is_empty() {
            default_sentry_hosts()
        } else {
            self.config.allowed_hosts.clone()
        };

        let signal = NormalisedSignal {
            source: self.source_id().to_string(),
            fingerprint,
            severity,
            project,
            environment,
            external_id: Some(external_id),
            evidence_links,
            frames,
            tags,
            raw_excerpt,
            demoted_fields,
        };
        signal.validate_and_sanitise(&allowed_hosts)
    }

    fn write_back(&self, req: &WriteBackRequest) -> Result<WriteBackOutcome> {
        let Some(base_url) = self.config.api_base_url.as_deref() else {
            return Ok(WriteBackOutcome::SkippedNoOp);
        };
        let token = self
            .config
            .token_env
            .as_deref()
            .and_then(|env_name| std::env::var(env_name).ok())
            .unwrap_or_default();
        let client = WriteBackClient::new(base_url, &self.config.allowed_hosts, token)?;
        client.send_blocking(req)
    }
}

fn value_id_string(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    if let Some(n) = v.as_u64() {
        return Some(n.to_string());
    }
    if let Some(n) = v.as_i64() {
        return Some(n.to_string());
    }
    None
}

fn value_id_string_ref(v: &serde_json::Value) -> Option<&str> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn extract_sentry_environment(
    data: &serde_json::Value,
    issue: &serde_json::Value,
) -> Option<String> {
    if let Some(env) = data
        .get("event")
        .and_then(|e| e.get("environment"))
        .and_then(|v| v.as_str())
        .or_else(|| issue.get("environment").and_then(|v| v.as_str()))
    {
        return Some(env.to_string());
    }
    for container in [data.get("event"), Some(issue)] {
        if let Some(tags) = container
            .and_then(|c| c.get("tags"))
            .and_then(|v| v.as_array())
        {
            for tag in tags {
                if let Some(arr) = tag.as_array() {
                    if arr.len() == 2 && arr[0].as_str() == Some("environment") {
                        if let Some(val) = arr[1].as_str() {
                            return Some(val.to_string());
                        }
                    }
                } else if tag.get("key").and_then(|k| k.as_str()) == Some("environment") {
                    if let Some(val) = tag.get("value").and_then(|v| v.as_str()) {
                        return Some(val.to_string());
                    }
                }
            }
        }
    }
    None
}

fn extract_sentry_links(data: &serde_json::Value, issue: &serde_json::Value) -> Vec<String> {
    let mut links = Vec::new();
    for candidate in [
        issue.get("permalink").and_then(|v| v.as_str()),
        issue.get("web_url").and_then(|v| v.as_str()),
        data.get("event")
            .and_then(|e| e.get("web_url"))
            .and_then(|v| v.as_str()),
        data.get("web_url").and_then(|v| v.as_str()),
    ]
    .into_iter()
    .flatten()
    {
        if !links.iter().any(|l| l == candidate) {
            links.push(candidate.to_string());
        }
    }
    links
}

fn extract_sentry_frames(data: &serde_json::Value, issue: &serde_json::Value) -> Vec<String> {
    let mut frames = Vec::new();
    for event_obj in [data.get("event"), Some(issue)].into_iter().flatten() {
        if let Some(values) = event_obj
            .get("exception")
            .and_then(|ex| ex.get("values"))
            .and_then(|v| v.as_array())
        {
            for ex in values {
                if let Some(frame_arr) = ex
                    .get("stacktrace")
                    .and_then(|st| st.get("frames"))
                    .and_then(|f| f.as_array())
                {
                    for frame in frame_arr {
                        if let Some(filename) = frame.get("filename").and_then(|v| v.as_str()) {
                            frames.push(filename.to_string());
                        }
                    }
                }
            }
        }
        if let Some(filename) = event_obj
            .get("metadata")
            .and_then(|m| m.get("filename"))
            .and_then(|v| v.as_str())
        {
            frames.push(filename.to_string());
        }
    }
    frames
}

fn extract_sentry_tags(
    data: &serde_json::Value,
    issue: &serde_json::Value,
    resource: &str,
) -> Vec<String> {
    let mut tags = vec![format!("resource:{resource}")];
    for container in [data.get("event"), Some(issue)].into_iter().flatten() {
        if let Some(arr) = container.get("tags").and_then(|v| v.as_array()) {
            for item in arr {
                if let Some(pair) = item.as_array() {
                    if pair.len() == 2 {
                        if let (Some(k), Some(v)) = (pair[0].as_str(), pair[1].as_str()) {
                            tags.push(format!("{k}:{v}"));
                        }
                    }
                } else if let (Some(k), Some(v)) = (
                    item.get("key").and_then(|k| k.as_str()),
                    item.get("value").and_then(|v| v.as_str()),
                ) {
                    tags.push(format!("{k}:{v}"));
                }
            }
        }
    }
    tags
}

fn build_sentry_excerpt(
    resource: &str,
    action: &str,
    data: &serde_json::Value,
    issue: &serde_json::Value,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!("resource: {resource} ({action})"));
    if let Some(title) = issue.get("title").and_then(|v| v.as_str()).or_else(|| {
        data.get("event")
            .and_then(|e| e.get("title"))
            .and_then(|v| v.as_str())
    }) {
        lines.push(format!("title: {title}"));
    }
    if let Some(culprit) = issue.get("culprit").and_then(|v| v.as_str()).or_else(|| {
        data.get("event")
            .and_then(|e| e.get("culprit"))
            .and_then(|v| v.as_str())
    }) {
        lines.push(format!("culprit: {culprit}"));
    }
    if let Some(meta) = issue.get("metadata") {
        if let Some(ty) = meta.get("type").and_then(|v| v.as_str()) {
            lines.push(format!("exception_type: {ty}"));
        }
        if let Some(val) = meta.get("value").and_then(|v| v.as_str()) {
            lines.push(format!("exception_value: {val}"));
        }
    }
    if let Some(rule) = data.get("triggered_rule").and_then(|v| v.as_str()) {
        lines.push(format!("triggered_rule: {rule}"));
    }
    let joined = lines.join("\n");
    let scrubbed = crate::scrub::scrub(&joined);
    if scrubbed.chars().count() > 1000 {
        let mut t: String = scrubbed.chars().take(1000).collect();
        t.push_str(" … (truncated)");
        t
    } else {
        scrubbed
    }
}
