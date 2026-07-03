//! Integration tests for `kranz exec` — fully headless missions for CI.
//!
//! No real `claude` binary is ever spawned: `cmd_exec` needs a backend, so it
//! is exercised only through its pure helpers. Parsing is checked with clap
//! `try_parse_from`; the plan-file parse path is checked by feeding
//! ticket-shaped markdown to `parse_mission_markdown` and asserting the folded
//! mission goal carries the goal / acceptance criteria; the outcome→exit-code
//! mapping is checked directly on `exit_code_for`.

use clap::Parser;
use kranz_cli::cli::{Cli, Command};
use kranz_cli::exec::{exit_code_for, parse_mission_markdown, EXIT_UNDERSPECIFIED};
use kranz_engine::types::MissionStatus;

// ---------------------------------------------------------------------------
// clap parsing — the exec subcommand + its flags
// ---------------------------------------------------------------------------

#[test]
fn parses_exec_with_short_file_flag() {
    let cli = Cli::try_parse_from(["kranz", "exec", "-f", "mission.md"]).unwrap();
    match cli.command {
        Command::Exec { file, yes, max_cycles } => {
            assert_eq!(file.to_str(), Some("mission.md"));
            assert!(!yes);
            assert_eq!(max_cycles, None);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_exec_with_long_file_flag_and_all_options() {
    let cli = Cli::try_parse_from([
        "kranz",
        "exec",
        "--file",
        "plans/ci.md",
        "--yes",
        "--max-cycles",
        "5",
    ])
    .unwrap();
    match cli.command {
        Command::Exec { file, yes, max_cycles } => {
            assert_eq!(file.to_str(), Some("plans/ci.md"));
            assert!(yes);
            assert_eq!(max_cycles, Some(5));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn exec_requires_a_file() {
    // -f/--file is mandatory: `kranz exec` alone is a parse error.
    assert!(Cli::try_parse_from(["kranz", "exec"]).is_err());
}

#[test]
fn exec_honors_global_repo_and_danger_flags() {
    let cli = Cli::try_parse_from([
        "kranz",
        "--repo",
        "/tmp/target",
        "--dangerously-allow-all",
        "exec",
        "-f",
        "mission.md",
    ])
    .unwrap();
    assert_eq!(cli.repo.as_deref(), Some(std::path::Path::new("/tmp/target")));
    assert!(cli.dangerously_allow_all);
    assert!(matches!(cli.command, Command::Exec { .. }));
}

#[test]
fn exec_rejects_non_numeric_max_cycles() {
    assert!(
        Cli::try_parse_from(["kranz", "exec", "-f", "mission.md", "--max-cycles", "lots"]).is_err()
    );
}

// ---------------------------------------------------------------------------
// plan-file parse path — ticket-shaped markdown → folded mission goal
// ---------------------------------------------------------------------------

const SAMPLE_MISSION: &str = "\
---
title: Rate-limit the public API
priority: 1
---

## Goal
Cap requests per API token so a single client cannot exhaust the pool.

## Context
The gateway currently forwards every request unthrottled.

## Scoping answers
- Use a token-bucket, 100 requests/minute per token.
- Return HTTP 429 with a Retry-After header when the bucket is empty.

## Acceptance hints
- A burst above the cap returns 429.
- Under the cap, requests pass through untouched.
";

#[test]
fn parses_ticket_shaped_markdown_and_extracts_goal() {
    let ticket = parse_mission_markdown("rate-limit", SAMPLE_MISSION).unwrap();

    assert_eq!(ticket.slug, "rate-limit");
    assert_eq!(ticket.title, "Rate-limit the public API");
    assert_eq!(ticket.priority, 1);
    assert!(ticket
        .goal
        .contains("Cap requests per API token"));
    assert_eq!(ticket.scoping_answers.len(), 2);
    assert_eq!(ticket.acceptance_hints.len(), 2);
}

#[test]
fn mission_goal_folds_the_whole_ticket_for_the_seed_turn() {
    // The folded mission_goal() is what exec seeds the orchestrator with; it
    // must carry the goal plus the scoping answers and acceptance hints so a
    // headless plan file is self-sufficient.
    let ticket = parse_mission_markdown("rate-limit", SAMPLE_MISSION).unwrap();
    let goal = ticket.mission_goal();

    assert!(goal.contains("Cap requests per API token"));
    assert!(goal.contains("token-bucket, 100 requests/minute"));
    assert!(goal.contains("returns 429"));
    assert!(goal.contains("## Scoping answers"));
    assert!(goal.contains("## Acceptance hints"));
    assert!(goal.contains("## Context"));
}

#[test]
fn goalless_preamble_still_parses_as_the_goal() {
    // A bare plan file with no `## Goal` heading: the preamble text is the goal.
    let md = "Add a health-check endpoint at /healthz that returns 200 OK.\n";
    let ticket = parse_mission_markdown("healthz", md).unwrap();
    assert!(ticket.goal.contains("health-check endpoint"));
    assert!(ticket.mission_goal().contains("/healthz"));
}

#[test]
fn unclosed_frontmatter_is_a_parse_error() {
    // A malformed plan file surfaces as an error (exec fails before any spend),
    // not a silently-empty mission.
    let md = "---\ntitle: broken\n\n## Goal\nnever closed the frontmatter\n";
    assert!(parse_mission_markdown("broken", md).is_err());
}

// ---------------------------------------------------------------------------
// outcome → exit-code mapping (pure fn)
// ---------------------------------------------------------------------------

#[test]
fn exit_code_mapping_matches_the_ci_contract() {
    assert_eq!(exit_code_for(MissionStatus::Complete), 0);
    assert_eq!(exit_code_for(MissionStatus::Failed), 1);
    assert_eq!(exit_code_for(MissionStatus::Blocked), 2);
    // The underspecified (NotReady) case is a distinct code handled before the
    // run and never routed through exit_code_for.
    assert_eq!(EXIT_UNDERSPECIFIED, 3);
}

#[test]
fn non_terminal_statuses_map_to_failure() {
    // run() only returns Complete/Failed/Blocked; anything else is defensive.
    for status in [
        MissionStatus::Planning,
        MissionStatus::Running,
        MissionStatus::Paused,
        MissionStatus::Validating,
        MissionStatus::Abandoned,
    ] {
        assert_eq!(exit_code_for(status), 1, "status {status:?}");
    }
}
