//! End-to-end tests of the mission engine loop (plan §4.5), driven entirely
//! through the mock backend and throwaway git repositories.
//!
//! ## How mock scripts line up with the engine
//!
//! `MockBackend` pops scripts FIFO on every `AgentBackend::start`, so each
//! test lists its scripts in exact session-start order (workers/validators
//! are single-shot; the orchestrator is one streaming session).
//!
//! The orchestrator protocol (see orchestrator.rs module docs): the *seed* is
//! the streaming initial prompt, and the engine pumps one `Result` for it at
//! session start — hence every orchestrator script begins with
//! `[mock_init, mock_result_text("ready")]`. After that, every engine turn is
//! one injected message consuming exactly one `on_message` batch, and each
//! batch must end in a `Result` (`[mock_text(reply), mock_result_text(reply)]`).
//! A missing batch parks the session forever — which is exactly what the
//! kill/resume test exploits (with a short stall timeout).
//!
//! All tests skip cleanly when `git` is not on PATH.

use kranz_engine::backend::{AgentBackend, PromptMode, SessionExit};
use kranz_engine::backend_mock::{mock_init, mock_result_text, mock_text, MockBackend, MockScript};
use kranz_engine::control;
use kranz_engine::event_log::EventLog;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::orchestrator::{MissionEngine, PlanRequest};
use kranz_engine::paths::MissionPaths;
use kranz_engine::reducer;
use kranz_engine::types::*;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Once};
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

/// Generous bound proving the engine loop cannot hang in tests.
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Git fixtures (same isolation discipline as git_ops_test.rs)
// ---------------------------------------------------------------------------

static ENV_ISOLATION: Once = Once::new();

/// Mask the host's global/system git config so identity, signing and hooks
/// never leak into the throwaway repos.
fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing = std::env::temp_dir()
            .join(format!("kranz-mission-test-no-config-{}", std::process::id()));
        std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
        std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
        if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
            std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
        }
    });
}

