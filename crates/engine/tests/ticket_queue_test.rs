//! Integration tests for the ticket parser + status files (M2.75 backlog) and
//! the per-repo priority execution queue.

use kranz_engine::deps;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::queue::{self, QueueEntry};
use kranz_engine::ticket::{parse_task_class_from_goal, Schedule, Ticket, TicketState};
use kranz_engine::types::MissionConfig;
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
    // No `blocked-by` key: tickets predating the field parse unchanged.
    assert!(t.blocked_by.is_empty());
}

#[test]
fn task_class_routing_parses_task_class_from_frontmatter() {
    let md = "\
---
title: Bump a dependency
task-class: execution-class
---

## Goal
Bump the dependency.
";
    let t = Ticket::parse("bump-dep", md).unwrap();
    assert_eq!(t.task_class, Some("execution-class".to_string()));
}

#[test]
fn task_class_routing_absent_key_yields_none() {
    let t = Ticket::parse("rate-limit", FULL).unwrap();
    assert_eq!(t.task_class, None);
}

// ---------------------------------------------------------------------------
// task-class travels through the folded mission_goal (f-1-2 integration seam:
// `kranz exec` only ever sees the folded goal string, not the `Ticket`, so
// this round-trip is what lets `MissionEngine::create` recover the class).
// ---------------------------------------------------------------------------

#[test]
fn mission_goal_folds_in_the_task_class_when_set() {
    let md = "\
---
title: Bump a dependency
task-class: execution-class
---

## Goal
Bump the dependency.
";
    let t = Ticket::parse("bump-dep", md).unwrap();
    let goal = t.mission_goal();
    assert_eq!(
        parse_task_class_from_goal(&goal),
        Some("execution-class".to_string())
    );
}

#[test]
fn mission_goal_omits_task_class_heading_when_unset() {
    let t = Ticket::parse("rate-limit", FULL).unwrap();
    let goal = t.mission_goal();
    assert_eq!(parse_task_class_from_goal(&goal), None);
    assert!(!goal.contains("## Task class"));
}

#[test]
fn parse_task_class_from_goal_ignores_unrelated_text() {
    assert_eq!(parse_task_class_from_goal("just a plain goal"), None);
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

#[test]
fn append_needs_context_truncates_long_question() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();
    let md = dir.join("feat.md");
    fs::write(&md, "---\ntitle: Feature\n---\n\n## Goal\nDo the thing.\n").unwrap();

    let long_question = "a".repeat(5000);
    Ticket::append_needs_context(root, "feat", std::slice::from_ref(&long_question)).unwrap();

    let after = fs::read_to_string(&md).unwrap();
    assert!(!after.contains(&long_question));
    let expected = format!("- {} … (truncated)", "a".repeat(500));
    assert!(after.contains(&expected));
}

#[test]
fn append_needs_context_caps_total_count() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();
    let md = dir.join("feat.md");
    fs::write(&md, "---\ntitle: Feature\n---\n\n## Goal\nDo the thing.\n").unwrap();

    let questions: Vec<String> = (0..30).map(|i| format!("Question {i}")).collect();
    Ticket::append_needs_context(root, "feat", &questions).unwrap();

    let after = fs::read_to_string(&md).unwrap();
    let heading_idx = after.find("## Needs context (from orchestrator)").unwrap();
    let block = &after[heading_idx..];
    let bullet_lines: Vec<&str> = block.lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(bullet_lines.len(), 21);
    for (i, line) in bullet_lines.iter().enumerate().take(20) {
        assert_eq!(*line, format!("- Question {i}"));
    }
    assert_eq!(bullet_lines[20], "- … (10 more omitted)");
}

