//! Engine tests for the non-interactive draft core ([`kranz_engine::draft`],
//! roadmap f-1-1), driven entirely through [`MockBackend`] — no real `claude`
//! binary. Mirrors the mock-wiring conventions of `mission_test.rs`
//! (`orch_script`/`MockBackend::with_scripts`/`MissionEngine::create`).

use kranz_engine::backend::AgentBackend;
use kranz_engine::backend_mock::{mock_init, mock_result_text, mock_text, MockScript};
use kranz_engine::draft::{drive_draft, looks_like_plan_json, DraftOutcome};
use kranz_engine::orchestrator::{MissionEngine, PlanRequest};
use kranz_engine::queue;
use kranz_engine::ticket::{Ticket, TicketState};
use kranz_engine::types::{MissionConfig, WorkerIsolation};
use kranz_engine::MockBackend;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Once};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Git fixtures (same isolation discipline as mission_test.rs)
// ---------------------------------------------------------------------------

static ENV_ISOLATION: Once = Once::new();

fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing =
            std::env::temp_dir().join(format!("kranz-draft-test-no-config-{}", std::process::id()));
        std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
        std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
        if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
            std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
        }
    });
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn setup() -> bool {
    isolate_git_env();
    if git_available() {
        true
    } else {
        eprintln!("skipping test: git is not on PATH");
        false
    }
}

fn raw_git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_repo() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let init = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .expect("spawn git init");
    if !init.status.success() {
        raw_git(dir.path(), &["init"]);
        raw_git(dir.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);
    }
    raw_git(dir.path(), &["config", "user.name", "test"]);
    raw_git(dir.path(), &["config", "user.email", "test@example.com"]);
    std::fs::write(dir.path().join("README.md"), "seed\n").unwrap();
    raw_git(dir.path(), &["add", "-A"]);
    raw_git(dir.path(), &["commit", "-m", "seed"]);
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize repo root");
    (dir, root)
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn test_cfg() -> MissionConfig {
    MissionConfig {
        skip_scrutiny: true,
        skip_functional: true,
        worker_isolation: WorkerIsolation::Checkout,
        ..MissionConfig::default()
    }
}

/// The orchestrator streaming script: session-start seed turn ("ready"),
/// then one entry of `replies` per subsequent engine turn (planning_turn,
/// then request_plan).
fn orch_script(replies: Vec<String>) -> MockScript {
    MockScript::streaming(vec![mock_init("orch-session"), mock_result_text("ready")]).responding(
        replies
            .iter()
            .map(|reply| vec![mock_text(reply), mock_result_text(reply)])
            .collect(),
    )
}

fn plan_json(goal: &str) -> String {
    json!({
        "goal": goal,
        "validationContract": [],
        "milestones": [{
            "title": "M1",
            "features": [{
                "title": "F1",
                "spec": "do it",
                "validationCriteria": ["works"]
            }]
        }]
    })
    .to_string()
}

/// A plan-shaped reply that reads as prose: contains a complete plan JSON
/// object (so it has both `"validationContract"` and `"milestones"`), but a
/// trailing brace outside the object breaks `parse_report`'s first-`{`..last-`}`
/// slice, and there is no fenced code block to fall back on.
fn plan_prose(goal: &str) -> String {
    format!(
        "Here is my plan:\n{}\nLet me know if you'd like changes {{done}}",
        plan_json(goal)
    )
}

fn write_ticket(repo: &Path, slug: &str, body: &str) -> Ticket {
    let dir = Ticket::tickets_dir(repo);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{slug}.md"));
    std::fs::write(&path, body).unwrap();
    Ticket::load(&path).unwrap()
}

const TICKET_BODY: &str = "\
---
title: Rate limit
priority: 2
---

## Goal
Add rate limiting.
";