fn git_available() -> bool {
    Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// Returns false (after a skip note) when git is missing.
fn setup() -> bool {
    isolate_git_env();
    if git_available() {
        true
    } else {
        eprintln!("skipping test: git is not on PATH");
        false
    }
}

/// Run git directly (test plumbing, independent of the code under test).
fn raw_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git").args(args).current_dir(dir).output().expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Fresh repo on branch `main` with one seed commit. The returned root is
/// canonicalized (macOS tempdirs are symlinks; git pathspecs need realpaths).
fn init_repo() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let init = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .expect("spawn git init");
    if !init.status.success() {
        // Older git without `init -b`.
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
// Engine / plan / script fixtures
// ---------------------------------------------------------------------------

const GOAL: &str = "ship the demo feature";

/// Baseline test config: both validators off (individual tests re-enable the
/// functional validator where the scenario needs a validation round).
fn test_cfg() -> MissionConfig {
    MissionConfig { skip_scrutiny: true, skip_functional: true, ..MissionConfig::default() }
}

fn make_engine(backend: &Arc<MockBackend>, root: &Path, cfg: MissionConfig) -> MissionEngine {
    let backend: Arc<dyn AgentBackend> = Arc::clone(backend) as Arc<dyn AgentBackend>;
    MissionEngine::create(backend, root, GOAL, cfg).expect("create mission engine")
}

/// A plan with one milestone ("M1") of `features` features.
fn simple_plan(features: usize, contract: Vec<Assertion>) -> Plan {
    Plan {
        goal: GOAL.to_string(),
        validation_contract: contract,
        milestones: vec![PlanMilestone {
            title: "M1".to_string(),
            features: (1..=features)
                .map(|i| PlanFeature {
                    title: format!("feature {i}"),
                    spec: format!("build part {i}"),
                    validation_criteria: vec![format!("part {i} works")],
                })
                .collect(),
        }],
    }
}

/// Worker script: completed single-shot run whose final text is a passing
/// WorkerReport (no commits made — the mock cannot touch the repo).
fn worker_pass() -> MockScript {
    MockScript::single_shot_json(&json!({
        "result": "pass",
        "summary": "implemented and tested",
        "filesTouched": [],
        "testsAdded": [],
        "testEvidence": "all green",
        "commits": []
    }))
}

/// Worker script: completed run whose report says result "fail".
fn worker_fail() -> MockScript {
    MockScript::single_shot_json(&json!({
        "result": "fail",
        "summary": "could not make the tests pass",
    }))
}

/// Validator script returning the given findings.
fn validator_with(findings: serde_json::Value) -> MockScript {
    MockScript::single_shot_json(&json!({ "findings": findings, "summary": "validated" }))
}

/// The orchestrator streaming script. `events` covers the SEED turn (the
/// engine pumps one Result at session start); each entry of `replies` is one
/// engine turn, in order, released by one injected message.
fn orch_script(replies: Vec<String>) -> MockScript {
    MockScript::streaming(vec![mock_init("orch-session"), mock_result_text("ready")]).responding(
        replies
            .iter()
            .map(|reply| vec![mock_text(reply), mock_result_text(reply)])
            .collect(),
    )
}

/// Judgement-turn reply (§4.5 f).
fn judgement(decision: &str, guidance: &str) -> String {
    json!({ "decision": decision, "guidance": guidance, "summary": format!("worker judged: {decision}") })
        .to_string()
}

/// Conversion-turn reply with `n` fix features (§4.5 g). Deliberately the
/// OLD shape — no "waived" key — proving pre-waive answers still parse
/// (serde defaults the field).
fn fix_features(n: usize) -> String {
    let features: Vec<serde_json::Value> = (1..=n)
        .map(|i| {
            json!({
                "title": format!("fix issue {i}"),
                "spec": format!("resolve validation finding {i}"),
                "validationCriteria": [format!("finding {i} resolved")]
            })
        })
        .collect();
    json!({ "fixFeatures": features, "summary": format!("{n} fix feature(s)") }).to_string()
}

/// Conversion-turn reply that waives the one finding instead of fixing it.
fn waive_reply(subject: &str, reason: &str) -> String {
    json!({
        "fixFeatures": [],
        "waived": [{ "subject": subject, "reason": reason }],
        "summary": "not worth a fix round"
    })
    .to_string()
}

/// Final-gate verdicts reply: every listed assertion id passes (§4.5 h).
fn verdicts_pass(ids: &[&str]) -> String {
    let verdicts: Vec<serde_json::Value> = ids
        .iter()
        .map(|id| json!({ "id": id, "pass": true, "evidence": "verified" }))
        .collect();
    json!({ "verdicts": verdicts, "summary": "all assertions hold" }).to_string()
}

fn assertion(id: &str, statement: &str, command: Option<&str>) -> Assertion {
    Assertion {
        id: id.to_string(),
        statement: statement.to_string(),
        check: if command.is_some() {
            AssertionCheck::Command
        } else {
            AssertionCheck::AgentJudgement
        },
        command: command.map(str::to_string),
    }
}

// ---------------------------------------------------------------------------
// Log helpers
// ---------------------------------------------------------------------------

fn read_log(paths: &MissionPaths) -> Vec<Event> {
    EventLog::read_events(&paths.events_file()).expect("read events.jsonl")
}

fn event_types(events: &[Event]) -> Vec<&'static str> {
    events.iter().map(|e| e.kind.type_name()).collect()
}

/// Seq of the first event of the given type (panics when absent).
fn seq_of(events: &[Event], type_name: &str) -> u64 {
    events
        .iter()
        .find(|e| e.kind.type_name() == type_name)
        .unwrap_or_else(|| panic!("no {type_name} event in {:?}", event_types(events)))
        .seq
}

// ---------------------------------------------------------------------------
// 1. Happy path: plan → features → validation → tag → final gate → complete
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn happy_path_completes_mission_with_tag_and_contract_gate() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Contract: one engine-run command assertion (`cd .` succeeds under both
    // `sh -c` and `cmd /C`) and one agent-judgement assertion.
    let contract = vec![
        assertion("a-1", "the build command succeeds", Some("cd .")),
        assertion("a-2", "error messages are actionable", None),
    ];

    // Session-start order:
    //   1. worker f-1-1            (single-shot, passing report)
    //   2. orchestrator            (streaming; first needed for judgement #1)
    //   3. worker f-1-2            (single-shot, passing report)
    //   4. functional validator    (no findings → milestone tag)
    // Orchestrator turns, in order:
    //   seed (from `events`), judgement f-1-1, judgement f-1-2, gate verdicts.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            judgement("complete", ""),
            judgement("complete", ""),
            verdicts_pass(&["a-2"]),
        ]),
        worker_pass(),
        validator_with(json!([])),
    ]));

    let cfg = MissionConfig { skip_functional: false, ..test_cfg() };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(2, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();
    drop(engine); // flush + release the lock before reading the log

    let events = read_log(&paths);
    let types = event_types(&events);
    for expected in [
        "mission.created",
        "plan.approved",
        "milestone.started",
        "feature.started",
        "worker.spawned",
        "worker.completed",
        "orchestrator.decision",
        "feature.completed",
        "milestone.validating",
        "milestone.completed",
        "mission.validating",
        "mission.completed",
    ] {
        assert!(types.contains(&expected), "missing {expected}: {types:?}");
    }
    // Lifecycle ordering: milestone tag before the gate, gate before complete.
    assert!(seq_of(&events, "milestone.completed") < seq_of(&events, "mission.validating"));
    assert!(seq_of(&events, "mission.validating") < seq_of(&events, "mission.completed"));

    // The milestone tag exists in git and is recorded on the event.
    let tag_name = format!("kranz/{mission_id}/ms-1");
    let tags = raw_git(&root, &["tag", "-l"]);
    assert!(tags.contains(&tag_name), "tag {tag_name} missing from: {tags}");
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::MilestoneCompleted { tag: Some(t), .. } if *t == tag_name
    )));

    // Both features completed with no commits (mock workers touch nothing).
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::FeatureCompleted { feature_id, commits } if feature_id == "f-1-2" && commits.is_empty()
    )));

    // Final state folds to Complete with both features Complete.
    let state = reducer::fold(&events).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Complete);
    assert!(state.mission.milestones[0]
        .features
        .iter()
        .all(|f| f.status == FeatureStatus::Complete));
}