#[test]
fn append_needs_context_leaves_short_questions_unchanged() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();
    let md = dir.join("feat.md");
    fs::write(&md, "---\ntitle: Feature\n---\n\n## Goal\nDo the thing.\n").unwrap();

    let questions = vec![
        "Which auth backend?".to_string(),
        "Is Postgres available?".to_string(),
        "What about rate limits?".to_string(),
    ];
    Ticket::append_needs_context(root, "feat", &questions).unwrap();

    let after = fs::read_to_string(&md).unwrap();
    assert!(after.contains("- Which auth backend?\n"));
    assert!(after.contains("- Is Postgres available?\n"));
    assert!(after.contains("- What about rate limits?\n"));
    assert!(!after.contains("truncated"));
    assert!(!after.contains("omitted"));
}

#[test]
fn append_needs_context_truncates_multibyte_on_char_boundary() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();
    let md = dir.join("feat.md");
    fs::write(&md, "---\ntitle: Feature\n---\n\n## Goal\nDo the thing.\n").unwrap();

    // Multibyte characters (each 3 bytes in UTF-8), well over 500 chars.
    let long_question = "な".repeat(600);
    Ticket::append_needs_context(root, "feat", &[long_question]).unwrap();

    let after = fs::read_to_string(&md).unwrap();
    let expected = format!("- {} … (truncated)", "な".repeat(500));
    assert!(after.contains(&expected));
}

// ---------------------------------------------------------------------------
// Wrong-plan escalation state (draft-stage "this plan is likely wrong")
// ---------------------------------------------------------------------------

#[test]
fn append_wrong_plan_mutates_md_and_status() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();
    let md = dir.join("feat.md");
    fs::write(&md, "---\ntitle: Feature\n---\n\n## Goal\nDo the thing.\n").unwrap();

    let reason = "The goal assumes a Postgres migration, but the store is SQLite.";
    Ticket::append_wrong_plan(root, "feat", reason).unwrap();

    // The markdown gained the escalation block, original content preserved.
    let after = fs::read_to_string(&md).unwrap();
    assert!(after.contains("## Wrong plan (from orchestrator)"));
    assert!(after.contains(reason));
    assert!(after.contains("## Goal"));
    assert!(!after.contains("## Needs context (from orchestrator)"));

    // The appended block re-parses as an ordinary (unknown) section.
    let reparsed = Ticket::load(&md).unwrap();
    assert_eq!(reparsed.goal, "Do the thing.");

    // State flipped to WrongPlan and round-trips on the kebab-case wire.
    assert_eq!(Ticket::read_state(root, "feat"), TicketState::WrongPlan);
    let status = fs::read_to_string(dir.join("feat.status")).unwrap();
    assert!(status.contains("\"wrong-plan\""), "kebab wire: {status}");
    assert!(
        status.contains(&format!("WRONG-PLAN: {reason}")),
        "the note carries the prefixed reason: {status}"
    );
}

#[test]
fn append_wrong_plan_truncates_long_reason() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();
    let md = dir.join("feat.md");
    fs::write(&md, "---\ntitle: Feature\n---\n\n## Goal\nDo the thing.\n").unwrap();

    let long_reason = "a".repeat(5000);
    Ticket::append_wrong_plan(root, "feat", &long_reason).unwrap();

    let expected = format!("{} … (truncated)", "a".repeat(500));
    let after = fs::read_to_string(&md).unwrap();
    assert!(!after.contains(&long_reason));
    assert!(after.contains(&expected));
    // The .status note is bounded too.
    let status = fs::read_to_string(dir.join("feat.status")).unwrap();
    assert!(status.contains("WRONG-PLAN: "));
    assert!(!status.contains(&long_reason));
}

