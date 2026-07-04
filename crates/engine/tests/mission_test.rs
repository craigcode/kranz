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
use kranz_engine::git_ops::GitRepo;
use kranz_engine::orchestrator::{synthesize_conflict_resolution, MissionEngine, PlanRequest};
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

/// Parallelization decision reply (roadmap M3): the listed feature ids are
/// independent and merge in the given order.
fn parallel_plan(ids: &[&str]) -> String {
    json!({
        "independent": ids,
        "mergeOrder": ids,
        "summary": format!("{} features are independent", ids.len())
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

    // M1 completion report: report.md exists in the mission dir with the
    // documented sections, and the report commit landed on the mission
    // branch BEFORE mission.completed was emitted (it is HEAD — nothing
    // commits after it).
    let report = std::fs::read_to_string(
        root.join(".kranz").join("missions").join(&mission_id).join("report.md"),
    )
    .expect("report.md written at completion");
    assert!(report.starts_with(&format!("# Mission report — {mission_id}")), "{report}");
    assert!(report.contains("## What shipped"), "{report}");
    assert!(report.contains("feature 1"), "{report}");
    assert!(report.contains("## Validation history"), "{report}");
    assert!(report.contains("actual vs"), "{report}");
    assert!(report.contains("## Contract outcomes"), "{report}");
    assert!(report.contains("**[a-2]**"), "both assertions listed: {report}");

    let subject = raw_git(&root, &["log", "-1", "--format=%s"]);
    assert_eq!(subject.trim(), format!("[kranz] mission report for {mission_id}"));
    let files = raw_git(&root, &["show", "--name-only", "--format=", "HEAD"]);
    let mut files: Vec<&str> = files.lines().filter(|l| !l.trim().is_empty()).collect();
    files.sort_unstable();
    assert_eq!(
        files,
        vec![
            ".kranz/missions/index.md".to_string(),
            format!(".kranz/missions/{mission_id}/report.md"),
        ],
        "the report commit carries report.md + the refreshed index"
    );

    // The mission's index line kept its format and gained the report link.
    let index =
        std::fs::read_to_string(root.join(".kranz").join("missions").join("index.md")).unwrap();
    let line = index
        .lines()
        .find(|l| l.contains(&format!("[{mission_id}](")))
        .expect("mission line in index.md");
    assert!(line.contains(&format!("({mission_id}/plan.md)")), "plan link kept: {line}");
    assert!(line.contains(&format!("[report]({mission_id}/report.md)")), "report link: {line}");
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

    let mission_id = engine.mission_id().to_string();
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

    // The completion report replays the round: the waived finding is listed
    // with its severity/subject/evidence and the waiver's justification.
    let report = std::fs::read_to_string(
        root.join(".kranz").join("missions").join(&mission_id).join("report.md"),
    )
    .expect("report.md written at completion");
    assert!(report.contains("## Validation history"), "{report}");
    assert!(report.contains("[minor] part 1 works"), "finding listed: {report}");
    assert!(report.contains("docstring omits the error case"), "evidence listed: {report}");
    assert!(report.contains("Disposition: waived."), "{report}");
    assert!(report.contains("part 1 works: docstring nitpick"), "waiver reason: {report}");
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
    let mission_id = engine.mission_id().to_string();
    drop(engine);

    // The completion report folds the paused span out of the elapsed time
    // and says so (mission.paused → mission.resumed is > 1s in this test).
    let report = std::fs::read_to_string(
        root.join(".kranz").join("missions").join(&mission_id).join("report.md"),
    )
    .expect("report.md written at completion");
    assert!(report.contains("paused)"), "paused time surfaced: {report}");

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
            ".kranz/missions/index.md".to_string(),
            format!(".kranz/missions/{mission_id}/plan.json"),
            format!(".kranz/missions/{mission_id}/plan.md"),
        ],
        "the approval commit contains plan.json + plan.md + the missions index"
    );
    let index =
        std::fs::read_to_string(root.join(".kranz").join("missions").join("index.md")).unwrap();
    assert!(index.starts_with("# Kranz missions"), "{index}");
    assert!(
        index.contains(&format!("[{mission_id}]({mission_id}/plan.md)")),
        "{index}"
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

/// The missions catalog upserts by mission id: appends new entries newest
/// last, replaces on re-approval, never duplicates.
#[test]
fn mission_index_upserts_by_id() {
    use kranz_engine::orchestrator::upsert_mission_index;
    let d1 = chrono::NaiveDate::from_ymd_opt(2026, 7, 2).unwrap();
    let d2 = chrono::NaiveDate::from_ymd_opt(2026, 7, 3).unwrap();

    let one = upsert_mission_index("", "m-aaa", "first goal", d1);
    assert!(one.starts_with("# Kranz missions"), "{one}");
    assert!(one.contains("- 2026-07-02 · [m-aaa](m-aaa/plan.md) — first goal"), "{one}");

    let two = upsert_mission_index(&one, "m-bbb", "second goal\nwith newline", d2);
    assert!(two.contains("[m-aaa]("), "{two}");
    assert!(two.contains("- 2026-07-03 · [m-bbb](m-bbb/plan.md) — second goal with newline"), "{two}");
    assert!(two.find("[m-aaa](").unwrap() < two.find("[m-bbb](").unwrap(), "newest last");

    let re = upsert_mission_index(&two, "m-aaa", "first goal, re-planned", d2);
    assert_eq!(re.matches("[m-aaa](").count(), 1, "no duplicate on re-approval: {re}");
    assert!(re.contains("first goal, re-planned"), "{re}");
}

/// The completion-report link appends to exactly the named mission's line,
/// idempotently, without disturbing the line format.
#[test]
fn mission_index_report_link_appends_once() {
    use kranz_engine::orchestrator::{mark_mission_index_report, upsert_mission_index};
    let d = chrono::NaiveDate::from_ymd_opt(2026, 7, 3).unwrap();
    let index = upsert_mission_index("", "m-aaa", "goal", d);
    let index = upsert_mission_index(&index, "m-bbb", "other goal", d);

    let marked = mark_mission_index_report(&index, "m-aaa");
    assert!(
        marked.contains("- 2026-07-03 · [m-aaa](m-aaa/plan.md) — goal · [report](m-aaa/report.md)"),
        "{marked}"
    );
    assert!(!marked.contains("[report](m-bbb/report.md)"), "only the named mission: {marked}");

    let again = mark_mission_index_report(&marked, "m-aaa");
    assert_eq!(again, marked, "idempotent");
    let unknown = mark_mission_index_report(&marked, "m-zzz");
    assert_eq!(unknown, marked, "unknown id leaves the index unchanged");
}

// ---------------------------------------------------------------------------
// 9. Mission hygiene: abandon (roadmap M2)
// ---------------------------------------------------------------------------

/// Abandoning a freshly-created (planning) mission appends exactly one
/// `mission.abandoned` event on a still-contiguous log, and the reducer folds
/// the log to `Abandoned`. The snapshot is refreshed to match.
#[tokio::test(flavor = "multi_thread")]
async fn abandon_planning_mission_sets_abandoned_status() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::new());
    let engine = make_engine(&backend, &root, test_cfg());
    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();
    // Drop the engine so its lock is released before abandon re-acquires it.
    drop(engine);

    let before = read_log(&paths);
    assert_eq!(reducer::fold(&before).unwrap().mission.status, MissionStatus::Planning);

    kranz_engine::orchestrator::abandon_mission(&root, &mission_id, "no longer needed", false)
        .expect("abandon a planning mission");

    // Exactly one new event, of the right kind, carrying the reason.
    let after = read_log(&paths);
    assert_eq!(after.len(), before.len() + 1, "one event appended");
    assert_eq!(after.first().unwrap().seq, 1);
    assert_eq!(after.last().unwrap().seq, after.len() as u64, "contiguous seq");
    assert!(matches!(
        &after.last().unwrap().kind,
        EventKind::MissionAbandoned { reason } if reason == "no longer needed"
    ));

    // The reducer folds to Abandoned, and the on-disk snapshot matches.
    assert_eq!(reducer::fold(&after).unwrap().mission.status, MissionStatus::Abandoned);
    let snapshot = reducer::read_snapshot(&paths.state_file()).expect("state.json");
    assert_eq!(snapshot.mission.status, MissionStatus::Abandoned);
    assert_eq!(snapshot.last_seq, after.last().unwrap().seq);
}