// ---------------------------------------------------------------------------
// 2. Validation round: finding → fix feature → clean round → complete
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn validation_round_creates_fix_feature_then_completes() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let finding = json!([{
        "subject": "part 1 works",
        "severity": "major",
        "evidence": "the endpoint returns 500 on empty input",
        "suggestedFix": "guard empty input"
    }]);

    // Session order: worker f-1-1, orchestrator, functional validator #1
    // (one finding), fix worker, functional validator #2 (clean).
    // Orchestrator turns: seed, judgement f-1-1, fix-features, judgement fix.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            judgement("complete", ""),
            fix_features(1),
            judgement("complete", ""),
        ]),
        validator_with(finding),
        worker_pass(),
        validator_with(json!([])),
    ]));

    let cfg = MissionConfig { skip_functional: false, ..test_cfg() };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);

    // One fix cycle consumed; the fix feature exists with origin Fix and the
    // documented id shape, and it completed.
    let state = engine.state();
    let ms = &state.mission.milestones[0];
    assert_eq!(ms.fix_cycles, 1);
    let fix = ms
        .features
        .iter()
        .find(|f| f.origin == FeatureOrigin::Fix)
        .expect("fix feature exists");
    assert_eq!(fix.id, "ms-1-fix-1-1");
    assert_eq!(fix.status, FeatureStatus::Complete);

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(types.contains(&"validation.finding"), "finding event: {types:?}");
    assert!(types.contains(&"fixfeature.created"), "fixfeature event: {types:?}");
    assert_eq!(
        types.iter().filter(|t| **t == "milestone.validating").count(),
        2,
        "two validation rounds: {types:?}"
    );
}

// ---------------------------------------------------------------------------
// 3. Loop guard: findings past the fix-cycle cap block the milestone
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn loop_guard_blocks_milestone_after_max_fix_cycles() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let finding = json!([{
        "subject": "part 1 works",
        "severity": "major",
        "evidence": "still failing",
        "suggestedFix": ""
    }]);

    // Round 1 finds a problem (fix cycle 1 allowed by cap=1); the fix worker
    // "passes" but round 2 finds a problem again. The conversion turn still
    // runs at the cap (the orchestrator could waive), but here it wants
    // ANOTHER fix — cap exceeded → blocked, no fixfeature.created.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            judgement("complete", ""),
            fix_features(1),
            judgement("complete", ""),
            fix_features(1), // round 2 conversion: fixes wanted at the cap
        ]),
        validator_with(finding.clone()),
        worker_pass(),
        validator_with(finding),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        max_fix_cycles_per_milestone: 1,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Blocked);
    assert_eq!(engine.state().mission.status, MissionStatus::Blocked);
    assert_eq!(engine.state().mission.milestones[0].status, MilestoneStatus::Blocked);
    assert_eq!(engine.state().mission.milestones[0].fix_cycles, 1);

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::MilestoneBlocked { reason, .. } if reason.contains("fix-cycle cap")
    )));
    // Round 2's conversion turn wanted fixes but the cap was spent: nothing
    // beyond round 1's single fix feature was ever created.
    assert_eq!(
        event_types(&events).iter().filter(|t| **t == "fixfeature.created").count(),
        1,
        "only round 1 created a fix feature"
    );
}

// ---------------------------------------------------------------------------
// 3b. Waive: all findings waived → milestone completes, no fix cycle
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn waive_completes_milestone() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let finding = json!([{
        "subject": "part 1 works",
        "severity": "minor",
        "evidence": "the new helper's docstring omits the error case",
        "suggestedFix": "extend the docstring"
    }]);

    // Session order: worker f-1-1, orchestrator, functional validator (one
    // minor finding). Orchestrator turns: seed, judgement f-1-1, conversion —
    // which waives the finding: no fix worker, no second validation round.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            judgement("complete", ""),
            waive_reply("part 1 works", "docstring nitpick"),
        ]),
        validator_with(finding),
    ]));

    let cfg = MissionConfig { skip_functional: false, ..test_cfg() };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);

    // No fix cycle consumed, no fix feature materialized.
    let ms = &engine.state().mission.milestones[0];
    assert_eq!(ms.status, MilestoneStatus::Complete);
    assert_eq!(ms.fix_cycles, 0, "a waived round consumes no fix cycle");
    assert!(ms.features.iter().all(|f| f.origin == FeatureOrigin::Plan));

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(types.contains(&"validation.finding"), "finding still surfaced: {types:?}");
    assert!(!types.contains(&"fixfeature.created"), "no fix feature: {types:?}");
    assert_eq!(
        types.iter().filter(|t| **t == "milestone.validating").count(),
        1,
        "exactly one validation round: {types:?}"
    );
    assert!(types.contains(&"mission.completed"), "mission completed: {types:?}");

    // The waiver is on the log as an orchestrator decision: summary names
    // the subject, detail carries the one-line justification.
    let (summary, detail) = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::OrchestratorDecision { summary, detail: Some(detail) }
                if summary.starts_with("waived") =>
            {
                Some((summary.clone(), detail.clone()))
            }
            _ => None,
        })
        .expect("a waive orchestrator.decision exists");
    assert!(summary.contains("waived 1 finding(s)"), "summary: {summary}");
    assert!(summary.contains("part 1 works"), "summary names the subject: {summary}");
    assert!(detail.contains("docstring nitpick"), "detail carries the reason: {detail}");
}

