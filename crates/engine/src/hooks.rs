//! External trigger core (design D-F, ticket `trigger-ci-pr-fix-mission`):
//! parse an authenticated GitHub webhook into a ticket-draft decision, dedupe
//! by trigger identity, and write the provenance-carrying ticket.
//!
//! D-F's rule is that external triggers create AUDITED work, never prompt
//! loops. Everything this module produces enters the normal ticket pipeline —
//! draft → plan approval → queue → `kranz work` — so spend gates and plan
//! approval stay exactly where they are. The module performs no git
//! operations at all: its only action set is [`TRIGGER_ACTIONS`] (draft, or
//! draft+queue when the operator pre-consented via the queue label), and a
//! regression test scans this file to keep it free of any git-mutation path.
//!
//! The webhook HTTP surface lives in `kranz-server` (`POST
//! /api/hooks/github`); this module is the pure, surface-agnostic core so any
//! surface can drive the same decisions.

use crate::error::{EngineError, Result};
use crate::ticket::Ticket;
use crate::{config, paths, scrub};
use serde::Deserialize;
use serde_json::Value;
use std::fmt;
use std::io::ErrorKind;
use std::path::Path;

// ---------------------------------------------------------------------------
// Configuration (`hooks` key of the layered config files)
// ---------------------------------------------------------------------------

/// Default comment label that drafts a fix ticket.
pub const DEFAULT_FIX_LABEL: &str = "kranz:fix";
/// Default comment label that additionally pre-consents to queueing the
/// drafted plan (plan approval itself is never skipped).
pub const DEFAULT_QUEUE_LABEL: &str = "kranz:fix-and-queue";

/// Webhook trigger configuration — the `hooks` key of `.kranz/config.json`
/// (additive; absent ⇒ the route refuses closed, never open-accepts).
///
/// Loaded separately from [`crate::types::MissionConfig`] on purpose: the
/// secret must never ride into a `mission.created` event's serialized config
/// or any other log, so this type has NO `Serialize` impl and a redacting
/// `Debug`. `Debug`/`tracing` output shows `[REDACTED]` for the secret.
#[derive(Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HooksConfig {
    /// Per-repo HMAC secret for `X-Hub-Signature-256` verification.
    pub secret: Option<String>,
    /// Comment label that drafts a fix ticket (default `kranz:fix`).
    pub fix_label: String,
    /// Comment label that additionally pre-consents to queueing the drafted
    /// plan (default `kranz:fix-and-queue`).
    pub queue_label: String,
    /// GitHub logins allowed to request work through PR comments. Empty
    /// disables comment triggers; a valid webhook signature authenticates
    /// GitHub, not the commenter's authority to spend or approve work.
    pub allow_users: Vec<String>,
}

impl Default for HooksConfig {
    fn default() -> Self {
        HooksConfig {
            secret: None,
            fix_label: DEFAULT_FIX_LABEL.to_string(),
            queue_label: DEFAULT_QUEUE_LABEL.to_string(),
            allow_users: Vec::new(),
        }
    }
}

impl fmt::Debug for HooksConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HooksConfig")
            .field("secret", &self.secret.as_ref().map(|_| "[REDACTED]"))
            .field("fix_label", &self.fix_label)
            .field("queue_label", &self.queue_label)
            .field("allow_users", &self.allow_users)
            .finish()
    }
}

/// Read the `hooks` key out of the same layered config files
/// ([`config::load`] reads): defaults, then the global file, then the project
/// file, later layers winning key-wise. Files without a `hooks` key are
/// skipped; a present-but-invalid `hooks` value is a config error naming the
/// layer. Absent everywhere ⇒ [`HooksConfig::default`] (no secret ⇒ the
/// caller refuses closed).
pub fn load_hooks(repo_root: &Path) -> Result<HooksConfig> {
    let mut layers: Vec<std::path::PathBuf> = Vec::new();
    if let Some(global) = paths::global_config() {
        layers.push(global);
    }
    layers.push(paths::project_config(repo_root));

    let mut merged = serde_json::json!({});
    for path in layers {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(EngineError::Config(format!(
                    "cannot read config file {}: {e}",
                    path.display()
                )))
            }
        };
        let value: Value = serde_json::from_str(&text).map_err(|e| {
            EngineError::Config(format!(
                "invalid JSON in config file {}: {e}",
                path.display()
            ))
        })?;
        if let Some(hooks) = value.get("hooks") {
            config::deep_merge(&mut merged, hooks);
        }
    }

    serde_json::from_value(merged)
        .map_err(|e| EngineError::Config(format!("hooks configuration does not deserialize: {e}")))
}