#[test]
fn wrong_plan_ticket_is_not_queueable() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("wp.md"),
        "---\ntitle: Feature\n---\n\n## Goal\nDo it.\n",
    )
    .unwrap();
    Ticket::write_state(root, "wp", TicketState::WrongPlan, None).unwrap();

    // The approve gate is a Review/Parked whitelist: a parked wrong-plan
    // ticket names itself in the refusal, exactly like NeedsContext would.
    let err = deps::approve_ticket(root, "wp", Some("m-x"), false).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("WRONG-PLAN"), "names the state: {msg}");
    assert!(
        msg.contains("only a REVIEW or PARKED ticket"),
        "refuses queueing: {msg}"
    );
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

    let ids: Vec<String> = queue::list(root)
        .into_iter()
        .map(|e| e.mission_id)
        .collect();
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
fn is_repo_busy_detects_a_live_lock() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let missions = root.join(".kranz").join("missions");

    // No missions dir yet: not busy.
    assert_eq!(queue::is_repo_busy(root), None);

    // A lock recording THIS process (guaranteed alive) is busy on every
    // platform.
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

/// Dead-pid detection is a unix capability today (`libc::kill(pid, 0)`). On
/// Windows `is_repo_busy` is conservative — any lock counts as busy — until a
/// Windows liveness probe lands (tracked as an M4 follow-up), matching the
/// event-log stale-lock posture where Windows needs `--force-lock`.
#[cfg(unix)]
#[test]
fn is_repo_busy_ignores_a_dead_lock() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dead_dir = root.join(".kranz").join("missions").join("dead-mission");
    fs::create_dir_all(&dead_dir).unwrap();
    fs::write(dead_dir.join("events.jsonl.lock"), i32::MAX.to_string()).unwrap();
    assert_eq!(
        queue::is_repo_busy(root),
        None,
        "a lock held by a dead pid must not count as busy (unix)"
    );
}

/// FINDING B REGRESSION: `is_repo_busy` must understand the CURRENT
/// multi-line lock format that `EventLog::acquire` actually writes. A
/// divergent local parser once parsed the ENTIRE file as one integer, so any
/// multi-line lock read as "unparseable ⇒ busy" — after a SIGKILL'd engine
/// (dead pid, multi-line lock) `kranz work` reported the repo busy forever.
#[cfg(unix)]
#[test]
fn is_repo_busy_ignores_a_dead_multiline_lock() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dead_dir = root.join(".kranz").join("missions").join("dead-mission");
    fs::create_dir_all(&dead_dir).unwrap();
    // The full three-line format: pid, acquire epoch secs, identity token.
    fs::write(
        dead_dir.join("events.jsonl.lock"),
        format!("{}\n{}\nsome-boot-id:12345\n", i32::MAX, 1_700_000_000u64),
    )
    .unwrap();
    assert_eq!(
        queue::is_repo_busy(root),
        None,
        "a multi-line lock with a dead holder must not count as busy"
    );
}

/// The inverse guard for the shared parser: a multi-line lock held by a LIVE
/// pid (ours) still counts as busy on every platform.
#[test]
fn is_repo_busy_detects_a_live_multiline_lock() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let live_dir = root.join(".kranz").join("missions").join("live-mission");
    fs::create_dir_all(&live_dir).unwrap();
    fs::write(
        live_dir.join("events.jsonl.lock"),
        format!("{}\n{}\n", std::process::id(), 1_700_000_000u64),
    )
    .unwrap();
    assert_eq!(
        queue::is_repo_busy(root).as_deref(),
        Some("live-mission"),
        "a multi-line lock held by a live pid identifies the running mission"
    );
}

// --- review P3: slugs are file stems, never paths ---------------------------

#[test]
fn traversal_slugs_are_rejected_before_any_filesystem_touch() {
    let tmp = tempfile::tempdir().unwrap();
    for bad in ["../evil", "a/b", "a\\b", "..", ".hidden", "", "x/../../y"] {
        assert!(!Ticket::valid_slug(bad), "must reject {bad:?}");
        assert!(Ticket::ensure_valid_slug(bad).is_err());
        assert!(
            Ticket::write_state(tmp.path(), bad, TicketState::New, None).is_err(),
            "write_state must refuse {bad:?}"
        );
        // read_state is total: invalid slugs read as New without touching disk.
        assert_eq!(Ticket::read_state(tmp.path(), bad), TicketState::New);
    }
    assert!(!tmp.path().join("..").join("evil.status").exists());
}

