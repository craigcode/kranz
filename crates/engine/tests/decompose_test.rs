//! Engine tests for `kranz decompose` ([`kranz_engine::decompose`]): the
//! planner turn driven entirely through [`MockBackend`] (no real `claude`),
//! and the order-of-execution proof that decomposed tickets run under the
//! EXISTING claim/deps machinery — no new orchestration. Drain scenarios
//! mirror the harness conventions of `work.rs`'s own drain tests
//! (`drain_queue_with_probe` + scripted `run_mission` + handwritten events).

use kranz_engine::backend_mock::{mock_result_error, MockScript};
use kranz_engine::decompose::{drive_decompose, write_dag, PlannedNode};
use kranz_engine::deps;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::queue;
use kranz_engine::ticket::{Ticket, TicketState};
use kranz_engine::types::MissionConfig;
use kranz_engine::work::drain_queue_with_probe;
use kranz_engine::{backend_readiness, MockBackend};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A fresh tempdir repo root (no git needed: decompose touches only
/// `.kranz/tickets`, and the drain harness below works off ticket files,
/// queue files, and handwritten event logs).
fn repo() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let path = dir.path().to_path_buf();
    (dir, path)
}

/// The planner's scripted reply: a two-node chain — `t1` (root), `t2`
/// blocked-by `t1`.
fn chain_json() -> serde_json::Value {
    json!([
        {
            "slug": "t1",
            "title": "Part one",
            "priority": 1,
            "goal": "deliver part one",
            "context": "first half of the goal",
            "acceptanceHints": ["part one works"],
            "blockedBy": []
        },
        {
            "slug": "t2",
            "title": "Part two",
            "priority": 2,
            "goal": "deliver part two",
            "context": "second half of the goal",
            "acceptanceHints": ["part two works"],
            "blockedBy": ["t1"]
        }
    ])
}

fn planner_backend(reply: serde_json::Value) -> MockBackend {
    MockBackend::with_scripts(vec![MockScript::single_shot_json(&reply)])
}

fn md_files(repo: &Path) -> Vec<String> {
    let dir = Ticket::tickets_dir(repo);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            name.ends_with(".md").then_some(name)
        })
        .collect();
    files.sort();
    files
}

// ---------------------------------------------------------------------------
// Drain harness (mirrors work.rs's own drain tests)
// ---------------------------------------------------------------------------

