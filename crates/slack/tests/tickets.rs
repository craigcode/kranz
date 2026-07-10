//! `/kranz ticket list` and `/kranz ticket show <slug>` — the read-only
//! backlog verbs (docs/backlog-and-slack.md). Covers: routing dispatches from
//! the single-line slash-command path and replies over `response_url`
//! (ephemeral); neither verb carries a `user_id` so there is structurally no
//! allowlist gate (mirrors `/kranz status`); an unknown/invalid slug is a
//! graceful error, never a panic; and a thread-context message never dispatches
//! these verbs (only `slash_commands` envelopes do — an `events_api` message,
//! even one that says "ticket list", stays on the thread-guidance path).

use kranz_engine::draft::DraftOutcome;
use kranz_engine::ticket::{Ticket, TicketState};
use kranz_slack::bridge::{
    build_ticket_list_reply, build_ticket_show_reply, gate_draft_command, not_authorized_blocks,
    run_approve_ticket_command, run_draft, DraftGate,
};
use kranz_slack::host::BoxFuture;
use kranz_slack::inbound::{route, Action, ThreadLookup};
use kranz_slack::{AskOutcome, NotifyFlags, PlanOutcome, PlanningHost, SharedHost, SlackConfig};
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
    /// Backs `approve_ticket`: calling through to the REAL
    /// `kranz_engine::deps::approve_ticket` against this repo root, so the
    /// approve tests exercise the exact same shared gate the REST/CLI paths
    /// run (never a re-implementation). `None` when a test never calls
    /// `approve_ticket` (the draft-only tests).
    repo_root: Option<std::path::PathBuf>,
    approve_ticket_calls: AtomicUsize,
    /// Records how many times `drain` (the `/kranz work run` host call) is
    /// called. `None` never errors in these tests.
    drain_calls: AtomicUsize,
    /// Records how many times `merge` (the `/kranz merge` host call) is
    /// called.
    merge_calls: AtomicUsize,
    merge_error: Option<String>,
}

#[derive(Clone, Copy)]
enum DraftOutcomeKind {
    ParkedForReview,
    NeedsContext,
    PlanAsProse,
}

impl FakeHost {
    fn new(outcome: DraftOutcomeKind) -> Self {
        FakeHost {
            draft_calls: AtomicUsize::new(0),
            outcome,
            repo_root: None,
            approve_ticket_calls: AtomicUsize::new(0),
            drain_calls: AtomicUsize::new(0),
            merge_calls: AtomicUsize::new(0),
            merge_error: None,
        }
    }

    fn with_repo_root(repo_root: std::path::PathBuf) -> Self {
        FakeHost {
            draft_calls: AtomicUsize::new(0),
            outcome: DraftOutcomeKind::ParkedForReview,
            repo_root: Some(repo_root),
            approve_ticket_calls: AtomicUsize::new(0),
            drain_calls: AtomicUsize::new(0),
            merge_calls: AtomicUsize::new(0),
            merge_error: None,
        }
    }

    fn with_merge_error(error: String) -> Self {
        let mut host = Self::new(DraftOutcomeKind::ParkedForReview);
        host.merge_error = Some(error);
        host
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
            DraftOutcomeKind::PlanAsProse => DraftOutcome::PlanAsProse {
                mission_id: "m-draft".into(),
            },
        };
        let slug = slug.to_string();
        Box::pin(async move {
            let _ = slug;
            Ok(outcome)
        })
    }

    fn approve_ticket<'a>(&'a self, slug: &'a str) -> BoxFuture<'a, anyhow::Result<String>> {
        self.approve_ticket_calls.fetch_add(1, Ordering::SeqCst);
        let repo_root = self
            .repo_root
            .clone()
            .expect("approve_ticket tests must construct FakeHost::with_repo_root");
        let slug = slug.to_string();
        Box::pin(async move {
            let approved = kranz_engine::deps::approve_ticket(&repo_root, &slug, None, false)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(approved.mission_id)
        })
    }

    fn drain<'a>(&'a self) -> BoxFuture<'a, anyhow::Result<()>> {
        self.drain_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }

    fn merge<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, anyhow::Result<Value>> {
        self.merge_calls.fetch_add(1, Ordering::SeqCst);
        let error = self.merge_error.clone();
        Box::pin(async move {
            match error {
                Some(error) => Err(anyhow::anyhow!(error)),
                None => Ok(json!({ "merged": true, "commit": "abc123" })),
            }
        })
    }

    fn ask<'a>(&'a self, _question: &'a str) -> BoxFuture<'a, anyhow::Result<AskOutcome>> {
        Box::pin(async { unreachable!("ticket tests never call ask") })
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

    let gate = gate_draft_command(&cfg, Some(&host), "rate-limit-notes", Some("U-outsider"));

    assert!(
        matches!(gate, DraftGate::Unauthorized),
        "unlisted user must be refused"
    );
    assert_eq!(
        fake.draft_calls.load(Ordering::SeqCst),
        0,
        "an unlisted user's draft must spawn zero engine turns"
    );
    // The refusal the caller posts on `Unauthorized` is the exact standard
    // one shared with `/kranz new`.
    let refusal = serde_json::to_string(&not_authorized_blocks()).unwrap();
    assert!(refusal.contains("not authorized to spend"));
}

