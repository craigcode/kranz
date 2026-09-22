//! Credential scrubbing and safe truncation (plan §3, roadmap M5).
//!
//! Every transcript line passes through [`scrub`] before it is written to
//! disk or broadcast as a `worker.message` event, replacing common credential
//! shapes with `[REDACTED]`. This is defense in depth, not a guarantee — the
//! permission layer (§4.7) is the primary control.
//!
//! The rule set is a fixed, `OnceLock`-compiled list of regexes plus one
//! entropy-gated pass. There are deliberately **no external dependencies**
//! (no secret-scanning crate): everything is `regex` + a hand-rolled Shannon
//! entropy helper. The design goal is high recall on real credential shapes
//! while keeping false positives low enough that ordinary prose, git SHAs,
//! UUIDs, and placeholder tokens survive untouched.
//!
//! # Rule ordering
//!
//! Rules run in list order and the output of each feeds the next, so the
//! **most specific patterns must run first**:
//!
//! 1. **PEM private-key blocks** (multi-line) — removed whole before anything
//!    inside them can match a narrower rule.
//! 2. **GCP service-account `private_key` JSON** — the escaped PEM body that
//!    lives on a single JSON line.
//! 3. **Vendor-specific fixed-prefix tokens** (Anthropic, OpenAI incl.
//!    `sk-proj-`, Google `AIza`, Stripe, npm, GitHub, AWS, Slack, JWT). These
//!    have unmistakable shapes, so they run before any generic rule.
//! 4. **Credential headers** — `Authorization: Bearer` / `Basic`. Scheme word
//!    kept, credential redacted.
//! 5. **Connection-string passwords** — `scheme://user:PASSWORD@host`. Only the
//!    password segment is redacted; user and host stay for diagnosis.
//! 6. **Generic assignment catch-all** — `key/secret/token/password = value`.
//!    Runs late so a vendor rule gets first crack at the value. Implemented as
//!    an allowlist-aware closure pass (not a static replacement) so placeholder
//!    tokens, UUIDs, and git SHAs assigned to secret-ish names survive.
//! 7. **Entropy-gated bare token** — a high-entropy base64/hex blob that sits
//!    next to a *broader* secret-ish key name (`access_token`, `client_secret`,
//!    `auth`, …) not covered by rule 6. This is the only rule that reasons
//!    about the *content* of the value, and it is gated behind both a key-name
//!    context match **and** the allowlist plus a 4.0 bits/char entropy floor,
//!    so random-looking prose, git SHAs, and UUIDs are never touched.
//!
//! Rules 1–5 are static `OnceLock` regex replacements; rules 6–7 are
//! `OnceLock`-compiled regexes applied through closures so they can consult the
//! allowlist and (for rule 7) entropy. Every pass is deterministic.
//!
//! [`truncate_chars`] cuts long content on a `char` boundary so multibyte
//! text can never panic the engine or produce invalid UTF-8.

use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::OnceLock;

/// Marker appended by [`truncate_chars`] when content was cut.
const TRUNCATION_MARKER: &str = "… [truncated]";

/// Replacement marker written in place of a redacted secret.
const REDACTED: &str = "[REDACTED]";

/// Tracked repository file containing one waived secret fingerprint per line.
pub const SECRET_ALLOWLIST_PATH: &str = ".kranz/secret-allowlist";

/// A secret detector hit. Never carries the secret value itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretFinding {
    pub rule_id: String,
    pub fingerprint: String,
    pub location: String,
    pub start: usize,
    pub end: usize,
}

/// Result of scanning and redacting a text payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretScan {
    pub redacted: String,
    pub findings: Vec<SecretFinding>,
}

/// One scrub pattern plus its replacement template. Replacements may use
/// `${1}` (and `${2}`) to preserve captured context (e.g. the key name of an
/// assignment, or the `user:`/`@host` framing of a connection string).
struct Rule {
    id: &'static str,
    re: Regex,
    replacement: &'static str,
    secret_group: Option<usize>,
}

fn rule(id: &'static str, pattern: &str, replacement: &'static str) -> Rule {
    Rule {
        id,
        re: Regex::new(pattern).expect("static scrub regex must compile"),
        replacement,
        secret_group: None,
    }
}

fn grouped_rule(
    id: &'static str,
    pattern: &str,
    replacement: &'static str,
    secret_group: usize,
) -> Rule {
    Rule {
        id,
        re: Regex::new(pattern).expect("static scrub regex must compile"),
        replacement,
        secret_group: Some(secret_group),
    }
}