// ---------------------------------------------------------------------------
// Signature verification (HMAC-SHA256, RFC 2104)
// ---------------------------------------------------------------------------

/// Hex-encoded HMAC-SHA256 (RFC 2104) of `message` under `key`. Implemented
/// over the workspace's existing `sha2` dependency — no crypto crate is
/// added. Tested against the RFC 4231 vectors.
pub fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    const BLOCK: usize = 64; // SHA-256 block size

    let mut key_block = [0u8; BLOCK];
    if key.len() > BLOCK {
        let hashed = Sha256::digest(key);
        key_block[..hashed.len()].copy_from_slice(&hashed);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut inner = Sha256::new();
    for byte in key_block {
        inner.update([byte ^ 0x36]);
    }
    inner.update(message);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    for byte in key_block {
        outer.update([byte ^ 0x5c]);
    }
    outer.update(inner_hash);
    let digest = outer.finalize();

    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        hex.push_str(&format!("{byte:02x}"));
    }
    hex
}

/// Verify a GitHub `X-Hub-Signature-256` header value (`sha256=<hex>`)
/// against the raw request body. Constant-time on the digest bytes, and the
/// secret is never logged. A missing or malformed header never verifies.
pub fn verify_signature(secret: &str, body: &[u8], header: Option<&str>) -> bool {
    use subtle::ConstantTimeEq;
    let Some(presented) = header.and_then(|value| value.strip_prefix("sha256=")) else {
        return false;
    };
    let expected = hmac_sha256_hex(secret.as_bytes(), body);
    expected.as_bytes().ct_eq(presented.as_bytes()).into()
}

// ---------------------------------------------------------------------------
// Trigger parsing (the allowlist + payload rules)
// ---------------------------------------------------------------------------

/// `X-GitHub-Event` values the hook route acts on. Everything else is
/// 202-ignored with a decision log line.
pub const ACCEPTED_EVENTS: [&str; 3] = [
    "workflow_run",
    "pull_request_review_comment",
    "issue_comment",
];

/// The only actions a trigger may drive (D-F: draft, or draft+queue on
/// recorded pre-consent). There is deliberately no run/land action: a
/// trigger ticket reaches execution only through the existing approval path.
/// A regression test pins this set.
pub const TRIGGER_ACTIONS: [&str; 2] = ["draft", "queue"];

/// Mission branch prefix — a CI failure on one of these is kranz's own work.
const MISSION_BRANCH_PREFIX: &str = "kranz/mission-";

/// Which external trigger opened the ticket (the `trigger:` frontmatter
/// provenance field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    /// `workflow_run` with `conclusion: failure`.
    CiFailure,
    /// A PR comment carrying the configured trigger label.
    PrComment,
}

impl TriggerKind {
    /// The `trigger:` frontmatter value.
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerKind::CiFailure => "ci-failure",
            TriggerKind::PrComment => "pr-comment",
        }
    }
}

/// The consent state recorded into the ticket body (D-F: never bypassed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// Draft only: a human runs the normal draft → approve → queue path.
    DraftOnly,
    /// The trigger comment carried the queue label: the draft may auto-queue
    /// on an approved plan (plan approval itself is never skipped).
    PreConsentedQueue,
}

impl Consent {
    /// Whether the draft may enqueue on an approved plan — the `then_enqueue`
    /// flag of the existing draft pipeline.
    pub fn then_enqueue(self) -> bool {
        matches!(self, Consent::PreConsentedQueue)
    }