#[tokio::test]
async fn draft_gate_acks_before_any_draft_call_then_run_draft_spawns_exactly_once() {
    // The j5 fix under test: the synchronous gate phase must produce the
    // hourglass ack WITHOUT having called `host.draft` yet — proving the ack
    // can be posted before the slow draft turn starts, not after.
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::ParkedForReview));
    let host: SharedHost = fake.clone();

    let gate = gate_draft_command(&cfg, Some(&host), "rate-limit-notes", Some("U-allowed"));

    let ack = match gate {
        DraftGate::Ready(ack) => ack,
        _ => panic!("authorized draft with a host must be Ready"),
    };
    assert_eq!(
        fake.draft_calls.load(Ordering::SeqCst),
        0,
        "the gate/ack phase must not call host.draft — the ack must be postable BEFORE the slow draft runs"
    );
    let ack_text = serde_json::to_string(&ack).unwrap().to_lowercase();
    assert!(
        ack_text.contains("hourglass"),
        "authorized draft acks with the immediate hourglass: {ack_text}"
    );

    // Only now — after the caller would have posted the ack — does the slow
    // run phase call the host, exactly once.
    let result_text = serde_json::to_string(&run_draft(&host, "rate-limit-notes").await).unwrap();
    assert!(result_text.contains("m-draft"), "result names the mission");
    assert_eq!(
        fake.draft_calls.load(Ordering::SeqCst),
        1,
        "run_draft spawns exactly one engine turn"
    );
}

#[tokio::test]
async fn draft_allowlist_empty_authorizes_everyone_same_as_new() {
    // Empty allow_users = no gate at all (mirrors `/kranz new`'s degrade when
    // unconfigured).
    let cfg = gated_cfg(vec![]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::ParkedForReview));
    let host: SharedHost = fake.clone();

    let gate = gate_draft_command(&cfg, Some(&host), "rate-limit-notes", None);

    assert!(matches!(gate, DraftGate::Ready(_)));
    let _ = run_draft(&host, "rate-limit-notes").await;
    assert_eq!(fake.draft_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn draft_needs_context_posts_the_orchestrators_questions_back() {
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::NeedsContext));
    let host: SharedHost = fake.clone();

    let gate = gate_draft_command(&cfg, Some(&host), "rate-limit-notes", Some("U-allowed"));
    assert!(matches!(gate, DraftGate::Ready(_)));

    let result_text = serde_json::to_string(&run_draft(&host, "rate-limit-notes").await).unwrap();
    assert!(
        result_text.contains("Which endpoint exactly?"),
        "posts the orchestrator's clarifying questions back to the invoker: {result_text}"
    );
    assert!(result_text.contains("Per-user or per-token?"));
    assert_eq!(fake.draft_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn draft_plan_as_prose_posts_an_honest_not_queued_message() {
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::PlanAsProse));
    let host: SharedHost = fake.clone();

    let gate = gate_draft_command(&cfg, Some(&host), "rate-limit-notes", Some("U-allowed"));
    assert!(matches!(gate, DraftGate::Ready(_)));

    let result_text = serde_json::to_string(&run_draft(&host, "rate-limit-notes").await).unwrap();
    assert!(
        result_text.contains("m-draft"),
        "names the mission: {result_text}"
    );
    assert!(
        result_text.contains("prose") && result_text.contains("NOT queued"),
        "conveys the plan was emitted as prose and nothing was queued: {result_text}"
    );
    assert!(
        !result_text.contains("approved and queued")
            && !result_text.contains("Draft ready for review"),
        "never claims success: {result_text}"
    );
    assert_eq!(fake.draft_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn draft_invalid_slug_errors_gracefully_with_no_host_call() {
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::ParkedForReview));
    let host: SharedHost = fake.clone();

    let gate = gate_draft_command(&cfg, Some(&host), "../../etc/passwd", Some("U-allowed"));

    let blocks = match gate {
        DraftGate::InvalidSlug(blocks) => blocks,
        _ => panic!("invalid slug must gate to InvalidSlug"),
    };
    let result_text = serde_json::to_string(&blocks).unwrap().to_lowercase();
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
    let gate = gate_draft_command(&cfg, None, "rate-limit-notes", Some("U-allowed"));
    let blocks = match gate {
        DraftGate::NoHost(blocks) => blocks,
        _ => panic!("no host must gate to NoHost"),
    };
    let result_text = serde_json::to_string(&blocks).unwrap().to_lowercase();
    assert!(result_text.contains("no hosted planning engine"));
}