/// The scrub rules, compiled once on first use. See the module docs for the
/// ordering contract; briefly: private-key blocks first, then vendor-specific
/// token shapes, then credential headers and connection strings, then the
/// generic assignment catch-all. The entropy pass is applied separately in
/// [`scrub`] *after* these run.
fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        vec![
            // 1. PEM private key blocks — the whole block, or just the BEGIN
            //    line when the END marker never arrives (partial output).
            rule(
                "pem-private-key",
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----|-----BEGIN [A-Z ]*PRIVATE KEY-----[^\r\n]*",
                REDACTED,
            ),
            // 2. GCP service-account JSON `"private_key": "-----BEGIN...\n..."`.
            //    The PEM body is escaped onto one line, so the multi-line rule
            //    above misses it. Keep the field name, redact the value.
            grouped_rule(
                "gcp-private-key-json",
                r#"(?i)("private_key"\s*:\s*")(-----BEGIN[^"]*)"#,
                "${1}[REDACTED]",
                2,
            ),
            // 3a. Anthropic API keys (before the generic sk- rule).
            rule("anthropic-api-key", r"\bsk-ant-[A-Za-z0-9_-]{8,}", REDACTED),
            // 3b. OpenAI project keys: sk-proj-<body>. Listed before the plain
            //     sk- rule because the body contains `-`/`_` which the plain
            //     rule would stop at, leaving a tail behind.
            rule(
                "openai-project-key",
                r"\bsk-proj-[A-Za-z0-9_-]{20,}",
                REDACTED,
            ),
            // 3c. OpenAI-style keys (plain sk-...).
            rule("openai-api-key", r"\bsk-[A-Za-z0-9]{20,}", REDACTED),
            // 3d. Google API keys (AIza + 35 chars).
            rule("google-api-key", r"\bAIza[0-9A-Za-z_-]{35}\b", REDACTED),
            // 3e. Stripe live/restricted/publishable keys.
            rule(
                "stripe-live-key",
                r"\b(?:sk|rk|pk)_live_[0-9A-Za-z]{16,}",
                REDACTED,
            ),
            // 3f. npm access tokens (npm_ + 36 chars).
            rule("npm-token", r"\bnpm_[0-9A-Za-z]{36}\b", REDACTED),
            // 3g. GitHub tokens: classic (ghp_), OAuth (gho_), server (ghs_).
            rule("github-token", r"\bgh[pos]_[A-Za-z0-9]{20,}", REDACTED),
            // 3h. GitHub fine-grained PATs.
            rule(
                "github-fine-grained-token",
                r"\bgithub_pat_[A-Za-z0-9_]{20,}",
                REDACTED,
            ),
            // 3i. AWS access key ids (exactly 16 chars after AKIA).
            rule("aws-access-key-id", r"\bAKIA[0-9A-Z]{16}\b", REDACTED),
            // 3j. AWS secret keys in config/env form; the key name is kept.
            grouped_rule(
                "aws-secret-access-key",
                r"(?i)\b(aws_secret_access_key\s*[=:]\s*)(\S+)",
                "${1}[REDACTED]",
                2,
            ),
            // 3k. Slack tokens.
            rule("slack-token", r"\bxox[baprs]-[A-Za-z0-9-]{10,}", REDACTED),
            // Sgian client credentials contain 32 random bytes encoded as hex.
            rule("sgian-client-token", r"\bsgc_[0-9a-fA-F]{64}\b", REDACTED),
            // 3l. JWTs (three base64url segments).
            rule(
                "jwt",
                r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}",
                REDACTED,
            ),
            // 4a. Authorization: Bearer <token> — keep the scheme word.
            grouped_rule(
                "authorization-bearer",
                r"(?i)\b(bearer\s+)([a-z0-9._~+/=-]{16,})",
                "${1}[REDACTED]",
                2,
            ),
            // 4b. Authorization: Basic <base64> — keep the scheme word.
            grouped_rule(
                "authorization-basic",
                r"(?i)\b(basic\s+)([a-z0-9+/]{16,}={0,2})",
                "${1}[REDACTED]",
                2,
            ),
            // 5. Connection strings with an embedded password:
            //    scheme://user:PASSWORD@host. Redact only the password segment;
            //    the `user:` prefix and `@host` remainder are preserved.
            grouped_rule(
                "connection-string-password",
                r"([a-zA-Z][a-zA-Z0-9+.-]*://[^\s:/@]+:)([^\s:/@]+)(@)",
                "${1}[REDACTED]${3}",
                2,
            ),
        ]
    })
}

/// Generic key/secret/token/password assignment finder. Group 1 captures the
/// key-name-plus-operator prefix (kept so logs stay diagnosable); group 2
/// captures the value (redacted unless allowlisted). Runs as a **closure** pass
/// after the fixed vendor rules so it can consult the allowlist — a bare regex
/// replacement could not tell a real secret from a `REPLACE_ME` placeholder or
/// a UUID. Values shorter than 8 chars ("None", "****") never match.
fn generic_assignment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)((?:api[_-]?key|secret|token|password|passwd|credential)["']?\s*[:=]\s*["']?)([^\s"']{8,})"#,
        )
        .expect("generic assignment regex must compile")
    })
}

/// Broader entropy-gated assignment finder: catches high-entropy base64/hex
/// blobs assigned to secret-ish names the generic rule does not list
/// (`auth`, `access_token`, `client_secret`, `private_key`). Group 2 is only
/// redacted when it clears the entropy/charset/allowlist bar, so widening the
/// key-name surface cannot introduce prose false positives.
fn entropy_assignment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?i)((?:access[_-]?token|auth[_-]?token|auth|client[_-]?secret|private[_-]?key)["']?\s*[:=]\s*["']?)([A-Za-z0-9+/_=-]{24,})"#,
        )
        .expect("entropy assignment regex must compile")
    })
}

