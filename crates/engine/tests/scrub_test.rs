//! Integration tests for `kranz_engine::scrub`.
//!
//! One positive case per credential pattern (secret gone, `[REDACTED]`
//! present, surrounding text intact), false-positive guards, and
//! char-boundary-safe truncation with multibyte input.

use kranz_engine::scrub::{
    filter_allowed, read_allowlist_text, scan_text, scan_text_at, scan_unified_diff, scrub,
    scrub_and_truncate, scrub_with_findings, truncate_chars,
};
use std::path::{Path, PathBuf};
use std::time::Instant;

const MARKER: &str = "[REDACTED]";
const TRUNCATED: &str = "… [truncated]";

#[test]
fn anthropic_key_redacted() {
    let out = scrub("using key sk-ant-api03-AbCdEf_123-xyz for auth");
    assert_eq!(out, "using key [REDACTED] for auth");
}

#[test]
fn scanner_reports_rule_id_fingerprint_and_no_secret_value() {
    let secret = "sk-ant-api03-AbCdEf_123-xyz";
    let scan = scrub_with_findings(&format!("using key {secret}"), "unit");

    assert_eq!(scan.redacted, "using key [REDACTED]");
    assert_eq!(scan.findings.len(), 1);
    let finding = &scan.findings[0];
    assert_eq!(finding.rule_id, "anthropic-api-key");
    assert_eq!(finding.location, "unit");
    assert!(!finding.fingerprint.contains(secret));
    assert_eq!(finding.fingerprint.len(), 24);
}

#[test]
fn scanner_context_rules_point_at_secret_span_only() {
    let token = "abcdEFGHijklMNOPqrstUVWX1234";
    let text = format!("Authorization: Bearer {token}");
    let scan = scrub_with_findings(&text, "headers");

    assert_eq!(scan.redacted, "Authorization: Bearer [REDACTED]");
    assert_eq!(scan.findings.len(), 1);
    let finding = &scan.findings[0];
    assert_eq!(finding.rule_id, "authorization-bearer");
    assert_eq!(&text[finding.start..finding.end], token);
}

#[test]
fn scanner_allowlist_filters_by_fingerprint() {
    let finding = scan_text("token = sk-AbCdEfGhIjKlMnOpQrStUvWx0123")
        .into_iter()
        .next()
        .expect("secret finding");
    let allowed = read_allowlist_text(&format!("# reviewed\n{}\n", finding.fingerprint));
    assert!(filter_allowed(vec![finding], &allowed).is_empty());
}

#[test]
fn unified_diff_scan_only_checks_added_lines() {
    let old_secret = "sk-ant-api03-OldSecret_123456";
    let new_secret = "sk-ant-api03-NewSecret_123456";
    let diff = format!(
        "diff --git a/.env b/.env\n--- a/.env\n+++ b/.env\n@@ -1,2 +1,2 @@\n-{old_secret}\n context\n+{new_secret}\n"
    );

    let findings = scan_unified_diff(&diff);

    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].rule_id, "anthropic-api-key");
    assert_eq!(findings[0].location, ".env:2");
}

#[test]
fn unified_diff_scan_suppresses_only_generic_generated_bundle_noise() {
    // Machine-generated assignments may trip the generic heuristic, but
    // high-confidence credential patterns in the same bundle must survive.
    let secret = "sk-ant-api03-NewSecret_123456";
    let generated_value = "MinifiedIdentifier_A1B2C3D4E5F6";
    let bundle = "crates/cli/assets/dashboard/dist/assets/index-abc123.js";
    let diff = format!(
        "diff --git a/{bundle} b/{bundle}\n--- a/{bundle}\n+++ b/{bundle}\n@@ -0,0 +1,2 @@\n+token=\"{generated_value}\"\n+token=\"{secret}\"\ndiff --git a/src/main.ts b/src/main.ts\n--- a/src/main.ts\n+++ b/src/main.ts\n@@ -0,0 +1 @@\n+token=\"{generated_value}\"\n"
    );

    let findings = scan_unified_diff(&diff);

    assert_eq!(findings.len(), 2);
    assert_eq!(findings[0].rule_id, "anthropic-api-key");
    assert_eq!(findings[0].location, format!("{bundle}:2"));
    assert_eq!(findings[1].rule_id, "generic-secret-assignment");
    assert_eq!(findings[1].location, "src/main.ts:1");
}