#[test]
fn ordinary_slugs_still_work() {
    for good in ["rate-limit", "fix_f1", "a1", "v0.1.0-notes"] {
        assert!(Ticket::valid_slug(good), "must accept {good:?}");
    }
}

// ---------------------------------------------------------------------------
// blocked-by dependency primitive
// ---------------------------------------------------------------------------

/// Writes a minimal events.jsonl for `mission_id` whose fold reaches
/// `MissionStatus::Complete` (MissionCreated then MissionCompleted), or stops
/// after MissionCreated when `complete` is false (leaves it Drafting).
fn write_mission_events(root: &Path, mission_id: &str, complete: bool) {
    let paths = kranz_engine::paths::MissionPaths::new(root, mission_id);
    let dir = paths.events_file().parent().unwrap().to_path_buf();
    fs::create_dir_all(&dir).unwrap();

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
    fs::write(paths.events_file(), body).unwrap();
}

#[test]
fn blocked_by_frontmatter_parses_into_list() {
    let md = "---\ntitle: needs review\nblocked-by: [a, b]\n---\nbody\n";
    let t = Ticket::parse("needs-review", md).unwrap();
    assert_eq!(t.blocked_by, vec!["a", "b"]);
}

#[test]
fn blocked_by_accepts_the_no_hyphen_key_alias() {
    let md = "---\nblockedby: [x]\n---\nbody\n";
    let t = Ticket::parse("aliased", md).unwrap();
    assert_eq!(t.blocked_by, vec!["x"]);
}

#[test]
fn blocked_by_unsatisfied_for_every_non_complete_or_missing_state() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();

    // slug's ticket lists three blockers: not-complete, no-mission, missing.
    write_ticket(
        &dir,
        "slug",
        "---\nblocked-by: [not-complete, no-mission, missing]\n---\nbody\n",
    );
    // A ticket file for the blockers that exist on disk (missing has none).
    write_ticket(&dir, "not-complete", "---\ntitle: nc\n---\nbody\n");
    write_ticket(&dir, "no-mission", "---\ntitle: nm\n---\nbody\n");

    // not-complete: recorded mission exists but hasn't reached Complete.
    Ticket::record_mission(root, "not-complete", "m-not-complete").unwrap();
    write_mission_events(root, "m-not-complete", false);

    // no-mission: no recorded mission at all.
    // missing: no ticket file at all.

    let unsatisfied = deps::unsatisfied_blockers(root, "slug").unwrap();
    assert_eq!(
        unsatisfied,
        vec!["not-complete", "no-mission", "missing"],
        "every non-Complete/missing blocker is unsatisfied"
    );

    // Now fold not-complete's mission to Complete and re-record the others.
    write_mission_events(root, "m-not-complete", true);
    Ticket::record_mission(root, "no-mission", "m-no-mission").unwrap();
    write_mission_events(root, "m-no-mission", true);
    write_ticket(&dir, "missing", "---\ntitle: now-exists\n---\nbody\n");
    Ticket::record_mission(root, "missing", "m-missing").unwrap();
    write_mission_events(root, "m-missing", true);

    let unsatisfied = deps::unsatisfied_blockers(root, "slug").unwrap();
    assert!(
        unsatisfied.is_empty(),
        "empty once every blocker's recorded mission is Complete, got {unsatisfied:?}"
    );
}

