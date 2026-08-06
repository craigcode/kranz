//! Report-renderer structure tests (report-as-explain-diff): the report
//! opens with goal + plan summary before any statistics, narrates features
//! with intent and evidence inline, and keeps the validation/contract
//! sections in their established order — all deterministic from the log.

use chrono::{TimeZone, Utc};
use kranz_engine::events::{Event, EventKind};
use kranz_engine::reducer;
use kranz_engine::report_render::render_mission_report;
use kranz_engine::types::{
    Assertion, AssertionCheck, MissionConfig, Plan, PlanFeature, PlanMilestone,
};

fn ev(seq: u64, kind: EventKind) -> Event {
    Event {
        seq,
        ts: Utc.with_ymd_and_hms(2026, 7, 24, 12, 0, 0).unwrap(),
        mission_id: "m-test".into(),
        kind,
    }
}

fn plan() -> Plan {
    Plan {
        goal: "add the widget and prove it works".into(),
        validation_contract: vec![
            Assertion {
                id: "a-1".into(),
                statement: "the build succeeds".into(),
                check: AssertionCheck::Command,
                command: Some("cargo test --workspace".into()),
                pty_script: None,
            },
            Assertion {
                id: "a-2".into(),
                statement: "the widget reads honestly".into(),
                check: AssertionCheck::AgentJudgement,
                command: None,
                pty_script: None,
            },
        ],
        milestones: vec![PlanMilestone {
            title: "Ship the widget".into(),
            features: vec![
                PlanFeature {
                    title: "build the widget".into(),
                    spec: "Add a Widget struct that renders the count. It must not allocate."
                        .into(),
                    validation_criteria: vec!["cargo test widget passes".into()],
                },
                PlanFeature {
                    title: "document the widget".into(),
                    spec: "One paragraph in the README.".into(),
                    validation_criteria: vec!["README mentions the widget".into()],
                },
            ],
        }],
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec![],
    }
}

fn completed_state() -> kranz_engine::types::MissionState {
    let p = plan();
    let events = vec![
        ev(
            1,
            EventKind::MissionCreated {
                goal: p.goal.clone(),
                base_branch: "main".into(),
                mission_branch: "kranz/mission-m-test".into(),
                config: MissionConfig::default(),
            },
        ),
        ev(
            2,
            EventKind::PlanApproved {
                plan: p,
                base_sha: None,
            },
        ),
        ev(
            3,
            EventKind::MilestoneStarted {
                milestone_id: "ms-1".into(),
                start_sha: "abc".into(),
            },
        ),
        ev(
            4,
            EventKind::FeatureStarted {
                feature_id: "f-1-1".into(),
            },
        ),
        ev(
            5,
            EventKind::FeatureCompleted {
                feature_id: "f-1-1".into(),
                commits: vec!["0123456789abcdef [f-1-1] add Widget struct".into()],
            },
        ),
        ev(
            6,
            EventKind::FeatureStarted {
                feature_id: "f-1-2".into(),
            },
        ),
        ev(
            7,
            EventKind::FeatureCompleted {
                feature_id: "f-1-2".into(),
                commits: vec!["1123456789abcdef [f-1-2] document the widget".into()],
            },
        ),
        ev(
            8,
            EventKind::MilestoneCompleted {
                milestone_id: "ms-1".into(),
                tag: None,
            },
        ),
        ev(9, EventKind::MissionCompleted {}),
    ];
    reducer::fold(&events).unwrap()
}

fn estimate() -> kranz_engine::cost::CostEstimate {
    kranz_engine::cost::CostEstimate {
        worker_runs: 2.0,
        validator_runs: 2.0,
        low_usd: 1.0,
        expected_usd: 2.0,
        high_usd: 5.0,
        shape: kranz_engine::cost::MissionShape::Unknown,
        confidence: kranz_engine::cost::Confidence::High,
    }
}

