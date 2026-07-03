//! Integration tests for the backlog CLI (`kranz ticket …`, `draft`, `queue`,
//! `work`).
//!
//! No real `claude` binary is ever spawned: the two handlers that would
//! (`draft`, `work`) are exercised only through their pure decision helpers
//! (`draft_decision`, `next_work_action`, `ticket_state_for_mission`). Parsing
//! is checked with clap `try_parse_from`; rendering and scaffolding run against
//! hand-written tickets in a tempdir; ticket-state transitions are asserted via
//! `Ticket::read_state` after calling the state writers directly.

use clap::Parser;
use kranz_cli::backlog::{
    self, draft_decision, next_work_action, render_queue, render_ticket_list, render_ticket_show,
    ticket_state_for_mission, ticket_template, DraftDecision, TicketRow, WorkAction,
};
use kranz_cli::cli::{Cli, Command, TicketCommand};
use kranz_engine::orchestrator::PlanRequest;
use kranz_engine::queue::{self, QueueEntry};
use kranz_engine::ticket::{Ticket, TicketState};
use kranz_engine::types::{MissionStatus, Plan, PlanFeature, PlanMilestone};
use std::path::Path;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn write_ticket(repo: &Path, slug: &str, body: &str) {
    let dir = Ticket::tickets_dir(repo);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{slug}.md")), body).unwrap();
}

fn sample_plan() -> Plan {
    Plan {
        goal: "ship the thing".to_string(),
        validation_contract: vec![],
        milestones: vec![PlanMilestone {
            title: "M1".to_string(),
            features: vec![PlanFeature {
                title: "F1".to_string(),
                spec: "do it".to_string(),
                validation_criteria: vec!["works".to_string()],
            }],
        }],
    }
}

fn entry(mission: &str, slug: Option<&str>, priority: u8, seq: u64) -> QueueEntry {
    QueueEntry {
        mission_id: mission.to_string(),
        ticket_slug: slug.map(str::to_string),
        priority,
        seq,
    }
}

// ---------------------------------------------------------------------------
// clap parsing — every new subcommand + flag
// ---------------------------------------------------------------------------

#[test]
fn parses_ticket_list() {
    let cli = Cli::try_parse_from(["kranz", "ticket", "list"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Ticket { command: TicketCommand::List }
    ));
}

