//! Integration tests for the control inbox (design.md "Cross-process
//! control") and the orchestrator context digest (plan §4.8).

use chrono::{DateTime, TimeZone, Utc};
use kranz_engine::control::{self, ControlWatcher};
use kranz_engine::digest;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::paths::MissionPaths;
use kranz_engine::reducer::fold;
use kranz_engine::types::*;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

const MISSION: &str = "m-1";

fn temp_paths() -> (tempfile::TempDir, MissionPaths) {
    let dir = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(dir.path(), MISSION);
    (dir, paths)
}

/// ControlCommand has no PartialEq; compare via its JSON value.
fn as_json(cmd: &ControlCommand) -> serde_json::Value {
    serde_json::to_value(cmd).unwrap()
}

fn control_entries(paths: &MissionPaths) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(paths.control_dir())
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// enqueue / drain
// ---------------------------------------------------------------------------

#[test]
fn enqueue_drain_round_trip_preserves_order() {
    let (_dir, paths) = temp_paths();

    let cmds = vec![
        ControlCommand::Pause,
        ControlCommand::Msg {
            text: "status update please".into(),
            interrupt: false,
        },
        ControlCommand::Msg {
            text: "stop now".into(),
            interrupt: true,
        },
        ControlCommand::ConfigChange {
            patch: json!({ "worker": { "model": "opus" } }),
        },
        ControlCommand::Resume,
    ];

    for cmd in &cmds {
        let path = control::enqueue(&paths, cmd).unwrap();
        assert!(path.exists(), "enqueued file must exist");
        assert_eq!(path.parent().unwrap(), paths.control_dir());
        // Distinct millis prefixes so lexicographic order == enqueue order.
        std::thread::sleep(Duration::from_millis(3));
    }

    let drained = control::drain(&paths).unwrap();
    assert_eq!(
        drained
            .iter()
            .map(|(_, cmd)| as_json(cmd))
            .collect::<Vec<_>>(),
        cmds.iter().map(as_json).collect::<Vec<_>>(),
        "drain must return commands in enqueue order"
    );

    // Drain is NON-destructive: every file survives until the caller has
    // durably applied its command and deletes it. A crash between drain and
    // apply therefore re-processes commands instead of losing them.
    for (path, _) in &drained {
        assert!(
            path.exists(),
            "drained file must survive drain: {}",
            path.display()
        );
    }
    assert_eq!(
        control_entries(&paths).len(),
        cmds.len(),
        "inbox untouched by drain"
    );

    // Processing stopped before any delete (simulated crash): a second drain
    // sees the exact same queue again.
    let again = control::drain(&paths).unwrap();
    assert_eq!(
        again
            .iter()
            .map(|(_, cmd)| as_json(cmd))
            .collect::<Vec<_>>(),
        cmds.iter().map(as_json).collect::<Vec<_>>(),
        "undeleted files drain again after a crash"
    );

    // The normal path — caller deletes after applying — empties the inbox.
    for (path, _) in drained {
        std::fs::remove_file(path).unwrap();
    }
    assert!(
        control_entries(&paths).is_empty(),
        "inbox empty once the caller deletes"
    );
    assert!(control::drain(&paths).unwrap().is_empty());
}

#[test]
fn enqueue_names_sort_chronologically() {
    let (_dir, paths) = temp_paths();
    let path = control::enqueue(&paths, &ControlCommand::Pause).unwrap();
    let name = path.file_name().unwrap().to_str().unwrap();

    // <20-digit zero-padded millis>-<8 hex>.json
    assert_eq!(
        name.len(),
        20 + 1 + 8 + ".json".len(),
        "unexpected name shape: {name}"
    );
    assert!(
        name[..20].chars().all(|c| c.is_ascii_digit()),
        "millis prefix: {name}"
    );
    assert_eq!(&name[20..21], "-");
    assert!(
        name[21..29].chars().all(|c| c.is_ascii_hexdigit()),
        "rand suffix: {name}"
    );
    assert!(name.ends_with(".json"));
}

#[test]
fn drain_on_missing_control_dir_is_empty() {
    let (_dir, paths) = temp_paths();
    // Nothing created yet: no mission dir, no control dir.
    assert!(control::drain(&paths).unwrap().is_empty());
    assert!(!control::peek_interrupt(&paths).unwrap());
}

