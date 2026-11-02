//! In-process [`SignalSource`] adapter contract, strict field validation,
//! bounded replay cache, GitHub trigger adapter, and conformance stub adapter
//! (`docs/scoping/signal-intake.md` §D-B, ticket `signal-intake-3-source-contract`).

use crate::error::{EngineError, Result};
use crate::hooks::{self, HooksConfig, Trigger};
use crate::signals::ledger::{SignalLedger, MAX_RAW_SIGNAL_BYTES};
use crate::signals::write_back::{WriteBackOutcome, WriteBackRequest};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

/// Maximum clock skew tolerated on a signed signal delivery timestamp (5 minutes).
pub const MAX_TIMESTAMP_SKEW_SECS: i64 = 300;

/// Maximum entries retained in the in-memory bounded replay cache.
pub const DEFAULT_REPLAY_CACHE_CAPACITY: usize = 4_096;

/// Reason an incoming webhook request was dropped before buffering/triage.
/// Contains neither secret material nor payload bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropReason {
    SecretNotConfigured,
    BodyTooLarge,
    MissingSignature,
    InvalidSignature,
    MissingTimestamp,
    InvalidTimestamp,
    TimestampSkew,
    ReplayedDelivery,
    MalformedPayload,
    UnsupportedEvent,
}

impl DropReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            DropReason::SecretNotConfigured => "secret_not_configured",
            DropReason::BodyTooLarge => "body_too_large",
            DropReason::MissingSignature => "missing_signature",
            DropReason::InvalidSignature => "invalid_signature",
            DropReason::MissingTimestamp => "missing_timestamp",
            DropReason::InvalidTimestamp => "invalid_timestamp",
            DropReason::TimestampSkew => "timestamp_skew",
            DropReason::ReplayedDelivery => "replayed_delivery",
            DropReason::MalformedPayload => "malformed_payload",
            DropReason::UnsupportedEvent => "unsupported_event",
        }
    }
}

/// Proof returned by [`SignalSource::verify`] after signature, skew, and replay checks pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDelivery {
    pub source: String,
    pub delivery_id: String,
    pub timestamp: DateTime<Utc>,
}

/// Bounded replay cache of `(source, delivery_id_or_sig_hash, timestamp)`.
#[derive(Debug, Clone)]
pub struct ReplayCache {
    capacity: usize,
    skew_secs: i64,
    entries: VecDeque<(String, String, i64)>,
}

impl Default for ReplayCache {
    fn default() -> Self {
        Self::new(DEFAULT_REPLAY_CACHE_CAPACITY, MAX_TIMESTAMP_SKEW_SECS)
    }
}

impl ReplayCache {
    pub fn new(capacity: usize, skew_secs: i64) -> Self {
        Self {
            capacity: capacity.max(16),
            skew_secs: skew_secs.max(1),
            entries: VecDeque::new(),
        }
    }

    /// Check whether `(source, delivery_key)` is within the skew window and not
    /// already seen; if fresh, insert it and return `Ok(())`.
    pub fn check_and_insert(
        &mut self,
        source: &str,
        delivery_key: &str,
        timestamp: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> std::result::Result<(), DropReason> {
        let now_ts = now.timestamp();
        let msg_ts = timestamp.timestamp();
        if (now_ts - msg_ts).abs() > self.skew_secs {
            return Err(DropReason::TimestampSkew);
        }

        // Evict expired entries outside the skew window.
        let cutoff = now_ts - self.skew_secs;
        self.entries.retain(|(_, _, ts)| *ts >= cutoff);

        if self
            .entries
            .iter()
            .any(|(s, k, _)| s == source && k == delivery_key)
        {
            return Err(DropReason::ReplayedDelivery);
        }

        if self.entries.len() >= self.capacity {
            self.entries.pop_front();
        }
        self.entries
            .push_back((source.to_string(), delivery_key.to_string(), msg_ts));
        Ok(())
    }
}

/// Canonical severity levels allowed outside the untrusted fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignalSeverity {
    Critical,
    High,
    Medium,
    Low,
    Info,
}