// -- `/kranz approve <slug>` — the slug-resolving twin of `/kranz approve
// <mission-id>`, routed through the exact same
// `kranz_engine::deps::approve_ticket` gate the REST/CLI approve paths run
// (never a divergent re-implementation — `FakeHost::approve_ticket` calls
// through to the real engine function against a temp repo root). ------------

#[tokio::test]
async fn approve_slug_for_review_ticket_queues_the_drafted_mission_end_to_end() {
    let tmp = TempDir::new().unwrap();
    write_ticket(
        tmp.path(),
        "rate-limit-notes",
        "---\ntitle: Rate-limit the notes API\npriority: 1\n---\n## Goal\nAdd a token bucket.\n",
    );
    Ticket::record_mission(tmp.path(), "rate-limit-notes", "m-rl").unwrap();
    Ticket::write_state(tmp.path(), "rate-limit-notes", TicketState::Review, None).unwrap();

    let cfg = gated_cfg(vec![]);
    let fake = Arc::new(FakeHost::with_repo_root(tmp.path().to_path_buf()));
    let host: SharedHost = fake.clone();

    let invocation = run_approve_ticket_command(&cfg, Some(&host), "rate-limit-notes", None).await;

    assert!(invocation.authorized);
    let result_text =
        serde_json::to_string(&invocation.result.expect("approve posts a result")).unwrap();
    assert!(
        result_text.contains("m-rl"),
        "result names the queued mission: {result_text}"
    );
    assert!(result_text.to_lowercase().contains("queued"));
    assert_eq!(fake.approve_ticket_calls.load(Ordering::SeqCst), 1);

    // The real side effects of `kranz_engine::deps::approve_ticket` happened:
    // the ticket flipped to Queued and the mission is really on the queue.
    assert_eq!(
        Ticket::read_state(tmp.path(), "rate-limit-notes"),
        TicketState::Queued
    );
    let entries = kranz_engine::queue::list(tmp.path());
    assert_eq!(
        entries.len(),
        1,
        "the drafted mission was queued exactly once"
    );
    assert_eq!(entries[0].mission_id, "m-rl");
    assert_eq!(entries[0].ticket_slug.as_deref(), Some("rate-limit-notes"));
}