    /// The consent label written into the ticket's provenance block.
    pub fn as_str(self) -> &'static str {
        match self {
            Consent::DraftOnly => "draft-only",
            Consent::PreConsentedQueue => "fix-and-queue",
        }
    }
}

/// One accepted trigger, normalized out of the webhook payload. Every field
/// that lands in the ticket is already bounded and scrubbed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    /// Dedup identity: the workflow run id or the PR number.
    pub dedup_id: String,
    pub title: String,
    /// Trigger source url (workflow run / comment permalink).
    pub source_url: String,
    pub actor: String,
    /// Bounded, scrubbed failure/comment excerpt for the ticket body.
    pub excerpt: String,
    pub consent: Consent,
}

/// Cap on the raw text pulled from a payload BEFORE scrubbing, so scrub work
/// is bounded; the ticket excerpt is then capped to [`MAX_EXCERPT_CHARS`].
const MAX_RAW_CHARS: usize = 8 * 1024;
/// Cap on the excerpt written into the ticket body.
const MAX_EXCERPT_CHARS: usize = 1000;
/// Cap on a one-line field (title, actor).
const MAX_LINE_CHARS: usize = 100;

/// Bound + scrub free text from a payload: the raw text is cut to
/// [`MAX_RAW_CHARS`] first (bounding scrub work while keeping a redaction
/// pass over everything that could survive), scrubbed, then cut to `max`.
fn bound_scrubbed(raw: &str, max: usize) -> String {
    let bounded = scrub::truncate_chars(raw, MAX_RAW_CHARS);
    let scrubbed = scrub::scrub(&bounded);
    scrub::truncate_chars(scrubbed.trim(), max)
}

/// One-line version of [`bound_scrubbed`] for titles and logins.
fn bound_line(raw: &str) -> String {
    let one_line = raw.replace(['\n', '\r'], " ");
    bound_scrubbed(&one_line, MAX_LINE_CHARS)
}

/// Parse a webhook payload into a [`Trigger`], or `None` when the event does
/// not match a trigger rule (the caller 202-ignores it with a decision log).
/// `event` is the `X-GitHub-Event` header value; anything outside
/// [`ACCEPTED_EVENTS`] is `None` here too, keeping the allowlist meaningful
/// at the core as well as at the route.
pub fn parse_trigger(event: &str, payload: &Value, cfg: &HooksConfig) -> Option<Trigger> {
    match event {
        "workflow_run" => parse_workflow_run(payload),
        "pull_request_review_comment" | "issue_comment" => parse_comment(event, payload, cfg),
        _ => None,
    }
}

/// `workflow_run`: only `action: completed` with `conclusion: failure` on the
/// repository's default branch or a mission branch.
fn parse_workflow_run(payload: &Value) -> Option<Trigger> {
    if payload.get("action")?.as_str()? != "completed" {
        return None;
    }
    let run = payload.get("workflow_run")?;
    if run.get("conclusion")?.as_str()? != "failure" {
        return None;
    }
    // A fork can name its branch `main` or `kranz/mission-*` too. The
    // workflow's source repository must be the repository receiving the
    // webhook before a branch name can authorize an automatic draft.
    let repository = payload.pointer("/repository/full_name")?.as_str()?;
    let head_repository = run.pointer("/head_repository/full_name")?.as_str()?;
    if repository.is_empty() || !repository.eq_ignore_ascii_case(head_repository) {
        return None;
    }
    let head_branch = run.get("head_branch")?.as_str()?;
    let default_branch = payload
        .pointer("/repository/default_branch")
        .and_then(Value::as_str)
        .unwrap_or("");
    let on_default = !default_branch.is_empty() && head_branch == default_branch;
    if !on_default && !head_branch.starts_with(MISSION_BRANCH_PREFIX) {
        return None;
    }

    let run_id = run.get("id")?.as_u64()?.to_string();
    let workflow = run.get("name").and_then(Value::as_str).unwrap_or("CI");
    let url = run
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let actor = run
        .pointer("/actor/login")
        .and_then(Value::as_str)
        .or_else(|| payload.pointer("/sender/login").and_then(Value::as_str))
        .unwrap_or("unknown");
    let head_sha = run
        .get("head_sha")
        .and_then(Value::as_str)
        .map(|sha| sha.chars().take(12).collect::<String>())
        .unwrap_or_default();
    let excerpt = format!(
        "workflow: {}\nbranch: {}\nhead_sha: {}\nconclusion: failure\nrun id: {}",
        bound_line(workflow),
        bound_line(head_branch),
        head_sha,
        run_id,
    );

    Some(Trigger {
        kind: TriggerKind::CiFailure,
        dedup_id: run_id.clone(),
        title: bound_line(&format!(
            "CI failure: {workflow} on {head_branch} (run {run_id})"
        )),
        source_url: url,
        actor: bound_line(actor),
        excerpt,
        consent: Consent::DraftOnly,
    })
}