#[test]
fn report_opens_with_goal_and_plan_summary_before_statistics() {
    let state = completed_state();
    let report = render_mission_report(
        &state,
        &[],
        &plan(),
        &estimate(),
        std::path::Path::new("/tmp"),
        None,
    );

    // Section order: goal → plan summary → statistics → shipped → validation
    // → contract outcomes (the explain-diff shape: intent before details).
    let ordered = [
        "**Goal:**",
        "## The plan",
        "**Elapsed:**",
        "## What shipped",
        "## Validation history",
        "## Contract outcomes",
    ];
    let mut cursor = 0;
    for marker in ordered {
        let at = report
            .find(marker)
            .unwrap_or_else(|| panic!("missing section {marker:?}:\n{report}"));
        assert!(at >= cursor, "{marker:?} out of order:\n{report}");
        cursor = at;
    }

    assert!(
        report.contains(
            "1 milestone, 2 features, gated by 2 contract assertions (1 command, 1 judgement)"
        ),
        "plan summary line:\n{report}"
    );
}

#[test]
fn report_narrates_features_with_intent_and_evidence_inline() {
    let state = completed_state();
    let report = render_mission_report(
        &state,
        &[],
        &plan(),
        &estimate(),
        std::path::Path::new("/tmp"),
        None,
    );

    // Intent prose (the spec's first sentence) follows the feature headline.
    assert!(
        report.contains("Add a Widget struct that renders the count."),
        "intent line present:\n{report}"
    );
    // Evidence inline: commit and the met criterion for the complete feature.
    assert!(
        report.contains("`0123456` [f-1-1] add Widget struct"),
        "{report}"
    );
    assert!(report.contains("- ✓ cargo test widget passes"), "{report}");
    // Contract verdicts carry the actual command.
    assert!(
        report.contains("- ✅ **[a-1]** the build succeeds *(command: `cargo test --workspace`)*"),
        "{report}"
    );
}

/// D-H operator language: the Workspace section must say when only source
/// isolation is active versus a workspace contract being present.
#[test]
fn report_workspace_section_states_contract_presence() {
    let state = completed_state();

    let without = render_mission_report(
        &state,
        &[],
        &plan(),
        &estimate(),
        std::path::Path::new("/tmp"),
        None,
    );
    assert!(
        without.contains("- **Workspace contract:** no workspace contract (source isolation only)"),
        "{without}"
    );
    assert!(
        !without.contains("- **Bootstrap:**") && !without.contains("- **Readiness:**"),
        "no contract ⇒ no gate outcome lines (never imply a runnable environment): {without}"
    );

    let contract = kranz_engine::workspace_contract::parse_workspace_contract(
        br#"{
            "schemaVersion": 1,
            "services": [
                {"name": "api", "start": "cargo run", "port": {"policy": "dynamic"}},
                {"name": "db", "start": "docker compose up db", "port": {"policy": {"fixed": 5432}}}
            ],
            "previews": [{"name": "app", "urlTemplate": "http://localhost:{port}/"}]
        }"#,
    )
    .expect("valid contract");
    let with = render_mission_report(
        &state,
        &[],
        &plan(),
        &estimate(),
        std::path::Path::new("/tmp"),
        Some(&contract),
    );
    assert!(
        with.contains("- **Workspace contract:** present (2 services, 1 previews)"),
        "{with}"
    );
    // Contract present but the gate never ran (no decisions on the log):
    // say so honestly rather than implying readiness.
    assert!(with.contains("- **Bootstrap:** not run yet"), "{with}");
    assert!(with.contains("- **Readiness:** not run yet"), "{with}");
}