#[tokio::test]
async fn approve_slug_blocked_by_unsatisfied_dependency_refuses_verbatim_and_queues_nothing() {
    let tmp = TempDir::new().unwrap();
    write_ticket(
        tmp.path(),
        "upstream",
        "---\ntitle: Upstream thing\npriority: 1\n---\n## Goal\nDo the base work.\n",
    );
    write_ticket(
        tmp.path(),
        "downstream",
        "---\ntitle: Downstream thing\npriority: 2\nblocked-by: [upstream]\n---\n## Goal\nBuild on top.\n",
    );
    Ticket::record_mission(tmp.path(), "downstream", "m-down").unwrap();
    Ticket::write_state(tmp.path(), "downstream", TicketState::Review, None).unwrap();

    let cfg = gated_cfg(vec![]);
    let fake = Arc::new(FakeHost::with_repo_root(tmp.path().to_path_buf()));
    let host: SharedHost = fake.clone();

    let invocation = run_approve_ticket_command(&cfg, Some(&host), "downstream", None).await;

    assert!(invocation.authorized);
    let result_text =
        serde_json::to_string(&invocation.result.expect("blocked approve posts a result")).unwrap();
    // The engine's own refusal message (crates/engine/src/deps.rs), forwarded
    // VERBATIM — not paraphrased, not summarized.
    assert!(
        result_text.contains(
            "cannot approve downstream: blocked by upstream (its mission is not Complete)"
        ),
        "expected the engine's verbatim blocked-by refusal in: {result_text}"
    );
    assert_eq!(fake.approve_ticket_calls.load(Ordering::SeqCst), 1);

    assert_eq!(
        Ticket::read_state(tmp.path(), "downstream"),
        TicketState::Review,
        "a blocked approve must not flip the ticket's state"
    );
    assert!(
        kranz_engine::queue::list(tmp.path()).is_empty(),
        "a blocked approve must queue nothing"
    );
}

// -- `/kranz work run` routing + gate ---------------------------------------

use kranz_slack::bridge::{gate_work_run_command, run_work_run, WorkRunGate};

#[test]
fn work_run_dispatches_from_single_line_slash_with_user() {
    let env = json!({
        "type": "slash_commands",
        "envelope_id": "env-workrun",
        "payload": {
            "command": "/kranz",
            "text": "work run",
            "channel_id": "C1",
            "user_id": "U123",
            "response_url": "https://hooks.slack/tix"
        }
    });
    let routed = route(&env, &no_lookup());
    assert_eq!(
        routed.action,
        Action::WorkRun {
            user_id: Some("U123".into()),
            response_url: Some("https://hooks.slack/tix".into()),
        }
    );
}

#[test]
fn bare_work_still_routes_to_work_report_only() {
    let routed = route(&slash_env("work"), &no_lookup());
    assert_eq!(
        routed.action,
        Action::Work {
            response_url: Some("https://hooks.slack/tix".into())
        }
    );
}

#[test]
fn work_run_extra_falls_through_to_help() {
    for text in ["work run extra", "work now"] {
        let routed = route(&slash_env(text), &no_lookup());
        assert_eq!(
            routed.action,
            Action::Help {
                response_url: Some("https://hooks.slack/tix".into())
            },
            "text={text:?} should route to help"
        );
    }
}

#[tokio::test]
async fn work_run_denies_an_unlisted_user_and_drains_nothing() {
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::ParkedForReview));
    let host: SharedHost = fake.clone();

    let gate = gate_work_run_command(&cfg, Some(&host), Some("U-outsider"));

    assert!(
        matches!(gate, WorkRunGate::Unauthorized),
        "unlisted user must be refused"
    );
    assert_eq!(
        fake.drain_calls.load(Ordering::SeqCst),
        0,
        "an unlisted user's work run must trigger zero drains"
    );
    let refusal = serde_json::to_string(&not_authorized_blocks()).unwrap();
    assert!(refusal.contains("not authorized to spend"));
}

#[tokio::test]
async fn work_run_gate_acks_before_any_drain_call_then_run_triggers_exactly_once() {
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::ParkedForReview));
    let host: SharedHost = fake.clone();

    let gate = gate_work_run_command(&cfg, Some(&host), Some("U-allowed"));

    let ack = match gate {
        WorkRunGate::Ready(ack) => ack,
        _ => panic!("authorized work run with a host must be Ready"),
    };
    assert_eq!(
        fake.drain_calls.load(Ordering::SeqCst),
        0,
        "the gate/ack phase must not call host.drain — the ack must be postable BEFORE the drain runs"
    );
    let ack_text = serde_json::to_string(&ack).unwrap().to_lowercase();
    assert!(
        ack_text.contains("draining"),
        "authorized work run acks immediately: {ack_text}"
    );

    let _ = run_work_run(&host).await;
    assert_eq!(
        fake.drain_calls.load(Ordering::SeqCst),
        1,
        "run_work_run triggers exactly one drain — no mission is resumed/run inline"
    );
}