// ---------------------------------------------------------------------------
// 3c. Waive at the cap: the live scenario — a spent fix-cycle cap must not
//     block a milestone whose only remaining findings the orchestrator waives
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn waive_at_cap_completes_instead_of_blocking() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let major = json!([{
        "subject": "part 1 works",
        "severity": "major",
        "evidence": "the endpoint returns 500 on empty input",
        "suggestedFix": "guard empty input"
    }]);
    let minor = json!([{
        "subject": "helper docs",
        "severity": "minor",
        "evidence": "docstring omits the error case",
        "suggestedFix": "extend the docstring"
    }]);

    // Round 1: a real finding consumes the only fix cycle (cap=1); the fix
    // worker passes. Round 2: one leftover nitpick with the cap spent. The
    // conversion turn must still run, and the waive must COMPLETE the
    // milestone — the old flow blocked here without ever asking.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            judgement("complete", ""),
            fix_features(1),
            judgement("complete", ""),
            waive_reply("helper docs", "cosmetic; outside the contract"),
        ]),
        validator_with(major),
        worker_pass(),
        validator_with(minor),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        max_fix_cycles_per_milestone: 1,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);
    let ms = &engine.state().mission.milestones[0];
    assert_eq!(ms.status, MilestoneStatus::Complete);
    assert_eq!(ms.fix_cycles, 1, "the waived round consumed no extra cycle");

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(!types.contains(&"milestone.blocked"), "must not block: {types:?}");
    assert!(types.contains(&"mission.completed"), "mission completed: {types:?}");
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::OrchestratorDecision { summary, .. }
            if summary.starts_with("waived 1 finding(s)") && summary.contains("helper docs")
    )));
}

// ---------------------------------------------------------------------------
// 3d. Waive at the final gate: all-waived gate findings complete the mission
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn waive_at_final_gate_completes_mission() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // One command assertion that fails portably (`cd` into a missing dir
    // errors under both `sh -c` and `cmd /C`) → one final-gate finding.
    let contract = vec![assertion("a-1", "the build succeeds", Some("cd kranz-no-such-dir"))];

    // Orchestrator turns: seed, judgement f-1-1, gate conversion (waive).
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            judgement("complete", ""),
            waive_reply("a-1", "command not runnable in this environment"),
        ]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(types.contains(&"validation.finding"), "gate finding surfaced: {types:?}");
    assert!(!types.contains(&"fixfeature.created"), "no fix feature: {types:?}");
    assert!(!types.contains(&"milestone.blocked"), "must not block: {types:?}");
    assert!(types.contains(&"mission.completed"), "mission completed: {types:?}");
    assert!(seq_of(&events, "mission.validating") < seq_of(&events, "mission.completed"));
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::OrchestratorDecision { summary, .. }
            if summary.starts_with("waived 1 finding(s)") && summary.contains("a-1")
    )));
}

// ---------------------------------------------------------------------------
// 4. Respawn budget: fail → respawn → budget exhausted → feature.failed
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn respawn_bounded_fails_feature_then_mission_continues() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // f-1-1: two failing worker runs. Judgement says "respawn" both times;
    // the first respawn is within budget (max_respawns = 1), the second
    // request exceeds it → the ENGINE fails the feature without consulting
    // further. f-1-2 then succeeds; validators are skipped, contract empty,
    // so the mission completes around the failed feature.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_fail(), // f-1-1 attempt 1
        orch_script(vec![
            judgement("respawn", "add the missing test double"),
            judgement("respawn", "try harder"), // denied: budget exhausted
            judgement("complete", ""),          // f-1-2
        ]),
        worker_fail(), // f-1-1 attempt 2 (the one allowed respawn)
        worker_pass(), // f-1-2
    ]));

    let cfg = MissionConfig { max_respawns: 1, ..test_cfg() };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(2, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let ms = &engine.state().mission.milestones[0];
    assert_eq!(ms.features[0].status, FeatureStatus::Failed);
    assert_eq!(ms.features[0].respawns, 1, "exactly one respawn honoured");
    assert_eq!(ms.features[0].worker_runs.len(), 2, "two worker runs total");
    assert_eq!(ms.features[1].status, FeatureStatus::Complete);

    // The respawned worker received the judgement guidance in its task.
    let specs = backend.started_specs();
    // start order: worker, orch, worker(respawn), worker(f-1-2)
    match &specs[2].prompt {
        PromptMode::SingleShot(task) => {
            assert!(task.contains("add the missing test double"), "guidance passed: {task}")
        }
        other => panic!("respawned worker must be single-shot, got {other:?}"),
    }

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::FeatureFailed { feature_id, reason }
            if feature_id == "f-1-1" && reason.contains("respawn budget exhausted")
    )));
}