/// Shannon entropy of `s` in bits per character. Empty input is 0.0. A uniform
/// random base64 string tends toward ~5.5–6 bits/char; a random hex string
/// toward ~3.9–4 bits/char; English prose sits well below (~2–3 for short
/// words). This is the signal the entropy pass thresholds on.
pub(crate) fn shannon_entropy(s: &str) -> f64 {
    if s.is_empty() {
        return 0.0;
    }
    let mut counts: HashMap<char, usize> = HashMap::new();
    for c in s.chars() {
        *counts.entry(c).or_insert(0) += 1;
    }
    let len = s.chars().count() as f64;
    counts
        .values()
        .map(|&count| {
            let p = count as f64 / len;
            -p * p.log2()
        })
        .sum()
}

/// True when `s` uses a base64/base64url/hex-ish alphabet only — the charset a
/// real machine-generated secret lives in. Rejects tokens containing spaces or
/// punctuation typical of prose. Used to keep the entropy pass off ordinary
/// words that merely look "random".
fn looks_like_secret_charset(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '_' | '-' | '='))
}

/// True when `value` is an obvious non-secret that must never be redacted,
/// regardless of entropy: placeholder/example tokens, all-same-character runs,
/// UUIDs, and 40-hex git SHAs. Kept conservative — this is the last line of
/// defense against a false positive.
fn is_allowlisted(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();

    // Placeholder / dummy substrings.
    const PLACEHOLDERS: &[&str] = &[
        "xxxx",
        "replace",
        "example",
        "changeme",
        "your",
        "dummy",
        "placeholder",
        "todo",
        "none",
        "redacted",
    ];
    if PLACEHOLDERS.iter().any(|p| lower.contains(p)) {
        return true;
    }

    // All-same-character runs ("aaaaaaaa…", "00000000…", "********").
    if let Some(first) = value.chars().next() {
        if value.chars().all(|c| c == first) {
            return true;
        }
    }

    // UUID (8-4-4-4-12 hex).
    if is_uuid(value) {
        return true;
    }

    // 40-char lowercase hex — a git SHA-1. (Full-length only; short SHAs are
    // too ambiguous to allowlist and too short to trip the entropy floor.)
    if value.len() == 40 && value.chars().all(|c| c.is_ascii_hexdigit()) && lower == value {
        return true;
    }

    false
}

/// True when `s` is a canonical 8-4-4-4-12 hyphenated UUID.
fn is_uuid(s: &str) -> bool {
    let groups: Vec<&str> = s.split('-').collect();
    if groups.len() != 5 {
        return false;
    }
    let widths = [8usize, 4, 4, 4, 12];
    groups
        .iter()
        .zip(widths)
        .all(|(g, w)| g.len() == w && g.chars().all(|c| c.is_ascii_hexdigit()))
}

/// True when a bare `value` in secret-key context should be redacted by the
/// entropy pass: right charset, long enough, high enough entropy, and not
/// allowlisted.
fn is_high_entropy_secret(value: &str) -> bool {
    value.len() >= 24
        && looks_like_secret_charset(value)
        && !is_allowlisted(value)
        && shannon_entropy(value) >= 4.0
}

fn secret_fingerprint(rule_id: &str, value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(rule_id.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    let digest = hasher.finalize();
    digest[..12].iter().map(|b| format!("{b:02x}")).collect()
}

fn push_finding(
    out: &mut Vec<SecretFinding>,
    occupied: &mut Vec<Range<usize>>,
    rule_id: &str,
    location: &str,
    range: Range<usize>,
    value: &str,
) {
    if is_allowlisted(value) {
        return;
    }
    if occupied
        .iter()
        .any(|existing| existing.start < range.end && range.start < existing.end)
    {
        return;
    }
    occupied.push(range.clone());
    out.push(SecretFinding {
        rule_id: rule_id.to_string(),
        fingerprint: secret_fingerprint(rule_id, value),
        location: location.to_string(),
        start: range.start,
        end: range.end,
    });
}

/// Find secrets in `text`, using `location` only for diagnostics.
pub fn scan_text_at(text: &str, location: &str) -> Vec<SecretFinding> {
    scan_text_with_assignments(text, text, location)
}

fn scan_text_with_assignments(
    text: &str,
    assignment_text: &str,
    location: &str,
) -> Vec<SecretFinding> {
    let mut out = Vec::new();
    let mut occupied: Vec<Range<usize>> = Vec::new();
    for rule in rules() {
        for caps in rule.re.captures_iter(text) {
            let m = rule
                .secret_group
                .and_then(|idx| caps.get(idx))
                .or_else(|| caps.get(0));
            if let Some(m) = m {
                push_finding(
                    &mut out,
                    &mut occupied,
                    rule.id,
                    location,
                    m.start()..m.end(),
                    m.as_str(),
                );
            }
        }
    }

    for caps in generic_assignment_re().captures_iter(assignment_text) {
        if let Some(value) = caps.get(2) {
            push_finding(
                &mut out,
                &mut occupied,
                "generic-secret-assignment",
                location,
                value.start()..value.end(),
                value.as_str(),
            );
        }
    }

    for caps in entropy_assignment_re().captures_iter(assignment_text) {
        if let Some(value) = caps.get(2) {
            if is_high_entropy_secret(value.as_str()) {
                push_finding(
                    &mut out,
                    &mut occupied,
                    "high-entropy-secret-assignment",
                    location,
                    value.start()..value.end(),
                    value.as_str(),
                );
            }
        }
    }
    out
}

/// Find secrets in `text`.
pub fn scan_text(text: &str) -> Vec<SecretFinding> {
    scan_text_at(text, "text")
}

/// Generic assignment pass: redact the value of a `key/secret/token/password`
/// assignment unless it is allowlisted. Runs as a closure (not a static regex
/// replacement) so placeholders (`REPLACE_ME`), UUIDs, git SHAs, and
/// all-same-char runs survive even when assigned to a secret-ish name.
fn scrub_assignments(text: &str) -> Cow<'_, str> {
    generic_assignment_re().replace_all(text, |caps: &regex::Captures<'_>| {
        let prefix = &caps[1];
        let value = &caps[2];
        if is_allowlisted(value) {
            caps[0].to_owned()
        } else {
            format!("{prefix}{REDACTED}")
        }
    })
}

