//! Integration tests for the ticket parser + status files (M2.75 backlog) and
//! the per-repo priority execution queue.

use kranz_engine::queue::{self, QueueEntry};
use kranz_engine::ticket::{Schedule, Ticket, TicketState};
use std::fs;
use std::path::Path;

// ---------------------------------------------------------------------------
// Ticket parsing
// ---------------------------------------------------------------------------

const FULL: &str = "\
---
title: Rate-limit the notes API
priority: 1
repo-refs: [src/api/, tokenstore.py]
schedule: nightly
maxBudgetUsd: 15
---

## Goal
Add per-token rate limiting to the notes API.

## Context
Abuse from a handful of tokens is degrading the service.
Prior art lives in the gateway.

## Scoping answers
- Test command: python3 -m unittest discover
- Conventions: stdlib only, no new deps
- Out of scope: auth changes, storage format

## Acceptance hints
- requests beyond N/min per token get 429
- existing endpoints unaffected (all current tests pass)
";

#[test]
fn parses_full_frontmatter_and_all_sections() {
    let t = Ticket::parse("rate-limit", FULL).unwrap();
    assert_eq!(t.slug, "rate-limit");
    assert_eq!(t.title, "Rate-limit the notes API");
    assert_eq!(t.priority, 1);
    assert_eq!(t.repo_refs, vec!["src/api/", "tokenstore.py"]);
    assert_eq!(t.schedule, Schedule::Nightly);
    assert_eq!(t.max_budget_usd, Some(15.0));
    assert_eq!(t.goal, "Add per-token rate limiting to the notes API.");
    assert!(t.context.contains("degrading the service"));
    assert!(t.context.contains("Prior art lives in the gateway."));
    assert_eq!(
        t.scoping_answers,
        vec![
            "Test command: python3 -m unittest discover",
            "Conventions: stdlib only, no new deps",
            "Out of scope: auth changes, storage format",
        ]
    );
    assert_eq!(
        t.acceptance_hints,
        vec![
            "requests beyond N/min per token get 429",
            "existing endpoints unaffected (all current tests pass)",
        ]
    );
    // raw_body carries the whole body after the frontmatter fence.
    assert!(t.raw_body.contains("## Goal"));
    assert!(!t.raw_body.contains("title:"));
}

#[test]
fn missing_frontmatter_uses_defaults_and_heading_title() {
    let md = "\
# Improve caching

Cache the expensive lookups.

## Acceptance hints
- second call is served from cache
";
    let t = Ticket::parse("caching", md).unwrap();
    // No frontmatter: all defaults.
    assert_eq!(t.priority, 2);
    assert_eq!(t.schedule, Schedule::Once);
    assert!(t.repo_refs.is_empty());
    assert_eq!(t.max_budget_usd, None);
    // Title falls back to the first heading.
    assert_eq!(t.title, "Improve caching");
    // Preamble (before the first `##`, excluding the `#` title) becomes the goal.
    assert_eq!(t.goal, "Cache the expensive lookups.");
    assert_eq!(t.acceptance_hints, vec!["second call is served from cache"]);
}

#[test]
fn title_falls_back_to_slug_when_no_frontmatter_or_heading() {
    let md = "Just a body with no heading and no frontmatter.\n";
    let t = Ticket::parse("orphan-slug", md).unwrap();
    assert_eq!(t.title, "orphan-slug");
    assert_eq!(t.goal, "Just a body with no heading and no frontmatter.");
}