/// Handwritten events.jsonl for `mission_id` (mirrors work.rs::write_events).
fn write_events(repo_root: &Path, mission_id: &str, kinds: Vec<EventKind>) {
    let dir = repo_root.join(".kranz").join("missions").join(mission_id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut lines = String::new();
    for (i, kind) in kinds.into_iter().enumerate() {
        let event = Event {
            seq: (i + 1) as u64,
            ts: chrono::Utc::now(),
            mission_id: mission_id.to_string(),
            kind,
        };
        lines.push_str(&serde_json::to_string(&event).unwrap());
        lines.push('\n');
    }
    std::fs::write(dir.join("events.jsonl"), lines).unwrap();
}

fn created() -> EventKind {
    EventKind::MissionCreated {
        goal: "decomposed node".to_string(),
        base_branch: "main".to_string(),
        mission_branch: "kranz/mission-fixture".to_string(),
        config: MissionConfig::default(),
    }
}

/// Link a decomposed ticket to its (pretend-drafted) mission and park it in
/// Review, exactly where `kranz draft` leaves a hand-written ticket.
fn park_drafted(repo: &Path, slug: &str, mission_id: &str) {
    Ticket::record_mission(repo, slug, mission_id).unwrap();
    Ticket::write_state(repo, slug, TicketState::Review, None).unwrap();
}

fn always_proceed(
    _repo: &Path,
    mission_id: &str,
) -> kranz_engine::error::Result<backend_readiness::ReadinessReport> {
    Ok(backend_readiness::ReadinessReport {
        mission_id: mission_id.to_string(),
        roles: vec![],
        overall: backend_readiness::ReadinessStatus::Ok,
        warnings: vec![],
    })
}

// ---------------------------------------------------------------------------
// The planner turn over the mock backend
// ---------------------------------------------------------------------------

#[tokio::test]
async fn planner_turn_with_yes_writes_the_dag() {
    let (_dir, root) = repo();
    let backend = planner_backend(chain_json());

    let drive = drive_decompose(
        &backend,
        &root,
        "build the two-part thing",
        &MissionConfig::default(),
        true,
    )
    .await
    .unwrap();

    assert_eq!(drive.nodes.len(), 2);
    let written = drive.written.expect("--yes must write");
    assert_eq!(written.len(), 2);
    assert_eq!(md_files(&root), vec!["t1.md", "t2.md"]);

    // The written tickets are ordinary tickets in the backlog sense.
    let t2 = Ticket::load(&Ticket::tickets_dir(&root).join("t2.md")).unwrap();
    assert_eq!(t2.blocked_by, vec!["t1".to_string()]);
    assert_eq!(t2.priority, 2);
    assert_eq!(Ticket::read_state(&root, "t2"), TicketState::New);

    // The planner session mirrors the draft loop's orchestrator shape:
    // read-only, single-shot, carrying the goal in its prompt.
    let specs = backend.started_specs();
    assert_eq!(specs.len(), 1);
    assert!(!specs[0].writable);
    match &specs[0].prompt {
        kranz_engine::backend::PromptMode::SingleShot(prompt) => {
            assert!(prompt.contains("build the two-part thing"));
            assert!(prompt.contains("blockedBy"));
        }
        other => panic!("expected a single-shot planner prompt, got {other:?}"),
    }
}

#[tokio::test]
async fn dry_run_without_yes_writes_nothing_even_for_a_valid_dag() {
    let (_dir, root) = repo();
    let backend = planner_backend(chain_json());

    let drive = drive_decompose(&backend, &root, "goal", &MissionConfig::default(), false)
        .await
        .unwrap();

    assert_eq!(drive.nodes.len(), 2, "the preview still carries the DAG");
    assert!(drive.written.is_none(), "no --yes, no write");
    assert!(
        md_files(&root).is_empty(),
        "dry-run must not write a single file"
    );
}

#[tokio::test]
async fn malformed_planner_reply_is_a_clear_error_and_writes_nothing() {
    let (_dir, root) = repo();
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot(
        "I cannot decompose this, it is too vague.",
    )]);

    let err = drive_decompose(&backend, &root, "goal", &MissionConfig::default(), true)
        .await
        .unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("JSON array"),
        "the error names the contract: {msg}"
    );
    assert!(md_files(&root).is_empty());
}

#[tokio::test]
async fn an_error_result_from_the_planner_surfaces_as_a_backend_error() {
    let (_dir, root) = repo();
    let backend = MockBackend::with_scripts(vec![MockScript {
        events: vec![mock_result_error("usage limit reached")],
        ..Default::default()
    }]);

    let err = drive_decompose(&backend, &root, "goal", &MissionConfig::default(), true)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("usage limit reached"));
    assert!(md_files(&root).is_empty());
}

#[tokio::test]
async fn an_over_8_node_reply_is_refused_before_any_write() {
    let (_dir, root) = repo();
    let nodes: Vec<serde_json::Value> = (0..9)
        .map(|i| json!({"slug": format!("n{i}"), "title": format!("node {i}")}))
        .collect();
    let backend = planner_backend(json!(nodes));

    let err = drive_decompose(&backend, &root, "goal", &MissionConfig::default(), true)
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("max"));
    assert!(md_files(&root).is_empty());
}

