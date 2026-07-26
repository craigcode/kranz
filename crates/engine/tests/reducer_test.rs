//! Integration tests for the pure event fold (plan §4.3/§4.4 semantics).

use chrono::{DateTime, TimeZone, Utc};
use kranz_engine::error::EngineError;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::reducer::{apply, dry_run_revised_plan, fold, read_snapshot, write_snapshot};
use kranz_engine::types::*;
use proptest::prelude::*;
use serde_json::json;
use sha2::{Digest, Sha256};

const MISSION: &str = "m-1";

fn base_ts() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
}

/// Deterministic event constructor: ts = base + seq seconds.
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
        goal: "build the thing".to_string(),
        base_branch: "main".to_string(),
        mission_branch: format!("kranz/mission-{MISSION}"),
        config: MissionConfig::default(),
    }
}

fn plan_feature(title: &str) -> PlanFeature {
    PlanFeature {
        title: title.to_string(),
        spec: format!("spec for {title}"),
        validation_criteria: vec![format!("{title} works")],
    }
}

/// Plan with 2 milestones / 3 features: ms-1 { f-1-1, f-1-2 }, ms-2 { f-2-1 }.
fn plan() -> Plan {
    Plan {
        goal: "build the thing, planned".to_string(),
        validation_contract: vec![Assertion {
            id: "a-1".to_string(),
            statement: "cargo test passes".to_string(),
            check: AssertionCheck::Command,
            command: Some("cargo test".to_string()),
        }],
        milestones: vec![
            PlanMilestone {
                title: "milestone one".to_string(),
                features: vec![plan_feature("alpha"), plan_feature("beta")],
            },
            PlanMilestone {
                title: "milestone two".to_string(),
                features: vec![plan_feature("gamma")],
            },
        ],
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec![],
    }
}

fn spawn(run_id: &str, feature_id: Option<&str>, milestone_id: Option<&str>) -> EventKind {
    EventKind::WorkerSpawned {
        run_id: run_id.to_string(),
        role: if feature_id.is_some() {
            Role::Worker
        } else {
            Role::ValidatorScrutiny
        },
        feature_id: feature_id.map(str::to_string),
        milestone_id: milestone_id.map(str::to_string),
        sdk_session_id: format!("sess-{run_id}"),
        model: "sonnet".to_string(),
        quant: "n/a".to_string(),
        weight_hash: None,
        prompt_hash: "deadbeef".to_string(),
        transcript_path: format!("runs/{run_id}.jsonl"),
    }
}

fn completed(run_id: &str, tokens: TokenUsage, cost: Option<f64>) -> EventKind {
    EventKind::WorkerCompleted {
        run_id: run_id.to_string(),
        result: RunResult::Pass,
        tokens,
        cost_usd: cost,
        report: None,
    }
}

fn tokens(input: u64, output: u64) -> TokenUsage {
    TokenUsage {
        input,
        output,
        cache_read: 1,
        cache_write: 2,
    }
}

fn fix_feature(id: &str) -> Feature {
    Feature {
        id: id.to_string(),
        title: format!("fix {id}"),
        spec: "fix it".to_string(),
        validation_criteria: vec!["fixed".to_string()],
        origin: FeatureOrigin::Fix,
        status: FeatureStatus::Pending,
        worker_runs: vec![],
        commits: vec![],
        respawns: 0,
    }
}

fn milestone<'a>(state: &'a MissionState, id: &str) -> &'a Milestone {
    state
        .mission
        .milestones
        .iter()
        .find(|m| m.id == id)
        .unwrap()
}

fn feature<'a>(state: &'a MissionState, id: &str) -> &'a Feature {
    state
        .mission
        .milestones
        .iter()
        .flat_map(|m| m.features.iter())
        .find(|f| f.id == id)
        .unwrap()
}

/// Fold a list of kinds with auto-assigned seqs.
fn fold_kinds(kinds: Vec<EventKind>) -> MissionState {
    let events: Vec<Event> = kinds
        .into_iter()
        .enumerate()
        .map(|(i, k)| ev(i as u64 + 1, k))
        .collect();
    fold(&events).unwrap()
}

// ---------------------------------------------------------------------------
// Golden happy path
// ---------------------------------------------------------------------------