#[test]
fn bracketed_list_values_parse_and_strip_quotes() {
    let md = "\
---
repo-refs: [\"a/b.rs\", 'c/d.rs', e/f.rs]
---
body
";
    let t = Ticket::parse("lists", md).unwrap();
    assert_eq!(t.repo_refs, vec!["a/b.rs", "c/d.rs", "e/f.rs"]);
}

#[test]
fn unknown_schedule_maps_to_once() {
    let md = "\
---
schedule: fortnightly
---
";
    let t = Ticket::parse("odd", md).unwrap();
    assert_eq!(t.schedule, Schedule::Once);
}

#[test]
fn schedule_is_case_insensitive() {
    let md = "---\nschedule: WEEKLY\n---\n";
    let t = Ticket::parse("sch", md).unwrap();
    assert_eq!(t.schedule, Schedule::Weekly);
}

#[test]
fn unclosed_frontmatter_is_a_config_error() {
    let md = "---\ntitle: broken\nno closing fence\n";
    let err = Ticket::parse("broken", md).unwrap_err();
    assert!(matches!(err, kranz_engine::error::EngineError::Config(_)));
}

#[test]
fn unknown_frontmatter_keys_are_ignored() {
    let md = "---\ntitle: keep\nmystery: value\n---\nbody\n";
    let t = Ticket::parse("keep", md).unwrap();
    assert_eq!(t.title, "keep");
}

#[test]
fn mission_goal_folds_sections() {
    let t = Ticket::parse("rate-limit", FULL).unwrap();
    let g = t.mission_goal();
    // Goal leads.
    assert!(g.starts_with("Add per-token rate limiting to the notes API."));
    // Scoping answers folded in as bullets.
    assert!(g.contains("## Scoping answers"));
    assert!(g.contains("- Test command: python3 -m unittest discover"));
    // Acceptance hints folded in.
    assert!(g.contains("## Acceptance hints"));
    assert!(g.contains("- requests beyond N/min per token get 429"));
    // Context folded in.
    assert!(g.contains("## Context"));
    assert!(g.contains("degrading the service"));
}

#[test]
fn mission_goal_uses_title_when_goal_empty() {
    let md = "---\ntitle: Only a title\n---\n";
    let t = Ticket::parse("bare", md).unwrap();
    assert_eq!(t.mission_goal().trim(), "Only a title");
}

// ---------------------------------------------------------------------------
// Ticket status files
// ---------------------------------------------------------------------------

#[test]
fn state_read_write_round_trip() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();

    // Default when no status file exists.
    assert_eq!(Ticket::read_state(root, "x"), TicketState::New);

    Ticket::write_state(root, "x", TicketState::Queued, Some("approved".into())).unwrap();
    assert_eq!(Ticket::read_state(root, "x"), TicketState::Queued);

    // The status file is a sibling of the (would-be) markdown, and JSON-shaped.
    let status = Ticket::tickets_dir(root).join("x.status");
    let text = fs::read_to_string(&status).unwrap();
    assert!(text.contains("\"state\""));
    assert!(text.contains("queued"));
    assert!(text.contains("approved"));

    // Overwriting drops the note when None is passed.
    Ticket::write_state(root, "x", TicketState::Done, None).unwrap();
    assert_eq!(Ticket::read_state(root, "x"), TicketState::Done);
    let text = fs::read_to_string(&status).unwrap();
    assert!(!text.contains("note"));
}

#[test]
fn append_needs_context_mutates_md_and_status() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();
    let md = dir.join("feat.md");
    fs::write(&md, "---\ntitle: Feature\n---\n\n## Goal\nDo the thing.\n").unwrap();

    let questions = vec![
        "Which auth backend?".to_string(),
        "Is Postgres available?".to_string(),
    ];
    Ticket::append_needs_context(root, "feat", &questions).unwrap();

    // The markdown gained the questions block.
    let after = fs::read_to_string(&md).unwrap();
    assert!(after.contains("## Needs context (from orchestrator)"));
    assert!(after.contains("- Which auth backend?"));
    assert!(after.contains("- Is Postgres available?"));
    // Original content is preserved.
    assert!(after.contains("## Goal"));
    assert!(after.contains("Do the thing."));

    // The appended block re-parses as an ordinary (unknown) section without error.
    let reparsed = Ticket::load(&md).unwrap();
    assert_eq!(reparsed.goal, "Do the thing.");

    // State flipped to NeedsContext.
    assert_eq!(Ticket::read_state(root, "feat"), TicketState::NeedsContext);
}

// ---------------------------------------------------------------------------
// Ticket listing
// ---------------------------------------------------------------------------

fn write_ticket(dir: &Path, slug: &str, body: &str) {
    fs::write(dir.join(format!("{slug}.md")), body).unwrap();
}

#[test]
fn list_sorts_by_priority_then_slug_and_skips_malformed() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();

    write_ticket(&dir, "zebra", "---\npriority: 3\n---\nlow prio\n");
    write_ticket(&dir, "alpha", "---\npriority: 1\n---\nhigh prio\n");
    write_ticket(&dir, "beta", "---\npriority: 1\n---\nalso high\n");
    // Malformed: unclosed frontmatter — must be skipped, not fatal.
    write_ticket(&dir, "broken", "---\ntitle: no fence\nstill going\n");
    // A non-.md file is ignored entirely.
    fs::write(dir.join("notes.txt"), "not a ticket").unwrap();
    // The status sibling of a ticket is not itself a ticket.
    fs::write(dir.join("alpha.status"), "{\"state\":\"new\"}").unwrap();

    let tickets = Ticket::list(root);
    let slugs: Vec<&str> = tickets.iter().map(|t| t.slug.as_str()).collect();
    // (priority, slug): alpha(1), beta(1), zebra(3). broken skipped.
    assert_eq!(slugs, vec!["alpha", "beta", "zebra"]);
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