// ---------------------------------------------------------------------------
// Ready plan → parked in Review
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ready_plan_parks_in_review_with_mission_recorded() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "rl", TICKET_BODY);

    let goal = ticket.mission_goal();
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            plan_json(&goal),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    let mission_id = engine.mission_id().to_string();

    let drive = drive_draft(&mut engine, &root, &ticket, false)
        .await
        .unwrap();

    match &drive.outcome {
        DraftOutcome::ParkedForReview {
            mission_id: out_id,
            mission_branch,
        } => {
            assert_eq!(out_id, &mission_id);
            assert_eq!(mission_branch, &format!("kranz/mission-{mission_id}"));
        }
        other => panic!("expected ParkedForReview, got {other:?}"),
    }
    assert_eq!(Ticket::read_state(&root, "rl"), TicketState::Review);
    assert_eq!(
        Ticket::mission_for(&root, "rl").as_deref(),
        Some(mission_id.as_str())
    );
    assert!(!queue::contains(&root, &mission_id));

    // The seed reply and approved plan must be surfaced, not dropped, so the
    // CLI (and any surface) can reproduce the pre-hoist stdout.
    assert_eq!(drive.seed_reply.as_deref(), Some("ready"));
    let plan = drive.plan.expect("Approve path must surface the plan");
    assert_eq!(plan.goal, goal);
}

// ---------------------------------------------------------------------------
// Ticket notes (D-BW-3): the discussion rides the drafter's seed message
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ticket_note_discussion_rides_the_draft_seed_without_forking_the_goal() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "rl-notes", TICKET_BODY);
    kranz_engine::ticket_notes::append_note(&root, "rl-notes", "alice", "why priority changed")
        .unwrap();
    kranz_engine::ticket_notes::append_note(&root, "rl-notes", "bob", "review: keep it small")
        .unwrap();

    let goal = ticket.mission_goal();
    let backend = Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
        "seeded, thinking...".to_string(),
        plan_json(&goal),
    ])]));
    let mut engine =
        MissionEngine::create(backend.clone(), root.clone(), &goal, test_cfg()).unwrap();
    let drive = drive_draft(&mut engine, &root, &ticket, false)
        .await
        .unwrap();
    assert!(matches!(
        drive.outcome,
        DraftOutcome::ParkedForReview { .. }
    ));

    // The orchestrator's seed message carries the folded ticket AND the notes.
    let injected = backend.injected_messages();
    assert_eq!(injected.len(), 1, "only the orchestrator session ran");
    let seed = &injected[0][0];
    assert!(seed.contains("Add rate limiting."), "ticket goal: {seed}");
    assert!(seed.contains("## Ticket notes"), "notes section: {seed}");
    assert!(
        seed.contains("alice: why priority changed"),
        "note 1: {seed}"
    );
    assert!(
        seed.contains("bob: review: keep it small"),
        "note 2: {seed}"
    );

    // …but the RECORDED mission goal is not forked: approve's goal-matching
    // and parse_task_class_from_goal read back exactly mission_goal().
    assert_eq!(engine.state().mission.goal, goal);
    assert!(!engine.state().mission.goal.contains("Ticket notes"));
}

// ---------------------------------------------------------------------------
// Ready plan + then_enqueue=true → Queued + queue entry
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ready_plan_with_then_enqueue_queues_the_mission() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "rl2", TICKET_BODY);

    let goal = ticket.mission_goal();
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            plan_json(&goal),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    let mission_id = engine.mission_id().to_string();

    let drive = drive_draft(&mut engine, &root, &ticket, true)
        .await
        .unwrap();

    match &drive.outcome {
        DraftOutcome::Enqueued { mission_id: out_id } => assert_eq!(out_id, &mission_id),
        other => panic!("expected Enqueued, got {other:?}"),
    }
    assert_eq!(Ticket::read_state(&root, "rl2"), TicketState::Queued);
    assert!(queue::contains(&root, &mission_id));
    assert!(drive.plan.is_some());
}

// ---------------------------------------------------------------------------
// NotReady reply → NeedsContext, questions appended
// ---------------------------------------------------------------------------