#[tokio::test]
async fn work_run_without_a_host_is_an_honest_refusal_pointing_at_the_cli() {
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let gate = gate_work_run_command(&cfg, None, Some("U-allowed"));
    let blocks = match gate {
        WorkRunGate::NoHost(blocks) => blocks,
        _ => panic!("no host must gate to NoHost"),
    };
    let result_text = serde_json::to_string(&blocks).unwrap().to_lowercase();
    assert!(result_text.contains("no hosted planning engine"));
    assert!(result_text.contains("kranz work"));
}

// -- `/kranz merge <slug|id>` routing + gate ---------------------------------

use kranz_slack::bridge::{gate_merge_command, run_merge, MergeGate};

#[test]
fn merge_dispatches_from_single_line_slash_with_id_and_user() {
    let env = json!({
        "type": "slash_commands",
        "envelope_id": "env-merge",
        "payload": {
            "command": "/kranz",
            "text": "merge m-42",
            "channel_id": "C1",
            "user_id": "U123",
            "response_url": "https://hooks.slack/tix"
        }
    });
    let routed = route(&env, &no_lookup());
    assert_eq!(
        routed.action,
        Action::Merge {
            mission_id: "m-42".into(),
            user_id: Some("U123".into()),
            response_url: Some("https://hooks.slack/tix".into()),
        }
    );
}

#[test]
fn merge_with_no_id_falls_through_to_help() {
    let routed = route(&slash_env("merge"), &no_lookup());
    assert_eq!(
        routed.action,
        Action::Help {
            response_url: Some("https://hooks.slack/tix".into())
        }
    );
}

#[tokio::test]
async fn merge_denies_an_unlisted_user_and_merges_nothing() {
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::ParkedForReview));
    let host: SharedHost = fake.clone();

    let gate = gate_merge_command(&cfg, Some(&host), Some("U-outsider"));

    assert!(
        matches!(gate, MergeGate::Unauthorized),
        "unlisted user must be refused"
    );
    assert_eq!(
        fake.merge_calls.load(Ordering::SeqCst),
        0,
        "an unlisted user's merge must trigger zero host.merge calls"
    );
    let refusal = serde_json::to_string(&not_authorized_blocks()).unwrap();
    assert!(refusal.contains("not authorized to spend"));
}

#[tokio::test]
async fn merge_gate_acks_before_any_merge_call_then_run_triggers_exactly_once() {
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let fake = Arc::new(FakeHost::new(DraftOutcomeKind::ParkedForReview));
    let host: SharedHost = fake.clone();

    let gate = gate_merge_command(&cfg, Some(&host), Some("U-allowed"));

    let ack = match gate {
        MergeGate::Ready(ack) => ack,
        _ => panic!("authorized merge with a host must be Ready"),
    };
    assert_eq!(
        fake.merge_calls.load(Ordering::SeqCst),
        0,
        "the gate/ack phase must not call host.merge — the ack must be postable BEFORE the merge runs"
    );
    let ack_text = serde_json::to_string(&ack).unwrap().to_lowercase();
    assert!(
        ack_text.contains("merging"),
        "authorized merge acks immediately: {ack_text}"
    );

    let _ = run_merge(&host, "m-42").await;
    assert_eq!(
        fake.merge_calls.load(Ordering::SeqCst),
        1,
        "run_merge triggers exactly one host.merge call"
    );
}

#[tokio::test]
async fn merge_failure_reply_is_bounded_and_keeps_the_gate_output_tail() {
    let detail = format!("{}\nTAIL_ASSERTION_FAILED", "early noise\n".repeat(1000));
    let fake = Arc::new(FakeHost::with_merge_error(detail));
    let host: SharedHost = fake;

    let blocks = run_merge(&host, "m-long").await;
    let text = blocks[0]["text"]["text"].as_str().unwrap();
    assert!(text.contains("TAIL_ASSERTION_FAILED"), "{text}");
    assert!(text.chars().count() <= 2500, "{}", text.chars().count());
}

#[tokio::test]
async fn merge_without_a_host_is_an_honest_refusal_pointing_at_the_cli() {
    let cfg = gated_cfg(vec!["U-allowed".into()]);
    let gate = gate_merge_command(&cfg, None, Some("U-allowed"));
    let blocks = match gate {
        MergeGate::NoHost(blocks) => blocks,
        _ => panic!("no host must gate to NoHost"),
    };
    let result_text = serde_json::to_string(&blocks).unwrap().to_lowercase();
    assert!(result_text.contains("no hosted planning engine"));
    assert!(result_text.contains("kranz merge"));
}
