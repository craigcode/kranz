//! `/kranz ticket list` and `/kranz ticket show <slug>` — the read-only
//! backlog verbs (docs/backlog-and-slack.md). Covers: routing dispatches from
//! the single-line slash-command path and replies over `response_url`
//! (ephemeral); neither verb carries a `user_id` so there is structurally no
//! allowlist gate (mirrors `/kranz status`); an unknown/invalid slug is a
//! graceful error, never a panic; and a thread-context message never dispatches
//! these verbs (only `slash_commands` envelopes do — an `events_api` message,
//! even one that says "ticket list", stays on the thread-guidance path).

use kranz_engine::draft::DraftOutcome;
use kranz_slack::bridge::{
    build_ticket_list_reply, build_ticket_show_reply, not_authorized_blocks, run_draft_command,
};
use kranz_slack::host::BoxFuture;
use kranz_slack::inbound::{route, Action, ThreadLookup};
use kranz_slack::{NotifyFlags, PlanOutcome, PlanningHost, SharedHost, SlackConfig};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

fn no_lookup() -> impl ThreadLookup {
    |_: &str| None
}

fn slash_env(text: &str) -> Value {
    json!({
        "type": "slash_commands",
        "envelope_id": "env-tix",
        "payload": {
            "command": "/kranz",
            "text": text,
            "channel_id": "C1",
            "response_url": "https://hooks.slack/tix"
        }
    })
}

fn write_ticket(repo: &std::path::Path, slug: &str, markdown: &str) {
    let dir = repo.join(".kranz").join("tickets");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{slug}.md")), markdown).unwrap();
}

fn all_text(blocks: &[Value]) -> String {
    serde_json::to_string(blocks).unwrap()
}

// -- ticket list: dispatches ephemerally, no allowlist gate -----------------

#[test]
fn ticket_list_dispatches_from_single_line_slash_and_replies_ephemeral() {
    // "Ephemeral" here means the reply travels over the slash `response_url`
    // (chat.responseUrl / postEphemeral path), same mechanism as `/kranz
    // status` — never posted to the channel at large.
    let routed = route(&slash_env("ticket list"), &no_lookup());
    assert_eq!(routed.envelope_id.as_deref(), Some("env-tix"));
    match routed.action {
        Action::TicketList { response_url } => {
            assert_eq!(response_url.as_deref(), Some("https://hooks.slack/tix"));
        }
        other => panic!("expected TicketList, got {other:?}"),
    }
}

#[test]
fn ticket_list_action_carries_no_user_id_so_it_cannot_be_allowlist_gated() {
    // Action::TicketList has no `user_id` field at all: there is nothing for
    // `SlackConfig::is_authorized` to consult, which is how the read-only
    // verbs are architecturally exempt from the spend allowlist (same shape
    // as Action::Status / Action::Work).
    let routed = route(&slash_env("ticket list"), &no_lookup());
    assert_eq!(
        routed.action,
        Action::TicketList {
            response_url: Some("https://hooks.slack/tix".into())
        }
    );
}

#[test]
fn ticket_list_reply_lists_ticket_rows_with_blocked_by_note() {
    let tmp = TempDir::new().unwrap();
    write_ticket(
        tmp.path(),
        "rate-limit-notes",
        "---\ntitle: Rate-limit the notes API\npriority: 1\n---\n## Goal\nAdd a token bucket.\n",
    );
    write_ticket(
        tmp.path(),
        "downstream-thing",
        "---\ntitle: Downstream thing\npriority: 2\nblocked-by: [rate-limit-notes]\n---\n## Goal\nBuild on top.\n",
    );

    let blocks = build_ticket_list_reply(tmp.path());
    let text = all_text(&blocks);
    assert!(text.contains("rate-limit-notes"), "lists first ticket slug");
    assert!(
        text.contains("Rate-limit the notes API"),
        "lists first ticket title"
    );
    assert!(
        text.contains("downstream-thing"),
        "lists second ticket slug"
    );
    assert!(
        text.contains("blocked by"),
        "notes the blocked-by relationship: {text}"
    );
    assert!(text.contains("New"), "shows the default pipeline state");
}