// ---------------------------------------------------------------------------
// 5. Pause / resume / user message
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn pause_resume_and_user_message_flow() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Orchestrator turns: seed, free-text consult (queued user message),
    // judgement f-1-1. The consult turn happens BEFORE the first worker run,
    // so the orchestrator session is the first session started.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        orch_script(vec![
            "Acknowledged — I'll fold the request into the remaining feature.".to_string(),
            judgement("complete", ""),
        ]),
        worker_pass(),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let paths = engine.paths().clone();

    // Pause is queued BEFORE run() starts; run() must drain it first and hold.
    control::enqueue(&paths, &ControlCommand::Pause).unwrap();

    let handle = tokio::spawn(async move {
        let result = engine.run().await;
        (engine, result)
    });

    // Give the loop time to drain the Pause and settle; the snapshot (kept in
    // lockstep by emit) must show Paused.
    tokio::time::sleep(Duration::from_millis(900)).await;
    let snapshot = reducer::read_snapshot(&paths.state_file()).expect("state.json snapshot");
    assert_eq!(snapshot.mission.status, MissionStatus::Paused, "engine paused while waiting");

    // Queue the user message WHILE PAUSED: a paused engine only drains its
    // inbox, so the message provably sits in pending_user_messages until the
    // resume — which makes the post-resume ordering deterministic (consult
    // turn strictly before the first worker run, matching the script FIFO).
    // Enqueueing Resume first instead would race: the engine could drain the
    // Resume alone and start a worker before ever seeing the message.
    control::enqueue(
        &paths,
        &ControlCommand::Msg { text: "swap feature".to_string(), interrupt: false },
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await; // a drain tick passes
    control::enqueue(&paths, &ControlCommand::Resume).unwrap();

    let (engine, result) = timeout(TEST_TIMEOUT, handle).await.expect("run must not hang").unwrap();
    assert_eq!(result.unwrap(), MissionStatus::Complete);
    drop(engine);

    let events = read_log(&paths);
    let paused = seq_of(&events, "mission.paused");
    let resumed = seq_of(&events, "mission.resumed");
    let user_msg = seq_of(&events, "user.message");
    let decision = events
        .iter()
        .filter(|e| e.kind.type_name() == "orchestrator.decision")
        .map(|e| e.seq)
        .find(|s| *s > user_msg)
        .expect("an orchestrator.decision follows the user message");
    assert!(paused < user_msg, "paused {paused} before user.message {user_msg}");
    assert!(user_msg < resumed, "user.message {user_msg} queued while paused, before resumed {resumed}");
    assert!(resumed < decision, "resumed {resumed} before the consult decision {decision}");
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::UserMessage { text, interrupt: false } if text == "swap feature"
    )));

    // Delete-after-apply: every applied control file was removed once its
    // event hit the log — the normal path leaves an empty inbox.
    let leftover: Vec<String> = std::fs::read_dir(paths.control_dir())
        .map(|rd| rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    assert!(leftover.is_empty(), "control inbox must be empty after apply: {leftover:?}");
}

// ---------------------------------------------------------------------------
// 5b. Scrubbing: orchestrator decision detail (structured-field leak)
// ---------------------------------------------------------------------------

/// Regression: the orchestrator's raw turn text becomes the
/// `orchestrator.decision` detail (and its summary feeds the decision
/// summary). A credential in that model-authored text must be redacted
/// before the event reaches events.jsonl.
#[tokio::test(flavor = "multi_thread")]
async fn orchestrator_decision_detail_is_scrubbed() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let token = "ghp_AbCdEfGhIjKlMnOpQrStUvWxYz0123";
    let leaky_judgement = json!({
        "decision": "complete",
        "guidance": "",
        "summary": format!("looks good; noticed {token} in the env"),
    })
    .to_string();

    // One feature, validators skipped, empty contract; orchestrator turns:
    // seed, then the leaky judgement.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![leaky_judgement]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let paths = engine.paths().clone();
    drop(engine);

    // The raw log never carries the token; the judgement decision is redacted
    // in both summary and detail.
    let raw_log = std::fs::read_to_string(paths.events_file()).unwrap();
    assert!(!raw_log.contains(token), "events.jsonl leaked the token");

    let events = read_log(&paths);
    let (summary, detail) = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::OrchestratorDecision { summary, detail: Some(detail) }
                if summary.starts_with("judgement for") =>
            {
                Some((summary.clone(), detail.clone()))
            }
            _ => None,
        })
        .expect("a judgement orchestrator.decision with detail exists");
    assert!(summary.contains("[REDACTED]"), "summary: {summary}");
    assert!(detail.contains("[REDACTED]"), "detail: {detail}");
    assert!(!detail.contains(token));
}