/// D-C/D-H: with a contract, the Workspace section renders the gate's
/// outcomes from its `orchestrator.decision` events — pass and failure
/// shapes alike, latest run wins.
#[test]
fn report_workspace_section_renders_bootstrap_readiness_outcomes() {
    let state = completed_state();
    let contract = kranz_engine::workspace_contract::parse_workspace_contract(
        br#"{"schemaVersion": 1, "bootstrap": ["cargo fetch"], "readiness": ["pg_isready"]}"#,
    )
    .expect("valid contract");
    let decision = |seq, summary: &str| {
        ev(
            seq,
            EventKind::OrchestratorDecision {
                summary: summary.into(),
                detail: None,
            },
        )
    };

    // Passing run: both phases report ok.
    let events = vec![
        decision(10, "workspace bootstrap: running 1 commands"),
        decision(11, "workspace bootstrap: 1/1 commands ok"),
        decision(12, "workspace readiness: running 1 checks"),
        decision(13, "workspace readiness: 1/1 checks ok"),
    ];
    let report = render_mission_report(
        &state,
        &events,
        &plan(),
        &estimate(),
        std::path::Path::new("/tmp"),
        Some(&contract),
    );
    assert!(
        report.contains("- **Bootstrap:** 1/1 commands ok"),
        "{report}"
    );
    assert!(
        report.contains("- **Readiness:** 1/1 checks ok"),
        "{report}"
    );

    // A later failing run supersedes the earlier pass (latest-wins), and
    // the failure copy names the phase and owner.
    let events = vec![
        decision(10, "workspace bootstrap: 1/1 commands ok"),
        decision(
            20,
            "workspace bootstrap: FAILED at command 1/1 — blocking mission (owner: repo-setup)",
        ),
        decision(21, "workspace readiness: 1/1 checks ok"),
    ];
    let report = render_mission_report(
        &state,
        &events,
        &plan(),
        &estimate(),
        std::path::Path::new("/tmp"),
        Some(&contract),
    );
    assert!(
        report.contains(
            "- **Bootstrap:** FAILED at command 1/1 — blocking mission (owner: repo-setup)"
        ),
        "{report}"
    );
    assert!(
        report.contains("- **Readiness:** 1/1 checks ok"),
        "{report}"
    );
}

/// D-B/D-E: the Workspace section renders the approval-time provider pin —
/// local-worktree named as source isolation (never a runnable workspace,
/// D-H), the contract's schema version when a contract exists, and the
/// honest no-contract variant otherwise. Missions approved before pinning
/// existed render no Provider line at all (never fabricated).
#[test]
fn report_workspace_section_renders_provider_pin() {
    use kranz_engine::types::WorkspacePin;

    // Pin with a contract: schema version named.
    let mut state = completed_state();
    state.workspace_pin = Some(WorkspacePin {
        provider: "local-worktree".into(),
        template: "worktree".into(),
        version: "1".into(),
    });
    let report = render_mission_report(
        &state,
        &[],
        &plan(),
        &estimate(),
        std::path::Path::new("/tmp"),
        None,
    );
    assert!(
        report.contains(
            "- **Provider:** local-worktree (source isolation) · template: worktree · contract schema v1"
        ),
        "{report}"
    );

    // Pin without a contract: the honest no-contract variant.
    let mut state = completed_state();
    state.workspace_pin = Some(WorkspacePin {
        provider: "local-worktree".into(),
        template: "checkout".into(),
        version: "none".into(),
    });
    let report = render_mission_report(
        &state,
        &[],
        &plan(),
        &estimate(),
        std::path::Path::new("/tmp"),
        None,
    );
    assert!(
        report.contains(
            "- **Provider:** local-worktree (source isolation) · template: checkout · no workspace contract"
        ),
        "{report}"
    );

    // No pin (mission approved before pinning existed): no Provider line —
    // the report never fails and never fabricates one.
    let state = completed_state();
    assert_eq!(state.workspace_pin, None);
    let report = render_mission_report(
        &state,
        &[],
        &plan(),
        &estimate(),
        std::path::Path::new("/tmp"),
        None,
    );
    assert!(
        !report.contains("- **Provider:**"),
        "absent pin ⇒ no Provider line: {report}"
    );
}