#[test]
fn golden_happy_path() {
    // Stage 1: created.
    let e1 = ev(1, created());
    let mut state = fold(std::slice::from_ref(&e1)).unwrap();
    assert_eq!(state.mission.id, MISSION);
    assert_eq!(state.mission.goal, "build the thing");
    assert_eq!(state.mission.status, MissionStatus::Planning);
    assert_eq!(state.mission.created_at, e1.ts);
    assert_eq!(state.mission.base_branch, "main");
    assert_eq!(state.mission.mission_branch, "kranz/mission-m-1");
    assert!(state.mission.milestones.is_empty());
    assert!(state.mission.validation_contract.is_empty());
    assert_eq!(state.last_seq, 1);
    assert_eq!(state.config, MissionConfig::default());

    // Stage 2: plan approved -> deterministic ids, all pending, Approved.
    apply(
        &mut state,
        &ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
    )
    .unwrap();
    assert_eq!(state.mission.status, MissionStatus::Approved);
    assert_eq!(state.mission.goal, "build the thing, planned");
    assert_eq!(state.mission.validation_contract.len(), 1);
    let ms_ids: Vec<&str> = state
        .mission
        .milestones
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(ms_ids, vec!["ms-1", "ms-2"]);
    let f_ids: Vec<&str> = state
        .mission
        .milestones
        .iter()
        .flat_map(|m| m.features.iter().map(|f| f.id.as_str()))
        .collect();
    assert_eq!(f_ids, vec!["f-1-1", "f-1-2", "f-2-1"]);
    for m in &state.mission.milestones {
        assert_eq!(m.status, MilestoneStatus::Pending);
        assert_eq!(m.fix_cycles, 0);
        assert_eq!(m.start_sha, None);
        for f in &m.features {
            assert_eq!(f.status, FeatureStatus::Pending);
            assert_eq!(f.origin, FeatureOrigin::Plan);
            assert!(f.worker_runs.is_empty());
        }
    }

    // Stage 3: milestone starts.
    apply(
        &mut state,
        &ev(
            3,
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".to_string(),
                start_sha: "sha-1".to_string(),
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").status, MilestoneStatus::Active);
    assert_eq!(
        milestone(&state, "ms-1").start_sha.as_deref(),
        Some("sha-1")
    );
    assert_eq!(state.mission.status, MissionStatus::Running);

    // Stage 4: feature + worker run.
    apply(
        &mut state,
        &ev(
            4,
            EventKind::FeatureStarted {
                feature_id: "f-1-1".to_string(),
            },
        ),
    )
    .unwrap();
    assert_eq!(feature(&state, "f-1-1").status, FeatureStatus::Active);

    let e5 = ev(5, spawn("r-1", Some("f-1-1"), Some("ms-1")));
    apply(&mut state, &e5).unwrap();
    let run = &state.runs["r-1"];
    assert_eq!(run.role, Role::Worker);
    assert_eq!(run.feature_id.as_deref(), Some("f-1-1"));
    assert_eq!(run.milestone_id.as_deref(), Some("ms-1"));
    assert_eq!(run.started_at, e5.ts);
    assert_eq!(run.ended_at, None);
    assert_eq!(run.tokens, TokenUsage::default());
    assert!(run.result.is_none() && run.report.is_none() && run.cost_usd.is_none());
    assert_eq!(feature(&state, "f-1-1").worker_runs, vec!["r-1"]);
    assert_eq!(feature(&state, "f-1-1").respawns, 0);

    // Stage 5: worker.message changes nothing but last_seq.
    let before = serde_json::to_value(&state).unwrap();
    apply(
        &mut state,
        &ev(
            6,
            EventKind::WorkerMessage {
                run_id: "r-1".to_string(),
                tag: "text".to_string(),
                content: "hi".to_string(),
            },
        ),
    )
    .unwrap();
    let mut after = serde_json::to_value(&state).unwrap();
    after["lastSeq"] = json!(5);
    assert_eq!(before, after);

    // Stage 6: worker completes -> run finalized, totals accumulate.
    let e7 = ev(7, completed("r-1", tokens(100, 50), Some(0.25)));
    apply(&mut state, &e7).unwrap();
    let run = &state.runs["r-1"];
    assert_eq!(run.result, Some(RunResult::Pass));
    assert_eq!(run.tokens, tokens(100, 50));
    assert_eq!(run.cost_usd, Some(0.25));
    assert_eq!(run.ended_at, Some(e7.ts));
    assert_eq!(state.totals, tokens(100, 50));
    assert!((state.total_cost_usd - 0.25).abs() < 1e-12);

    // Stage 7: feature completes with commits.
    apply(
        &mut state,
        &ev(
            8,
            EventKind::FeatureCompleted {
                feature_id: "f-1-1".to_string(),
                commits: vec!["c1".to_string(), "c2".to_string()],
            },
        ),
    )
    .unwrap();
    assert_eq!(feature(&state, "f-1-1").status, FeatureStatus::Complete);
    assert_eq!(feature(&state, "f-1-1").commits, vec!["c1", "c2"]);

    // Stage 8: second feature (with a cost-less run: cost treated as 0).
    apply(
        &mut state,
        &ev(
            9,
            EventKind::FeatureStarted {
                feature_id: "f-1-2".to_string(),
            },
        ),
    )
    .unwrap();
    apply(
        &mut state,
        &ev(10, spawn("r-2", Some("f-1-2"), Some("ms-1"))),
    )
    .unwrap();
    apply(&mut state, &ev(11, completed("r-2", tokens(10, 5), None))).unwrap();
    apply(
        &mut state,
        &ev(
            12,
            EventKind::FeatureCompleted {
                feature_id: "f-1-2".to_string(),
                commits: vec!["c3".to_string()],
            },
        ),
    )
    .unwrap();
    assert_eq!(
        state.totals,
        TokenUsage {
            input: 110,
            output: 55,
            cache_read: 2,
            cache_write: 4
        }
    );
    assert!((state.total_cost_usd - 0.25).abs() < 1e-12);

    // Stage 9: milestone validates clean and completes.
    apply(
        &mut state,
        &ev(
            13,
            EventKind::MilestoneValidating {
                milestone_id: "ms-1".into(),
            },
        ),
    )
    .unwrap();
    assert_eq!(
        milestone(&state, "ms-1").status,
        MilestoneStatus::Validating
    );
    apply(
        &mut state,
        &ev(
            14,
            EventKind::MilestoneCompleted {
                milestone_id: "ms-1".into(),
                tag: None,
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").status, MilestoneStatus::Complete);
    assert_eq!(milestone(&state, "ms-1").fix_cycles, 0);
    assert_eq!(state.mission.status, MissionStatus::Running);

    // Stage 10: second milestone, then final gate and completion.
    apply(
        &mut state,
        &ev(
            15,
            EventKind::MilestoneStarted {
                milestone_id: "ms-2".to_string(),
                start_sha: "sha-2".to_string(),
            },
        ),
    )
    .unwrap();
    apply(
        &mut state,
        &ev(
            16,
            EventKind::FeatureStarted {
                feature_id: "f-2-1".to_string(),
            },
        ),
    )
    .unwrap();
    apply(
        &mut state,
        &ev(
            17,
            EventKind::FeatureCompleted {
                feature_id: "f-2-1".to_string(),
                commits: vec![],
            },
        ),
    )
    .unwrap();
    apply(
        &mut state,
        &ev(
            18,
            EventKind::MilestoneCompleted {
                milestone_id: "ms-2".into(),
                tag: Some("v1".into()),
            },
        ),
    )
    .unwrap();
    apply(&mut state, &ev(19, EventKind::MissionValidating {})).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Validating);
    apply(&mut state, &ev(20, EventKind::MissionCompleted {})).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Complete);
    assert_eq!(state.last_seq, 20);
}

// ---------------------------------------------------------------------------
// Guards
// ---------------------------------------------------------------------------

#[test]
fn fold_rejects_empty_and_wrong_first_event() {
    assert!(matches!(fold(&[]), Err(EngineError::InvalidState(_))));
    let not_created = ev(1, EventKind::MissionPaused {});
    assert!(matches!(
        fold(&[not_created]),
        Err(EngineError::InvalidState(_))
    ));
}

#[test]
fn apply_rejects_non_contiguous_seq_and_second_created() {
    let mut state = fold(&[ev(1, created())]).unwrap();
    let err = apply(&mut state, &ev(3, EventKind::MissionPaused {})).unwrap_err();
    assert!(matches!(err, EngineError::InvalidState(_)));
    let err = apply(&mut state, &ev(2, created())).unwrap_err();
    assert!(matches!(err, EngineError::InvalidState(_)));
}

#[test]
fn unknown_ids_are_invalid_state() {
    let base = vec![
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
    ];
    let cases = vec![
        EventKind::MilestoneStarted {
            milestone_id: "ms-99".into(),
            start_sha: "x".into(),
        },
        EventKind::FeatureStarted {
            feature_id: "f-9-9".into(),
        },
        spawn("r-1", Some("f-9-9"), None),
        spawn("r-1", None, Some("ms-99")),
        EventKind::WorkerMessage {
            run_id: "r-99".into(),
            tag: "text".into(),
            content: "".into(),
        },
        completed("r-99", TokenUsage::default(), None),
        EventKind::MilestoneValidating {
            milestone_id: "ms-99".into(),
        },
        EventKind::FixFeatureCreated {
            milestone_id: "ms-99".into(),
            feature: fix_feature("fx"),
        },
        EventKind::MilestoneBlocked {
            milestone_id: "ms-99".into(),
            reason: "r".into(),
        },
    ];
    for kind in cases {
        let mut state = fold(&base).unwrap();
        let err = apply(&mut state, &ev(3, kind.clone())).unwrap_err();
        assert!(
            matches!(err, EngineError::InvalidState(_)),
            "expected InvalidState for {kind:?}"
        );
    }
}

#[test]
fn respawns_count_second_and_later_runs() {
    let mut state = fold(&[
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
    ])
    .unwrap();
    apply(&mut state, &ev(3, spawn("r-1", Some("f-1-1"), None))).unwrap();
    assert_eq!(feature(&state, "f-1-1").respawns, 0);
    apply(&mut state, &ev(4, spawn("r-2", Some("f-1-1"), None))).unwrap();
    assert_eq!(feature(&state, "f-1-1").respawns, 1);
    apply(&mut state, &ev(5, spawn("r-3", Some("f-1-1"), None))).unwrap();
    assert_eq!(feature(&state, "f-1-1").respawns, 2);
    assert_eq!(
        feature(&state, "f-1-1").worker_runs,
        vec!["r-1", "r-2", "r-3"]
    );

    // Duplicate run id is corruption.
    let err = apply(&mut state, &ev(6, spawn("r-1", Some("f-1-2"), None))).unwrap_err();
    assert!(matches!(err, EngineError::InvalidState(_)));
}

// ---------------------------------------------------------------------------
// Fix cycles
// ---------------------------------------------------------------------------

#[test]
fn fix_cycles_increment_once_per_validation_round() {
    let mut state = fold(&[
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
                milestone_id: "ms-1".into(),
                start_sha: "s".into(),
            },
        ),
        ev(
            4,
            EventKind::MilestoneValidating {
                milestone_id: "ms-1".into(),
            },
        ),
        ev(5, spawn("r-v1", None, Some("ms-1"))),
        ev(
            6,
            EventKind::ValidationFinding {
                milestone_id: "ms-1".into(),
                run_id: "r-v1".into(),
                finding: Finding {
                    subject: "a-1".into(),
                    severity: "major".into(),
                    evidence: "it broke".into(),
                    suggested_fix: "fix it".into(),
                    class: String::new(),
                },
            },
        ),
    ])
    .unwrap();
    assert_eq!(
        milestone(&state, "ms-1").status,
        MilestoneStatus::Validating
    );
    assert_eq!(milestone(&state, "ms-1").fix_cycles, 0);

    // Round 1: two fixfeatures, ONE increment.
    apply(
        &mut state,
        &ev(
            7,
            EventKind::FixFeatureCreated {
                milestone_id: "ms-1".into(),
                feature: fix_feature("fx-1"),
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").fix_cycles, 1);
    assert_eq!(milestone(&state, "ms-1").status, MilestoneStatus::Active);
    apply(
        &mut state,
        &ev(
            8,
            EventKind::FixFeatureCreated {
                milestone_id: "ms-1".into(),
                feature: fix_feature("fx-2"),
            },
        ),
    )
    .unwrap();
    assert_eq!(
        milestone(&state, "ms-1").fix_cycles,
        1,
        "same round must not double-count"
    );
    let fx_ids: Vec<&str> = milestone(&state, "ms-1")
        .features
        .iter()
        .map(|f| f.id.as_str())
        .collect();
    assert_eq!(fx_ids, vec!["f-1-1", "f-1-2", "fx-1", "fx-2"]);
    assert_eq!(feature(&state, "fx-1").origin, FeatureOrigin::Fix);

    // Round 2: validating again, another fixfeature -> second increment.
    apply(
        &mut state,
        &ev(
            9,
            EventKind::MilestoneValidating {
                milestone_id: "ms-1".into(),
            },
        ),
    )
    .unwrap();
    apply(
        &mut state,
        &ev(
            10,
            EventKind::FixFeatureCreated {
                milestone_id: "ms-1".into(),
                feature: fix_feature("fx-3"),
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").fix_cycles, 2);
}

// ---------------------------------------------------------------------------
// Blocking
// ---------------------------------------------------------------------------

#[test]
fn blocked_and_unblocked_transition_milestone_and_mission() {
    let mut state = fold(&[
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
                milestone_id: "ms-1".into(),
                start_sha: "s".into(),
            },
        ),
    ])
    .unwrap();
    apply(
        &mut state,
        &ev(
            4,
            EventKind::MilestoneBlocked {
                milestone_id: "ms-1".into(),
                reason: "fix cycles exceeded".into(),
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").status, MilestoneStatus::Blocked);
    assert_eq!(state.mission.status, MissionStatus::Blocked);

    apply(
        &mut state,
        &ev(
            5,
            EventKind::MilestoneUnblocked {
                milestone_id: "ms-1".into(),
                reason: "user raised cap".into(),
                validator_guidance: None,
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").status, MilestoneStatus::Active);
    assert_eq!(state.mission.status, MissionStatus::Running);
}

#[test]
fn unblock_guidance_folds_replaces_and_clears_on_completion() {
    let mut state = fold(&[
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
                milestone_id: "ms-1".into(),
                start_sha: "s".into(),
            },
        ),
    ])
    .unwrap();

    // Guidance folds into milestone state on unblock.
    apply(
        &mut state,
        &ev(
            4,
            EventKind::MilestoneUnblocked {
                milestone_id: "ms-1".into(),
                reason: "try again".into(),
                validator_guidance: Some("FMT FIRST".into()),
            },
        ),
    )
    .unwrap();
    assert_eq!(
        milestone(&state, "ms-1").validator_guidance.as_deref(),
        Some("FMT FIRST"),
        "guidance must fold so a resumed engine can inject it"
    );

    // A later bare unblock replaces (clears) it — latest unblock wins.
    apply(
        &mut state,
        &ev(
            5,
            EventKind::MilestoneUnblocked {
                milestone_id: "ms-1".into(),
                reason: "and again".into(),
                validator_guidance: None,
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").validator_guidance, None);

    // Set once more, then completion clears it so it can never leak into a
    // later re-run.
    apply(
        &mut state,
        &ev(
            6,
            EventKind::MilestoneUnblocked {
                milestone_id: "ms-1".into(),
                reason: "third".into(),
                validator_guidance: Some("check a3".into()),
            },
        ),
    )
    .unwrap();
    apply(
        &mut state,
        &ev(
            7,
            EventKind::MilestoneCompleted {
                milestone_id: "ms-1".into(),
                tag: None,
            },
        ),
    )
    .unwrap();
    assert_eq!(milestone(&state, "ms-1").validator_guidance, None);
}

// ---------------------------------------------------------------------------
// No dead states: every status enum value reachable via events
// ---------------------------------------------------------------------------

#[test]
fn every_status_value_is_reachable() {
    let mut mission_seen: Vec<MissionStatus> = Vec::new();
    let mut milestone_seen: Vec<MilestoneStatus> = Vec::new();
    let mut feature_seen: Vec<FeatureStatus> = Vec::new();

    let kinds = vec![
        created(), // Planning
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        }, // Approved; ms/f Pending
        EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: "s".into(),
        }, // Approved -> Running; ms Active
        EventKind::FeatureStarted {
            feature_id: "f-1-1".into(),
        }, // f Active
        EventKind::FeatureCompleted {
            feature_id: "f-1-1".into(),
            commits: vec![],
        }, // f Complete
        EventKind::FeatureFailed {
            feature_id: "f-1-2".into(),
            reason: "r".into(),
        }, // f Failed
        EventKind::FeatureSkipped {
            feature_id: "f-2-1".into(),
            reason: "r".into(),
        }, // f Skipped
        EventKind::MilestoneValidating {
            milestone_id: "ms-1".into(),
        }, // ms Validating
        EventKind::MilestoneBlocked {
            milestone_id: "ms-1".into(),
            reason: "r".into(),
        }, // Blocked x2
        EventKind::MilestoneUnblocked {
            milestone_id: "ms-1".into(),
            reason: "r".into(),
            validator_guidance: None,
        },
        EventKind::MilestoneCompleted {
            milestone_id: "ms-1".into(),
            tag: None,
        }, // ms Complete
        EventKind::MissionPaused {}, // Paused
        EventKind::MissionResumed {},
        EventKind::MissionValidating {}, // Validating
        EventKind::MissionCompleted {},  // Complete
    ];

    let mut state = fold(&[ev(1, kinds[0].clone())]).unwrap();
    let record = |state: &MissionState,
                  mission_seen: &mut Vec<MissionStatus>,
                  milestone_seen: &mut Vec<MilestoneStatus>,
                  feature_seen: &mut Vec<FeatureStatus>| {
        mission_seen.push(state.mission.status);
        for m in &state.mission.milestones {
            milestone_seen.push(m.status);
            for f in &m.features {
                feature_seen.push(f.status);
            }
        }
    };
    record(
        &state,
        &mut mission_seen,
        &mut milestone_seen,
        &mut feature_seen,
    );
    for (i, kind) in kinds.iter().enumerate().skip(1) {
        apply(&mut state, &ev(i as u64 + 1, kind.clone())).unwrap();
        record(
            &state,
            &mut mission_seen,
            &mut milestone_seen,
            &mut feature_seen,
        );
    }

    // MissionStatus::Failed needs its own history (a mission ends once).
    let failed = fold_kinds(vec![
        created(),
        EventKind::MissionFailed {
            reason: "budget exhausted".into(),
        },
    ]);
    mission_seen.push(failed.mission.status);

    for status in [
        MissionStatus::Planning,
        MissionStatus::Approved,
        MissionStatus::Running,
        MissionStatus::Paused,
        MissionStatus::Blocked,
        MissionStatus::Validating,
        MissionStatus::Complete,
        MissionStatus::Failed,
    ] {
        assert!(
            mission_seen.contains(&status),
            "dead MissionStatus: {status:?}"
        );
    }
    for status in [
        MilestoneStatus::Pending,
        MilestoneStatus::Active,
        MilestoneStatus::Validating,
        MilestoneStatus::Complete,
        MilestoneStatus::Blocked,
    ] {
        assert!(
            milestone_seen.contains(&status),
            "dead MilestoneStatus: {status:?}"
        );
    }
    for status in [
        FeatureStatus::Pending,
        FeatureStatus::Active,
        FeatureStatus::Complete,
        FeatureStatus::Skipped,
        FeatureStatus::Failed,
    ] {
        assert!(
            feature_seen.contains(&status),
            "dead FeatureStatus: {status:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Approved status
// ---------------------------------------------------------------------------

#[test]
fn approved_status_folds_on_plan_approved() {
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
    ]);
    assert_eq!(state.mission.status, MissionStatus::Approved);
}

#[test]
fn plan_approved_copies_command_grants_into_mission() {
    let mut granted_plan = plan();
    granted_plan.command_grants = vec!["gc lint".to_string()];
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: granted_plan,
            base_sha: None,
        },
    ]);
    assert_eq!(state.mission.command_grants, vec!["gc lint".to_string()]);
}

#[test]
fn mission_touch_set_round_trips_through_serde() {
    let mut touchy_plan = plan();
    touchy_plan.touch_set = vec!["src/**/*.rs".to_string()];
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: touchy_plan,
            base_sha: None,
        },
    ]);
    let json = serde_json::to_value(&state.mission).unwrap();
    assert_eq!(json["touchSet"], serde_json::json!(["src/**/*.rs"]));
    let round_tripped: Mission = serde_json::from_value(json).unwrap();
    assert_eq!(round_tripped.touch_set, state.mission.touch_set);
}

#[test]
fn plan_approved_copies_touch_set_into_mission() {
    let mut touchy_plan = plan();
    touchy_plan.touch_set = vec!["src/**/*.rs".to_string()];
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: touchy_plan,
            base_sha: None,
        },
    ]);
    assert_eq!(state.mission.touch_set, vec!["src/**/*.rs".to_string()]);
}

#[test]
fn approved_status_transitions_to_running_on_milestone_started() {
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: "s".into(),
        },
    ]);
    assert_eq!(state.mission.status, MissionStatus::Running);
}