/// An abandoned mission must never be resumed and run — abandon exists to stop
/// spend, and the abandon event (being the newest log write) would otherwise be
/// auto-selected by `kranz run`. run() must reject it with no worker spawned.
#[tokio::test(flavor = "multi_thread")]
async fn running_an_abandoned_mission_is_rejected() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());
    let engine = make_engine(&backend, &root, test_cfg());
    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();
    drop(engine);

    kranz_engine::orchestrator::abandon_mission(&root, &mission_id, "stop", false)
        .expect("abandon");

    let backend2: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
    let mut resumed = MissionEngine::resume(backend2, &root, &mission_id, false).expect("resume");
    let err = resumed.run().await.expect_err("running a terminal mission must be rejected");
    assert!(
        matches!(err, kranz_engine::error::EngineError::InvalidState(_)),
        "expected InvalidState, got {err:?}"
    );
    // No worker.spawned appended by the rejected run.
    let events = read_log(&paths);
    assert!(
        !events.iter().any(|e| matches!(e.kind, EventKind::WorkerSpawned { .. })),
        "a rejected run must not spawn workers"
    );
}

/// Abandoning an already-terminal mission errors without touching the log.
#[tokio::test(flavor = "multi_thread")]
async fn abandon_already_terminal_mission_errors() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::new());
    let engine = make_engine(&backend, &root, test_cfg());
    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();
    drop(engine);

    // First abandon succeeds and makes the mission terminal.
    kranz_engine::orchestrator::abandon_mission(&root, &mission_id, "first", false).unwrap();
    let after_first = read_log(&paths);

    // A second abandon is rejected: the mission is already terminal.
    let err = kranz_engine::orchestrator::abandon_mission(&root, &mission_id, "again", false)
        .expect_err("abandoning a terminal mission must error");
    assert!(
        err.to_string().contains("already terminal"),
        "error should name the terminal state: {err}"
    );

    // The rejected call appended nothing.
    let after_second = read_log(&paths);
    assert_eq!(after_second.len(), after_first.len(), "no event appended on the rejected abandon");
}