#[test]
fn blocked_by_cycle_detects_direct_and_transitive_cycles_none_for_acyclic() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();

    // Direct a<->b cycle.
    write_ticket(&dir, "a", "---\nblocked-by: [b]\n---\nbody\n");
    write_ticket(&dir, "b", "---\nblocked-by: [a]\n---\nbody\n");
    let cycle = deps::detect_cycle(root, "a").unwrap();
    assert_eq!(
        cycle,
        Some(vec!["a".to_string(), "b".to_string(), "a".to_string()])
    );

    // Longer a->b->c->a cycle.
    write_ticket(&dir, "a", "---\nblocked-by: [b]\n---\nbody\n");
    write_ticket(&dir, "b", "---\nblocked-by: [c]\n---\nbody\n");
    write_ticket(&dir, "c", "---\nblocked-by: [a]\n---\nbody\n");
    let cycle = deps::detect_cycle(root, "a").unwrap();
    assert_eq!(
        cycle,
        Some(vec![
            "a".to_string(),
            "b".to_string(),
            "c".to_string(),
            "a".to_string()
        ])
    );

    // Acyclic a->b->c chain: no cycle.
    write_ticket(&dir, "a", "---\nblocked-by: [b]\n---\nbody\n");
    write_ticket(&dir, "b", "---\nblocked-by: [c]\n---\nbody\n");
    write_ticket(&dir, "c", "---\ntitle: leaf\n---\nbody\n");
    let cycle = deps::detect_cycle(root, "a").unwrap();
    assert_eq!(cycle, None);
}

#[test]
fn is_blocked_false_when_no_blocked_by() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();

    write_ticket(&dir, "solo", "---\ntitle: solo\n---\nbody\n");

    assert!(!deps::is_blocked(root, "solo").unwrap());
}

#[test]
fn is_blocked_true_when_blocker_mission_absent_or_incomplete() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();

    // no-mission: dep exists but has no recorded mission at all.
    write_ticket(
        &dir,
        "waits-on-no-mission",
        "---\nblocked-by: [no-mission]\n---\nbody\n",
    );
    write_ticket(&dir, "no-mission", "---\ntitle: nm\n---\nbody\n");
    assert!(deps::is_blocked(root, "waits-on-no-mission").unwrap());

    // incomplete: dep's recorded mission never reached Complete.
    write_ticket(
        &dir,
        "waits-on-incomplete",
        "---\nblocked-by: [incomplete]\n---\nbody\n",
    );
    write_ticket(&dir, "incomplete", "---\ntitle: inc\n---\nbody\n");
    Ticket::record_mission(root, "incomplete", "m-incomplete").unwrap();
    write_mission_events(root, "m-incomplete", false);
    assert!(deps::is_blocked(root, "waits-on-incomplete").unwrap());
}

#[test]
fn is_blocked_false_when_blocker_mission_complete() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();
    let dir = Ticket::tickets_dir(root);
    fs::create_dir_all(&dir).unwrap();

    write_ticket(&dir, "child", "---\nblocked-by: [dep]\n---\nbody\n");
    write_ticket(&dir, "dep", "---\ntitle: dep\n---\nbody\n");
    Ticket::record_mission(root, "dep", "m-dep").unwrap();
    write_mission_events(root, "m-dep", true);

    assert!(!deps::is_blocked(root, "child").unwrap());
}

// --- review P1: queue concurrency + crash-safe claims -----------------------