#[test]
fn approved_status_transitions_to_running_on_worker_spawned() {
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        spawn("r-1", None, None),
    ]);
    assert_eq!(state.mission.status, MissionStatus::Running);
}

#[test]
fn approved_status_full_lifecycle_still_folds_to_complete() {
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: "s".into(),
        },
        EventKind::FeatureStarted {
            feature_id: "f-1-1".into(),
        },
        EventKind::FeatureCompleted {
            feature_id: "f-1-1".into(),
            commits: vec![],
        },
        EventKind::FeatureStarted {
            feature_id: "f-1-2".into(),
        },
        EventKind::FeatureCompleted {
            feature_id: "f-1-2".into(),
            commits: vec![],
        },
        EventKind::MilestoneCompleted {
            milestone_id: "ms-1".into(),
            tag: None,
        },
        EventKind::MilestoneStarted {
            milestone_id: "ms-2".into(),
            start_sha: "s2".into(),
        },
        EventKind::FeatureStarted {
            feature_id: "f-2-1".into(),
        },
        EventKind::FeatureCompleted {
            feature_id: "f-2-1".into(),
            commits: vec![],
        },
        EventKind::MilestoneCompleted {
            milestone_id: "ms-2".into(),
            tag: None,
        },
        EventKind::MissionCompleted {},
    ]);
    assert_eq!(state.mission.status, MissionStatus::Complete);
}

#[test]
fn plan_revision_approval_merges_remaining_work() {
    let mut revised = plan();
    revised.goal = "build the revised thing".into();
    revised.milestones[0].features[1].spec = "tightened beta spec".into();
    revised.milestones[0].features.push(plan_feature("delta"));
    revised.milestones.push(PlanMilestone {
        title: "milestone three".into(),
        features: vec![plan_feature("omega")],
    });

    let mut state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: Some("base-1".into()),
        },
        EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: "sha-1".into(),
        },
        EventKind::FeatureCompleted {
            feature_id: "f-1-1".into(),
            commits: vec!["c-alpha".into()],
        },
        EventKind::PlanRevisionProposed {
            revision: 1,
            plan: revised.clone(),
            instructions: "add omega, tighten beta".into(),
        },
    ]);

    assert_eq!(state.latest_plan_revision, 1);
    assert_eq!(state.pending_revision.as_ref().map(|p| p.revision), Some(1));

    apply(
        &mut state,
        &ev(
            6,
            EventKind::PlanRevised {
                revision: 1,
                plan: revised,
            },
        ),
    )
    .unwrap();

    assert!(state.pending_revision.is_none());
    assert_eq!(state.latest_plan_revision, 1);
    assert_eq!(state.mission.goal, "build the revised thing");
    assert_eq!(feature(&state, "f-1-1").status, FeatureStatus::Complete);
    assert_eq!(feature(&state, "f-1-1").commits, vec!["c-alpha"]);
    assert_eq!(feature(&state, "f-1-2").spec, "tightened beta spec");
    assert_eq!(feature(&state, "ms-1-rev-1-1").title, "delta");
    assert_eq!(milestone(&state, "ms-3").title, "milestone three");
    assert_eq!(feature(&state, "f-3-1").title, "omega");
}

#[test]
fn plan_revision_reject_clears_pending_without_changing_plan() {
    let mut revised = plan();
    revised.goal = "do something else".into();

    let mut state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::PlanRevisionProposed {
            revision: 2,
            plan: revised,
            instructions: "change direction".into(),
        },
    ]);

    assert!(state.pending_revision.is_some());
    apply(
        &mut state,
        &ev(
            4,
            EventKind::PlanRevisionRejected {
                revision: 2,
                reason: "operator rejected".into(),
            },
        ),
    )
    .unwrap();

    assert!(state.pending_revision.is_none());
    assert_eq!(state.latest_plan_revision, 2);
    assert_eq!(state.mission.goal, "build the thing, planned");
    assert_eq!(state.mission.milestones.len(), 2);
}

#[test]
fn plan_revision_cannot_change_completed_milestones() {
    let mut revised = plan();
    revised.milestones[0].features[0].spec = "rewrite completed alpha".into();

    let events: Vec<Event> = vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::MilestoneCompleted {
            milestone_id: "ms-1".into(),
            tag: None,
        },
        EventKind::PlanRevisionProposed {
            revision: 1,
            plan: revised.clone(),
            instructions: "bad".into(),
        },
        EventKind::PlanRevised {
            revision: 1,
            plan: revised,
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(i, kind)| ev(i as u64 + 1, kind))
    .collect();
    let err = fold(&events).unwrap_err();

    assert!(matches!(err, EngineError::InvalidState(_)));
    assert!(err
        .to_string()
        .contains("alters completed milestone 'milestone one'"));
}