/// A live engine holding the mission lock makes abandon fail with LockHeld
/// (the CLI turns this into "stop the running mission or pass --force-lock").
#[tokio::test(flavor = "multi_thread")]
async fn abandon_fails_while_engine_holds_lock() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::new());
    // Keep the engine ALIVE: it still holds the single-writer lock.
    let engine = make_engine(&backend, &root, test_cfg());
    let mission_id = engine.mission_id().to_string();

    let err = kranz_engine::orchestrator::abandon_mission(&root, &mission_id, "x", false)
        .expect_err("abandon must fail while the lock is held");
    assert!(
        matches!(err, kranz_engine::error::EngineError::LockHeld(_)),
        "expected LockHeld, got: {err}"
    );

    drop(engine);
}

// ---------------------------------------------------------------------------
// 10. Environment preflight (roadmap M2)
// ---------------------------------------------------------------------------

/// preflight() flags a contract command whose leading program is plainly
/// missing from PATH (a `warn`), and stays silent for commands whose program
/// resolves — a bare shell builtin (`cd .`) or the ubiquitous `true`.
#[tokio::test(flavor = "multi_thread")]
async fn preflight_flags_missing_program_and_ignores_present_ones() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());

    // A mission whose contract command uses a program that cannot exist.
    let missing = vec![assertion(
        "a-1",
        "the check passes",
        Some("definitely-not-a-real-program-xyz --check"),
    )];
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, missing)).unwrap();
    let issues = engine.preflight();
    let warn = issues
        .iter()
        .find(|i| i.message.contains("definitely-not-a-real-program-xyz"))
        .expect("the missing program is flagged");
    assert_eq!(warn.severity, "warn", "a missing program is a warning, not an error");
    // No spurious hard-error issues: this IS a git repo with a writable .kranz.
    assert!(
        !issues.iter().any(|i| i.severity == "error"),
        "no false hard errors: {issues:?}"
    );
    drop(engine);

    // A mission whose contract commands both resolve → no issues at all.
    let (_dir2, root2) = init_repo();
    let backend2 = Arc::new(MockBackend::new());
    let present = vec![
        assertion("a-1", "trivially true", Some("true")),
        assertion("a-2", "a builtin", Some("cd .")),
    ];
    let mut engine2 = make_engine(&backend2, &root2, test_cfg());
    engine2.approve_plan(simple_plan(1, present)).unwrap();
    assert!(
        engine2.preflight().is_empty(),
        "true / cd . resolve, so preflight is clean: {:?}",
        engine2.preflight()
    );
}

/// run() emits exactly one `orchestrator.decision` summarizing preflight
/// issues (before any worker spawns) when the contract names a missing
/// program, and the mission still completes — preflight never blocks.
#[tokio::test(flavor = "multi_thread")]
async fn run_emits_preflight_decision_when_issues_exist() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // The one command assertion names a program that cannot resolve, so its
    // final-gate command would fail — so we WAIVE it at the gate to let the
    // mission complete (preflight is orthogonal to the gate; we are asserting
    // the preflight decision fires, not the gate outcome).
    let contract = vec![assertion(
        "a-1",
        "the check passes",
        Some("definitely-not-a-real-program-xyz --check"),
    )];

    // Orchestrator turns: seed, judgement f-1-1, then the final-gate conversion
    // turn waives the failing command assertion.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            judgement("complete", ""),
            waive_reply("a-1", "command program unavailable in this environment"),
        ]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);

    // Exactly one preflight decision, and it precedes the first worker spawn.
    let preflight_seqs: Vec<u64> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::OrchestratorDecision { summary, .. } if summary.starts_with("preflight:") => {
                Some(e.seq)
            }
            _ => None,
        })
        .collect();
    assert_eq!(preflight_seqs.len(), 1, "exactly one preflight decision: {preflight_seqs:?}");
    let preflight_seq = preflight_seqs[0];
    let first_spawn = seq_of(&events, "worker.spawned");
    assert!(
        preflight_seq < first_spawn,
        "preflight decision {preflight_seq} precedes the first worker spawn {first_spawn}"
    );
    // The summary names the missing program.
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::OrchestratorDecision { summary, .. }
            if summary.starts_with("preflight:")
                && summary.contains("definitely-not-a-real-program-xyz")
    )));
}

// ---------------------------------------------------------------------------
// 11. Mid-mission re-planning (roadmap M2)
// ---------------------------------------------------------------------------