#[test]
fn ticket_list_reply_on_empty_backlog_is_a_friendly_empty_state() {
    let tmp = TempDir::new().unwrap();
    let blocks = build_ticket_list_reply(tmp.path());
    let text = all_text(&blocks);
    assert!(text.to_lowercase().contains("no backlog tickets"));
}

// -- ticket show: dispatches ephemerally, no allowlist gate, graceful errors --

#[test]
fn ticket_show_dispatches_from_single_line_slash_and_replies_ephemeral() {
    let routed = route(&slash_env("ticket show rate-limit-notes"), &no_lookup());
    assert_eq!(routed.envelope_id.as_deref(), Some("env-tix"));
    assert_eq!(
        routed.action,
        Action::TicketShow {
            slug: "rate-limit-notes".into(),
            response_url: Some("https://hooks.slack/tix".into())
        }
    );
}

#[test]
fn ticket_show_action_carries_no_user_id_so_it_cannot_be_allowlist_gated() {
    match route(&slash_env("ticket show anything"), &no_lookup()).action {
        Action::TicketShow { slug, .. } => assert_eq!(slug, "anything"),
        other => panic!("expected TicketShow, got {other:?}"),
    }
}

#[test]
fn ticket_show_reply_renders_goal_state_blocked_by_and_needs_context() {
    let tmp = TempDir::new().unwrap();
    write_ticket(
        tmp.path(),
        "rate-limit-notes",
        "---\ntitle: Rate-limit the notes API\nblocked-by: [auth-refactor]\n---\n\
         ## Goal\nAdd a token bucket, cap at 100/min.\n\n\
         ## Needs context\n- Which endpoint exactly?\n- Per-user or per-token?\n",
    );

    let blocks = build_ticket_show_reply(tmp.path(), "rate-limit-notes");
    let text = all_text(&blocks);
    assert!(text.contains("Rate-limit the notes API"));
    assert!(text.contains("token bucket"));
    assert!(text.contains("auth-refactor"), "shows blocked-by");
    assert!(
        text.contains("Which endpoint exactly?"),
        "shows needs-context questions"
    );
    assert!(text.contains("Per-user or per-token?"));
}

#[test]
fn ticket_show_unknown_slug_is_a_graceful_ephemeral_error_no_panic() {
    let tmp = TempDir::new().unwrap();
    let blocks = build_ticket_show_reply(tmp.path(), "does-not-exist");
    let text = all_text(&blocks);
    assert!(
        text.to_lowercase().contains("unknown ticket"),
        "got: {text}"
    );
}

#[test]
fn ticket_show_invalid_slug_is_a_graceful_ephemeral_error_no_panic() {
    let tmp = TempDir::new().unwrap();
    // A path-traversal attempt must be rejected by ensure_valid_slug before
    // any filesystem access, not panic and not escape .kranz/tickets/.
    for bad in ["../../etc/passwd", "..", "", "has space"] {
        let blocks = build_ticket_show_reply(tmp.path(), bad);
        let text = all_text(&blocks);
        assert!(
            text.to_lowercase().contains("invalid"),
            "slug {bad:?} should be a graceful invalid-slug error, got: {text}"
        );
    }
}

// -- thread-context invocation is never dispatched --------------------------

#[test]
fn ticket_verbs_typed_in_a_thread_message_are_not_dispatched() {
    // Only `slash_commands` envelopes reach `route_slash`; an `events_api`
    // message (even one that says "ticket list" or "ticket show <slug>")
    // follows the thread-guidance rules instead and is never turned into
    // Action::TicketList / Action::TicketShow.
    for text in ["ticket list", "ticket show rate-limit-notes"] {
        let env = json!({
            "type": "events_api",
            "payload": { "event": {
                "type": "message",
                "text": text,
                "ts": "1700000000.000200",
                "thread_ts": "1700000000.000100",
                "user": "U123"
            }}
        });
        // Unknown thread -> Ignore; a known thread would fold to Guidance.
        // Either way it is never TicketList/TicketShow.
        let action = route(&env, &no_lookup()).action;
        assert_ne!(action, Action::TicketList { response_url: None });
        match action {
            Action::TicketList { .. } | Action::TicketShow { .. } => {
                panic!("thread message {text:?} must not dispatch a ticket verb")
            }
            _ => {}
        }
    }
}