// ---------------------------------------------------------------------------
// 6. Kill + resume (§4.3 acceptance, in-process)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn kill_and_resume_completes_on_single_log() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // --- Phase 1: the "crash" -------------------------------------------
    // The first worker's session dies (exit Failed, no result). The engine
    // then tries a judgement turn, but the orchestrator script has NO
    // on_message batches: the session parks, the (shortened) stall timeout
    // declares it dead, the retry re-seeds — and the backend has no script
    // left, so run() errors out mid-feature. That is our in-process kill.
    let crash_worker = MockScript {
        events: vec![mock_init("w-crash"), mock_text("working on it…")],
        exit: SessionExit::Failed("simulated process crash".to_string()),
        ..Default::default()
    };
    let backend1 = Arc::new(MockBackend::with_scripts(vec![
        crash_worker,
        orch_script(vec![]), // seed only; the judgement turn starves
    ]));

    let mut engine = make_engine(&backend1, &root, test_cfg());
    engine.approve_plan(simple_plan(2, vec![])).unwrap();
    engine.set_orch_stall_timeout(Duration::from_millis(400));
    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();

    let err = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .expect_err("phase 1 must error out (simulated crash)");
    eprintln!("phase 1 crashed as scripted: {err}");

    // Phase-1 orchestrator sdk session id (for the resume assertion below).
    let phase1_events = {
        drop(engine); // releases the lock and flushes buffered deltas
        read_log(&paths)
    };
    let phase1_orch_sdk_id = phase1_events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::WorkerSpawned { role: Role::Orchestrator, sdk_session_id, .. } => {
                Some(sdk_session_id.clone())
            }
            _ => None,
        })
        .expect("phase 1 spawned an orchestrator run");
    let state = reducer::fold(&phase1_events).unwrap();
    assert_eq!(
        state.mission.milestones[0].features[0].status,
        FeatureStatus::Active,
        "crash left f-1-1 mid-feature"
    );

    // --- Phase 2: resume with fresh scripts ------------------------------
    // f-1-1 is Active → respawn candidate: a fresh worker finishes it. The
    // orchestrator is resumed via --resume (same sdk session id) and judges
    // both features complete. Validators skipped, contract empty → complete.
    let backend2 = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(), // f-1-1 rerun
        orch_script(vec![judgement("complete", ""), judgement("complete", "")]),
        worker_pass(), // f-1-2
    ]));
    let backend2_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend2) as Arc<dyn AgentBackend>;
    let mut engine =
        MissionEngine::resume(backend2_dyn, &root, &mission_id, false).expect("resume mission");

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);
    drop(engine);

    // The resumed orchestrator session used --resume with the phase-1 id.
    let specs = backend2.started_specs();
    let orch_spec = specs
        .iter()
        .find(|s| matches!(s.prompt, PromptMode::Streaming(_)))
        .expect("phase 2 started a streaming orchestrator session");
    assert_eq!(orch_spec.resume.as_deref(), Some(phase1_orch_sdk_id.as_str()));

    // ONE events.jsonl spanning both engine lifetimes: contiguous seq
    // (read_events refuses gaps) and a final fold of Complete.
    let events = read_log(&paths);
    assert_eq!(events.first().unwrap().seq, 1);
    assert_eq!(events.last().unwrap().seq, events.len() as u64);
    let state = reducer::fold(&events).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Complete);
    assert!(event_types(&events).contains(&"mission.completed"));
}

// ---------------------------------------------------------------------------
// 7. Forced re-seed mid-mission (§4.8 acceptance)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn force_reseed_reseeds_with_digest_and_plan() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let plan_json = json!({
        "goal": GOAL,
        "validationContract": [],
        "milestones": [{
            "title": "M1",
            "features": [
                { "title": "feature 1", "spec": "build part 1", "validationCriteria": ["part 1 works"] },
                { "title": "feature 2", "spec": "build part 2", "validationCriteria": ["part 2 works"] }
            ]
        }]
    })
    .to_string();

    // Session order: orchestrator #1 (planning: one conversational turn plus
    // the plan-JSON turn), worker f-1-1, orchestrator #2 (the re-seeded
    // session created after force_reseed; judges both features), worker f-1-2.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        orch_script(vec![
            "Understood. Two features under one milestone; no open questions.".to_string(),
            plan_json,
        ]),
        worker_pass(),
        orch_script(vec![judgement("complete", ""), judgement("complete", "")]),
        worker_pass(),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());

    // Planning phase drives orchestrator session #1.
    let reply = timeout(TEST_TIMEOUT, engine.planning_turn("plan two features please"))
        .await
        .expect("planning turn must not hang")
        .unwrap();
    assert!(reply.contains("no open questions"));
    let plan = match timeout(TEST_TIMEOUT, engine.request_plan())
        .await
        .expect("request_plan must not hang")
        .unwrap()
    {
        PlanRequest::Ready(plan) => plan,
        PlanRequest::NotReady(text) => panic!("scripted plan JSON must parse, got: {text}"),
    };
    assert_eq!(plan.milestones.len(), 1);
    engine.approve_plan(plan).unwrap();

    // Kill the live session and forget its id: the next orchestrator need
    // must take the fresh re-seed path. Behaviour must not visibly change.
    engine.force_reseed();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);

    // The second streaming session was seeded (initial prompt) with the
    // re-seed context: digest header + the approved plan.json.
    let specs = backend.started_specs();
    let streaming: Vec<_> =
        specs.iter().filter(|s| matches!(s.prompt, PromptMode::Streaming(_))).collect();
    assert_eq!(streaming.len(), 2, "exactly two orchestrator sessions");
    assert!(streaming[1].resume.is_none(), "re-seed is a fresh session, not a resume");
    match &streaming[1].prompt {
        PromptMode::Streaming(seed) => {
            assert!(seed.starts_with("MISSION m-"), "digest header first: {seed}");
            assert!(seed.contains("APPROVED PLAN (plan.json):"), "plan.json embedded: {seed}");
            assert!(seed.contains("build part 1"), "plan content present: {seed}");
        }
        other => panic!("orchestrator must be streaming, got {other:?}"),
    }
    // And every injected turn of the new session is digest-prefixed (§4.8).
    let injected = backend.injected_messages();
    let second_orch_injected = &injected[2]; // start order: orch1, worker, orch2, worker
    assert!(!second_orch_injected.is_empty());
    assert!(
        second_orch_injected[0].starts_with("MISSION m-"),
        "turn digest prefix: {}",
        second_orch_injected[0]
    );

    // The re-seed is announced on the log.
    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::OrchestratorDecision { summary, .. } if summary.contains("re-seeded")
    )));
}