/// request_revised_plan returns a Ready proposal (streaming orchestrator turn,
/// same as request_plan); approve_revised_plan then commits revised-plan.md,
/// records an orchestrator.decision, skips a dropped pending feature, and adds
/// a new feature to the active milestone as a fix-origin feature.
#[tokio::test(flavor = "multi_thread")]
async fn request_and_approve_revised_plan_drops_and_adds_features() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // The revised plan the orchestrator proposes: same single milestone "M1",
    // but the remaining features are { feature 1 (kept), a brand-new feature }
    // — i.e. "feature 2" and "feature 3" are dropped, and "extra feature" is
    // added.
    let revised_json = json!({
        "goal": GOAL,
        "validationContract": [],
        "milestones": [{
            "title": "M1",
            "features": [
                { "title": "feature 1", "spec": "build part 1", "validationCriteria": ["part 1 works"] },
                { "title": "extra feature", "spec": "build the newly-needed part", "validationCriteria": ["extra works"] }
            ]
        }]
    })
    .to_string();

    // One streaming orchestrator session; the single turn is the revised-plan
    // demand → the revised plan JSON.
    let backend = Arc::new(MockBackend::with_scripts(vec![orch_script(vec![revised_json])]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    // Approve a 3-feature plan: mission goes Running, milestone ms-1 Pending
    // with features f-1-1, f-1-2, f-1-3 (all pending, none started).
    engine.approve_plan(simple_plan(3, vec![])).unwrap();
    assert_eq!(engine.state().mission.status, MissionStatus::Running);

    // Propose the revision.
    let request = timeout(TEST_TIMEOUT, engine.request_revised_plan())
        .await
        .expect("request_revised_plan must not hang")
        .expect("scripted plan JSON is not a backend error");
    let plan = match request {
        PlanRequest::Ready(plan) => plan,
        PlanRequest::NotReady(text) => panic!("scripted revised plan must parse: {text}"),
    };
    assert_eq!(plan.milestones[0].features.len(), 2);

    // Apply it.
    engine.approve_revised_plan(plan).expect("apply the revised plan");

    // f-1-2 and f-1-3 are dropped (skipped); f-1-1 untouched; one fix-origin
    // feature added to ms-1 with the re-plan id shape.
    let ms = &engine.state().mission.milestones[0];
    let by_id = |id: &str| ms.features.iter().find(|f| f.id == id).cloned();
    assert_eq!(by_id("f-1-1").unwrap().status, FeatureStatus::Pending, "kept feature untouched");
    assert_eq!(by_id("f-1-2").unwrap().status, FeatureStatus::Skipped, "dropped feature skipped");
    assert_eq!(by_id("f-1-3").unwrap().status, FeatureStatus::Skipped, "dropped feature skipped");
    // Re-plan ids carry a cycle discriminator (`-replan-<cycle>-<n>`) so a
    // second re-plan of the same milestone can't collide.
    let added = by_id("ms-1-replan-1-1").expect("added feature exists with re-plan id");
    assert_eq!(added.origin, FeatureOrigin::Fix, "added feature is fix-origin");
    assert_eq!(added.status, FeatureStatus::Pending);
    assert_eq!(added.title, "extra feature");

    // revised-plan.md was written + committed on the mission branch.
    let mission_id = engine.mission_id().to_string();
    let md = std::fs::read_to_string(
        root.join(".kranz").join("missions").join(&mission_id).join("revised-plan.md"),
    )
    .expect("revised-plan.md written");
    assert!(md.starts_with(&format!("# Revised mission plan — {mission_id}")), "{md}");
    assert!(md.contains("## Re-plan changes applied"), "{md}");
    assert!(md.contains("extra feature"), "{md}");
    let subject = raw_git(&root, &["log", "-1", "--format=%s"]);
    assert_eq!(subject.trim(), format!("[kranz] revised plan for {mission_id}"));

    // The log carries the re-plan decision, two feature.skipped, one
    // fixfeature.created.
    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::OrchestratorDecision { summary, .. } if summary.starts_with("re-plan for ms-1")
    )));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(&e.kind, EventKind::FeatureSkipped { reason, .. }
                if reason.contains("re-plan")))
            .count(),
        2,
        "both dropped features skipped by the re-plan"
    );
    assert_eq!(
        event_types(&events).iter().filter(|t| **t == "fixfeature.created").count(),
        1,
        "exactly one feature added by the re-plan"
    );

    // The revised mission still folds cleanly (contiguous log, no corruption).
    let state = reducer::fold(&events).unwrap();
    assert_eq!(state.mission.milestones[0].features.len(), 4, "3 planned + 1 added");
}