#[test]
fn plan_revision_tolerates_completed_milestone_whitespace() {
    // A revision that reproduces a completed milestone's feature with ONLY
    // whitespace differences (the orchestrator LLM will not echo it back
    // byte-for-byte) must fold cleanly — the pre-emit gate tolerates this, so
    // the reducer must too. Before the fix, the reducer compared these fields
    // exactly, so an accepted PlanRevised became unfoldable and bricked the
    // mission. Regression guard for the gate/reducer trim split.
    let mut revised = plan();
    let spec = revised.milestones[0].features[0].spec.clone();
    let title = revised.milestones[0].features[0].title.clone();
    revised.milestones[0].features[0].spec = format!("  {spec}\n");
    revised.milestones[0].features[0].title = format!("{title}\t");
    revised.goal = "build the revised thing".into();

    let events: Vec<Event> = vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::MilestoneCompleted {
            milestone_id: "ms-1".into(),
            tag: None,
        },
        EventKind::PlanRevisionProposed {
            revision: 1,
            plan: revised.clone(),
            instructions: "reword".into(),
        },
        EventKind::PlanRevised {
            revision: 1,
            plan: revised,
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(i, kind)| ev(i as u64 + 1, kind))
    .collect();

    let state = fold(&events).expect("whitespace-only completed-milestone diff must fold cleanly");
    assert_eq!(state.mission.goal, "build the revised thing");
    // Completed work is frozen: the ORIGINAL stored feature text is kept, not
    // the reformatted variant.
    assert_eq!(
        feature(&state, "f-1-1").spec,
        plan().milestones[0].features[0].spec
    );
}

#[test]
fn plan_revision_proposed_revision_zero_is_rejected() {
    let events: Vec<Event> = vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::PlanRevisionProposed {
            revision: 0,
            plan: plan(),
            instructions: "x".into(),
        },
    ]
    .into_iter()
    .enumerate()
    .map(|(i, kind)| ev(i as u64 + 1, kind))
    .collect();
    let err = fold(&events).unwrap_err();
    assert!(matches!(err, EngineError::InvalidState(_)));
    assert!(err.to_string().contains("must be >= 1"));
}

#[test]
fn plan_revised_mismatched_revision_is_rejected() {
    let mut state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::PlanRevisionProposed {
            revision: 1,
            plan: plan(),
            instructions: "x".into(),
        },
    ]);
    let next = state.last_seq + 1;
    let err = apply(
        &mut state,
        &ev(
            next,
            EventKind::PlanRevised {
                revision: 2,
                plan: plan(),
            },
        ),
    )
    .unwrap_err();
    assert!(matches!(err, EngineError::InvalidState(_)));
    assert!(err
        .to_string()
        .contains("does not match pending revision 1"));
}

#[test]
fn plan_revision_rejected_mismatched_revision_is_rejected() {
    let mut state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::PlanRevisionProposed {
            revision: 1,
            plan: plan(),
            instructions: "x".into(),
        },
    ]);
    let next = state.last_seq + 1;
    let err = apply(
        &mut state,
        &ev(
            next,
            EventKind::PlanRevisionRejected {
                revision: 2,
                reason: "x".into(),
            },
        ),
    )
    .unwrap_err();
    assert!(matches!(err, EngineError::InvalidState(_)));
    assert!(err
        .to_string()
        .contains("does not match pending revision 1"));
}

#[test]
fn dry_run_revised_plan_refuses_completed_milestone_rewrite() {
    // The orchestrator dry-runs the fold before it durably appends PlanRevised.
    // A revision that genuinely rewrites completed work is refused here, so the
    // unappliable event never reaches the append-only log; a whitespace-only
    // variant, by contrast, dry-runs clean.
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::MilestoneCompleted {
            milestone_id: "ms-1".into(),
            tag: None,
        },
        EventKind::PlanRevisionProposed {
            revision: 1,
            plan: plan(),
            instructions: "seed".into(),
        },
    ]);

    let mut bad = plan();
    bad.milestones[0].features[0].spec = "genuinely different work".into();
    let err = dry_run_revised_plan(&state, &bad, 1).unwrap_err();
    assert!(matches!(err, EngineError::InvalidState(_)));

    let mut ok = plan();
    let spec = ok.milestones[0].features[0].spec.clone();
    ok.milestones[0].features[0].spec = format!("{spec}\n");
    dry_run_revised_plan(&state, &ok, 1).expect("whitespace-only revision dry-runs clean");
}

#[test]
fn approved_status_guard_never_overwrites_terminal_status() {
    let mut state = fold_kinds(vec![
        created(),
        EventKind::MissionFailed {
            reason: "budget exhausted".into(),
        },
    ]);
    assert_eq!(state.mission.status, MissionStatus::Failed);

    // The guard on MilestoneStarted/WorkerSpawned only fires when the
    // mission is Approved; a terminal status must survive it untouched.
    let next_seq = state.last_seq + 1;
    apply(
        &mut state,
        &ev(
            next_seq,
            EventKind::WorkerSpawned {
                run_id: "r-after-failure".to_string(),
                role: Role::ValidatorScrutiny,
                feature_id: None,
                milestone_id: None,
                sdk_session_id: "sess-r-after-failure".to_string(),
                model: "sonnet".to_string(),
                quant: "n/a".to_string(),
                weight_hash: None,
                prompt_hash: "deadbeef".to_string(),
                transcript_path: "runs/r-after-failure.jsonl".to_string(),
            },
        ),
    )
    .unwrap();
    assert_eq!(state.mission.status, MissionStatus::Failed);
}

// ---------------------------------------------------------------------------
// Decisions and user messages
// ---------------------------------------------------------------------------

#[test]
fn decisions_cap_at_ten_and_clear_pending_messages() {
    let mut state = fold(&[ev(1, created())]).unwrap();
    let mut seq = 1;
    let mut push = |state: &mut MissionState, kind: EventKind| {
        seq += 1;
        apply(state, &ev(seq, kind)).unwrap();
    };

    push(
        &mut state,
        EventKind::UserMessage {
            text: "msg-1".into(),
            interrupt: false,
        },
    );
    push(
        &mut state,
        EventKind::UserMessage {
            text: "msg-2".into(),
            interrupt: true,
        },
    );
    assert_eq!(state.pending_user_messages, vec!["msg-1", "msg-2"]);

    push(
        &mut state,
        EventKind::OrchestratorDecision {
            summary: "d-1".into(),
            detail: Some("why".into()),
        },
    );
    assert!(
        state.pending_user_messages.is_empty(),
        "decision consumes the queue"
    );
    assert_eq!(state.recent_decisions, vec!["d-1"]);

    for i in 2..=13 {
        push(
            &mut state,
            EventKind::OrchestratorDecision {
                summary: format!("d-{i}"),
                detail: None,
            },
        );
    }
    assert_eq!(state.recent_decisions.len(), 10, "capped at 10");
    let expected: Vec<String> = (4..=13).map(|i| format!("d-{i}")).collect();
    assert_eq!(
        state.recent_decisions, expected,
        "oldest dropped, newest last"
    );
}

// ---------------------------------------------------------------------------
// Config merge
// ---------------------------------------------------------------------------

#[test]
fn config_changed_deep_merges_patch() {
    let mut state = fold(&[ev(1, created())]).unwrap();
    apply(
        &mut state,
        &ev(
            2,
            EventKind::ConfigChanged {
                patch: json!({
                    "worker": { "model": "opus" },
                    "maxParallelWorkers": 3,
                    "denyPatterns": ["git push*"],
                    "claudeBinary": "/opt/claude"
                }),
            },
        ),
    )
    .unwrap();

    // Patched leaves changed...
    assert_eq!(state.config.worker.model, "opus");
    assert_eq!(state.config.max_parallel_workers, 3);
    assert_eq!(state.config.deny_patterns, vec!["git push*"]);
    assert_eq!(state.config.claude_binary.as_deref(), Some("/opt/claude"));
    // ...sibling leaves of merged objects survive...
    assert_eq!(state.config.worker.reasoning_effort, "medium");
    assert_eq!(state.config.worker.max_turns, Some(50));
    // ...and untouched sections are untouched.
    assert_eq!(
        state.config.orchestrator,
        MissionConfig::default().orchestrator
    );

    // Non-object patch values replace wholesale (null clears an Option).
    apply(
        &mut state,
        &ev(
            3,
            EventKind::ConfigChanged {
                patch: json!({ "claudeBinary": null }),
            },
        ),
    )
    .unwrap();
    assert_eq!(state.config.claude_binary, None);
}