#[tokio::test]
async fn not_ready_reply_appends_needs_context_and_flips_state() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "rl3", TICKET_BODY);

    let goal = ticket.mission_goal();
    // request_plan retries once on unparseable JSON before giving up; both the
    // first turn and the retry come back as prose here.
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            "let me think about this some more".to_string(),
            "- which auth backend?\n- is postgres available?".to_string(),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    let mission_id = engine.mission_id().to_string();

    let drive = drive_draft(&mut engine, &root, &ticket, false)
        .await
        .unwrap();

    match &drive.outcome {
        DraftOutcome::NeedsContext {
            mission_id: out_id,
            questions,
        } => {
            assert_eq!(out_id, &mission_id);
            assert_eq!(
                questions,
                &vec![
                    "which auth backend?".to_string(),
                    "is postgres available?".to_string(),
                ]
            );
        }
        other => panic!("expected NeedsContext, got {other:?}"),
    }
    assert_eq!(Ticket::read_state(&root, "rl3"), TicketState::NeedsContext);
    assert!(
        drive.plan.is_none(),
        "NeedsContext path must not surface a plan"
    );

    let path = Ticket::tickets_dir(&root).join("rl3.md");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("## Needs context (from orchestrator)"));
    assert!(body.contains("- which auth backend?"));
    assert!(body.contains("- is postgres available?"));
}

// ---------------------------------------------------------------------------
// looks_like_plan_json heuristic
// ---------------------------------------------------------------------------

#[test]
fn looks_like_plan_json_matches_both_keys_only() {
    assert!(looks_like_plan_json(
        r#"{"validationContract": [], "milestones": []}"#
    ));
    assert!(!looks_like_plan_json(r#"{"validationContract": []}"#));
    assert!(!looks_like_plan_json(r#"{"milestones": []}"#));
    assert!(!looks_like_plan_json("just some prose"));
}

// ---------------------------------------------------------------------------
// Plan-as-prose → bounded plan-channel retry recovers → approved like Ready
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plan_as_prose_recovers_via_bounded_retry_and_approves() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "rl4", TICKET_BODY);

    let goal = ticket.mission_goal();
    // Turn order: seed ("ready") -> planning_turn -> request_plan#1 attempt
    // (plan-shaped prose, fails parse) -> request_plan#1 retry (still prose,
    // still fails parse) -> drive_draft's bounded extra request_plan#2 attempt
    // (clean JSON, parses immediately).
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            plan_prose(&goal),
            plan_prose(&goal),
            plan_json(&goal),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    let mission_id = engine.mission_id().to_string();

    let drive = drive_draft(&mut engine, &root, &ticket, false)
        .await
        .unwrap();

    match &drive.outcome {
        DraftOutcome::ParkedForReview {
            mission_id: out_id,
            mission_branch,
        } => {
            assert_eq!(out_id, &mission_id);
            assert_eq!(mission_branch, &format!("kranz/mission-{mission_id}"));
        }
        other => panic!("expected ParkedForReview, got {other:?}"),
    }
    assert_eq!(Ticket::read_state(&root, "rl4"), TicketState::Review);
    let plan = drive
        .plan
        .expect("recovered Approve path must surface the plan");
    assert_eq!(plan.goal, goal);

    let path = Ticket::tickets_dir(&root).join("rl4.md");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        !body.contains("validationContract"),
        "plan JSON must never be filed to the ticket body"
    );
}

#[tokio::test]
async fn plan_as_prose_recovers_via_bounded_retry_and_enqueues() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "rl5", TICKET_BODY);

    let goal = ticket.mission_goal();
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            plan_prose(&goal),
            plan_prose(&goal),
            plan_json(&goal),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    let mission_id = engine.mission_id().to_string();

    let drive = drive_draft(&mut engine, &root, &ticket, true)
        .await
        .unwrap();

    match &drive.outcome {
        DraftOutcome::Enqueued { mission_id: out_id } => assert_eq!(out_id, &mission_id),
        other => panic!("expected Enqueued, got {other:?}"),
    }
    assert_eq!(Ticket::read_state(&root, "rl5"), TicketState::Queued);
    assert!(queue::contains(&root, &mission_id));
    assert!(drive.plan.is_some());

    let path = Ticket::tickets_dir(&root).join("rl5.md");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(!body.contains("validationContract"));
}