fn entry(mission: &str, priority: u8) -> QueueEntry {
    QueueEntry {
        mission_id: mission.to_string(),
        ticket_slug: None,
        priority,
        seq: 0,
    }
}

#[test]
fn enqueue_orders_by_priority_then_insertion() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();

    // Insert out of priority order; equal priorities keep insertion order.
    queue::enqueue(root, entry("m-low", 3)).unwrap();
    queue::enqueue(root, entry("m-high-a", 1)).unwrap();
    queue::enqueue(root, entry("m-mid", 2)).unwrap();
    queue::enqueue(root, entry("m-high-b", 1)).unwrap();

    let ids: Vec<String> = queue::list(root).into_iter().map(|e| e.mission_id).collect();
    assert_eq!(ids, vec!["m-high-a", "m-high-b", "m-mid", "m-low"]);

    // Seq is a global monotonic counter reflecting insertion order across all
    // priorities: m-low, m-high-a, m-mid, m-high-b were inserted in that order.
    let by_id = |id: &str| {
        queue::list(root)
            .into_iter()
            .find(|e| e.mission_id == id)
            .unwrap()
            .seq
    };
    assert!(by_id("m-low") < by_id("m-high-a"));
    assert!(by_id("m-high-a") < by_id("m-mid"));
    assert!(by_id("m-mid") < by_id("m-high-b"));
    // Within equal priority 1, the earlier-inserted m-high-a still leads
    // m-high-b in the final order (seq breaks the tie).
    assert!(by_id("m-high-a") < by_id("m-high-b"));
}

#[test]
fn peek_contains_and_remove() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();

    assert!(queue::peek(root).is_none());

    queue::enqueue(root, entry("m1", 2)).unwrap();
    queue::enqueue(root, entry("m2", 1)).unwrap();

    // peek returns the highest-priority entry.
    assert_eq!(queue::peek(root).unwrap().mission_id, "m2");

    assert!(queue::contains(root, "m1"));
    assert!(queue::contains(root, "m2"));
    assert!(!queue::contains(root, "nope"));

    assert!(queue::remove(root, "m2"));
    assert!(!queue::contains(root, "m2"));
    // Removing again is a no-op returning false.
    assert!(!queue::remove(root, "m2"));

    assert_eq!(queue::peek(root).unwrap().mission_id, "m1");
}

#[test]
fn enqueue_is_idempotent_per_mission() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();

    let first = queue::enqueue(root, entry("dup", 2)).unwrap();
    let second = queue::enqueue(root, entry("dup", 2)).unwrap();
    // Same seq returned; only one entry present.
    assert_eq!(first.seq, second.seq);
    assert_eq!(queue::list(root).len(), 1);
}

#[test]
fn is_repo_busy_detects_a_live_lock_and_ignores_a_dead_one() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let missions = root.join(".kranz").join("missions");

    // No missions dir yet: not busy.
    assert_eq!(queue::is_repo_busy(root), None);

    // A mission whose lock records a DEAD pid (i32::MAX is not a live process).
    let dead_dir = missions.join("dead-mission");
    fs::create_dir_all(&dead_dir).unwrap();
    fs::write(
        dead_dir.join("events.jsonl.lock"),
        i32::MAX.to_string(),
    )
    .unwrap();
    assert_eq!(
        queue::is_repo_busy(root),
        None,
        "a lock held by a dead pid must not count as busy"
    );

    // A mission whose lock records THIS process (guaranteed alive).
    let live_dir = missions.join("live-mission");
    fs::create_dir_all(&live_dir).unwrap();
    fs::write(
        live_dir.join("events.jsonl.lock"),
        std::process::id().to_string(),
    )
    .unwrap();
    assert_eq!(
        queue::is_repo_busy(root).as_deref(),
        Some("live-mission"),
        "a lock held by a live pid identifies the running mission"
    );
}