#[test]
fn config_changed_invalid_patch_is_config_error() {
    let mut state = fold(&[ev(1, created())]).unwrap();
    let err = apply(
        &mut state,
        &ev(
            2,
            EventKind::ConfigChanged {
                patch: json!({ "maxParallelWorkers": "many" }),
            },
        ),
    )
    .unwrap_err();
    assert!(matches!(err, EngineError::Config(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------
// Snapshots
// ---------------------------------------------------------------------------

#[test]
fn snapshot_round_trips_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");

    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::MilestoneStarted {
            milestone_id: "ms-1".into(),
            start_sha: "s".into(),
        },
        spawn("r-1", Some("f-1-1"), Some("ms-1")),
        completed("r-1", tokens(7, 3), Some(0.1)),
    ]);
    write_snapshot(&state, &path).unwrap();
    assert!(
        !path.with_file_name("state.json.tmp").exists(),
        "tmp file renamed away"
    );

    let loaded = read_snapshot(&path).unwrap();
    assert_eq!(
        serde_json::to_value(&state).unwrap(),
        serde_json::to_value(&loaded).unwrap()
    );

    // Overwrite works (rename over existing file).
    write_snapshot(&loaded, &path).unwrap();
    assert_eq!(read_snapshot(&path).unwrap().last_seq, state.last_seq);
}

// ---------------------------------------------------------------------------
// Wire format (§4.3 envelope): exact JSON shape, including key order
// ---------------------------------------------------------------------------

#[test]
fn event_wire_format_matches_plan() {
    let ts = base_ts(); // 2026-01-02T03:04:05Z

    let worker_completed = Event {
        seq: 412,
        ts,
        mission_id: "m-01".to_string(),
        kind: EventKind::WorkerCompleted {
            run_id: "r-1".to_string(),
            result: RunResult::Pass,
            tokens: TokenUsage {
                input: 10,
                output: 5,
                cache_read: 2,
                cache_write: 1,
            },
            cost_usd: Some(0.5),
            report: None,
        },
    };
    assert_eq!(
        serde_json::to_string(&worker_completed).unwrap(),
        r#"{"seq":412,"ts":"2026-01-02T03:04:05Z","missionId":"m-01","type":"worker.completed","payload":{"runId":"r-1","result":"pass","tokens":{"input":10,"output":5,"cacheRead":2,"cacheWrite":1},"costUsd":0.5}}"#
    );

    let milestone_started = Event {
        seq: 3,
        ts,
        mission_id: "m-01".to_string(),
        kind: EventKind::MilestoneStarted {
            milestone_id: "ms-1".to_string(),
            start_sha: "abc123".to_string(),
        },
    };
    assert_eq!(
        serde_json::to_string(&milestone_started).unwrap(),
        r#"{"seq":3,"ts":"2026-01-02T03:04:05Z","missionId":"m-01","type":"milestone.started","payload":{"milestoneId":"ms-1","startSha":"abc123"}}"#
    );

    let user_message = Event {
        seq: 9,
        ts,
        mission_id: "m-01".to_string(),
        kind: EventKind::UserMessage {
            text: "ship it".to_string(),
            interrupt: true,
        },
    };
    assert_eq!(
        serde_json::to_string(&user_message).unwrap(),
        r#"{"seq":9,"ts":"2026-01-02T03:04:05Z","missionId":"m-01","type":"user.message","payload":{"text":"ship it","interrupt":true}}"#
    );

    let paused = Event {
        seq: 10,
        ts,
        mission_id: "m-01".to_string(),
        kind: EventKind::MissionPaused {},
    };
    assert_eq!(
        serde_json::to_string(&paused).unwrap(),
        r#"{"seq":10,"ts":"2026-01-02T03:04:05Z","missionId":"m-01","type":"mission.paused","payload":{}}"#
    );

    // And back: the envelope deserializes to the same event.
    let parsed: Event =
        serde_json::from_str(&serde_json::to_string(&worker_completed).unwrap()).unwrap();
    assert_eq!(parsed.seq, 412);
    assert_eq!(parsed.kind.type_name(), "worker.completed");
}

// ---------------------------------------------------------------------------
// Property tests: fold == incremental apply; fold is deterministic
// ---------------------------------------------------------------------------

/// Script steps that always produce valid event sequences when interpreted
/// against the fixed 2x2 plan below.
#[derive(Debug, Clone)]
enum Action {
    MilestoneStarted(u8),
    FeatureStarted(u8),
    SpawnWorker(u8),
    SpawnValidator(u8),
    Message(u8),
    Complete(u8, u16, u16),
    FeatureCompleted(u8),
    FeatureFailed(u8),
    FeatureSkipped(u8),
    MilestoneValidating(u8),
    Finding(u8, u8),
    FixFeature(u8),
    Blocked(u8),
    Unblocked(u8),
    MilestoneCompleted(u8),
    Pause,
    Resume,
    UserMsg(String),
    Decision(String),
    ConfigPatch(u8),
    MissionValidating,
}

fn action_strategy() -> impl Strategy<Value = Action> {
    let text = || proptest::string::string_regex("[a-z0-9 ]{0,12}").unwrap();
    prop_oneof![
        any::<u8>().prop_map(Action::MilestoneStarted),
        any::<u8>().prop_map(Action::FeatureStarted),
        any::<u8>().prop_map(Action::SpawnWorker),
        any::<u8>().prop_map(Action::SpawnValidator),
        any::<u8>().prop_map(Action::Message),
        (any::<u8>(), any::<u16>(), any::<u16>()).prop_map(|(r, i, o)| Action::Complete(r, i, o)),
        any::<u8>().prop_map(Action::FeatureCompleted),
        any::<u8>().prop_map(Action::FeatureFailed),
        any::<u8>().prop_map(Action::FeatureSkipped),
        any::<u8>().prop_map(Action::MilestoneValidating),
        (any::<u8>(), any::<u8>()).prop_map(|(m, r)| Action::Finding(m, r)),
        any::<u8>().prop_map(Action::FixFeature),
        any::<u8>().prop_map(Action::Blocked),
        any::<u8>().prop_map(Action::Unblocked),
        any::<u8>().prop_map(Action::MilestoneCompleted),
        Just(Action::Pause),
        Just(Action::Resume),
        text().prop_map(Action::UserMsg),
        text().prop_map(Action::Decision),
        any::<u8>().prop_map(Action::ConfigPatch),
        Just(Action::MissionValidating),
    ]
}

/// Fixed plan for the property tests: ms-1 { f-1-1, f-1-2 }, ms-2 { f-2-1, f-2-2 }.
fn prop_plan() -> Plan {
    let feature = |t: &str| PlanFeature {
        title: t.into(),
        spec: t.into(),
        validation_criteria: vec![],
    };
    Plan {
        goal: "prop goal".into(),
        validation_contract: vec![],
        milestones: vec![
            PlanMilestone {
                title: "m1".into(),
                features: vec![feature("a"), feature("b")],
            },
            PlanMilestone {
                title: "m2".into(),
                features: vec![feature("c"), feature("d")],
            },
        ],
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec![],
    }
}

/// Interpret actions into a contiguous, always-valid event list.
fn interpret(actions: &[Action]) -> Vec<Event> {
    const MS: [&str; 2] = ["ms-1", "ms-2"];
    const FS: [&str; 4] = ["f-1-1", "f-1-2", "f-2-1", "f-2-2"];
    let patches = [
        json!({ "worker": { "model": "opus" } }),
        json!({ "maxParallelWorkers": 4 }),
        json!({ "skipScrutiny": true, "validatorFunctional": { "reasoningEffort": "high" } }),
    ];

    let mut events = vec![
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: prop_plan(),
                base_sha: None,
            },
        ),
    ];
    let mut runs: Vec<String> = Vec::new();
    let mut run_counter = 0u32;
    let mut fix_counter = 0u32;

    for action in actions {
        let seq = events.len() as u64 + 1;
        let ms = |i: u8| MS[i as usize % MS.len()].to_string();
        let fs = |i: u8| FS[i as usize % FS.len()].to_string();
        let kind = match action {
            Action::MilestoneStarted(m) => EventKind::MilestoneStarted {
                milestone_id: ms(*m),
                start_sha: "sha".into(),
            },
            Action::FeatureStarted(f) => EventKind::FeatureStarted { feature_id: fs(*f) },
            Action::SpawnWorker(f) => {
                run_counter += 1;
                let id = format!("r-{run_counter}");
                runs.push(id.clone());
                spawn(&id, Some(&fs(*f)), None)
            }
            Action::SpawnValidator(m) => {
                run_counter += 1;
                let id = format!("r-{run_counter}");
                runs.push(id.clone());
                spawn(&id, None, Some(&ms(*m)))
            }
            Action::Message(r) => {
                if runs.is_empty() {
                    continue;
                }
                let id = runs[*r as usize % runs.len()].clone();
                EventKind::WorkerMessage {
                    run_id: id,
                    tag: "text".into(),
                    content: "x".into(),
                }
            }
            Action::Complete(r, input, output) => {
                if runs.is_empty() {
                    continue;
                }
                let id = runs[*r as usize % runs.len()].clone();
                completed(
                    &id,
                    TokenUsage {
                        input: *input as u64,
                        output: *output as u64,
                        cache_read: 0,
                        cache_write: 0,
                    },
                    Some(0.01),
                )
            }
            Action::FeatureCompleted(f) => EventKind::FeatureCompleted {
                feature_id: fs(*f),
                commits: vec!["c".into()],
            },
            Action::FeatureFailed(f) => EventKind::FeatureFailed {
                feature_id: fs(*f),
                reason: "r".into(),
            },
            Action::FeatureSkipped(f) => EventKind::FeatureSkipped {
                feature_id: fs(*f),
                reason: "r".into(),
            },
            Action::MilestoneValidating(m) => EventKind::MilestoneValidating {
                milestone_id: ms(*m),
            },
            Action::Finding(m, r) => {
                if runs.is_empty() {
                    continue;
                }
                let id = runs[*r as usize % runs.len()].clone();
                EventKind::ValidationFinding {
                    milestone_id: ms(*m),
                    run_id: id,
                    finding: Finding {
                        subject: "s".into(),
                        severity: "minor".into(),
                        evidence: "e".into(),
                        suggested_fix: String::new(),
                        class: String::new(),
                    },
                }
            }
            Action::FixFeature(m) => {
                fix_counter += 1;
                EventKind::FixFeatureCreated {
                    milestone_id: ms(*m),
                    feature: fix_feature(&format!("fx-{fix_counter}")),
                }
            }
            Action::Blocked(m) => EventKind::MilestoneBlocked {
                milestone_id: ms(*m),
                reason: "r".into(),
            },
            Action::Unblocked(m) => EventKind::MilestoneUnblocked {
                milestone_id: ms(*m),
                reason: "r".into(),
                validator_guidance: None,
            },
            Action::MilestoneCompleted(m) => EventKind::MilestoneCompleted {
                milestone_id: ms(*m),
                tag: None,
            },
            Action::Pause => EventKind::MissionPaused {},
            Action::Resume => EventKind::MissionResumed {},
            Action::UserMsg(text) => EventKind::UserMessage {
                text: text.clone(),
                interrupt: false,
            },
            Action::Decision(summary) => EventKind::OrchestratorDecision {
                summary: summary.clone(),
                detail: None,
            },
            Action::ConfigPatch(i) => EventKind::ConfigChanged {
                patch: patches[*i as usize % patches.len()].clone(),
            },
            Action::MissionValidating => EventKind::MissionValidating {},
        };
        events.push(ev(seq, kind));
    }
    events
}