/// Entropy-gated pass: redact a bare high-entropy token **only** when it is
/// assigned to a broader secret-ish key name (`access_token`, `client_secret`,
/// `auth`, …) and clears the entropy/charset/allowlist bar. This never inspects
/// free prose — it requires the key-name context first — so a random-looking
/// word in a sentence, a git SHA after "commit", or a UUID is left untouched.
fn scrub_entropy(text: &str) -> Cow<'_, str> {
    entropy_assignment_re().replace_all(text, |caps: &regex::Captures<'_>| {
        let prefix = &caps[1];
        let value = &caps[2];
        if is_high_entropy_secret(value) {
            format!("{prefix}{REDACTED}")
        } else {
            caps[0].to_owned()
        }
    })
}

/// Replace anything that looks like a credential with `[REDACTED]`.
///
/// For assignment-shaped matches (`api_key=...`, `aws_secret_access_key: ...`,
/// `Bearer ...`, `Basic ...`) the key name / scheme is preserved and only the
/// secret value is redacted. Connection-string passwords redact the password
/// segment only, keeping `user:` and `@host`. The final entropy pass catches
/// unprefixed high-entropy blobs assigned to secret-ish names, gated so prose,
/// git SHAs, and UUIDs survive.
fn scrub_plain(text: &str) -> String {
    let mut out = text.to_owned();
    for rule in rules() {
        if let Cow::Owned(replaced) = rule.re.replace_all(&out, rule.replacement) {
            out = replaced;
        }
    }
    // Generic assignment pass (rule 6): allowlist-aware, so placeholders and
    // UUIDs assigned to secret-ish names survive.
    if let Cow::Owned(replaced) = scrub_assignments(&out) {
        out = replaced;
    }
    // Entropy pass runs last (rule 7): the fixed-shape and generic rules above
    // have already handled everything with a recognizable prefix or name, so a
    // high-entropy blob still sitting in a broader secret-key slot is worth
    // redacting.
    if let Cow::Owned(replaced) = scrub_entropy(&out) {
        out = replaced;
    }
    out
}

// Decode JSON before redacting its strings. Applying regex replacements to
// serialized strings can consume the backslash of an escaped quote and turn
// a valid decision (or a transcript containing one) into malformed JSON.
fn scrub_json_text(text: &str) -> Option<String> {
    // Validate syntax first, but do not serialize a parsed object: that would
    // collapse duplicate keys and could turn a rejected decision into a valid
    // one. Replace individual string tokens, preserving all other bytes.
    serde_json::from_str::<serde_json::Value>(text).ok()?;
    let bytes = text.as_bytes();
    let mut cursor = 0;
    let mut copied = 0;
    let mut out = String::new();
    let mut key: Option<String> = None;
    while cursor < bytes.len() {
        let start = cursor;
        if bytes[cursor] == b'"' {
            cursor += 1;
            while cursor < bytes.len() {
                match bytes[cursor] {
                    b'\\' => cursor += 2,
                    b'"' => {
                        cursor += 1;
                        break;
                    }
                    _ => cursor += 1,
                }
            }
            let decoded: String = serde_json::from_str(&text[start..cursor]).ok()?;
            let is_key = text[cursor..].trim_start().starts_with(':');
            let redacted = if is_key {
                scrub_plain(&decoded)
            } else {
                scrub_json_assignment(scrub_impl(&decoded), key.as_deref())
            };
            if redacted != decoded {
                out.push_str(&text[copied..start]);
                // Runtime evidence deliberately escapes prompt delimiters.
                // Re-encoding a changed string must not restore those markers.
                out.push_str(
                    &serde_json::to_string(&redacted)
                        .ok()?
                        .replace('<', "\\u003c")
                        .replace('>', "\\u003e"),
                );
                copied = cursor;
            }
            key = is_key.then_some(decoded);
        } else if bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b':' {
            cursor += 1;
        } else {
            // Numeric credentials must not escape merely because JSON did not
            // quote them. Structural delimiters consume any pending field key.
            if key.is_some() && matches!(bytes[cursor], b'-' | b'0'..=b'9') {
                while cursor < bytes.len()
                    && matches!(
                        bytes[cursor],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    cursor += 1;
                }
                let value = &text[start..cursor];
                let redacted = scrub_json_assignment(value.to_owned(), key.as_deref());
                if redacted != value {
                    out.push_str(&text[copied..start]);
                    out.push_str(&serde_json::to_string(&redacted).ok()?);
                    copied = cursor;
                }
            } else {
                cursor += 1;
            }
            key = None;
        }
    }
    out.push_str(&text[copied..]);
    Some(out)
}