#[test]
fn concurrent_enqueues_get_unique_seqs_and_all_land() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let mut handles = Vec::new();
    for t in 0..8 {
        let root = root.clone();
        handles.push(std::thread::spawn(move || {
            for i in 0..8 {
                queue::enqueue(
                    &root,
                    QueueEntry {
                        mission_id: format!("m-{t}-{i}"),
                        ticket_slug: None,
                        priority: 2,
                        seq: 0,
                    },
                )
                .expect("enqueue under contention");
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let entries = queue::list(&root);
    assert_eq!(entries.len(), 64, "every concurrent enqueue landed");
    let mut seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(
        seqs.len(),
        64,
        "no duplicate sequence numbers under contention"
    );
}

#[test]
fn claim_lifecycle_finish_release_and_dead_recovery() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    for id in ["m-a", "m-b"] {
        queue::enqueue(
            root,
            QueueEntry {
                mission_id: id.into(),
                ticket_slug: None,
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();
    }

    // Claim removes the entry from the visible queue without deleting it.
    let claim = queue::claim_front(root).expect("front claimable");
    assert_eq!(claim.entry.mission_id, "m-a");
    assert_eq!(
        queue::list(root).len(),
        1,
        "claimed entry hidden from the queue"
    );

    // Release puts it back at its original position.
    queue::release_claim(claim);
    assert_eq!(queue::list(root).len(), 2, "released entry restored");
    assert_eq!(
        queue::peek(root).unwrap().mission_id,
        "m-a",
        "order preserved"
    );

    // Finish retires it for good.
    let claim = queue::claim_front(root).unwrap();
    queue::finish_claim(claim);
    assert_eq!(queue::list(root).len(), 1);
    assert_eq!(queue::peek(root).unwrap().mission_id, "m-b");

    // A claim held by a DEAD pid is recovered; one held by THIS live process
    // is left alone. Foreign-pid liveness detection is unix-only
    // (`libc::kill(pid, 0)` → ESRCH); on Windows a claim held by another pid
    // can't be proven dead, so recover_dead_claims is a documented no-op there
    // and the forged claim simply persists — hence the recovery assertion is
    // cfg(unix), mirroring the queue/lock liveness tests above.
    let claim = queue::claim_front(root).unwrap();
    #[cfg(unix)]
    {
        let claimed_dir = queue::queue_dir(root);
        let live_name = std::fs::read_dir(&claimed_dir)
            .unwrap()
            .flatten()
            .map(|f| f.file_name().to_string_lossy().to_string())
            .find(|n| n.contains(".claimed."))
            .expect("live claim file present");
        // Forge a dead-pid claim beside it.
        let dead_name = live_name.replace(
            &format!(".claimed.{}", std::process::id()),
            ".claimed.999999999",
        );
        std::fs::copy(claimed_dir.join(&live_name), claimed_dir.join(&dead_name)).unwrap();
        let recovered = queue::recover_dead_claims(root);
        assert_eq!(
            recovered, 1,
            "dead-pid claim recovered, live claim untouched"
        );
        assert!(
            claimed_dir.join(&live_name).exists(),
            "live claim survives recovery"
        );
    }
    queue::finish_claim(claim);
}

// ---------------------------------------------------------------------------
// Dead-claim recovery: liveness beats age (queue-liveness-over-age, review P2)
// ---------------------------------------------------------------------------

/// Back-date a file's mtime by `secs` (claim-recovery age tests).
fn age_file(path: &Path, secs: u64) {
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    let past = std::time::SystemTime::now() - std::time::Duration::from_secs(secs);
    file.set_modified(past).unwrap();
}

fn queued(root: &Path, mission_id: &str) {
    queue::enqueue(
        root,
        QueueEntry {
            mission_id: mission_id.into(),
            ticket_slug: None,
            priority: 2,
            seq: 0,
        },
    )
    .unwrap();
}

/// Path of the single `.claimed.*` file currently in the queue dir.
fn claimed_path(root: &Path) -> std::path::PathBuf {
    fs::read_dir(queue::queue_dir(root))
        .unwrap()
        .flatten()
        .map(|f| f.path())
        .find(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().contains(".claimed."))
        })
        .expect("a claimed file exists")
}

/// Rename `mission_id`'s entry file into a claim held by `pid`, returning
/// the claimed path.
fn forge_claim(root: &Path, mission_id: &str, pid: &str) -> std::path::PathBuf {
    let dir = queue::queue_dir(root);
    let name = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|f| f.file_name().to_string_lossy().into_owned())
        .find(|n| n.ends_with(&format!("-{mission_id}.json")))
        .expect("entry file exists");
    let claimed = dir.join(format!("{name}.claimed.{pid}"));
    fs::rename(dir.join(&name), &claimed).unwrap();
    claimed
}

/// [`forge_claim`] with an identity-token suffix (`<pid>.<token>`), the
/// shape `claim_front` writes when the platform provides tokens.
fn forge_claim_with_token(
    root: &Path,
    mission_id: &str,
    pid: &str,
    token: &str,
) -> std::path::PathBuf {
    let dir = queue::queue_dir(root);
    let name = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|f| f.file_name().to_string_lossy().into_owned())
        .find(|n| n.ends_with(&format!("-{mission_id}.json")))
        .expect("entry file exists");
    let claimed = dir.join(format!("{name}.claimed.{pid}.{token}"));
    fs::rename(dir.join(&name), &claimed).unwrap();
    claimed
}

/// 4th-pass review: an ALIVE pid whose recorded token is not the claimant's
/// is a recycled pid after a crash — the age backstop must fire, or the
/// claim strands forever behind an unrelated long-lived process.
#[cfg(unix)]
#[test]
fn old_claim_with_recycled_pid_token_is_recovered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    queued(root, "m-recycled");
    let claim = forge_claim_with_token(
        root,
        "m-recycled",
        &std::process::id().to_string(),
        "0000000000000000",
    );
    age_file(&claim, 2 * 3600);

    assert_eq!(
        queue::recover_dead_claims(root),
        1,
        "alive pid + foreign token = recycled claimant: age backstop fires"
    );
    assert!(queue::peek(root).is_some(), "entry is requeued");
}

