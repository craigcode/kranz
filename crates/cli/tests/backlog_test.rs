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
    self, cmd_queue_remove, draft_decision, next_work_action, render_queue, render_ticket_list,
    render_ticket_show, ticket_state_for_mission, ticket_template, work_skip_for_failed_blocker,
    DraftDecision, TicketRow, WorkAction,
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
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec![],
        standards_manifest: None,
        reviewer_independence: None,
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
        Command::Ticket {
            command: TicketCommand::List
        }
    ));
}

#[test]
fn parses_ticket_show() {
    let cli = Cli::try_parse_from(["kranz", "ticket", "show", "my-slug"]).unwrap();
    match cli.command {
        Command::Ticket {
            command: TicketCommand::Show { slug },
        } => assert_eq!(slug, "my-slug"),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_ticket_new_with_title_and_goal() {
    let cli = Cli::try_parse_from([
        "kranz",
        "ticket",
        "new",
        "rate-limit",
        "--title",
        "Rate limit the API",
        "--goal",
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
    let cli = Cli::try_parse_from([
        "kranz",
        "ticket",
        "approve",
        "slug",
        "--mission",
        "m-abc123",
    ])
    .unwrap();
    match cli.command {
        Command::Ticket {
            command:
                TicketCommand::Approve {
                    slug,
                    mission,
                    force,
                },
        } => {
            assert_eq!(slug, "slug");
            assert_eq!(mission.as_deref(), Some("m-abc123"));
            assert!(!force);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_ticket_approve_with_force() {
    let cli = Cli::try_parse_from(["kranz", "ticket", "approve", "slug", "--force"]).unwrap();
    match cli.command {
        Command::Ticket {
            command: TicketCommand::Approve { slug, force, .. },
        } => {
            assert_eq!(slug, "slug");
            assert!(force);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_ticket_queue_with_mission() {
    let cli =
        Cli::try_parse_from(["kranz", "ticket", "queue", "slug", "--mission", "m-abc123"]).unwrap();
    match cli.command {
        Command::Ticket {
            command:
                TicketCommand::Queue {
                    slug,
                    mission,
                    force,
                },
        } => {
            assert_eq!(slug, "slug");
            assert_eq!(mission.as_deref(), Some("m-abc123"));
            assert!(!force);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_ticket_queue_with_force() {
    let cli = Cli::try_parse_from(["kranz", "ticket", "queue", "slug", "--force"]).unwrap();
    match cli.command {
        Command::Ticket {
            command: TicketCommand::Queue { slug, force, .. },
        } => {
            assert_eq!(slug, "slug");
            assert!(force);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

/// D-A: `ticket queue` is the ticket-queueing verb; `ticket approve` is kept
/// as a deprecated alias. Both must parse to the same slug/mission/force
/// shape (just a different enum variant) — proving the alias carries no
/// behavioral drift from the renamed command.
#[test]
fn ticket_queue_alias_parses_same_as_approve() {
    let queue_cli =
        Cli::try_parse_from(["kranz", "ticket", "queue", "slug", "--mission", "m-1"]).unwrap();
    let approve_cli =
        Cli::try_parse_from(["kranz", "ticket", "approve", "slug", "--mission", "m-1"]).unwrap();

    let Command::Ticket {
        command:
            TicketCommand::Queue {
                slug: qs,
                mission: qm,
                force: qf,
            },
    } = queue_cli.command
    else {
        panic!("expected TicketCommand::Queue");
    };
    let Command::Ticket {
        command:
            TicketCommand::Approve {
                slug: as_,
                mission: am,
                force: af,
            },
    } = approve_cli.command
    else {
        panic!("expected TicketCommand::Approve");
    };

    assert_eq!(qs, as_);
    assert_eq!(qm, am);
    assert_eq!(qf, af);
}

/// D-A: `ticket approve` must still parse — it is a deprecated alias kept
/// for one release, not removed.
#[test]
fn ticket_queue_alias_approve_still_parses() {
    let cli = Cli::try_parse_from(["kranz", "ticket", "approve", "slug"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Ticket {
            command: TicketCommand::Approve { .. }
        }
    ));
}

#[test]
fn parses_draft_and_draft_yes() {
    let plain = Cli::try_parse_from(["kranz", "draft", "slug"]).unwrap();
    match plain.command {
        Command::Draft {
            slug,
            yes,
            from_mission,
        } => {
            assert_eq!(slug, "slug");
            assert!(!yes);
            assert_eq!(from_mission, None);
        }
        other => panic!("unexpected: {other:?}"),
    }
    let yes = Cli::try_parse_from(["kranz", "draft", "slug", "--yes"]).unwrap();
    assert!(matches!(yes.command, Command::Draft { yes: true, .. }));
}

#[test]
fn parses_draft_from_mission() {
    let cli = Cli::try_parse_from([
        "kranz",
        "draft",
        "defect-slug",
        "--from-mission",
        "m-abc123",
    ])
    .unwrap();
    match cli.command {
        Command::Draft {
            slug,
            yes,
            from_mission,
        } => {
            assert_eq!(slug, "defect-slug");
            assert!(!yes);
            assert_eq!(from_mission.as_deref(), Some("m-abc123"));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_queue() {
    let cli = Cli::try_parse_from(["kranz", "queue"]).unwrap();
    assert!(matches!(cli.command, Command::Queue { remove: None }));

    let remove = Cli::try_parse_from(["kranz", "queue", "--remove", "m-abc123"]).unwrap();
    assert!(matches!(
        remove.command,
        Command::Queue {
            remove: Some(ref mission_id)
        } if mission_id == "m-abc123"
    ));
}

#[test]
fn parses_work_and_work_once() {
    let drain = Cli::try_parse_from(["kranz", "work"]).unwrap();
    assert!(matches!(
        drain.command,
        Command::Work {
            once: false,
            expect: None
        }
    ));
    let once = Cli::try_parse_from(["kranz", "work", "--once"]).unwrap();
    assert!(matches!(
        once.command,
        Command::Work {
            once: true,
            expect: None
        }
    ));
    let expected =
        Cli::try_parse_from(["kranz", "work", "--once", "--expect", "m-abc123"]).unwrap();
    assert!(matches!(
        expected.command,
        Command::Work {
            once: true,
            expect: Some(ref mission_id)
        } if mission_id == "m-abc123"
    ));
    assert!(Cli::try_parse_from(["kranz", "work", "--expect", "m-abc123"]).is_err());
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
        .map(|t| {
            let state = Ticket::read_state(repo, &t.slug);
            TicketRow {
                ticket: t,
                label: backlog::ticket_terminal_label(repo, &t.slug, state),
            }
        })
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
    let out = render_ticket_show(
        &ticket,
        backlog::ticket_state_label(TicketState::NeedsContext),
    );

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
// ticket ready — defer-until listing (D-BW-3)
// ---------------------------------------------------------------------------

#[test]
fn defer_until_parses_ticket_ready_flag() {
    let cli = Cli::try_parse_from(["kranz", "ticket", "ready"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Ticket {
            command: TicketCommand::Ready {
                include_deferred: false
            }
        }
    ));
    let cli = Cli::try_parse_from(["kranz", "ticket", "ready", "--include-deferred"]).unwrap();
    assert!(matches!(
        cli.command,
        Command::Ticket {
            command: TicketCommand::Ready {
                include_deferred: true
            }
        }
    ));
}

#[test]
fn defer_until_ready_listing_excludes_future_and_shows_past() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_ticket(
        repo,
        "ripened",
        "---\ntitle: Ripened\ndefer-until: 2000-01-01T00:00:00Z\n---\n## Goal\nnow\n",
    );
    write_ticket(
        repo,
        "parked",
        "---\ntitle: Parked\ndefer-until: 2999-01-01T00:00:00Z\n---\n## Goal\nlater\n",
    );
    write_ticket(repo, "plain", "---\ntitle: Plain\n---\n## Goal\nanytime\n");
    // In-flight / terminal states are never "ready", deferred or not.
    write_ticket(
        repo,
        "queued",
        "---\ntitle: Queued\n---\n## Goal\nrunning\n",
    );
    Ticket::write_state(repo, "queued", TicketState::Queued, None).unwrap();

    let out = backlog::cmd_ticket_ready(repo, false);
    assert!(out.contains("ripened"), "past deferral is ready:\n{out}");
    assert!(out.contains("plain"), "no deferral is ready:\n{out}");
    assert!(
        !out.contains("parked"),
        "future deferral is excluded from the default listing:\n{out}"
    );
    assert!(
        !out.contains("queued"),
        "in-flight states are not ready:\n{out}"
    );
}

#[test]
fn defer_until_include_deferred_lists_with_time() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_ticket(
        repo,
        "parked",
        "---\ntitle: Parked\ndefer-until: 2999-01-01T00:00:00Z\n---\n## Goal\nlater\n",
    );

    let out = backlog::cmd_ticket_ready(repo, true);
    assert!(
        out.contains("parked"),
        "--include-deferred shows it:\n{out}"
    );
    assert!(
        out.contains("2999-01-01T00:00:00Z"),
        "shown with its defer time:\n{out}"
    );
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
    assert!(out
        .lines()
        .any(|l| l.contains("m-bbb") && l.trim_end().ends_with('-')));
}

#[test]
fn queue_remove_retires_only_the_named_entry() {
    let repo = tempfile::tempdir().unwrap();
    for mission_id in ["m-remove1", "m-keep222"] {
        queue::enqueue(
            repo.path(),
            QueueEntry {
                mission_id: mission_id.to_string(),
                ticket_slug: None,
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();
    }

    let message = cmd_queue_remove(repo.path(), "m-remove1").unwrap();

    assert_eq!(message, "removed m-remove1 from the queue\n");
    assert!(!queue::contains(repo.path(), "m-remove1"));
    assert!(queue::contains(repo.path(), "m-keep222"));
    assert!(cmd_queue_remove(repo.path(), "m-remove1").is_err());
}

// ---------------------------------------------------------------------------
// draft decision helper (PlanRequest + --yes → next TicketState)
// ---------------------------------------------------------------------------

#[test]
fn draft_ready_without_yes_parks_for_review() {
    let req = PlanRequest::Ready(sample_plan());
    assert_eq!(
        draft_decision(&req, false),
        DraftDecision::Approve {
            then_enqueue: false,
            next_state: TicketState::Review
        }
    );
}

#[test]
fn draft_ready_with_yes_enqueues() {
    let req = PlanRequest::Ready(sample_plan());
    assert_eq!(
        draft_decision(&req, true),
        DraftDecision::Approve {
            then_enqueue: true,
            next_state: TicketState::Queued
        }
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
fn draft_wrong_plan_maps_to_escalation_decision() {
    let req = PlanRequest::WrongPlan {
        reason: "the premise is broken".to_string(),
    };
    // The `--yes` flag never overrides a WrongPlan short-circuit either.
    assert_eq!(
        draft_decision(&req, true),
        DraftDecision::WrongPlan {
            reason: "the premise is broken".to_string()
        }
    );
}

#[test]
fn ticket_state_for_mission_maps_terminal_status() {
    assert_eq!(
        ticket_state_for_mission(MissionStatus::Complete),
        TicketState::Done
    );
    assert_eq!(
        ticket_state_for_mission(MissionStatus::Blocked),
        TicketState::NeedsContext
    );
    assert_eq!(
        ticket_state_for_mission(MissionStatus::Failed),
        TicketState::Failed
    );
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
        WorkAction::Busy {
            mission_id: "m-running".to_string()
        }
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
    let shown = render_ticket_show(
        &reloaded,
        backlog::ticket_state_label(TicketState::NeedsContext),
    );
    assert!(shown.contains("what deps?"));
}

#[test]
fn state_transitions_wrong_plan_appends_and_flags() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    backlog::cmd_ticket_new(repo, "wrong", "Misframed", None).unwrap();

    let DraftDecision::WrongPlan { reason } = draft_decision(
        &PlanRequest::WrongPlan {
            reason: "the goal is misframed".to_string(),
        },
        false,
    ) else {
        panic!("expected WrongPlan");
    };
    Ticket::append_wrong_plan(repo, "wrong", &reason).unwrap();

    assert_eq!(Ticket::read_state(repo, "wrong"), TicketState::WrongPlan);
    // The label is distinct from NEEDS-CONTEXT.
    assert_eq!(
        backlog::ticket_state_label(TicketState::WrongPlan),
        "WRONG-PLAN"
    );
    // The reason was appended verbatim to the ticket body…
    let reloaded = Ticket::load(&Ticket::tickets_dir(repo).join("wrong.md")).unwrap();
    assert!(reloaded
        .raw_body
        .contains("## Wrong plan (from orchestrator)"));
    assert!(reloaded.raw_body.contains("the goal is misframed"));
    // …and `show` surfaces both the label and the reason.
    let shown = render_ticket_show(
        &reloaded,
        backlog::ticket_state_label(TicketState::WrongPlan),
    );
    assert!(shown.contains("[WRONG-PLAN]"));
    assert!(shown.contains("## Wrong plan (from orchestrator)"));
    assert!(shown.contains("the goal is misframed"));
}

#[test]
fn ticket_approve_enqueues_review_ticket_and_sets_queued() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    backlog::cmd_ticket_new(repo, "approve-me", "Approve Me", Some("g")).unwrap();
    Ticket::write_state(repo, "approve-me", TicketState::Review, None).unwrap();

    // Explicit --mission avoids needing a real drafted mission on disk.
    let code = backlog::cmd_ticket_approve(repo, "approve-me", Some("m-explicit"), false).unwrap();
    assert_eq!(code, 0);

    assert_eq!(Ticket::read_state(repo, "approve-me"), TicketState::Queued);
    let queued = queue::list(repo);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].mission_id, "m-explicit");
    assert_eq!(queued[0].ticket_slug.as_deref(), Some("approve-me"));
    assert_eq!(queued[0].priority, 2);
}

/// D-A: `cmd_ticket_queue` is the renamed core; proves it has the same
/// enqueue/state-transition behavior as the (now-deprecated) approve path.
#[test]
fn ticket_queue_alias_queue_enqueues_review_ticket_and_sets_queued() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    backlog::cmd_ticket_new(repo, "queue-me", "Queue Me", Some("g")).unwrap();
    Ticket::write_state(repo, "queue-me", TicketState::Review, None).unwrap();

    let code = backlog::cmd_ticket_queue(repo, "queue-me", Some("m-explicit"), false).unwrap();
    assert_eq!(code, 0);

    assert_eq!(Ticket::read_state(repo, "queue-me"), TicketState::Queued);
    let queued = queue::list(repo);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].mission_id, "m-explicit");
    assert_eq!(queued[0].ticket_slug.as_deref(), Some("queue-me"));
    assert_eq!(queued[0].priority, 2);
}

#[test]
fn ticket_approve_refuses_non_review_ticket() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    backlog::cmd_ticket_new(repo, "fresh", "Fresh", None).unwrap();
    // Still New → cannot approve.
    let err = backlog::cmd_ticket_approve(repo, "fresh", Some("m-x"), false).unwrap_err();
    assert!(err.to_string().contains("REVIEW"));
    assert!(queue::list(repo).is_empty());
}

// ---------------------------------------------------------------------------
// ticket approve — blocked-by gating (`blocked_by_approve_*`)
// ---------------------------------------------------------------------------

/// Writes a minimal events.jsonl for `mission_id` whose fold reaches
/// `MissionStatus::Complete` (MissionCreated then MissionCompleted), or stops
/// after MissionCreated when `complete` is false (leaves it Drafting).
/// Mirrors the twin helper in `engine/tests/ticket_queue_test.rs`.
fn write_mission_events(root: &Path, mission_id: &str, complete: bool) {
    use kranz_engine::events::{Event, EventKind};
    use kranz_engine::types::MissionConfig;

    let paths = kranz_engine::paths::MissionPaths::new(root, mission_id);
    let dir = paths.events_file().parent().unwrap().to_path_buf();
    std::fs::create_dir_all(&dir).unwrap();

    let ts = chrono::Utc::now();
    let mut events = vec![Event {
        seq: 1,
        ts,
        mission_id: mission_id.to_string(),
        kind: EventKind::MissionCreated {
            goal: "do the thing".to_string(),
            base_branch: "main".to_string(),
            mission_branch: format!("kranz/mission-{mission_id}"),
            config: MissionConfig::default(),
        },
    }];
    if complete {
        events.push(Event {
            seq: 2,
            ts,
            mission_id: mission_id.to_string(),
            kind: EventKind::MissionCompleted {},
        });
    }

    let body: String = events
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(paths.events_file(), body).unwrap();
}

/// Parks a REVIEW ticket with an optional `blocked-by` list, matching the
/// state `draft` (no `--yes`) leaves behind.
fn park_for_review(repo: &Path, slug: &str, blocked_by: &[&str]) {
    let body = if blocked_by.is_empty() {
        format!("---\ntitle: {slug}\n---\nbody\n")
    } else {
        format!(
            "---\ntitle: {slug}\nblocked-by: [{}]\n---\nbody\n",
            blocked_by.join(", ")
        )
    };
    write_ticket(repo, slug, &body);
    Ticket::write_state(repo, slug, TicketState::Review, None).unwrap();
}

#[test]
fn blocked_by_approve_refuses_and_names_unsatisfied_blocker() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // The blocker exists as a ticket with a recorded mission that hasn't
    // reached Complete.
    write_ticket(repo, "dep", "---\ntitle: dep\n---\nbody\n");
    Ticket::record_mission(repo, "dep", "m-dep").unwrap();
    write_mission_events(repo, "m-dep", false);

    park_for_review(repo, "blocked", &["dep"]);

    let err = backlog::cmd_ticket_approve(repo, "blocked", Some("m-blocked"), false).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("dep"),
        "message should name the blocker: {msg}"
    );
    assert!(
        msg.contains("not Complete"),
        "message should explain why: {msg}"
    );
    // Refused: ticket stays Review, nothing queued.
    assert_eq!(Ticket::read_state(repo, "blocked"), TicketState::Review);
    assert!(queue::list(repo).is_empty());
}

#[test]
fn blocked_by_approve_force_overrides_unsatisfied_blocker() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    write_ticket(repo, "dep", "---\ntitle: dep\n---\nbody\n");
    Ticket::record_mission(repo, "dep", "m-dep").unwrap();
    write_mission_events(repo, "m-dep", false);

    park_for_review(repo, "blocked", &["dep"]);

    let code = backlog::cmd_ticket_approve(repo, "blocked", Some("m-blocked"), true).unwrap();
    assert_eq!(code, 0);
    assert_eq!(Ticket::read_state(repo, "blocked"), TicketState::Queued);
    let queued = queue::list(repo);
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0].mission_id, "m-blocked");
}

#[test]
fn blocked_by_approve_succeeds_without_force_once_blocker_complete() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    write_ticket(repo, "dep", "---\ntitle: dep\n---\nbody\n");
    Ticket::record_mission(repo, "dep", "m-dep").unwrap();
    write_mission_events(repo, "m-dep", true);

    park_for_review(repo, "blocked", &["dep"]);

    let code = backlog::cmd_ticket_approve(repo, "blocked", Some("m-blocked"), false).unwrap();
    assert_eq!(code, 0);
    assert_eq!(Ticket::read_state(repo, "blocked"), TicketState::Queued);
}

#[test]
fn blocked_by_approve_refuses_cycle_with_path_even_with_force() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // a <-> b cycle: park "a" for review with blocked-by b, and b blocked-by a.
    park_for_review(repo, "a", &["b"]);
    write_ticket(repo, "b", "---\nblocked-by: [a]\n---\nbody\n");

    for force in [false, true] {
        let err = backlog::cmd_ticket_approve(repo, "a", Some("m-a"), force).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("blocked-by cycle: a -> b -> a"),
            "message should include the cycle path: {msg}"
        );
    }
    // Refused in both cases: ticket stays Review, nothing queued.
    assert_eq!(Ticket::read_state(repo, "a"), TicketState::Review);
    assert!(queue::list(repo).is_empty());
}

// ---------------------------------------------------------------------------
// work dispatcher — work-time blocker re-check (`work_skips_failed_blocker`)
// ---------------------------------------------------------------------------

#[test]
fn work_skips_failed_blocker() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // Batch-approval queued both "dep" and "blocked" before "dep"'s mission
    // ran; by the time `work` gets to "blocked", "dep" has since failed.
    write_ticket(repo, "dep", "---\ntitle: dep\n---\nbody\n");
    Ticket::record_mission(repo, "dep", "m-dep").unwrap();
    Ticket::write_state(repo, "dep", TicketState::Failed, None).unwrap();

    write_ticket(
        repo,
        "blocked",
        "---\ntitle: blocked\nblocked-by: [dep]\n---\nbody\n",
    );
    Ticket::write_state(repo, "blocked", TicketState::Queued, None).unwrap();

    let skip = work_skip_for_failed_blocker(repo, "blocked").unwrap();
    assert_eq!(skip, Some("dep".to_string()));
}

#[test]
fn work_does_not_skip_when_blocker_merely_incomplete() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();

    // "dep" is still Running (not yet Complete, but not Failed either) — the
    // required behavior only skips on a failed blocker, never on a merely
    // not-yet-complete one.
    write_ticket(repo, "dep", "---\ntitle: dep\n---\nbody\n");
    Ticket::record_mission(repo, "dep", "m-dep").unwrap();
    Ticket::write_state(repo, "dep", TicketState::Running, None).unwrap();

    write_ticket(
        repo,
        "blocked",
        "---\ntitle: blocked\nblocked-by: [dep]\n---\nbody\n",
    );
    Ticket::write_state(repo, "blocked", TicketState::Queued, None).unwrap();

    let skip = work_skip_for_failed_blocker(repo, "blocked").unwrap();
    assert_eq!(skip, None);
}

// ---------------------------------------------------------------------------
// Committed ticket lifecycle state (design ticket-state-frontmatter): the
// ready/list surfaces resolve the frontmatter `state:` key with precedence
// over the `.status` sidecar cache.
// ---------------------------------------------------------------------------

#[test]
fn ticket_state_frontmatter_parses_migrate_state_command() {
    let cli = Cli::try_parse_from(["kranz", "ticket", "migrate-state"]).unwrap();
    match cli.command {
        Command::Ticket {
            command: TicketCommand::MigrateState { yes },
        } => assert!(!yes, "dry-run is the default"),
        other => panic!("unexpected: {other:?}"),
    }
    let cli = Cli::try_parse_from(["kranz", "ticket", "migrate-state", "--yes"]).unwrap();
    match cli.command {
        Command::Ticket {
            command: TicketCommand::MigrateState { yes },
        } => assert!(yes),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn ticket_state_frontmatter_ready_excludes_terminal_frontmatter_state() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_ticket(repo, "open-one", "---\ntitle: Open\n---\n## Goal\ndo it\n");
    write_ticket(
        repo,
        "closed-one",
        "---\ntitle: Closed\nstate: superseded\n---\n## Goal\ndone elsewhere\n",
    );
    // Even with a stale REVIEW cache (or, on a fresh clone, none at all) the
    // committed terminal frontmatter keeps the ticket out of the ready path.
    Ticket::write_state(repo, "closed-one", TicketState::Review, None).unwrap();

    let out = backlog::cmd_ticket_ready(repo, false);
    assert!(out.contains("open-one"), "out:\n{out}");
    assert!(!out.contains("closed-one"), "out:\n{out}");
    assert!(!out.contains("SUPERSEDED"), "out:\n{out}");
}

#[test]
fn ticket_state_frontmatter_list_renders_lifecycle_labels() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_ticket(
        repo,
        "superseded-one",
        "---\ntitle: Sup\nstate: superseded\n---\n## Goal\nx\n",
    );
    write_ticket(
        repo,
        "wontfix-one",
        "---\ntitle: Wont\nstate: wontfix\n---\n## Goal\nx\n",
    );
    // No sidecars at all (fresh clone): the labels come from the committed
    // frontmatter alone, and the tickets stay listed (terminal ≠ hidden).
    let out = backlog::cmd_ticket_list(repo);
    assert!(out.contains("SUPERSEDED"), "out:\n{out}");
    assert!(out.contains("WONTFIX"), "out:\n{out}");
}