/// approve_revised_plan rejects a revision that drops (or reorders away) an
/// already-Complete milestone: completed work must reappear first, unchanged.
#[tokio::test(flavor = "multi_thread")]
async fn approve_revised_plan_rejects_dropping_a_completed_milestone() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Two milestones, one feature each. M1 completes cleanly (worker pass, no
    // validators); M2's single feature "passes", round 1 finds a problem (fix
    // cycle 1, allowed by cap=1), the fix worker "passes", but round 2 finds a
    // problem again → cap exceeded → M2 blocks and run() returns Blocked with
    // M1 Complete.
    let finding = json!([{
        "subject": "part 2 works",
        "severity": "major",
        "evidence": "still failing",
        "suggestedFix": ""
    }]);
    let backend = Arc::new(MockBackend::with_scripts(vec![
        // Session order: worker M1-f1, orchestrator, functional validator M1
        // (clean), worker M2-f1, functional validator M2 round 1 (finding),
        // fix worker M2, functional validator M2 round 2 (finding again).
        worker_pass(),
        orch_script(vec![
            judgement("complete", ""), // M1 f-1-1
            judgement("complete", ""), // M2 f-2-1
            fix_features(1),            // M2 round 1 conversion → one fix
            judgement("complete", ""), // M2 fix worker
            fix_features(1),            // M2 round 2 conversion at the cap → wants another
        ]),
        validator_with(json!([])),       // M1 validation: clean → M1 completes
        worker_pass(),                    // M2 f-2-1
        validator_with(finding.clone()),  // M2 round 1: finding (fix cycle 1)
        worker_pass(),                    // M2 fix worker
        validator_with(finding),          // M2 round 2: finding again → blocked
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        max_fix_cycles_per_milestone: 1, // round-2 findings exceed the cap → blocked
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    // Two milestones, one feature each.
    let plan = Plan {
        goal: GOAL.to_string(),
        validation_contract: vec![],
        milestones: vec![
            PlanMilestone {
                title: "M1".to_string(),
                features: vec![PlanFeature {
                    title: "feature 1".to_string(),
                    spec: "build part 1".to_string(),
                    validation_criteria: vec!["part 1 works".to_string()],
                }],
            },
            PlanMilestone {
                title: "M2".to_string(),
                features: vec![PlanFeature {
                    title: "feature 2".to_string(),
                    spec: "build part 2".to_string(),
                    validation_criteria: vec!["part 2 works".to_string()],
                }],
            },
        ],
    };
    engine.approve_plan(plan).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Blocked, "M2 blocked at the fix-cycle cap");
    assert_eq!(
        engine.state().mission.milestones[0].status,
        MilestoneStatus::Complete,
        "M1 is complete"
    );

    // A revised plan that DROPS the completed M1 (only lists M2) must be
    // rejected — completed work cannot be discarded.
    let drops_completed = Plan {
        goal: GOAL.to_string(),
        validation_contract: vec![],
        milestones: vec![PlanMilestone {
            title: "M2".to_string(),
            features: vec![PlanFeature {
                title: "feature 2".to_string(),
                spec: "build part 2".to_string(),
                validation_criteria: vec!["part 2 works".to_string()],
            }],
        }],
    };
    let err = engine
        .approve_revised_plan(drops_completed)
        .expect_err("dropping a completed milestone must be rejected");
    assert!(
        matches!(err, kranz_engine::error::EngineError::InvalidState(_)),
        "expected InvalidState, got: {err}"
    );
    assert!(err.to_string().contains("M1"), "error names the dropped completed milestone: {err}");

    // A revised plan that ALTERS the completed M1's features is also rejected.
    let alters_completed = Plan {
        goal: GOAL.to_string(),
        validation_contract: vec![],
        milestones: vec![
            PlanMilestone {
                title: "M1".to_string(),
                features: vec![PlanFeature {
                    title: "feature 1 RENAMED".to_string(),
                    spec: "build part 1".to_string(),
                    validation_criteria: vec!["part 1 works".to_string()],
                }],
            },
            PlanMilestone {
                title: "M2".to_string(),
                features: vec![PlanFeature {
                    title: "feature 2".to_string(),
                    spec: "build part 2".to_string(),
                    validation_criteria: vec!["part 2 works".to_string()],
                }],
            },
        ],
    };
    let err = engine
        .approve_revised_plan(alters_completed)
        .expect_err("altering a completed milestone's features must be rejected");
    assert!(err.to_string().contains("alters"), "error explains the alteration: {err}");
}

// ---------------------------------------------------------------------------
// 12. Parallel workers (roadmap M3, flag-gated)
// ---------------------------------------------------------------------------

/// With max_parallel_workers=2 a 1-milestone / 2-independent-feature mission
/// completes: the orchestrator marks both features independent, BOTH run
/// (2 worker.spawned), their per-feature branches merge into the mission branch
/// in the declared order, the milestone and mission complete, and NO worktree
/// is leaked (git worktree list is back to just the primary tree).
#[tokio::test(flavor = "multi_thread")]
async fn parallel_batch_runs_both_features_and_leaks_no_worktrees() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Session start order (FIFO): the orchestrator streaming session is needed
    // FIRST for the parallelization decision (before any worker), then the two
    // feature workers.
    //   1. orchestrator (streaming)
    //   2. worker f-1-1
    //   3. worker f-1-2
    // Orchestrator turns, in order:
    //   seed, parallel-plan (both independent), judgement f-1-1, judgement f-1-2.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        orch_script(vec![
            parallel_plan(&["f-1-1", "f-1-2"]),
            judgement("complete", ""),
            judgement("complete", ""),
        ]),
        worker_pass(),
        worker_pass(),
    ]));

    let cfg = MissionConfig { max_parallel_workers: 2, ..test_cfg() };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(2, vec![])).unwrap();
    let mission_id = engine.mission_id().to_string();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);

    // Both plan features completed.
    let ms = &engine.state().mission.milestones[0];
    assert_eq!(ms.features[0].status, FeatureStatus::Complete, "f-1-1 complete");
    assert_eq!(ms.features[1].status, FeatureStatus::Complete, "f-1-2 complete");

    let paths = engine.paths().clone();
    drop(engine);

    // Two feature workers spawned (one per feature) plus the orchestrator.
    let events = read_log(&paths);
    let worker_spawns = events
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::WorkerSpawned { role: Role::Worker, .. }))
        .count();
    assert_eq!(worker_spawns, 2, "both features ran a worker");

    // feature.started for both, feature.completed for both, in the log.
    let types = event_types(&events);
    assert_eq!(
        types.iter().filter(|t| **t == "feature.completed").count(),
        2,
        "both features completed: {types:?}"
    );
    assert!(types.contains(&"milestone.completed"), "milestone completed: {types:?}");
    assert!(types.contains(&"mission.completed"), "mission completed: {types:?}");

    // The parallel batch summary decision is on the log (existing event vocab).
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::OrchestratorDecision { summary, .. }
                if summary.starts_with("parallel:") && summary.contains("2 workers")
        )),
        "a parallel summary decision naming 2 workers exists"
    );
    // And the parallel-plan decision fired before any worker spawn.
    let plan_seq = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::OrchestratorDecision { summary, .. } if summary.starts_with("parallel plan for") => {
                Some(e.seq)
            }
            _ => None,
        })
        .expect("parallel plan decision exists");
    let first_worker_spawn = events
        .iter()
        .find(|e| matches!(&e.kind, EventKind::WorkerSpawned { role: Role::Worker, .. }))
        .expect("a worker spawned")
        .seq;
    assert!(plan_seq < first_worker_spawn, "parallel plan precedes the first worker");

    // NO leaked worktrees: git worktree list is back to a single (primary)
    // working tree. The per-feature worktree dirs are gone from disk too.
    let repo = GitRepo::open(&root).unwrap();
    let worktrees = repo.list_worktrees().unwrap();
    assert_eq!(worktrees.len(), 1, "only the primary worktree remains: {worktrees:?}");
    // The per-feature branches were cleaned up as well.
    assert!(
        !repo.branch_exists(&format!("kranz/wt/{mission_id}/f-1-1")).unwrap_or(false),
        "per-feature worktree branch must be deleted"
    );
    assert!(
        !repo.branch_exists(&format!("kranz/wt/{mission_id}/f-1-2")).unwrap_or(false),
        "per-feature worktree branch must be deleted"
    );
    // No worktree dir for THIS mission leaked into the temp dir.
    let leak_prefix = format!("kranz-wt-{mission_id}-");
    for entry in std::fs::read_dir(std::env::temp_dir()).unwrap().flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        assert!(
            !name.starts_with(&leak_prefix),
            "a parallel worktree dir leaked into temp: {name}"
        );
    }
}

