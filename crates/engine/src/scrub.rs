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
use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::OnceLock;

/// Marker appended by [`truncate_chars`] when content was cut.
const TRUNCATION_MARKER: &str = "… [truncated]";

/// Replacement marker written in place of a redacted secret.
const REDACTED: &str = "[REDACTED]";

/// One scrub pattern plus its replacement template. Replacements may use
/// `${1}` (and `${2}`) to preserve captured context (e.g. the key name of an
/// assignment, or the `user:`/`@host` framing of a connection string).
struct Rule {
    re: Regex,
    replacement: &'static str,
}

/// The scrub rules, compiled once on first use. See the module docs for the
/// ordering contract; briefly: private-key blocks first, then vendor-specific
/// token shapes, then credential headers and connection strings, then the
/// generic assignment catch-all. The entropy pass is applied separately in
/// [`scrub`] *after* these run.
fn rules() -> &'static [Rule] {
    static RULES: OnceLock<Vec<Rule>> = OnceLock::new();
    RULES.get_or_init(|| {
        let rule = |pattern: &str, replacement: &'static str| Rule {
            re: Regex::new(pattern).expect("static scrub regex must compile"),
            replacement,
        };
        vec![
            // 1. PEM private key blocks — the whole block, or just the BEGIN
            //    line when the END marker never arrives (partial output).
            rule(
                r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----|-----BEGIN [A-Z ]*PRIVATE KEY-----[^\r\n]*",
                REDACTED,
            ),
            // 2. GCP service-account JSON `"private_key": "-----BEGIN...\n..."`.
            //    The PEM body is escaped onto one line, so the multi-line rule
            //    above misses it. Keep the field name, redact the value.
            rule(
                r#"(?i)("private_key"\s*:\s*")-----BEGIN[^"]*"#,
                "${1}[REDACTED]",
            ),
            // 3a. Anthropic API keys (before the generic sk- rule).
            rule(r"\bsk-ant-[A-Za-z0-9_-]{8,}", REDACTED),
            // 3b. OpenAI project keys: sk-proj-<body>. Listed before the plain
            //     sk- rule because the body contains `-`/`_` which the plain
            //     rule would stop at, leaving a tail behind.
            rule(r"\bsk-proj-[A-Za-z0-9_-]{20,}", REDACTED),
            // 3c. OpenAI-style keys (plain sk-...).
            rule(r"\bsk-[A-Za-z0-9]{20,}", REDACTED),
            // 3d. Google API keys (AIza + 35 chars).
            rule(r"\bAIza[0-9A-Za-z_-]{35}\b", REDACTED),
            // 3e. Stripe live/restricted/publishable keys.
            rule(r"\b(?:sk|rk|pk)_live_[0-9A-Za-z]{16,}", REDACTED),
            // 3f. npm access tokens (npm_ + 36 chars).
            rule(r"\bnpm_[0-9A-Za-z]{36}\b", REDACTED),
            // 3g. GitHub tokens: classic (ghp_), OAuth (gho_), server (ghs_).
            rule(r"\bgh[pos]_[A-Za-z0-9]{20,}", REDACTED),
            // 3h. GitHub fine-grained PATs.
            rule(r"\bgithub_pat_[A-Za-z0-9_]{20,}", REDACTED),
            // 3i. AWS access key ids (exactly 16 chars after AKIA).
            rule(r"\bAKIA[0-9A-Z]{16}\b", REDACTED),
            // 3j. AWS secret keys in config/env form; the key name is kept.
            rule(r"(?i)\b(aws_secret_access_key\s*[=:]\s*)\S+", "${1}[REDACTED]"),
            // 3k. Slack tokens.
            rule(r"\bxox[baprs]-[A-Za-z0-9-]{10,}", REDACTED),
            // 3l. JWTs (three base64url segments).
            rule(
                r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{5,}",
                REDACTED,
            ),
            // 4a. Authorization: Bearer <token> — keep the scheme word.
            rule(r"(?i)\b(bearer\s+)[a-z0-9._~+/=-]{16,}", "${1}[REDACTED]"),
            // 4b. Authorization: Basic <base64> — keep the scheme word.
            rule(r"(?i)\b(basic\s+)[a-z0-9+/]{16,}={0,2}", "${1}[REDACTED]"),
            // 5. Connection strings with an embedded password:
            //    scheme://user:PASSWORD@host. Redact only the password segment;
            //    the `user:` prefix and `@host` remainder are preserved.
            rule(
                r"([a-zA-Z][a-zA-Z0-9+.-]*://[^\s:/@]+:)[^\s:/@]+(@)",
                "${1}[REDACTED]${2}",
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
pub fn scrub(text: &str) -> String {
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
}