#[test]
fn corrupt_file_is_quarantined_and_never_blocks_the_queue() {
    let (_dir, paths) = temp_paths();
    std::fs::create_dir_all(paths.control_dir()).unwrap();

    // Corrupt file that sorts FIRST — it must not block later commands.
    let corrupt_name = "00000000000000000000-deadbeef.json";
    std::fs::write(paths.control_dir().join(corrupt_name), "{not json").unwrap();
    // Non-.json files are ignored entirely.
    std::fs::write(paths.control_dir().join("notes.txt"), "ignore me").unwrap();

    control::enqueue(&paths, &ControlCommand::Pause).unwrap();
    std::thread::sleep(Duration::from_millis(3));
    control::enqueue(&paths, &ControlCommand::Resume).unwrap();

    let drained = control::drain(&paths).unwrap();
    assert_eq!(
        drained
            .iter()
            .map(|(_, cmd)| as_json(cmd))
            .collect::<Vec<_>>(),
        vec![
            as_json(&ControlCommand::Pause),
            as_json(&ControlCommand::Resume)
        ],
        "valid commands drain despite the corrupt file"
    );

    let entries = control_entries(&paths);
    assert!(
        entries.contains(&format!("{corrupt_name}.bad")),
        "corrupt file renamed .bad: {entries:?}"
    );
    assert!(
        entries.contains(&"notes.txt".to_string()),
        "stray file untouched: {entries:?}"
    );
    assert_eq!(
        entries.len(),
        4,
        "both valid files still queued (drain is non-destructive): {entries:?}"
    );

    // The .bad quarantine never comes back on later drains; the (undeleted)
    // valid files do.
    assert_eq!(control::drain(&paths).unwrap().len(), 2);
    for (path, _) in drained {
        std::fs::remove_file(path).unwrap();
    }
    assert!(control::drain(&paths).unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// peek_interrupt
// ---------------------------------------------------------------------------

#[test]
fn peek_interrupt_only_on_interrupt_msg_and_is_non_destructive() {
    let (_dir, paths) = temp_paths();

    control::enqueue(&paths, &ControlCommand::Pause).unwrap();
    std::thread::sleep(Duration::from_millis(3));
    control::enqueue(
        &paths,
        &ControlCommand::Msg {
            text: "fyi".into(),
            interrupt: false,
        },
    )
    .unwrap();
    assert!(
        !control::peek_interrupt(&paths).unwrap(),
        "no interrupt queued yet"
    );

    std::thread::sleep(Duration::from_millis(3));
    control::enqueue(
        &paths,
        &ControlCommand::Msg {
            text: "stop".into(),
            interrupt: true,
        },
    )
    .unwrap();
    assert!(control::peek_interrupt(&paths).unwrap());
    assert!(
        control::peek_interrupt(&paths).unwrap(),
        "peek must not consume"
    );

    // Everything (including the interrupt Msg) still drains, in order.
    let drained = control::drain(&paths).unwrap();
    assert_eq!(drained.len(), 3);
    assert!(
        matches!(&drained[2].1, ControlCommand::Msg { text, interrupt: true } if text == "stop"),
        "interrupt message survived the peeks: {:?}",
        drained[2].1
    );
}

// ---------------------------------------------------------------------------
// ControlWatcher
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wait_for_interrupt_fires_notify_within_bounded_time() {
    let (_dir, paths) = temp_paths();
    control::enqueue(
        &paths,
        &ControlCommand::Msg {
            text: "abort".into(),
            interrupt: true,
        },
    )
    .unwrap();

    let notify = Arc::new(Notify::new());
    let notified = notify.notified();
    tokio::pin!(notified);
    notified.as_mut().enable(); // register interest before the watcher fires

    let watcher = tokio::spawn(ControlWatcher::wait_for_interrupt(
        paths.clone(),
        Duration::from_millis(10),
        notify.clone(),
    ));

    tokio::time::timeout(Duration::from_secs(5), notified)
        .await
        .expect("notify must fire within 5s");
    tokio::time::timeout(Duration::from_secs(5), watcher)
        .await
        .expect("watcher must return after notifying")
        .unwrap();

    // The watcher peeks; the interrupt message is still queued for drain.
    let drained = control::drain(&paths).unwrap();
    assert_eq!(drained.len(), 1);
    assert!(matches!(
        drained[0].1,
        ControlCommand::Msg {
            interrupt: true,
            ..
        }
    ));
}

/// Regression (interrupt loss): the watcher must fire `notify_one`, which
/// stores a permit when nobody is registered yet. With `notify_waiters` a
/// fire during `backend.start()` (or between two `notified()` registrations
/// of the run loop) woke nobody, stored nothing, and the interrupt was lost
/// forever.
#[tokio::test]
async fn wait_for_interrupt_permit_survives_until_a_late_waiter() {
    let (_dir, paths) = temp_paths();
    control::enqueue(
        &paths,
        &ControlCommand::Msg {
            text: "abort".into(),
            interrupt: true,
        },
    )
    .unwrap();

    // Run the watcher TO COMPLETION with nobody listening.
    let notify = Arc::new(Notify::new());
    tokio::time::timeout(
        Duration::from_secs(5),
        ControlWatcher::wait_for_interrupt(
            paths.clone(),
            Duration::from_millis(10),
            Arc::clone(&notify),
        ),
    )
    .await
    .expect("watcher must fire and return within 5s");

    // A waiter that registers only AFTER the fire must still observe it.
    tokio::time::timeout(Duration::from_secs(5), notify.notified())
        .await
        .expect("the stored permit must complete a late notified()");
}

#[tokio::test]
async fn wait_for_interrupt_does_not_fire_without_interrupt() {
    let (_dir, paths) = temp_paths();
    control::enqueue(&paths, &ControlCommand::Pause).unwrap();
    control::enqueue(
        &paths,
        &ControlCommand::Msg {
            text: "fyi".into(),
            interrupt: false,
        },
    )
    .unwrap();

    let notify = Arc::new(Notify::new());
    let notified = notify.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();

    let watcher = tokio::spawn(ControlWatcher::wait_for_interrupt(
        paths.clone(),
        Duration::from_millis(10),
        notify.clone(),
    ));

    let fired = tokio::time::timeout(Duration::from_millis(250), notified).await;
    assert!(
        fired.is_err(),
        "notify must NOT fire without an interrupt message"
    );
    assert!(!watcher.is_finished(), "watcher keeps polling");
    watcher.abort();
}

// ---------------------------------------------------------------------------
// Digest
// ---------------------------------------------------------------------------

fn base_ts() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
}

fn ev(seq: u64, kind: EventKind) -> Event {
    Event {
        seq,
        ts: base_ts() + chrono::Duration::seconds(seq as i64),
        mission_id: MISSION.to_string(),
        kind,
    }
}

fn created() -> EventKind {
    EventKind::MissionCreated {
        goal: "Ship the widget".to_string(),
        base_branch: "main".to_string(),
        mission_branch: format!("kranz/mission-{MISSION}"),
        config: MissionConfig::default(),
    }
}

fn plan() -> Plan {
    let feature = |title: &str| PlanFeature {
        title: title.to_string(),
        spec: format!("spec for {title}"),
        validation_criteria: vec![format!("{title} works")],
    };
    Plan {
        goal: "Ship the widget, planned".to_string(),
        validation_contract: vec![
            Assertion {
                id: "a-1".to_string(),
                statement: "cargo test passes".to_string(),
                check: AssertionCheck::Command,
                command: Some("cargo test".to_string()),
            },
            Assertion {
                id: "a-2".to_string(),
                statement: "docs are accurate".to_string(),
                check: AssertionCheck::AgentJudgement,
                command: None,
            },
        ],
        milestones: vec![
            PlanMilestone {
                title: "Milestone one".to_string(),
                features: vec![feature("Alpha"), feature("Beta")],
            },
            PlanMilestone {
                title: "Milestone two".to_string(),
                features: vec![feature("Gamma")],
            },
        ],
        command_grants: vec![],
        touch_set: vec![],
    }
}

/// created / plan.approved / milestone.started / feature.started /
/// worker.spawned / worker.completed / user.message / orchestrator.decision —
/// then one more open user message so both digest branches render.
fn digest_events() -> Vec<Event> {
    vec![
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
        ev(
            3,
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".to_string(),
                start_sha: "abc123".to_string(),
            },
        ),
        ev(
            4,
            EventKind::FeatureStarted {
                feature_id: "f-1-1".to_string(),
            },
        ),
        ev(
            5,
            EventKind::WorkerSpawned {
                run_id: "r-1".to_string(),
                role: Role::Worker,
                feature_id: Some("f-1-1".to_string()),
                milestone_id: Some("ms-1".to_string()),
                sdk_session_id: "sess-r-1".to_string(),
                model: "sonnet".to_string(),
                prompt_hash: "deadbeef".to_string(),
                transcript_path: "runs/r-1.jsonl".to_string(),
            },
        ),
        ev(
            6,
            EventKind::WorkerCompleted {
                run_id: "r-1".to_string(),
                result: RunResult::Pass,
                tokens: TokenUsage {
                    input: 1200,
                    output: 340,
                    cache_read: 0,
                    cache_write: 0,
                },
                cost_usd: Some(0.5),
                report: None,
            },
        ),
        ev(
            7,
            EventKind::UserMessage {
                text: "please add docs".to_string(),
                interrupt: false,
            },
        ),
        ev(
            8,
            EventKind::OrchestratorDecision {
                summary: "started milestone one; alpha implemented".to_string(),
                detail: None,
            },
        ),
        ev(
            9,
            EventKind::UserMessage {
                text: "and update the readme".to_string(),
                interrupt: false,
            },
        ),
    ]
}