/// Wall-clock overlap (roadmap M3 "done when"): the two worker SESSIONS run at
/// the same time, not one-at-a-time. Proven WITHOUT touching the mock backend:
/// the engine's own peak-concurrency tracker records how many worker sessions
/// were live simultaneously and surfaces it in the batch summary decision. With
/// max_parallel_workers=2 and two independent features, the peak is 2.
///
/// The single-writer invariant still holds around that overlap: the resulting
/// events.jsonl has CONTIGUOUS seq (read_events refuses gaps), both workers'
/// worker.spawned + worker.completed present, and it folds cleanly to Complete.
#[tokio::test(flavor = "multi_thread")]
async fn parallel_batch_sessions_overlap_in_wall_clock() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Same script shape as the leak test: orchestrator first (parallel plan +
    // two judgements), then the two feature workers.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        orch_script(vec![
            parallel_plan(&["f-1-1", "f-1-2"]),
            judgement("complete", ""),
            judgement("complete", ""),
        ]),
        worker_pass(),
        worker_pass(),
    ]));

    let cfg = MissionConfig { max_parallel_workers: 2, ..test_cfg() };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(2, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);

    // The batch summary decision records a peak overlap of 2 concurrent worker
    // sessions — the sessions provably ran at the same time.
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::OrchestratorDecision { summary, .. }
                if summary.starts_with("parallel:") && summary.contains("peak 2 concurrent")
        )),
        "the batch summary must record peak 2 concurrent sessions: {:?}",
        events
            .iter()
            .filter_map(|e| match &e.kind {
                EventKind::OrchestratorDecision { summary, .. } => Some(summary.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
    );

    // Single-writer invariant around the overlap: contiguous seq, both workers
    // spawned + completed, clean fold to Complete.
    assert_eq!(events.first().unwrap().seq, 1);
    assert_eq!(events.last().unwrap().seq, events.len() as u64, "contiguous seq");
    let worker_spawns = events
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::WorkerSpawned { role: Role::Worker, .. }))
        .count();
    let worker_completes = events
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::WorkerCompleted { .. }))
        .count();
    assert_eq!(worker_spawns, 2, "both worker sessions spawned");
    // 2 workers + orchestrator turns each emit worker.completed; at least the
    // two feature workers must be present.
    assert!(worker_completes >= 2, "both worker sessions completed: {worker_completes}");
    let state = reducer::fold(&events).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Complete);
}