/// The same recycle shape but NOT aged: no evidence the claimant died, so
/// the claim stands (the age backstop is the only recovery path for it).
#[cfg(unix)]
#[test]
fn young_claim_with_recycled_pid_token_is_not_recovered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    queued(root, "m-recycled-young");
    forge_claim_with_token(
        root,
        "m-recycled-young",
        &std::process::id().to_string(),
        "0000000000000000",
    );

    assert_eq!(queue::recover_dead_claims(root), 0);
}

/// A REAL claim from `claim_front` — which writes the current process's
/// identity token when the platform provides one — aged past the backstop
/// still stands: provably-ours alive claimant.
#[cfg(unix)]
#[test]
fn old_claim_with_matching_identity_token_is_not_recovered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    queued(root, "m-matching");
    let claim = queue::claim_front(root).expect("front claimable");
    age_file(&claimed_path(root), 2 * 3600);

    assert_eq!(
        queue::recover_dead_claims(root),
        0,
        "token-matching live claimant stands at any age"
    );
    queue::finish_claim(claim);
}

/// An OLD claim whose pid is ALIVE must NOT be requeued: age only breaks a
/// tie the liveness probe cannot settle — it never overrides a live
/// dispatcher, or a second dispatcher would requeue (and duplicate) a
/// genuinely long-running mission. unix-only: the probe is `kill(pid, 0)`;
/// on non-unix an aged claim is conservatively recovered by age alone (see
/// the cfg(not(unix)) test below).
#[cfg(unix)]
#[test]
fn old_claim_with_live_pid_is_not_recovered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    queued(root, "m-long");
    let claim = queue::claim_front(root).expect("front claimable");
    age_file(&claimed_path(root), 2 * 3600);

    assert_eq!(
        queue::recover_dead_claims(root),
        0,
        "live pid: age must not override confirmed liveness"
    );
    assert!(
        queue::peek(root).is_none(),
        "entry stays claimed instead of being requeued"
    );
    queue::finish_claim(claim);
}

/// The dead-pid rule is unchanged by the reorder: a provably dead pid is
/// recovered whatever the claim's age — here >1h.
#[cfg(unix)]
#[test]
fn old_claim_with_dead_pid_is_recovered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    queued(root, "m-dead-old");
    age_file(&forge_claim(root, "m-dead-old", "999999999"), 2 * 3600);

    assert_eq!(queue::recover_dead_claims(root), 1);
    assert_eq!(
        queue::peek(root).unwrap().mission_id,
        "m-dead-old",
        "requeued entry is claimable again"
    );
}