impl SignalSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            SignalSeverity::Critical => "critical",
            SignalSeverity::High => "high",
            SignalSeverity::Medium => "medium",
            SignalSeverity::Low => "low",
            SignalSeverity::Info => "info",
        }
    }

    pub fn parse_strict(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "critical" | "fatal" => Some(SignalSeverity::Critical),
            "high" | "error" => Some(SignalSeverity::High),
            "medium" | "warning" | "warn" => Some(SignalSeverity::Medium),
            "low" | "debug" => Some(SignalSeverity::Low),
            "info" | "informational" => Some(SignalSeverity::Info),
            _ => None,
        }
    }
}

/// Normalised signal fields produced by a [`SignalSource`] adapter.
///
/// All non-fenced fields (`source`, `fingerprint`, `severity`, `project`,
/// `environment`, `evidence_links`, `external_id`) are validated against strict
/// character-class allowlists and URL host rules by
/// [`NormalisedSignal::validate_and_sanitise`]; any field that fails validation
/// is either rejected (when identity-bearing: `source`, `fingerprint`) or
/// demoted into `demoted_fields` inside the nonced untrusted fence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NormalisedSignal {
    pub source: String,
    pub fingerprint: String,
    pub severity: SignalSeverity,
    pub project: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_links: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub raw_excerpt: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub demoted_fields: Vec<String>,
}

impl NormalisedSignal {
    /// Enforce the character-class allowlists and URL host rules from
    /// `docs/scoping/signal-intake.md` invariant 2.
    ///
    /// - `source`: `[a-z0-9_-]{1,32}` (hard error if invalid)
    /// - `fingerprint`: `[a-zA-Z0-9._:-]{1,128}` (hard error if invalid)
    /// - `project`: `[a-zA-Z0-9._/-]{1,128}` with no `..` (demoted into
    ///   `demoted_fields` and replaced with `"unvalidated-project"` if invalid)
    /// - `environment`: `[a-zA-Z0-9._-]{1,64}` (demoted into `demoted_fields`
    ///   and cleared to `None` if invalid)
    /// - `external_id`: `[a-zA-Z0-9_-]{1,128}` (demoted into `demoted_fields`
    ///   and cleared to `None` if invalid)
    /// - `evidence_links`: max 4 links × 512 chars, `https://` only, host in
    ///   `allowed_hosts` (case-insensitive exact or subdomain match), no
    ///   userinfo, no whitespace/control chars, no `[`, `]`, `(`, `)` (invalid
    ///   links are demoted into `demoted_fields`).
    pub fn validate_and_sanitise(mut self, allowed_hosts: &[String]) -> Result<Self> {
        if !is_valid_source(&self.source) {
            return Err(EngineError::Config(format!(
                "invalid signal source '{}': must match [a-z0-9_-]{{1,32}}",
                crate::presentation::visible(&self.source)
            )));
        }
        if !is_valid_fingerprint(&self.fingerprint) {
            return Err(EngineError::Config(format!(
                "invalid signal fingerprint '{}': must match [a-zA-Z0-9._:-]{{1,128}}",
                crate::presentation::visible(&self.fingerprint)
            )));
        }

        if !is_valid_project(&self.project) {
            self.demoted_fields
                .push(format!("project: {}", self.project));
            self.project = "unvalidated-project".to_string();
        }

        if let Some(env) = self.environment.take() {
            if is_valid_environment(&env) {
                self.environment = Some(env);
            } else {
                self.demoted_fields.push(format!("environment: {env}"));
            }
        }

        if let Some(ext_id) = self.external_id.take() {
            if is_valid_external_id(&ext_id) {
                self.external_id = Some(ext_id);
            } else {
                self.demoted_fields.push(format!("external_id: {ext_id}"));
            }
        }

        let raw_links = std::mem::take(&mut self.evidence_links);
        for (idx, link) in raw_links.into_iter().enumerate() {
            if idx < 4
                && self.evidence_links.len() < 4
                && is_valid_evidence_link(&link, allowed_hosts)
            {
                self.evidence_links.push(link);
            } else {
                self.demoted_fields.push(format!("evidence_link: {link}"));
            }
        }

        // Sanitise stack-frame paths: keep only safe repo-relative path shapes
        // for path-overlap matching; demote malformed frame strings into the fence.
        let raw_frames = std::mem::take(&mut self.frames);
        for frame in raw_frames {
            if let Some(clean) = normalise_frame_path(&frame) {
                if !self.frames.contains(&clean) {
                    self.frames.push(clean);
                }
            } else {
                self.demoted_fields.push(format!("frame: {frame}"));
            }
        }

        // Sanitise tags (`[a-zA-Z0-9._:-]{1,64}`).
        let raw_tags = std::mem::take(&mut self.tags);
        for tag in raw_tags {
            if !tag.is_empty()
                && tag.len() <= 64
                && tag
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
            {
                self.tags.push(tag);
            } else {
                self.demoted_fields.push(format!("tag: {tag}"));
            }
        }

        Ok(self)
    }