// ---------------------------------------------------------------------------
// Order of execution over the EXISTING queue/drain harness (no code change —
// these prove the existing claim/deps semantics hold for decomposed tickets)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_blocked_node_cannot_queue_before_its_blocker_completes_then_runs_after() {
    let (_dir, root) = repo();
    write_dag(
        &root,
        &[
            PlannedNode {
                slug: "t1".to_string(),
                title: "Part one".to_string(),
                priority: 1,
                goal: "part one".to_string(),
                context: String::new(),
                acceptance_hints: vec![],
                blocked_by: vec![],
            },
            PlannedNode {
                slug: "t2".to_string(),
                title: "Part two".to_string(),
                priority: 1,
                goal: "part two".to_string(),
                context: String::new(),
                acceptance_hints: vec![],
                blocked_by: vec!["t1".to_string()],
            },
        ],
    )
    .unwrap();
    park_drafted(&root, "t1", "m-t1");
    park_drafted(&root, "t2", "m-t2");

    // The approve gate: t2 refuses while t1's mission is not Complete,
    // naming the blocker — so t2 can never even enter the queue ahead of t1.
    let err = deps::approve_ticket(&root, "t2", None, false).unwrap_err();
    assert!(
        format!("{err:#}").contains("t1"),
        "the refusal names the unsatisfied blocker: {err}"
    );
    assert!(!queue::contains(&root, "m-t2"));

    deps::approve_ticket(&root, "t1", None, false).unwrap();

    // t1's mission Completes; the gate opens for t2.
    write_events(
        &root,
        "m-t1",
        vec![created(), EventKind::MissionCompleted {}],
    );
    deps::approve_ticket(&root, "t2", None, false).unwrap();

    // Both entries are queued (t1 first); the drain runs them in order and
    // reconciles each ticket to Done off the existing machinery.
    let ran_order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let ran_clone = ran_order.clone();
    let repo_path = root.clone();
    let report = drain_queue_with_probe(
        &root,
        false,
        move |mission_id| {
            let ran = ran_clone.clone();
            let repo_path = repo_path.clone();
            async move {
                ran.lock().unwrap().push(mission_id.clone());
                write_events(
                    &repo_path,
                    &mission_id,
                    vec![created(), EventKind::MissionCompleted {}],
                );
                Ok(0)
            }
        },
        always_proceed,
    )
    .await
    .unwrap();

    assert!(report.skipped.is_empty());
    assert_eq!(
        *ran_order.lock().unwrap(),
        vec!["m-t1".to_string(), "m-t2".to_string()],
        "the blocker ran before its dependent"
    );
    assert_eq!(Ticket::read_state(&root, "t1"), TicketState::Done);
    assert_eq!(Ticket::read_state(&root, "t2"), TicketState::Done);
}

#[tokio::test]
async fn a_failed_blocker_skips_dependents_with_a_recorded_warning() {
    let (_dir, root) = repo();
    write_dag(
        &root,
        &[
            PlannedNode {
                slug: "t1".to_string(),
                title: "Part one".to_string(),
                priority: 1,
                goal: "part one".to_string(),
                context: String::new(),
                acceptance_hints: vec![],
                blocked_by: vec![],
            },
            PlannedNode {
                slug: "t2".to_string(),
                title: "Part two".to_string(),
                priority: 1,
                goal: "part two".to_string(),
                context: String::new(),
                acceptance_hints: vec![],
                blocked_by: vec!["t1".to_string()],
            },
        ],
    )
    .unwrap();
    park_drafted(&root, "t1", "m-t1");
    park_drafted(&root, "t2", "m-t2");

    // Batch-approval: t2 is force-queued alongside t1 before t1 completes.
    deps::approve_ticket(&root, "t1", None, false).unwrap();
    deps::approve_ticket(&root, "t2", None, true).unwrap();

    // t1's mission FAILS mid-drain; t2 must be skipped, never run.
    let ran: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let ran_clone = ran.clone();
    let repo_path = root.clone();
    let report = drain_queue_with_probe(
        &root,
        false,
        move |mission_id| {
            let ran = ran_clone.clone();
            let repo_path = repo_path.clone();
            async move {
                ran.lock().unwrap().push(mission_id.clone());
                assert_eq!(mission_id, "m-t1", "only the blocker may run");
                write_events(
                    &repo_path,
                    &mission_id,
                    vec![
                        created(),
                        EventKind::MissionFailed {
                            reason: "validation failed".to_string(),
                        },
                    ],
                );
                Ok(1)
            }
        },
        always_proceed,
    )
    .await
    .unwrap();

    assert_eq!(*ran.lock().unwrap(), vec!["m-t1".to_string()]);
    assert_eq!(report.ran, vec!["m-t1".to_string()]);
    assert_eq!(
        report.skipped,
        vec!["m-t2".to_string()],
        "the dependent was skipped, not run"
    );
    assert_eq!(Ticket::read_state(&root, "t1"), TicketState::Failed);
    assert_eq!(Ticket::read_state(&root, "t2"), TicketState::Failed);
    // The recorded warning: the dependent's status note says WHY it was
    // skipped (and names the failed blocker).
    let note = std::fs::read_to_string(Ticket::tickets_dir(&root).join("t2.status")).unwrap();
    assert!(
        note.contains("blocked-by t1 failed"),
        "the skip warning is recorded on the ticket: {note}"
    );
    // The skip finished the claim: nothing re-queues forever.
    assert!(queue::list(&root).is_empty());
}