proptest! {
    #[test]
    fn fold_equals_incremental_apply_and_is_deterministic(
        actions in proptest::collection::vec(action_strategy(), 0..60)
    ) {
        let events = interpret(&actions);

        let folded = fold(&events).unwrap();

        let mut incremental = fold(&events[..1]).unwrap();
        for event in &events[1..] {
            apply(&mut incremental, event).unwrap();
        }

        prop_assert_eq!(
            serde_json::to_value(&folded).unwrap(),
            serde_json::to_value(&incremental).unwrap()
        );

        // Determinism: folding the same events twice yields byte-identical JSON.
        let again = fold(&events).unwrap();
        prop_assert_eq!(
            serde_json::to_string(&folded).unwrap(),
            serde_json::to_string(&again).unwrap()
        );
    }
}

/// Final-gate findings are attributed to the reserved engine run id, which
/// must fold without a matching WorkerRun (any other unknown run id refuses).
#[test]
fn validation_finding_accepts_reserved_engine_run_id() {
    let finding = |run: &str| EventKind::ValidationFinding {
        milestone_id: "ms-1".to_string(),
        run_id: run.to_string(),
        finding: Finding {
            subject: "a-1".to_string(),
            severity: "major".to_string(),
            evidence: "command failed".to_string(),
            suggested_fix: String::new(),
            class: String::new(),
        },
    };

    let events = vec![
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
        ev(3, finding(kranz_engine::reducer::ENGINE_RUN_ID)),
    ];
    fold(&events).expect("engine run id must fold cleanly");

    let events = vec![
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
        ev(3, finding("r-77")),
    ];
    assert!(matches!(fold(&events), Err(EngineError::InvalidState(_))));
}

/// A colliding fixfeature id (e.g. a second re-plan minting the same
/// `<ms>-replan-N`) is rejected loudly rather than silently shadowing.
#[test]
fn fixfeature_created_rejects_a_duplicate_feature_id() {
    let mut state = fold(&[
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
    ])
    .expect("fold base");
    let ms = state.mission.milestones[0].id.clone();

    // First fixfeature with id "dup" — accepted.
    apply(
        &mut state,
        &ev(
            3,
            EventKind::FixFeatureCreated {
                milestone_id: ms.clone(),
                feature: fix_feature("dup"),
            },
        ),
    )
    .expect("first fixfeature accepted");

    // Second fixfeature reusing the same id — must error, not shadow.
    let err = apply(
        &mut state,
        &ev(
            4,
            EventKind::FixFeatureCreated {
                milestone_id: ms.clone(),
                feature: fix_feature("dup"),
            },
        ),
    )
    .expect_err("duplicate feature id must be rejected");
    assert!(
        matches!(err, EngineError::InvalidState(_)),
        "expected InvalidState, got {err:?}"
    );

    // Only one feature with that id landed.
    let count = state.mission.milestones[0]
        .features
        .iter()
        .filter(|f| f.id == "dup")
        .count();
    assert_eq!(count, 1, "no silent duplicate");
}

// ---------------------------------------------------------------------------
// Capability grant flow (grant.requested / grant.approved / grant.denied)
// ---------------------------------------------------------------------------

/// State folded up to ms-1 Active: created → plan approved → milestone started.
fn state_at_active_milestone() -> MissionState {
    fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::MilestoneStarted {
            milestone_id: "ms-1".to_string(),
            start_sha: "sha-1".to_string(),
        },
    ])
}

#[test]
fn grant_requested_sets_pending_and_approved_extends_grants() {
    let mut state = state_at_active_milestone();
    let next = state.last_seq + 1;
    assert!(state.pending_grant_request.is_none());
    assert!(state.mission.command_grants.is_empty());

    apply(
        &mut state,
        &ev(
            next,
            EventKind::GrantRequested {
                kind: GrantKind::Command,
                milestone_id: "ms-1".to_string(),
                command: "gc lint --strict".to_string(),
            },
        ),
    )
    .expect("grant.requested accepted");
    let pending = state
        .pending_grant_request
        .clone()
        .expect("pending grant set");
    assert_eq!(pending.milestone_id, "ms-1");
    assert_eq!(pending.command, "gc lint --strict");

    apply(
        &mut state,
        &ev(
            next + 1,
            EventKind::GrantApproved {
                kind: GrantKind::Command,
                command: "gc lint --strict".to_string(),
            },
        ),
    )
    .expect("grant.approved accepted");
    assert!(state.pending_grant_request.is_none(), "pending cleared");
    assert_eq!(
        state.mission.command_grants,
        vec!["gc lint --strict".to_string()],
        "approved command extends command_grants"
    );
}

#[test]
fn grant_denied_clears_pending_without_widening_grants() {
    let mut state = state_at_active_milestone();
    let next = state.last_seq + 1;
    apply(
        &mut state,
        &ev(
            next,
            EventKind::GrantRequested {
                kind: GrantKind::Command,
                milestone_id: "ms-1".to_string(),
                command: "gc lint --strict".to_string(),
            },
        ),
    )
    .unwrap();
    apply(
        &mut state,
        &ev(
            next + 1,
            EventKind::GrantDenied {
                kind: GrantKind::Command,
                command: "gc lint --strict".to_string(),
                reason: "operator denied".to_string(),
            },
        ),
    )
    .expect("grant.denied accepted");
    assert!(state.pending_grant_request.is_none(), "pending cleared");
    assert!(
        state.mission.command_grants.is_empty(),
        "deny must never widen command_grants"
    );
}

#[test]
fn grant_approved_without_pending_is_rejected() {
    // A forged/replayed grant.approved with nothing parked must not widen.
    let mut state = state_at_active_milestone();
    let next = state.last_seq + 1;
    let err = apply(
        &mut state,
        &ev(
            next,
            EventKind::GrantApproved {
                kind: GrantKind::Command,
                command: "gc lint".to_string(),
            },
        ),
    )
    .expect_err("grant.approved with no pending must be rejected");
    assert!(matches!(err, EngineError::InvalidState(_)), "got {err:?}");
    assert!(state.mission.command_grants.is_empty());
}

#[test]
fn grant_approved_for_a_different_command_is_rejected() {
    // The command echoed on approval must match the parked request, so a
    // stale/forged approval can't apply a grant the operator never saw.
    let mut state = state_at_active_milestone();
    let next = state.last_seq + 1;
    apply(
        &mut state,
        &ev(
            next,
            EventKind::GrantRequested {
                kind: GrantKind::Command,
                milestone_id: "ms-1".to_string(),
                command: "gc lint --strict".to_string(),
            },
        ),
    )
    .unwrap();
    let err = apply(
        &mut state,
        &ev(
            next + 1,
            EventKind::GrantApproved {
                kind: GrantKind::Command,
                command: "rm -rf /".to_string(),
            },
        ),
    )
    .expect_err("mismatched grant.approved must be rejected");
    assert!(matches!(err, EngineError::InvalidState(_)), "got {err:?}");
    assert!(
        state.pending_grant_request.is_some(),
        "pending request survives a rejected approval"
    );
    assert!(state.mission.command_grants.is_empty());
}

#[test]
fn grant_requested_for_unknown_milestone_is_rejected() {
    let mut state = state_at_active_milestone();
    let next = state.last_seq + 1;
    let err = apply(
        &mut state,
        &ev(
            next,
            EventKind::GrantRequested {
                kind: GrantKind::Command,
                milestone_id: "ms-nope".to_string(),
                command: "gc lint".to_string(),
            },
        ),
    )
    .expect_err("grant.requested for an unknown milestone must be rejected");
    assert!(matches!(err, EngineError::InvalidState(_)), "got {err:?}");
    assert!(state.pending_grant_request.is_none());
}

#[test]
fn grant_requested_with_empty_command_is_rejected() {
    let mut state = state_at_active_milestone();
    let next = state.last_seq + 1;
    let err = apply(
        &mut state,
        &ev(
            next,
            EventKind::GrantRequested {
                kind: GrantKind::Command,
                milestone_id: "ms-1".to_string(),
                command: "   ".to_string(),
            },
        ),
    )
    .expect_err("empty grant command must be rejected");
    assert!(matches!(err, EngineError::InvalidState(_)), "got {err:?}");
}

#[test]
fn grant_approved_is_extend_only_and_deduped() {
    // Approving a command already granted (e.g. via the plan) is a no-op push,
    // not a duplicate.
    let mut state = state_at_active_milestone();
    state.mission.command_grants = vec!["gc lint".to_string()];
    let next = state.last_seq + 1;
    apply(
        &mut state,
        &ev(
            next,
            EventKind::GrantRequested {
                kind: GrantKind::Command,
                milestone_id: "ms-1".to_string(),
                command: "gc lint".to_string(),
            },
        ),
    )
    .unwrap();
    apply(
        &mut state,
        &ev(
            next + 1,
            EventKind::GrantApproved {
                kind: GrantKind::Command,
                command: "gc lint".to_string(),
            },
        ),
    )
    .unwrap();
    assert_eq!(
        state.mission.command_grants,
        vec!["gc lint".to_string()],
        "no duplicate grant appended"
    );
}

#[test]
fn grant_denied_folds_after_its_milestone_is_dropped_but_blocking_it_would_brick() {
    // Regression for the deny-path brick (adversarial review): a parked grant
    // references a milestone that a later plan revision can DROP. `grant.denied`
    // must still fold — it only touches `pending_grant_request`, never the
    // milestone — so denying (or the deny-default timeout) clears the request
    // cleanly. But a `MilestoneBlocked` for the now-missing milestone fails its
    // OWN reducer fold; since `emit` appends before it folds, emitting it would
    // brick the mission on every future load. That is precisely why
    // `deny_pending_grant` guards the block on the milestone still existing —
    // this test pins both halves of that invariant.
    let mut state = state_at_active_milestone(); // ms-1 (active) + ms-2 (pending)
    let next = state.last_seq + 1;
    apply(
        &mut state,
        &ev(
            next,
            EventKind::GrantRequested {
                kind: GrantKind::Command,
                milestone_id: "ms-2".to_string(),
                command: "gc audit".to_string(),
            },
        ),
    )
    .expect("grant parked for ms-2");

    // Simulate a revision dropping ms-2 (the exact effect apply_revised_plan has
    // when the revised plan has fewer milestones).
    state.mission.milestones.retain(|m| m.id != "ms-2");

    // grant.denied still folds and clears the pending request — no brick.
    apply(
        &mut state,
        &ev(
            next + 1,
            EventKind::GrantDenied {
                kind: GrantKind::Command,
                command: "gc audit".to_string(),
                reason: "timed out".to_string(),
            },
        ),
    )
    .expect("grant.denied folds even though ms-2 is gone");
    assert!(state.pending_grant_request.is_none());

    // Whereas a MilestoneBlocked for the dropped ms-2 WOULD brick — proving the
    // orchestrator's existence guard on that emit is load-bearing.
    let err = apply(
        &mut state,
        &ev(
            next + 2,
            EventKind::MilestoneBlocked {
                milestone_id: "ms-2".to_string(),
                reason: "would brick".to_string(),
            },
        ),
    )
    .expect_err("MilestoneBlocked for a dropped milestone must be rejected");
    assert!(matches!(err, EngineError::InvalidState(_)), "got {err:?}");
}