// ---------------------------------------------------------------------------
// Plan-as-prose on BOTH the initial request and the bounded retry → honest
// PlanAsProse outcome, no questions filed, no plan JSON in the ticket body.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plan_as_prose_persists_through_retry_yields_honest_outcome() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "rl6", TICKET_BODY);

    let goal = ticket.mission_goal();
    // request_plan#1: attempt + retry, both plan-shaped prose. drive_draft's
    // bounded extra request_plan#2: attempt + retry, both plan-shaped prose
    // again. Exactly one extra request_plan call — no looping.
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            plan_prose(&goal),
            plan_prose(&goal),
            plan_prose(&goal),
            plan_prose(&goal),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    let mission_id = engine.mission_id().to_string();

    let drive = drive_draft(&mut engine, &root, &ticket, false)
        .await
        .unwrap();

    match &drive.outcome {
        DraftOutcome::PlanAsProse { mission_id: out_id } => {
            assert_eq!(out_id, &mission_id);
        }
        other => panic!("expected PlanAsProse, got {other:?}"),
    }
    assert!(drive.plan.is_none());
    assert_eq!(Ticket::read_state(&root, "rl6"), TicketState::NeedsContext);

    let path = Ticket::tickets_dir(&root).join("rl6.md");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(
        !body.contains("## Needs context (from orchestrator)"),
        "plan-as-prose must not append the plan JSON as filed questions"
    );
    assert!(!body.contains("validationContract"));

    let status_path = Ticket::tickets_dir(&root).join("rl6.status");
    let status = std::fs::read_to_string(&status_path).unwrap();
    assert!(status.contains("emitted it as prose"));
}

// ---------------------------------------------------------------------------
// WrongPlan escalation — the planner's third voice (planner-initiated only)
// ---------------------------------------------------------------------------

fn wrong_plan_json(reason: &str) -> String {
    json!({ "wrongPlan": reason }).to_string()
}

const WRONG_PLAN_REASON: &str = "The goal assumes a Postgres migration, but the store is \
SQLite — any plan built on that premise is confidently wrong.";

#[tokio::test]
async fn wrong_plan_reply_parks_ticket_with_reason() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "wp", TICKET_BODY);

    let goal = ticket.mission_goal();
    // The escalation lands on the FIRST plan-request attempt — no JSON retry.
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            wrong_plan_json(WRONG_PLAN_REASON),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    let mission_id = engine.mission_id().to_string();

    let drive = drive_draft(&mut engine, &root, &ticket, false)
        .await
        .unwrap();

    match &drive.outcome {
        DraftOutcome::WrongPlan {
            mission_id: out_id,
            reason,
        } => {
            assert_eq!(out_id, &mission_id);
            assert_eq!(reason, WRONG_PLAN_REASON);
        }
        other => panic!("expected WrongPlan, got {other:?}"),
    }
    assert_eq!(Ticket::read_state(&root, "wp"), TicketState::WrongPlan);
    assert!(
        drive.plan.is_none(),
        "WrongPlan path must not surface a plan"
    );
    assert!(
        !queue::contains(&root, &mission_id),
        "a wrong-plan escalation never enqueues"
    );

    // The ticket body carries the escalation section…
    let path = Ticket::tickets_dir(&root).join("wp.md");
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.contains("## Wrong plan (from orchestrator)"));
    assert!(body.contains(WRONG_PLAN_REASON));
    assert!(
        !body.contains("## Needs context (from orchestrator)"),
        "a wrong-plan escalation is not a needs-context park"
    );

    // …and the .status note carries the reason with the WRONG-PLAN prefix,
    // under the kebab-case wire state.
    let status = std::fs::read_to_string(Ticket::tickets_dir(&root).join("wp.status")).unwrap();
    assert!(
        status.contains("\"wrong-plan\""),
        "kebab wire state: {status}"
    );
    assert!(
        status.contains(&format!("WRONG-PLAN: {WRONG_PLAN_REASON}")),
        "note carries the prefixed reason: {status}"
    );
}

#[tokio::test]
async fn wrong_plan_on_the_json_retry_also_escalates() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "wp2", TICKET_BODY);

    let goal = ticket.mission_goal();
    // First attempt is prose (triggers the JSON-only retry); the retry carries
    // the escalation JSON.
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            "hmm, something is off here".to_string(),
            wrong_plan_json(WRONG_PLAN_REASON),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();

    let drive = drive_draft(&mut engine, &root, &ticket, false)
        .await
        .unwrap();

    match &drive.outcome {
        DraftOutcome::WrongPlan { reason, .. } => assert_eq!(reason, WRONG_PLAN_REASON),
        other => panic!("expected WrongPlan, got {other:?}"),
    }
    assert_eq!(Ticket::read_state(&root, "wp2"), TicketState::WrongPlan);
}