// ---------------------------------------------------------------------------
// 7b. Planning conversation: plan-not-ready prose and seed-reply capture
// ---------------------------------------------------------------------------

/// An orchestrator that answers the plan demand AND the JSON-only retry with
/// prose is not ready to emit — a conversational state, not a backend error:
/// `request_plan` returns Ok(NotReady(<the prose>)), the mission stays in
/// Planning, and the SAME session can still produce the plan on a later
/// request (the conversation continued).
#[tokio::test(flavor = "multi_thread")]
async fn plan_not_ready_returns_prose() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let plan_json = json!({
        "goal": GOAL,
        "validationContract": [],
        "milestones": [{
            "title": "M1",
            "features": [
                { "title": "feature 1", "spec": "build part 1", "validationCriteria": ["part 1 works"] }
            ]
        }]
    })
    .to_string();
    let prose_first =
        "Before I emit the plan I still need an answer: which database should the demo target?"
            .to_string();
    let prose_retry =
        "I cannot emit the plan yet — please answer the database question first.".to_string();

    // One streaming orchestrator session; turns in order: plan demand →
    // prose, JSON-only retry → prose again, second plan demand → plan JSON.
    let backend = Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
        prose_first,
        prose_retry,
        plan_json,
    ])]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    let request = timeout(TEST_TIMEOUT, engine.request_plan())
        .await
        .expect("request_plan must not hang")
        .expect("prose replies are a conversational state, not a backend error");
    match request {
        PlanRequest::NotReady(text) => assert!(
            text.contains("database question"),
            "NotReady carries the retry turn's prose: {text}"
        ),
        PlanRequest::Ready(plan) => panic!("prose must not parse as a plan: {plan:?}"),
    }
    assert_eq!(
        engine.state().mission.status,
        MissionStatus::Planning,
        "a not-ready plan request leaves the mission in Planning"
    );

    // The conversation continued on the same session: the next request
    // parses the scripted plan JSON.
    let request = timeout(TEST_TIMEOUT, engine.request_plan())
        .await
        .expect("second request_plan must not hang")
        .unwrap();
    match request {
        PlanRequest::Ready(plan) => assert_eq!(plan.milestones.len(), 1),
        PlanRequest::NotReady(text) => panic!("scripted plan JSON must parse, got: {text}"),
    }
}