// -- `/kranz draft <slug>` routing -------------------------------------------

#[test]
fn draft_dispatches_from_single_line_slash_with_slug_and_user() {
    let env = json!({
        "type": "slash_commands",
        "envelope_id": "env-draft",
        "payload": {
            "command": "/kranz",
            "text": "draft rate-limit-notes",
            "channel_id": "C1",
            "user_id": "U123",
            "response_url": "https://hooks.slack/tix"
        }
    });
    let routed = route(&env, &no_lookup());
    assert_eq!(
        routed.action,
        Action::Draft {
            slug: "rate-limit-notes".into(),
            user_id: Some("U123".into()),
            response_url: Some("https://hooks.slack/tix".into()),
        }
    );
}

#[test]
fn bare_draft_falls_through_to_help() {
    let routed = route(&slash_env("draft"), &no_lookup());
    assert_eq!(
        routed.action,
        Action::Help {
            response_url: Some("https://hooks.slack/tix".into())
        }
    );
}

// -- `/kranz draft <slug>`: spend-gated exactly like `/kranz new` -----------

/// A fake `PlanningHost` for draft tests: records how many times `draft` is
/// called (an engine spawn) and returns a canned, switchable outcome. Every
/// other method panics if reached — no draft test exercises them.
struct FakeHost {
    draft_calls: AtomicUsize,
    outcome: DraftOutcomeKind,
}

#[derive(Clone, Copy)]
enum DraftOutcomeKind {
    ParkedForReview,
    NeedsContext,
}

impl FakeHost {
    fn new(outcome: DraftOutcomeKind) -> Self {
        FakeHost {
            draft_calls: AtomicUsize::new(0),
            outcome,
        }
    }
}

impl PlanningHost for FakeHost {
    fn create<'a>(&'a self, _goal: &'a str) -> BoxFuture<'a, anyhow::Result<String>> {
        Box::pin(async { unreachable!("draft tests never call create") })
    }

    fn planning_turn<'a>(
        &'a self,
        _id: &'a str,
        _text: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<String>> {
        Box::pin(async { unreachable!("draft tests never call planning_turn") })
    }

    fn request_plan<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, anyhow::Result<PlanOutcome>> {
        Box::pin(async { unreachable!("draft tests never call request_plan") })
    }

    fn approve_pending<'a>(
        &'a self,
        _id: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<Option<String>>> {
        Box::pin(async { unreachable!("draft tests never call approve_pending") })
    }

    fn start<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async { unreachable!("draft tests never call start") })
    }

    fn release<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async { unreachable!("draft tests never call release") })
    }

    fn draft<'a>(&'a self, slug: &'a str) -> BoxFuture<'a, anyhow::Result<DraftOutcome>> {
        self.draft_calls.fetch_add(1, Ordering::SeqCst);
        let outcome = match self.outcome {
            DraftOutcomeKind::ParkedForReview => DraftOutcome::ParkedForReview {
                mission_id: "m-draft".into(),
                mission_branch: "kranz/mission-m-draft".into(),
            },
            DraftOutcomeKind::NeedsContext => DraftOutcome::NeedsContext {
                mission_id: "m-draft".into(),
                questions: vec![
                    "Which endpoint exactly?".into(),
                    "Per-user or per-token?".into(),
                ],
            },
        };
        let slug = slug.to_string();
        Box::pin(async move {
            let _ = slug;
            Ok(outcome)
        })
    }
}

fn gated_cfg(allow_users: Vec<String>) -> SlackConfig {
    SlackConfig {
        bot_token: "xoxb".into(),
        app_token: "xapp".into(),
        channel: "C1".into(),
        notify: NotifyFlags::default(),
        allow_users,
        dashboard_url: None,
        instance_name: None,
    }
}

