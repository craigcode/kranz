//! External trigger core (design D-F, ticket `trigger-ci-pr-fix-mission`):
//! HMAC verification, the event allowlist + payload rules, dedup, provenance
//! in the drafted ticket, and the source-level "no git-mutation path"
//! assertion. All tests are `ghook_`-prefixed so the contract filter
//! (`cargo test --workspace ghook`) matches this work and nothing else.

use kranz_engine::hooks::{self, Consent, HooksConfig, Trigger, TriggerDraft, TriggerKind};
use kranz_engine::ticket::{Ticket, TicketState};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Once;
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Environment isolation (load_hooks reads ~/.kranz/config.json)
// ---------------------------------------------------------------------------

static ENV_ISOLATION: Once = Once::new();

/// Point HOME/USERPROFILE at an empty dir so the real global config can never
/// leak a `hooks` key into these tests.
fn isolate_home() {
    ENV_ISOLATION.call_once(|| {
        let home =
            std::env::temp_dir().join(format!("kranz-hooks-test-home-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&home);
        std::env::set_var(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, &home);
    });
}

fn cfg_with_secret() -> HooksConfig {
    HooksConfig {
        secret: Some("whsec-test".to_string()),
        ..HooksConfig::default()
    }
}

// ---------------------------------------------------------------------------
// Payload fixtures (GitHub webhook shapes)
// ---------------------------------------------------------------------------

fn workflow_run_payload(action: &str, conclusion: &str, head_branch: &str) -> Value {
    json!({
        "action": action,
        "workflow_run": {
            "id": 9876543210_u64,
            "name": "gates",
            "head_branch": head_branch,
            "head_sha": "0123456789abcdef0123456789abcdef01234567",
            "conclusion": conclusion,
            "html_url": "https://github.com/octo/hello/actions/runs/9876543210",
            "actor": { "login": "octocat" }
        },
        "repository": { "full_name": "octo/hello", "default_branch": "main" },
        "sender": { "login": "octocat" }
    })
}

fn issue_comment_payload(body: &str, on_pr: bool) -> Value {
    let issue = if on_pr {
        json!({ "number": 42, "pull_request": { "html_url": "https://github.com/octo/hello/pull/42" } })
    } else {
        json!({ "number": 42 })
    };
    json!({
        "action": "created",
        "issue": issue,
        "comment": {
            "body": body,
            "html_url": "https://github.com/octo/hello/pull/42#issuecomment-7",
            "user": { "login": "reviewer" }
        },
        "repository": { "full_name": "octo/hello", "default_branch": "main" },
        "sender": { "login": "reviewer" }
    })
}

fn review_comment_payload(body: &str) -> Value {
    json!({
        "action": "created",
        "pull_request": { "number": 42 },
        "comment": {
            "body": body,
            "html_url": "https://github.com/octo/hello/pull/42#discussion_r7",
            "user": { "login": "reviewer" }
        },
        "repository": { "full_name": "octo/hello", "default_branch": "main" },
        "sender": { "login": "reviewer" }
    })
}

// ---------------------------------------------------------------------------
// HMAC-SHA256 (RFC 2104) + header verification
// ---------------------------------------------------------------------------

#[test]
fn ghook_hmac_sha256_matches_rfc4231_vectors() {
    // Vectors from RFC 4231 (verified independently against python3 hmac).
    assert_eq!(
        hooks::hmac_sha256_hex(&[0x0b; 20], b"Hi There"),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    assert_eq!(
        hooks::hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    assert_eq!(
        hooks::hmac_sha256_hex(&[0xaa; 20], &[0xdd; 50]),
        "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe"
    );
}

#[test]
fn ghook_verify_signature_accepts_valid_and_refuses_invalid() {
    let body = br#"{"action":"completed"}"#;
    let good = format!("sha256={}", hooks::hmac_sha256_hex(b"whsec-test", body));
    assert!(hooks::verify_signature("whsec-test", body, Some(&good)));

    // Wrong secret.
    assert!(!hooks::verify_signature("whsec-other", body, Some(&good)));
    // Missing `sha256=` prefix.
    assert!(!hooks::verify_signature(
        "whsec-test",
        body,
        Some(good.trim_start_matches("sha256="))
    ));
    // Garbage hex.
    assert!(!hooks::verify_signature(
        "whsec-test",
        body,
        Some("sha256=zzzz")
    ));
    // Missing header.
    assert!(!hooks::verify_signature("whsec-test", body, None));
    // Truncated digest (prefix of the real one).
    assert!(!hooks::verify_signature(
        "whsec-test",
        body,
        Some(&good[..good.len() - 2])
    ));
}

// ---------------------------------------------------------------------------
// parse_trigger — workflow_run
// ---------------------------------------------------------------------------

#[test]
fn ghook_parse_workflow_run_accepts_failure_on_default_branch() {
    let payload = workflow_run_payload("completed", "failure", "main");
    let trigger = hooks::parse_trigger("workflow_run", &payload, &cfg_with_secret())
        .expect("a default-branch failure is a trigger");

    assert_eq!(trigger.kind, TriggerKind::CiFailure);
    assert_eq!(trigger.dedup_id, "9876543210");
    assert_eq!(
        trigger.source_url,
        "https://github.com/octo/hello/actions/runs/9876543210"
    );
    assert_eq!(trigger.actor, "octocat");
    assert_eq!(trigger.consent, Consent::DraftOnly);
    assert!(trigger.excerpt.contains("workflow: gates"));
    assert!(trigger.excerpt.contains("branch: main"));
    assert!(trigger.excerpt.contains("conclusion: failure"));
}

#[test]
fn ghook_parse_workflow_run_accepts_mission_branch_failure() {
    let payload = workflow_run_payload("completed", "failure", "kranz/mission-m-abc123");
    let trigger = hooks::parse_trigger("workflow_run", &payload, &cfg_with_secret())
        .expect("a mission-branch failure is a trigger");
    assert_eq!(trigger.kind, TriggerKind::CiFailure);
}

#[test]
fn ghook_parse_workflow_run_ignores_non_matching_events() {
    let cfg = cfg_with_secret();
    // A successful run is not a trigger.
    assert!(hooks::parse_trigger(
        "workflow_run",
        &workflow_run_payload("completed", "success", "main"),
        &cfg
    )
    .is_none());
    // A still-running workflow (requested/in_progress action) is not a trigger.
    assert!(hooks::parse_trigger(
        "workflow_run",
        &workflow_run_payload("requested", "failure", "main"),
        &cfg
    )
    .is_none());
    // A failure on an unrelated branch is not a trigger.
    assert!(hooks::parse_trigger(
        "workflow_run",
        &workflow_run_payload("completed", "failure", "feature-x"),
        &cfg
    )
    .is_none());
}

// ---------------------------------------------------------------------------
// parse_trigger — PR comments
// ---------------------------------------------------------------------------

#[test]
fn ghook_parse_comment_fix_label_is_draft_only() {
    let payload = issue_comment_payload("this broke the build, kranz:fix please", true);
    let trigger = hooks::parse_trigger("issue_comment", &payload, &cfg_with_secret())
        .expect("a fix-labelled PR comment is a trigger");

    assert_eq!(trigger.kind, TriggerKind::PrComment);
    assert_eq!(trigger.dedup_id, "42");
    assert_eq!(trigger.consent, Consent::DraftOnly);
    assert!(!trigger.consent.then_enqueue());
    assert_eq!(
        trigger.source_url,
        "https://github.com/octo/hello/pull/42#issuecomment-7"
    );
    assert_eq!(trigger.actor, "reviewer");
    assert!(trigger.excerpt.contains("kranz:fix"));
}

#[test]
fn ghook_parse_comment_queue_label_pre_consents_and_wins_over_fix_label() {
    let cfg = cfg_with_secret();
    // The queue label alone pre-consents…
    let payload = issue_comment_payload("kranz:fix-and-queue", true);
    let trigger = hooks::parse_trigger("issue_comment", &payload, &cfg)
        .expect("a queue-labelled PR comment is a trigger");
    assert_eq!(trigger.consent, Consent::PreConsentedQueue);
    assert!(trigger.consent.then_enqueue());

    // …and because the queue label starts with the fix label, a body holding
    // both must still read as pre-consent (checked queue-first).
    let payload = issue_comment_payload("kranz:fix and also kranz:fix-and-queue", true);
    let trigger = hooks::parse_trigger("issue_comment", &payload, &cfg).unwrap();
    assert_eq!(trigger.consent, Consent::PreConsentedQueue);
}

#[test]
fn ghook_parse_review_comment_uses_pull_request_number() {
    let payload = review_comment_payload("kranz:fix this nit");
    let trigger = hooks::parse_trigger("pull_request_review_comment", &payload, &cfg_with_secret())
        .expect("a fix-labelled review comment is a trigger");
    assert_eq!(trigger.dedup_id, "42");
    assert_eq!(trigger.consent, Consent::DraftOnly);
}

#[test]
fn ghook_parse_comment_ignores_non_pr_labelless_and_non_created() {
    let cfg = cfg_with_secret();
    // A comment on a plain issue (no pull_request key) is not a trigger.
    assert!(hooks::parse_trigger(
        "issue_comment",
        &issue_comment_payload("kranz:fix", false),
        &cfg
    )
    .is_none());
    // No label, no trigger.
    assert!(hooks::parse_trigger(
        "issue_comment",
        &issue_comment_payload("looks good", true),
        &cfg
    )
    .is_none());
    // Only `created` comments trigger (edited/deleted do not).
    let mut payload = issue_comment_payload("kranz:fix", true);
    payload["action"] = json!("edited");
    assert!(hooks::parse_trigger("issue_comment", &payload, &cfg).is_none());
}

#[test]
fn ghook_parse_trigger_rejects_unallowlisted_event_kinds() {
    let cfg = cfg_with_secret();
    let payload = json!({ "ref": "refs/heads/main", "repository": { "full_name": "octo/hello" } });
    assert!(hooks::parse_trigger("push", &payload, &cfg).is_none());
    assert!(hooks::parse_trigger("", &payload, &cfg).is_none());
}

#[test]
fn ghook_comment_excerpt_is_bounded_and_scrubbed() {
    let token = "ghp_abcdefghij1234567890ABCD";
    let long = format!("kranz:fix\n{token}\n{}", "x".repeat(5000));
    let payload = issue_comment_payload(&long, true);
    let trigger = hooks::parse_trigger("issue_comment", &payload, &cfg_with_secret()).unwrap();

    assert!(
        trigger.excerpt.chars().count() <= 1050,
        "excerpt is capped (plus truncation marker), got {}",
        trigger.excerpt.chars().count()
    );
    assert!(
        !trigger.excerpt.contains(token),
        "the GitHub token shape must be scrubbed out"
    );
    assert!(trigger.excerpt.contains("[REDACTED]"));
}

// ---------------------------------------------------------------------------
// Repository identity
// ---------------------------------------------------------------------------

#[test]
fn ghook_remote_full_name_parsing() {
    assert_eq!(
        hooks::github_full_name_from_remote("git@github.com:octo/hello.git"),
        Some("octo/hello".to_string())
    );
    assert_eq!(
        hooks::github_full_name_from_remote("https://github.com/octo/hello.git"),
        Some("octo/hello".to_string())
    );
    assert_eq!(
        hooks::github_full_name_from_remote("https://github.com/octo/hello"),
        Some("octo/hello".to_string())
    );
    assert_eq!(
        hooks::github_full_name_from_remote("ssh://git@github.com/octo/hello.git"),
        Some("octo/hello".to_string())
    );
    // Non-GitHub hosts and odd shapes cannot establish identity.
    assert_eq!(
        hooks::github_full_name_from_remote("git@gitlab.com:octo/hello.git"),
        None
    );
    assert_eq!(
        hooks::github_full_name_from_remote("https://example.com/octo/hello"),
        None
    );
    assert_eq!(hooks::github_full_name_from_remote("/local/path"), None);
    assert_eq!(
        hooks::github_full_name_from_remote("https://github.com/octo"),
        None
    );
}

#[test]
fn ghook_repo_full_name_from_payload() {
    let payload = workflow_run_payload("completed", "failure", "main");
    assert_eq!(
        hooks::repo_full_name_from_payload(&payload),
        Some("octo/hello".to_string())
    );
    assert_eq!(hooks::repo_full_name_from_payload(&json!({})), None);
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

#[test]
fn ghook_load_hooks_defaults_when_unconfigured() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let cfg = hooks::load_hooks(tmp.path()).unwrap();
    assert_eq!(cfg.secret, None);
    assert_eq!(cfg.fix_label, hooks::DEFAULT_FIX_LABEL);
    assert_eq!(cfg.queue_label, hooks::DEFAULT_QUEUE_LABEL);
}

#[test]
fn ghook_load_hooks_reads_project_layer_and_custom_labels() {
    isolate_home();
    let tmp = TempDir::new().unwrap();
    let kranz = tmp.path().join(".kranz");
    std::fs::create_dir_all(&kranz).unwrap();
    std::fs::write(
        kranz.join("config.json"),
        r#"{ "hooks": { "secret": "s3cr3t", "fixLabel": "bot:fix" } }"#,
    )
    .unwrap();
    let cfg = hooks::load_hooks(tmp.path()).unwrap();
    assert_eq!(cfg.secret.as_deref(), Some("s3cr3t"));
    assert_eq!(cfg.fix_label, "bot:fix");
    // Untouched keys keep their defaults (additive layering).
    assert_eq!(cfg.queue_label, hooks::DEFAULT_QUEUE_LABEL);
}

#[test]
fn ghook_hooks_config_debug_never_shows_the_secret() {
    let cfg = HooksConfig {
        secret: Some("super-secret-value".to_string()),
        ..HooksConfig::default()
    };
    let debug = format!("{cfg:?}");
    assert!(!debug.contains("super-secret-value"));
    assert!(debug.contains("[REDACTED]"));
}

// ---------------------------------------------------------------------------
// Ticket drafting: dedup + provenance
// ---------------------------------------------------------------------------

fn ci_trigger() -> Trigger {
    hooks::parse_trigger(
        "workflow_run",
        &workflow_run_payload("completed", "failure", "main"),
        &cfg_with_secret(),
    )
    .unwrap()
}

fn comment_trigger(body: &str) -> Trigger {
    hooks::parse_trigger(
        "issue_comment",
        &issue_comment_payload(body, true),
        &cfg_with_secret(),
    )
    .unwrap()
}

fn trigger_tickets_in(repo: &Path) -> Vec<String> {
    Ticket::list(repo)
        .iter()
        .map(|t| t.slug.clone())
        .filter(|s| s.starts_with("trigger-"))
        .collect()
}

#[test]
fn ghook_draft_trigger_ticket_writes_provenance_and_dedupes() {
    let tmp = TempDir::new().unwrap();
    let trigger = ci_trigger();

    let outcome = hooks::draft_trigger_ticket(tmp.path(), &trigger).unwrap();
    let slug = match outcome {
        TriggerDraft::Drafted { slug } => slug,
        other => panic!("expected Drafted, got {other:?}"),
    };
    assert_eq!(slug, "trigger-ci-9876543210");

    // The ticket parses back and carries the full provenance.
    let ticket = Ticket::load(&Ticket::tickets_dir(tmp.path()).join(format!("{slug}.md"))).unwrap();
    assert_eq!(ticket.trigger.as_deref(), Some("ci-failure"));
    assert!(ticket
        .raw_body
        .contains("Source: https://github.com/octo/hello/actions/runs/9876543210"));
    assert!(ticket.raw_body.contains("Actor: octocat"));
    assert!(ticket.raw_body.contains("Consent: draft-only"));
    assert!(ticket.raw_body.contains("conclusion: failure"));
    assert_eq!(Ticket::read_state(tmp.path(), &slug), TicketState::New);

    // A second event for the same run is a no-op, never a duplicate ticket.
    let again = hooks::draft_trigger_ticket(tmp.path(), &trigger).unwrap();
    assert_eq!(again, TriggerDraft::Duplicate { slug: slug.clone() });
    assert_eq!(trigger_tickets_in(tmp.path()), vec![slug]);
}

#[test]
fn ghook_draft_trigger_ticket_records_queue_pre_consent() {
    let tmp = TempDir::new().unwrap();
    let trigger = comment_trigger("kranz:fix-and-queue the flaky test");

    let outcome = hooks::draft_trigger_ticket(tmp.path(), &trigger).unwrap();
    let TriggerDraft::Drafted { slug } = outcome else {
        panic!("expected Drafted, got {outcome:?}")
    };
    assert_eq!(slug, "trigger-pr-42");

    let ticket = Ticket::load(&Ticket::tickets_dir(tmp.path()).join(format!("{slug}.md"))).unwrap();
    assert_eq!(ticket.trigger.as_deref(), Some("pr-comment"));
    assert!(ticket.raw_body.contains("Consent: fix-and-queue"));
    assert!(ticket
        .raw_body
        .contains("plan approval itself is never skipped"));
    assert!(ticket
        .raw_body
        .contains("kranz:fix-and-queue the flaky test"));
}

#[test]
fn ghook_human_authored_tickets_have_no_trigger_provenance() {
    let tmp = TempDir::new().unwrap();
    Ticket::scaffold(tmp.path(), "plain", "a human ticket", None, None).unwrap();
    let ticket = Ticket::load(&Ticket::tickets_dir(tmp.path()).join("plain.md")).unwrap();
    assert_eq!(ticket.trigger, None);
}

// ---------------------------------------------------------------------------
// The never-push / never-land invariant: allow-set + source scan
// ---------------------------------------------------------------------------

#[test]
fn ghook_trigger_action_allow_set_is_draft_and_queue_only() {
    assert_eq!(hooks::TRIGGER_ACTIONS, ["draft", "queue"]);
}

#[test]
fn ghook_module_source_has_no_git_mutation_paths() {
    // The trigger core must never reference a git-mutation path: the route
    // drafts/queues tickets and nothing more (the repo's never-land-from-
    // automation invariant). Scan the module source for the engine's git
    // mutation entry points and raw mutation command strings.
    const NEEDLES: [&str; 8] = [
        "git push",
        "git merge",
        "git publish",
        "push_mission",
        "merge_mission",
        "merge_no_ff",
        "fast_forward",
        "publish",
    ];
    let source = include_str!("../src/hooks.rs");
    for needle in NEEDLES {
        assert!(
            !source.contains(needle),
            "crates/engine/src/hooks.rs must not reference '{needle}'"
        );
    }
}