    /// Render the fenced untrusted evidence block (including any demoted fields
    /// and the scrubbed raw excerpt).
    pub fn fenced_evidence_block(&self) -> String {
        let mut combined = String::new();
        if !self.demoted_fields.is_empty() {
            combined.push_str("Demoted unvalidated fields:\n");
            for field in &self.demoted_fields {
                combined.push_str("- ");
                combined.push_str(&crate::scrub::scrub(field));
                combined.push('\n');
            }
            combined.push('\n');
        }
        combined.push_str(&crate::scrub::scrub(&self.raw_excerpt));
        crate::signals::fence_untrusted_signal_evidence(&combined)
    }
}

pub fn is_valid_source(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'))
}

pub fn is_valid_fingerprint(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

pub fn is_valid_project(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && !s.contains("..")
        && !s.starts_with('/')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
}

pub fn is_valid_environment(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn is_valid_external_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

pub fn is_valid_evidence_link(url_str: &str, allowed_hosts: &[String]) -> bool {
    if url_str.is_empty()
        || url_str.len() > 512
        || url_str.contains('\\')
        || url_str.chars().any(|c| {
            c.is_control() || c.is_whitespace() || matches!(c, '[' | ']' | '(' | ')' | '<' | '>')
        })
    {
        return false;
    }
    let Ok(parsed) = reqwest::Url::parse(url_str) else {
        return false;
    };
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    host_matches_allowlist(host, allowed_hosts)
}

pub fn host_matches_allowlist(host: &str, allowed_hosts: &[String]) -> bool {
    let host_lower = host.to_ascii_lowercase();
    allowed_hosts.iter().any(|allowed| {
        let allowed_lower = allowed.trim().to_ascii_lowercase();
        !allowed_lower.is_empty()
            && (host_lower == allowed_lower
                || host_lower
                    .strip_suffix(&allowed_lower)
                    .is_some_and(|prefix| prefix.ends_with('.')))
    })
}

fn normalise_frame_path(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_start_matches("./").trim_start_matches('/');
    if trimmed.is_empty()
        || trimmed.len() > 256
        || trimmed.contains("..")
        || trimmed.contains('\\')
        || trimmed.chars().any(|c| c.is_control())
    {
        return None;
    }
    if trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
    {
        Some(trimmed.to_string())
    } else {
        None
    }
}

/// Look up a header case-insensitively in a header map.
pub fn header_value<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Parse a Unix-seconds or RFC 3339 timestamp header.
pub fn parse_timestamp_header(raw: &str) -> std::result::Result<DateTime<Utc>, DropReason> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(DropReason::MissingTimestamp);
    }
    if let Ok(secs) = trimmed.parse::<i64>() {
        return DateTime::from_timestamp(secs, 0).ok_or(DropReason::InvalidTimestamp);
    }
    DateTime::parse_from_rfc3339(trimmed)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| DropReason::InvalidTimestamp)
}

/// In-process adapter contract for external signal sources (`docs/scoping/signal-intake.md` §D-B).
pub trait SignalSource: Send + Sync {
    /// Stable lowercase identifier for this source (`[a-z0-9_-]{1,32}`).
    fn source_id(&self) -> &'static str;