#[test]
fn ingest_scanner_leaves_legitimate_high_entropy_operational_text() {
    let text = "\
commit 4b825dc642cb6eb9a060e54bf8d69288fbee4904
artifact_sha256 = 04f8996da763b7a969b1028ee3007569eaf89c5b7cf7f9633d368973b66a5058
base64 fixture: VGhpcyBpcyBhIG5vbi1zZWNyZXQgdGVzdCBmaXh0dXJlIHBheWxvYWQ=
diff --git a/src/lib.rs b/src/lib.rs
@@ -1,2 +1,2 @@
-let old_revision = \"f572d396fae9206628714fb2ce00f72e94f2258f\";
+let new_revision = \"3b18e3e9c4d90f19aa6e7e64bb9d5a9f5ce7b443\";
config = {\"backend\":\"codex\",\"model\":\"gpt-5-codex\",\"sandbox\":{\"enforce\":\"fs+net\"}}
";

    let scan = scrub_with_findings(text, "fixture");

    assert_eq!(scan.redacted, text);
    assert!(scan.findings.is_empty(), "{:?}", scan.findings);
}

#[test]
fn ingest_scanner_leaves_committed_mission_reports_unchanged() {
    let started = Instant::now();
    let repo = workspace_root();
    let reports = collect_named_files(&repo.join(".kranz").join("missions"), "report.md");

    assert!(
        reports.len() >= 5,
        "expected committed mission reports to audit; found {}",
        reports.len()
    );
    for report in &reports {
        let text = std::fs::read_to_string(report).expect("read committed report");
        let location = rel_path(&repo, report);
        let findings = scan_text_at(&text, &location);
        assert!(
            findings.is_empty(),
            "ingest scanner would redact legitimate committed report content in {location}: {findings:?}"
        );
    }
    eprintln!(
        "audited {} committed mission reports in {:?}",
        reports.len(),
        started.elapsed()
    );
}

#[test]
#[ignore = "audits gitignored local runtime events.jsonl logs from a lived-in checkout"]
fn ingest_scanner_audits_local_runtime_artifacts() {
    let started = Instant::now();
    let repo = workspace_root();
    let missions = repo.join(".kranz").join("missions");
    let mut artifacts = collect_named_files(&missions, "report.md");
    artifacts.extend(collect_named_files(&missions, "events.jsonl"));
    artifacts.sort();

    assert!(
        !artifacts.is_empty(),
        "expected at least one local mission artifact under {}",
        missions.display()
    );
    let mut bytes = 0usize;
    for path in &artifacts {
        let text = std::fs::read_to_string(path).expect("read local mission artifact");
        bytes += text.len();
        let location = rel_path(&repo, path);
        let findings = scan_text_at(&text, &location);
        assert!(
            findings.is_empty(),
            "ingest scanner would redact local mission artifact content in {location}: {findings:?}"
        );
    }
    eprintln!(
        "audited {} local mission artifacts ({} bytes) in {:?}",
        artifacts.len(),
        bytes,
        started.elapsed()
    );
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
    assert!(
        out.contains("aws_secret_access_key"),
        "key name must survive: {out}"
    );
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
    assert!(
        out.contains("Bearer [REDACTED]"),
        "scheme word must survive: {out}"
    );
    assert!(out.contains("Authorization"));
    assert!(out.ends_with("next line"));
}

#[test]
fn generic_assignments_keep_key_name() {
    let cases = [
        (
            r#"api_key = "abcd1234efgh5678""#,
            "api_key",
            "abcd1234efgh5678",
        ),
        ("password: hunter2hunter2", "password", "hunter2hunter2"),
        (
            "export MY_TOKEN=deadbeefcafe1234",
            "MY_TOKEN=",
            "deadbeefcafe1234",
        ),
        ("secret: velvetunderground", "secret", "velvetunderground"),
        (
            "passwd=correcthorsebattery",
            "passwd",
            "correcthorsebattery",
        ),
        (
            "credential: aVeryLongValue123",
            "credential",
            "aVeryLongValue123",
        ),
    ];
    for (input, key, value) in cases {
        let out = scrub(input);
        assert!(out.contains(key), "key name lost in {input:?} -> {out:?}");
        assert!(
            !out.contains(value),
            "secret survived in {input:?} -> {out:?}"
        );
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
// M5: new vendor-specific patterns — one positive per pattern.
// ---------------------------------------------------------------------------

#[test]
fn google_api_key_redacted() {
    // Google API keys are AIza + exactly 35 chars (39 total).
    let out = scrub("key AIzaSyA1B2C3D4E5F6G7H8I9J0K1L2M3N4O5P6z end");
    assert!(!out.contains("AIzaSy"), "{out}");
    assert!(out.contains(MARKER));
    assert!(out.starts_with("key "));
    assert!(out.ends_with(" end"));
}

#[test]
fn gcp_service_account_private_key_json_redacted() {
    let input = r#"{"type":"service_account","private_key":"-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBg\n-----END PRIVATE KEY-----\n","client_email":"x@y.iam"}"#;
    let out = scrub(input);
    assert!(!out.contains("MIIEvQIBADANBg"), "PEM body survived: {out}");
    assert!(!out.contains("BEGIN PRIVATE KEY"), "{out}");
    assert!(
        out.contains("private_key"),
        "field name must survive: {out}"
    );
    assert!(out.contains(MARKER));
    // Non-secret sibling fields are untouched.
    assert!(out.contains("service_account"), "{out}");
    assert!(out.contains("client_email"), "{out}");
}

#[test]
fn stripe_live_keys_redacted() {
    let input = "secret sk_live_4eC39HqLyjWDarjtT1zdp7dc \
                 restricted rk_live_51H8xAbCdEfGhIjKlMnOpQrS \
                 pub pk_live_9zYxWvUtSrQpOnMlKjIhGfEd done";
    let out = scrub(input);
    assert!(!out.contains("sk_live_"), "{out}");
    assert!(!out.contains("rk_live_"), "{out}");
    assert!(!out.contains("pk_live_"), "{out}");
    assert_eq!(out.matches(MARKER).count(), 3, "{out}");
    assert!(out.starts_with("secret "));
    assert!(out.ends_with(" done"));
}

#[test]
fn npm_token_redacted() {
    let out = scrub("npm token npm_abcdefghijklmnopqrstuvwxyz0123456789 done");
    assert!(!out.contains("npm_abcdef"), "{out}");
    assert!(out.contains(MARKER));
    assert!(out.starts_with("npm token "));
    assert!(out.ends_with(" done"));
}

#[test]
fn openai_project_key_redacted() {
    let out = scrub("key sk-proj-AbCd1234_efGh5678-ijKl90mnOpQr end");
    assert!(!out.contains("sk-proj-"), "{out}");
    assert!(out.contains(MARKER));
    assert!(out.starts_with("key "));
    assert!(out.ends_with(" end"));
}

#[test]
fn basic_auth_header_redacted_scheme_kept() {
    let out = scrub("Authorization: Basic dXNlcm5hbWU6cGFzc3dvcmQxMjM=\nnext line");
    assert!(!out.contains("dXNlcm5hbWU6cGFzc3dvcmQxMjM="), "{out}");
    assert!(
        out.contains("Basic [REDACTED]"),
        "scheme word must survive: {out}"
    );
    assert!(out.contains("Authorization"));
    assert!(out.ends_with("next line"));
}

#[test]
fn connection_string_password_redacted_user_host_kept() {
    let out = scrub("postgres://appuser:s3cr3tP@ssw0rd@db.internal:5432/prod");
    assert!(!out.contains("s3cr3tP"), "password survived: {out}");
    assert!(out.contains("appuser"), "user must survive: {out}");
    assert!(
        out.contains("db.internal:5432/prod"),
        "host must survive: {out}"
    );
    assert!(out.contains("postgres://appuser:[REDACTED]@"), "{out}");
}

// ---------------------------------------------------------------------------
// M5: entropy-gated redaction and its guards.
// ---------------------------------------------------------------------------

#[test]
fn entropy_rule_redacts_bare_high_entropy_value_in_key_context() {
    // access_token is only in the entropy pass's name list (not the generic
    // rule), so this exercises the entropy heuristic specifically: a 40-char
    // base64 blob with no recognizable vendor prefix.
    let out = scrub("access_token = aB3xQ9zK7mP2wR5tY8uV1nJ4kL6dF0sGhWqZ7xC");
    assert!(
        !out.contains("aB3xQ9zK7mP2wR5tY8uV1nJ4kL6dF0sGhWqZ7xC"),
        "{out}"
    );
    assert!(out.contains("access_token"), "key name must survive: {out}");
    assert!(out.contains(MARKER));
}

#[test]
fn entropy_rule_declines_low_entropy_value() {
    // A long but low-entropy value under an entropy-only key name ("auth",
    // which contains no generic secret substring) is NOT redacted — it does
    // not clear the 4.0 bits/char bar.
    let text = "auth = abababababababababababababababab";
    let out = scrub(text);
    assert!(
        out.contains("abababababababababababababababab"),
        "low-entropy redacted: {out}"
    );
    assert!(!out.contains(MARKER), "{out}");
}

#[test]
fn entropy_rule_leaves_high_entropy_looking_word_in_prose() {
    // No key-name context: the entropy pass must not fire, even though the
    // word is longish and mixed-case.
    let prose = "The Supercalifragilisticexpialidocious algorithm ran overnight.";
    assert_eq!(scrub(prose), prose, "prose was modified: {}", scrub(prose));
}

#[test]
fn entropy_rule_leaves_git_sha_after_commit() {
    // A 40-hex git SHA after "commit" is allowlisted and must survive even
    // though it is long and hex.
    let text = "commit da39a3ee5e6b4b0d3255bfef95601890afd80709 landed the fix";
    assert_eq!(scrub(text), text, "git SHA was redacted: {}", scrub(text));
}

#[test]
fn entropy_rule_leaves_uuid_in_key_context() {
    // Even assigned to a token-ish name, a canonical UUID is allowlisted.
    let text = "session_token: 550e8400-e29b-41d4-a716-446655440000";
    let out = scrub(text);
    assert!(
        out.contains("550e8400-e29b-41d4-a716-446655440000"),
        "UUID redacted: {out}"
    );
    assert!(!out.contains(MARKER), "{out}");
}

#[test]
fn entropy_rule_leaves_placeholder_in_key_context() {
    // Placeholder value assigned to api_key: allowlisted, not redacted.
    let text = "api_key = REPLACE_ME_WITH_YOUR_KEY_1234567890";
    let out = scrub(text);
    assert!(out.contains("REPLACE_ME_WITH_YOUR_KEY_1234567890"), "{out}");
}

// ---------------------------------------------------------------------------
// truncate_chars
// ---------------------------------------------------------------------------

#[test]
fn truncate_noop_when_short_enough() {
    let s = "héllo".repeat(4); // 20 chars, 24 bytes
    assert_eq!(
        truncate_chars(&s, 20),
        s,
        "exactly max chars must not truncate"
    );
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
    assert!(
        out.contains(MARKER),
        "secret must be scrubbed before the cut: {out}"
    );
    assert!(!out.contains("sk-ant-"), "{out}");
    assert!(out.ends_with(TRUNCATED), "{out}");
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("engine crate lives under crates/engine")
        .to_path_buf()
}

fn collect_named_files(root: &Path, name: &str) -> Vec<PathBuf> {
    fn walk(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, name, out);
            } else if path.file_name().and_then(|s| s.to_str()) == Some(name) {
                out.push(path);
            }
        }
    }

    let mut out = Vec::new();
    walk(root, name, &mut out);
    out.sort();
    out
}

fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}