#[test]
fn touch_path_grant_extends_touch_set_not_command_grants() {
    let mut state = state_at_active_milestone();
    let next = state.last_seq + 1;
    apply(
        &mut state,
        &ev(
            next,
            EventKind::GrantRequested {
                milestone_id: "ms-1".to_string(),
                kind: GrantKind::TouchPath,
                command: "docs/report.md".to_string(),
            },
        ),
    )
    .expect("touch grant parked");
    assert_eq!(
        state.pending_grant_request.as_ref().map(|p| p.kind),
        Some(GrantKind::TouchPath)
    );

    apply(
        &mut state,
        &ev(
            next + 1,
            EventKind::GrantApproved {
                kind: GrantKind::TouchPath,
                command: "docs/report.md".to_string(),
            },
        ),
    )
    .expect("touch grant approved");
    // The approved path joins touch_set — NOT command_grants.
    assert_eq!(state.mission.touch_set, vec!["docs/report.md".to_string()]);
    assert!(state.mission.command_grants.is_empty());
    assert!(state.pending_grant_request.is_none());
}

#[test]
fn grant_approved_with_mismatched_kind_is_rejected() {
    // A forged approval that flips the kind must not apply the target to the
    // wrong list (e.g. a touch path landing in command_grants).
    let mut state = state_at_active_milestone();
    let next = state.last_seq + 1;
    apply(
        &mut state,
        &ev(
            next,
            EventKind::GrantRequested {
                milestone_id: "ms-1".to_string(),
                kind: GrantKind::TouchPath,
                command: "docs/report.md".to_string(),
            },
        ),
    )
    .unwrap();
    let err = apply(
        &mut state,
        &ev(
            next + 1,
            EventKind::GrantApproved {
                kind: GrantKind::Command,
                command: "docs/report.md".to_string(),
            },
        ),
    )
    .expect_err("kind must match the parked request");
    assert!(matches!(err, EngineError::InvalidState(_)), "got {err:?}");
    assert!(state.mission.command_grants.is_empty());
    assert!(state.mission.touch_set.is_empty());
    assert!(state.pending_grant_request.is_some());
}

#[test]
fn worker_deny_grant_extends_deny_exceptions_only() {
    let mut state = state_at_active_milestone();
    let next = state.last_seq + 1;
    apply(
        &mut state,
        &ev(
            next,
            EventKind::GrantRequested {
                milestone_id: "ms-1".to_string(),
                kind: GrantKind::WorkerDeny,
                command: "Bash(git push*)".to_string(),
            },
        ),
    )
    .expect("worker-deny grant parked");
    apply(
        &mut state,
        &ev(
            next + 1,
            EventKind::GrantApproved {
                kind: GrantKind::WorkerDeny,
                command: "Bash(git push*)".to_string(),
            },
        ),
    )
    .expect("worker-deny grant approved");
    // The lifted rule joins deny_exceptions — NOT command_grants or touch_set.
    assert_eq!(
        state.mission.deny_exceptions,
        vec!["Bash(git push*)".to_string()]
    );
    assert!(state.mission.command_grants.is_empty());
    assert!(state.mission.touch_set.is_empty());
    assert!(state.pending_grant_request.is_none());
}

// ---------------------------------------------------------------------------
// Model provenance (quant / weight_hash)
// ---------------------------------------------------------------------------

#[test]
fn provenance_backcompat_defaults_quant_and_omits_weight_hash() {
    // An old worker.spawned log line predating provenance fields has no
    // "quant" or "weightHash" keys at all.
    let spawned_json = json!({
        "type": "worker.spawned",
        "payload": {
            "runId": "r-1",
            "role": "worker",
            "featureId": "f-1-1",
            "sdkSessionId": "sess-r-1",
            "model": "sonnet",
            "promptHash": "deadbeef",
            "transcriptPath": "runs/r-1.jsonl"
        }
    });
    let kind: EventKind = serde_json::from_value(spawned_json).unwrap();
    match kind {
        EventKind::WorkerSpawned {
            quant, weight_hash, ..
        } => {
            assert_eq!(quant, "n/a");
            assert_eq!(weight_hash, None);
        }
        other => panic!("expected worker.spawned, got {other:?}"),
    }
}

#[test]
fn weight_hash_round_trips_through_serde_and_reducer_fold() {
    let with_provenance = EventKind::WorkerSpawned {
        run_id: "r-1".to_string(),
        role: Role::Worker,
        feature_id: Some("f-1-1".to_string()),
        milestone_id: None,
        sdk_session_id: "sess-r-1".to_string(),
        model: "sonnet".to_string(),
        quant: "q4_k_m".to_string(),
        weight_hash: Some("deadbeefcafef00d".to_string()),
        prompt_hash: "deadbeef".to_string(),
        transcript_path: "runs/r-1.jsonl".to_string(),
    };

    // serde round-trip.
    let json = serde_json::to_value(&with_provenance).unwrap();
    assert_eq!(json["payload"]["quant"], "q4_k_m");
    assert_eq!(json["payload"]["weightHash"], "deadbeefcafef00d");
    let back: EventKind = serde_json::from_value(json).unwrap();
    match back {
        EventKind::WorkerSpawned {
            quant, weight_hash, ..
        } => {
            assert_eq!(quant, "q4_k_m");
            assert_eq!(weight_hash, Some("deadbeefcafef00d".to_string()));
        }
        other => panic!("expected worker.spawned, got {other:?}"),
    }

    // reducer::fold round-trip onto WorkerRun.
    let state = fold(&[
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
        ev(3, with_provenance),
    ])
    .unwrap();
    let run = state.runs.get("r-1").expect("run recorded");
    assert_eq!(run.quant, "q4_k_m");
    assert_eq!(run.weight_hash, Some("deadbeefcafef00d".to_string()));
}

/// TEST-ONLY fixture standing in for a future `backend_local` spawn: given
/// stubbed GGUF file bytes, hash the content (sha256, 64 lowercase hex
/// chars) and build a `worker.spawned` `EventKind` carrying that hash and a
/// quantisation label — the shape a real local backend would emit.
///
/// This does NOT construct or exercise any real local inference backend; it
/// only proves the provenance fields fold correctly for the local regime
/// (frontier's own coverage lives in `mission_test.rs`).
fn local_worker_spawned_from_stubbed_gguf(run_id: &str, gguf_bytes: &[u8]) -> (String, EventKind) {
    let mut hasher = Sha256::new();
    hasher.update(gguf_bytes);
    let weight_hash = format!("{:x}", hasher.finalize());
    assert_eq!(weight_hash.len(), 64, "sha256 hex digest is 64 chars");

    let kind = EventKind::WorkerSpawned {
        run_id: run_id.to_string(),
        role: Role::Worker,
        feature_id: Some("f-1-1".to_string()),
        milestone_id: None,
        sdk_session_id: format!("sess-{run_id}"),
        model: "local-llama-3-8b".to_string(),
        quant: "q4_k_m".to_string(),
        weight_hash: Some(weight_hash.clone()),
        prompt_hash: "deadbeef".to_string(),
        transcript_path: format!("runs/{run_id}.jsonl"),
    };
    (weight_hash, kind)
}

#[test]
fn local_fixture_weight_hash_pins_stubbed_gguf_content_onto_run() {
    // Stand-in "GGUF" content — bytes are irrelevant beyond being stable and
    // non-empty; a real local backend would hash the actual weights file.
    let stubbed_gguf = b"GGUF\x00\x00\x00\x03fake-local-weights-for-provenance-test";
    let (weight_hash, spawned) = local_worker_spawned_from_stubbed_gguf("r-local-1", stubbed_gguf);

    let state = fold(&[
        ev(1, created()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan(),
                base_sha: None,
            },
        ),
        ev(3, spawned),
    ])
    .unwrap();

    let run = state.runs.get("r-local-1").expect("run recorded");
    assert_eq!(run.quant, "q4_k_m", "quantisation label preserved");
    assert_eq!(
        run.weight_hash,
        Some(weight_hash.clone()),
        "content hash pins the exact weights used — a bare model name would not be auditable"
    );
    assert_eq!(run.weight_hash.as_ref().unwrap().len(), 64);

    // Hashing the SAME content again reproduces the SAME hash (content-
    // addressed, not incidental) — different content must NOT collide.
    let (same_hash_again, _) = local_worker_spawned_from_stubbed_gguf("r-local-2", stubbed_gguf);
    assert_eq!(same_hash_again, weight_hash);
    let (different_hash, _) =
        local_worker_spawned_from_stubbed_gguf("r-local-3", b"different stubbed weights");
    assert_ne!(different_hash, weight_hash);
}

// ---------------------------------------------------------------------------
// tier.escalated: flip executor tier, reset Worker backend, keep milestone active
// ---------------------------------------------------------------------------