    /// Verify authenticity, size cap (256 KiB), timestamp skew (5 min), and
    /// replay cache BEFORE any untrusted byte reaches the ledger or triage.
    fn verify(
        &self,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        now: DateTime<Utc>,
        replay_cache: &mut ReplayCache,
    ) -> std::result::Result<VerifiedDelivery, DropReason>;

    /// Normalise a verified payload and enforce field allowlists / demotion.
    fn normalise(
        &self,
        headers: &BTreeMap<String, String>,
        body: &[u8],
    ) -> Result<NormalisedSignal>;

    /// Write outcome status back to the source issue (no-op for sources that do
    /// not configure outbound write-back).
    fn write_back(&self, _req: &WriteBackRequest) -> Result<WriteBackOutcome> {
        Ok(WriteBackOutcome::SkippedNoOp)
    }
}

// ---------------------------------------------------------------------------
// GitHub trigger ported onto SignalSource (decision D-B / D-D)
// ---------------------------------------------------------------------------

/// GitHub webhook trigger adapter implementing [`SignalSource`].
///
/// Preserves the exact behavior of `crates/engine/src/hooks.rs` while also
/// recording verified receipts in the repo-level signal ledger.
pub struct GitHubSignalSource {
    pub config: HooksConfig,
}

impl GitHubSignalSource {
    pub fn new(config: HooksConfig) -> Self {
        Self { config }
    }

    /// Parse the inner [`Trigger`] after verification, keeping full backwards
    /// compatibility with [`hooks::parse_trigger`].
    pub fn parse_github_trigger(
        &self,
        headers: &BTreeMap<String, String>,
        body: &[u8],
    ) -> Result<Option<Trigger>> {
        let event = header_value(headers, "x-github-event").unwrap_or_default();
        let payload: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| EngineError::Config(format!("invalid GitHub webhook JSON: {e}")))?;
        Ok(hooks::parse_trigger(event, &payload, &self.config))
    }

    /// Verify, record in the signal ledger, and draft the trigger ticket (per D-D).
    pub fn receive_and_draft(
        &self,
        repo_root: &Path,
        headers: &BTreeMap<String, String>,
        body: &[u8],
        now: DateTime<Utc>,
        replay_cache: &mut ReplayCache,
    ) -> std::result::Result<Option<(Trigger, hooks::TriggerDraft)>, DropReason> {
        let verified = match self.verify(headers, body, now, replay_cache) {
            Ok(v) => v,
            Err(reason) => {
                let _ = SignalLedger::record_dropped(
                    repo_root,
                    self.source_id(),
                    reason.as_str(),
                    header_value(headers, "x-github-delivery"),
                );
                return Err(reason);
            }
        };
        let payload: serde_json::Value =
            serde_json::from_slice(body).map_err(|_| DropReason::MalformedPayload)?;
        let event = header_value(headers, "x-github-event").unwrap_or_default();
        let Some(trigger) = hooks::parse_trigger(event, &payload, &self.config) else {
            return Ok(None);
        };
        let _ = SignalLedger::record_received(
            repo_root,
            self.source_id(),
            Some(&verified.delivery_id),
            body,
            None,
        );
        let drafted = hooks::draft_trigger_ticket(repo_root, &trigger)
            .map_err(|_| DropReason::MalformedPayload)?;
        Ok(Some((trigger, drafted)))
    }
}