/// Crash-safety (roadmap M3 invariant c): a crash DURING a parallel batch —
/// before the engine has appended the concurrent workers' buffered events —
/// leaves a log the resume path recovers from, and re-running the batch
/// completes the mission on ONE contiguous events.jsonl.
///
/// The "crash" is simulated in-process: the batch fans out two workers, but the
/// backend has only ONE worker script queued, so the second concurrent session
/// fails to start. run_parallel_batch_inner drains the JoinSet and returns the
/// error; run() propagates it. Both features already have feature.started on the
/// log (Phase A), so they resume as Active respawn candidates — no buffered
/// worker event of the successful task ever reached the log (accepted loss).
/// The cleanup guard + resume() sweep the per-feature worktrees/branches, and a
/// fresh engine finishes both features sequentially.
#[tokio::test(flavor = "multi_thread")]
async fn crash_mid_parallel_batch_resumes_cleanly() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // --- Phase 1: the crash ---------------------------------------------
    // Orchestrator marks both features independent, then the batch fans out
    // two concurrent workers — but only ONE worker script is queued, so the
    // other session's start errors and the batch aborts.
    let backend1 = Arc::new(MockBackend::with_scripts(vec![
        orch_script(vec![parallel_plan(&["f-1-1", "f-1-2"])]),
        worker_pass(), // only ONE worker script for a TWO-worker batch
    ]));

    let cfg = MissionConfig { max_parallel_workers: 2, ..test_cfg() };
    let mut engine = make_engine(&backend1, &root, cfg);
    engine.approve_plan(simple_plan(2, vec![])).unwrap();
    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();

    let err = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .expect_err("a starved concurrent worker aborts the batch (simulated crash)");
    eprintln!("phase 1 crashed as scripted: {err}");
    drop(engine); // releases the lock, flushes buffered deltas

    // The log is intact and contiguous despite the crash: it folds cleanly,
    // both features are Active (Phase A emitted feature.started; no worker
    // events landed), and the milestone is still in-flight.
    let phase1 = read_log(&paths);
    assert_eq!(phase1.first().unwrap().seq, 1);
    assert_eq!(phase1.last().unwrap().seq, phase1.len() as u64, "contiguous seq after crash");
    let state1 = reducer::fold(&phase1).expect("crashed log still folds cleanly");
    for f in &state1.mission.milestones[0].features {
        assert_eq!(f.status, FeatureStatus::Active, "features left Active by the crash");
        assert!(f.worker_runs.is_empty(), "no worker run recorded before the crash");
    }
    // No WORKER session's buffered events reached the log (the successful
    // task's buffer was dropped — accepted loss). Orchestrator runs still
    // complete their turns; only feature-worker spawns/completions are the
    // buffered-and-lost ones.
    assert!(
        !phase1
            .iter()
            .any(|e| matches!(&e.kind, EventKind::WorkerSpawned { role: Role::Worker, .. })),
        "no buffered worker session survived the crash"
    );

    // --- Phase 2: resume and finish -------------------------------------
    // resume() sweeps the orphaned per-feature worktrees/branches. Both
    // features are Active respawn candidates → the SEQUENTIAL path finishes
    // them (each: worker → judgement). Validators skipped, empty contract.
    let backend2 = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(), // f-1-1 rerun (sequential)
        orch_script(vec![judgement("complete", ""), judgement("complete", "")]),
        worker_pass(), // f-1-2 rerun (sequential)
    ]));
    let backend2_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend2) as Arc<dyn AgentBackend>;
    let mut engine =
        MissionEngine::resume(backend2_dyn, &root, &mission_id, false).expect("resume mission");

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);
    drop(engine);

    // ONE events.jsonl spanning both lifetimes: contiguous seq, clean Complete
    // fold, both features complete.
    let events = read_log(&paths);
    assert_eq!(events.first().unwrap().seq, 1);
    assert_eq!(events.last().unwrap().seq, events.len() as u64, "one contiguous log");
    let state = reducer::fold(&events).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Complete);
    assert!(state.mission.milestones[0]
        .features
        .iter()
        .all(|f| f.status == FeatureStatus::Complete));

    // No leaked worktrees after recovery.
    let repo = GitRepo::open(&root).unwrap();
    assert_eq!(repo.list_worktrees().unwrap().len(), 1, "no leaked worktrees: recovered clean");
    assert!(!repo.branch_exists(&format!("kranz/wt/{mission_id}/f-1-1")).unwrap_or(false));
    assert!(!repo.branch_exists(&format!("kranz/wt/{mission_id}/f-1-2")).unwrap_or(false));
}

/// Sequential invariance: the SAME 1-milestone / 2-feature mission run with
/// max_parallel_workers=1 behaves exactly as it does today — no parallelization
/// decision turn, no worktree branches, and the features run one at a time via
/// the sequential path (worker → judgement → worker → judgement).
#[tokio::test(flavor = "multi_thread")]
async fn max_parallel_one_is_the_unchanged_sequential_path() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Sequential session order: worker f-1-1 runs first, THEN the orchestrator
    // is started for the first judgement, then worker f-1-2. (Identical to the
    // pre-M3 happy path shape.)
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![judgement("complete", ""), judgement("complete", "")]),
        worker_pass(),
    ]));

    let cfg = MissionConfig { max_parallel_workers: 1, ..test_cfg() };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(2, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run()).await.expect("run must not hang").unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);

    // No parallelization decision was ever taken (the gate short-circuits at
    // max_parallel_workers == 1 before any parallel code runs).
    assert!(
        !events.iter().any(|e| matches!(
            &e.kind,
            EventKind::OrchestratorDecision { summary, .. }
                if summary.starts_with("parallel plan for") || summary.starts_with("parallel:")
        )),
        "no parallel decisions with max_parallel_workers=1"
    );

    // Both features completed, mission complete, exactly two feature workers.
    let types = event_types(&events);
    assert_eq!(
        types.iter().filter(|t| **t == "feature.completed").count(),
        2,
        "both features completed sequentially: {types:?}"
    );
    assert!(types.contains(&"mission.completed"));

    // No worktree branches were ever created; a single primary worktree.
    let repo = GitRepo::open(&root).unwrap();
    assert_eq!(repo.list_worktrees().unwrap().len(), 1, "no extra worktrees in sequential mode");
}

// ---------------------------------------------------------------------------
// 12b. Parallel-merge CONFLICT → resolution fix-feature (roadmap M3)
// ---------------------------------------------------------------------------
//
// The mock backend never touches the repo (workers make no commits — see
// worker_pass), so two per-feature worktree branches off the same start sha
// can never produce a REAL merge conflict end-to-end. Per the task, the
// conflict→resolution logic is therefore verified at the unit level against
// the pure `synthesize_conflict_resolution` helper the batch calls: given a
// MergeOutcome::Conflict's file list and the failed feature, it produces the
// resolution Feature with the right id/origin/spec, and the infinite-chain
// guard refuses to spawn a resolution-of-a-resolution.