#[test]
fn tier_escalation_flips_tier_resets_worker_and_reactivates_milestone() {
    let mut state = fold(&[
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
                milestone_id: "ms-1".into(),
                start_sha: "s".into(),
            },
        ),
    ])
    .unwrap();

    // Route the Worker onto the local tier and drive the milestone into a
    // failed-validation round, mirroring what two stuck local validations
    // would have left behind before escalation.
    state.config.worker.backend = Some("local".to_string());
    state.config.worker.base_url = Some("http://localhost:8080".to_string());
    state.config.worker.context_budget = Some(8192);
    state.config.worker.temperature = Some(0.2);
    assert_eq!(state.executor_tier(), ExecutorTier::Local);
    {
        let ms = state
            .mission
            .milestones
            .iter_mut()
            .find(|m| m.id == "ms-1")
            .unwrap();
        ms.status = MilestoneStatus::Validating;
        ms.fix_cycles = 2;
    }
    // Validator role configs must be untouched by the escalation fold.
    let validator_scrutiny_before = state.config.validator_scrutiny.clone();
    let validator_functional_before = state.config.validator_functional.clone();

    apply(
        &mut state,
        &ev(
            4,
            EventKind::TierEscalated {
                milestone_id: "ms-1".into(),
                from: ExecutorTier::Local,
                to: ExecutorTier::Frontier,
                reason: "two failed local validations".into(),
            },
        ),
    )
    .unwrap();

    assert_eq!(state.executor_tier(), ExecutorTier::Frontier);
    assert_eq!(state.config.worker.backend, None);
    assert_eq!(state.config.worker.base_url, None);
    assert_eq!(state.config.worker.context_budget, None);
    assert_eq!(state.config.worker.temperature, None);

    let ms = milestone(&state, "ms-1");
    assert_eq!(ms.status, MilestoneStatus::Active);
    assert_eq!(ms.fix_cycles, 0);

    assert_eq!(state.escalated_milestones, 1);
    assert_eq!(state.config.validator_scrutiny, validator_scrutiny_before);
    assert_eq!(
        state.config.validator_functional,
        validator_functional_before
    );
}

#[test]
fn tier_escalation_defaults_on_old_logs_without_the_event() {
    let state = fold(&[
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
                milestone_id: "ms-1".into(),
                start_sha: "s".into(),
            },
        ),
    ])
    .unwrap();

    assert_eq!(state.executor_tier(), ExecutorTier::Frontier);
    assert_eq!(state.escalated_milestones, 0);
}

// ---------------------------------------------------------------------------
// escalation_rate: derived from local_executor_milestones + escalated_milestones
// ---------------------------------------------------------------------------

/// Plan with 3 milestones so a milestone can start before, at, and after an
/// escalation: ms-1 { f-1-1 }, ms-2 { f-2-1 }, ms-3 { f-3-1 }.
fn plan_three_milestones() -> Plan {
    Plan {
        goal: "build the thing, planned".to_string(),
        validation_contract: vec![Assertion {
            id: "a-1".to_string(),
            statement: "cargo test passes".to_string(),
            check: AssertionCheck::Command,
            command: Some("cargo test".to_string()),
        }],
        milestones: vec![
            PlanMilestone {
                title: "milestone one".to_string(),
                features: vec![plan_feature("alpha")],
            },
            PlanMilestone {
                title: "milestone two".to_string(),
                features: vec![plan_feature("beta")],
            },
            PlanMilestone {
                title: "milestone three".to_string(),
                features: vec![plan_feature("gamma")],
            },
        ],
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec![],
    }
}

fn created_with_local_worker() -> EventKind {
    let mut config = MissionConfig::default();
    config.worker.backend = Some("local".to_string());
    EventKind::MissionCreated {
        goal: "build the thing".to_string(),
        base_branch: "main".to_string(),
        mission_branch: format!("kranz/mission-{MISSION}"),
        config,
    }
}

#[test]
fn escalation_rate_counts_local_starts_and_one_escalation() {
    let state = fold(&[
        ev(1, created_with_local_worker()),
        ev(
            2,
            EventKind::PlanApproved {
                plan: plan_three_milestones(),
                base_sha: None,
            },
        ),
        // ms-1 and ms-2 start while the Worker is still routed to Local.
        ev(
            3,
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".into(),
                start_sha: "s1".into(),
            },
        ),
        ev(
            4,
            EventKind::MilestoneStarted {
                milestone_id: "ms-2".into(),
                start_sha: "s2".into(),
            },
        ),
        // ms-1 gets stuck and escalates after two failed local validations.
        ev(
            5,
            EventKind::TierEscalated {
                milestone_id: "ms-1".into(),
                from: ExecutorTier::Local,
                to: ExecutorTier::Frontier,
                reason: "two failed local validations".into(),
            },
        ),
        // ms-3 starts AFTER escalation, so the Worker is now Frontier — it
        // must not be counted in the local-executor denominator.
        ev(
            6,
            EventKind::MilestoneStarted {
                milestone_id: "ms-3".into(),
                start_sha: "s3".into(),
            },
        ),
    ])
    .unwrap();

    assert_eq!(state.local_executor_milestones, 2);
    assert_eq!(state.escalated_milestones, 1);
    assert_eq!(state.escalation_rate(), 0.5);
}

#[test]
fn escalation_rate_is_zero_when_mission_never_routes_local() {
    let state = fold(&[
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
                milestone_id: "ms-1".into(),
                start_sha: "s".into(),
            },
        ),
    ])
    .unwrap();

    assert_eq!(state.local_executor_milestones, 0);
    assert_eq!(state.escalated_milestones, 0);
    assert_eq!(state.escalation_rate(), 0.0);
}

// ---------------------------------------------------------------------------
// WorkspaceProvider seam lifecycle events (design D-B/D-E, ticket
// workspace-provider-seam)
// ---------------------------------------------------------------------------

/// The three additive workspace.* events fold: provisioned records the
/// provider kind on state (last provision wins), readiness/teardown are
/// audit-only and leave the mission status machine untouched.
#[test]
fn workspace_provider_events_fold_into_state() {
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
        EventKind::WorkspaceProvisioned {
            provider: "local-worktree".to_string(),
            cwd: "/tmp/m-1_integration".to_string(),
            detail: None,
        },
        EventKind::WorkspaceReadinessReport {
            outcome: "ready".to_string(),
            detail: None,
        },
        EventKind::WorkspaceTeardown {
            mode: "keep".to_string(),
        },
        // A resume re-provisions: the last provisioned provider kind wins.
        EventKind::WorkspaceProvisioned {
            provider: "local-worktree".to_string(),
            cwd: "/tmp/m-1_integration".to_string(),
            detail: None,
        },
        EventKind::WorkspaceReadinessReport {
            outcome: "failed".to_string(),
            detail: Some("workspace gate: readiness check 1/1 failed".to_string()),
        },
    ]);

    assert_eq!(state.workspace_provider.as_deref(), Some("local-worktree"));
    // Audit-only: the readiness failure report does NOT block anything by
    // itself (the milestone.blocked event does that, as before).
    assert_eq!(state.mission.status, MissionStatus::Approved);
    assert_eq!(milestone(&state, "ms-1").status, MilestoneStatus::Pending);
}

/// Old logs/state snapshots (written before the provider seam) still fold:
/// `workspaceProvider` absent from state.json deserializes to None, and a
/// folded state never carries a provider until the first
/// workspace.provisioned.
#[test]
fn workspace_provider_state_field_backcompat_with_pre_seam_snapshots() {
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
    ]);
    assert_eq!(state.workspace_provider, None);

    // A pre-seam state.json has no workspaceProvider key at all; it must
    // still deserialize (additive #[serde(default)] field) — and the new
    // binary omits the key again while the value is None, so snapshots stay
    // byte-identical until the seam first fires.
    let value = serde_json::to_value(&state).unwrap();
    assert!(
        !value.as_object().unwrap().contains_key("workspaceProvider"),
        "absent while None: {value}"
    );
    let parsed: MissionState = serde_json::from_value(value).unwrap();
    assert_eq!(parsed.workspace_provider, None);
}

// ---------------------------------------------------------------------------
// Workspace provider pin at approval (design D-B, ticket
// workspace-provider-pin-at-approval)
// ---------------------------------------------------------------------------

/// `workspace.provider.pinned` folds into `state.workspace_pin`; a retried
/// approval re-pins (last pin wins).
#[test]
fn workspace_provider_pin_folds_into_state() {
    let state = fold_kinds(vec![
        created(),
        EventKind::WorkspaceProviderPinned {
            provider: "local-worktree".to_string(),
            template: "worktree".to_string(),
            version: "1".to_string(),
        },
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
    ]);

    assert_eq!(
        state.workspace_pin,
        Some(WorkspacePin {
            provider: "local-worktree".to_string(),
            template: "worktree".to_string(),
            version: "1".to_string(),
        })
    );
    assert_eq!(state.mission.status, MissionStatus::Approved);

    // A retried approval (first attempt failed after emitting the pin)
    // re-pins: last pin wins.
    let state = fold_kinds(vec![
        created(),
        EventKind::WorkspaceProviderPinned {
            provider: "local-worktree".to_string(),
            template: "worktree".to_string(),
            version: "none".to_string(),
        },
        EventKind::WorkspaceProviderPinned {
            provider: "local-worktree".to_string(),
            template: "worktree".to_string(),
            version: "1".to_string(),
        },
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
    ]);
    assert_eq!(
        state.workspace_pin.map(|p| p.version),
        Some("1".to_string())
    );
}

/// Old logs (written before the pin event existed) fold to
/// `workspace_pin: None`, and a pre-pin state.json has no `workspacePin` key
/// at all — the additive serde-default field deserializes to None and the new
/// binary omits the key again while the value is None, so snapshots stay
/// byte-identical until the pin first fires.
#[test]
fn workspace_provider_pin_backcompat_with_pre_pin_logs_and_snapshots() {
    let state = fold_kinds(vec![
        created(),
        EventKind::PlanApproved {
            plan: plan(),
            base_sha: None,
        },
    ]);
    assert_eq!(state.workspace_pin, None);

    let value = serde_json::to_value(&state).unwrap();
    assert!(
        !value.as_object().unwrap().contains_key("workspacePin"),
        "absent while None: {value}"
    );
    let parsed: MissionState = serde_json::from_value(value).unwrap();
    assert_eq!(parsed.workspace_pin, None);

    // A present pin serializes under the camelCase wire name.
    let state = fold_kinds(vec![
        created(),
        EventKind::WorkspaceProviderPinned {
            provider: "local-worktree".to_string(),
            template: "checkout".to_string(),
            version: "none".to_string(),
        },
    ]);
    let value = serde_json::to_value(&state).unwrap();
    assert_eq!(
        value["workspacePin"],
        json!({"provider": "local-worktree", "template": "checkout", "version": "none"}),
        "{value}"
    );
}
