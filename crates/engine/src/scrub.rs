//! Credential scrubbing and safe truncation (plan §3).
//!
//! Every transcript line passes through [`scrub`] before it is written to
//! disk or broadcast as a `worker.message` event, replacing common credential
//! shapes with `[REDACTED]`. This is defense in depth, not a guarantee — the
//! permission layer (§4.7) is the primary control.
//!
//! [`truncate_chars`] cuts long content on a `char` boundary so multibyte
//! text can never panic the engine or produce invalid UTF-8.

use regex::Regex;
use std::borrow::Cow;
use std::sync::OnceLock;

/// Marker appended by [`truncate_chars`] when content was cut.
const TRUNCATION_MARKER: &str = "… [truncated]";

/// One scrub pattern plus its replacement template. Replacements may use
/// `${1}` to preserve a captured prefix (e.g. the key name of an assignment).
struct Rule {
    re: Regex,
    replacement: &'static str,
}

/// The scrub rules, compiled once on first use. Order matters:
/// multi-line/private-key blocks go first so bodies are removed whole, and
/// specific token shapes run before the generic assignment catch-all.
fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let rule = |pattern: &str, replacement: &'static str| Rule {
            re: Regex::new(pattern).expect("static scrub regex must compile"),
            replacement,
        };
        vec![
            // PEM private key blocks — the whole block, or just the BEGIN
            // line when the END marker never arrives (partial output).
            rule(
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----|-----BEGIN [A-Z ]*PRIVATE KEY-----[^\r\n]*",
                "[REDACTED]",
            ),
            // Anthropic API keys (before the generic sk- rule).
            rule(r"\bsk-ant-[A-Za-z0-9_-]{8,}", "[REDACTED]"),
            // OpenAI-style keys.
            rule(r"\bsk-[A-Za-z0-9]{20,}", "[REDACTED]"),
            // GitHub tokens: classic (ghp_), OAuth (gho_), server (ghs_).
            rule(r"\bgh[pos]_[A-Za-z0-9]{20,}", "[REDACTED]"),
            // GitHub fine-grained PATs.
            rule(r"\bgithub_pat_[A-Za-z0-9_]{20,}", "[REDACTED]"),
            // AWS access key ids (exactly 16 chars after AKIA).
            rule(r"\bAKIA[0-9A-Z]{16}\b", "[REDACTED]"),
            // AWS secret keys in config/env form; the key name is kept.
            rule(r"(?i)\b(aws_secret_access_key\s*[=:]\s*)\S+", "${1}[REDACTED]"),
            // Slack tokens.
            rule(r"\bxox[baprs]-[A-Za-z0-9-]{10,}", "[REDACTED]"),
            // JWTs (three base64url segments).
            rule(
                r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}",
                "[REDACTED]",
            ),
            // Authorization: Bearer <token> — keep the scheme word.
            rule(r"(?i)\b(bearer\s+)[a-z0-9._~+/=-]{16,}", "${1}[REDACTED]"),
            // Generic key/secret/token/password assignments — redact only the
            // value; the key name stays so logs remain diagnosable. Values
            // shorter than 8 chars ("None", "****") are left alone.
            rule(
                r#"(?i)((?:api[_-]?key|secret|token|password|passwd|credential)["']?\s*[:=]\s*["']?)([^\s"']{8,})"#,
                "${1}[REDACTED]",
            ),
        ]
    })
}

/// Replace anything that looks like a credential with `[REDACTED]`.
///
/// For assignment-shaped matches (`api_key=...`, `aws_secret_access_key: ...`,
/// `Bearer ...`) the key name / scheme is preserved and only the secret value
/// is redacted.
pub fn scrub(text: &str) -> String {
    let mut out = text.to_owned();
    for rule in rules() {
        if let Cow::Owned(replaced) = rule.re.replace_all(&out, rule.replacement) {
            out = replaced;
        }
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