/// `issue_comment` / `pull_request_review_comment`: only `action: created`
/// where the comment body carries the configured fix (or queue) label and
/// the target is a pull request. The queue label is checked FIRST: its
/// default (`kranz:fix-and-queue`) starts with the fix label, so checking
/// the fix label first would misread pre-consent as draft-only.
fn parse_comment(event: &str, payload: &Value, cfg: &HooksConfig) -> Option<Trigger> {
    if payload.get("action")?.as_str()? != "created" {
        return None;
    }
    let actor = payload.pointer("/comment/user/login")?.as_str()?;
    if actor.is_empty()
        || !cfg
            .allow_users
            .iter()
            .any(|allowed| allowed.eq_ignore_ascii_case(actor))
    {
        return None;
    }
    let body = payload.pointer("/comment/body")?.as_str()?;
    let consent = if body.contains(&cfg.queue_label) {
        Consent::PreConsentedQueue
    } else if body.contains(&cfg.fix_label) {
        Consent::DraftOnly
    } else {
        return None;
    };

    let pr_number = match event {
        // A review comment is always on a PR.
        "pull_request_review_comment" => payload.pointer("/pull_request/number")?.as_u64()?,
        // An issue comment only triggers when the issue IS a pull request.
        _ => {
            payload.pointer("/issue/pull_request")?;
            payload.pointer("/issue/number")?.as_u64()?
        }
    };

    let url = payload
        .pointer("/comment/html_url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some(Trigger {
        kind: TriggerKind::PrComment,
        dedup_id: pr_number.to_string(),
        title: bound_line(&format!("PR #{pr_number} fix requested by {actor}")),
        source_url: url,
        actor: bound_line(actor),
        excerpt: bound_scrubbed(body, MAX_EXCERPT_CHARS),
        consent,
    })
}

// ---------------------------------------------------------------------------
// Repository identity (no cross-repo triggers)
// ---------------------------------------------------------------------------