#[tokio::test]
async fn draft_denies_an_unlisted_user_and_spawns_nothing() {
    // Model: steer_denies_an_unlisted_user_and_enqueues_nothing. A non-empty
    // allowlist gates draft exactly like `/kranz new`: an unlisted user gets
    // the standard refusal and the fake host's draft counter never moves.
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::ParkedForReview));
    let host: SharedHost = fake.clone();

    let invocation =
        run_draft_command(&cfg, Some(&host), "rate-limit-notes", Some("U-outsider")).await;

    assert!(!invocation.authorized, "unlisted user must be refused");
    assert!(invocation.ack.is_none(), "unauthorized draft acks nothing");
    assert!(
        invocation.result.is_none(),
        "unauthorized draft posts nothing beyond the refusal"
    );
    assert_eq!(
        fake.draft_calls.load(Ordering::SeqCst),
        0,
        "an unlisted user's draft must spawn zero engine turns"
    );
    // The refusal the caller posts on `!authorized` is the exact standard one
    // shared with `/kranz new`.
    let refusal = serde_json::to_string(&not_authorized_blocks()).unwrap();
    assert!(refusal.contains("not authorized to spend"));
}

#[tokio::test]
async fn draft_allows_a_listed_user_acks_immediately_and_spawns_exactly_once() {
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::ParkedForReview));
    let host: SharedHost = fake.clone();

    let invocation =
        run_draft_command(&cfg, Some(&host), "rate-limit-notes", Some("U-allowed")).await;

    assert!(invocation.authorized);
    let ack_text = serde_json::to_string(&invocation.ack.expect("authorized draft acks"))
        .unwrap()
        .to_lowercase();
    assert!(
        ack_text.contains("hourglass"),
        "authorized draft posts an immediate hourglass ack: {ack_text}"
    );
    let result_text =
        serde_json::to_string(&invocation.result.expect("authorized draft posts a result"))
            .unwrap();
    assert!(result_text.contains("m-draft"), "result names the mission");
    assert_eq!(
        fake.draft_calls.load(Ordering::SeqCst),
        1,
        "authorized draft spawns exactly one engine turn"
    );
}

#[tokio::test]
async fn draft_allowlist_empty_authorizes_everyone_same_as_new() {
    // Empty allow_users = no gate at all (mirrors `/kranz new`'s degrade when
    // unconfigured).
    let cfg = gated_cfg(vec![]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::ParkedForReview));
    let host: SharedHost = fake.clone();

    let invocation = run_draft_command(&cfg, Some(&host), "rate-limit-notes", None).await;

    assert!(invocation.authorized);
    assert_eq!(fake.draft_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn draft_needs_context_posts_the_orchestrators_questions_back() {
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::NeedsContext));
    let host: SharedHost = fake.clone();

    let invocation =
        run_draft_command(&cfg, Some(&host), "rate-limit-notes", Some("U-allowed")).await;

    assert!(invocation.authorized);
    let result_text =
        serde_json::to_string(&invocation.result.expect("needs-context posts a result")).unwrap();
    assert!(
        result_text.contains("Which endpoint exactly?"),
        "posts the orchestrator's clarifying questions back to the invoker: {result_text}"
    );
    assert!(result_text.contains("Per-user or per-token?"));
    assert_eq!(fake.draft_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn draft_invalid_slug_errors_gracefully_with_no_host_call() {
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::ParkedForReview));
    let host: SharedHost = fake.clone();

    let invocation =
        run_draft_command(&cfg, Some(&host), "../../etc/passwd", Some("U-allowed")).await;

    assert!(invocation.authorized, "gate already passed");
    assert!(invocation.ack.is_none(), "invalid slug never acks a draft");
    let result_text =
        serde_json::to_string(&invocation.result.expect("invalid slug errors gracefully"))
            .unwrap()
            .to_lowercase();
    assert!(result_text.contains("invalid"), "got: {result_text}");
    assert_eq!(
        fake.draft_calls.load(Ordering::SeqCst),
        0,
        "an invalid slug must never reach the host"
    );
}

#[tokio::test]
async fn draft_without_a_host_is_an_honest_refusal() {
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let invocation = run_draft_command(&cfg, None, "rate-limit-notes", Some("U-allowed")).await;
    assert!(invocation.authorized);
    assert!(invocation.ack.is_none());
    let result_text =
        serde_json::to_string(&invocation.result.expect("no-host case errors gracefully"))
            .unwrap()
            .to_lowercase();
    assert!(result_text.contains("no hosted planning engine"));
}
