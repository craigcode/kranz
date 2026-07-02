//! Integration tests for `kranz_engine::scrub`.
//!
//! One positive case per credential pattern (secret gone, `[REDACTED]`
//! present, surrounding text intact), false-positive guards, and
//! char-boundary-safe truncation with multibyte input.

use kranz_engine::scrub::{scrub, scrub_and_truncate, truncate_chars};

const MARKER: &str = "[REDACTED]";
const TRUNCATED: &str = "… [truncated]";

#[test]
fn anthropic_key_redacted() {
    let out = scrub("using key sk-ant-api03-AbCdEf_123-xyz for auth");
    assert_eq!(out, "using key [REDACTED] for auth");
}

#[test]
fn openai_style_key_redacted() {
    let out = scrub("openai sk-AbCdEfGhIjKlMnOpQrStUvWx0123 end");
    assert!(!out.contains("sk-AbCdEf"), "{out}");
    assert!(out.contains(MARKER));
    assert!(out.starts_with("openai "));
    assert!(out.ends_with(" end"));
}

#[test]
fn github_tokens_redacted() {
    let input = "classic ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ123456 \
                 oauth gho_abcdefghijklmnopqrstuv123 \
                 server ghs_ZYXWVUTSRQPONMLKJIHGFE99 \
                 fine github_pat_11ABCDEFG0_abcdefghijklmnopqrs done";
    let out = scrub(input);
    assert!(!out.contains("ghp_"), "{out}");
    assert!(!out.contains("gho_"), "{out}");
    assert!(!out.contains("ghs_"), "{out}");
    assert!(!out.contains("github_pat_"), "{out}");
    assert_eq!(out.matches(MARKER).count(), 4, "{out}");
    assert!(out.starts_with("classic "));
    assert!(out.ends_with(" done"));
}

#[test]
fn aws_access_key_id_redacted() {
    let out = scrub("aws_access_key_id = AKIAIOSFODNN7EXAMPLE\n");
    assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"), "{out}");
    assert!(out.contains(MARKER));
    assert!(out.contains("aws_access_key_id"));
}

#[test]
fn aws_secret_access_key_redacted_key_name_kept() {
    let out = scrub("aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY");
    assert!(!out.contains("wJalrXUtnFEMI"), "{out}");
    assert!(out.contains("aws_secret_access_key"), "key name must survive: {out}");
    assert!(out.contains(MARKER));
}

#[test]
fn slack_token_redacted() {
    let out = scrub("posting with xoxb-123456789012-abcdefghijkl now");
    assert!(!out.contains("xoxb-"), "{out}");
    assert!(out.contains(MARKER));
    assert!(out.starts_with("posting with "));
    assert!(out.ends_with(" now"));
}

#[test]
fn jwt_redacted() {
    let jwt = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.\
               eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4ifQ.\
               TJVA95OrM7E2cBab30RMHrHDcEfxjoYZgeFONFh7HgQ";
    let jwt: String = jwt.split_whitespace().collect();
    let out = scrub(&format!("jwt {jwt} end"));
    assert!(!out.contains("eyJ"), "{out}");
    assert!(out.contains(MARKER));
    assert_eq!(out, format!("jwt {MARKER} end"));
}

#[test]
fn bearer_header_redacted_scheme_kept() {
    let out = scrub("Authorization: Bearer abcdef1234567890abcdef\nnext line");
    assert!(!out.contains("abcdef1234567890abcdef"), "{out}");
    assert!(out.contains("Bearer [REDACTED]"), "scheme word must survive: {out}");
    assert!(out.contains("Authorization"));
    assert!(out.ends_with("next line"));
}

#[test]
fn generic_assignments_keep_key_name() {
    let cases = [
        (r#"api_key = "abcd1234efgh5678""#, "api_key", "abcd1234efgh5678"),
        ("password: hunter2hunter2", "password", "hunter2hunter2"),
        ("export MY_TOKEN=deadbeefcafe1234", "MY_TOKEN=", "deadbeefcafe1234"),
        ("secret: velvetunderground", "secret", "velvetunderground"),
        ("passwd=correcthorsebattery", "passwd", "correcthorsebattery"),
        ("credential: aVeryLongValue123", "credential", "aVeryLongValue123"),
    ];
    for (input, key, value) in cases {
        let out = scrub(input);
        assert!(out.contains(key), "key name lost in {input:?} -> {out:?}");
        assert!(!out.contains(value), "secret survived in {input:?} -> {out:?}");
        assert!(out.contains(MARKER), "no marker in {input:?} -> {out:?}");
    }
}

#[test]
fn private_key_block_redacted_multiline() {
    let text = "before\n-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA7x8\nQIDAQAB\n-----END RSA PRIVATE KEY-----\nafter";
    let out = scrub(text);
    assert_eq!(out, "before\n[REDACTED]\nafter");
}

#[test]
fn private_key_begin_line_redacted_when_end_missing() {
    let text = "x\n-----BEGIN OPENSSH PRIVATE KEY-----\nrest of output";
    let out = scrub(text);
    assert!(!out.contains("BEGIN OPENSSH PRIVATE KEY"), "{out}");
    assert!(out.contains(MARKER));
    assert!(out.starts_with("x\n"));
    assert!(out.ends_with("rest of output"));
}

#[test]
fn false_positives_left_untouched() {
    let benign = [
        "token = None",
        "password: ****",
        "The token bucket algorithm is a rate limiter.",
        "no secrets here, just plain prose about git and tests.",
        "she asked for the password policy document",
    ];
    for text in benign {
        assert_eq!(scrub(text), text, "benign text was modified");
    }
}

#[test]
fn multiple_secrets_in_one_text() {
    let out = scrub("id AKIAIOSFODNN7EXAMPLE and pat ghp_ABCDEFGHIJKLMNOPQRST1234 done");
    assert_eq!(out.matches(MARKER).count(), 2, "{out}");
    assert!(out.starts_with("id "));
    assert!(out.ends_with(" done"));
}

// ---------------------------------------------------------------------------
// truncate_chars
// ---------------------------------------------------------------------------

#[test]
fn truncate_noop_when_short_enough() {
    let s = "héllo".repeat(4); // 20 chars, 24 bytes
    assert_eq!(truncate_chars(&s, 20), s, "exactly max chars must not truncate");
    assert_eq!(truncate_chars(&s, 100), s);
    assert_eq!(truncate_chars("", 0), "");
}

#[test]
fn truncate_cuts_on_char_boundary_with_multibyte() {
    let s = "héllo".repeat(10); // 50 chars; 'é' is 2 bytes
    let out = truncate_chars(&s, 7);
    assert!(out.starts_with("héllohé"), "{out}");
    assert!(out.ends_with(TRUNCATED), "{out}");
    let kept: usize = out.chars().count() - TRUNCATED.chars().count();
    assert_eq!(kept, 7, "must keep exactly max chars: {out}");
}

#[test]
fn truncate_at_zero_keeps_only_marker() {
    assert_eq!(truncate_chars("abc", 0), TRUNCATED);
}

#[test]
fn scrub_and_truncate_scrubs_before_cutting() {
    // 40-char key body: raw text is long, scrubbed text is 19 chars.
    let input = format!("key sk-ant-{} tail", "a".repeat(40));
    let out = scrub_and_truncate(&input, 18);
    assert!(out.contains(MARKER), "secret must be scrubbed before the cut: {out}");
    assert!(!out.contains("sk-ant-"), "{out}");
    assert!(out.ends_with(TRUNCATED), "{out}");
}