/// The `repository.full_name` (`owner/repo`) a GitHub webhook payload claims.
pub fn repo_full_name_from_payload(payload: &Value) -> Option<String> {
    payload
        .pointer("/repository/full_name")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Derive `owner/repo` from a GitHub remote URL (`git@github.com:o/r.git`,
/// `ssh://git@github.com/o/r.git`, `https://github.com/o/r[.git]`). Any other
/// host or shape is `None` — the caller refuses closed when it cannot
/// establish the served repository's identity.
pub fn github_full_name_from_remote(url: &str) -> Option<String> {
    let path = if let Some(scp) = url.strip_prefix("git@github.com:") {
        scp
    } else {
        url.strip_prefix("https://github.com/")
            .or_else(|| url.strip_prefix("ssh://git@github.com/"))?
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (owner, repo) = (parts.next()?, parts.next()?);
    if parts.next().is_some() || owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

// ---------------------------------------------------------------------------
// Ticket drafting (dedup + provenance)
// ---------------------------------------------------------------------------

/// Outcome of drafting a trigger ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerDraft {
    /// The ticket was written (state New, ready for the draft pipeline).
    Drafted { slug: String },
    /// A ticket for this trigger already exists — a second event for the
    /// same workflow run / PR is a no-op, never a duplicate ticket.
    Duplicate { slug: String },
}

/// Deterministic ticket slug for a trigger: `trigger-ci-<run id>` /
/// `trigger-pr-<number>`. The dedup identity is numeric in both accepted
/// payloads; the filter keeps the slug valid even if a future payload shape
/// changes that.
pub fn trigger_slug(kind: TriggerKind, dedup_id: &str) -> String {
    let clean: String = dedup_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    let prefix = match kind {
        TriggerKind::CiFailure => "trigger-ci",
        TriggerKind::PrComment => "trigger-pr",
    };
    format!("{prefix}-{clean}")
}

/// Write the trigger's ticket, deduped by slug: an existing ticket file for
/// the same workflow run / PR is a no-op ([`TriggerDraft::Duplicate`]).
/// Performs no git operations; the ticket enters the normal pipeline at
/// state New, exactly like a human-authored one.
pub fn draft_trigger_ticket(repo_root: &Path, trigger: &Trigger) -> Result<TriggerDraft> {
    let slug = trigger_slug(trigger.kind, &trigger.dedup_id);
    Ticket::ensure_valid_slug(&slug)?;
    let path = Ticket::md_path(repo_root, &slug);
    if path.exists() {
        return Ok(TriggerDraft::Duplicate { slug });
    }
    let body = trigger_ticket_template(trigger);
    // Self-check, mirroring Ticket::scaffold: a template that does not parse
    // back is a bug, not a ticket.
    Ticket::parse(&slug, &body).map_err(|e| {
        EngineError::Other(format!(
            "internal error: trigger ticket template does not parse: {e}"
        ))
    })?;
    std::fs::create_dir_all(Ticket::tickets_dir(repo_root))?;
    std::fs::write(&path, body)?;
    Ok(TriggerDraft::Drafted { slug })
}

/// The trigger ticket's markdown: the standard four sections plus the
/// additive `trigger:` frontmatter field and a provenance block in
/// `## Context` (source url, actor, consent state, bounded scrubbed
/// excerpt). Parses back cleanly through [`Ticket::parse`].
pub fn trigger_ticket_template(trigger: &Trigger) -> String {
    let goal = match trigger.kind {
        TriggerKind::CiFailure => format!(
            "Investigate and fix the CI failure recorded at {} (details in Context). \
             Reproduce the failing gate locally, land the minimal fix, and keep the \
             repo's gate suite green.",
            trigger.source_url
        ),
        TriggerKind::PrComment => format!(
            "Address the review feedback from {} (PR #{}, excerpt in Context). \
             Land the minimal change that resolves it.",
            trigger.source_url, trigger.dedup_id
        ),
    };
    let consent_note = match trigger.consent {
        Consent::DraftOnly => {
            "a human drives the normal draft → approve → queue path; \
             the trigger itself queues nothing"
        }
        Consent::PreConsentedQueue => {
            "operator pre-consented via the queue label: the draft \
             auto-queues on an approved plan (plan approval itself is never skipped)"
        }
    };
    let excerpt_heading = match trigger.kind {
        TriggerKind::CiFailure => "Failure excerpt (bounded, scrubbed)",
        TriggerKind::PrComment => "Comment excerpt (bounded, scrubbed)",
    };
    format!(
        "---\n\
         title: {title}\n\
         priority: 2\n\
         schedule: once\n\
         trigger: {kind}\n\
         ---\n\
         \n\
         ## Goal\n\
         {goal}\n\
         \n\
         ## Context\n\
         Trigger: {kind} (GitHub webhook)\n\
         Source: {source}\n\
         Actor: {actor}\n\
         Consent: {consent} — {consent_note}\n\
         \n\
         {excerpt_heading}:\n\
         {excerpt}\n\
         \n\
         ## Scoping answers\n\
         \n\
         ## Acceptance hints\n\
         - Reproduce the failure or concern before changing code.\n\
         - The fix mission starts only through the normal approval path.\n",
        title = bound_line(&trigger.title),
        kind = trigger.kind.as_str(),
        goal = bound_scrubbed(&goal, MAX_EXCERPT_CHARS),
        source = bound_line(&trigger.source_url),
        actor = bound_line(&trigger.actor),
        consent = trigger.consent.as_str(),
        excerpt = trigger.excerpt,
    )
}