fn scrub_json_assignment(mut value: String, key: Option<&str>) -> String {
    if let Some(key) = key {
        // Match only the immediate value's context, not assignments inside a
        // nested JSON string that has already been redacted and re-escaped.
        let prefix = format!("{key}=\"");
        let contextual = format!("{prefix}{value}");
        for (regex, entropy_only) in [
            (generic_assignment_re(), false),
            (entropy_assignment_re(), true),
        ] {
            let Some(caps) = regex.captures(&contextual) else {
                continue;
            };
            let candidate = caps.get(2).expect("assignment value capture");
            if candidate.start() == prefix.len()
                && if entropy_only {
                    is_high_entropy_secret(candidate.as_str())
                } else {
                    !is_allowlisted(candidate.as_str())
                }
            {
                value.replace_range(..candidate.len(), REDACTED);
                break;
            }
        }
    }
    value
}

fn scrub_impl(text: &str) -> String {
    if let Some(redacted) = scrub_json_text(text) {
        return redacted;
    }
    // Preserve fenced replies and their surrounding prose. This only redacts;
    // the decision parser still owns whether a particular fence is an answer.
    let mut out = String::new();
    let mut plain_start = 0;
    let mut body_start = None;
    let mut cursor = 0;
    for line in text.split_inclusive('\n') {
        let start = cursor;
        cursor += line.len();
        let trimmed = line.trim();
        if body_start.is_none() && matches!(trimmed, "```" | "```json" | "```JSON") {
            body_start = Some(cursor);
        } else if trimmed == "```" {
            if let Some(body) = body_start.take() {
                if let Some(redacted) = scrub_json_text(&text[body..start]) {
                    out.push_str(&scrub_plain(&text[plain_start..body]));
                    out.push_str(&redacted);
                    // Serialization may remove the newline before the fence.
                    if !redacted.ends_with('\n') {
                        out.push('\n');
                    }
                    plain_start = start;
                }
            }
        }
    }
    out.push_str(&scrub_plain(&text[plain_start..]));
    out
}

/// Scan and redact anything that looks like a credential.
pub fn scrub_with_findings(text: &str, location: &str) -> SecretScan {
    SecretScan {
        redacted: scrub_impl(text),
        findings: scan_text_at(text, location),
    }
}

pub fn scrub(text: &str) -> String {
    scrub_impl(text)
}

/// Redact every string leaf in a JSON value. Findings carry JSON-pointer-ish
/// locations rooted at `location`.
pub fn scrub_json_value(value: &mut serde_json::Value, location: &str) -> Vec<SecretFinding> {
    fn walk(value: &mut serde_json::Value, path: String, findings: &mut Vec<SecretFinding>) {
        match value {
            serde_json::Value::String(s) => {
                let scan = scrub_with_findings(s, &path);
                *s = scan.redacted;
                findings.extend(scan.findings);
            }
            serde_json::Value::Array(items) => {
                for (idx, item) in items.iter_mut().enumerate() {
                    walk(item, format!("{path}/{idx}"), findings);
                }
            }
            serde_json::Value::Object(map) => {
                for (key, item) in map.iter_mut() {
                    walk(item, format!("{path}/{key}"), findings);
                }
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
    }

    let mut findings = Vec::new();
    walk(value, location.to_string(), &mut findings);
    findings
}

/// Generated dashboard bundles contain machine-generated assignments that
/// trip the broad generic heuristic. Suppress only that low-confidence rule;
/// fixed credential patterns and the entropy-gated rules still scan bundles.
const GENERATED_DIFF_PATH_PREFIXES: &[&str] =
    &["apps/dashboard/dist/", "crates/cli/assets/dashboard/dist/"];

/// Scan only added lines in a unified git diff.
pub fn scan_unified_diff(diff: &str) -> Vec<SecretFinding> {
    let mut findings = Vec::new();
    let mut path = "<diff>".to_string();
    let mut generated_dashboard_bundle = false;
    let mut new_line: Option<usize> = None;

    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ b/") {
            path = rest.to_string();
            generated_dashboard_bundle = GENERATED_DIFF_PATH_PREFIXES
                .iter()
                .any(|prefix| path.starts_with(prefix));
            continue;
        }
        if line.starts_with("@@ ") {
            new_line = parse_new_hunk_start(line);
            continue;
        }
        if line.starts_with("+++") {
            continue;
        }
        if let Some(added) = line.strip_prefix('+') {
            let line_no = new_line.unwrap_or(0);
            let location = if line_no == 0 {
                path.clone()
            } else {
                format!("{path}:{line_no}")
            };
            let mut line_findings = scan_text_at(added, &location);
            if generated_dashboard_bundle {
                line_findings.retain(|finding| finding.rule_id != "generic-secret-assignment");
            }
            findings.extend(line_findings);
            if let Some(n) = &mut new_line {
                *n += 1;
            }
        } else if !line.starts_with('-') {
            if let Some(n) = &mut new_line {
                *n += 1;
            }
        }
    }

    findings
}