/// A Plan-origin, Pending feature fixture with the given id.
fn plan_feature(id: &str, title: &str, spec: &str) -> Feature {
    Feature {
        id: id.to_string(),
        title: title.to_string(),
        spec: spec.to_string(),
        validation_criteria: vec![format!("{title} works")],
        origin: FeatureOrigin::Plan,
        status: FeatureStatus::Pending,
        worker_runs: Vec::new(),
        commits: Vec::new(),
        respawns: 0,
    }
}

/// A parallel-merge conflict on the SECOND feature synthesizes a resolution
/// fix-feature: id `<ms>-conflict-1`, origin Fix, status Pending, and a spec
/// that names the conflicting files, the original feature's spec, and the note
/// that earlier features already merged.
#[test]
fn conflict_synthesizes_resolution_fix_feature() {
    // Milestone ms-1 with two independent plan features; the first merged
    // cleanly, the second (f-1-2) conflicted.
    let f_1_1 = plan_feature("f-1-1", "feature one", "build part one");
    let f_1_2 = plan_feature("f-1-2", "feature two", "build the widget in src/widget.rs");
    let existing = vec![f_1_1, f_1_2.clone()];
    let conflict_files = vec!["src/widget.rs".to_string(), "src/lib.rs".to_string()];

    let resolution = synthesize_conflict_resolution("ms-1", &f_1_2, &conflict_files, &existing)
        .expect("a Plan-origin conflict must synthesize a resolution feature");

    // Id shape `<ms>-conflict-<n>`; n=1 (no existing -conflict- features).
    assert_eq!(resolution.id, "ms-1-conflict-1", "namespaced conflict-resolution id");
    // Origin Fix + Pending → the sequential loop (first_incomplete + next_feature)
    // picks it up on the next iteration, straight on the mission branch.
    assert_eq!(resolution.origin, FeatureOrigin::Fix, "resolution is a fix feature");
    assert_eq!(resolution.status, FeatureStatus::Pending, "resolution starts Pending");
    assert!(resolution.worker_runs.is_empty(), "fresh feature, no runs yet");
    assert_eq!(resolution.respawns, 0);

    // The spec carries the original title, the original spec, the conflicting
    // file list, and the earlier-features-merged note.
    let spec = &resolution.spec;
    assert!(spec.contains("build the widget in src/widget.rs"), "original spec text: {spec}");
    assert!(spec.contains("src/widget.rs"), "conflicting file listed: {spec}");
    assert!(spec.contains("src/lib.rs"), "second conflicting file listed: {spec}");
    assert!(
        spec.contains("already contains") || spec.contains("merged first"),
        "notes earlier features already merged: {spec}"
    );
    assert!(resolution.title.contains("feature two"), "title references original: {}", resolution.title);
    // Original feature's validation criteria carried over.
    assert_eq!(resolution.validation_criteria, f_1_2.validation_criteria);
}

/// The `-conflict-<n>` suffix increments off the count of existing conflict
/// features on the milestone, so two conflicts in one batch never collide
/// (mirrors the replan-id fix).
#[test]
fn second_conflict_gets_a_fresh_namespaced_id() {
    let f_1_2 = plan_feature("f-1-2", "feature two", "spec two");
    let f_1_3 = plan_feature("f-1-3", "feature three", "spec three");
    // After the first conflict fired, ms-1-conflict-1 already exists on the
    // milestone; the second conflict must derive n=2.
    let mut resolution_1 = plan_feature("ms-1-conflict-1", "Resolve merge conflict: feature two", "…");
    resolution_1.origin = FeatureOrigin::Fix;
    let existing = vec![f_1_2, f_1_3.clone(), resolution_1];

    let resolution =
        synthesize_conflict_resolution("ms-1", &f_1_3, &["src/x.rs".to_string()], &existing)
            .expect("second conflict synthesizes a resolution");
    assert_eq!(resolution.id, "ms-1-conflict-2", "second conflict is -conflict-2, no collision");
}

/// Infinite-chain guard: a feature whose id already contains `-conflict-`
/// (a resolution feature) must NOT spawn a resolution-of-a-resolution. The
/// helper returns None, so the milestone proceeds to validation/loop-guard as
/// today rather than looping conflict→resolution forever.
#[test]
fn resolution_feature_does_not_spawn_another_resolution() {
    let mut resolution = plan_feature("ms-1-conflict-1", "Resolve merge conflict: feature two", "spec");
    resolution.origin = FeatureOrigin::Fix;
    let existing = vec![resolution.clone()];

    let again =
        synthesize_conflict_resolution("ms-1", &resolution, &["src/x.rs".to_string()], &existing);
    assert!(again.is_none(), "a -conflict- feature must not spawn another resolution");
}

/// A conflict where git named no specific files still produces a usable
/// resolution spec (the file list falls back to a clear placeholder rather
/// than an empty string).
#[test]
fn conflict_with_no_named_files_still_synthesizes() {
    let f = plan_feature("f-1-1", "feature one", "the original work");
    let resolution = synthesize_conflict_resolution("ms-2", &f, &[], std::slice::from_ref(&f))
        .expect("empty file list still synthesizes");
    assert_eq!(resolution.id, "ms-2-conflict-1");
    assert!(resolution.spec.contains("the original work"), "original spec preserved");
    assert!(
        resolution.spec.contains("no specific files"),
        "empty conflict list gets a placeholder: {}",
        resolution.spec
    );
}