/// Committed layout snapshot — digest::render must be byte-identical to this
/// for the state folded from digest_events().
const EXPECTED_DIGEST: &str = "MISSION m-1 [running] — Ship the widget, planned
branch kranz/mission-m-1 (from main) | tokens in/out 1200/340 | cost $0.50
CONTRACT:
- [a-1|command] cargo test passes :: cargo test
- [a-2|judgement] docs are accurate
MILESTONES:
ms-1 [active] Milestone one (fixCycles 0)
  f-1-1 [active|plan] Alpha (runs 1, respawns 0)
  f-1-2 [pending|plan] Beta (runs 0, respawns 0)
ms-2 [pending] Milestone two (fixCycles 0)
  f-2-1 [pending|plan] Gamma (runs 0, respawns 0)
RECENT DECISIONS:
- started milestone one; alpha implemented
OPEN USER MESSAGES:
- and update the readme
You are resuming from durable state; the event log is authoritative.";

#[test]
fn digest_matches_committed_snapshot_and_is_deterministic() {
    let state = fold(&digest_events()).unwrap();
    let rendered = digest::render(&state);

    // Key lines present.
    assert!(rendered.contains("MISSION m-1 [running] — Ship the widget, planned"));
    assert!(rendered.contains("branch kranz/mission-m-1 (from main)"));
    assert!(rendered.contains("- [a-1|command] cargo test passes :: cargo test"));
    assert!(rendered.contains("- [a-2|judgement] docs are accurate"));
    assert!(rendered.contains("ms-1 [active] Milestone one (fixCycles 0)"));
    assert!(rendered.contains("  f-1-1 [active|plan] Alpha (runs 1, respawns 0)"));
    assert!(rendered.contains("- started milestone one; alpha implemented"));
    assert!(rendered.contains("- and update the readme"));
    assert!(
        rendered.ends_with("You are resuming from durable state; the event log is authoritative.")
    );

    // Exact committed snapshot, stable across runs.
    assert_eq!(rendered, EXPECTED_DIGEST);
    assert_eq!(
        digest::render(&state),
        rendered,
        "same state renders byte-identically"
    );
}