/// The seed turn's reply (the orchestrator's first words — often scoping
/// questions) is captured instead of discarded: `take_seed_reply` returns it
/// exactly once after the first turn of a fresh session, and again for the
/// resume-ack seed when a restarted engine resumes the sdk session.
#[tokio::test(flavor = "multi_thread")]
async fn seed_reply_is_captured_once_for_fresh_and_resumed_sessions() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // --- Fresh session: the planning seed's reply ends in questions -------
    let seed_text = "Two questions before I plan: which auth flows are in scope, and is the \
                     dashboard part of this mission?";
    let backend = Arc::new(MockBackend::with_scripts(vec![MockScript::streaming(vec![
        mock_init("orch-session"),
        mock_text(seed_text),
        mock_result_text(seed_text),
    ])
    .responding(vec![vec![mock_text("noted"), mock_result_text("noted")]])]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    let reply = timeout(TEST_TIMEOUT, engine.planning_turn("hello"))
        .await
        .expect("planning turn must not hang")
        .unwrap();
    assert_eq!(reply, "noted");
    assert_eq!(
        engine.take_seed_reply().as_deref(),
        Some(seed_text),
        "the seed turn's reply is captured, not discarded"
    );
    assert_eq!(engine.take_seed_reply(), None, "the seed reply is taken exactly once");

    let mission_id = engine.mission_id().to_string();
    drop(engine); // releases the lock; the sdk session id is on the log

    // --- Resume path: the resume-ack seed reply is captured too -----------
    let ack_text = "Acknowledged — resuming the planning conversation.";
    let backend2 = Arc::new(MockBackend::with_scripts(vec![MockScript::streaming(vec![
        mock_init("orch-session"),
        mock_text(ack_text),
        mock_result_text(ack_text),
    ])
    .responding(vec![vec![mock_text("continuing"), mock_result_text("continuing")]])]));
    let backend2_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend2) as Arc<dyn AgentBackend>;
    let mut engine =
        MissionEngine::resume(backend2_dyn, &root, &mission_id, false).expect("resume mission");

    let reply = timeout(TEST_TIMEOUT, engine.planning_turn("go on"))
        .await
        .expect("resumed planning turn must not hang")
        .unwrap();
    assert_eq!(reply, "continuing");
    assert_eq!(
        engine.take_seed_reply().as_deref(),
        Some(ack_text),
        "the resume-ack seed reply is captured"
    );
    assert_eq!(engine.take_seed_reply(), None);

    // And that second session really was a --resume of the first.
    let specs = backend2.started_specs();
    let orch_spec = specs
        .iter()
        .find(|s| matches!(s.prompt, PromptMode::Streaming(_)))
        .expect("the resumed engine started a streaming orchestrator session");
    assert!(orch_spec.resume.is_some(), "resume-ack path expected (--resume set)");
}

// ---------------------------------------------------------------------------
// 8. Plan approval mechanics
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn plan_approval_writes_plan_branch_and_commit() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    let mission_id = engine.mission_id().to_string();

    // Contract ids are missing/duplicated on purpose: approval must assign
    // unique-ish ids (a-1..).
    let mut plan = simple_plan(1, vec![
        assertion("", "tests pass", Some("cargo test")),
        assertion("", "docs updated", None),
    ]);
    plan.milestones[0].title = "Milestone One".to_string();
    engine.approve_plan(plan).unwrap();

    // plan.json exists, parses, and carries the assigned assertion ids.
    let paths = engine.paths().clone();
    let plan_text = std::fs::read_to_string(paths.plan_file()).expect("plan.json written");
    let written: Plan = serde_json::from_str(&plan_text).expect("plan.json parses");
    let ids: Vec<&str> = written.validation_contract.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids, vec!["a-1", "a-2"]);

    // Mission branch created from main and checked out; the approval commit
    // contains exactly plan.json + its human-readable plan.md twin.
    let branch = raw_git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(branch.trim(), format!("kranz/mission-{mission_id}"));
    let subject = raw_git(&root, &["log", "-1", "--format=%s"]);
    assert_eq!(subject.trim(), format!("[kranz] approved plan for {mission_id}"));
    let files = raw_git(&root, &["show", "--name-only", "--format=", "HEAD"]);
    let mut files: Vec<&str> = files.lines().filter(|l| !l.trim().is_empty()).collect();
    files.sort_unstable();
    assert_eq!(
        files,
        vec![
            format!(".kranz/missions/{mission_id}/plan.json"),
            format!(".kranz/missions/{mission_id}/plan.md"),
        ],
        "the approval commit contains exactly plan.json + plan.md"
    );
    let md = std::fs::read_to_string(
        root.join(".kranz").join("missions").join(&mission_id).join("plan.md"),
    )
    .expect("plan.md written");
    assert!(md.starts_with(&format!("# Mission plan — {mission_id}")), "{md}");
    assert!(md.contains("## Validation contract"), "{md}");
    assert!(md.contains("**[a-1]**"), "{md}");
    assert!(md.contains("## Milestone 1 —"), "{md}");
    assert!(md.contains("Done when:"), "{md}");
    // main itself did not move: it still points at the seed commit.
    let main_subject = raw_git(&root, &["log", "-1", "--format=%s", "main"]);
    assert_eq!(main_subject.trim(), "seed");

    // Reducer state: plan.approved materialized milestones/features with the
    // documented ids, and the mission is Running.
    let state = engine.state();
    assert_eq!(state.mission.status, MissionStatus::Running);
    assert_eq!(state.mission.milestones.len(), 1);
    assert_eq!(state.mission.milestones[0].id, "ms-1");
    assert_eq!(state.mission.milestones[0].features[0].id, "f-1-1");
    assert_eq!(state.mission.validation_contract.len(), 2);

    // plan.approved is on the log.
    drop(engine);
    let events = read_log(&paths);
    assert!(event_types(&events).contains(&"plan.approved"));

    // Double-approval is rejected (mission already Running).
    // (Recreate an engine handle just to probe the state machine guard —
    // resume() re-acquires the lock the drop released.)
    let backend_dyn: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
    let mut engine = MissionEngine::resume(backend_dyn, &root, &mission_id, false).unwrap();
    let err = engine.approve_plan(simple_plan(1, vec![])).unwrap_err();
    assert!(err.to_string().contains("Planning"), "got: {err}");
}