impl SignalSource for GitHubSignalSource {
    fn source_id(&self) -> &'static str {
        "github"
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
        let sig =
            header_value(headers, "x-hub-signature-256").ok_or(DropReason::MissingSignature)?;
        if !hooks::verify_signature(secret, body, Some(sig)) {
            return Err(DropReason::InvalidSignature);
        }
        let timestamp = match header_value(headers, "x-github-timestamp") {
            Some(raw) => parse_timestamp_header(raw)?,
            None => now,
        };
        let delivery_id = header_value(headers, "x-github-delivery")
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
        let event = header_value(headers, "x-github-event").unwrap_or_default();
        let payload: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| EngineError::Config(format!("invalid GitHub JSON: {e}")))?;
        let trigger = hooks::parse_trigger(event, &payload, &self.config).ok_or_else(|| {
            EngineError::Config("event is not an actionable GitHub trigger".into())
        })?;
        let project = hooks::repo_full_name_from_payload(&payload)
            .unwrap_or_else(|| "unknown/repo".to_string());
        let fingerprint = format!("{}:{}", trigger.kind.as_str(), trigger.dedup_id);
        let signal = NormalisedSignal {
            source: self.source_id().to_string(),
            fingerprint,
            severity: SignalSeverity::High,
            project,
            environment: Some("ci".to_string()),
            external_id: Some(trigger.dedup_id.clone()),
            evidence_links: vec![trigger.source_url.clone()],
            frames: Vec::new(),
            tags: vec![trigger.kind.as_str().to_string()],
            raw_excerpt: trigger.excerpt,
            demoted_fields: Vec::new(),
        };
        signal.validate_and_sanitise(&["github.com".to_string()])
    }
}

// ---------------------------------------------------------------------------
// Domain-free StubSignalSource for conformance and CI injection tests
// ---------------------------------------------------------------------------

/// Stub adapter used by the `SignalSource` conformance suite and the CI
/// injection corpus (`signal-intake-3-source-contract`, `signal-intake-6-injection-suite`).
pub struct StubSignalSource {
    pub secret: Option<String>,
    pub allowed_hosts: Vec<String>,
}

impl StubSignalSource {
    pub fn new(secret: Option<String>, allowed_hosts: Vec<String>) -> Self {
        Self {
            secret,
            allowed_hosts,
        }
    }

    pub fn sign_body(secret: &str, timestamp: i64, body: &[u8]) -> String {
        let mut msg = format!("{timestamp}.").into_bytes();
        msg.extend_from_slice(body);
        format!("sha256={}", hooks::hmac_sha256_hex(secret.as_bytes(), &msg))
    }
}

impl SignalSource for StubSignalSource {
    fn source_id(&self) -> &'static str {
        "stub"
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
            .secret
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or(DropReason::SecretNotConfigured)?;
        let ts_raw =
            header_value(headers, "x-signal-timestamp").ok_or(DropReason::MissingTimestamp)?;
        let timestamp = parse_timestamp_header(ts_raw)?;
        let sig =
            header_value(headers, "x-signal-signature").ok_or(DropReason::MissingSignature)?;

        let mut signed_msg = format!("{}.", timestamp.timestamp()).into_bytes();
        signed_msg.extend_from_slice(body);
        if !hooks::verify_signature(secret, &signed_msg, Some(sig)) {
            return Err(DropReason::InvalidSignature);
        }

        let delivery_id = header_value(headers, "x-signal-delivery")
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
        _headers: &BTreeMap<String, String>,
        body: &[u8],
    ) -> Result<NormalisedSignal> {
        let payload: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| EngineError::Config(format!("invalid stub signal JSON: {e}")))?;
        let fingerprint = payload
            .get("fingerprint")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let project = payload
            .get("project")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let severity_raw = payload
            .get("severity")
            .and_then(|v| v.as_str())
            .unwrap_or("high");
        let mut demoted_fields = Vec::new();
        let severity = match SignalSeverity::parse_strict(severity_raw) {
            Some(s) => s,
            None => {
                demoted_fields.push(format!("severity: {severity_raw}"));
                SignalSeverity::Medium
            }
        };
        let environment = payload
            .get("environment")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let external_id = payload
            .get("externalId")
            .or_else(|| payload.get("external_id"))
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let evidence_links = payload
            .get("links")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let frames = payload
            .get("frames")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let tags = payload
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let raw_excerpt = payload
            .get("message")
            .or_else(|| payload.get("excerpt"))
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        let signal = NormalisedSignal {
            source: self.source_id().to_string(),
            fingerprint,
            severity,
            project,
            environment,
            external_id,
            evidence_links,
            frames,
            tags,
            raw_excerpt,
            demoted_fields,
        };
        signal.validate_and_sanitise(&self.allowed_hosts)
    }
}