#[test]
fn parses_ticket_show() {
    let cli = Cli::try_parse_from(["kranz", "ticket", "show", "my-slug"]).unwrap();
    match cli.command {
        Command::Ticket { command: TicketCommand::Show { slug } } => assert_eq!(slug, "my-slug"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_ticket_new_with_title_and_goal() {
    let cli = Cli::try_parse_from([
        "kranz", "ticket", "new", "rate-limit", "--title", "Rate limit the API", "--goal",
        "cap requests per token",
    ])
    .unwrap();
    match cli.command {
        Command::Ticket {
            command: TicketCommand::New { slug, title, goal },
        } => {
            assert_eq!(slug, "rate-limit");
            assert_eq!(title, "Rate limit the API");
            assert_eq!(goal.as_deref(), Some("cap requests per token"));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn ticket_new_requires_title() {
    // --title is mandatory.
    assert!(Cli::try_parse_from(["kranz", "ticket", "new", "slug"]).is_err());
}

#[test]
fn parses_ticket_approve_with_mission() {
    let cli =
        Cli::try_parse_from(["kranz", "ticket", "approve", "slug", "--mission", "m-abc123"]).unwrap();
    match cli.command {
        Command::Ticket {
            command: TicketCommand::Approve { slug, mission },
        } => {
            assert_eq!(slug, "slug");
            assert_eq!(mission.as_deref(), Some("m-abc123"));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_draft_and_draft_yes() {
    let plain = Cli::try_parse_from(["kranz", "draft", "slug"]).unwrap();
    match plain.command {
        Command::Draft { slug, yes } => {
            assert_eq!(slug, "slug");
            assert!(!yes);
        }
        other => panic!("unexpected: {other:?}"),
    }
    let yes = Cli::try_parse_from(["kranz", "draft", "slug", "--yes"]).unwrap();
    assert!(matches!(yes.command, Command::Draft { yes: true, .. }));
}

#[test]
fn parses_queue() {
    let cli = Cli::try_parse_from(["kranz", "queue"]).unwrap();
    assert!(matches!(cli.command, Command::Queue));
}

#[test]
fn parses_work_and_work_once() {
    let drain = Cli::try_parse_from(["kranz", "work"]).unwrap();
    assert!(matches!(drain.command, Command::Work { once: false }));
    let once = Cli::try_parse_from(["kranz", "work", "--once"]).unwrap();
    assert!(matches!(once.command, Command::Work { once: true }));
}

// ---------------------------------------------------------------------------
// ticket new — scaffold produces a parseable ticket
// ---------------------------------------------------------------------------

#[test]
fn template_parses_back_and_carries_goal() {
    let body = ticket_template("Rate limit the API", Some("cap requests per token"));
    let ticket = Ticket::parse("rate-limit", &body).expect("template must parse");
    assert_eq!(ticket.title, "Rate limit the API");
    assert_eq!(ticket.priority, 2);
    assert_eq!(ticket.goal.trim(), "cap requests per token");
}

#[test]
fn template_without_goal_parses_with_empty_goal() {
    let body = ticket_template("A title", None);
    let ticket = Ticket::parse("slug", &body).expect("template must parse");
    assert_eq!(ticket.title, "A title");
    assert!(ticket.goal.trim().is_empty());
}

#[test]
fn cmd_ticket_new_writes_file_and_sets_new_state() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    let path = backlog::cmd_ticket_new(repo, "my-ticket", "My Ticket", Some("the goal")).unwrap();
    assert!(path.is_file());

    // Round-trips through the engine parser.
    let ticket = Ticket::load(&path).unwrap();
    assert_eq!(ticket.title, "My Ticket");
    assert_eq!(ticket.goal.trim(), "the goal");

    // A freshly scaffolded ticket has no status file → New.
    assert_eq!(Ticket::read_state(repo, "my-ticket"), TicketState::New);
}

#[test]
fn cmd_ticket_new_refuses_to_overwrite() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    backlog::cmd_ticket_new(repo, "dup", "First", None).unwrap();
    let err = backlog::cmd_ticket_new(repo, "dup", "Second", None).unwrap_err();
    assert!(err.to_string().contains("already exists"));
}

// ---------------------------------------------------------------------------
// ticket list / show rendering
// ---------------------------------------------------------------------------

#[test]
fn render_ticket_list_empty() {
    assert_eq!(render_ticket_list(&[]), "no tickets\n");
}

#[test]
fn render_ticket_list_from_tempdir() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_ticket(
        repo,
        "high",
        "---\ntitle: High priority\npriority: 1\n---\n## Goal\ndo it\n",
    );
    write_ticket(
        repo,
        "low",
        "---\ntitle: Low priority\npriority: 3\n---\n## Goal\nlater\n",
    );
    // Give `low` a state so its label renders.
    Ticket::write_state(repo, "low", TicketState::Review, None).unwrap();

    let tickets = Ticket::list(repo);
    let rows: Vec<TicketRow<'_>> = tickets
        .iter()
        .map(|t| TicketRow { ticket: t, state: Ticket::read_state(repo, &t.slug) })
        .collect();
    let out = render_ticket_list(&rows);

    // Header + both slugs; `high` (priority 1) sorts before `low`.
    assert!(out.contains("SLUG"));
    assert!(out.contains("high"));
    assert!(out.contains("High priority"));
    assert!(out.contains("REVIEW"));
    assert!(out.contains("NEW"));
    let high_at = out.find("high").unwrap();
    let low_at = out.find("low").unwrap();
    assert!(high_at < low_at, "priority 1 must sort first:\n{out}");
}

#[test]
fn render_ticket_show_includes_sections_and_needs_context() {
    let ticket = Ticket::parse(
        "demo",
        "---\ntitle: Demo\npriority: 2\n---\n\
         ## Goal\nbuild it\n\
         ## Context\nbecause reasons\n\
         ## Scoping answers\n- test: cargo test\n\
         ## Acceptance hints\n- all green\n\
         ## Needs context (from orchestrator)\n- which module?\n",
    )
    .unwrap();
    let out = render_ticket_show(&ticket, TicketState::NeedsContext);

    assert!(out.contains("ticket demo [NEEDS-CONTEXT]"));
    assert!(out.contains("build it"));
    assert!(out.contains("because reasons"));
    assert!(out.contains("test: cargo test"));
    assert!(out.contains("all green"));
    // The verbatim needs-context block is surfaced.
    assert!(out.contains("Needs context (from orchestrator)"));
    assert!(out.contains("which module?"));
}

#[test]
fn cmd_ticket_show_errors_on_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let err = backlog::cmd_ticket_show(tmp.path(), "nope").unwrap_err();
    assert!(err.to_string().contains("not found"));
}

// ---------------------------------------------------------------------------
// queue rendering
// ---------------------------------------------------------------------------

#[test]
fn render_queue_empty_idle() {
    let out = render_queue(&[], None);
    assert!(out.contains("repo idle"));
    assert!(out.contains("queue empty"));
}

#[test]
fn render_queue_lists_positions_and_busy() {
    let entries = vec![
        entry("m-aaa", Some("ticket-a"), 1, 0),
        entry("m-bbb", None, 2, 1),
    ];
    let out = render_queue(&entries, Some("m-running"));
    assert!(out.contains("repo busy: mission m-running is running"));
    assert!(out.contains("m-aaa"));
    assert!(out.contains("ticket-a"));
    assert!(out.contains("m-bbb"));
    // No slug renders as `-`.
    assert!(out.lines().any(|l| l.contains("m-bbb") && l.trim_end().ends_with('-')));
}

// ---------------------------------------------------------------------------
// draft decision helper (PlanRequest + --yes → next TicketState)
// ---------------------------------------------------------------------------

#[test]
fn draft_ready_without_yes_parks_for_review() {
    let req = PlanRequest::Ready(sample_plan());
    assert_eq!(
        draft_decision(&req, false),
        DraftDecision::Approve { then_enqueue: false, next_state: TicketState::Review }
    );
}

#[test]
fn draft_ready_with_yes_enqueues() {
    let req = PlanRequest::Ready(sample_plan());
    assert_eq!(
        draft_decision(&req, true),
        DraftDecision::Approve { then_enqueue: true, next_state: TicketState::Queued }
    );
}

#[test]
fn draft_not_ready_needs_context_splits_questions() {
    let req = PlanRequest::NotReady(
        "I need to know:\n- which test runner?\n- is auth in scope?\n".to_string(),
    );
    match draft_decision(&req, true) {
        DraftDecision::NeedsContext { questions } => {
            // The `--yes` flag never overrides a NeedsContext short-circuit.
            assert_eq!(
                questions,
                vec![
                    "I need to know:".to_string(),
                    "which test runner?".to_string(),
                    "is auth in scope?".to_string(),
                ]
            );
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn draft_not_ready_single_line_becomes_one_question() {
    let req = PlanRequest::NotReady("What is the deploy target?".to_string());
    match draft_decision(&req, false) {
        DraftDecision::NeedsContext { questions } => {
            assert_eq!(questions, vec!["What is the deploy target?".to_string()]);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn ticket_state_for_mission_maps_terminal_status() {
    assert_eq!(ticket_state_for_mission(MissionStatus::Complete), TicketState::Done);
    assert_eq!(ticket_state_for_mission(MissionStatus::Blocked), TicketState::Failed);
    assert_eq!(ticket_state_for_mission(MissionStatus::Failed), TicketState::Failed);
}

// ---------------------------------------------------------------------------
// work dispatcher next-action helper
// ---------------------------------------------------------------------------

#[test]
fn work_action_empty_queue() {
    assert_eq!(next_work_action(None, None), WorkAction::Empty);
    // Even when the repo is busy, an empty queue means nothing to do.
    assert_eq!(next_work_action(None, Some("m-x")), WorkAction::Empty);
}

#[test]
fn work_action_busy_waits() {
    let front = entry("m-front", Some("t"), 1, 0);
    assert_eq!(
        next_work_action(Some(&front), Some("m-running")),
        WorkAction::Busy { mission_id: "m-running".to_string() }
    );
}

#[test]
fn work_action_runs_front_when_idle() {
    let front = entry("m-front", Some("ticket-x"), 1, 0);
    assert_eq!(
        next_work_action(Some(&front), None),
        WorkAction::Run {
            mission_id: "m-front".to_string(),
            ticket_slug: Some("ticket-x".to_string())
        }
    );
}

// ---------------------------------------------------------------------------
// ticket-state transitions via the engine's read_state, driven by helpers
// ---------------------------------------------------------------------------

#[test]
fn state_transitions_new_to_drafting_to_review() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    backlog::cmd_ticket_new(repo, "flow", "Flow", Some("goal")).unwrap();
    assert_eq!(Ticket::read_state(repo, "flow"), TicketState::New);

    Ticket::write_state(repo, "flow", TicketState::Drafting, None).unwrap();
    assert_eq!(Ticket::read_state(repo, "flow"), TicketState::Drafting);

    // Ready → Review (parked) is what draft_decision names without --yes.
    let decision = draft_decision(&PlanRequest::Ready(sample_plan()), false);
    let DraftDecision::Approve { next_state, .. } = decision else {
        panic!("expected Approve");
    };
    Ticket::write_state(repo, "flow", next_state, None).unwrap();
    assert_eq!(Ticket::read_state(repo, "flow"), TicketState::Review);
}

#[test]
fn state_transitions_needs_context_appends_and_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    backlog::cmd_ticket_new(repo, "under", "Underspecified", None).unwrap();

    let DraftDecision::NeedsContext { questions } =
        draft_decision(&PlanRequest::NotReady("- what deps?\n".to_string()), true)
    else {
        panic!("expected NeedsContext");
    };
    Ticket::append_needs_context(repo, "under", &questions).unwrap();

    assert_eq!(Ticket::read_state(repo, "under"), TicketState::NeedsContext);
    // The question was appended verbatim to the ticket body.
    let reloaded = Ticket::load(&Ticket::tickets_dir(repo).join("under.md")).unwrap();
    assert!(reloaded.raw_body.contains("what deps?"));
    // And `show` surfaces it.
    let shown = render_ticket_show(&reloaded, TicketState::NeedsContext);
    assert!(shown.contains("what deps?"));
}

#[test]
fn ticket_approve_enqueues_review_ticket_and_sets_queued() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    backlog::cmd_ticket_new(repo, "approve-me", "Approve Me", Some("g")).unwrap();
    Ticket::write_state(repo, "approve-me", TicketState::Review, None).unwrap();

    // Explicit --mission avoids needing a real drafted mission on disk.
    let code = backlog::cmd_ticket_approve(repo, "approve-me", Some("m-explicit")).unwrap();
    assert_eq!(code, 0);

    assert_eq!(Ticket::read_state(repo, "approve-me"), TicketState::Queued);
    let queued = queue::list(repo);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].mission_id, "m-explicit");
    assert_eq!(queued[0].ticket_slug.as_deref(), Some("approve-me"));
    assert_eq!(queued[0].priority, 2);
}

#[test]
fn ticket_approve_refuses_non_review_ticket() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    backlog::cmd_ticket_new(repo, "fresh", "Fresh", None).unwrap();
    // Still New → cannot approve.
    let err = backlog::cmd_ticket_approve(repo, "fresh", Some("m-x")).unwrap_err();
    assert!(err.to_string().contains("REVIEW"));
    assert!(queue::list(repo).is_empty());
}