#[test]
fn digest_shows_none_when_no_open_user_messages() {
    // Stop before the trailing user message: the decision consumed the queue.
    let events = &digest_events()[..8];
    let state = fold(events).unwrap();
    assert!(digest::render(&state).contains("OPEN USER MESSAGES:\n(none)"));
}

#[test]
fn digest_truncates_long_titles_and_decisions() {
    let long_title = "x".repeat(300);
    let long_decision = "d".repeat(300);
    let mut plan = plan();
    plan.milestones[0].features[0].title = long_title;

    let state = fold(&[
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan,
                base_sha: None,
            },
        ),
        ev(
            3,
            EventKind::OrchestratorDecision {
                summary: long_decision,
                detail: None,
            },
        ),
    ])
    .unwrap();
    let rendered = digest::render(&state);

    // Titles cut at 160 chars, decisions at 200; marker appended.
    let cut_title = format!("{}… [truncated]", "x".repeat(160));
    assert!(
        rendered.contains(&cut_title),
        "title truncated at 160 chars"
    );
    assert!(
        !rendered.contains(&"x".repeat(161)),
        "no more than 160 title chars survive"
    );

    let cut_decision = format!("{}… [truncated]", "d".repeat(200));
    assert!(
        rendered.contains(&cut_decision),
        "decision truncated at 200 chars"
    );
    assert!(
        !rendered.contains(&"d".repeat(201)),
        "no more than 200 decision chars survive"
    );
}

#[test]
fn render_reseed_appends_plan_json_verbatim() {
    let state = fold(&digest_events()).unwrap();
    let plan_json = serde_json::to_string_pretty(&plan()).unwrap();

    let reseed = digest::render_reseed(&state, &plan_json);
    assert_eq!(
        reseed,
        format!(
            "{}\n\nAPPROVED PLAN (plan.json):\n{plan_json}",
            digest::render(&state)
        )
    );
    assert!(
        reseed.ends_with(&plan_json),
        "plan JSON is appended verbatim"
    );
}