fn parse_new_hunk_start(line: &str) -> Option<usize> {
    let plus = line.split_whitespace().find(|part| part.starts_with('+'))?;
    let number = plus
        .trim_start_matches('+')
        .split(',')
        .next()
        .filter(|s| !s.is_empty())?;
    number.parse().ok()
}

pub fn read_allowlist_text(text: &str) -> std::collections::BTreeSet<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| line.split_whitespace().next())
        .map(str::to_string)
        .collect()
}

pub fn filter_allowed(
    findings: Vec<SecretFinding>,
    allowed: &std::collections::BTreeSet<String>,
) -> Vec<SecretFinding> {
    findings
        .into_iter()
        .filter(|f| !allowed.contains(&f.fingerprint))
        .collect()
}

pub fn format_findings(findings: &[SecretFinding]) -> String {
    findings
        .iter()
        .map(|finding| {
            format!(
                "{} [{}] {} bytes {}..{}",
                finding.fingerprint, finding.rule_id, finding.location, finding.start, finding.end
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Files larger than this are NEVER read for scanning (13th-pass review,
/// P1): the scan reads whole files into memory for regex passes, so an
/// unbounded read lets a worker-authored path exhaust engine memory. 8 MiB
/// is generous for source text — secrets live in small files — and an
/// oversized file is skipped exactly like an unreadable one (see
/// [`scan_paths`]' contract).
const SCAN_PATH_MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// Read one scan candidate, or `None` for anything that is not a bounded
/// REGULAR file. Hardened against the hostile-tree shapes a worker can
/// plant (13th-pass review, P1 — the old `std::fs::read` followed symlinks
/// and had no size bound, so a FIFO blocked checkpointing indefinitely and
/// a symlink to `/dev/zero` or a huge file read without limit):
///
/// - the parent chain is pinned NO-FOLLOW and the leaf opened with
///   `FollowSymlinks::No` (the `crate::paths::open_parent_nofollow`
///   capability idiom), so a symlinked candidate is never read through;
/// - the leaf open carries `O_NONBLOCK` on unix (the flag the event log's
///   pinned reads use, `crate::event_log`), so a FIFO open returns
///   immediately instead of blocking on a writer that never comes — the
///   fstat below then refuses the non-regular entry;
/// - the OPENED fd is fstat-verified regular and at most
///   [`SCAN_PATH_MAX_FILE_BYTES`], closing the swap race between any
///   earlier directory listing and the open;
/// - the read itself takes at most cap+1 bytes, so a file racing larger
///   after fstat stays bounded (and is skipped whole — a partial scan
///   would be a false sense of coverage).
fn read_scan_candidate(path: &Path) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let (parent, name) = crate::paths::open_parent_nofollow(path).ok()?;
    let mut options = cap_std::fs::OpenOptions::new();
    {
        use cap_fs_ext::OpenOptionsFollowExt as _;
        use cap_primitives::fs::FollowSymlinks;
        options.read(true).follow(FollowSymlinks::No);
    }
    #[cfg(unix)]
    {
        use cap_fs_ext::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = parent.open_with(name, &options).ok()?.into_std();
    let metadata = file.metadata().ok()?;
    if !metadata.file_type().is_file() || metadata.len() > SCAN_PATH_MAX_FILE_BYTES {
        return None;
    }
    let mut buf = Vec::new();
    (&mut &file)
        .take(SCAN_PATH_MAX_FILE_BYTES + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() as u64 > SCAN_PATH_MAX_FILE_BYTES {
        return None;
    }
    Some(buf)
}

/// Scan file contents about to be committed by the engine.
///
/// The contract is "findings for what could be scanned": anything that is
/// not a bounded regular file — a symlink, FIFO, socket, device,
/// directory, an oversized or unreadable entry — is SKIPPED, never fatal
/// and never noted in the finding stream. A skip NOTE would let a worker
/// force checkpoint refusals by planting big or special files (a mission
/// DoS), and skipping is semantically right for the scan's job: a
/// checked-in symlink carries no secret BYTES of its own, and an oversized
/// or unreadable file rides the same posture unreadable entries always
/// had. See [`read_scan_candidate`] for the no-follow / non-blocking /
/// size-bounded mechanics (13th-pass review, P1).
pub fn scan_paths(repo_root: &Path, paths: &[&Path]) -> Vec<SecretFinding> {
    let mut findings = Vec::new();
    for path in paths {
        let full = if path.is_absolute() {
            path.to_path_buf()
        } else {
            repo_root.join(path)
        };
        let Some(bytes) = read_scan_candidate(&full) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let location = full
            .strip_prefix(repo_root)
            .ok()
            .and_then(|p| p.to_str())
            .unwrap_or_else(|| full.to_str().unwrap_or("<path>"));
        let assignments = if full.extension().is_some_and(|ext| ext == "py") {
            python_assignment_text(&text)
        } else {
            Cow::Borrowed(text.as_ref())
        };
        findings.extend(scan_text_with_assignments(&text, &assignments, location));
    }
    findings
}

// A Python suite header such as `if supplied != VALID_TOKEN:` is not an
// assignment. Its colon otherwise lets the generic heuristic consume the
// next statement (and even swallow a real credential's variable name).
// Mask only those terminal colons, retaining byte offsets. Fixed credential
// patterns still inspect the original file, and data/config scans are intact.
fn python_assignment_text(text: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(text);
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end();
        let keyword = trimmed.split_whitespace().next().unwrap_or("");
        if trimmed.ends_with(':')
            && matches!(
                keyword,
                "if" | "elif" | "while" | "for" | "with" | "except" | "class" | "match" | "case"
            )
        {
            let colon = offset + trimmed.len() - 1;
            out.to_mut().replace_range(colon..colon + 1, " ");
        }
        offset += line.len();
    }
    out
}

/// Truncate to at most `max` characters (not bytes), appending
/// `… [truncated]` when anything was cut. Always cuts on a `char` boundary,
/// so multibyte input can never split.
pub fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        // Fewer than or exactly `max` chars: nothing to cut.
        None => text.to_owned(),
        Some((cut_at, _)) => {
            let mut out = String::with_capacity(cut_at + TRUNCATION_MARKER.len());
            out.push_str(&text[..cut_at]);
            out.push_str(TRUNCATION_MARKER);
            out
        }
    }
}

/// [`scrub`] then [`truncate_chars`] — scrubbing happens first so truncation
/// can never split a secret into an unrecognizable (and unredacted) prefix.
pub fn scrub_and_truncate(text: &str, max: usize) -> String {
    truncate_chars(&scrub(text), max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entropy_of_empty_is_zero() {
        assert_eq!(shannon_entropy(""), 0.0);
    }

    #[test]
    fn entropy_of_uniform_string_is_zero() {
        assert_eq!(shannon_entropy("aaaaaaaa"), 0.0);
    }

    #[test]
    fn entropy_of_random_base64_is_high() {
        // A realistic random-looking base64 blob.
        let e = shannon_entropy("aB3xQ9zK7mP2wR5tY8uV1nJ4kL6dF0sG");
        assert!(e >= 4.0, "entropy too low: {e}");
    }

    #[test]
    fn entropy_of_english_word_is_low() {
        let e = shannon_entropy("bureaucracy");
        assert!(e < 4.0, "prose entropy unexpectedly high: {e}");
    }

    #[test]
    fn uuid_recognized() {
        assert!(is_uuid("550e8400-e29b-41d4-a716-446655440000"));
        assert!(!is_uuid("not-a-uuid"));
        assert!(!is_uuid("550e8400e29b41d4a716446655440000"));
    }

    #[test]
    fn allowlist_covers_placeholders_and_shas() {
        assert!(is_allowlisted("REPLACE_ME_WITH_REAL_KEY_1234567890"));
        assert!(is_allowlisted("xxxxxxxxxxxxxxxxxxxxxxxx"));
        assert!(is_allowlisted("aaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(is_allowlisted("550e8400-e29b-41d4-a716-446655440000"));
        // 40-hex git SHA.
        assert!(is_allowlisted("da39a3ee5e6b4b0d3255bfef95601890afd80709"));
    }

    // -----------------------------------------------------------------------
    // 13th-pass review (P1): scan_paths reads are no-follow, non-blocking,
    // regular-file-only, and size-bounded. A secret shape the scanner
    // provably flags (the anthropic-api-key rule) anchors every anti-vacuity
    // arm.
    // -----------------------------------------------------------------------

    /// A token the anthropic-api-key rule flags on any scanned text.
    const SCRUB_NOFOLLOW_SECRET: &str = "sk-ant-api03-ScrubNofollowTestValue1";

    /// Run scan_paths on a spawned thread with a hard timeout: this group's
    /// assertions are about NOT hanging (a FIFO without a writer blocked the
    /// old `std::fs::read` forever; a followed `/dev/zero` read without
    /// bound), so the probe itself must be bounded. Panics after `secs` —
    /// a hung read IS the failure this finding exists to catch.
    fn scan_with_timeout(root: &Path, paths: &[&Path], secs: u64) -> Vec<SecretFinding> {
        let root = root.to_path_buf();
        let paths: Vec<std::path::PathBuf> = paths.iter().map(|p| p.to_path_buf()).collect();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let refs: Vec<&Path> = paths.iter().map(std::path::PathBuf::as_path).collect();
            let _ = tx.send(scan_paths(&root, &refs));
        });
        rx.recv_timeout(std::time::Duration::from_secs(secs))
            .expect("scan_paths must not block")
    }

    /// A worker-created FIFO must not block the checkpoint scan: the
    /// non-blocking no-follow open returns immediately, the fstat check
    /// refuses the non-regular entry, and the FIFO is skipped.
    #[cfg(unix)]
    #[test]
    fn scrub_nofollow_fifo_does_not_block_checkpoint_scan() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("planted.fifo");
        let c_path = std::ffi::CString::new(fifo.to_str().expect("utf-8 temp path")).unwrap();
        let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
        assert_eq!(rc, 0, "mkfifo failed: {}", std::io::Error::last_os_error());

        let findings = scan_with_timeout(dir.path(), &[Path::new("planted.fifo")], 10);
        assert!(
            findings.is_empty(),
            "a FIFO is skipped, never scanned: {findings:?}"
        );
    }

    /// A symlink to /dev/zero (an unbounded byte source) is never read
    /// through: the no-follow open refuses the link itself.
    #[cfg(unix)]
    #[test]
    fn scrub_nofollow_symlink_to_dev_zero_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/dev/zero", dir.path().join("zero")).unwrap();

        let findings = scan_with_timeout(dir.path(), &[Path::new("zero")], 10);
        assert!(
            findings.is_empty(),
            "a symlink to an unbounded source is skipped, never read through: {findings:?}"
        );
    }

    /// A symlinked candidate is not read through even when its target is a
    /// real file full of findings — the scan's job is the tree's own bytes,
    /// and a checked-in symlink carries none.
    #[cfg(unix)]
    #[test]
    fn scrub_nofollow_symlinked_file_is_not_read_through() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let real = outside.path().join("real.txt");
        std::fs::write(&real, SCRUB_NOFOLLOW_SECRET).unwrap();
        std::os::unix::fs::symlink(&real, dir.path().join("linked.txt")).unwrap();

        let findings = scan_paths(dir.path(), &[Path::new("linked.txt")]);
        assert!(
            findings.is_empty(),
            "a symlink is never read through: {findings:?}"
        );
        // Anti-vacuity: the same bytes scanned directly DO produce the finding.
        let findings = scan_paths(dir.path(), &[real.as_path()]);
        assert!(
            findings.iter().any(|f| f.rule_id == "anthropic-api-key"),
            "the direct scan must flag the secret: {findings:?}"
        );
    }

    /// An oversized regular file is bounded: skipped WHOLE (a partial scan
    /// would be a false sense of coverage), and the read itself is capped
    /// regardless of how the file grows. Just under the cap, the same
    /// secret scans normally.
    #[test]
    fn scrub_nofollow_oversized_file_is_skipped_and_under_cap_scans() {
        let dir = tempfile::tempdir().unwrap();
        let mut content = SCRUB_NOFOLLOW_SECRET.as_bytes().to_vec();
        content.resize(SCAN_PATH_MAX_FILE_BYTES as usize + 1, b'x');
        std::fs::write(dir.path().join("big.txt"), &content).unwrap();

        let findings = scan_with_timeout(dir.path(), &[Path::new("big.txt")], 10);
        assert!(
            findings.is_empty(),
            "an oversized file is skipped whole, never partially scanned: {findings:?}"
        );

        // Anti-vacuity: under the cap the same secret is found.
        std::fs::write(dir.path().join("small.txt"), SCRUB_NOFOLLOW_SECRET).unwrap();
        let findings = scan_paths(dir.path(), &[Path::new("small.txt")]);
        assert!(
            findings.iter().any(|f| f.rule_id == "anthropic-api-key"),
            "under-cap content still scans: {findings:?}"
        );
    }

    /// Composition audit (ticket `config-fail-open-audit`): a
    /// `.kranz/secret-allowlist` waiver is scoped to ONE (rule, value)
    /// fingerprint — it silences the exact reviewed finding and nothing
    /// else. No waiver shape disables a whole rule, so the list can only
    /// ever grow by reviewed, per-finding entries; it is a subtract-only
    /// filter over the finding stream, never a replace of the rule set.
    #[test]
    fn composition_audit_secret_allowlist_waives_one_fingerprint_never_a_rule() {
        let text_a = "sk-ant-api03-CompositionAuditValueA1";
        let text_b = "sk-ant-api03-CompositionAuditValueB2";
        let findings = scan_text(&format!("{text_a} {text_b}"));
        assert_eq!(findings.len(), 2, "both keys must be found: {findings:?}");

        // Waiving finding A leaves finding B standing under the SAME rule —
        // a waiver cannot take the rule down with it.
        let waived: std::collections::BTreeSet<String> =
            [findings[0].fingerprint.clone()].into_iter().collect();
        let remaining = filter_allowed(findings, &waived);
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].rule_id, "anthropic-api-key");

        // An empty or garbage waiver text changes nothing.
        let findings = scan_text(text_a);
        assert_eq!(
            filter_allowed(findings.clone(), &Default::default()),
            findings
        );
        let garbage = read_allowlist_text("# reviewed\nnot-a-fingerprint\n");
        assert_eq!(filter_allowed(findings.clone(), &garbage), findings);
    }
}