#[tokio::test]
async fn wrong_plan_is_never_inferred_from_prose() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "wp3", TICKET_BODY);

    let goal = ticket.mission_goal();
    // Prose that TALKS about the plan being wrong — no {"wrongPlan": …} JSON —
    // stays a plain not-ready reply: the escalation is planner-initiated via
    // the JSON channel only.
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            "I can draft this, but honestly the goal seems misframed — the premise may be \
             broken."
                .to_string(),
            "still concerned the spec is confidently off".to_string(),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();

    let drive = drive_draft(&mut engine, &root, &ticket, false)
        .await
        .unwrap();

    match &drive.outcome {
        DraftOutcome::NeedsContext { .. } => {}
        other => panic!("prose must degrade to NeedsContext, never WrongPlan: {other:?}"),
    }
    assert_eq!(Ticket::read_state(&root, "wp3"), TicketState::NeedsContext);
}

#[tokio::test]
async fn request_plan_maps_wrong_plan_json_to_the_variant() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "wp4", TICKET_BODY);

    let goal = ticket.mission_goal();
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            wrong_plan_json(WRONG_PLAN_REASON),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    engine.planning_turn(&goal).await.unwrap();

    match engine.request_plan().await.unwrap() {
        PlanRequest::WrongPlan { reason } => assert_eq!(reason, WRONG_PLAN_REASON),
        PlanRequest::Ready(plan) => panic!("escalation JSON must not parse as a plan: {plan:?}"),
        PlanRequest::NotReady(text) => panic!("escalation JSON must not degrade to prose: {text}"),
    }
}

#[tokio::test]
async fn request_plan_still_parses_plan_json_as_ready() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "wp5", TICKET_BODY);

    let goal = ticket.mission_goal();
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            plan_json(&goal),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    engine.planning_turn(&goal).await.unwrap();

    match engine.request_plan().await.unwrap() {
        PlanRequest::Ready(plan) => assert_eq!(plan.goal, goal),
        PlanRequest::WrongPlan { reason } => {
            panic!("plan JSON must never map to the escalation: {reason}")
        }
        PlanRequest::NotReady(text) => panic!("scripted plan JSON must parse, got: {text}"),
    }
}

#[tokio::test]
async fn redraft_after_wrong_plan_is_accepted() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let ticket = write_ticket(&root, "wp6", TICKET_BODY);

    // First draft: the planner escalates.
    let goal = ticket.mission_goal();
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            wrong_plan_json(WRONG_PLAN_REASON),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    let drive = drive_draft(&mut engine, &root, &ticket, false)
        .await
        .unwrap();
    assert!(matches!(drive.outcome, DraftOutcome::WrongPlan { .. }));
    assert_eq!(Ticket::read_state(&root, "wp6"), TicketState::WrongPlan);
    drop(engine);

    // Re-draft (a fresh mission, exactly like re-drafting a needs-context
    // ticket): the ticket body now carries the escalation section, and the new
    // planner returns a plan — parked in Review like any ready draft.
    let ticket = Ticket::load(&Ticket::tickets_dir(&root).join("wp6.md")).unwrap();
    let goal = ticket.mission_goal();
    let backend: Arc<dyn AgentBackend> =
        Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
            "seeded, thinking...".to_string(),
            plan_json(&goal),
        ])]));
    let mut engine = MissionEngine::create(backend, root.clone(), &goal, test_cfg()).unwrap();
    let drive = drive_draft(&mut engine, &root, &ticket, false)
        .await
        .unwrap();
    assert!(
        matches!(drive.outcome, DraftOutcome::ParkedForReview { .. }),
        "re-draft of a wrong-plan ticket is accepted: {:?}",
        drive.outcome
    );
    assert_eq!(Ticket::read_state(&root, "wp6"), TicketState::Review);
}