/// Preserved pre-existing behavior: a dead pid is recovered even when the
/// claim is younger than the one-hour backstop.
#[cfg(unix)]
#[test]
fn young_claim_with_dead_pid_is_recovered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    queued(root, "m-dead-young");
    forge_claim(root, "m-dead-young", "999999999");

    assert_eq!(queue::recover_dead_claims(root), 1);
    assert_eq!(queue::peek(root).unwrap().mission_id, "m-dead-young");
}

/// Ambiguous-probe path: a claim whose pid suffix can't be parsed belongs
/// to no dispatcher the probe could confirm — it is recovered immediately
/// (existing rule, platform-independent).
#[test]
fn claim_with_unparseable_pid_is_recovered() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    queued(root, "m-garbled");
    forge_claim(root, "m-garbled", "not-a-pid");

    assert_eq!(queue::recover_dead_claims(root), 1);
    assert_eq!(queue::peek(root).unwrap().mission_id, "m-garbled");
}

/// The non-unix posture: no liveness probe exists, so an aged claim is
/// recovered by age ALONE even though the recorded pid (ours) is alive —
/// the conservative choice, since a recycled pid reads alive and nothing
/// can disprove it. Windows CI is the oracle for this path.
#[cfg(not(unix))]
#[test]
fn old_claim_is_recovered_by_age_when_pid_liveness_is_unprobeable() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    queued(root, "m-win");
    let claim = queue::claim_front(root).expect("front claimable");
    age_file(&claimed_path(root), 2 * 3600);

    assert_eq!(queue::recover_dead_claims(root), 1);
    assert_eq!(queue::peek(root).unwrap().mission_id, "m-win");
    queue::finish_claim(claim);
}

// ---------------------------------------------------------------------------
// Ticket→mission link (fix-ticket-mission-linkage)
// ---------------------------------------------------------------------------

#[test]
fn mission_link_records_and_survives_state_flips() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();

    // No sidecar yet: no link.
    assert_eq!(Ticket::mission_for(root, "my-fix"), None);

    Ticket::record_mission(root, "my-fix", "m-abc123").unwrap();
    assert_eq!(
        Ticket::mission_for(root, "my-fix").as_deref(),
        Some("m-abc123")
    );
    // record_mission with no prior sidecar lands in Drafting.
    assert_eq!(Ticket::read_state(root, "my-fix"), TicketState::Drafting);

    // Every later state flip must PRESERVE the link — approve resolves
    // through it after the draft parked (Review) and queued (Queued).
    for state in [TicketState::Review, TicketState::Queued, TicketState::Done] {
        Ticket::write_state(root, "my-fix", state, None).unwrap();
        assert_eq!(
            Ticket::mission_for(root, "my-fix").as_deref(),
            Some("m-abc123"),
            "state flip to {state:?} must not erase the mission link"
        );
    }

    // Traversal-shaped slugs never read or write.
    assert_eq!(Ticket::mission_for(root, "../evil"), None);
    assert!(Ticket::record_mission(root, "../evil", "m-x").is_err());
}

#[test]
fn slug_for_mission_reverse_lookup() {
    let repo = tempfile::tempdir().unwrap();
    let root = repo.path();

    assert_eq!(Ticket::slug_for_mission(root, "m-abc123"), None);
    assert_eq!(Ticket::slug_for_mission(root, ""), None);

    Ticket::scaffold(root, "my-fix", "Fix", None, None).unwrap();
    Ticket::record_mission(root, "my-fix", "m-abc123").unwrap();
    assert_eq!(
        Ticket::slug_for_mission(root, "m-abc123").as_deref(),
        Some("my-fix")
    );
    assert_eq!(Ticket::slug_for_mission(root, "m-other"), None);
}
