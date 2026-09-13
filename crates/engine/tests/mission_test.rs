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

use kranz_engine::auth_verify::AuthVerdict;
use kranz_engine::backend::{AgentBackend, PromptMode};
use kranz_engine::backend_claude::parse_stream_line;
use kranz_engine::backend_mock::{
    mock_init, mock_result_error, mock_result_json, mock_result_text, mock_text, MockBackend,
    MockScript,
};
use kranz_engine::control;
use kranz_engine::cost;
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::events::{Event, EventKind};
use kranz_engine::git_ops::GitRepo;
use kranz_engine::orchestrator::{synthesize_conflict_resolution, MissionEngine, PlanRequest};
use kranz_engine::paths::MissionPaths;
use kranz_engine::reducer;
use kranz_engine::types::*;
use serde_json::json;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Once};
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

/// `sh`/`cmd` portable "succeed iff `path` exists as a file".
///
/// Workspace/readiness/assertion commands bottom out in `cmd /C` on Windows
/// and `sh -c` elsewhere. cmd.exe has no `test` builtin and Windows ships no
/// `test.exe`, so the POSIX spelling resolves only where Git's `usr/bin` is on
/// PATH — true on hosted CI, false on a stock Windows developer box, which is
/// why fixtures once described here as portable were not.
fn file_exists_cmd(path: &str) -> String {
    if cfg!(windows) {
        format!("if exist {path} (exit 0) else (exit 1)")
    } else {
        format!("test -f {path}")
    }
}

/// Portable "write `text` as one line into `path`". `printf` is a POSIX binary
/// cmd.exe cannot run; `echo` is a builtin in both shells. cmd would carry the
/// space before `>` into the file, so close it up there.
fn write_line_cmd(text: &str, path: &str) -> String {
    if cfg!(windows) {
        format!("echo {text}>{path}")
    } else {
        format!("echo {text} > {path}")
    }
}

/// Portable "succeed iff `path` exists, then run `then`".
fn if_file_exists_cmd(path: &str, then: &str) -> String {
    if cfg!(windows) {
        format!("if exist {path} ({then}) else (exit 1)")
    } else {
        format!("test -f {path} && {then}")
    }
}

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
        let missing = std::env::temp_dir().join(format!(
            "kranz-mission-test-no-config-{}",
            std::process::id()
        ));
        std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
        std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
        if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
            std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
        }
    });
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Returns false (after a skip note) when git is missing.
fn setup() -> bool {
    isolate_git_env();
    if git_available() {
        true
    } else {
        kranz_engine::test_capability::skip(
            kranz_engine::test_capability::capability::GIT,
            "git is not on PATH",
        );
        false
    }
}

/// Run git directly (test plumbing, independent of the code under test).
fn raw_git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn interpret_head_trailers(dir: &Path) -> String {
    let message = raw_git(dir, &["log", "-1", "--format=%B"]);
    let mut child = Command::new("git")
        .args(["interpret-trailers", "--parse"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn git interpret-trailers");
    child
        .stdin
        .as_mut()
        .expect("stdin piped")
        .write_all(message.as_bytes())
        .expect("write commit message");
    let out = child.wait_with_output().expect("wait for trailers");
    assert!(
        out.status.success(),
        "git interpret-trailers failed: {}",
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
/// functional validator where the scenario needs a validation round). Pin
/// Checkout isolation — production default is Worktree, but these mock-driven
/// e2e tests exercise the sequential checkout path.
fn test_cfg() -> MissionConfig {
    MissionConfig {
        skip_scrutiny: true,
        skip_functional: true,
        worker_isolation: WorkerIsolation::Checkout,
        // These mock-driven integration tests exercise validator state
        // transitions, not the host process-sandbox implementation. Windows
        // has no containment tier, so opt in explicitly rather than weakening
        // the production fail-closed default.
        validator_allow_uncontained_degrade: true,
        ..MissionConfig::default()
    }
}

fn make_engine(backend: &Arc<MockBackend>, root: &Path, cfg: MissionConfig) -> MissionEngine {
    let backend: Arc<dyn AgentBackend> = Arc::clone(backend) as Arc<dyn AgentBackend>;
    let mut engine =
        MissionEngine::create(backend, root, GOAL, cfg).expect("create mission engine");
    // Pre-seed the worker-auth verdict (mission m-165b6f, f-2-2): otherwise
    // the live preflight (orchestrator.rs `worker_auth_verdict`) would
    // consume the first queued `MockScript` meant for a real worker or
    // validator session, desyncing every test's FIFO script order. Matches
    // the fail-safe `Inconclusive` these tests hardcoded before the
    // preflight was wired in, so mission-flow behaviour here is unchanged;
    // the preflight itself is exercised by `auth_verify`'s own unit tests.
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    engine
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
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec![],
        standards_manifest: None,
        reviewer_independence: None,
    }
}

fn considered_alternatives() -> ConsideredAlternatives {
    ConsideredAlternatives {
        chosen: "single integrated slice with tests at the acceptance boundary".to_string(),
        rejected: vec![
            RejectedAlternative {
                approach: "big-bang rewrite".to_string(),
                trade_off: "too much review surface for one approval".to_string(),
            },
            RejectedAlternative {
                approach: "docs-only spike".to_string(),
                trade_off: "would not deliver the requested behavior".to_string(),
            },
        ],
    }
}

/// Worker script: completed single-shot run whose final text is a passing
/// WorkerReport. Writes a unique file into the session cwd so the worker
/// leaves a dirty tree behind (§4.4), which the engine checkpoints as a
/// real, non-meta commit on the mission branch.
fn worker_pass() -> MockScript {
    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = format!("delivered-{n}.txt");
    MockScript::single_shot_json(&json!({
        "result": "pass",
        "summary": "implemented and tested",
        "filesTouched": [path],
        "testsAdded": [],
        "testEvidence": "all green",
        "commits": []
    }))
    .writes_file(&path, "delivered by the mock worker\n")
}

/// Worker script: completed single-shot run whose final text is a passing
/// WorkerReport, but which writes NOTHING to the session cwd — the tree
/// stays clean, no checkpoint commit lands, and the feature's commit list is
/// empty. Used to construct an empty-deliverable mission (feature f-2-2).
fn worker_pass_no_write() -> MockScript {
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

/// Worker script: the backend CLI dies in seconds with an auth error and NO
/// terminal event (ticket worker-spawn-auth-failure-budget) — no result event
/// is replayed, and the exit carries the "without emitting a result message"
/// plus the claude auth signature ("Not logged in"). This is the m-eee81f
/// cursor auth-death shape, on the claude kind the mock backend maps to.
fn worker_auth_death() -> MockScript {
    MockScript {
        events: vec![mock_init("mock-session")],
        exit: kranz_engine::backend::SessionExit::Failed(
            "claude exited with exit status: 1 without emitting a result message; \
             stderr tail: Error: Not logged in"
                .to_string(),
        ),
        ..Default::default()
    }
}

/// Validator script returning the given findings.
fn validator_with(findings: serde_json::Value) -> MockScript {
    MockScript::single_shot_json(&json!({ "findings": findings, "summary": "validated" }))
}

/// Validator script that is DENIED `command` and, blocked on it, fails to
/// produce a trusted report (result is an error, no parseable report) — the
/// realistic shape of a validator stopped by a capability boundary, and the
/// case the grant flow is gated on (`!validator_outcome_trusted`). The tool_use
/// and denied tool_result are built from real Claude stream shapes via
/// `parse_stream_line` (a denied `tool_result` carries `tool: None`; the command
/// lives only on the preceding `tool_use`), so the runner's positional
/// ToolUse→ToolResult correlation is exercised exactly as in production — not
/// the fabricated `mock_denied` shape a prior attempt leaned on and shipped a
/// dead trigger behind.
fn validator_denied(command: &str) -> MockScript {
    let tool_use_line = json!({
        "type": "assistant",
        "message": { "id": "vm1", "content": [
            { "type": "tool_use", "name": "Bash", "input": { "command": command } }
        ] }
    })
    .to_string();
    let denied_line = json!({
        "type": "user",
        "message": { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "vt1",
              "content": format!("Permission denied: Bash({command}) requires approval"),
              "is_error": true }
        ] }
    })
    .to_string();
    let mut events = vec![mock_init("mock-session")];
    events.extend(parse_stream_line(&tool_use_line));
    events.extend(parse_stream_line(&denied_line));
    events.push(mock_result_error(&format!(
        "stopped: `{command}` was denied and the checks could not run"
    )));
    MockScript {
        events,
        ..Default::default()
    }
}

/// Validator script that is DENIED `command` but STILL produces a trusted PASS
/// (clean report) — an incidental denial that did not block validation. The
/// grant flow must NOT park on this (gated on `!validator_outcome_trusted`), or
/// a later deny would wrongly block a milestone that actually passed.
fn validator_denied_but_passing(command: &str) -> MockScript {
    let tool_use_line = json!({
        "type": "assistant",
        "message": { "id": "vm2", "content": [
            { "type": "tool_use", "name": "Bash", "input": { "command": command } }
        ] }
    })
    .to_string();
    let denied_line = json!({
        "type": "user",
        "message": { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "vt2",
              "content": format!("Permission denied: Bash({command}) requires approval"),
              "is_error": true }
        ] }
    })
    .to_string();
    let report = json!({ "findings": [], "summary": "validated despite one denied probe" });
    let mut events = vec![mock_init("mock-session")];
    events.extend(parse_stream_line(&tool_use_line));
    events.extend(parse_stream_line(&denied_line));
    events.push(mock_text(&report.to_string()));
    events.push(mock_result_json(&report));
    MockScript {
        events,
        ..Default::default()
    }
}

/// Validator script that produces NO trusted report and NO capturable denial
/// (a plain error). Models a primary backend whose failure the runner can't map
/// to a command (e.g. a Droid validator, which emits no tool events) — so the
/// grant, if any, can only be detected on the Claude retry.
fn validator_untrusted_no_denial() -> MockScript {
    MockScript {
        events: vec![
            mock_init("mock-session"),
            mock_result_error("stopped: backend produced no trusted report"),
        ],
        ..Default::default()
    }
}

/// Validator script whose `fs+net` session is REFUSED `host:port` by the run's
/// egress proxy and, blocked on it, fails to produce a trusted report (result
/// error, no parseable report) — the egress analogue of [`validator_denied`],
/// gated on the same `!validator_outcome_trusted`. The CONNECT goes through
/// the real proxy the runner wires into the session env (`connects_via_proxy`),
/// so the denial reaches `RunOutcome.denied_egress` via the actual 3.3a signal
/// path, not a fabricated outcome field. macOS-only: only macOS resolves
/// `fs+net` to Seatbelt + proxy (Linux bwrap spawns no proxy — 3.3a).
#[cfg(target_os = "macos")]
fn validator_egress_denied(host: &str, port: u16) -> MockScript {
    MockScript {
        events: vec![
            mock_init("mock-session"),
            mock_result_error(&format!(
                "stopped: egress to `{host}:{port}` was denied and the checks could not run"
            )),
        ],
        ..Default::default()
    }
    .connects_via_proxy(host, port)
}

/// Poll the state snapshot until a grant request is parked (mirrors the
/// pause/resume test's snapshot poll — `emit` keeps state.json in lockstep).
async fn wait_for_pending_grant(paths: &MissionPaths) {
    for _ in 0..400 {
        if let Ok(snap) = reducer::read_snapshot(&paths.state_file()) {
            if snap.pending_grant_request.is_some() {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting for a parked grant request");
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

/// Dirty-tree-turn reply (§4.4): the worker left uncommitted changes; commit
/// them as-is so they land on the mission branch.
fn dirty_tree_commit_as_is() -> String {
    json!({ "action": "commit-as-is", "note": "worker delivered files" }).to_string()
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

/// Capture-turn reply (§ cross-mission lesson capture): nothing worth
/// carrying forward, so the completion path's capture turn writes nothing.
fn no_lesson() -> String {
    "NONE".to_string()
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

/// Conversion-turn reply that marks the given subject an author-broken
/// command assertion (escalate-to-operator route).
fn command_broken_reply(subject: &str, diagnosis: &str) -> String {
    json!({
        "fixFeatures": [],
        "waived": [],
        "commandBroken": [{ "subject": subject, "diagnosis": diagnosis }],
        "summary": "escalating a possibly author-broken assertion"
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
        negative_control: None,
        pty_script: None,
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
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            verdicts_pass(&["a-2"]),
            "Always add a regression test alongside the fix it covers.".to_string(),
        ]),
        worker_pass(),
        validator_with(json!([])),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(2, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
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
    assert!(
        tags.contains(&tag_name),
        "tag {tag_name} missing from: {tags}"
    );
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::MilestoneCompleted { tag: Some(t), .. } if *t == tag_name
    )));

    // Both features completed, each with a real checkpoint commit (the mock
    // workers write a file and the §4.4 discipline commits it as-is).
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::FeatureCompleted { feature_id, commits } if feature_id == "f-1-2" && !commits.is_empty()
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
        root.join(".kranz")
            .join("missions")
            .join(&mission_id)
            .join("report.md"),
    )
    .expect("report.md written at completion");
    assert!(
        report.starts_with(&format!("# Mission report — {mission_id}")),
        "{report}"
    );
    assert!(report.contains("## What shipped"), "{report}");
    assert!(report.contains("## Workspace"), "{report}");
    assert!(report.contains("**Isolation:** `checkout`"), "{report}");
    assert!(report.contains("**Worker/validator cwd:**"), "{report}");
    assert!(report.contains("**Sandbox:**"), "{report}");
    assert!(report.contains("**Preflight:**"), "{report}");
    assert!(report.contains("feature 1"), "{report}");
    assert!(report.contains("## Validation history"), "{report}");
    assert!(report.contains("actual vs"), "{report}");
    assert!(report.contains("## Contract outcomes"), "{report}");
    assert!(
        report.contains("**[a-2]**"),
        "both assertions listed: {report}"
    );

    // The completion report reuses the estimate persisted at approval (M1
    // review P2), not one recomputed against a since-changed corpus/config:
    // estimate.json exists and the report cites its exact expected figure.
    let approved: kranz_engine::cost::CostEstimate = serde_json::from_str(
        &std::fs::read_to_string(paths.estimate_file())
            .expect("estimate.json persisted at approval"),
    )
    .unwrap();
    assert!(
        report.contains(&format!("(expected ${:.2})", approved.expected_usd)),
        "report must cite the approved estimate (expected ${:.2}): {report}",
        approved.expected_usd
    );

    let subject = raw_git(&root, &["log", "-1", "--format=%s"]);
    assert_eq!(
        subject.trim(),
        format!("[kranz] mission report for {mission_id}")
    );
    let trailers = interpret_head_trailers(&root);
    assert!(
        trailers.contains(&format!("Kranz-Mission: {mission_id}")),
        "{trailers}"
    );
    assert!(
        trailers.contains(&format!("Kranz-Cost-USD: {:.4}", state.total_cost_usd)),
        "{trailers}"
    );
    assert!(
        trailers.contains(&format!("Kranz-Tokens-Input: {}", state.totals.input))
            && trailers.contains(&format!("Kranz-Tokens-Output: {}", state.totals.output))
            && trailers.contains(&format!(
                "Kranz-Tokens-Cache-Read: {}",
                state.totals.cache_read
            ))
            && trailers.contains(&format!(
                "Kranz-Tokens-Cache-Write: {}",
                state.totals.cache_write
            )),
        "{trailers}"
    );
    let files = raw_git(&root, &["show", "--name-only", "--format=", "HEAD"]);
    let mut files: Vec<&str> = files.lines().filter(|l| !l.trim().is_empty()).collect();
    files.sort_unstable();
    assert_eq!(
        files,
        vec![
            ".kranz/lessons/index.md".to_string(),
            format!(".kranz/lessons/{mission_id}.md"),
            ".kranz/missions/index.md".to_string(),
            format!(".kranz/missions/{mission_id}/report.md"),
        ],
        "the FINDINGS-EMPTY completion's prose lesson lands in the SAME report commit \
         as report.md (not a separate commit)"
    );
    let lesson = std::fs::read_to_string(
        root.join(".kranz")
            .join("lessons")
            .join(format!("{mission_id}.md")),
    )
    .expect("lesson file written");
    assert!(lesson.contains("regression test"), "{lesson}");
    let lessons_index =
        std::fs::read_to_string(root.join(".kranz").join("lessons").join("index.md")).unwrap();
    assert!(
        lessons_index.contains(&format!("{mission_id}.md")),
        "{lessons_index}"
    );

    // The mission's index line kept its format and gained the report link.
    let index =
        std::fs::read_to_string(root.join(".kranz").join("missions").join("index.md")).unwrap();
    let line = index
        .lines()
        .find(|l| l.contains(&format!("[{mission_id}](")))
        .expect("mission line in index.md");
    assert!(
        line.contains(&format!("({mission_id}/plan.md)")),
        "plan link kept: {line}"
    );
    assert!(
        line.contains(&format!("[report]({mission_id}/report.md)")),
        "report link: {line}"
    );
}

// ---------------------------------------------------------------------------
// 1b. Frontier provenance: every real worker.spawned emit site (orchestrator
// self-spawn + runner worker/validator spawns) records the frontier regime —
// quant "n/a", no weight hash — for the CLI backends (Claude/Codex/Droid are
// all frontier; the mock backend here stands in for them).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn completed_mission_worker_spawns_record_frontier_provenance() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);

    let spawns: Vec<_> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::WorkerSpawned {
                run_id,
                quant,
                weight_hash,
                ..
            } => Some((run_id.as_str(), quant.as_str(), weight_hash.clone())),
            _ => None,
        })
        .collect();
    assert!(
        !spawns.is_empty(),
        "at least one worker.spawned must be on the log"
    );
    for (run_id, quant, weight_hash) in &spawns {
        assert_eq!(
            *quant, "n/a",
            "run {run_id} must record the frontier quant sentinel"
        );
        assert_eq!(
            *weight_hash, None,
            "run {run_id} must record no weight hash (frontier regime)"
        );
    }

    // Same holds folded onto state: every WorkerRun agrees with its spawn event.
    let state = reducer::fold(&events).unwrap();
    for (run_id, _, _) in &spawns {
        let run = state.runs.get(*run_id).expect("run recorded");
        assert_eq!(run.quant, "n/a");
        assert_eq!(run.weight_hash, None);
    }
}

// ---------------------------------------------------------------------------
// 1b. Workspace contract (D-A): approve fails closed on an invalid contract
// ---------------------------------------------------------------------------

/// Present-but-invalid `.kranz/workspace.json` refuses approval with the
/// repo-setup owner named, BEFORE any branch/commit side effects.
#[tokio::test(flavor = "multi_thread")]
async fn invalid_workspace_contract_refused_at_approve() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(
        &root,
        r#"{"schemaVersion": 1, "secrets": ["sk-live-value-not-a-name"]}"#,
    );

    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    let err = engine
        .approve_plan(simple_plan(1, vec![]))
        .expect_err("invalid workspace contract must refuse approval");
    let msg = err.to_string();
    assert!(msg.contains("workspace contract"), "{msg}");
    assert!(msg.contains("repo-setup"), "{msg}");
    assert!(msg.contains("not a secret NAME"), "{msg}");

    // Fail-closed means no side effects: no mission branch, no approval event.
    let branches = raw_git(&root, &["branch", "--list"]);
    assert!(
        !branches.contains("kranz/mission-"),
        "refused approve must not create the mission branch: {branches}"
    );
    assert_eq!(
        raw_git(&root, &["branch", "--show-current"]).trim(),
        "main",
        "refused approve must not move the primary checkout"
    );
    let events = read_log(&engine.paths().clone());
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::PlanApproved { .. })),
        "refused approve must not emit plan.approved"
    );
}

/// Missing contract ⇒ today's behavior exactly: approval proceeds.
#[tokio::test(flavor = "multi_thread")]
async fn no_workspace_contract_approves_unchanged() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    assert!(
        !root.join(".kranz/workspace.json").exists(),
        "fixture must start without a contract"
    );

    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine
        .approve_plan(simple_plan(1, vec![]))
        .expect("approve without a contract behaves as today");
    let events = read_log(&engine.paths().clone());
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, EventKind::PlanApproved { .. })),
        "plan.approved must land on the log"
    );
}

/// A valid contract is accepted at approve (validation is not over-eager).
#[tokio::test(flavor = "multi_thread")]
async fn valid_workspace_contract_approves() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "bootstrap": ["cargo fetch"],
            "services": [{"name": "db", "start": "docker compose up db", "port": {"policy": {"fixed": 5432}}}],
            "readiness": ["pg_isready"],
            "previews": [{"name": "app", "urlTemplate": "http://localhost:{port}/"}],
            "secrets": ["DATABASE_URL"],
            "mounts": ["/var/cache/cargo"]
        }"#,
    );

    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine
        .approve_plan(simple_plan(1, vec![]))
        .expect("valid workspace contract must approve");
}

// ---------------------------------------------------------------------------
// 1b2. Workspace provider pin at approval (D-B, ticket
// workspace-provider-pin-at-approval): the EFFECTIVE provider identity —
// kind + template + version — is pinned into the log and state at approval,
// immediately before plan.approved. Unknown provider names refuse approval
// BEFORE any side effect (owner: operator). Local-worktree-only missions pin
// too: the default made explicit (D-H).
// ---------------------------------------------------------------------------

/// The pin event fires at approval, immediately before plan.approved, and
/// folds into state — without a contract the version is the honest "none"
/// (never an implied schema).
#[tokio::test(flavor = "multi_thread")]
async fn workspace_provider_pin_recorded_at_approval() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    assert!(!root.join(".kranz/workspace.json").exists());

    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine
        .approve_plan(simple_plan(1, vec![]))
        .expect("approve pins the provider");

    // Folded into state (checkout isolation, no contract ⇒ "none").
    let pin = engine
        .state()
        .workspace_pin
        .as_ref()
        .expect("the pin folded into mission state");
    assert_eq!(pin.provider, "local-worktree");
    assert_eq!(pin.template, "checkout");
    assert_eq!(pin.version, "none");
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let pin_pos = events
        .iter()
        .position(|e| matches!(e.kind, EventKind::WorkspaceProviderPinned { .. }))
        .expect("workspace.provider.pinned on the log");
    let approved_pos = events
        .iter()
        .position(|e| matches!(e.kind, EventKind::PlanApproved { .. }))
        .expect("plan.approved on the log");
    assert_eq!(
        approved_pos,
        pin_pos + 1,
        "the log reads: provider pinned → plan approved"
    );
    match &events[pin_pos].kind {
        EventKind::WorkspaceProviderPinned {
            provider,
            template,
            version,
        } => {
            assert_eq!(provider, "local-worktree");
            assert_eq!(template, "checkout");
            assert_eq!(version, "none");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

/// With a base-branch workspace contract, the pin records the contract's
/// schemaVersion as its version.
#[tokio::test(flavor = "multi_thread")]
async fn workspace_provider_pin_with_contract_records_schema_version() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(
        &root,
        r#"{"schemaVersion": 1, "readiness": ["pg_isready"]}"#,
    );

    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine
        .approve_plan(simple_plan(1, vec![]))
        .expect("approve with a contract pins the provider");
    let pin = engine
        .state()
        .workspace_pin
        .as_ref()
        .expect("the pin folded into mission state");
    assert_eq!(pin.provider, "local-worktree");
    assert_eq!(pin.template, "checkout");
    assert_eq!(
        pin.version, "1",
        "the contract's schemaVersion is the pin version"
    );
}

/// An unknown `workspace.provider` name refuses approval — the same
/// fail-closed resolution the seam applies at run start, but at APPROVAL
/// time, before any branch/commit side effect. Mirrors
/// `invalid_workspace_contract_refused_at_approve`'s zero-side-effect
/// assertions. Owner: operator, with the config key named.
#[tokio::test(flavor = "multi_thread")]
async fn unknown_workspace_provider_refused_at_approve() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::new());
    let cfg = MissionConfig {
        workspace: WorkspaceConfig {
            provider: Some("codr".to_string()), // misspelled — never silently defaults
            ..Default::default()
        },
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    let err = engine
        .approve_plan(simple_plan(1, vec![]))
        .expect_err("an unknown workspace provider must refuse approval");
    let msg = err.to_string();
    assert!(msg.contains("workspace.provider"), "{msg}");
    assert!(msg.contains("\"codr\""), "{msg}");
    assert!(msg.contains("owner: operator"), "{msg}");

    // Fail-closed means no side effects: no mission branch, no approval
    // event, no pin event.
    let branches = raw_git(&root, &["branch", "--list"]);
    assert!(
        !branches.contains("kranz/mission-"),
        "refused approve must not create the mission branch: {branches}"
    );
    assert_eq!(
        raw_git(&root, &["branch", "--show-current"]).trim(),
        "main",
        "refused approve must not move the primary checkout"
    );
    let events = read_log(&engine.paths().clone());
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::PlanApproved { .. })),
        "refused approve must not emit plan.approved"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkspaceProviderPinned { .. })),
        "refused approve must not emit workspace.provider.pinned"
    );
}

// ---------------------------------------------------------------------------
// 1b3. Remote workspace provider (D-B implementation #3, ticket
// workspace-remote-coder-provider): pin fields at approval, fail-closed
// config/creds, and the full provision → poll → block/complete flow against
// a loopback Coder-shaped mock (127.0.0.1 only — no live substrate).
// ---------------------------------------------------------------------------

/// Remote mission config pointing at a loopback mock substrate.
fn remote_workspace_cfg(base_url: &str, token_env: &str) -> MissionConfig {
    MissionConfig {
        workspace: WorkspaceConfig {
            provider: Some("remote".to_string()),
            remote: Some(RemoteWorkspaceConfig {
                base_url: Some(base_url.to_string()),
                template: Some("tmpl-baked-ami".to_string()),
                token_env: Some(token_env.to_string()),
                idle_after_hours: None,
            }),
            teardown_mode: None,
        },
        ..test_cfg()
    }
}

/// A loopback Coder-shaped substrate mock: answers create/status/transition
/// calls per the documented wire shape, recording every raw request.
struct MockSubstrate {
    base_url: String,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
}

impl MockSubstrate {
    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

/// Read one HTTP/1.1 request (headers + content-length body) verbatim.
async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = socket.read(&mut chunk).await.expect("read request");
        assert!(n > 0, "connection closed before the full request arrived");
        buf.extend_from_slice(&chunk[..n]);
        if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&buf[..pos]).to_string();
            let content_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if buf.len() >= pos + 4 + content_length {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

/// Spawn the mock on 127.0.0.1, answering `workspace_status` with `status`
/// forever. The task is aborted when the test's runtime shuts down.
fn spawn_mock_substrate(status: &str) -> MockSubstrate {
    spawn_mock_substrate_impl(status, false)
}

/// `fail_transitions`: the stop/delete (`/builds`) transition answers HTTP
/// 500 — the substrate-side teardown failure path (ticket
/// workspace-idle-hibernate).
fn spawn_mock_substrate_impl(status: &str, fail_transitions: bool) -> MockSubstrate {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    listener
        .set_nonblocking(true)
        .expect("nonblocking listener");
    let base_url = format!("http://{}", listener.local_addr().expect("addr"));
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let task_requests = Arc::clone(&requests);
    let status = status.to_string();
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::from_std(listener).expect("tokio listener");
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let request = read_request(&mut socket).await;
            task_requests.lock().unwrap().push(request.clone());
            let (status_line, body) = if request.starts_with("POST /api/v2/users/me/workspaces ") {
                ("200 OK", r#"{"id":"ws-m1","urls":[{"name":"app","url":"https://app--m-1.coder.example.com","auth":true}],"takeover":"https://coder.example.com/@me/ws-m1"}"#.to_string())
            } else if request.starts_with("GET /api/v2/workspaces/") {
                (
                    "200 OK",
                    format!(r#"{{"latest_build":{{"status":"{status}"}}}}"#),
                )
            } else if request.starts_with("POST /api/v2/workspaces/")
                && request.contains("/builds ")
            {
                if fail_transitions {
                    (
                        "500 Internal Server Error",
                        r#"{"error":"substrate transition failed"}"#.to_string(),
                    )
                } else {
                    ("200 OK", "{}".to_string())
                }
            } else {
                task_requests
                    .lock()
                    .unwrap()
                    .push(format!("UNEXPECTED: {request}"));
                ("200 OK", "{}".to_string())
            };
            let response = format!(
                "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            if socket.write_all(response.as_bytes()).await.is_err() {
                return;
            }
        }
    });
    MockSubstrate { base_url, requests }
}

/// The remote pin lands at approval with all three fields — provider
/// `"remote"`, the CONFIGURED template/image id, the adapter version —
/// immediately before plan.approved, with NO substrate contact and NO token
/// env var needed (approval-time pinning stays pure).
#[tokio::test(flavor = "multi_thread")]
async fn remote_workspace_provider_pin_recorded_at_approval() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(&root, r#"{"schemaVersion": 1, "readiness": ["exit 0"]}"#);

    let backend = Arc::new(MockBackend::new());
    let cfg = remote_workspace_cfg(
        "https://coder.internal.example.com",
        "KRANZ_TEST_REMOTE_TOKEN_PIN_NEVER_READ",
    );
    let mut engine = make_engine(&backend, &root, cfg);
    engine
        .approve_plan(simple_plan(1, vec![]))
        .expect("approve pins the remote provider without contacting a substrate");

    let pin = engine
        .state()
        .workspace_pin
        .as_ref()
        .expect("the pin folded into mission state");
    assert_eq!(pin.provider, "remote");
    assert_eq!(pin.template, "tmpl-baked-ami", "the configured template id");
    assert_eq!(
        pin.version, "coder-v1",
        "the adapter version, not a contract schema"
    );
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let pin_pos = events
        .iter()
        .position(|e| matches!(e.kind, EventKind::WorkspaceProviderPinned { .. }))
        .expect("workspace.provider.pinned on the log");
    let approved_pos = events
        .iter()
        .position(|e| matches!(e.kind, EventKind::PlanApproved { .. }))
        .expect("plan.approved on the log");
    assert_eq!(
        approved_pos,
        pin_pos + 1,
        "the log reads: provider pinned → plan approved"
    );
    match &events[pin_pos].kind {
        EventKind::WorkspaceProviderPinned {
            provider,
            template,
            version,
        } => {
            assert_eq!(provider, "remote");
            assert_eq!(template, "tmpl-baked-ami");
            assert_eq!(version, "coder-v1");
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

/// Incomplete `workspace.remote.*` config refuses APPROVAL — the missing key
/// named, owner operator — before any branch/commit/event side effect
/// (mirrors `unknown_workspace_provider_refused_at_approve`).
#[tokio::test(flavor = "multi_thread")]
async fn remote_workspace_incomplete_config_refused_at_approve() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::new());
    let cfg = MissionConfig {
        workspace: WorkspaceConfig {
            provider: Some("remote".to_string()),
            remote: Some(RemoteWorkspaceConfig {
                base_url: None, // missing — must be named in the refusal
                template: Some("tmpl-baked-ami".to_string()),
                token_env: Some("CODER_SESSION_TOKEN".to_string()),
                idle_after_hours: None,
            }),
            teardown_mode: None,
        },
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    let err = engine
        .approve_plan(simple_plan(1, vec![]))
        .expect_err("incomplete remote config must refuse approval");
    let msg = err.to_string();
    assert!(msg.contains("workspace.remote.baseUrl"), "{msg}");
    assert!(msg.contains("owner: operator"), "{msg}");
    assert!(
        msg.contains("refusing rather than silently falling back"),
        "{msg}"
    );

    // Fail-closed means no side effects: no mission branch, no approval
    // event, no pin event.
    let branches = raw_git(&root, &["branch", "--list"]);
    assert!(
        !branches.contains("kranz/mission-"),
        "refused approve must not create the mission branch: {branches}"
    );
    let events = read_log(&engine.paths().clone());
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::PlanApproved { .. })),
        "refused approve must not emit plan.approved"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkspaceProviderPinned { .. })),
        "refused approve must not emit workspace.provider.pinned"
    );
}

/// A substrate that reports ready: the mission runs to completion; the
/// provisioned event carries the takeover URL and name-matched previews
/// (with the substrate's auth report); readiness is honestly recorded as
/// substrate-reported only — the contract's bootstrap/readiness commands
/// NEVER execute (proven by the marker and the absent gate decisions).
#[tokio::test(flavor = "multi_thread")]
async fn remote_workspace_ready_mission_completes_and_records_substrate_urls() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "bootstrap": ["echo must-not-run > .remote-bootstrap-marker"],
            "readiness": ["exit 99"],
            "previews": [{"name": "app", "urlTemplate": "http://localhost:{port}/"}],
            "secrets": ["DATABASE_URL"]
        }"#,
    );

    std::env::set_var("KRANZ_TEST_REMOTE_TOKEN_READY", "test-token-ready");
    let substrate = spawn_mock_substrate("running");
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let cfg = remote_workspace_cfg(&substrate.base_url, "KRANZ_TEST_REMOTE_TOKEN_READY");
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    assert!(
        !root.join(".remote-bootstrap-marker").exists(),
        "contract bootstrap/readiness never executes on the remote substrate in v1"
    );
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let (takeover, previews, detail) = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::WorkspaceProvisioned {
                provider,
                takeover,
                previews,
                detail,
                ..
            } if provider == "remote" => Some((takeover.clone(), previews.clone(), detail.clone())),
            _ => None,
        })
        .expect("workspace.provisioned (remote) on the log");
    assert_eq!(
        takeover.as_deref(),
        Some("https://coder.example.com/@me/ws-m1"),
        "the substrate's takeover URL rides the provisioned event"
    );
    assert_eq!(
        previews,
        Some(vec![ProvisionedPreview {
            name: "app".to_string(),
            url: "https://app--m-1.coder.example.com".to_string(),
            auth: Some(true),
        }]),
        "the substrate-reported preview URL, name-matched, with its auth report"
    );
    assert!(
        detail.as_deref().unwrap().contains("DATABASE_URL"),
        "the injected secret NAMES are recorded (never values): {detail:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::WorkspaceReadinessReport { outcome, .. } if outcome == "ready"
        )),
        "substrate-reported readiness recorded"
    );
    let remote_lines = gate_decisions(&events, "workspace remote:");
    assert_eq!(remote_lines.len(), 1);
    assert!(
        remote_lines[0].contains("substrate-reported readiness only"),
        "honest wording — no implied contract gate: {}",
        remote_lines[0]
    );
    assert!(
        gate_decisions(&events, "workspace bootstrap:").is_empty()
            && gate_decisions(&events, "workspace readiness:").is_empty(),
        "no gate phase lines on the remote path (the commands never ran)"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::WorkspaceTeardown { mode, .. } if mode == "keep"
        )),
        "Keep teardown recorded (the workspace stays live for takeover)"
    );

    // The wire calls: create with the pinned template + secret NAMES, one
    // status poll, and NO stop/delete (Keep).
    let requests = substrate.requests();
    let create = requests
        .iter()
        .find(|r| r.starts_with("POST /api/v2/users/me/workspaces "))
        .expect("the create call");
    assert!(
        create.contains(r#""template_id":"tmpl-baked-ami""#),
        "{create}"
    );
    assert!(
        create.contains(r#""env_names":["DATABASE_URL"]"#),
        "{create}"
    );
    assert!(
        requests
            .iter()
            .any(|r| r.starts_with("GET /api/v2/workspaces/ws-m1 ")),
        "the readiness poll: {requests:?}"
    );
    assert!(
        !requests.iter().any(|r| r.contains("/builds ")),
        "Keep ⇒ no stop/delete transition: {requests:?}"
    );
    assert!(
        !requests.iter().any(|r| r.starts_with("UNEXPECTED:")),
        "no unexpected substrate calls: {requests:?}"
    );
}

/// A substrate that reports FAILED: the mission BLOCKS with the
/// provider-owned shape — `workspace provider:` + owner `provider`, never
/// `repo-setup` — and no worker ever spawns (no spend on a failed
/// workspace).
#[tokio::test(flavor = "multi_thread")]
async fn remote_workspace_failed_status_blocks_with_provider_owner() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "previews": [{"name": "app", "urlTemplate": "http://localhost:{port}/"}],
            "secrets": ["DATABASE_URL"]
        }"#,
    );

    std::env::set_var("KRANZ_TEST_REMOTE_TOKEN_FAILED", "test-token-failed");
    let substrate = spawn_mock_substrate("failed");
    // No scripts queued: ANY session start would error the run — the empty
    // backend itself proves no spend.
    let backend = Arc::new(MockBackend::new());
    let cfg = remote_workspace_cfg(&substrate.base_url, "KRANZ_TEST_REMOTE_TOKEN_FAILED");
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);
    assert_eq!(
        engine.state().mission.milestones[0].status,
        MilestoneStatus::Blocked
    );
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let reason = gate_block_reason(&events, "ms-1");
    assert!(
        reason.starts_with("workspace provider:"),
        "the provider-owned block shape: {reason}"
    );
    assert!(reason.contains("owner: provider"), "{reason}");
    assert!(
        reason.contains("kranz-remote-"),
        "names the workspace: {reason}"
    );
    assert!(
        !reason.contains("repo-setup") && !reason.contains("owner: operator"),
        "distinct from the contract and config owners: {reason}"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::WorkspaceReadinessReport { outcome, .. } if outcome == "failed"
        )),
        "the readiness report records the provider failure"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkerSpawned { .. })),
        "no spend on a failed workspace"
    );
    assert!(
        substrate
            .requests()
            .iter()
            .any(|r| r.starts_with("POST /api/v2/users/me/workspaces ")),
        "the substrate was asked to create the workspace"
    );
}

/// Complete config but an UNSET token env var: provision fails closed at run
/// start (before spend) naming the env var NAME and the config key — never a
/// silent fallback to local.
#[tokio::test(flavor = "multi_thread")]
async fn remote_workspace_missing_token_fails_closed_at_run_start() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(&root, r#"{"schemaVersion": 1, "readiness": ["exit 0"]}"#);

    let var = "KRANZ_TEST_REMOTE_TOKEN_NEVER_SET";
    std::env::remove_var(var); // defensive: prove unset
    let backend = Arc::new(MockBackend::new());
    let cfg = remote_workspace_cfg("http://127.0.0.1:1", var);
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let err = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .expect_err("missing creds fail closed at provision");
    let msg = err.to_string();
    assert!(msg.contains(var), "names the env var NAME: {msg}");
    assert!(msg.contains("workspace.remote.tokenEnv"), "{msg}");
    assert!(msg.contains("owner: operator"), "{msg}");
    let events = read_log(&engine.paths().clone());
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkspaceProvisioned { .. })),
        "no workspace was provisioned"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkerSpawned { .. })),
        "no spend"
    );
}

// ---------------------------------------------------------------------------
// 1b4. Workspace idle-hibernate / destroy lifecycle (ticket
// workspace-idle-hibernate): the engine drives the configured
// `workspace.teardownMode` at TERMINAL states (keep default; local-worktree
// always keep), records the outcome on `workspace.teardown.state`, folds it
// into `state.workspace_lifecycle` with the event ts, and passes a
// configured `workspace.remote.idleAfterHours` VALUE through to the
// substrate at create (the substrate owns idle scheduling — kranz never
// schedules VMs).
// ---------------------------------------------------------------------------

/// Remote mission config with a terminal teardown mode and/or a
/// substrate-side idle policy (ticket workspace-idle-hibernate).
fn remote_workspace_cfg_teardown(
    base_url: &str,
    token_env: &str,
    teardown_mode: Option<&str>,
    idle_after_hours: Option<f64>,
) -> MissionConfig {
    MissionConfig {
        workspace: WorkspaceConfig {
            provider: Some("remote".to_string()),
            remote: Some(RemoteWorkspaceConfig {
                base_url: Some(base_url.to_string()),
                template: Some("tmpl-baked-ami".to_string()),
                token_env: Some(token_env.to_string()),
                idle_after_hours,
            }),
            teardown_mode: teardown_mode.map(str::to_string),
        },
        ..test_cfg()
    }
}

/// The latest `workspace.teardown` on the log as (mode, state, ts).
fn teardown_outcome(events: &[Event]) -> (String, Option<String>, chrono::DateTime<chrono::Utc>) {
    let teardown = workspace_lifecycle_events(events, "workspace.teardown");
    assert_eq!(teardown.len(), 1, "exactly one teardown per run()");
    match &teardown[0].kind {
        EventKind::WorkspaceTeardown { mode, state } => {
            (mode.clone(), state.clone(), teardown[0].ts)
        }
        other => panic!("wrong variant: {other:?}"),
    }
}

/// Terminal hibernate on the remote provider: the engine stops the
/// substrate workspace, records mode+state on the event, folds the
/// lifecycle with the event's own ts, and passes the configured
/// idleAfterHours VALUE through at create (recorded, substrate-owned).
#[tokio::test(flavor = "multi_thread")]
async fn remote_workspace_terminal_hibernate_stops_the_workspace_and_records_lifecycle() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "readiness": ["exit 0"],
            "secrets": ["DATABASE_URL"]
        }"#,
    );

    std::env::set_var("KRANZ_TEST_REMOTE_TOKEN_HIBERNATE", "test-token-hibernate");
    let substrate = spawn_mock_substrate("running");
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let cfg = remote_workspace_cfg_teardown(
        &substrate.base_url,
        "KRANZ_TEST_REMOTE_TOKEN_HIBERNATE",
        Some("hibernate"),
        Some(24.0),
    );
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    let lifecycle = engine
        .state()
        .workspace_lifecycle
        .clone()
        .expect("the teardown outcome folded into state");
    assert_eq!(lifecycle.state, "stopped");
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let (mode, state, ts) = teardown_outcome(&events);
    assert_eq!(mode, "hibernate");
    assert_eq!(state.as_deref(), Some("stopped"));
    assert_eq!(
        lifecycle.ts, ts,
        "the folded lifecycle ts IS the transition event's own ts (workspace-hours anchor)"
    );
    let detail = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::WorkspaceProvisioned {
                provider, detail, ..
            } if provider == "remote" => detail.clone(),
            _ => None,
        })
        .expect("workspace.provisioned (remote) on the log");
    assert!(
        detail.contains("idle policy: hibernate after 24h (substrate-owned)"),
        "the provisioned event records the substrate-owned idle policy: {detail:?}"
    );

    // The wire: create carried the idle policy VALUE; exactly one
    // transition — stop (hibernate), never delete.
    let requests = substrate.requests();
    let create = requests
        .iter()
        .find(|r| r.starts_with("POST /api/v2/users/me/workspaces "))
        .expect("the create call");
    assert!(
        create.contains(r#""idle_after_hours":24.0"#),
        "the idle policy VALUE rides the create call: {create}"
    );
    let transitions: Vec<&String> = requests.iter().filter(|r| r.contains("/builds ")).collect();
    assert_eq!(transitions.len(), 1, "exactly one transition: {requests:?}");
    assert!(
        transitions[0].contains(r#""transition":"stop""#),
        "hibernate is the stop transition: {transitions:?}"
    );
    assert!(
        !requests.iter().any(|r| r.starts_with("UNEXPECTED:")),
        "no unexpected substrate calls: {requests:?}"
    );
}

/// Terminal destroy on the remote provider: the substrate workspace is
/// deleted and the outcome recorded/folded as destroyed.
#[tokio::test(flavor = "multi_thread")]
async fn remote_workspace_terminal_destroy_deletes_the_workspace_and_records_lifecycle() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(&root, r#"{"schemaVersion": 1, "readiness": ["exit 0"]}"#);

    std::env::set_var("KRANZ_TEST_REMOTE_TOKEN_DESTROY", "test-token-destroy");
    let substrate = spawn_mock_substrate("running");
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let cfg = remote_workspace_cfg_teardown(
        &substrate.base_url,
        "KRANZ_TEST_REMOTE_TOKEN_DESTROY",
        Some("destroy"),
        None,
    );
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    let lifecycle = engine
        .state()
        .workspace_lifecycle
        .clone()
        .expect("the teardown outcome folded into state");
    assert_eq!(lifecycle.state, "destroyed");
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let (mode, state, _) = teardown_outcome(&events);
    assert_eq!(mode, "destroy");
    assert_eq!(state.as_deref(), Some("destroyed"));

    let requests = substrate.requests();
    let transitions: Vec<&String> = requests.iter().filter(|r| r.contains("/builds ")).collect();
    assert_eq!(transitions.len(), 1, "exactly one transition: {requests:?}");
    assert!(
        transitions[0].contains(r#""transition":"delete""#),
        "destroy is the delete transition: {transitions:?}"
    );
    assert!(
        !requests.iter().any(|r| r.starts_with("UNEXPECTED:")),
        "no unexpected substrate calls: {requests:?}"
    );
}

/// A substrate-side teardown failure NEVER masks the mission's terminal
/// outcome: the run still completes, the failure is logged as a decision
/// (reason in the detail), and the outcome folds as `failed` — the
/// workspace may still be live, which cost tooling must see.
#[tokio::test(flavor = "multi_thread")]
async fn remote_workspace_teardown_failure_keeps_the_terminal_outcome() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(&root, r#"{"schemaVersion": 1, "readiness": ["exit 0"]}"#);

    std::env::set_var(
        "KRANZ_TEST_REMOTE_TOKEN_TEARDOWN_FAIL",
        "test-token-teardown-fail",
    );
    let substrate = spawn_mock_substrate_impl("running", true);
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let cfg = remote_workspace_cfg_teardown(
        &substrate.base_url,
        "KRANZ_TEST_REMOTE_TOKEN_TEARDOWN_FAIL",
        Some("destroy"),
        None,
    );
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(
        status,
        MissionStatus::Complete,
        "the teardown failure must not change the mission's terminal outcome"
    );
    let lifecycle = engine
        .state()
        .workspace_lifecycle
        .clone()
        .expect("the failed outcome still folds");
    assert_eq!(lifecycle.state, "failed");
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let (mode, state, _) = teardown_outcome(&events);
    assert_eq!(mode, "destroy");
    assert_eq!(state.as_deref(), Some("failed"));
    assert!(
        events
            .iter()
            .any(|e| matches!(e.kind, EventKind::MissionCompleted {})),
        "mission.completed still on the log"
    );

    // The failure is logged as a decision with the scrubbed reason in the
    // detail — never silently swallowed.
    let decision = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::OrchestratorDecision { summary, detail }
                if summary.starts_with("workspace teardown (destroy) failed") =>
            {
                Some((summary.clone(), detail.clone()))
            }
            _ => None,
        })
        .expect("the teardown failure decision: {events:?}");
    assert!(
        decision.0.contains("the mission outcome stands"),
        "{}",
        decision.0
    );
    assert!(decision.0.contains("owner: operator"), "{}", decision.0);
    let detail = decision.1.expect("the reason rides the decision detail");
    assert!(
        detail.contains("delete_workspace") && detail.contains("HTTP 500"),
        "the scrubbed provider reason: {detail}"
    );

    // The substrate WAS asked to delete (and refused) — no silent skip.
    let requests = substrate.requests();
    assert!(
        requests
            .iter()
            .any(|r| r.contains("/builds ") && r.contains(r#""transition":"delete""#)),
        "the delete transition was attempted: {requests:?}"
    );
}

/// Local-worktree is ALWAYS keep, even with a configured teardown mode —
/// the integration worktree's filesystem lifecycle belongs to the
/// mission-branch/merge machinery, so a configured destroy records an
/// honest keep (local-worktree semantics unchanged).
#[tokio::test(flavor = "multi_thread")]
async fn workspace_teardown_mode_local_worktree_is_always_keep() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let cfg = MissionConfig {
        workspace: WorkspaceConfig {
            teardown_mode: Some("destroy".to_string()),
            ..Default::default()
        },
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    let lifecycle = engine
        .state()
        .workspace_lifecycle
        .clone()
        .expect("the keep outcome folds");
    assert_eq!(lifecycle.state, "kept");
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let (mode, state, _) = teardown_outcome(&events);
    assert_eq!(
        mode, "keep",
        "local-worktree NEVER destroys: a configured destroy records an honest keep"
    );
    assert_eq!(state.as_deref(), Some("kept"));
}

/// An unknown `workspace.teardownMode` fails closed at run start — naming
/// the config key and the bad value, before any workspace event or spend
/// (mirrors `workspace_provider_unknown_name_fails_closed_at_run_start`:
/// the bad value arrives via a post-approval config.changed patch).
#[tokio::test(flavor = "multi_thread")]
async fn workspace_teardown_mode_unknown_fails_closed_at_run_start() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Approve clean. No scripts queued: ANY session start would error the
    // run anyway — the empty backend itself proves no spawn.
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();
    drop(engine);

    // Post-approval config drift: set an unknown teardown mode.
    {
        let mut log = EventLog::acquire(&paths, &mission_id, Duration::ZERO, LockForce::No)
            .expect("acquire log");
        log.append(EventKind::ConfigChanged {
            patch: json!({"workspace": {"teardownMode": "purge"}}),
        })
        .expect("append config.changed");
    }

    let backend: Arc<dyn AgentBackend> = backend;
    let mut engine =
        MissionEngine::resume(backend, &root, &mission_id, LockForce::No).expect("resume mission");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);

    let err = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .expect_err("an unknown teardown mode must fail closed");
    let msg = err.to_string();
    assert!(msg.contains("workspace.teardownMode"), "{msg}");
    assert!(msg.contains("\"purge\""), "{msg}");
    assert!(msg.contains("owner: operator"), "{msg}");
    drop(engine);

    let events = read_log(&paths);
    for wire_name in [
        "workspace.provisioned",
        "workspace.readiness",
        "workspace.teardown",
    ] {
        assert!(
            workspace_lifecycle_events(&events, wire_name).is_empty(),
            "no {wire_name} event on a run that failed closed"
        );
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkerSpawned { .. })),
        "no worker may spawn when the teardown mode fails closed"
    );
}

// ---------------------------------------------------------------------------
// 1c. Workspace bootstrap + readiness gate (D-C, ticket
// workspace-bootstrap-preflight): with a committed contract the gate runs in
// the execution cwd before any worker spawns; failures BLOCK with owner
// repo-setup; without a contract nothing changes.
//
// Shell lines stay `sh`/`cmd` portable (CI runs this suite on windows):
// `echo`, `>`, `&&`, `exit`, and `cd` only. File-existence checks go
// through `portable_shell_json` / `file_exists_cmd`: `test` is a POSIX binary
// with no cmd.exe builtin, so inlining it only worked where Git's usr/bin
// happened to be on PATH.
// ---------------------------------------------------------------------------

/// Commit a workspace contract (plus a .gitignore for the gate's marker
/// files, so bootstrap output never dirties the worker's tree) onto the BASE
/// branch BEFORE approve — D-A: the contract is base-branch-owned, and the
/// run-time gate reads the committed base-branch copy
/// (`load_workspace_contract_at_ref`), not the working tree.
/// Rewrite the POSIX snippets these fixtures use into cmd.exe equivalents when
/// the suite runs on Windows, so every `commit_workspace_contract` call site is
/// portable without hand-editing nineteen JSON literals.
///
/// Only the contents of JSON string values are touched (the fixtures contain
/// no escaped quotes), and only a leading `test -f` is rewritten — the shape
/// cmd.exe cannot run. `echo`, `>`, `>>`, `&&`, `cd`, and `exit` already parse
/// in both shells.
fn portable_shell_json(contract_json: &str) -> String {
    if !cfg!(windows) {
        return contract_json.to_string();
    }
    contract_json
        .split('"')
        .enumerate()
        .map(|(index, segment)| {
            if index % 2 == 0 {
                return segment.to_string();
            }
            match segment.strip_prefix("test -f ") {
                None => segment.to_string(),
                Some(rest) => match rest.split_once(" && ") {
                    Some((path, then)) => format!("if exist {path} ({then}) else (exit 1)"),
                    None => format!("if exist {rest} (exit 0) else (exit 1)"),
                },
            }
        })
        .collect::<Vec<_>>()
        .join("\"")
}

fn commit_workspace_contract(root: &Path, contract_json: &str) {
    let contract_json = portable_shell_json(contract_json);
    std::fs::create_dir_all(root.join(".kranz")).unwrap();
    std::fs::write(root.join(".kranz").join("workspace.json"), &contract_json).unwrap();
    std::fs::write(root.join(".gitignore"), ".boot-marker\n.boot-count\n").unwrap();
    raw_git(root, &["add", ".kranz/workspace.json", ".gitignore"]);
    raw_git(root, &["commit", "-m", "workspace contract"]);
}

/// Decision summaries carrying `prefix`, in log order.
fn gate_decisions<'a>(events: &'a [Event], prefix: &str) -> Vec<&'a str> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::OrchestratorDecision { summary, .. } if summary.starts_with(prefix) => {
                Some(summary.as_str())
            }
            _ => None,
        })
        .collect()
}

/// The latest `milestone.blocked` reason for `milestone_id`.
fn gate_block_reason(events: &[Event], milestone_id: &str) -> String {
    events
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EventKind::MilestoneBlocked {
                milestone_id: id,
                reason,
                ..
            } if id == milestone_id => Some(reason.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no milestone.blocked for {milestone_id} on the log"))
}

/// Happy path (D-C): bootstrap echoes into a marker, readiness checks it,
/// and ONLY THEN does the first worker spawn — proven by the decision events
/// preceding the first `worker.spawned`, by the marker the final gate's own
/// command assertion re-checks in the execution cwd, and by the report's
/// Workspace lines.
#[tokio::test(flavor = "multi_thread")]
async fn workspace_bootstrap_readiness_gate_runs_before_workers() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "bootstrap": ["echo boot > .boot-marker"],
            "readiness": ["test -f .boot-marker"]
        }"#,
    );

    // The final gate's command assertion re-proves the marker is visible in
    // the mission execution cwd (it runs engine-side in the same cwd).
    let contract = vec![assertion(
        "a-1",
        "the workspace marker exists",
        Some(&file_exists_cmd(".boot-marker")),
    )];
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();
    drop(engine);

    // Bootstrap ran in the execution cwd (the relative marker landed at the
    // workspace root).
    let marker = std::fs::read_to_string(root.join(".boot-marker"))
        .expect("bootstrap wrote the marker into the workspace cwd");
    assert!(marker.contains("boot"), "{marker}");

    let events = read_log(&paths);
    // Start/pass decisions for both phases, in order, and ALL of them before
    // the first worker.spawned — readiness is a gate, not an afterthought.
    assert_eq!(
        gate_decisions(&events, "workspace bootstrap:"),
        vec![
            "workspace bootstrap: running 1 commands",
            "workspace bootstrap: 1/1 commands ok"
        ]
    );
    assert_eq!(
        gate_decisions(&events, "workspace readiness:"),
        vec![
            "workspace readiness: running 1 checks",
            "workspace readiness: 1/1 checks ok"
        ]
    );
    let readiness_ok_seq = events
        .iter()
        .find(|e| matches!(&e.kind, EventKind::OrchestratorDecision { summary, .. } if summary == "workspace readiness: 1/1 checks ok"))
        .map(|e| e.seq)
        .unwrap();
    assert!(
        readiness_ok_seq < seq_of(&events, "worker.spawned"),
        "readiness must pass before the first worker spawns"
    );

    // report.md's Workspace section carries the gate outcomes (D-H).
    let report = std::fs::read_to_string(
        root.join(".kranz")
            .join("missions")
            .join(&mission_id)
            .join("report.md"),
    )
    .expect("report.md written at completion");
    assert!(
        report.contains("- **Bootstrap:** 1/1 commands ok"),
        "{report}"
    );
    assert!(
        report.contains("- **Readiness:** 1/1 checks ok"),
        "{report}"
    );
}

/// Bootstrap command failure ⇒ the mission BLOCKS honestly (owner
/// repo-setup), naming the failing command and its exit code with a scrubbed
/// output tail — and no worker ever spawns (no spend on a half-ready app).
#[tokio::test(flavor = "multi_thread")]
async fn workspace_bootstrap_failure_blocks_before_any_worker() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "bootstrap": [
                "echo boot > .boot-marker",
                "echo leaking sk-ant-api03-a1b2c3d4e5f6 1>&2 && exit 42"
            ],
            "readiness": ["test -f .boot-marker"]
        }"#,
    );

    // No scripts queued: ANY session start (worker/validator/orchestrator)
    // would error the run — the empty backend itself proves no spawn.
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);
    assert_eq!(engine.state().mission.status, MissionStatus::Blocked);
    assert_eq!(
        engine.state().mission.milestones[0].status,
        MilestoneStatus::Blocked
    );
    let paths = engine.paths().clone();
    drop(engine);

    // Ordered execution: the FIRST bootstrap command ran before the failure.
    assert!(root.join(".boot-marker").exists());

    let events = read_log(&paths);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkerSpawned { .. })),
        "no worker may spawn on a failed bootstrap: {:?}",
        event_types(&events)
    );
    // The never-started milestone is started first, then blocked, so the
    // started → blocked → (later) unblocked event invariant holds.
    assert!(seq_of(&events, "milestone.started") < seq_of(&events, "milestone.blocked"));
    assert_eq!(
        gate_decisions(&events, "workspace bootstrap:"),
        vec![
            "workspace bootstrap: running 2 commands",
            "workspace bootstrap: FAILED at command 2/2 — blocking mission (owner: repo-setup)"
        ]
    );
    // Bootstrap stopped at the first failure: readiness never ran.
    assert!(gate_decisions(&events, "workspace readiness:").is_empty());

    let reason = gate_block_reason(&events, "ms-1");
    assert!(
        reason.contains("workspace gate: bootstrap command 2/2 failed"),
        "{reason}"
    );
    assert!(reason.contains("owner: repo-setup"), "{reason}");
    assert!(reason.contains("exit code 42"), "{reason}");
    assert!(reason.contains("echo leaking"), "{reason}");
    assert!(
        !reason.contains("sk-ant-api03-a1b2c3d4e5f6"),
        "the output tail must be scrubbed: {reason}"
    );
    assert!(reason.contains("[REDACTED]"), "{reason}");
}

/// Readiness failure ⇒ the mission BLOCKS naming the check; bootstrap ran to
/// completion first (its marker exists).
#[tokio::test(flavor = "multi_thread")]
async fn workspace_readiness_failure_blocks_after_bootstrap() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "bootstrap": ["echo boot > .boot-marker"],
            "readiness": ["test -f .no-such-readiness-file"]
        }"#,
    );

    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);
    let paths = engine.paths().clone();
    drop(engine);

    // Bootstrap ran (marker exists) before the readiness gate blocked.
    assert!(root.join(".boot-marker").exists());

    let events = read_log(&paths);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkerSpawned { .. })),
        "no worker may spawn on a failed readiness check"
    );
    assert_eq!(
        gate_decisions(&events, "workspace bootstrap:"),
        vec![
            "workspace bootstrap: running 1 commands",
            "workspace bootstrap: 1/1 commands ok"
        ]
    );
    assert_eq!(
        gate_decisions(&events, "workspace readiness:"),
        vec![
            "workspace readiness: running 1 checks",
            "workspace readiness: FAILED at check 1/1 — blocking mission (owner: repo-setup)"
        ]
    );
    let reason = gate_block_reason(&events, "ms-1");
    assert!(
        reason.contains("workspace gate: readiness check 1/1 failed"),
        "{reason}"
    );
    assert!(reason.contains("owner: repo-setup"), "{reason}");
    assert!(
        reason.contains(&file_exists_cmd(".no-such-readiness-file")),
        "the reason names the failing check: {reason}"
    );
}

/// No contract ⇒ byte-identical pre-gate behavior: no gate decisions on the
/// log, and the report says "source isolation only" (D-H — never imply a
/// runnable environment exists when it does not).
#[tokio::test(flavor = "multi_thread")]
async fn no_contract_run_has_no_workspace_gate_events() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    assert!(
        !root.join(".kranz/workspace.json").exists(),
        "fixture must start without a contract"
    );

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    assert!(gate_decisions(&events, "workspace bootstrap:").is_empty());
    assert!(gate_decisions(&events, "workspace readiness:").is_empty());

    let report = std::fs::read_to_string(
        root.join(".kranz")
            .join("missions")
            .join(&mission_id)
            .join("report.md"),
    )
    .expect("report.md written at completion");
    assert!(
        report.contains("- **Workspace contract:** no workspace contract (source isolation only)"),
        "{report}"
    );
    assert!(!report.contains("- **Bootstrap:**"), "{report}");
    assert!(!report.contains("- **Readiness:**"), "{report}");
}

/// v1 documented behavior: bootstrap runs ONCE PER run() invocation and is
/// idempotent-by-contract — a resume after crash RE-RUNS it. Proven with the
/// kill/resume idiom: the appending bootstrap command leaves one line per
/// run, so two engine lifetimes leave two lines.
#[tokio::test(flavor = "multi_thread")]
async fn workspace_bootstrap_reruns_on_resume_after_crash() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "bootstrap": ["echo run >> .boot-count"],
            "readiness": ["test -f .boot-count"]
        }"#,
    );

    // --- Phase 1: the "crash" (same idiom as kill_and_resume): the worker
    // passes writing nothing, then the judgement turn starves (no on_message
    // batches), the short stall timeout declares the orchestrator dead, and
    // the retry finds no script left — run() errors out mid-feature.
    let backend1 = Arc::new(MockBackend::with_scripts(vec![
        worker_pass_no_write(),
        orch_script(vec![]),
    ]));
    let mut engine = make_engine(&backend1, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    engine.set_orch_stall_timeout(Duration::from_millis(400));
    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();

    timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .expect_err("phase 1 must error out (simulated crash)");
    drop(engine);

    let count = std::fs::read_to_string(root.join(".boot-count"))
        .expect("phase 1 bootstrap wrote the count file");
    assert_eq!(
        count.lines().count(),
        1,
        "phase 1 ran bootstrap once: {count:?}"
    );

    // --- Phase 2: resume. The gate runs again BEFORE the respawned worker —
    // bootstrap is re-run, not remembered (durable readiness state is the
    // provider seam's job, not v1's).
    let backend2 = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let backend2_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend2) as Arc<dyn AgentBackend>;
    let mut engine = MissionEngine::resume(backend2_dyn, &root, &mission_id, LockForce::No)
        .expect("resume mission");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    drop(engine);

    let count = std::fs::read_to_string(root.join(".boot-count"))
        .expect("count file persists across the resume");
    assert_eq!(
        count.lines().count(),
        2,
        "resume re-ran bootstrap (once per run() invocation): {count:?}"
    );
    let events = read_log(&paths);
    assert_eq!(
        gate_decisions(&events, "workspace bootstrap: running").len(),
        2,
        "one bootstrap start decision per run() invocation"
    );
}

/// A gate-owned block lifts automatically once the environment is fixed and
/// the gate passes again — resume must not wedge on a block whose
/// precondition is gone (no orchestrator unblock consultation for a
/// repo-setup problem). The "environment fix" here is an operator-created
/// file OUTSIDE the repo, so nothing about the mission/plan changes.
#[tokio::test(flavor = "multi_thread")]
async fn workspace_gate_block_lifts_once_environment_is_fixed() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    // The readiness flag is a directory INSIDE the workspace: `cd env-ready`
    // is a relative, drive-letter-free check that both sh and cmd evaluate
    // identically (an absolute Windows path hits cmd's ERROR_INVALID_NAME on
    // the runner's RUNNER~1 temp paths). `cd` fails on a missing dir in both
    // shells and passes once it exists.
    let ready_dir = root.join("env-ready");
    let contract = serde_json::json!({
        "schemaVersion": 1,
        "readiness": ["cd env-ready"],
    })
    .to_string();
    commit_workspace_contract(&root, &contract);

    // Phase 1: readiness fails (flag absent) → Blocked before any spend.
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();
    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);
    drop(engine);

    // The operator fixes the environment; a plain resume must proceed.
    std::fs::create_dir_all(&ready_dir).unwrap();
    let backend2 = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let backend2_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend2) as Arc<dyn AgentBackend>;
    let mut engine = MissionEngine::resume(backend2_dyn, &root, &mission_id, LockForce::No)
        .expect("resume mission");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    let paths2 = engine.paths().clone();
    drop(engine);
    let last_block_reason = read_log(&paths2)
        .iter()
        .rev()
        .find_map(|e| match &e.kind {
            EventKind::MilestoneBlocked { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .unwrap_or_else(|| "(none)".to_string());
    assert_eq!(
        status,
        MissionStatus::Complete,
        "a fixed environment must resume without an unblock consultation (last block reason: {last_block_reason})"
    );

    let events = read_log(&paths);
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::MilestoneUnblocked { milestone_id, reason, .. }
            if milestone_id == "ms-1" && reason.contains("workspace gate now passing")
    )));
    assert!(seq_of(&events, "milestone.unblocked") < seq_of(&events, "worker.spawned"));
}

/// D-A at run time: the gate reads the contract COMMITTED ON THE BASE
/// BRANCH — a mission branch cannot weaken the contract that gates its own
/// spend (checkout mode's working tree IS the mission branch mid-run, so a
/// working-tree read would see the weakened copy and let the mission run).
#[tokio::test(flavor = "multi_thread")]
async fn workspace_gate_reads_contract_from_base_branch_not_mission_branch() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "readiness": ["test -f .base-required-marker"]
        }"#,
    );

    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine
        .approve_plan(simple_plan(1, vec![]))
        .expect("base contract is valid");

    // The mission branch "weakens" the contract: approve (checkout mode)
    // left the primary checkout ON the mission branch, so this commit lands
    // there — the working tree now holds a contract with NO readiness gate.
    std::fs::write(
        root.join(".kranz").join("workspace.json"),
        br#"{"schemaVersion": 1}"#,
    )
    .unwrap();
    raw_git(&root, &["add", ".kranz/workspace.json"]);
    raw_git(&root, &["commit", "-m", "weaken the workspace contract"]);

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(
        status,
        MissionStatus::Blocked,
        "the gate must apply the BASE branch's contract, not the weakened mission-branch copy"
    );
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let reason = gate_block_reason(&events, "ms-1");
    assert!(
        reason.contains(&file_exists_cmd(".base-required-marker")),
        "the base branch's readiness check gated the run: {reason}"
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkerSpawned { .. })),
        "the weakened contract must never let a worker spawn"
    );
}

// ---------------------------------------------------------------------------
// WorkspaceProvider seam (design D-B/D-E, ticket workspace-provider-seam)
// ---------------------------------------------------------------------------

/// All events carrying one of the D-E workspace lifecycle wire names.
fn workspace_lifecycle_events<'a>(events: &'a [Event], wire_name: &str) -> Vec<&'a Event> {
    events
        .iter()
        .filter(|e| e.kind.type_name() == wire_name)
        .collect()
}

/// The seam's lifecycle events land on the log with the D-E wire names and
/// fold into state: `workspace.provisioned` (provider kind + cwd) precedes
/// the gate's decision events, `workspace.readiness` follows them (still
/// before the first worker spawn), and `workspace.teardown` closes the run.
#[tokio::test(flavor = "multi_thread")]
async fn workspace_provider_events_land_and_fold_into_state() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "bootstrap": ["echo boot > .boot-marker"],
            "readiness": ["test -f .boot-marker"]
        }"#,
    );

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    // The last provisioned provider kind folded into state (D-E).
    assert_eq!(
        engine.state().workspace_provider.as_deref(),
        Some("local-worktree")
    );
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let provisioned = workspace_lifecycle_events(&events, "workspace.provisioned");
    assert_eq!(provisioned.len(), 1, "one provision per run()");
    match &provisioned[0].kind {
        EventKind::WorkspaceProvisioned {
            provider,
            cwd,
            detail,
            ..
        } => {
            assert_eq!(provider, "local-worktree");
            assert_eq!(
                cwd,
                &root.display().to_string(),
                "checkout mode provisions the repo root as the workspace cwd"
            );
            assert_eq!(detail, &None, "local-worktree carries no detail");
        }
        other => panic!("wrong variant: {other:?}"),
    }

    let readiness = workspace_lifecycle_events(&events, "workspace.readiness");
    assert_eq!(readiness.len(), 1);
    match &readiness[0].kind {
        EventKind::WorkspaceReadinessReport { outcome, detail } => {
            assert_eq!(outcome, "ready");
            assert_eq!(detail, &None);
        }
        other => panic!("wrong variant: {other:?}"),
    }

    let teardown = workspace_lifecycle_events(&events, "workspace.teardown");
    assert_eq!(teardown.len(), 1);
    match &teardown[0].kind {
        EventKind::WorkspaceTeardown { mode, .. } => assert_eq!(mode, "keep"),
        other => panic!("wrong variant: {other:?}"),
    }

    // Ordering: provisioned before the gate's first decision line, readiness
    // after it, both before the first worker spawn; teardown closes the run.
    let first_gate_seq = events
        .iter()
        .find(|e| matches!(&e.kind, EventKind::OrchestratorDecision { summary, .. } if summary.starts_with("workspace bootstrap:")))
        .map(|e| e.seq)
        .expect("a gate decision on the log");
    assert!(provisioned[0].seq < first_gate_seq);
    assert!(readiness[0].seq > first_gate_seq);
    assert!(readiness[0].seq < seq_of(&events, "worker.spawned"));
    assert!(teardown[0].seq > readiness[0].seq);
}

/// A contract-less run still records `workspace.provisioned` (the workspace
/// exists — today's isolation cwd), but NO readiness report: never imply a
/// runnable environment exists when it does not (D-H).
#[tokio::test(flavor = "multi_thread")]
async fn workspace_provider_no_contract_provisions_without_readiness_report() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    assert!(!root.join(".kranz/workspace.json").exists());

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    assert_eq!(
        engine.state().workspace_provider.as_deref(),
        Some("local-worktree")
    );
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    assert_eq!(
        workspace_lifecycle_events(&events, "workspace.provisioned").len(),
        1
    );
    assert!(
        workspace_lifecycle_events(&events, "workspace.readiness").is_empty(),
        "no readiness artifact without a contract (D-H): {:?}",
        event_types(&events)
    );
    assert_eq!(
        workspace_lifecycle_events(&events, "workspace.teardown").len(),
        1
    );
}

/// Readiness failure blocks with the gate's established reason (byte-
/// identical to the pre-seam gate) AND the `workspace.readiness` report
/// records outcome `failed` carrying the same scrubbed reason (D-E).
#[tokio::test(flavor = "multi_thread")]
async fn workspace_provider_readiness_failure_blocks_with_failed_report() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_workspace_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "bootstrap": ["echo boot > .boot-marker"],
            "readiness": ["test -f .no-such-readiness-file"]
        }"#,
    );

    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let reason = gate_block_reason(&events, "ms-1");
    assert!(
        reason.contains("workspace gate: readiness check 1/1 failed"),
        "{reason}"
    );

    let readiness = workspace_lifecycle_events(&events, "workspace.readiness");
    assert_eq!(readiness.len(), 1);
    match &readiness[0].kind {
        EventKind::WorkspaceReadinessReport { outcome, detail } => {
            assert_eq!(outcome, "failed");
            assert_eq!(
                detail.as_deref(),
                Some(reason.as_str()),
                "the report carries the same scrubbed reason the block records"
            );
        }
        other => panic!("wrong variant: {other:?}"),
    }
    assert!(readiness[0].seq < seq_of(&events, "milestone.blocked"));
    // The blocked run keeps its workspace for resume — teardown records keep.
    assert_eq!(
        workspace_lifecycle_events(&events, "workspace.teardown").len(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkerSpawned { .. })),
        "no worker may spawn on a failed readiness check"
    );
}

/// Unknown `workspace.provider` names fail closed at run start — a clear
/// error naming the bad value, no workspace lifecycle events, no worker, no
/// spend — never a silent fallback to local.
///
/// Approval refuses an unknown provider first (see
/// `unknown_workspace_provider_refused_at_approve`), so the bad name here
/// arrives the only way it still can: a `config.changed` patch AFTER
/// approval (the pin records what was consented to; the seam's run-start
/// resolution stays the fail-closed backstop for post-approval config
/// drift).
#[tokio::test(flavor = "multi_thread")]
async fn workspace_provider_unknown_name_fails_closed_at_run_start() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Approve clean — the pin fires (local-worktree). No scripts queued: ANY
    // session start would error the run anyway — the empty backend itself
    // proves no spawn.
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();
    drop(engine);

    // Post-approval config drift: flip workspace.provider to an unknown name
    // via the config.changed patch channel.
    {
        let mut log = EventLog::acquire(&paths, &mission_id, Duration::ZERO, LockForce::No)
            .expect("acquire log");
        log.append(EventKind::ConfigChanged {
            patch: json!({"workspace": {"provider": "coder"}}),
        })
        .expect("append config.changed");
    }

    let backend: Arc<dyn AgentBackend> = backend;
    let mut engine =
        MissionEngine::resume(backend, &root, &mission_id, LockForce::No).expect("resume mission");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);

    let err = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .expect_err("an unknown workspace provider must fail closed");
    let msg = err.to_string();
    assert!(msg.contains("workspace.provider"), "{msg}");
    assert!(msg.contains("\"coder\""), "{msg}");
    assert!(msg.contains("local-worktree"), "{msg}");
    assert_eq!(engine.state().workspace_provider, None);
    drop(engine);

    let events = read_log(&paths);
    for wire_name in [
        "workspace.provisioned",
        "workspace.readiness",
        "workspace.teardown",
    ] {
        assert!(
            workspace_lifecycle_events(&events, wire_name).is_empty(),
            "no {wire_name} event on a run that failed closed"
        );
    }
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkerSpawned { .. })),
        "no worker may spawn when the provider fails closed"
    );
}

/// Resume re-provisions (idempotent, mirroring the gate's
/// reruns-on-resume idiom): two engine lifetimes ⇒ two
/// `workspace.provisioned` events recording the same provider kind; the
/// crashed run records no teardown (crash semantics), the completing run
/// records `keep`.
#[tokio::test(flavor = "multi_thread")]
async fn workspace_provider_resume_reprovisions_idempotently() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // --- Phase 1: the "crash" (same idiom as kill_and_resume /
    // workspace_bootstrap_reruns_on_resume_after_crash): the worker passes
    // writing nothing, the judgement turn starves, the short stall timeout
    // declares the orchestrator dead, and the retry finds no script left.
    let backend1 = Arc::new(MockBackend::with_scripts(vec![
        worker_pass_no_write(),
        orch_script(vec![]),
    ]));
    let mut engine = make_engine(&backend1, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    engine.set_orch_stall_timeout(Duration::from_millis(400));
    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();

    timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .expect_err("phase 1 must error out (simulated crash)");
    drop(engine);

    let events = read_log(&paths);
    assert_eq!(
        workspace_lifecycle_events(&events, "workspace.provisioned").len(),
        1,
        "the crashed run provisioned exactly once"
    );
    assert!(
        workspace_lifecycle_events(&events, "workspace.teardown").is_empty(),
        "a crashed run records no teardown (the resume sweep owns leftovers)"
    );

    // --- Phase 2: resume. Provision runs again — re-resolving the same
    // workspace, not remembering a durable one.
    let backend2 = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let backend2_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend2) as Arc<dyn AgentBackend>;
    let mut engine = MissionEngine::resume(backend2_dyn, &root, &mission_id, LockForce::No)
        .expect("resume mission");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    assert_eq!(
        engine.state().workspace_provider.as_deref(),
        Some("local-worktree")
    );
    drop(engine);

    let events = read_log(&paths);
    let provisioned = workspace_lifecycle_events(&events, "workspace.provisioned");
    assert_eq!(
        provisioned.len(),
        2,
        "resume re-provisions: one workspace.provisioned per run() invocation"
    );
    for event in provisioned {
        match &event.kind {
            EventKind::WorkspaceProvisioned { provider, .. } => {
                assert_eq!(provider, "local-worktree")
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }
    assert_eq!(
        workspace_lifecycle_events(&events, "workspace.teardown").len(),
        1,
        "only the completing run records teardown"
    );
}

// ---------------------------------------------------------------------------
// Golden-data hooks (design D-D, ticket golden-data-hooks): the contract's
// optional `data` block provisions a de-identified golden dataset into the
// workspace before agents run; skew Blocks owned + actionable, never flake.
//
// Shell lines stay `sh`/`cmd` portable (echo / > / >> / && / exit; file
// existence via `portable_shell_json`)
// only) and markers are relative paths in the workspace cwd — no
// JSON-interpolated absolute paths.
// ---------------------------------------------------------------------------

/// Commit a workspace contract carrying a `data` block (plus a .gitignore
/// for every marker the hooks write, so hook output never dirties the
/// worker's tree) onto the BASE branch BEFORE approve — D-A: the run-time
/// gate reads the committed base-branch copy.
fn commit_data_contract(root: &Path, contract_json: &str) {
    let contract_json = portable_shell_json(contract_json);
    std::fs::create_dir_all(root.join(".kranz")).unwrap();
    std::fs::write(root.join(".kranz").join("workspace.json"), &contract_json).unwrap();
    std::fs::write(
        root.join(".gitignore"),
        ".boot-marker\n.data-clone-marker\n.data-migrate-marker\n.data-skew-marker\n.data-reset-count\n",
    )
    .unwrap();
    raw_git(root, &["add", ".kranz/workspace.json", ".gitignore"]);
    raw_git(root, &["commit", "-m", "workspace contract"]);
}

/// The seq of the first `orchestrator.decision` whose summary starts with
/// `prefix` (for lifecycle-order assertions between decision lines).
fn decision_seq(events: &[Event], prefix: &str) -> u64 {
    events
        .iter()
        .find(|e| matches!(&e.kind, EventKind::OrchestratorDecision { summary, .. } if summary.starts_with(prefix)))
        .map(|e| e.seq)
        .unwrap_or_else(|| panic!("no decision starting with {prefix:?}"))
}

/// Provision order (D-D), proven by markers: clone writes a marker, migrate
/// reads it, bootstrap runs later, readiness after that, skewCheck last —
/// everything before the first worker spawn.
#[tokio::test(flavor = "multi_thread")]
async fn golden_data_hooks_run_in_provision_order_before_workers() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_data_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "data": {
                "clone": "echo cloned > .data-clone-marker",
                "migrate": "test -f .data-clone-marker && echo mig > .data-migrate-marker",
                "skewCheck": "test -f .boot-marker && echo checked > .data-skew-marker"
            },
            "bootstrap": ["test -f .data-migrate-marker && echo boot > .boot-marker"],
            "readiness": ["test -f .boot-marker"]
        }"#,
    );

    // The final gate's command assertion re-proves the skew marker is
    // visible in the mission execution cwd (it runs engine-side there).
    let contract = vec![assertion(
        "a-1",
        "the skew check ran",
        Some(&file_exists_cmd(".data-skew-marker")),
    )];
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    assert_eq!(
        gate_decisions(&events, "workspace data:"),
        vec![
            "workspace data: clone `echo cloned > .data-clone-marker` → ok (exit code 0)"
                .to_string(),
            format!(
                "workspace data: migrate `{}` → ok (exit code 0)",
                if_file_exists_cmd(".data-clone-marker", "echo mig > .data-migrate-marker")
            ),
            format!(
                "workspace data: skewCheck `{}` → ok (exit code 0)",
                if_file_exists_cmd(".boot-marker", "echo checked > .data-skew-marker")
            ),
        ]
    );
    // Lifecycle order: migrate before bootstrap, skewCheck after readiness,
    // all of it before the first worker spawns.
    assert!(
        decision_seq(&events, "workspace data: migrate")
            < decision_seq(&events, "workspace bootstrap:")
    );
    assert!(
        decision_seq(&events, "workspace readiness: 1/1")
            < decision_seq(&events, "workspace data: skewCheck")
    );
    assert!(decision_seq(&events, "workspace data: skewCheck") < seq_of(&events, "worker.spawned"));
    // Skew passing leaves the ordinary ready report (no skew artifact).
    let readiness = workspace_lifecycle_events(&events, "workspace.readiness");
    assert_eq!(readiness.len(), 1);
    match &readiness[0].kind {
        EventKind::WorkspaceReadinessReport { outcome, .. } => assert_eq!(outcome, "ready"),
        other => panic!("wrong variant: {other:?}"),
    }
}

/// skewCheck exit 1 ⇒ the SKEW case (D-D): Blocked with owner repo-setup,
/// the migrate hook named as the action, a scrubbed tail, the distinct
/// `skew` readiness outcome — never a readiness flake and never a validator
/// finding.
#[tokio::test(flavor = "multi_thread")]
async fn golden_data_skew_blocks_owned_actionable_and_never_a_finding() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_data_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "data": {
                "migrate": "echo mig > .data-migrate-marker",
                "skewCheck": "echo skewed sk-ant-api03-a1b2c3d4e5f6 1>&2 && exit 1"
            },
            "bootstrap": ["echo boot > .boot-marker"],
            "readiness": ["test -f .boot-marker"]
        }"#,
    );

    // No scripts queued: ANY session start (worker/validator/orchestrator)
    // would error the run — the empty backend itself proves no spawn.
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);
    assert_eq!(
        engine.state().mission.milestones[0].status,
        MilestoneStatus::Blocked
    );
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    // Bootstrap and readiness passed first — skew is the LAST readiness
    // step, distinct from a readiness flake.
    assert_eq!(
        gate_decisions(&events, "workspace bootstrap:"),
        vec![
            "workspace bootstrap: running 1 commands",
            "workspace bootstrap: 1/1 commands ok"
        ]
    );
    assert_eq!(
        gate_decisions(&events, "workspace readiness:"),
        vec![
            "workspace readiness: running 1 checks",
            "workspace readiness: 1/1 checks ok"
        ]
    );

    let reason = gate_block_reason(&events, "ms-1");
    assert!(
        reason.contains("workspace gate: data skewCheck failed"),
        "{reason}"
    );
    assert!(reason.contains("owner: repo-setup"), "{reason}");
    assert!(reason.contains("exit code 1"), "{reason}");
    assert!(
        reason
            .contains("run the data migrate hook (`echo mig > .data-migrate-marker`), then resume"),
        "the action names the declared migrate hook: {reason}"
    );
    assert!(
        !reason.contains("readiness check"),
        "the skew reason never presents as a readiness flake: {reason}"
    );
    assert!(
        !reason.contains("sk-ant-api03-a1b2c3d4e5f6"),
        "the output tail must be scrubbed: {reason}"
    );
    assert!(reason.contains("[REDACTED]"), "{reason}");

    // The workspace.readiness report records the distinct skew outcome
    // carrying the same reason (D-E) — additive alongside ready/failed.
    let readiness = workspace_lifecycle_events(&events, "workspace.readiness");
    assert_eq!(readiness.len(), 1);
    match &readiness[0].kind {
        EventKind::WorkspaceReadinessReport { outcome, detail } => {
            assert_eq!(outcome, "skew");
            assert_eq!(detail.as_deref(), Some(reason.as_str()));
        }
        other => panic!("wrong variant: {other:?}"),
    }

    // Never a validator finding, never any spend.
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::ValidationFinding { .. })),
        "skew must not present as a validator finding: {:?}",
        event_types(&events)
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkerSpawned { .. })),
        "no worker may spawn on a skewed dataset"
    );
}

/// resetBetweenRounds (D-D): the reset hook runs BEFORE each validation
/// round's validator spawn — counted via an append-marker across two rounds
/// (finding → fix → clean round).
#[tokio::test(flavor = "multi_thread")]
async fn golden_data_reset_runs_before_each_validation_round() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_data_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "data": {
                "reset": "echo reset >> .data-reset-count",
                "resetBetweenRounds": true
            }
        }"#,
    );

    let finding = json!([{
        "subject": "part 1 works",
        "severity": "major",
        "evidence": "the endpoint returns 500 on empty input",
        "suggestedFix": "guard empty input"
    }]);
    // Session order: worker f-1-1, orchestrator, functional validator #1
    // (one finding), fix worker, functional validator #2 (clean) — two
    // validation rounds, hence two resets.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            fix_features(1),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        validator_with(finding),
        worker_pass(),
        validator_with(json!([])),
    ]));
    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    let paths = engine.paths().clone();
    drop(engine);

    // One appended line per validation round (portable across sh and cmd —
    // only the line COUNT is asserted, never the bytes).
    let count = std::fs::read_to_string(root.join(".data-reset-count"))
        .expect("the reset hook appended into the workspace cwd");
    assert_eq!(
        count.lines().count(),
        2,
        "one reset per validation round: {count:?}"
    );

    let events = read_log(&paths);
    let resets = gate_decisions(&events, "workspace data: reset");
    assert_eq!(
        resets,
        vec!["workspace data: reset `echo reset >> .data-reset-count` → ok (exit code 0)"; 2],
        "one reset decision per round"
    );
    // Each reset fires after its round's milestone.validating and before
    // the next one — i.e. before that round's validators spawn.
    let validating_seqs: Vec<u64> = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::MilestoneValidating { .. }))
        .map(|e| e.seq)
        .collect();
    assert_eq!(validating_seqs.len(), 2, "two validation rounds");
    let reset_seqs: Vec<u64> = events
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::OrchestratorDecision { summary, .. } if summary.starts_with("workspace data: reset")))
        .map(|e| e.seq)
        .collect();
    assert_eq!(reset_seqs.len(), 2, "one reset decision per round");
    assert!(
        validating_seqs[0] < reset_seqs[0] && reset_seqs[0] < validating_seqs[1],
        "round 1's reset precedes round 2: {validating_seqs:?} vs {reset_seqs:?}"
    );
    assert!(
        validating_seqs[1] < reset_seqs[1],
        "round 2's reset follows its validating event: {validating_seqs:?} vs {reset_seqs:?}"
    );
}

/// A reset failure Blocks with the same owned shape as skew (hook, exit,
/// scrubbed tail, owner, action) BEFORE any validator spawns — never a
/// validator finding.
#[tokio::test(flavor = "multi_thread")]
async fn golden_data_reset_failure_blocks_owned_before_any_validator() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_data_contract(
        &root,
        r#"{
            "schemaVersion": 1,
            "data": {
                "reset": "exit 7",
                "resetBetweenRounds": true
            }
        }"#,
    );

    // NO validator script queued: the reset must block before any validator
    // spawn — a consumed validator session would error the run, proving the
    // ordering from the backend side too.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![dirty_tree_commit_as_is(), judgement("complete", "")]),
    ]));
    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);
    assert_eq!(
        engine.state().mission.milestones[0].status,
        MilestoneStatus::Blocked
    );
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    assert!(
        seq_of(&events, "milestone.validating") < seq_of(&events, "milestone.blocked"),
        "the block lands inside the validation round"
    );
    assert_eq!(
        gate_decisions(&events, "workspace data: reset"),
        vec![
            "workspace data: reset `exit 7` → FAILED (exit code 7) — blocking mission (owner: repo-setup)"
        ]
    );
    let reason = gate_block_reason(&events, "ms-1");
    assert!(
        reason.contains("workspace gate: data reset hook failed"),
        "{reason}"
    );
    assert!(reason.contains("owner: repo-setup"), "{reason}");
    assert!(reason.contains("exit code 7"), "{reason}");
    assert!(reason.contains("`exit 7`"), "{reason}");
    assert!(reason.contains("then resume"), "{reason}");
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::ValidationFinding { .. })),
        "a reset failure must not present as a validator finding: {:?}",
        event_types(&events)
    );
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
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            fix_features(1),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        validator_with(finding),
        worker_pass(),
        validator_with(json!([])),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
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
    assert!(
        types.contains(&"validation.finding"),
        "finding event: {types:?}"
    );
    assert!(
        types.contains(&"fixfeature.created"),
        "fixfeature event: {types:?}"
    );
    assert_eq!(
        types
            .iter()
            .filter(|t| **t == "milestone.validating")
            .count(),
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
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            fix_features(1),
            dirty_tree_commit_as_is(),
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

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);
    assert_eq!(engine.state().mission.status, MissionStatus::Blocked);
    assert_eq!(
        engine.state().mission.milestones[0].status,
        MilestoneStatus::Blocked
    );
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
        event_types(&events)
            .iter()
            .filter(|t| **t == "fixfeature.created")
            .count(),
        1,
        "only round 1 created a fix feature"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unblock_guidance_survives_restart_and_reaches_validator() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Phase 1: drive the milestone to Blocked exactly like the cap test
    // above (cap=1, findings in both rounds).
    let finding = json!([{
        "subject": "part 1 works",
        "severity": "major",
        "evidence": "still failing",
        "suggestedFix": ""
    }]);
    let backend1 = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            fix_features(1),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            fix_features(1),
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
    let mut engine = make_engine(&backend1, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);
    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();
    drop(engine);

    // The operator asks for an unblock; the guidance text must travel.
    control::enqueue(
        &paths,
        &ControlCommand::Msg {
            text: "unblock and guide the validator".into(),
            interrupt: false,
        },
    )
    .unwrap();

    // Phase 2: a FRESH engine resumes from the event log (process-restart
    // equivalent — state is folded, not carried). The orchestrator's unblock
    // decision carries validatorGuidance; validation then passes.
    let guidance = "FMT FIRST: run cargo fmt before the contract gate";
    let unblock = json!({
        "action": "unblock-raise-cap",
        "note": "cap raised with guidance",
        "validatorGuidance": guidance,
    })
    .to_string();
    let backend2 = Arc::new(MockBackend::with_scripts(vec![
        orch_script(vec![unblock, no_lesson()]),
        validator_with(json!([])),
    ]));
    let backend2_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend2) as Arc<dyn AgentBackend>;
    let mut engine = MissionEngine::resume(backend2_dyn, &root, &mission_id, LockForce::No)
        .expect("resume mission");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    drop(engine);

    // (a) The event log carries the guidance verbatim (restart durability).
    let events = read_log(&paths);
    let unblock_idx = events
        .iter()
        .position(|e| matches!(&e.kind, EventKind::MilestoneUnblocked { .. }))
        .expect("unblock event present");
    match &events[unblock_idx].kind {
        EventKind::MilestoneUnblocked {
            validator_guidance, ..
        } => assert_eq!(validator_guidance.as_deref(), Some(guidance)),
        _ => unreachable!(),
    }

    // (b) Folding the log prefix up to the unblock reconstructs the guidance
    // — this is exactly what MissionEngine::resume injects from.
    let prefix_state = reducer::fold(&events[..=unblock_idx]).unwrap();
    assert_eq!(
        prefix_state.mission.milestones[0]
            .validator_guidance
            .as_deref(),
        Some(guidance),
        "folded state must carry the guidance across a restart"
    );

    // (c) The fresh validator session's task contains the guidance verbatim.
    let specs = backend2.started_specs();
    let validator_task = specs
        .iter()
        .find_map(|s| match &s.prompt {
            PromptMode::SingleShot(task) if task.contains("Validate milestone") => Some(task),
            _ => None,
        })
        .expect("a validator session ran in phase 2");
    assert!(
        validator_task.contains(guidance),
        "validator task must carry the operator guidance verbatim: {validator_task}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unblock_add_fix_schedules_repair_before_revalidation() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Phase 1: block at the fix-cycle cap (same shape as the guidance test).
    let finding = json!([{
        "subject": "part 1 works",
        "severity": "major",
        "evidence": "fmt check fails",
        "suggestedFix": ""
    }]);
    let backend1 = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            fix_features(1),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            fix_features(1),
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
    let mut engine = make_engine(&backend1, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);
    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();
    drop(engine);

    control::enqueue(
        &paths,
        &ControlCommand::Msg {
            text: "it is just rustfmt — repair then re-validate".into(),
            interrupt: false,
        },
    )
    .unwrap();

    // Phase 2: the orchestrator chooses unblock-add-fix. Script ORDER is the
    // assertion that the repair worker runs before the re-validation: the
    // orch decision, then a worker, then the (passing) validator.
    let decision = json!({
        "action": "unblock-add-fix",
        "note": "schedule a fmt repair",
        "fix": {
            "title": "run cargo fmt --all",
            "spec": "run cargo fmt --all and commit the result",
            "validationCriteria": ["cargo fmt --all --check exits clean"]
        }
    })
    .to_string();
    let backend2 = Arc::new(MockBackend::with_scripts(vec![
        orch_script(vec![
            decision,
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        worker_pass(),
        validator_with(json!([])),
    ]));
    let backend2_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend2) as Arc<dyn AgentBackend>;
    let mut engine = MissionEngine::resume(backend2_dyn, &root, &mission_id, LockForce::No)
        .expect("resume mission");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let state = engine.state();
    assert_eq!(
        state.mission.milestones[0].fix_cycles, 1,
        "a blocked-state repair must not spend a fix cycle"
    );
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let unblock_seq = events
        .iter()
        .find(|e| matches!(&e.kind, EventKind::MilestoneUnblocked { .. }))
        .expect("unblock event")
        .seq;
    // Phase 1 created one fix feature of its own; the repair is the one
    // created AFTER the unblock.
    let post_unblock_fixes: Vec<_> = events
        .iter()
        .filter(|e| e.seq > unblock_seq && matches!(&e.kind, EventKind::FixFeatureCreated { .. }))
        .collect();
    assert_eq!(
        post_unblock_fixes.len(),
        1,
        "exactly one repair feature follows the unblock"
    );
    match &post_unblock_fixes[0].kind {
        EventKind::FixFeatureCreated { feature, .. } => {
            assert_eq!(feature.title, "run cargo fmt --all");
            assert_eq!(feature.origin, FeatureOrigin::Fix);
        }
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn contract_commands_run_engine_side_and_reach_validator_task() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // A portable contract command that passes everywhere (git repo present):
    // the engine must run it during the validation round and hand the
    // captured PASS to the functional validator — the validator never has to
    // run it. (A failing command would trip the final gate's non-waivable
    // command-assertion finding later; that path has its own tests.)
    let contract = vec![assertion("a-1", "the build succeeds", Some("git status"))];
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        validator_with(json!([])),
    ]));
    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    // The mock validator judges pass on the evidence; the mission completes.
    assert_eq!(status, MissionStatus::Complete);
    drop(engine);

    let specs = backend.started_specs();
    let validator_task = specs
        .iter()
        .find_map(|s| match &s.prompt {
            PromptMode::SingleShot(task) if task.contains("Validate milestone") => Some(task),
            _ => None,
        })
        .expect("a validator session ran");
    assert!(
        validator_task.contains("[a-1] `git status` → PASS"),
        "engine-captured PASS must ride the validator task: {validator_task}"
    );
    assert!(
        validator_task.contains("do NOT re-run"),
        "the results block must forbid re-running: {validator_task}"
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
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            waive_reply("part 1 works", "docstring nitpick"),
            no_lesson(),
        ]),
        validator_with(finding),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
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
    assert!(
        types.contains(&"validation.finding"),
        "finding still surfaced: {types:?}"
    );
    assert!(
        !types.contains(&"fixfeature.created"),
        "no fix feature: {types:?}"
    );
    assert_eq!(
        types
            .iter()
            .filter(|t| **t == "milestone.validating")
            .count(),
        1,
        "exactly one validation round: {types:?}"
    );
    assert!(
        types.contains(&"mission.completed"),
        "mission completed: {types:?}"
    );

    // The waiver is on the log as an orchestrator decision: summary names
    // the subject, detail carries the one-line justification.
    let (summary, detail) = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::OrchestratorDecision {
                summary,
                detail: Some(detail),
            } if summary.starts_with("waived") => Some((summary.clone(), detail.clone())),
            _ => None,
        })
        .expect("a waive orchestrator.decision exists");
    assert!(
        summary.contains("waived 1 finding(s)"),
        "summary: {summary}"
    );
    assert!(
        summary.contains("part 1 works"),
        "summary names the subject: {summary}"
    );
    assert!(
        detail.contains("docstring nitpick"),
        "detail carries the reason: {detail}"
    );

    // The completion report replays the round: the waived finding is listed
    // with its severity/subject/evidence and the waiver's justification.
    let report = std::fs::read_to_string(
        root.join(".kranz")
            .join("missions")
            .join(&mission_id)
            .join("report.md"),
    )
    .expect("report.md written at completion");
    assert!(report.contains("## Validation history"), "{report}");
    assert!(
        report.contains("[minor] part 1 works"),
        "finding listed: {report}"
    );
    assert!(
        report.contains("docstring omits the error case"),
        "evidence listed: {report}"
    );
    assert!(report.contains("Disposition: waived."), "{report}");
    assert!(
        report.contains("part 1 works: docstring nitpick"),
        "waiver reason: {report}"
    );
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
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            fix_features(1),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            waive_reply("helper docs", "cosmetic; outside the contract"),
            no_lesson(),
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

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    let ms = &engine.state().mission.milestones[0];
    assert_eq!(ms.status, MilestoneStatus::Complete);
    assert_eq!(ms.fix_cycles, 1, "the waived round consumed no extra cycle");

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(
        !types.contains(&"milestone.blocked"),
        "must not block: {types:?}"
    );
    assert!(
        types.contains(&"mission.completed"),
        "mission completed: {types:?}"
    );
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::OrchestratorDecision { summary, .. }
            if summary.starts_with("waived 1 finding(s)") && summary.contains("helper docs")
    )));
}

// ---------------------------------------------------------------------------
// 3d. Final-gate command assertions are non-waivable (but still fixable)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn command_assertion_at_final_gate_is_non_waivable() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Portable failing command. Waive is refused and replaced with a fix
    // feature; when the fix still leaves the command red and the cap is
    // spent, the mission Blocks (never Completes via waive).
    let contract = vec![assertion(
        "a-1",
        "the build succeeds",
        Some("cd kranz-no-such-dir"),
    )];

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            waive_reply("a-1", "command not runnable in this environment"),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            waive_reply("a-1", "still not runnable"),
        ]),
        worker_pass(), // fix feature for the refused waive
    ]));

    let cfg = MissionConfig {
        max_fix_cycles_per_milestone: 1,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    let types = event_types(&events);
    let decision_summaries: Vec<&str> = events
        .iter()
        .filter_map(|event| match &event.kind {
            EventKind::OrchestratorDecision { summary, .. } => Some(summary.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        types.contains(&"validation.finding"),
        "gate finding surfaced: {types:?}"
    );
    assert!(
        types.contains(&"fixfeature.created"),
        "refused waive must synthesize a fix feature: {types:?}"
    );
    assert!(
        types.contains(&"milestone.blocked"),
        "second refused waive at the fix-cycle cap must block: {types:?}"
    );
    assert!(
        !types.contains(&"mission.completed"),
        "command assertion must not COMPLETE via waive: {types:?}"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::OrchestratorDecision { summary, .. }
                if summary.contains("refused model waive") && summary.contains("a-1")
        )),
        "must surface the refuse-waive decision: {types:?}; decisions: {decision_summaries:?}"
    );
}

// ---------------------------------------------------------------------------
// 3d-2. Author-broken command assertions escalate to the operator (f-1-1)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn command_broken_assertion_escalates_to_operator() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Portable failing command, same shape as the non-waivable test. This
    // time the orchestrator judges it author-broken instead of trying to
    // waive it — the escalation route must block immediately with the
    // fix-cycle cap left untouched.
    let contract = vec![assertion(
        "a-1",
        "the build succeeds",
        Some("cd kranz-no-such-dir"),
    )];

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            command_broken_reply("a-1", "grep can only match pre-change; false negative"),
        ]),
    ]));

    let cfg = MissionConfig {
        max_fix_cycles_per_milestone: 1,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);

    let ms = &engine.state().mission.milestones[0];
    assert_eq!(ms.fix_cycles, 0, "escalation must not spend a fix cycle");

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(
        !types.contains(&"fixfeature.created"),
        "escalation must not synthesize a fix feature: {types:?}"
    );
    assert!(
        !types.contains(&"mission.completed"),
        "escalation must not complete the mission: {types:?}"
    );
    let blocked = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::MilestoneBlocked { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .expect("milestone.blocked event present");
    assert!(
        blocked.contains("a-1"),
        "blocked reason names the assertion id: {blocked}"
    );
    let lower = blocked.to_lowercase();
    assert!(
        lower.contains("evidence"),
        "blocked reason mentions evidence: {blocked}"
    );
    assert!(
        lower.contains("buggy") || lower.contains("false negative"),
        "blocked reason indicates the assertion appears broken: {blocked}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn noncommand_finding_marked_command_broken_does_not_escalate() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Same shape as validation_round_creates_fix_feature_then_completes,
    // except the conversion turn (wrongly) marks the validator finding
    // commandBroken. Validator findings always carry class == "" (never
    // "command-assertion"), so convert_findings' escape-hatch guard must
    // drop the mislabelled escalation and route it through the normal fix
    // path instead.
    let finding = json!([{
        "subject": "part 1 works",
        "severity": "major",
        "evidence": "the endpoint returns 500 on empty input",
        "suggestedFix": "guard empty input"
    }]);

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            command_broken_reply("part 1 works", "wrongly claimed author-broken"),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        validator_with(finding),
        worker_pass(),
        validator_with(json!([])),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let state = engine.state();
    let ms = &state.mission.milestones[0];
    assert_eq!(
        ms.fix_cycles, 1,
        "mislabelled escalation must be treated as a normal fix"
    );

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(
        types.contains(&"fixfeature.created"),
        "mislabelled escalation must still route to a fix feature: {types:?}"
    );
    assert!(
        !types.contains(&"milestone.blocked"),
        "mislabelled escalation must not block: {types:?}"
    );
}

// ---------------------------------------------------------------------------
// 3d-3. Declared pty-script assertions that never execute cannot green
// (ticket pty-script-skip-vacuous-green)
// ---------------------------------------------------------------------------

/// A plan DECLARES a pty-script assertion but no session ever executes it —
/// here via the host-independent "did not execute" case (check=pty-script
/// with no script payload; the harness SKIP arms — non-unix host — are
/// unit-covered in pty_harness). The functional validator greens its round
/// anyway (the vacuous-green hole), and still the mission must NOT
/// complete: the final gate finds no validation.pty.transcript verdict for
/// the declared assertion and raises a critical, non-waivable finding
/// naming it.
#[tokio::test(flavor = "multi_thread")]
async fn declared_pty_script_that_never_executes_cannot_green() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let contract = vec![Assertion {
        id: "a-pty".to_string(),
        statement: "the REPL echoes input back".to_string(),
        check: AssertionCheck::PtyScript,
        command: None,
        negative_control: None,
        pty_script: None,
    }];

    // Session-start order: worker, orchestrator (streaming), functional
    // validator (greens the round despite the cannot-run evidence line).
    // Orchestrator turns: seed, dirty-tree, judgement f-1-1, then the
    // final-gate conversion turn — the finding is class "command-assertion",
    // so the orchestrator escalates it to the operator as un-runnable here.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            command_broken_reply("a-pty", "declared pty-script never executed on this host"),
        ]),
        validator_with(json!([])),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(
        status,
        MissionStatus::Blocked,
        "a declared pty-script that never executed must not green"
    );

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(
        !types.contains(&"mission.completed"),
        "no vacuous green off a green round: {types:?}"
    );
    // The gate's finding surfaces on the feed, attributed to the engine,
    // naming the assertion whose declared validation never ran.
    let finding = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::ValidationFinding { finding, .. } => Some(finding.clone()),
            _ => None,
        })
        .expect("final-gate finding surfaced for the unexecuted pty-script");
    assert_eq!(finding.subject, "a-pty");
    assert_eq!(
        finding.class, "command-assertion",
        "non-waivable class: {finding:?}"
    );
    assert!(
        finding.evidence.contains("validation.pty.transcript")
            && finding.evidence.contains("never executed"),
        "the finding names the missing verdict: {}",
        finding.evidence
    );
    let blocked = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::MilestoneBlocked { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .expect("milestone.blocked event present");
    assert!(
        blocked.contains("a-pty"),
        "blocked reason names the assertion id: {blocked}"
    );
}

/// Cancellation joins the PTY cleanup before the mission can release its
/// writer lock and let a second engine resume the retained worktree.
#[cfg(unix)]
#[tokio::test]
async fn cancelling_pty_validation_stops_writes_before_mission_unlock() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let contract = vec![Assertion {
        id: "a-cancel-pty".into(),
        statement: "the target completes".into(),
        check: AssertionCheck::PtyScript,
        command: None,
        negative_control: None,
        pty_script: Some(PtyScript {
            command: "printf '%s' $$ > pty.pid; exec sleep 30".into(),
            steps: vec![PtyStep::Expect {
                pattern: "never printed".into(),
                regex: false,
                timeout_ms: Some(30_000),
            }],
            timeout_secs: Some(30),
        }),
    }];
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![dirty_tree_commit_as_is(), judgement("complete", "")]),
    ]));
    let mut engine = make_engine(
        &backend,
        &root,
        MissionConfig {
            skip_functional: false,
            ..test_cfg()
        },
    );
    engine.approve_plan(simple_plan(1, contract)).unwrap();
    let paths = engine.paths().clone();
    let mut run = Box::pin(engine.run());
    let pid: i32 = tokio::select! {
        status = &mut run => panic!("mission ended before cancellation: {status:?}"),
        pid = async {
            for _ in 0..1000 {
                if let Ok(text) = std::fs::read_to_string(root.join("pty.pid")) {
                    if let Ok(pid) = text.parse() { return pid; }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            panic!("PTY validation did not start");
        } => pid,
    };
    let started = std::time::Instant::now();
    // This is the same future-drop path the CLI's SIGINT branch takes.
    drop(run);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(paths.lock_file().exists(), "the engine still owns its lock");
    assert!(
        unsafe { libc::kill(pid, 0) } != 0,
        "the PTY target outlived cancellation and could race a resumed mission"
    );
    drop(engine);
    assert!(!paths.lock_file().exists());
    let events = read_log(&paths);
    assert!(!events
        .iter()
        .any(|event| matches!(event.kind, EventKind::ValidationPtyTranscript { .. })));
}

/// The other side of the backstop (unix hosts): a declared pty-script that
/// EXECUTES and PASSES greens exactly as before — the round drives the
/// scripted session, the transcript event lands, and the final gate's
/// unexecuted-assertion scan finds the verdict and stays silent.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn declared_pty_script_executes_and_passes_greens() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // The pty_harness REPL fixture: a `> ` prompt that echoes input back as
    // `echo:<line>` and says `bye` on `quit`.
    let contract = vec![Assertion {
        id: "a-pty".to_string(),
        statement: "the REPL echoes input back".to_string(),
        check: AssertionCheck::PtyScript,
        command: None,
        negative_control: None,
        pty_script: Some(PtyScript {
            command: "printf '> '; while IFS= read -r line; do case \"$line\" in quit) \
                 printf 'bye\\n'; exit 0;; *) printf 'echo:%s\\n> ' \"$line\";; esac; done"
                .to_string(),
            steps: vec![
                PtyStep::Expect {
                    pattern: "> ".to_string(),
                    regex: false,
                    timeout_ms: Some(10_000),
                },
                PtyStep::Send {
                    text: "hello\n".to_string(),
                },
                PtyStep::Expect {
                    pattern: "echo:hello".to_string(),
                    regex: false,
                    timeout_ms: Some(10_000),
                },
                PtyStep::Send {
                    text: "quit\n".to_string(),
                },
                PtyStep::Expect {
                    pattern: "bye".to_string(),
                    regex: false,
                    timeout_ms: Some(10_000),
                },
            ],
            timeout_secs: Some(30),
        }),
    }];

    // Session-start order: worker, orchestrator (streaming), functional
    // validator (no findings). Orchestrator turns: seed, dirty-tree,
    // judgement f-1-1, capture (NONE).
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        validator_with(json!([])),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(
        status,
        MissionStatus::Complete,
        "a declared pty-script that executed and passed still greens"
    );

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    // The round drove the session and recorded the verdict — exactly the
    // evidence the final gate's backstop keys on.
    let verdict = events.iter().find_map(|e| match &e.kind {
        EventKind::ValidationPtyTranscript {
            assertion_id,
            verdict,
            ..
        } if assertion_id == "a-pty" => Some(*verdict),
        _ => None,
    });
    assert_eq!(
        verdict,
        Some(kranz_engine::gate::GateVerdict::Pass),
        "the executed session's verdict is on the log"
    );
    // The gate recorded its posture and found nothing to flag.
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::OrchestratorDecision { summary, .. }
            if summary == "pty-script assertions not re-run at the final gate"
    )));
    assert!(
        !events.iter().any(|e| matches!(
            &e.kind,
            EventKind::ValidationFinding { finding, .. } if finding.subject == "a-pty"
        )),
        "no finding for an executed pty-script: {:?}",
        event_types(&events)
    );
    // The transcript artifact landed under the mission's runs/ dir.
    let transcripts = paths.runs_dir().join("pty-transcripts");
    assert!(
        transcripts.is_dir() && std::fs::read_dir(&transcripts).unwrap().next().is_some(),
        "transcript artifact written under {}",
        transcripts.display()
    );
}

// ---------------------------------------------------------------------------
// 3e. Capture-turn best-effort: an erroring capture turn never stalls
// completion (mission.completed still fires, no lesson is written).
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn capture_turn_error_still_completes_mission() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Orchestrator turns: seed, judgement f-1-1 (FINDINGS-EMPTY gate, empty
    // contract), then the capture turn — scripted as an ERROR result rather
    // than a reply. `orch_turn` retries once via force_reseed, but no further
    // orchestrator script is queued, so the retry's fresh session fails to
    // start and the turn ultimately errors. `capture_lesson` must swallow
    // that error (best-effort) rather than stall `mission.completed`.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        MockScript::streaming(vec![mock_init("orch-session"), mock_result_text("ready")])
            .responding(vec![
                vec![
                    mock_text(&dirty_tree_commit_as_is()),
                    mock_result_text(&dirty_tree_commit_as_is()),
                ],
                vec![
                    mock_text(&judgement("complete", "")),
                    mock_result_text(&judgement("complete", "")),
                ],
                vec![mock_text("boom"), mock_result_error("boom")],
            ]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(
        status,
        MissionStatus::Complete,
        "completion must proceed despite the capture-turn error"
    );

    let mission_id = engine.mission_id().to_string();
    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(
        types.contains(&"mission.completed"),
        "mission completed: {types:?}"
    );

    // No lesson was captured, and completion says so on the event feed.
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::OrchestratorDecision { summary, .. } if summary == "no cross-mission lesson captured"
    )));
    assert!(
        !root
            .join(".kranz")
            .join("lessons")
            .join(format!("{mission_id}.md"))
            .exists(),
        "no lesson file written when the capture turn errors"
    );

    // The report commit still lands, just without any lesson files.
    let subject = raw_git(&root, &["log", "-1", "--format=%s"]);
    assert_eq!(
        subject.trim(),
        format!("[kranz] mission report for {mission_id}")
    );
}

// ---------------------------------------------------------------------------
// 4. Respawn budget: fail → respawn → budget exhausted → feature.failed
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn retry_retains_prior_checkpoint_commit_receipts() {
    if !setup() {
        return;
    }
    for isolation in [WorkerIsolation::Checkout, WorkerIsolation::Worktree] {
        for succeeds in [true, false] {
            let (_dir, root) = init_repo();
            let partial = MockScript::single_shot_json(&json!({
                "result": "partial",
                "summary": "implementation left for the next attempt to verify",
                "commits": []
            }))
            .writes_file("attempt.txt", "work from the first attempt\n");
            let mut replies = vec![dirty_tree_commit_as_is()];
            if succeeds {
                replies.push(judgement("complete", "verified the retained work"));
            }
            replies.push(no_lesson());
            let backend = Arc::new(MockBackend::with_scripts(vec![
                partial,
                orch_script(replies),
                if succeeds {
                    worker_pass_no_write()
                } else {
                    worker_fail()
                },
            ]));
            let cfg = MissionConfig {
                max_respawns: 1,
                worker_isolation: isolation,
                ..test_cfg()
            };
            let mut engine = make_engine(&backend, &root, cfg);
            engine.approve_plan(simple_plan(1, vec![])).unwrap();
            timeout(TEST_TIMEOUT, engine.run()).await.unwrap().unwrap();
            let feature = &engine.state().mission.milestones[0].features[0];
            assert_eq!(
                feature.status,
                if succeeds {
                    FeatureStatus::Complete
                } else {
                    FeatureStatus::Failed
                }
            );
            let committed = raw_git(
                &root,
                &[
                    "log",
                    &engine.state().mission.mission_branch,
                    "--format=%H",
                    "--",
                    "attempt.txt",
                ],
            );
            assert_eq!(committed.lines().count(), 1);
            assert_eq!(feature.commits.len(), 1, "{isolation:?}, succeeds={succeeds}: prior attempt's checkpoint must remain attributed");
            assert_eq!(
                feature.commits[0].split_whitespace().next(),
                Some(committed.trim())
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn respawn_bounded_fails_feature_then_mission_continues() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // f-1-1: two failing worker runs. The trusted runner verdict now respawns
    // non-pass workers before any orchestrator judgement; the first respawn
    // is within budget (max_respawns = 1), the second request exceeds it →
    // the ENGINE fails the feature. f-1-2 then succeeds; validators are
    // skipped, contract empty, so the mission completes around the failed
    // feature.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_fail(), // f-1-1 attempt 1
        worker_fail(), // f-1-1 attempt 2 (the one allowed respawn)
        worker_pass(), // f-1-2
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""), // f-1-2
            no_lesson(),
        ]),
    ]));

    let cfg = MissionConfig {
        max_respawns: 1,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(2, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let ms = &engine.state().mission.milestones[0];
    assert_eq!(ms.features[0].status, FeatureStatus::Failed);
    assert_eq!(ms.features[0].respawns, 1, "exactly one respawn honoured");
    assert_eq!(ms.features[0].worker_runs.len(), 2, "two worker runs total");
    assert_eq!(ms.features[1].status, FeatureStatus::Complete);

    // The respawned worker received the runner-failure guidance in its task.
    let specs = backend.started_specs();
    // start order: worker, worker(respawn), worker(f-1-2), orch
    match &specs[1].prompt {
        PromptMode::SingleShot(task) => {
            assert!(
                task.contains("worker run not trusted"),
                "runner failure guidance passed: {task}"
            )
        }
        other => panic!("respawned worker must be single-shot, got {other:?}"),
    }

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::FeatureFailed { feature_id, reason, .. }
            if feature_id == "f-1-1" && reason.contains("respawn budget exhausted")
    )));
}

/// Ticket worker-spawn-auth-failure-budget: a worker whose backend CLI dies
/// in seconds with an auth signature is an INFRASTRUCTURE failure — it must
/// NOT consume the respawn budget or fail the feature. The milestone blocks
/// with a `backend unauthenticated` reason naming the re-auth action; the
/// feature stays Active so it re-runs once the operator re-auths.
#[tokio::test(flavor = "multi_thread")]
async fn worker_auth_death_blocks_milestone_without_burning_respawn_budget() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_auth_death(), // f-1-1: the backend CLI auth-dies instantly
    ]));
    // A generous respawn budget: the point is that NONE of it is consumed.
    let cfg = MissionConfig {
        max_respawns: 3,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();

    // The mission parks for operator action, it does NOT fail the feature.
    assert_eq!(status, MissionStatus::Blocked);
    let ms = &engine.state().mission.milestones[0];
    assert_eq!(
        ms.features[0].status,
        FeatureStatus::Active,
        "the feature stays active (re-runs on re-auth), not failed"
    );
    assert_eq!(
        ms.features[0].respawns, 0,
        "an auth death must not consume the respawn budget"
    );
    assert_eq!(
        ms.features[0].worker_runs.len(),
        1,
        "exactly one worker run — no respawn was spawned"
    );

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::MilestoneBlocked { reason, .. }
                if reason.contains("unauthenticated") && reason.contains("claude")
        )),
        "a milestone.blocked naming the backend and the re-auth action"
    );
    // And the feature was never failed.
    assert!(
        !events.iter().any(|e| matches!(
            &e.kind,
            EventKind::FeatureFailed { feature_id, .. } if feature_id == "f-1-1"
        )),
        "an auth death must not fail the feature"
    );
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
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
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
    assert_eq!(
        snapshot.mission.status,
        MissionStatus::Paused,
        "engine paused while waiting"
    );

    // Queue the user message WHILE PAUSED: a paused engine only drains its
    // inbox, so the message provably sits in pending_user_messages until the
    // resume — which makes the post-resume ordering deterministic (consult
    // turn strictly before the first worker run, matching the script FIFO).
    // Enqueueing Resume first instead would race: the engine could drain the
    // Resume alone and start a worker before ever seeing the message.
    control::enqueue(
        &paths,
        &ControlCommand::Msg {
            text: "swap feature".to_string(),
            interrupt: false,
        },
    )
    .unwrap();
    tokio::time::sleep(Duration::from_millis(700)).await; // a drain tick passes
    control::enqueue(&paths, &ControlCommand::Resume).unwrap();

    let (engine, result) = timeout(TEST_TIMEOUT, handle)
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(result.unwrap(), MissionStatus::Complete);
    let mission_id = engine.mission_id().to_string();
    drop(engine);

    // The completion report folds the paused span out of the elapsed time
    // and says so (mission.paused → mission.resumed is > 1s in this test).
    let report = std::fs::read_to_string(
        root.join(".kranz")
            .join("missions")
            .join(&mission_id)
            .join("report.md"),
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
    assert!(
        paused < user_msg,
        "paused {paused} before user.message {user_msg}"
    );
    assert!(
        user_msg < resumed,
        "user.message {user_msg} queued while paused, before resumed {resumed}"
    );
    assert!(
        resumed < decision,
        "resumed {resumed} before the consult decision {decision}"
    );
    assert!(events.iter().any(|e| matches!(
        &e.kind,
        EventKind::UserMessage { text, interrupt: false } if text == "swap feature"
    )));

    // Delete-after-apply: every applied control file was removed once its
    // event hit the log — the normal path leaves an empty inbox.
    let leftover: Vec<String> = std::fs::read_dir(paths.control_dir())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        leftover.is_empty(),
        "control inbox must be empty after apply: {leftover:?}"
    );
}

// ---------------------------------------------------------------------------
// 5c. Grant-request decision flow (capability denial → operator approve/deny)
// ---------------------------------------------------------------------------

/// A validator denied a command outside its allow-set parks a grant request
/// naming that exact command; approving it extends command_grants (mission-wide)
/// and the re-run validation round clears, driving the mission to Complete.
#[tokio::test]
async fn validator_denial_grant_approved_extends_grants_and_completes() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let denied_cmd = "gc audit --deep";

    // FIFO by session start: worker f-1-1, orchestrator (dirty-tree, judgement,
    // capture), denied validator (round 1 → park), clean validator (round 2
    // after approve → milestone tag).
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        validator_denied(denied_cmd),
        validator_with(json!([])),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let paths = engine.paths().clone();

    let handle = tokio::spawn(async move {
        let result = engine.run().await;
        (engine, result)
    });

    // Wait for the parked request, verify it names the exact command, approve.
    wait_for_pending_grant(&paths).await;
    let snap = reducer::read_snapshot(&paths.state_file()).unwrap();
    let pending = snap.pending_grant_request.expect("parked grant request");
    assert_eq!(pending.command, denied_cmd);
    assert_eq!(pending.milestone_id, "ms-1");
    control::enqueue(
        &paths,
        &ControlCommand::ApproveGrant {
            command: denied_cmd.to_string(),
        },
    )
    .unwrap();

    let (engine, result) = timeout(TEST_TIMEOUT, handle)
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(result.unwrap(), MissionStatus::Complete);
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let types = event_types(&events);
    for expected in [
        "grant.requested",
        "grant.approved",
        "milestone.completed",
        "mission.completed",
    ] {
        assert!(types.contains(&expected), "missing {expected}: {types:?}");
    }
    // request names the exact command; approve carries the same command.
    assert!(events.iter().any(|e| matches!(&e.kind,
        EventKind::GrantRequested { command, milestone_id, .. }
            if command == denied_cmd && milestone_id == "ms-1")));
    // Ordering: request → approve → milestone tag.
    assert!(seq_of(&events, "grant.requested") < seq_of(&events, "grant.approved"));
    assert!(seq_of(&events, "grant.approved") < seq_of(&events, "milestone.completed"));
    // The approved command is now a durable mission-wide grant.
    let state = reducer::fold(&events).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Complete);
    assert!(
        state
            .mission
            .command_grants
            .contains(&denied_cmd.to_string()),
        "approved command must join command_grants: {:?}",
        state.mission.command_grants
    );
    assert!(state.pending_grant_request.is_none());
}

/// Denying a parked grant appends grant.denied and blocks the milestone with
/// the existing refusal semantics — the mission ends Blocked and command_grants
/// is never widened.
#[tokio::test]
async fn validator_denial_grant_denied_blocks_the_milestone() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let denied_cmd = "gc audit --deep";

    // FIFO: worker f-1-1, orchestrator (dirty-tree, judgement), denied
    // validator. Deny blocks the milestone before any final gate, so no
    // capture turn / second validator is scripted.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![dirty_tree_commit_as_is(), judgement("complete", "")]),
        validator_denied(denied_cmd),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let paths = engine.paths().clone();

    let handle = tokio::spawn(async move {
        let result = engine.run().await;
        (engine, result)
    });

    wait_for_pending_grant(&paths).await;
    control::enqueue(
        &paths,
        &ControlCommand::DenyGrant {
            command: denied_cmd.to_string(),
            reason: "not authorized this run".to_string(),
        },
    )
    .unwrap();

    let (engine, result) = timeout(TEST_TIMEOUT, handle)
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(result.unwrap(), MissionStatus::Blocked);
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let types = event_types(&events);
    for expected in ["grant.requested", "grant.denied", "milestone.blocked"] {
        assert!(types.contains(&expected), "missing {expected}: {types:?}");
    }
    // Ordering: denial precedes the block it triggers.
    assert!(seq_of(&events, "grant.denied") < seq_of(&events, "milestone.blocked"));
    let state = reducer::fold(&events).unwrap();
    assert!(
        state.mission.command_grants.is_empty(),
        "deny must never widen command_grants: {:?}",
        state.mission.command_grants
    );
    assert!(state.pending_grant_request.is_none());
}

/// Deny-default safety valve: an unanswered grant request times out to
/// grant.denied on its own (no operator input), blocking the milestone. Proves
/// a parked request can never silently stall a mission open forever.
#[tokio::test]
async fn unanswered_grant_request_times_out_to_denied() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let denied_cmd = "gc audit --deep";

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![dirty_tree_commit_as_is(), judgement("complete", "")]),
        validator_denied(denied_cmd),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    // Fail closed on the very first park-gate tick — no control command needed.
    engine.set_grant_request_timeout(Duration::from_millis(0));
    let paths = engine.paths().clone();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);
    drop(engine);

    let events = read_log(&paths);
    let types = event_types(&events);
    for expected in ["grant.requested", "grant.denied", "milestone.blocked"] {
        assert!(types.contains(&expected), "missing {expected}: {types:?}");
    }
    // The denial reason records the timeout (deny-default), not an operator.
    assert!(events.iter().any(|e| matches!(&e.kind,
        EventKind::GrantDenied { reason, .. } if reason.contains("timed out"))));
    let state = reducer::fold(&events).unwrap();
    assert!(state.mission.command_grants.is_empty());
}

/// An incidental command denial on a validator that STILL produced a trusted
/// PASS must NOT park for a grant (the grant flow is gated on an untrusted
/// outcome). Otherwise a later deny would wrongly block a milestone that
/// actually passed. The milestone completes with no grant.requested.
#[tokio::test]
async fn incidental_denial_on_a_passing_validator_does_not_park() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        validator_denied_but_passing("gc audit --deep"),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let paths = engine.paths().clone();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    drop(engine);

    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(
        !types.contains(&"grant.requested"),
        "a passing validator must not park for a grant: {types:?}"
    );
    assert!(types.contains(&"milestone.completed"));
    assert!(types.contains(&"mission.completed"));
}

/// The per-milestone grant-request cap bounds the park→approve→re-validate loop:
/// once the cap is hit, no further grant is offered and the milestone blocks via
/// the existing refusal path (never spins). Driven with cap = 1 so a single
/// approval reaches the boundary.
#[tokio::test]
async fn grant_request_cap_blocks_instead_of_looping() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let denied_cmd = "gc audit --deep";

    // r1 parks (count→1); after approve, r2 is over the cap (1 ≥ 1) → falls
    // through to the Claude retry (3rd denied script) → still over cap → block.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![dirty_tree_commit_as_is(), judgement("complete", "")]),
        validator_denied(denied_cmd),
        validator_denied(denied_cmd),
        validator_denied(denied_cmd),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    engine.set_grant_request_cap(1);
    let paths = engine.paths().clone();

    let handle = tokio::spawn(async move {
        let result = engine.run().await;
        (engine, result)
    });

    // Approve the one grant the cap allows; the next round must block, not spin.
    wait_for_pending_grant(&paths).await;
    control::enqueue(
        &paths,
        &ControlCommand::ApproveGrant {
            command: denied_cmd.to_string(),
        },
    )
    .unwrap();

    let (engine, result) = timeout(TEST_TIMEOUT, handle)
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(result.unwrap(), MissionStatus::Blocked);
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    // Exactly one grant was offered (the cap), then the milestone blocked.
    let requested = events
        .iter()
        .filter(|e| e.kind.type_name() == "grant.requested")
        .count();
    assert_eq!(requested, 1, "cap = 1 must offer exactly one grant");
    assert!(event_types(&events).contains(&"milestone.blocked"));
}

/// A denial the runner can only read on the CLAUDE RETRY (the primary validator
/// backend produced no capturable command) still surfaces a grant. Guards the
/// post-retry grant check, so Codex/Droid-backed validators aren't silently
/// un-grantable.
#[tokio::test]
async fn grant_offered_from_the_claude_retry_when_primary_had_no_command() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let denied_cmd = "gc audit --deep";

    // Round 1: primary validator untrusted with NO command → retry with Claude,
    // which IS denied → grant parked. Approve → round 2 clean → complete.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        validator_untrusted_no_denial(),
        validator_denied(denied_cmd),
        validator_with(json!([])),
    ]));

    let cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let paths = engine.paths().clone();

    let handle = tokio::spawn(async move {
        let result = engine.run().await;
        (engine, result)
    });

    wait_for_pending_grant(&paths).await;
    let snap = reducer::read_snapshot(&paths.state_file()).unwrap();
    assert_eq!(
        snap.pending_grant_request.expect("parked").command,
        denied_cmd,
        "the grant must name the command the retry surfaced"
    );
    control::enqueue(
        &paths,
        &ControlCommand::ApproveGrant {
            command: denied_cmd.to_string(),
        },
    )
    .unwrap();

    let (engine, result) = timeout(TEST_TIMEOUT, handle)
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(result.unwrap(), MissionStatus::Complete);
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    assert!(event_types(&events).contains(&"grant.requested"));
    let state = reducer::fold(&events).unwrap();
    assert!(state
        .mission
        .command_grants
        .contains(&denied_cmd.to_string()));
}

/// A worker write outside the touch_set parks a TOUCH-PATH grant naming the
/// path (driven by the out-of-contract sweep, validators skipped). Approving it
/// extends touch_set so the re-validation sweep is clean and the mission
/// completes.
#[tokio::test]
async fn out_of_contract_write_parks_a_touch_grant_and_approve_completes() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // A plan with a touch_set the worker then writes OUTSIDE of.
    let plan = Plan {
        goal: GOAL.to_string(),
        validation_contract: vec![],
        milestones: vec![PlanMilestone {
            title: "M1".to_string(),
            features: vec![PlanFeature {
                title: "feature 1".to_string(),
                spec: "build part 1".to_string(),
                validation_criteria: vec!["part 1 works".to_string()],
            }],
        }],
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec!["src/**".to_string()],
        standards_manifest: None,
        reviewer_independence: None,
    };
    let worker = MockScript::single_shot_json(&json!({
        "result": "pass",
        "summary": "implemented",
        "filesTouched": ["out-of-bounds.txt"],
        "testsAdded": [],
        "testEvidence": "ok",
        "commits": []
    }))
    .writes_file("out-of-bounds.txt", "written outside the touch-set\n");

    // Validators skipped (test_cfg default): the engine's out-of-contract sweep
    // alone produces the finding, so no validator scripts are needed.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker,
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(plan).unwrap();
    let paths = engine.paths().clone();

    let handle = tokio::spawn(async move {
        let result = engine.run().await;
        (engine, result)
    });

    wait_for_pending_grant(&paths).await;
    let snap = reducer::read_snapshot(&paths.state_file()).unwrap();
    let pending = snap.pending_grant_request.expect("parked touch grant");
    assert_eq!(pending.kind, GrantKind::TouchPath);
    assert_eq!(pending.command, "out-of-bounds.txt");
    control::enqueue(
        &paths,
        &ControlCommand::ApproveGrant {
            command: "out-of-bounds.txt".to_string(),
        },
    )
    .unwrap();

    let (engine, result) = timeout(TEST_TIMEOUT, handle)
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(result.unwrap(), MissionStatus::Complete);
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let types = event_types(&events);
    for expected in [
        "grant.requested",
        "grant.approved",
        "milestone.completed",
        "mission.completed",
    ] {
        assert!(types.contains(&expected), "missing {expected}: {types:?}");
    }
    // The approved path joined touch_set (extend-only), not command_grants.
    let state = reducer::fold(&events).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Complete);
    assert!(
        state
            .mission
            .touch_set
            .contains(&"out-of-bounds.txt".to_string()),
        "approved path must join touch_set: {:?}",
        state.mission.touch_set
    );
    assert!(state.mission.command_grants.is_empty());
}

/// Denying a touch grant does NOT block the milestone (unlike a command deny):
/// the out-of-contract write flows to the normal fix/waive path. Here the
/// orchestrator waives it and the mission completes — touch_set never widened.
#[tokio::test]
async fn out_of_contract_write_touch_grant_denied_flows_to_waive() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let plan = Plan {
        goal: GOAL.to_string(),
        validation_contract: vec![],
        milestones: vec![PlanMilestone {
            title: "M1".to_string(),
            features: vec![PlanFeature {
                title: "feature 1".to_string(),
                spec: "build part 1".to_string(),
                validation_criteria: vec!["part 1 works".to_string()],
            }],
        }],
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec!["src/**".to_string()],
        standards_manifest: None,
        reviewer_independence: None,
    };
    let worker = MockScript::single_shot_json(&json!({
        "result": "pass",
        "summary": "implemented",
        "filesTouched": ["out-of-bounds.txt"],
        "testsAdded": [],
        "testEvidence": "ok",
        "commits": []
    }))
    .writes_file("out-of-bounds.txt", "written outside the touch-set\n");

    // After the deny, the re-validation round's sweep re-produces the finding;
    // the cap is saturated (no re-offer) so it reaches the conversion turn,
    // which waives it → milestone completes → capture turn.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker,
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            waive_reply("out-of-bounds.txt", "acceptable scratch file"),
            no_lesson(),
        ]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(plan).unwrap();
    let paths = engine.paths().clone();

    let handle = tokio::spawn(async move {
        let result = engine.run().await;
        (engine, result)
    });

    wait_for_pending_grant(&paths).await;
    control::enqueue(
        &paths,
        &ControlCommand::DenyGrant {
            command: "out-of-bounds.txt".to_string(),
            reason: "keep it out of contract".to_string(),
        },
    )
    .unwrap();

    let (engine, result) = timeout(TEST_TIMEOUT, handle)
        .await
        .expect("run must not hang")
        .unwrap();
    // Waived, not blocked → the mission completes.
    assert_eq!(result.unwrap(), MissionStatus::Complete);
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(types.contains(&"grant.denied"), "{types:?}");
    // A touch deny must NOT block the milestone (that's the command-deny path).
    assert!(!types.contains(&"milestone.blocked"), "{types:?}");
    // The out-of-contract finding was recorded (flowed to fix/waive), and the
    // denied path was never added to touch_set.
    assert!(types.contains(&"validation.finding"), "{types:?}");
    let state = reducer::fold(&events).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Complete);
    assert!(
        !state
            .mission
            .touch_set
            .contains(&"out-of-bounds.txt".to_string()),
        "deny must not widen touch_set: {:?}",
        state.mission.touch_set
    );
}

/// A worker command blocked by a deny rule parks a WORKER-DENY grant naming the
/// RULE (not the command). Approving it adds the rule to deny_exceptions —
/// subtracting it from the worker deny set — and the respawned worker completes.
#[tokio::test]
async fn worker_deny_grant_lifts_the_rule_and_respawn_completes() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let denied_cmd = "git push origin main";

    // Worker run 1: blocked by a denied `git push` and reports FAIL (the deny
    // gate only offers a guardrail-lift when the worker didn't succeed anyway).
    // Clean tree (no deliverable) so there's no dirty-tree turn before the park.
    let tool_use = json!({
        "type": "assistant",
        "message": { "id": "w1", "content": [
            { "type": "tool_use", "name": "Bash", "input": { "command": denied_cmd } }
        ] }
    })
    .to_string();
    let denied = json!({
        "type": "user",
        "message": { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "t1",
              "content": format!("Permission denied: Bash({denied_cmd})"), "is_error": true }
        ] }
    })
    .to_string();
    let report1 = json!({
        "result": "fail", "summary": "blocked from pushing",
        "filesTouched": [], "testsAdded": [], "testEvidence": "", "commits": []
    });
    let mut w1 = vec![mock_init("w1")];
    w1.extend(parse_stream_line(&tool_use));
    w1.extend(parse_stream_line(&denied));
    w1.push(mock_text(&report1.to_string()));
    w1.push(mock_result_json(&report1));
    let worker_denied = MockScript {
        events: w1,
        ..Default::default()
    };

    // FIFO: worker run 1 (denied → park), worker run 2 (respawn after lift,
    // writes a deliverable), orchestrator (dirty-tree, judgement, capture).
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_denied,
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let paths = engine.paths().clone();

    let handle = tokio::spawn(async move {
        let result = engine.run().await;
        (engine, result)
    });

    wait_for_pending_grant(&paths).await;
    let snap = reducer::read_snapshot(&paths.state_file()).unwrap();
    let pending = snap
        .pending_grant_request
        .expect("parked worker-deny grant");
    assert_eq!(pending.kind, GrantKind::WorkerDeny);
    // The grant target is the RULE the operator lifts, not the raw command.
    assert_eq!(pending.command, "Bash(git push*)");
    control::enqueue(
        &paths,
        &ControlCommand::ApproveGrant {
            command: "Bash(git push*)".to_string(),
        },
    )
    .unwrap();

    let (engine, result) = timeout(TEST_TIMEOUT, handle)
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(result.unwrap(), MissionStatus::Complete);
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let types = event_types(&events);
    for expected in ["grant.requested", "grant.approved", "mission.completed"] {
        assert!(types.contains(&expected), "missing {expected}: {types:?}");
    }
    // The lifted rule is now a durable, logged deny exception.
    let state = reducer::fold(&events).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Complete);
    assert!(
        state
            .mission
            .deny_exceptions
            .contains(&"Bash(git push*)".to_string()),
        "the lifted rule must join deny_exceptions: {:?}",
        state.mission.deny_exceptions
    );
}

/// A worker that hit a denial but STILL reported success must NOT park a
/// worker-deny grant — eroding a guardrail for a run that already succeeded
/// would be a spurious prompt. The milestone completes with no grant.requested.
#[tokio::test]
async fn worker_denial_on_a_passing_run_does_not_park() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let denied_cmd = "git push origin main";

    // Worker reports PASS despite a denied `git push`, and writes a deliverable.
    let tool_use = json!({
        "type": "assistant",
        "message": { "id": "w1", "content": [
            { "type": "tool_use", "name": "Bash", "input": { "command": denied_cmd } }
        ] }
    })
    .to_string();
    let denied = json!({
        "type": "user",
        "message": { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "t1",
              "content": format!("Permission denied: Bash({denied_cmd})"), "is_error": true }
        ] }
    })
    .to_string();
    let report = json!({
        "result": "pass", "summary": "shipped without the push",
        "filesTouched": ["delivered.txt"], "testsAdded": [], "testEvidence": "ok", "commits": []
    });
    let mut w = vec![mock_init("w1")];
    w.extend(parse_stream_line(&tool_use));
    w.extend(parse_stream_line(&denied));
    w.push(mock_text(&report.to_string()));
    w.push(mock_result_json(&report));
    let worker = MockScript {
        events: w,
        ..Default::default()
    }
    .writes_file("delivered.txt", "shipped\n");

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker,
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let paths = engine.paths().clone();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    drop(engine);

    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(
        !types.contains(&"grant.requested"),
        "a passing worker must not park a deny-lift grant: {types:?}"
    );
    assert!(types.contains(&"mission.completed"));
}

/// A worker-deny grant respawn does NOT consume the failure-retry budget: with
/// `max_respawns = 1`, a grant respawn followed by a genuine judgement respawn
/// still completes (it would fail "respawn budget exhausted" without the
/// grant-respawn decoupling).
#[tokio::test]
async fn worker_deny_grant_respawn_does_not_eat_the_respawn_budget() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let denied_cmd = "git push origin main";

    // Run 1: denied `git push`, reports fail → parks a deny-lift grant.
    let tool_use = json!({
        "type": "assistant",
        "message": { "id": "w1", "content": [
            { "type": "tool_use", "name": "Bash", "input": { "command": denied_cmd } }
        ] }
    })
    .to_string();
    let denied = json!({
        "type": "user",
        "message": { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "t1",
              "content": format!("Permission denied: Bash({denied_cmd})"), "is_error": true }
        ] }
    })
    .to_string();
    let report1 = json!({ "result": "fail", "summary": "blocked from pushing" });
    let mut w1 = vec![mock_init("w1")];
    w1.extend(parse_stream_line(&tool_use));
    w1.extend(parse_stream_line(&denied));
    w1.push(mock_text(&report1.to_string()));
    w1.push(mock_result_json(&report1));
    let worker_denied = MockScript {
        events: w1,
        ..Default::default()
    };

    // FIFO: run 1 (denied → park; grant respawn), run 2 (plain fail → the
    // deterministic runner-verdict respawn, NO orch turn), run 3 (pass,
    // deliverable). Orch turns: dirty-tree + judge run 3 (complete), capture.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_denied,
        worker_fail(),
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));

    // max_respawns = 1: the ONE judgement respawn (run 2 → run 3) must survive
    // the earlier grant respawn (run 1 → run 2), which the decoupling exempts.
    let cfg = MissionConfig {
        max_respawns: 1,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let paths = engine.paths().clone();

    let handle = tokio::spawn(async move {
        let result = engine.run().await;
        (engine, result)
    });

    wait_for_pending_grant(&paths).await;
    control::enqueue(
        &paths,
        &ControlCommand::ApproveGrant {
            command: "Bash(git push*)".to_string(),
        },
    )
    .unwrap();

    let (engine, result) = timeout(TEST_TIMEOUT, handle)
        .await
        .expect("run must not hang")
        .unwrap();
    // Completes — the grant respawn did NOT exhaust the retry budget.
    assert_eq!(result.unwrap(), MissionStatus::Complete);
    // Three spawns for the one feature: initial + grant respawn + judgement
    // respawn. Two respawns recorded where max_respawns = 1 allows only one
    // judgement-driven retry — proof the grant respawn was exempted, not
    // merely that the mission completed some other way.
    let feature = &engine.state().mission.milestones[0].features[0];
    assert_eq!(feature.status, FeatureStatus::Complete);
    assert_eq!(feature.worker_runs.len(), 3, "initial + 2 respawns");
    assert_eq!(feature.respawns, 2, "grant respawn + judgement respawn");
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(types.contains(&"mission.completed"), "{types:?}");
    assert!(
        !events.iter().any(|e| matches!(&e.kind,
            EventKind::FeatureFailed { reason, .. } if reason.contains("respawn budget"))),
        "the grant respawn must not exhaust the budget"
    );
}

/// Egress grant (3.3b): a sandboxed (`fs+net`) validator refused a destination
/// by its egress proxy parks an EGRESS grant naming `host:port`; approving
/// extends `egress_grants` (and ONLY egress_grants), the re-run's proxy
/// allowlist covers the granted host, and the milestone completes. Mirrors
/// `validator_denial_grant_approved_extends_grants_and_completes`.
///
/// macOS-only: the per-host denial signal exists only where `fs+net` resolves
/// to Seatbelt + the filtering proxy (Linux bwrap spawns no proxy — 3.3a).
#[cfg(target_os = "macos")]
#[tokio::test]
async fn egress_denial_grant_approved_extends_egress_grants_and_completes() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let denied_host = "registry.npmjs.org";
    let target = format!("{denied_host}:443");

    // FIFO by session start: worker f-1-1, orchestrator (dirty-tree, judgement,
    // capture), egress-denied validator (round 1 → park), clean validator
    // (round 2 after approve → milestone tag).
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        validator_egress_denied(denied_host, 443),
        validator_with(json!([])),
    ]));

    let mut cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    cfg.validator_functional.sandbox.enforce = SandboxEnforce::FsNet;
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let paths = engine.paths().clone();

    let handle = tokio::spawn(async move {
        let result = engine.run().await;
        (engine, result)
    });

    // Wait for the parked request: it must name the refused destination and
    // carry the Egress kind — never silently lumped with command grants.
    wait_for_pending_grant(&paths).await;
    let snap = reducer::read_snapshot(&paths.state_file()).unwrap();
    let pending = snap.pending_grant_request.expect("parked grant request");
    assert_eq!(pending.kind, GrantKind::Egress);
    assert_eq!(pending.command, target);
    assert_eq!(pending.milestone_id, "ms-1");
    control::enqueue(
        &paths,
        &ControlCommand::ApproveGrant {
            command: target.clone(),
        },
    )
    .unwrap();

    let (engine, result) = timeout(TEST_TIMEOUT, handle)
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(result.unwrap(), MissionStatus::Complete);
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let types = event_types(&events);
    for expected in [
        "grant.requested",
        "grant.approved",
        "milestone.completed",
        "mission.completed",
    ] {
        assert!(types.contains(&expected), "missing {expected}: {types:?}");
    }
    assert!(events.iter().any(|e| matches!(&e.kind,
        EventKind::GrantRequested { kind: GrantKind::Egress, command, milestone_id }
            if command == &target && milestone_id == "ms-1")));
    assert!(seq_of(&events, "grant.requested") < seq_of(&events, "grant.approved"));
    assert!(seq_of(&events, "grant.approved") < seq_of(&events, "milestone.completed"));

    let state = reducer::fold(&events).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Complete);
    // Capability honesty: the destination joined egress_grants and ONLY
    // egress_grants — command_grants and touch_set are untouched.
    assert!(
        state.mission.egress_grants.contains(&target),
        "approved destination must join egress_grants: {:?}",
        state.mission.egress_grants
    );
    assert!(state.mission.command_grants.is_empty());
    assert!(state.mission.touch_set.is_empty());
    assert!(state.pending_grant_request.is_none());

    // The re-run's proxy allowlist covers the granted host: the round-2
    // validator's spec folded the grant into its sandbox egress inputs — the
    // exact list `effective_egress` extends into the proxy allowlist at start.
    let revalidated = backend
        .started_specs()
        .last()
        .expect("a re-run validator session started")
        .clone();
    let sandbox = revalidated.sandbox.expect("fs+net sandbox on the re-run");
    assert!(
        sandbox.inputs.egress.contains(&target),
        "the re-run's proxy allowlist must contain the granted host: {:?}",
        sandbox.inputs.egress
    );

    // And the denial JSONL holds exactly the round-1 denial: the re-run was
    // not refused again (its outcome carried no denied_egress, or the grant
    // flow would have re-parked instead of completing).
    let content = std::fs::read_to_string(paths.egress_denials_file()).unwrap();
    assert_eq!(
        content.lines().count(),
        1,
        "only the pre-grant denial is recorded: {content}"
    );
}

/// Denying a parked egress grant blocks the milestone with the egress reason
/// (the same refusal semantics as a command grant) and never widens
/// `egress_grants`. Mirrors `validator_denial_grant_denied_blocks_the_milestone`.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn egress_denial_grant_denied_blocks_the_milestone() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let denied_host = "registry.npmjs.org";
    let target = format!("{denied_host}:443");

    // FIFO: worker f-1-1, orchestrator (dirty-tree, judgement), egress-denied
    // validator. Deny blocks the milestone before any final gate, so no
    // capture turn / second validator is scripted.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![dirty_tree_commit_as_is(), judgement("complete", "")]),
        validator_egress_denied(denied_host, 443),
    ]));

    let mut cfg = MissionConfig {
        skip_functional: false,
        ..test_cfg()
    };
    cfg.validator_functional.sandbox.enforce = SandboxEnforce::FsNet;
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let paths = engine.paths().clone();

    let handle = tokio::spawn(async move {
        let result = engine.run().await;
        (engine, result)
    });

    wait_for_pending_grant(&paths).await;
    control::enqueue(
        &paths,
        &ControlCommand::DenyGrant {
            command: target.clone(),
            reason: "not authorized this run".to_string(),
        },
    )
    .unwrap();

    let (engine, result) = timeout(TEST_TIMEOUT, handle)
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(result.unwrap(), MissionStatus::Blocked);
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let types = event_types(&events);
    for expected in ["grant.requested", "grant.denied", "milestone.blocked"] {
        assert!(types.contains(&expected), "missing {expected}: {types:?}");
    }
    assert!(seq_of(&events, "grant.denied") < seq_of(&events, "milestone.blocked"));
    // The block honestly names the egress boundary, the refused destination,
    // and the operator's reason.
    assert!(
        events.iter().any(|e| matches!(&e.kind,
            EventKind::MilestoneBlocked { reason, .. }
                if reason.starts_with("egress denied:")
                    && reason.contains(&target)
                    && reason.contains("not authorized this run"))),
        "the block must carry the egress denial reason"
    );
    let state = reducer::fold(&events).unwrap();
    assert!(
        state.mission.egress_grants.is_empty(),
        "deny must never widen egress_grants: {:?}",
        state.mission.egress_grants
    );
    assert!(state.pending_grant_request.is_none());
}

// ---------------------------------------------------------------------------
// 5b. Scrubbing: orchestrator decision detail (structured-field leak)
// ---------------------------------------------------------------------------

#[test]
fn mission_created_secret_redacts_and_audits() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let secret = "sk-ant-api03-AbCdEf_123-xyz";
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::with_scripts(vec![]));

    let engine = MissionEngine::create(backend, &root, &format!("ship with {secret}"), test_cfg())
        .expect("create mission");
    let paths = engine.paths().clone();
    drop(engine);

    let raw_log = std::fs::read_to_string(paths.events_file()).unwrap();
    assert!(
        !raw_log.contains(secret),
        "events.jsonl leaked secret: {raw_log}"
    );
    assert!(raw_log.contains("[REDACTED]"));
    let events = read_log(&paths);
    assert!(events.iter().any(|event| {
        matches!(
            &event.kind,
            EventKind::SecretRedacted {
                rule_id,
                location,
                ..
            } if rule_id == "anthropic-api-key" && location.contains("/payload/goal")
        )
    }));
}

// ---------------------------------------------------------------------------
// 5c. f-1-2: executor routing on the `MissionEngine::create` integration
// seam. Every ticket-seeded mission (kranz draft, kranz exec, REST, Slack)
// creates its mission from `Ticket::mission_goal()`'s folded goal string —
// this is the one wiring point that must apply routing, so it is exercised
// here directly rather than only through `route_task_class_executor`'s pure
// unit tests (which pass regardless of whether any caller ever invokes them).
// ---------------------------------------------------------------------------

/// A goal folded from an execution-class ticket (mirrors `kranz exec`'s
/// `read_mission_file` -> `Ticket::mission_goal()` seed path) must land the
/// Worker on the local tier by the time `mission.created` is emitted.
#[test]
fn create_routes_execution_class_ticket_goal_to_local_executor() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::with_scripts(vec![]));

    let ticket = kranz_engine::ticket::Ticket::parse(
        "bump-dep",
        "\
---
title: Bump a dependency
task-class: execution-class
---

## Goal
Bump the dependency to the latest patch release.
",
    )
    .expect("parse ticket");
    let goal = ticket.mission_goal();

    let mut cfg = test_cfg();
    cfg.worker.base_url = Some("http://127.0.0.1:8080".to_string());
    cfg.worker.context_budget = Some(16_384);

    let engine = MissionEngine::create(backend, &root, &goal, cfg).expect("create routed mission");

    assert_eq!(engine.state().executor_tier(), ExecutorTier::Local);
    assert_eq!(
        engine.state().config.worker.backend.as_deref(),
        Some("local")
    );
    // Validator stays frontier throughout, per the mission's D-X decision.
    assert_ne!(
        engine.state().config.validator_scrutiny.backend.as_deref(),
        Some("local")
    );

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::OrchestratorDecision { summary, .. }
                if summary.contains("executor routed local")
        )),
        "expected a recorded routing decision; events: {:?}",
        event_types(&events)
    );
}

/// A goal with no task class (a plain non-ticket mission, or a ticket with no
/// `task-class` set) must leave the executor on the frontier default even
/// when a local endpoint happens to be configured.
#[test]
fn create_leaves_executor_frontier_when_goal_carries_no_task_class() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::with_scripts(vec![]));

    let mut cfg = test_cfg();
    cfg.worker.base_url = Some("http://127.0.0.1:8080".to_string());
    cfg.worker.context_budget = Some(16_384);

    let engine = MissionEngine::create(backend, &root, GOAL, cfg).expect("create unrouted mission");

    assert_eq!(engine.state().executor_tier(), ExecutorTier::Frontier);
    assert_eq!(engine.state().config.worker.backend, None);
}

// ---------------------------------------------------------------------------
// 5d. Tracked routing rules (ticket routing-rules-config): the base-branch-
// owned `.kranz/routing-rules.json` populates the routing table at create,
// fails closed at draft/approve, and a mission-branch edit is ignored and
// surfaced. The pure parse/validate/determinism cases live in
// `routing_rules.rs`/`routing.rs`; these are the integration seams.
// ---------------------------------------------------------------------------

/// Commit `.kranz/routing-rules.json` on the CURRENT branch (mirrors
/// `commit_workspace_contract`).
fn commit_routing_rules(root: &Path, rules_json: &str) {
    std::fs::create_dir_all(root.join(".kranz")).unwrap();
    std::fs::write(root.join(".kranz").join("routing-rules.json"), rules_json).unwrap();
    raw_git(root, &["add", ".kranz/routing-rules.json"]);
    raw_git(root, &["commit", "-m", "routing rules"]);
}

fn execution_class_goal() -> String {
    kranz_engine::ticket::Ticket::parse(
        "bump-dep",
        "\
---
title: Bump a dependency
task-class: execution-class
---

## Goal
Bump the dependency to the latest patch release.
",
    )
    .expect("parse ticket")
    .mission_goal()
}

/// A valid rules file on the base branch IS the mission's routing table:
/// the pattern rule routes the execution-class ticket local (endpoint
/// configured), both rule forms land on `mission.created`'s config, and the
/// load is recorded beside the routing decision.
#[test]
fn routing_rules_config_create_loads_base_rules_and_routes() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_routing_rules(
        &root,
        r#"{
            "taskClassRules": [
                {"taskClass": "docs-class", "tier": "frontier"}
            ],
            "patternRules": [
                {"pattern": "execution-*", "tier": "local"}
            ]
        }"#,
    );
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::with_scripts(vec![]));

    let mut cfg = test_cfg();
    cfg.worker.base_url = Some("http://127.0.0.1:8080".to_string());
    cfg.worker.context_budget = Some(16_384);
    let engine =
        MissionEngine::create(backend, &root, &execution_class_goal(), cfg).expect("create");

    // The file populated the table (both forms), and the pattern rule
    // routed the class local.
    assert_eq!(engine.state().executor_tier(), ExecutorTier::Local);
    assert_eq!(engine.state().config.routing.task_class_rules.len(), 1);
    assert_eq!(engine.state().config.routing.pattern_rules.len(), 1);
    assert_eq!(
        engine.state().config.routing.pattern_rules[0].pattern,
        "execution-*"
    );

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::OrchestratorDecision { summary, .. }
                if summary.contains("routing rules loaded from .kranz/routing-rules.json (base branch \"main\"): 1 task-class rule(s), 1 pattern rule(s)")
        )),
        "expected the rules-load record; events: {:?}",
        event_types(&events)
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::OrchestratorDecision { summary, .. }
                if summary.contains("executor routed local (routing-table rule)")
        )),
        "expected the table-rule routing decision; events: {:?}",
        event_types(&events)
    );
}

/// Present-but-invalid rules fail the DRAFT closed — create errors naming
/// the file, the rule index, and the field, BEFORE any mission side effects.
#[test]
fn routing_rules_config_invalid_rules_fail_draft_closed_naming_the_rule() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_routing_rules(
        &root,
        r#"{"taskClassRules": [{"taskClass": "ok-class", "tier": "local"}, {"taskClass": " ", "tier": "frontier"}]}"#,
    );
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::with_scripts(vec![]));

    let err = match MissionEngine::create(backend, &root, &execution_class_goal(), test_cfg()) {
        Ok(_) => panic!("an invalid rules file must refuse mission creation"),
        Err(err) => err,
    };
    let text = format!("{err}");
    assert!(text.contains(".kranz/routing-rules.json"), "{text}");
    assert!(text.contains("taskClassRules[1].taskClass"), "{text}");
    assert!(text.contains("owner: repo-setup"), "{text}");
    assert!(
        !root.join(".kranz").join("missions").exists(),
        "a refused draft leaves no mission side effects"
    );
}

/// Present-but-invalid rules at APPROVE time fail approval closed, mirroring
/// the workspace contract's approve-time validation — even though this
/// mission's route was already pinned (validly) at create.
#[test]
fn routing_rules_config_invalid_rules_fail_approve_closed() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_routing_rules(
        &root,
        r#"{"patternRules": [{"pattern": "*", "tier": "frontier"}]}"#,
    );
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());

    // The rules go stale-invalid on the base between create and approve.
    std::fs::write(
        root.join(".kranz").join("routing-rules.json"),
        r#"{"patternRules": [{"pattern": "", "tier": "frontier"}]}"#,
    )
    .unwrap();
    raw_git(&root, &["add", ".kranz/routing-rules.json"]);
    raw_git(&root, &["commit", "-m", "break the routing rules"]);

    let err = engine
        .approve_plan(simple_plan(1, vec![]))
        .expect_err("approve must fail closed on invalid base rules");
    let text = format!("{err}");
    assert!(text.contains(".kranz/routing-rules.json"), "{text}");
    assert!(text.contains("patternRules[0].pattern"), "{text}");
    assert!(text.contains("owner: repo-setup"), "{text}");
}

/// Regression: NO rules file ⇒ today's behavior byte-for-byte — the legacy
/// literal floor routes, the table stays empty, and no rules-load record
/// appears.
#[test]
fn routing_rules_config_no_file_keeps_legacy_floor_byte_identical() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::with_scripts(vec![]));

    let mut cfg = test_cfg();
    cfg.worker.base_url = Some("http://127.0.0.1:8080".to_string());
    cfg.worker.context_budget = Some(16_384);
    let engine =
        MissionEngine::create(backend, &root, &execution_class_goal(), cfg).expect("create");

    assert!(engine.state().config.routing.is_empty());
    assert_eq!(engine.state().executor_tier(), ExecutorTier::Local);
    assert_eq!(
        engine.state().config.worker.backend.as_deref(),
        Some("local")
    );

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    assert!(
        !events.iter().any(|e| matches!(
            &e.kind,
            EventKind::OrchestratorDecision { summary, .. } if summary.contains("routing rules loaded")
        )),
        "no file ⇒ no rules-load record"
    );
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::OrchestratorDecision { summary, .. }
                if summary.contains("executor routed local (execution-class)")
        )),
        "the legacy literal-floor decision is unchanged; events: {:?}",
        event_types(&events)
    );
}

/// Ownership end-to-end: the mission branch edits the rules file; the edit
/// can never re-route the mission (the base's copy pinned the route at
/// creation), the attempt is surfaced on the decision log at run time, and
/// the worker's `worker.spawned` records the effective route plus the
/// deciding rule. Here the base rule routes local but no endpoint is
/// configured, so the EFFECTIVE tier fails safe to frontier while the record
/// still names the rule — requested vs effective is exactly the honesty the
/// provenance exists for.
#[tokio::test(flavor = "multi_thread")]
async fn routing_rules_config_mission_branch_edit_ignored_and_surfaced() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    commit_routing_rules(
        &root,
        r#"{"taskClassRules": [{"taskClass": "execution-class", "tier": "local"}]}"#,
    );

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let backend_dyn: Arc<dyn AgentBackend> = backend.clone();
    let mut engine = MissionEngine::create(backend_dyn, &root, &execution_class_goal(), test_cfg())
        .expect("create routed mission");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    // The mission branch "weakens" the rules (approve in checkout mode left
    // the primary checkout ON the mission branch): every class routes local.
    commit_routing_rules(
        &root,
        r#"{"patternRules": [{"pattern": "*", "tier": "local"}]}"#,
    );

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);

    // Surfaced: the inert mission-branch edit is operator-visible.
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::OrchestratorDecision { summary, .. }
                if summary.contains("edits .kranz/routing-rules.json — ignored: routing rules are base-branch-owned")
        )),
        "expected the branch-edit surface note; decisions: {:?}",
        events.iter().filter_map(|e| match &e.kind {
            EventKind::OrchestratorDecision { summary, .. } => Some(summary),
            _ => None,
        }).collect::<Vec<_>>()
    );

    // Ignored: the worker ran the BASE rules' route — the exact rule
    // matched, the effective tier failed safe to frontier (no endpoint), and
    // the mission branch's `* → local` never entered the record.
    let worker_route = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::WorkerSpawned {
                role: Role::Worker,
                executor_route,
                ..
            } => Some(executor_route.clone()),
            _ => None,
        })
        .expect("a worker.spawned with a route record");
    let route = worker_route.expect("the worker session carries route provenance");
    assert_eq!(route.tier, ExecutorTier::Frontier);
    assert_eq!(route.rule.as_deref(), Some("taskClassRules[0]"));

    // Non-worker sessions are never routed: no record.
    assert!(events.iter().all(|e| match &e.kind {
        EventKind::WorkerSpawned {
            role: Role::Orchestrator,
            executor_route,
            ..
        } => executor_route.is_none(),
        _ => true,
    }));
}

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
        orch_script(vec![
            dirty_tree_commit_as_is(),
            leaky_judgement,
            no_lesson(),
        ]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
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
            EventKind::OrchestratorDecision {
                summary,
                detail: Some(detail),
            } if summary.starts_with("judgement for") => Some((summary.clone(), detail.clone())),
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
async fn resume_retains_prior_checkpoint_commit_receipts() {
    if !setup() {
        return;
    }
    for isolation in [WorkerIsolation::Checkout, WorkerIsolation::Worktree] {
        let (_dir, root) = init_repo();
        let backend1 = Arc::new(MockBackend::with_scripts(vec![
            worker_pass_no_write().writes_file("before-crash.txt", "retained work\n"),
            orch_script(vec![dirty_tree_commit_as_is()]),
        ]));
        let mut engine = make_engine(
            &backend1,
            &root,
            MissionConfig {
                worker_isolation: isolation,
                ..test_cfg()
            },
        );
        engine.approve_plan(simple_plan(1, vec![])).unwrap();
        engine.set_orch_stall_timeout(Duration::from_millis(400));
        let mission_id = engine.mission_id().to_string();
        let paths = engine.paths().clone();
        timeout(TEST_TIMEOUT, engine.run())
            .await
            .expect("first run must not hang")
            .expect_err("judgement stalls after the checkpoint");
        drop(engine);
        let before = reducer::fold(&read_log(&paths)).unwrap();
        let feature = &before.mission.milestones[0].features[0];
        assert_eq!(feature.status, FeatureStatus::Active);
        assert_eq!(feature.commits.len(), 1);
        assert!(before.feature_base_shas.contains_key(&feature.id));

        // Resume must reconstruct the baseline and receipts from the log,
        // even when the second worker only verifies the existing commit.
        let backend2 = Arc::new(MockBackend::with_scripts(vec![
            worker_pass_no_write(),
            orch_script(vec![judgement("complete", "verified"), no_lesson()]),
        ]));
        let backend2_dyn: Arc<dyn AgentBackend> = backend2;
        let mut engine =
            MissionEngine::resume(backend2_dyn, &root, &mission_id, LockForce::No).unwrap();
        engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
        assert_eq!(
            timeout(TEST_TIMEOUT, engine.run()).await.unwrap().unwrap(),
            MissionStatus::Complete
        );
        let after = &engine.state().mission.milestones[0].features[0];
        assert_eq!(after.commits, feature.commits, "{isolation:?}");
        assert_eq!(engine.state().feature_base_shas, before.feature_base_shas);
        assert_eq!(
            raw_git(
                &root,
                &[
                    "show",
                    &format!("{}:before-crash.txt", before.mission.mission_branch)
                ]
            ),
            "retained work\n"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_and_resume_completes_on_single_log() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // --- Phase 1: the "crash" -------------------------------------------
    // The first worker reaches a trusted pass, then the judgement turn stalls:
    // the orchestrator script has NO on_message batches, so the session parks,
    // the (shortened) stall timeout declares it dead, the retry re-seeds — and
    // the backend has no script left, so run() errors out mid-feature. That is
    // our in-process kill.
    let backend1 = Arc::new(MockBackend::with_scripts(vec![
        worker_pass_no_write(),
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
            EventKind::WorkerSpawned {
                role: Role::Orchestrator,
                sdk_session_id,
                ..
            } => Some(sdk_session_id.clone()),
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
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        worker_pass(), // f-1-2
    ]));
    let backend2_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend2) as Arc<dyn AgentBackend>;
    let mut engine = MissionEngine::resume(backend2_dyn, &root, &mission_id, LockForce::No)
        .expect("resume mission");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    drop(engine);

    // The resumed orchestrator session used --resume with the phase-1 id.
    let specs = backend2.started_specs();
    let orch_spec = specs
        .iter()
        .find(|s| matches!(s.prompt, PromptMode::Streaming(_)))
        .expect("phase 2 started a streaming orchestrator session");
    assert_eq!(
        orch_spec.resume.as_deref(),
        Some(phase1_orch_sdk_id.as_str())
    );

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
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        worker_pass(),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());

    // Planning phase drives orchestrator session #1.
    let reply = timeout(
        TEST_TIMEOUT,
        engine.planning_turn("plan two features please"),
    )
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
        PlanRequest::WrongPlan { reason } => {
            panic!("scripted plan JSON must parse, got a wrong-plan escalation: {reason}")
        }
    };
    assert_eq!(plan.milestones.len(), 1);
    engine.approve_plan(plan).unwrap();

    // Kill the live session and forget its id: the next orchestrator need
    // must take the fresh re-seed path. Behaviour must not visibly change.
    engine.force_reseed();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    // The second streaming session was seeded (initial prompt) with the
    // re-seed context: digest header + the approved plan.json.
    let specs = backend.started_specs();
    let streaming: Vec<_> = specs
        .iter()
        .filter(|s| matches!(s.prompt, PromptMode::Streaming(_)))
        .collect();
    assert_eq!(streaming.len(), 2, "exactly two orchestrator sessions");
    assert!(
        streaming[1].resume.is_none(),
        "re-seed is a fresh session, not a resume"
    );
    match &streaming[1].prompt {
        PromptMode::Streaming(seed) => {
            assert!(
                seed.starts_with("MISSION m-"),
                "digest header first: {seed}"
            );
            assert!(
                seed.contains("APPROVED PLAN (plan.json):"),
                "plan.json embedded: {seed}"
            );
            assert!(
                seed.contains("build part 1"),
                "plan content present: {seed}"
            );
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
        PlanRequest::WrongPlan { reason } => {
            panic!("prose must not parse as a wrong-plan escalation: {reason}")
        }
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
        PlanRequest::WrongPlan { reason } => {
            panic!("scripted plan JSON must parse, got a wrong-plan escalation: {reason}")
        }
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
    let backend = Arc::new(MockBackend::with_scripts(vec![MockScript::streaming(
        vec![
            mock_init("orch-session"),
            mock_text(seed_text),
            mock_result_text(seed_text),
        ],
    )
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
    assert_eq!(
        engine.take_seed_reply(),
        None,
        "the seed reply is taken exactly once"
    );

    let mission_id = engine.mission_id().to_string();
    drop(engine); // releases the lock; the sdk session id is on the log

    // --- Resume path: the resume-ack seed reply is captured too -----------
    let ack_text = "Acknowledged — resuming the planning conversation.";
    let backend2 = Arc::new(MockBackend::with_scripts(vec![MockScript::streaming(
        vec![
            mock_init("orch-session"),
            mock_text(ack_text),
            mock_result_text(ack_text),
        ],
    )
    .responding(vec![vec![
        mock_text("continuing"),
        mock_result_text("continuing"),
    ]])]));
    let backend2_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend2) as Arc<dyn AgentBackend>;
    let mut engine = MissionEngine::resume(backend2_dyn, &root, &mission_id, LockForce::No)
        .expect("resume mission");

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
    assert!(
        orch_spec.resume.is_some(),
        "resume-ack path expected (--resume set)"
    );
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
    let mut plan = simple_plan(
        1,
        vec![
            assertion("", "tests pass", Some("cargo test")),
            assertion("", "docs updated", None),
        ],
    );
    plan.milestones[0].title = "Milestone One".to_string();
    engine.approve_plan(plan).unwrap();

    // plan.json exists, parses, and carries the assigned assertion ids.
    let paths = engine.paths().clone();
    let plan_text = std::fs::read_to_string(paths.plan_file()).expect("plan.json written");
    let written: Plan = serde_json::from_str(&plan_text).expect("plan.json parses");
    let ids: Vec<&str> = written
        .validation_contract
        .iter()
        .map(|a| a.id.as_str())
        .collect();
    assert_eq!(ids, vec!["a-1", "a-2"]);

    // Mission branch created from main and checked out; the approval commit
    // contains exactly plan.json + its human-readable plan.md twin.
    let branch = raw_git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert_eq!(branch.trim(), format!("kranz/mission-{mission_id}"));
    let subject = raw_git(&root, &["log", "-1", "--format=%s"]);
    assert_eq!(
        subject.trim(),
        format!("[kranz] approved plan for {mission_id}")
    );
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
        root.join(".kranz")
            .join("missions")
            .join(&mission_id)
            .join("plan.md"),
    )
    .expect("plan.md written");
    assert!(
        md.starts_with(&format!("# Mission plan — {mission_id}")),
        "{md}"
    );
    // Cost estimate section: fresh repo, no completed missions, so the
    // provenance pins to the built-in-defaults wording and the figures come
    // from the same calibrate+estimate call the engine makes internally.
    let calibration = cost::calibrate(&root);
    assert_eq!(calibration.missions_used, 0);
    let expected_estimate = cost::estimate(&written, &test_cfg(), &calibration.params);
    assert!(md.contains("## Cost estimate"), "{md}");
    assert!(
        md.contains(&format!("${:.2}", expected_estimate.low_usd)),
        "{md}"
    );
    assert!(
        md.contains(&format!("${:.2}", expected_estimate.expected_usd)),
        "{md}"
    );
    assert!(
        md.contains(&format!("${:.2}", expected_estimate.high_usd)),
        "{md}"
    );
    assert!(
        md.contains("built-in defaults — no completed missions yet"),
        "{md}"
    );
    assert!(md.contains("## Validation contract"), "{md}");
    assert!(md.contains("**[a-1]**"), "{md}");
    assert!(md.contains("## Milestone 1 —"), "{md}");
    assert!(md.contains("Done when:"), "{md}");
    assert!(
        md.lines().all(|line| !line.ends_with([' ', '\t'])),
        "plan.md must not contain trailing whitespace:\n{md}"
    );
    assert!(
        md.ends_with('\n') && !md.ends_with("\n\n"),
        "plan.md must end with exactly one newline:\n{md}"
    );
    // main itself did not move: it still points at the seed commit.
    let main_subject = raw_git(&root, &["log", "-1", "--format=%s", "main"]);
    assert_eq!(main_subject.trim(), "seed");

    // Reducer state: plan.approved materialized milestones/features with the
    // documented ids, and the mission is Approved (run loop hasn't started).
    let state = engine.state();
    assert_eq!(state.mission.status, MissionStatus::Approved);
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
    let mut engine = MissionEngine::resume(backend_dyn, &root, &mission_id, LockForce::No).unwrap();
    let err = engine.approve_plan(simple_plan(1, vec![])).unwrap_err();
    assert!(err.to_string().contains("Planning"), "got: {err}");
}

/// finding a6 / feature f-1-2: `approve_plan` runs the contract lint against
/// the untouched base tree, surfaces the polarity-bug suspect distinctly from
/// the benign already-failing assertion in both plan.md and an
/// `orchestrator.decision`, and never blocks approval.
#[tokio::test(flavor = "multi_thread")]
async fn approval_lint_surfaces_suspects_in_plan_md_and_decision() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());

    // `exit 0` already passes on the untouched base — an author-bug suspect.
    // `exit 1` fails on the untouched base — the usual, benign case.
    let plan = simple_plan(
        1,
        vec![
            assertion("", "vacuous assertion", Some("exit 0")),
            assertion("", "not-yet-landed assertion", Some("exit 1")),
        ],
    );
    engine.approve_plan(plan).unwrap();

    let paths = engine.paths().clone();
    let md = std::fs::read_to_string(paths.plan_md_file()).expect("plan.md written");
    assert!(md.contains("## Contract lint"), "{md}");
    assert!(
        md.contains("author-bug suspects (already pass / no verdict on the untouched base)"),
        "{md}"
    );
    assert!(md.contains("[a-1] exit 0"), "{md}");
    assert!(
        md.contains("base-expected-to-fail (benign): [a-2] exit 1"),
        "{md}"
    );

    drop(engine);
    let events = read_log(&paths);
    let decision = events.iter().find_map(|e| match &e.kind {
        EventKind::OrchestratorDecision { summary, detail }
            if summary.contains("contract lint") =>
        {
            Some((summary.clone(), detail.clone()))
        }
        _ => None,
    });
    let (summary, detail) = decision.expect("contract lint orchestrator.decision emitted");
    assert!(summary.contains("1 author-bug suspect"), "{summary}");
    let detail = detail.expect("decision carries the full lint summary");
    assert!(detail.contains("[a-1] exit 0"), "{detail}");
    assert!(detail.contains("[a-2] exit 1"), "{detail}");
}

/// Approval-time assertion commands are agent-authored code. They run in a
/// detached disposable worktree at the pinned base SHA, never in the primary
/// checkout, even when sandbox enforcement is off. A command that overwrites
/// a tracked source file therefore cannot alter the operator's tree, and the
/// disposable registration/directory is gone before approval returns.
#[tokio::test(flavor = "multi_thread")]
async fn approval_lint_uses_disposable_base_and_preserves_primary_checkout() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());

    let plan = simple_plan(
        1,
        vec![assertion(
            "",
            "hostile approval assertion",
            Some(&write_line_cmd("tampered", "README.md")),
        )],
    );
    engine.approve_plan(plan).unwrap();

    let paths = engine.paths().clone();
    assert_eq!(
        std::fs::read_to_string(root.join("README.md")).unwrap(),
        "seed\n",
        "the approval command must not alter the primary checkout"
    );
    assert!(
        !paths
            .runs_dir()
            .join("approval-contract-lint-worktree")
            .exists(),
        "the disposable approval worktree is cleaned"
    );
    assert!(
        !raw_git(&root, &["worktree", "list", "--porcelain"])
            .contains("approval-contract-lint-worktree"),
        "the disposable worktree registration is pruned"
    );
    let md = std::fs::read_to_string(paths.plan_md_file()).expect("plan.md written");
    assert!(
        md.contains("author-bug suspects (already pass / no verdict on the untouched base)"),
        "{md}"
    );
    assert!(
        !md.contains("working tree with uncommitted changes"),
        "the pinned disposable tree is clean: {md}"
    );
}

/// A predictable mission-branch name is not an authority channel. If a
/// branch already contains commits before approval, those bytes were not
/// derived from the pinned approval base and must not be smuggled into the
/// mission deliverable.
#[tokio::test(flavor = "multi_thread")]
async fn approve_refuses_preexisting_mission_branch_commits() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    let branch = engine.state().mission.mission_branch.clone();

    raw_git(&root, &["checkout", "-b", &branch]);
    std::fs::write(root.join("planted.txt"), "not approved\n").unwrap();
    raw_git(&root, &["add", "planted.txt"]);
    raw_git(&root, &["commit", "-m", "planted mission commit"]);
    raw_git(&root, &["checkout", "main"]);

    let error = engine
        .approve_plan(simple_plan(1, vec![]))
        .expect_err("pre-existing mission bytes must fail closed");
    assert!(
        error
            .to_string()
            .contains("refusing to approve pre-existing commits"),
        "{error}"
    );
    assert!(
        !read_log(engine.paths())
            .iter()
            .any(|event| matches!(event.kind, EventKind::PlanApproved { .. })),
        "the refusal emits no approval authority"
    );
}

/// finding a3 / feature f-1-2: approve_plan never returns Err because of
/// lint outcomes, even when every command assertion in the contract already
/// passes on the untouched base (the maximal-suspect case) — the lint is
/// advisory only and must never block approval.
#[tokio::test(flavor = "multi_thread")]
async fn approval_lint_never_blocks() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());

    // Both assertions already pass on the untouched base: every command is
    // an author-bug suspect, yet approval must still succeed.
    let plan = simple_plan(
        1,
        vec![
            assertion("", "vacuous assertion one", Some("exit 0")),
            // `cd .` not `test 1 -eq 1`: `test` is a POSIX binary cmd.exe
            // cannot run, so on Windows the second assertion would fail to
            // execute and this test's premise (BOTH already pass on the
            // untouched base) would be silently false.
            assertion("", "vacuous assertion two", Some("cd .")),
        ],
    );
    engine.approve_plan(plan).unwrap();
    assert_eq!(engine.state().mission.status, MissionStatus::Approved);
}

/// finding a3 / feature f-1-2: the contract lint runs its command
/// assertions through a scoped OS-thread bridge, so driving `approve_plan`
/// from inside a live tokio runtime must not panic with "Cannot start a
/// runtime from within a runtime" and must return Ok.
#[tokio::test(flavor = "multi_thread")]
async fn approval_lint_no_nested_runtime_panic() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());

    let plan = simple_plan(
        1,
        vec![
            assertion("", "passes on base", Some("exit 0")),
            assertion("", "fails on base", Some("exit 1")),
        ],
    );
    // No panic (and no Err) proves the gate runtime lived on the bridge
    // thread rather than being nested on this Tokio worker.
    engine.approve_plan(plan).unwrap();
    assert_eq!(engine.state().mission.status, MissionStatus::Approved);
}

/// ticket contract-validation-gates: the named, deterministic contract gates
/// run at approval through the gate plugin interface. A negated grep whose
/// target is absent from the pristine base trips BOTH wrong-polarity
/// (static: passes because the target is absent) and passes-on-base
/// (graduated lint: exits zero on the untouched base), and the defect-class
/// names reach plan.md and the approval orchestrator.decision — while
/// approval itself still succeeds (advisory posture unchanged).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn contract_gate_named_verdicts_reach_plan_md_and_decision() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());

    // `! grep -q marker <missing>`: grep errors on the absent path and the
    // negation turns that into success — the assertion passes BECAUSE the
    // target is absent, and it already exits zero on the untouched base.
    let plan = simple_plan(
        1,
        vec![
            assertion(
                "",
                "marker absent",
                Some("! grep -q landed-marker kranz-no-such-file.txt"),
            ),
            assertion("", "not-yet-landed assertion", Some("exit 1")),
        ],
    );
    engine.approve_plan(plan).unwrap();

    let paths = engine.paths().clone();
    let md = std::fs::read_to_string(paths.plan_md_file()).expect("plan.md written");
    assert!(md.contains("named contract gates"), "{md}");
    assert!(md.contains("wrong-polarity: FAIL"), "{md}");
    assert!(md.contains("passes-on-base: FAIL"), "{md}");
    assert!(md.contains("vacuous-filter: PASS"), "{md}");
    assert!(md.contains("env-sensitive: PASS"), "{md}");

    drop(engine);
    let events = read_log(&paths);
    let decision = events.iter().find_map(|e| match &e.kind {
        EventKind::OrchestratorDecision { summary, detail }
            if summary.contains("contract lint") =>
        {
            Some((summary.clone(), detail.clone()))
        }
        _ => None,
    });
    let (summary, detail) = decision.expect("contract lint orchestrator.decision emitted");
    // The pre-existing suspect headline is preserved; the failed classes
    // are appended by name (contract_health still parses the prefix).
    assert!(summary.contains("1 author-bug suspect"), "{summary}");
    assert!(
        summary.contains("named contract gate(s) failed: wrong-polarity, passes-on-base"),
        "{summary}"
    );
    let detail = detail.expect("decision carries the lint summary and gate verdicts");
    assert!(detail.contains("wrong-polarity: FAIL"), "{detail}");
    assert!(detail.contains("passes-on-base: FAIL"), "{detail}");
    assert!(detail.contains("[a-1]"), "{detail}");
}

/// ticket contract-validation-gates: at the final gate the static named
/// gates re-check the contract against the ACTIVE tree — a negated grep
/// whose target is STILL absent there passed vacuously, so an advisory
/// decision names the class. The mission still completes: the gate records
/// the named verdict, it does not change what passes (posture unchanged).
///
/// unix-only fixture: the vacuous-green shape needs shell negation
/// (`! grep -q …`), which the final gate's `cmd /C` on Windows cannot
/// parse (the approve-time lint always runs `sh`, the final gate runs the
/// platform shell — a pre-existing divergence this fixture would trip,
/// not a behavior of the gates under test). The static gate logic itself
/// is platform-neutral and covered by the contract_gates unit tests on
/// every platform; this test only proves the decision-event plumbing.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn contract_gate_final_gate_decision_names_vacuous_green() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let contract = vec![assertion(
        "",
        "marker absent",
        Some("! grep -q landed-marker kranz-no-such-file.txt"),
    )];
    // One worker (passing report), one orchestrator turn (checkpoint +
    // feature-completion judgement + lesson extraction); both validators
    // are off in test_cfg, so the milestone tags clean and the final gate
    // runs the contract.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    let decision = events.iter().find_map(|e| match &e.kind {
        EventKind::OrchestratorDecision { summary, detail }
            if summary.contains("contract gates (final gate)") =>
        {
            Some((summary.clone(), detail.clone()))
        }
        _ => None,
    });
    let (summary, detail) = decision.expect("final-gate contract-gate decision emitted");
    assert!(summary.contains("wrong-polarity"), "{summary}");
    let detail = detail.expect("decision carries the gate verdicts");
    assert!(detail.contains("wrong-polarity: FAIL"), "{detail}");
    assert!(detail.contains("[a-1]"), "{detail}");
}

/// ticket gate-results-first-class-events (KRZ-312): approving a plan
/// records every approval-gate evaluation as a first-class gate.result
/// event — one per gate, in pipeline order (the four-gate floor in ticket
/// order), each carrying its ladder position (surface + section index), the
/// stated verdict, and the gate-local artefact handle. The events land
/// AFTER plan.approved (the "Git first" invariant: no event until approval
/// cannot fail) and before the advisory lint decision.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn gate_result_events_record_the_approval_ladder() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());

    // Same contract shape as contract_gate_named_verdicts_…: a vacuous-green
    // negated grep (trips wrong-polarity + passes-on-base) plus a benign
    // not-yet-landed assertion.
    let plan = simple_plan(
        1,
        vec![
            assertion(
                "",
                "marker absent",
                Some("! grep -q landed-marker kranz-no-such-file.txt"),
            ),
            assertion("", "not-yet-landed assertion", Some("exit 1")),
        ],
    );
    engine.approve_plan(plan).unwrap();

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);

    let ladder: Vec<(String, u32, String, String)> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::GateResult {
                gate,
                surface,
                kind,
                index,
                verdict,
                artefact_ref,
                ..
            } => {
                assert_eq!(
                    *surface,
                    kranz_engine::gate::GateSurface::Approval,
                    "approval gates carry the approval surface"
                );
                assert_eq!(*kind, kranz_engine::gate::GateKind::Deterministic);
                Some((
                    gate.clone(),
                    *index,
                    serde_json::to_value(verdict)
                        .unwrap()
                        .as_str()
                        .unwrap()
                        .to_string(),
                    artefact_ref.clone(),
                ))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        ladder,
        vec![
            (
                "vacuous-filter".to_string(),
                0,
                "pass".to_string(),
                "contract gate vacuous-filter".to_string()
            ),
            (
                "wrong-polarity".to_string(),
                1,
                "fail".to_string(),
                "contract gate wrong-polarity".to_string()
            ),
            (
                "passes-on-base".to_string(),
                2,
                "fail".to_string(),
                "contract gate passes-on-base".to_string()
            ),
            (
                "env-sensitive".to_string(),
                3,
                "pass".to_string(),
                "contract gate env-sensitive".to_string()
            ),
        ],
        "one gate.result per approval gate, in pipeline order"
    );

    // The failing gates' findings travel in the event payload (verbatim
    // gate-local detail), so the log alone carries the evidence.
    let wrong_polarity = events.iter().find_map(|e| match &e.kind {
        EventKind::GateResult {
            gate,
            artefact_detail,
            ..
        } if gate == "wrong-polarity" => artefact_detail.clone(),
        _ => None,
    });
    assert!(
        wrong_polarity
            .as_deref()
            .unwrap_or_default()
            .contains("negated grep targets missing path"),
        "{wrong_polarity:?}"
    );

    // Ordering: plan.approved < gate.result ladder < the advisory lint
    // decision (a retried approval can never double-record a ladder).
    let seq_of = |pred: &dyn Fn(&Event) -> bool| {
        events
            .iter()
            .find(|e| pred(e))
            .map(|e| e.seq)
            .expect("event present")
    };
    let approved_seq = seq_of(&|e| matches!(e.kind, EventKind::PlanApproved { .. }));
    let first_gate_seq = seq_of(&|e| matches!(e.kind, EventKind::GateResult { .. }));
    let decision_seq = seq_of(
        &|e| matches!(&e.kind, EventKind::OrchestratorDecision { summary, .. } if summary.contains("contract lint")),
    );
    assert!(
        approved_seq < first_gate_seq,
        "{approved_seq} < {first_gate_seq}"
    );
    assert!(
        first_gate_seq < decision_seq,
        "{first_gate_seq} < {decision_seq}"
    );
}

/// ticket gate-results-first-class-events (KRZ-312): the final gate records
/// its whole ladder as gate.result events too — the static floor re-checked
/// against the active tree (passes-on-base absent by design), one event per
/// gate, pass AND fail, with the final-gate surface. The mission still
/// completes: the events are records, not gates.
///
/// unix-only for the same reason as
/// contract_gate_final_gate_decision_names_vacuous_green (the
/// vacuous-green shape needs shell negation).
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn gate_result_events_record_the_final_gate_ladder() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let contract = vec![assertion(
        "",
        "marker absent",
        Some("! grep -q landed-marker kranz-no-such-file.txt"),
    )];
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);

    let ladder: Vec<(String, u32, String)> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::GateResult {
                gate,
                surface,
                index,
                verdict,
                ..
            } if *surface == kranz_engine::gate::GateSurface::FinalGate => Some((
                gate.clone(),
                *index,
                serde_json::to_value(verdict)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_string(),
            )),
            _ => None,
        })
        .collect();
    assert_eq!(
        ladder,
        vec![
            ("vacuous-filter".to_string(), 0, "pass".to_string()),
            ("wrong-polarity".to_string(), 1, "fail".to_string()),
            ("env-sensitive".to_string(), 2, "pass".to_string()),
        ],
        "the final-gate floor, in pipeline order, passes and failures alike"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn approve_plan_requires_considered_alternatives_for_large_scope() {
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());
    let cfg = MissionConfig {
        considered_alternatives_feature_threshold: 2,
        considered_alternatives_touch_set_threshold: 0,
        considered_alternatives_high_usd_threshold: 0.0,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);

    let err = engine.approve_plan(simple_plan(2, vec![])).unwrap_err();
    assert!(
        err.to_string().contains("considered alternatives required"),
        "large-scope refusal names the missing review material: {err}"
    );
    assert_eq!(engine.state().mission.status, MissionStatus::Planning);
}

#[tokio::test(flavor = "multi_thread")]
async fn approve_plan_persists_considered_alternatives_when_required() {
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());
    let cfg = MissionConfig {
        considered_alternatives_feature_threshold: 2,
        considered_alternatives_touch_set_threshold: 0,
        considered_alternatives_high_usd_threshold: 0.0,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    let mission_id = engine.mission_id().to_string();
    let mut plan = simple_plan(2, vec![]);
    plan.considered_alternatives = Some(considered_alternatives());

    engine.approve_plan(plan).unwrap();

    let plan_text = std::fs::read_to_string(engine.paths().plan_file()).unwrap();
    let written: Plan = serde_json::from_str(&plan_text).unwrap();
    assert!(
        written.considered_alternatives.is_some(),
        "plan.json carries the review section"
    );
    let md = std::fs::read_to_string(
        root.join(".kranz")
            .join("missions")
            .join(&mission_id)
            .join("plan.md"),
    )
    .unwrap();
    assert!(md.contains("## Considered alternatives"), "{md}");
    assert!(md.contains("big-bang rewrite"), "{md}");
}

/// Once a prior mission has COMPLETED in the repo, a later `approve_plan`'s
/// plan.md cites calibrated params (not the built-in defaults) and the
/// "based on N completed mission(s)" provenance wording.
#[tokio::test(flavor = "multi_thread")]
async fn plan_md_cost_estimate_uses_calibration_once_a_mission_completes() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Run one mission to completion so `cost::calibrate` has actuals to
    // average (single feature, empty contract: worker report -> judgement
    // -> capture-lesson turn, per the happy-path helpers above).
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));
    let mut first = make_engine(&backend, &root, test_cfg());
    first.approve_plan(simple_plan(1, vec![])).unwrap();
    let status = timeout(TEST_TIMEOUT, first.run())
        .await
        .expect("first mission must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    drop(first);
    raw_git(&root, &["checkout", "main"]);

    // A brand-new mission's plan approval must now see missions_used == 1.
    let backend2 = Arc::new(MockBackend::new());
    let mut second = make_engine(&backend2, &root, test_cfg());
    let plan = simple_plan(1, vec![]);
    let calibration = cost::calibrate(&root);
    assert_eq!(
        calibration.missions_used, 1,
        "the completed first mission must calibrate the second's estimate"
    );
    let expected_estimate = cost::estimate(&plan, &test_cfg(), &calibration.params);
    second.approve_plan(plan).unwrap();

    let mission_id = second.mission_id().to_string();
    let md = std::fs::read_to_string(
        root.join(".kranz")
            .join("missions")
            .join(&mission_id)
            .join("plan.md"),
    )
    .expect("plan.md written");
    assert!(md.contains("based on 1 completed mission(s)"), "{md}");
    assert!(
        !md.contains("built-in defaults — no completed missions yet"),
        "{md}"
    );
    assert!(
        md.contains(&format!("${:.2}", expected_estimate.expected_usd)),
        "{md}"
    );
}

/// approve_plan resolves the base branch's tip and records it as
/// `state.mission.base_sha` (and the emitted `plan.approved` event's
/// `baseSha`) — pinned at approval time, not re-resolved later.
#[test]
fn approval_records_base_branch_sha() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    // Base branch tip BEFORE approval, recorded independently of the code
    // under test via a raw git rev-parse.
    let base_tip_before = raw_git(&root, &["rev-parse", "main"]).trim().to_string();

    let backend = Arc::new(MockBackend::new());
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let state = engine.state();
    assert_eq!(
        state.mission.base_sha.as_deref(),
        Some(base_tip_before.as_str()),
        "folded state.mission.base_sha must equal the base branch tip recorded before approval"
    );

    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let approved = events
        .iter()
        .find(|e| e.kind.type_name() == "plan.approved")
        .expect("plan.approved event must be on the log");
    match &approved.kind {
        EventKind::PlanApproved { base_sha, .. } => {
            assert_eq!(base_sha.as_deref(), Some(base_tip_before.as_str()));
        }
        other => panic!("expected PlanApproved, got: {other:?}"),
    }

    // Base branch itself never moved — approve_plan commits onto the mission
    // branch only, so the recorded sha is still the base tip.
    let base_tip_after = raw_git(&root, &["rev-parse", "main"]).trim().to_string();
    assert_eq!(base_tip_after, base_tip_before, "base branch must not move");
}

/// The base sha recorded at plan approval reaches both the worker session's
/// and the validator session's env as `KRANZ_BASE_SHA`, so contracts never
/// diff against the moving base branch.
#[tokio::test(flavor = "multi_thread")]
async fn base_sha_reaches_worker_and_validator_env() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let base_tip_before = raw_git(&root, &["rev-parse", "main"]).trim().to_string();

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            verdicts_pass(&["a-2"]),
            no_lesson(),
        ]),
        validator_with(json!([])),
    ]));

    let contract = vec![assertion("a-2", "error messages are actionable", None)];
    let cfg = MissionConfig {
        skip_scrutiny: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, contract)).unwrap();
    assert_eq!(
        engine.state().mission.base_sha.as_deref(),
        Some(base_tip_before.as_str())
    );

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let specs = backend.started_specs();
    let worker_spec = specs
        .iter()
        .find(|s| matches!(s.prompt, PromptMode::SingleShot(ref t) if t.contains("Implement feature")))
        .expect("a worker spec was started");
    assert_eq!(
        worker_spec.env.get("KRANZ_BASE_SHA").map(String::as_str),
        Some(base_tip_before.as_str()),
        "worker env carries the recorded base sha"
    );

    let validator_spec = specs
        .iter()
        .find(|s| matches!(s.prompt, PromptMode::SingleShot(ref t) if t.contains("Validate milestone")))
        .expect("a validator spec was started");
    assert_eq!(
        validator_spec.env.get("KRANZ_BASE_SHA").map(String::as_str),
        Some(base_tip_before.as_str()),
        "validator env carries the recorded base sha"
    );
}

/// A mock worker that writes a file into its session cwd leaves a dirty tree
/// behind, which the engine's §4.4 discipline checkpoints as a real,
/// non-meta commit on the mission branch — proving mock missions can deliver
/// actual commits (not just an empty diff).
#[tokio::test(flavor = "multi_thread")]
async fn worker_file_write_lands_a_non_meta_commit_and_mission_completes() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let base_tip_before = raw_git(&root, &["rev-parse", "main"]).trim().to_string();

    let worker_writes = MockScript::single_shot_json(&json!({
        "result": "pass",
        "summary": "implemented and tested",
        "filesTouched": ["feature.txt"],
        "testsAdded": [],
        "testEvidence": "all green",
        "commits": []
    }))
    .writes_file("feature.txt", "delivered by the mock worker\n");

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_writes,
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            verdicts_pass(&["a-2"]),
            no_lesson(),
        ]),
        validator_with(json!([])),
    ]));

    let contract = vec![assertion("a-2", "error messages are actionable", None)];
    let cfg = MissionConfig {
        skip_scrutiny: false,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let mission_branch = engine.state().mission.mission_branch.clone();
    let log = raw_git(
        &root,
        &[
            "log",
            &format!("{base_tip_before}..{mission_branch}"),
            "--format=%s",
        ],
    );
    let subjects: Vec<&str> = log.lines().collect();
    assert!(
        subjects
            .iter()
            .any(|s| !kranz_engine::contract_sweep::is_meta_commit(s)),
        "expected at least one non-meta commit on the mission branch, got: {subjects:?}"
    );
    assert!(
        subjects
            .iter()
            .any(|s| s.contains("feature.txt") || s.contains("checkpoint") || s.contains("f-1-1")),
        "the worker's dirty tree should have produced the feature checkpoint commit: {subjects:?}"
    );
    let feature_file = std::fs::read_to_string(root.join("feature.txt"))
        .expect("the mock worker's write should survive on the mission branch");
    assert_eq!(feature_file, "delivered by the mock worker\n");
}

/// The missions catalog upserts by mission id: appends new entries newest
/// last, replaces on re-approval, never duplicates.
#[test]
fn mission_index_upserts_by_id() {
    use kranz_engine::planning::upsert_mission_index;
    let d1 = chrono::NaiveDate::from_ymd_opt(2026, 7, 2).unwrap();
    let d2 = chrono::NaiveDate::from_ymd_opt(2026, 7, 3).unwrap();

    let one = upsert_mission_index("", "m-aaa", "first goal", d1);
    assert!(one.starts_with("# Kranz missions"), "{one}");
    assert!(
        one.contains("- 2026-07-02 · [m-aaa](m-aaa/plan.md) — first goal"),
        "{one}"
    );

    let two = upsert_mission_index(&one, "m-bbb", "second goal\nwith newline", d2);
    assert!(two.contains("[m-aaa]("), "{two}");
    assert!(
        two.contains("- 2026-07-03 · [m-bbb](m-bbb/plan.md) — second goal with newline"),
        "{two}"
    );
    assert!(
        two.find("[m-aaa](").unwrap() < two.find("[m-bbb](").unwrap(),
        "newest last"
    );

    let re = upsert_mission_index(&two, "m-aaa", "first goal, re-planned", d2);
    assert_eq!(
        re.matches("[m-aaa](").count(),
        1,
        "no duplicate on re-approval: {re}"
    );
    assert!(re.contains("first goal, re-planned"), "{re}");
}

/// The completion-report link appends to exactly the named mission's line,
/// idempotently, without disturbing the line format.
#[test]
fn mission_index_report_link_appends_once() {
    use kranz_engine::mission_catalog::mark_mission_index_report;
    use kranz_engine::planning::upsert_mission_index;
    let d = chrono::NaiveDate::from_ymd_opt(2026, 7, 3).unwrap();
    let index = upsert_mission_index("", "m-aaa", "goal", d);
    let index = upsert_mission_index(&index, "m-bbb", "other goal", d);

    let marked = mark_mission_index_report(&index, "m-aaa");
    assert!(
        marked.contains("- 2026-07-03 · [m-aaa](m-aaa/plan.md) — goal · [report](m-aaa/report.md)"),
        "{marked}"
    );
    assert!(
        !marked.contains("[report](m-bbb/report.md)"),
        "only the named mission: {marked}"
    );

    let again = mark_mission_index_report(&marked, "m-aaa");
    assert_eq!(again, marked, "idempotent");
    let unknown = mark_mission_index_report(&marked, "m-zzz");
    assert_eq!(unknown, marked, "unknown id leaves the index unchanged");
}

/// Pruning a mission's line removes exactly that line, keeps the header and
/// every other line byte-for-byte, and is a no-op for an id with no line.
#[test]
fn mission_index_prune_removes_only_named_line() {
    use kranz_engine::mission_catalog::prune_mission_index;
    use kranz_engine::planning::upsert_mission_index;
    let d = chrono::NaiveDate::from_ymd_opt(2026, 7, 3).unwrap();
    let index = upsert_mission_index("", "m-aaa", "first goal", d);
    let index = upsert_mission_index(&index, "m-bbb", "second goal", d);

    let pruned = prune_mission_index(&index, "m-aaa");
    assert!(!pruned.contains("[m-aaa]("), "{pruned}");
    assert!(pruned.contains("[m-bbb]("), "{pruned}");
    assert!(pruned.starts_with("# Kranz missions"), "{pruned}");
    assert!(
        pruned.contains("- 2026-07-03 · [m-bbb](m-bbb/plan.md) — second goal"),
        "{pruned}"
    );

    let noop = prune_mission_index(&pruned, "m-zzz");
    assert_eq!(noop, pruned, "unknown id leaves the index unchanged");

    let empty = prune_mission_index("", "m-aaa");
    assert_eq!(empty, "", "empty input returns unchanged");
}

/// `mission_index_ids` returns exactly the catalog's ids, in file order,
/// taking the plan.md bracket (not the trailing `[report]` link) as the id.
#[test]
fn mission_index_ids_lists_ids_in_order() {
    use kranz_engine::mission_catalog::{mark_mission_index_report, mission_index_ids};
    use kranz_engine::planning::upsert_mission_index;
    let d = chrono::NaiveDate::from_ymd_opt(2026, 7, 3).unwrap();
    let index = upsert_mission_index("", "m-aaa", "first goal", d);
    let index = upsert_mission_index(&index, "m-bbb", "second goal", d);
    let index = mark_mission_index_report(&index, "m-aaa");

    assert_eq!(
        mission_index_ids(&index),
        vec!["m-aaa".to_string(), "m-bbb".to_string()]
    );
}

/// Deleting a mission prunes its line from the on-disk catalog, leaving the
/// other mission's line intact.
#[test]
fn delete_prunes_missions_index() {
    use kranz_engine::mission_catalog::prune_mission_index_file;
    use kranz_engine::planning::upsert_mission_index;
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize repo root");
    let d = chrono::NaiveDate::from_ymd_opt(2026, 7, 3).unwrap();

    let index_dir = root.join(".kranz").join("missions");
    std::fs::create_dir_all(&index_dir).unwrap();
    let index_path = index_dir.join("index.md");
    let index = upsert_mission_index("", "m-aaa", "first goal", d);
    let index = upsert_mission_index(&index, "m-bbb", "second goal", d);
    std::fs::write(&index_path, &index).unwrap();

    prune_mission_index_file(&root, "m-aaa");

    let after = std::fs::read_to_string(&index_path).unwrap();
    assert!(!after.contains("[m-aaa]("), "{after}");
    assert!(after.contains("[m-bbb]("), "{after}");
}

/// Pruning against a repo with no index file at all is a no-op: it must not
/// create one.
#[test]
fn delete_prunes_missions_index_missing_file_is_noop() {
    use kranz_engine::mission_catalog::prune_mission_index_file;
    let dir = tempfile::tempdir().expect("create tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize repo root");

    prune_mission_index_file(&root, "m-aaa");

    assert!(!root
        .join(".kranz")
        .join("missions")
        .join("index.md")
        .exists());
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
    assert_eq!(
        reducer::fold(&before).unwrap().mission.status,
        MissionStatus::Planning
    );

    kranz_engine::mission_catalog::abandon_mission(
        &root,
        &mission_id,
        "no longer needed",
        LockForce::No,
    )
    .expect("abandon a planning mission");

    // Exactly one new event, of the right kind, carrying the reason.
    let after = read_log(&paths);
    assert_eq!(after.len(), before.len() + 1, "one event appended");
    assert_eq!(after.first().unwrap().seq, 1);
    assert_eq!(
        after.last().unwrap().seq,
        after.len() as u64,
        "contiguous seq"
    );
    assert!(matches!(
        &after.last().unwrap().kind,
        EventKind::MissionAbandoned { reason } if reason == "no longer needed"
    ));

    // The reducer folds to Abandoned, and the on-disk snapshot matches.
    assert_eq!(
        reducer::fold(&after).unwrap().mission.status,
        MissionStatus::Abandoned
    );
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

    kranz_engine::mission_catalog::abandon_mission(&root, &mission_id, "stop", LockForce::No)
        .expect("abandon");

    let backend2: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
    let mut resumed =
        MissionEngine::resume(backend2, &root, &mission_id, LockForce::No).expect("resume");
    let err = resumed
        .run()
        .await
        .expect_err("running a terminal mission must be rejected");
    assert!(
        matches!(err, kranz_engine::error::EngineError::InvalidState(_)),
        "expected InvalidState, got {err:?}"
    );
    // No worker.spawned appended by the rejected run.
    let events = read_log(&paths);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e.kind, EventKind::WorkerSpawned { .. })),
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
    kranz_engine::mission_catalog::abandon_mission(&root, &mission_id, "first", LockForce::No)
        .unwrap();
    let after_first = read_log(&paths);

    // A second abandon is rejected: the mission is already terminal.
    let err =
        kranz_engine::mission_catalog::abandon_mission(&root, &mission_id, "again", LockForce::No)
            .expect_err("abandoning a terminal mission must error");
    assert!(
        err.to_string().contains("already terminal"),
        "error should name the terminal state: {err}"
    );

    // The rejected call appended nothing.
    let after_second = read_log(&paths);
    assert_eq!(
        after_second.len(),
        after_first.len(),
        "no event appended on the rejected abandon"
    );
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

    let err =
        kranz_engine::mission_catalog::abandon_mission(&root, &mission_id, "x", LockForce::No)
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
    assert_eq!(
        warn.severity, "warn",
        "a missing program is a warning, not an error"
    );
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
        assertion("a-1", "trivially true", Some("exit 0")),
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
/// program. Preflight never blocks; the final gate still refuses a waive of
/// the failing command assertion (non-waivable, fixable).
#[tokio::test(flavor = "multi_thread")]
async fn run_emits_preflight_decision_when_issues_exist() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let contract = vec![assertion(
        "a-1",
        "the check passes",
        Some("definitely-not-a-real-program-xyz --check"),
    )];

    // Cap=1: refused waive → fix feature → second refuse at cap → Blocked.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            waive_reply("a-1", "command program unavailable in this environment"),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            waive_reply("a-1", "still unavailable"),
        ]),
        worker_pass(),
    ]));

    let cfg = MissionConfig {
        max_fix_cycles_per_milestone: 1,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Blocked);

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);

    // Exactly one preflight decision, and it precedes the first worker spawn.
    let preflight_seqs: Vec<u64> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::OrchestratorDecision { summary, .. }
                if summary.starts_with("preflight:") =>
            {
                Some(e.seq)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        preflight_seqs.len(),
        1,
        "exactly one preflight decision: {preflight_seqs:?}"
    );
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

/// The sandbox preflight probe (f-2-3) is inert when the worker role has
/// `sandbox.enforce == off`: zero sandbox-related issues, regardless of
/// platform.
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_preflight_inert_when_enforce_off() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());

    let contract = vec![assertion(
        "a-1",
        "writes outside the allowlist",
        Some("sh -c 'echo x > $HOME/kranz_pf_should_not_run'"),
    )];
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let issues = engine.preflight();
    assert!(
        !issues
            .iter()
            .any(|i| i.message.contains("fs sandbox profile")),
        "enforce:off must add zero sandbox preflight issues: {issues:?}"
    );
}

/// Linux has a real bwrap enforcement tier, but the macOS profile preflight
/// probe remains inapplicable there and emits no Seatbelt-specific issue.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_preflight_emits_no_macos_profile_issue_on_linux() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());

    let mut cfg = test_cfg();
    cfg.worker.sandbox.enforce = kranz_engine::types::SandboxEnforce::Fs;

    let contract = vec![assertion(
        "a-1",
        "writes outside the allowlist",
        Some("sh -c 'echo x > $HOME/kranz_pf_should_not_run'"),
    )];
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let issues = engine.preflight();
    assert!(
        !issues
            .iter()
            .any(|i| i.message.contains("fs sandbox profile")),
        "Linux must add no macOS-profile preflight issue: {issues:?}"
    );
}

/// Windows now resolves the production AppContainer process tier. This
/// integration-level consumer guards the public platform decision without
/// trying to re-enter the test-harness executable as a production helper.
#[cfg(windows)]
#[test]
fn sandbox_preflight_windows_process_backend_is_appcontainer() {
    assert_eq!(
        kranz_engine::sandbox::platform_support(kranz_engine::types::SandboxEnforce::Fs, "windows"),
        kranz_engine::sandbox::SandboxDecision::Enforce(
            kranz_engine::sandbox::SandboxBackend::AppContainer
        )
    );
}

/// On macOS with the worker role opted into `enforce: fs`, a contract command
/// that writes outside the generated allowlist (session cwd / mission dir /
/// tmpdir / extraWrite) fails under the sandbox and surfaces as a `warn`
/// PreflightIssue naming that assertion id; a benign command yields no
/// sandbox issue; and the probe never returns an `error` nor blocks the run.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread")]
async fn sandbox_preflight_flags_command_that_writes_outside_allowlist() {
    if !setup() {
        return;
    }
    // Apply-smoke, not just a PATH check: the preflight wraps its probes in
    // a generated profile, and under the gate sandbox wrap (a wrapped
    // `cargo test` dogfooding this repo — ticket
    // gate-sandbox-supervision-dogfood) a nested apply of any profile but
    // the identical one is kernel-denied (probed 2026-08-05; no SBPL clause
    // can allow it). Skip with a detectable marker rather than fail on the
    // outer sandbox's presence.
    if std::process::Command::new("which")
        .arg("sandbox-exec")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        kranz_engine::test_capability::skip(
            kranz_engine::test_capability::capability::SANDBOX_EXEC,
            "sandbox-exec not found on this host",
        );
        return;
    }
    match std::process::Command::new("sandbox-exec")
        .arg("-p")
        .arg("(version 1)\n(allow default)\n")
        .arg("/usr/bin/true")
        .output()
    {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            eprintln!(
                "SKIP-UNDER-WRAP (gate-sandbox-supervision-dogfood): \
                 sandbox-exec cannot apply a smoke profile here (nested apply is denied \
                 inside the gate sandbox wrap); skipping: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        Err(e) => {
            eprintln!("sandbox-exec smoke probe failed; skipping: {e}");
            return;
        }
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::new());

    let mut cfg = test_cfg();
    cfg.worker.sandbox.enforce = kranz_engine::types::SandboxEnforce::Fs;

    let marker = format!("kranz_pf_{}", uuid::Uuid::new_v4());
    // The REAL home is outside the generated allowlist (session cwd /
    // mission dir / tmpdir): bake it in literally — the probe's contract env
    // deliberately redefines $HOME to the writable mission scratch.
    let real_home = std::env::var("HOME").expect("HOME must be set for this test");
    let contract = vec![
        assertion(
            "a-outside",
            "writes outside the allowlist",
            Some(&format!("echo x > '{real_home}/{marker}'")),
        ),
        assertion("a-benign", "trivially true", Some("exit 0")),
    ];
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, contract)).unwrap();

    let issues = engine.preflight();

    assert!(
        !issues.iter().any(|i| i.severity == "error"),
        "sandbox preflight must never escalate to error: {issues:?}"
    );

    let outside_issue = issues
        .iter()
        .find(|i| i.message.contains("[a-outside]") && i.message.contains("fs sandbox profile"));
    assert!(
        outside_issue.is_some(),
        "expected a sandbox warn for the out-of-allowlist command: {issues:?}"
    );
    assert_eq!(outside_issue.unwrap().severity, "warn");

    assert!(
        !issues
            .iter()
            .any(|i| i.message.contains("[a-benign]") && i.message.contains("fs sandbox profile")),
        "benign command must not produce a sandbox issue: {issues:?}"
    );

    // Clean up in case the sandbox somehow did not block the write.
    if let Ok(home) = std::env::var("HOME") {
        let _ = std::fs::remove_file(std::path::Path::new(&home).join(&marker));
    }
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
    let backend = Arc::new(MockBackend::with_scripts(vec![orch_script(vec![
        revised_json,
    ])]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    // Approve a 3-feature plan: mission goes Approved, milestone ms-1 Pending
    // with features f-1-1, f-1-2, f-1-3 (all pending, none started).
    engine.approve_plan(simple_plan(3, vec![])).unwrap();
    assert_eq!(engine.state().mission.status, MissionStatus::Approved);

    // Propose the revision.
    let request = timeout(TEST_TIMEOUT, engine.request_revised_plan())
        .await
        .expect("request_revised_plan must not hang")
        .expect("scripted plan JSON is not a backend error");
    let plan = match request {
        PlanRequest::Ready(plan) => plan,
        PlanRequest::NotReady(text) => panic!("scripted revised plan must parse: {text}"),
        PlanRequest::WrongPlan { reason } => {
            panic!("scripted revised plan must parse, got a wrong-plan escalation: {reason}")
        }
    };
    assert_eq!(plan.milestones[0].features.len(), 2);

    // Apply it.
    engine
        .approve_revised_plan(plan)
        .expect("apply the revised plan");

    // f-1-2 and f-1-3 are dropped (skipped); f-1-1 untouched; one fix-origin
    // feature added to ms-1 with the re-plan id shape.
    let ms = &engine.state().mission.milestones[0];
    let by_id = |id: &str| ms.features.iter().find(|f| f.id == id).cloned();
    assert_eq!(
        by_id("f-1-1").unwrap().status,
        FeatureStatus::Pending,
        "kept feature untouched"
    );
    assert_eq!(
        by_id("f-1-2").unwrap().status,
        FeatureStatus::Skipped,
        "dropped feature skipped"
    );
    assert_eq!(
        by_id("f-1-3").unwrap().status,
        FeatureStatus::Skipped,
        "dropped feature skipped"
    );
    // Re-plan ids carry a cycle discriminator (`-replan-<cycle>-<n>`) so a
    // second re-plan of the same milestone can't collide.
    let added = by_id("ms-1-replan-1-1").expect("added feature exists with re-plan id");
    assert_eq!(
        added.origin,
        FeatureOrigin::Fix,
        "added feature is fix-origin"
    );
    assert_eq!(added.status, FeatureStatus::Pending);
    assert_eq!(added.title, "extra feature");

    // revised-plan.md was written + committed on the mission branch.
    let mission_id = engine.mission_id().to_string();
    let md = std::fs::read_to_string(
        root.join(".kranz")
            .join("missions")
            .join(&mission_id)
            .join("revised-plan.md"),
    )
    .expect("revised-plan.md written");
    assert!(
        md.starts_with(&format!("# Revised mission plan — {mission_id}")),
        "{md}"
    );
    assert!(md.contains("## Re-plan changes applied"), "{md}");
    assert!(md.contains("extra feature"), "{md}");
    let subject = raw_git(&root, &["log", "-1", "--format=%s"]);
    assert_eq!(
        subject.trim(),
        format!("[kranz] revised plan for {mission_id}")
    );

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
            .filter(
                |e| matches!(&e.kind, EventKind::FeatureSkipped { reason, .. }
                if reason.contains("re-plan"))
            )
            .count(),
        2,
        "both dropped features skipped by the re-plan"
    );
    assert_eq!(
        event_types(&events)
            .iter()
            .filter(|t| **t == "fixfeature.created")
            .count(),
        1,
        "exactly one feature added by the re-plan"
    );

    // The revised mission still folds cleanly (contiguous log, no corruption).
    let state = reducer::fold(&events).unwrap();
    assert_eq!(
        state.mission.milestones[0].features.len(),
        4,
        "3 planned + 1 added"
    );
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
            dirty_tree_commit_as_is(),
            judgement("complete", ""), // M1 f-1-1
            dirty_tree_commit_as_is(),
            judgement("complete", ""), // M2 f-2-1
            fix_features(1),           // M2 round 1 conversion → one fix
            dirty_tree_commit_as_is(),
            judgement("complete", ""), // M2 fix worker
            fix_features(1),           // M2 round 2 conversion at the cap → wants another
        ]),
        validator_with(json!([])), // M1 validation: clean → M1 completes
        worker_pass(),             // M2 f-2-1
        validator_with(finding.clone()), // M2 round 1: finding (fix cycle 1)
        worker_pass(),             // M2 fix worker
        validator_with(finding),   // M2 round 2: finding again → blocked
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
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec![],
        standards_manifest: None,
        reviewer_independence: None,
    };
    engine.approve_plan(plan).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(
        status,
        MissionStatus::Blocked,
        "M2 blocked at the fix-cycle cap"
    );
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
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec![],
        standards_manifest: None,
        reviewer_independence: None,
    };
    let err = engine
        .approve_revised_plan(drops_completed)
        .expect_err("dropping a completed milestone must be rejected");
    assert!(
        matches!(err, kranz_engine::error::EngineError::InvalidState(_)),
        "expected InvalidState, got: {err}"
    );
    assert!(
        err.to_string().contains("M1"),
        "error names the dropped completed milestone: {err}"
    );

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
        considered_alternatives: None,
        command_grants: vec![],
        touch_set: vec![],
        standards_manifest: None,
        reviewer_independence: None,
    };
    let err = engine
        .approve_revised_plan(alters_completed)
        .expect_err("altering a completed milestone's features must be rejected");
    assert!(
        err.to_string().contains("alters"),
        "error explains the alteration: {err}"
    );
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
            no_lesson(),
        ]),
        worker_pass(),
        worker_pass(),
    ]));

    let cfg = MissionConfig {
        max_parallel_workers: 2,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(2, vec![])).unwrap();
    let mission_id = engine.mission_id().to_string();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    // Both plan features completed.
    let ms = &engine.state().mission.milestones[0];
    assert_eq!(
        ms.features[0].status,
        FeatureStatus::Complete,
        "f-1-1 complete"
    );
    assert_eq!(
        ms.features[1].status,
        FeatureStatus::Complete,
        "f-1-2 complete"
    );

    let paths = engine.paths().clone();
    drop(engine);

    // Two feature workers spawned (one per feature) plus the orchestrator.
    let events = read_log(&paths);
    let worker_spawns = events
        .iter()
        .filter(|e| {
            matches!(
                &e.kind,
                EventKind::WorkerSpawned {
                    role: Role::Worker,
                    ..
                }
            )
        })
        .count();
    assert_eq!(worker_spawns, 2, "both features ran a worker");

    // feature.started for both, feature.completed for both, in the log.
    let types = event_types(&events);
    assert_eq!(
        types.iter().filter(|t| **t == "feature.completed").count(),
        2,
        "both features completed: {types:?}"
    );
    assert!(
        types.contains(&"milestone.completed"),
        "milestone completed: {types:?}"
    );
    assert!(
        types.contains(&"mission.completed"),
        "mission completed: {types:?}"
    );

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
            EventKind::OrchestratorDecision { summary, .. }
                if summary.starts_with("parallel plan for") =>
            {
                Some(e.seq)
            }
            _ => None,
        })
        .expect("parallel plan decision exists");
    let first_worker_spawn = events
        .iter()
        .find(|e| {
            matches!(
                &e.kind,
                EventKind::WorkerSpawned {
                    role: Role::Worker,
                    ..
                }
            )
        })
        .expect("a worker spawned")
        .seq;
    assert!(
        plan_seq < first_worker_spawn,
        "parallel plan precedes the first worker"
    );

    // NO leaked worktrees: git worktree list is back to a single (primary)
    // working tree. The per-feature worktree dirs are gone from disk too.
    let repo = GitRepo::open(&root).unwrap();
    let worktrees = repo.list_worktrees().unwrap();
    assert_eq!(
        worktrees.len(),
        1,
        "only the primary worktree remains: {worktrees:?}"
    );
    // The per-feature branches were cleaned up as well.
    assert!(
        !repo
            .branch_exists(&format!("kranz/wt/{mission_id}/f-1-1"))
            .unwrap_or(false),
        "per-feature worktree branch must be deleted"
    );
    assert!(
        !repo
            .branch_exists(&format!("kranz/wt/{mission_id}/f-1-2"))
            .unwrap_or(false),
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
/// the same time, not one-at-a-time. The engine's own peak-concurrency tracker
/// records how many worker sessions were live simultaneously and surfaces it
/// in the batch summary decision; with max_parallel_workers=2 and two
/// independent features, the peak is 2.
///
/// The overlap is DETERMINISTIC, not scheduler-dependent: each worker script
/// carries a rendezvous — its Result is withheld until 3 sessions have
/// started (orchestrator + both workers) — so neither worker can finish
/// before the other starts, on any runner load. (Without the rendezvous this
/// test flaked on slow CI runners: both workers ran back-to-back and the peak
/// read 1. With it, a sequential-dispatch regression deadlocks into the
/// timeout below instead of flaky-passing.)
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
    // two judgements), then the two feature workers, rendezvoused so both must
    // be live at once.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        orch_script(vec![
            parallel_plan(&["f-1-1", "f-1-2"]),
            judgement("complete", ""),
            judgement("complete", ""),
            no_lesson(),
        ]),
        worker_pass().rendezvous(3),
        worker_pass().rendezvous(3),
    ]));

    let cfg = MissionConfig {
        max_parallel_workers: 2,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(2, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
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
    assert_eq!(
        events.last().unwrap().seq,
        events.len() as u64,
        "contiguous seq"
    );
    let worker_spawns = events
        .iter()
        .filter(|e| {
            matches!(
                &e.kind,
                EventKind::WorkerSpawned {
                    role: Role::Worker,
                    ..
                }
            )
        })
        .count();
    let worker_completes = events
        .iter()
        .filter(|e| matches!(&e.kind, EventKind::WorkerCompleted { .. }))
        .count();
    assert_eq!(worker_spawns, 2, "both worker sessions spawned");
    // 2 workers + orchestrator turns each emit worker.completed; at least the
    // two feature workers must be present.
    assert!(
        worker_completes >= 2,
        "both worker sessions completed: {worker_completes}"
    );
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

    let cfg = MissionConfig {
        max_parallel_workers: 2,
        ..test_cfg()
    };
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
    assert_eq!(
        phase1.last().unwrap().seq,
        phase1.len() as u64,
        "contiguous seq after crash"
    );
    let state1 = reducer::fold(&phase1).expect("crashed log still folds cleanly");
    for f in &state1.mission.milestones[0].features {
        assert_eq!(
            f.status,
            FeatureStatus::Active,
            "features left Active by the crash"
        );
        assert!(
            f.worker_runs.is_empty(),
            "no worker run recorded before the crash"
        );
    }
    // No WORKER session's buffered events reached the log (the successful
    // task's buffer was dropped — accepted loss). Orchestrator runs still
    // complete their turns; only feature-worker spawns/completions are the
    // buffered-and-lost ones.
    assert!(
        !phase1.iter().any(|e| matches!(
            &e.kind,
            EventKind::WorkerSpawned {
                role: Role::Worker,
                ..
            }
        )),
        "no buffered worker session survived the crash"
    );

    // --- Phase 2: resume and finish -------------------------------------
    // resume() sweeps the orphaned per-feature worktrees/branches. Both
    // features are Active respawn candidates → the SEQUENTIAL path finishes
    // them (each: worker → judgement). Validators skipped, empty contract.
    let backend2 = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(), // f-1-1 rerun (sequential)
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        worker_pass(), // f-1-2 rerun (sequential)
    ]));
    let backend2_dyn: Arc<dyn AgentBackend> = Arc::clone(&backend2) as Arc<dyn AgentBackend>;
    let mut engine = MissionEngine::resume(backend2_dyn, &root, &mission_id, LockForce::No)
        .expect("resume mission");
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    drop(engine);

    // ONE events.jsonl spanning both lifetimes: contiguous seq, clean Complete
    // fold, both features complete.
    let events = read_log(&paths);
    assert_eq!(events.first().unwrap().seq, 1);
    assert_eq!(
        events.last().unwrap().seq,
        events.len() as u64,
        "one contiguous log"
    );
    let state = reducer::fold(&events).unwrap();
    assert_eq!(state.mission.status, MissionStatus::Complete);
    assert!(state.mission.milestones[0]
        .features
        .iter()
        .all(|f| f.status == FeatureStatus::Complete));

    // No leaked worktrees after recovery.
    let repo = GitRepo::open(&root).unwrap();
    assert_eq!(
        repo.list_worktrees().unwrap().len(),
        1,
        "no leaked worktrees: recovered clean"
    );
    assert!(!repo
        .branch_exists(&format!("kranz/wt/{mission_id}/f-1-1"))
        .unwrap_or(false));
    assert!(!repo
        .branch_exists(&format!("kranz/wt/{mission_id}/f-1-2"))
        .unwrap_or(false));
}

/// The resume() worktree/branch sweep is destructive (`worktree remove
/// --force`, `branch -D`) and must therefore only run once the single-writer
/// lock is held. A second `kranz run` racing a LIVE engine mid parallel batch
/// must fail LockHeld WITHOUT touching the live batch's worktrees/branches
/// (cycle-3 review finding).
#[tokio::test(flavor = "multi_thread")]
async fn resume_does_not_sweep_worktrees_while_lock_is_live() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::new());
    // Engine 1 stays ALIVE holding the single-writer lock, mid "batch".
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();
    let mission_id = engine.mission_id().to_string();

    // Simulate the live batch's per-feature worktree + branch (what Phase A
    // creates before the concurrent workers run). Same layout as
    // parallel_worktree_path: temp dir, kranz-wt-<mission>-<feature>.
    let repo = GitRepo::open(&root).unwrap();
    let branch = format!("kranz/wt/{mission_id}/f-1-1");
    let wt_path = std::env::temp_dir().join(format!("kranz-wt-{mission_id}-f-1-1"));
    let head = repo.head_sha().unwrap();
    repo.add_worktree(&wt_path, &branch, &head).unwrap();

    // Engine 2 (no --force-lock) must refuse at the lock, BEFORE any sweep.
    let backend2: Arc<dyn AgentBackend> = Arc::new(MockBackend::new());
    let err = match MissionEngine::resume(backend2, &root, &mission_id, LockForce::No) {
        Ok(_) => panic!("resume must fail while a live engine holds the lock"),
        Err(e) => e,
    };
    assert!(
        matches!(err, kranz_engine::error::EngineError::LockHeld(_)),
        "expected LockHeld, got: {err}"
    );

    // The live batch's worktree and branch are untouched.
    assert!(
        wt_path.exists(),
        "live worktree must not be swept by a lock-refused resume"
    );
    assert!(
        repo.branch_exists(&branch).unwrap_or(false),
        "live branch must not be -D'd by a lock-refused resume"
    );

    // Cleanup.
    let _ = repo.remove_worktree(&wt_path);
    let _ = repo.prune_worktrees();
    let _ = repo.delete_branch_force(&branch);
    drop(engine);
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
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
        worker_pass(),
    ]));

    let cfg = MissionConfig {
        max_parallel_workers: 1,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(2, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
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
    assert_eq!(
        repo.list_worktrees().unwrap().len(),
        1,
        "no extra worktrees in sequential mode"
    );
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
    assert_eq!(
        resolution.id, "ms-1-conflict-1",
        "namespaced conflict-resolution id"
    );
    // Origin Fix + Pending → the sequential loop (first_incomplete + next_feature)
    // picks it up on the next iteration, straight on the mission branch.
    assert_eq!(
        resolution.origin,
        FeatureOrigin::Fix,
        "resolution is a fix feature"
    );
    assert_eq!(
        resolution.status,
        FeatureStatus::Pending,
        "resolution starts Pending"
    );
    assert!(
        resolution.worker_runs.is_empty(),
        "fresh feature, no runs yet"
    );
    assert_eq!(resolution.respawns, 0);

    // The spec carries the original title, the original spec, the conflicting
    // file list, and the earlier-features-merged note.
    let spec = &resolution.spec;
    assert!(
        spec.contains("build the widget in src/widget.rs"),
        "original spec text: {spec}"
    );
    assert!(
        spec.contains("src/widget.rs"),
        "conflicting file listed: {spec}"
    );
    assert!(
        spec.contains("src/lib.rs"),
        "second conflicting file listed: {spec}"
    );
    assert!(
        spec.contains("already contains") || spec.contains("merged first"),
        "notes earlier features already merged: {spec}"
    );
    assert!(
        resolution.title.contains("feature two"),
        "title references original: {}",
        resolution.title
    );
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
    let mut resolution_1 = plan_feature(
        "ms-1-conflict-1",
        "Resolve merge conflict: feature two",
        "…",
    );
    resolution_1.origin = FeatureOrigin::Fix;
    let existing = vec![f_1_2, f_1_3.clone(), resolution_1];

    let resolution =
        synthesize_conflict_resolution("ms-1", &f_1_3, &["src/x.rs".to_string()], &existing)
            .expect("second conflict synthesizes a resolution");
    assert_eq!(
        resolution.id, "ms-1-conflict-2",
        "second conflict is -conflict-2, no collision"
    );
}

/// Infinite-chain guard: a feature whose id already contains `-conflict-`
/// (a resolution feature) must NOT spawn a resolution-of-a-resolution. The
/// helper returns None, so the milestone proceeds to validation/loop-guard as
/// today rather than looping conflict→resolution forever.
#[test]
fn resolution_feature_does_not_spawn_another_resolution() {
    let mut resolution = plan_feature(
        "ms-1-conflict-1",
        "Resolve merge conflict: feature two",
        "spec",
    );
    resolution.origin = FeatureOrigin::Fix;
    let existing = vec![resolution.clone()];

    let again =
        synthesize_conflict_resolution("ms-1", &resolution, &["src/x.rs".to_string()], &existing);
    assert!(
        again.is_none(),
        "a -conflict- feature must not spawn another resolution"
    );
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
    assert!(
        resolution.spec.contains("the original work"),
        "original spec preserved"
    );
    assert!(
        resolution.spec.contains("no specific files"),
        "empty conflict list gets a placeholder: {}",
        resolution.spec
    );
}

// ---------------------------------------------------------------------------
// Branch isolation + checkout restore (fix-work-branch-isolation,
// fix-checkout-restore-on-completion)
// ---------------------------------------------------------------------------

/// The exact live failure from the first `kranz work` train: approval put the
/// checkout on the mission branch, the operator (or another draft) moved it
/// back to main, and run() then committed every worker commit straight to
/// main. run() must re-assert the mission branch — and, once terminal,
/// restore the base checkout.
#[tokio::test]
async fn run_reasserts_mission_branch_and_restores_base() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            "NONE".to_string(),
        ]),
    ]));
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine
        .approve_plan(simple_plan(
            1,
            vec![assertion("a-1", "the build command succeeds", Some("cd ."))],
        ))
        .unwrap();

    // Simulate the drift: operator back on main after approval.
    raw_git(&root, &["checkout", "main"]);
    let main_before = raw_git(&root, &["rev-parse", "main"]);

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    let branch = engine.state().mission.mission_branch.clone();
    drop(engine);

    // Worker commits landed on the mission branch; main never moved.
    assert_eq!(
        main_before,
        raw_git(&root, &["rev-parse", "main"]),
        "main must not receive mission commits"
    );
    let ahead: u32 = raw_git(&root, &["rev-list", "--count", &format!("main..{branch}")])
        .trim()
        .parse()
        .unwrap();
    assert!(ahead > 0, "the mission branch must carry the work");

    // run() deliberately leaves the mission branch checked out: report.md
    // and plan.md live there, and the operator reads them at completion.
    // (The dispatcher and draft restore checkouts at THEIR boundaries.)
    assert_eq!(
        raw_git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        branch,
        "completed run keeps its artifacts visible on the mission branch"
    );
}

/// Creating a mission while another mission's branch is checked out records a
/// poisoned base (observed live: three drafts stacked). create() refuses.
#[tokio::test]
async fn create_refuses_another_missions_branch_as_base() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    raw_git(&root, &["checkout", "-b", "kranz/mission-m-fake01"]);
    let backend: Arc<dyn AgentBackend> = Arc::new(MockBackend::with_scripts(vec![]));
    let Err(err) = MissionEngine::create(backend, &root, GOAL, test_cfg()) else {
        panic!("create must refuse a kranz/mission-* base branch");
    };
    let msg = err.to_string();
    assert!(
        msg.contains("another mission's branch"),
        "refusal must explain the stacking hazard, got: {msg}"
    );
}

/// Feature f-2-2: the final gate's deterministic non-emptiness safety net.
/// A mock mission whose sole worker delivers nothing (no file writes → no
/// checkpoint commit) must terminate Failed with an honest, non-empty
/// reason, and must never emit `mission.completed` — regardless of the
/// (empty, therefore vacuously green) contract.
#[tokio::test(flavor = "multi_thread")]
async fn empty_deliverable_mission_terminates_failed() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass_no_write(),
        orch_script(vec![judgement("complete", "")]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Failed);

    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(
        types.contains(&"mission.failed"),
        "expected mission.failed: {types:?}"
    );
    assert!(
        !types.contains(&"mission.completed"),
        "must never complete on an empty deliverable diff: {types:?}"
    );
    let reason = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::MissionFailed { reason } => Some(reason.clone()),
            _ => None,
        })
        .expect("mission.failed event carries a reason");
    assert!(!reason.trim().is_empty(), "reason must be non-empty");
}

/// Regression guard: a genuine mission whose worker actually delivers a file
/// (a real, non-meta commit lands on the mission branch) is inert to the
/// f-2-2 safety net — it still completes normally and never emits
/// `mission.failed`.
#[tokio::test(flavor = "multi_thread")]
async fn delivering_mission_still_completes() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);

    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let types = event_types(&events);
    assert!(
        types.contains(&"mission.completed"),
        "expected mission.completed: {types:?}"
    );
    assert!(
        !types.contains(&"mission.failed"),
        "delivering mission must not trip the empty-deliverable safety net: {types:?}"
    );
}

/// Closing the meta-subject spoof hole at the final gate: a worker-authored
/// commit titled like an engine meta commit ("[kranz] mission report
/// cleanup" matches the "[kranz] mission report" template) but carrying a
/// real file IS a deliverable. The empty-diff safety net must count it (the
/// meta exemption is path-verified), where the old subject-only match
/// excluded it and failed the mission on a supposedly empty diff.
#[tokio::test(flavor = "multi_thread")]
async fn spoofed_meta_subject_commit_counts_as_deliverable() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_pass_no_write(),
        orch_script(vec![judgement("complete", ""), no_lesson()]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    // A misbehaving worker's own commit on the mission branch (checkout mode
    // keeps it checked out after approval): meta-template subject, real file.
    std::fs::write(root.join("smuggled.txt"), "real deliverable\n").unwrap();
    raw_git(&root, &["add", "smuggled.txt"]);
    raw_git(&root, &["commit", "-m", "[kranz] mission report cleanup"]);

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(
        status,
        MissionStatus::Complete,
        "a spoofed-subject commit carrying a real file is a deliverable; \
         the empty-deliverable net must count it, not hide it"
    );

    let paths = engine.paths().clone();
    drop(engine);
    let types = event_types(&read_log(&paths));
    assert!(
        !types.contains(&"mission.failed"),
        "must not fail as empty-deliverable: {types:?}"
    );
}

// ---------------------------------------------------------------------------
// 13. Checkpoint secret-scan refusals (§4.4 dirty-tree turn meets the scan)
// ---------------------------------------------------------------------------

/// Fixture secret for the refusal tests below (same shape as
/// git_ops_test.rs's scan-block test; rule id "anthropic-api-key").
const LEAKED_SECRET: &str = "sk-ant-api03-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

/// Worker script: passing report, but the session leaves an UNCOMMITTED
/// secret-bearing file in its cwd — the §4.4 checkpoint's secret scan will
/// refuse to commit it.
fn worker_leaks_secret() -> MockScript {
    MockScript::single_shot_json(&json!({
        "result": "pass",
        "summary": "implemented and tested",
        "filesTouched": ["leak.txt"],
        "testsAdded": [],
        "testEvidence": "all green",
        "commits": []
    }))
    .writes_file("leak.txt", format!("ANTHROPIC_API_KEY={LEAKED_SECRET}\n"))
}

/// Sequential path: the worker leaves a secret-bearing uncommitted file, the
/// orchestrator says commit-as-is, and the checkpoint's secret scan refuses.
/// The refusal must NOT error the run (the old `?` wedged the mission: the
/// tree is still dirty on resume, so resume re-hit the identical error).
/// Instead it is recorded — an orchestrator.decision plus feature.failed
/// carrying the scan's reason — and the milestone BLOCKS: the refused
/// content is still dirty in the shared sequential tree, so running further
/// features would only cascade the same refusal onto them. The mission ends
/// Blocked (resumable once the operator cleans or allowlists the named
/// paths), a recorded state rather than a raw Err.
#[tokio::test(flavor = "multi_thread")]
async fn secret_scan_refusal_fails_feature_instead_of_erroring_run() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_leaks_secret(),
        // Turns: seed, dirty-tree decision. The refusal fails the feature
        // BEFORE any judgement turn, then blocks the milestone; the blocked
        // flow returns without further turns (no queued user message).
        orch_script(vec![dirty_tree_commit_as_is()]),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .expect("a scan refusal must not error the run loop");
    // The refusal poisons the shared tree, so the milestone blocks and the
    // mission ends Blocked — a recorded, resumable state, not a raw Err the
    // operator can only retry into the same wall.
    assert_eq!(status, MissionStatus::Blocked);
    assert_eq!(engine.state().mission.status, MissionStatus::Blocked);
    assert_eq!(
        engine.state().mission.milestones[0].status,
        MilestoneStatus::Blocked,
        "the milestone blocks against the poisoned tree"
    );
    assert_eq!(
        engine.state().mission.milestones[0].features[0].status,
        FeatureStatus::Failed,
        "the leaking feature is failed, not left Active"
    );

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);

    // feature.failed carries the scan's reason (rule id, no secret bytes).
    let reason = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::FeatureFailed { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .expect("feature.failed with the refusal reason is on the log");
    assert!(
        reason.contains("secret scan"),
        "reason names the scan: {reason}"
    );
    assert!(
        reason.contains("anthropic-api-key"),
        "reason names the rule: {reason}"
    );

    // The refusal decision is recorded for the audit trail.
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::OrchestratorDecision { summary, .. }
                if summary.contains("refused by secret scan")
        )),
        "an orchestrator.decision records the refusal"
    );

    // milestone.blocked names the cleanup cue: the still-dirty paths.
    let blocked_reason = events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::MilestoneBlocked { reason, .. } => Some(reason.clone()),
            _ => None,
        })
        .expect("milestone.blocked with the cleanup cue is on the log");
    assert!(
        blocked_reason.contains("secret scan") && blocked_reason.contains("leak.txt"),
        "blocked reason names the scan and the dirty path: {blocked_reason}"
    );

    // And the raw secret never reaches the event log in any event.
    let raw_log = std::fs::read_to_string(paths.events_file()).unwrap();
    assert!(
        !raw_log.contains(LEAKED_SECRET),
        "the raw secret must never land in events.jsonl"
    );
}

/// The cascade the block prevents: with a second Pending feature behind the
/// leaking one, feature B must never RUN against the poisoned tree — the old
/// continue-the-milestone behaviour spawned B, tripped B's dirty-tree turn on
/// A's still-uncommitted secret, and failed B with a reason naming A's leak
/// (cascading misattribution). B's worker script and a second dirty-tree
/// reply are queued so that old behaviour would be visible; they must go
/// unconsumed.
#[tokio::test(flavor = "multi_thread")]
async fn secret_scan_refusal_blocks_milestone_so_later_features_never_run() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    let backend = Arc::new(MockBackend::with_scripts(vec![
        worker_leaks_secret(),
        // One dirty-tree reply for f-1-1's refusal, plus a second one that
        // only a (wrong) f-1-2 dirty-tree turn would consume.
        orch_script(vec![dirty_tree_commit_as_is(), dirty_tree_commit_as_is()]),
        // Would be popped by f-1-2's worker spawn — must never happen.
        worker_pass_no_write(),
    ]));

    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(2, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .expect("a scan refusal must not error the run loop");
    assert_eq!(status, MissionStatus::Blocked);
    let ms = &engine.state().mission.milestones[0];
    assert_eq!(ms.status, MilestoneStatus::Blocked);
    assert_eq!(ms.features[0].status, FeatureStatus::Failed);
    assert_eq!(
        ms.features[1].status,
        FeatureStatus::Pending,
        "feature B must stay Pending, not be failed against A's poisoned tree"
    );

    // Exactly two sessions started: f-1-1's worker and the orchestrator.
    // f-1-2's queued worker script was never popped.
    assert_eq!(
        backend.started_specs().len(),
        2,
        "no session may spawn for feature B after the milestone blocked"
    );

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);

    // Only f-1-1 was ever spawned and only f-1-1 failed; no event blames
    // feature B with feature A's leak.
    assert!(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::WorkerSpawned { feature_id: Some(id), .. } if id == "f-1-1"
        )),
        "feature A's worker spawned"
    );
    assert!(
        !events.iter().any(|e| matches!(
            &e.kind,
            EventKind::WorkerSpawned { feature_id: Some(id), .. } if id == "f-1-2"
        )),
        "feature B's worker must never spawn"
    );
    let failed: Vec<&String> = events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::FeatureFailed { feature_id, .. } => Some(feature_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        failed,
        vec!["f-1-1"],
        "exactly one feature.failed, for the feature that actually leaked"
    );
}

/// Parallel path of the same refusal: parallel workers leave secret-bearing
/// files in their worktrees. The old code `let _ =`-swallowed the checkpoint
/// error, silently dropping the dirty deliverables before judgement; now
/// each refusal is recorded (orchestrator.decision + feature.failed) and the
/// batch — and run loop — still finishes cleanly instead of erroring. Both
/// workers leak (scripts pop FIFO, and worker/feature pairing within the
/// batch is scheduler-dependent), so the assertion holds per feature.
#[tokio::test(flavor = "multi_thread")]
async fn parallel_secret_scan_refusal_is_recorded_not_swallowed() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();

    // Session start order: orchestrator (parallelization decision) first,
    // then both workers. Neither feature reaches a judgement turn — each
    // checkpoint refusal short-circuits to not-ready-to-merge.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        orch_script(vec![parallel_plan(&["f-1-1", "f-1-2"])]),
        worker_leaks_secret(),
        worker_leaks_secret(),
    ]));

    let cfg = MissionConfig {
        max_parallel_workers: 2,
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(2, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .expect("a parallel scan refusal must not error the run loop");
    // Both features failed their checkpoints → nothing merged → the final
    // gate's empty-deliverable net fails the mission as a recorded state.
    assert_eq!(status, MissionStatus::Failed);
    let ms = &engine.state().mission.milestones[0];
    assert_eq!(ms.features[0].status, FeatureStatus::Failed);
    assert_eq!(ms.features[1].status, FeatureStatus::Failed);

    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);

    // One refusal decision per feature — recorded, not swallowed.
    for feature_id in ["f-1-1", "f-1-2"] {
        assert!(
            events.iter().any(|e| matches!(
                &e.kind,
                EventKind::OrchestratorDecision { summary, .. }
                    if summary.contains("refused by secret scan")
                        && summary.contains(feature_id)
            )),
            "a refusal decision names {feature_id}"
        );
    }
    let raw_log = std::fs::read_to_string(paths.events_file()).unwrap();
    assert!(
        !raw_log.contains(LEAKED_SECRET),
        "the raw secret must never land in events.jsonl"
    );

    // The cleanup guard still reaped the (dirty) per-feature worktrees.
    let repo = GitRepo::open(&root).unwrap();
    let worktrees = repo.list_worktrees().unwrap();
    assert_eq!(
        worktrees.len(),
        1,
        "only the primary worktree remains: {worktrees:?}"
    );
}

// ---------------------------------------------------------------------------
// Pack contract (ticket pack-contract-gates-prompts): a configured pack's
// deterministic gate runs at the final gate and its prompt reaches the
// target role; invalid packs fail closed; no pack ⇒ byte-identical.
// ---------------------------------------------------------------------------

/// The committed synthetic example pack — ALL vocabulary synthetic (`zz-`).
fn pack_contract_fixture_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pack-contract-synthetic")
}

/// Minimal script set for a one-feature, validators-off mission that reaches
/// the final gate and completes (the same shape as the other single-feature
/// completions in this file).
fn pack_contract_scripts() -> Vec<MockScript> {
    vec![
        worker_pass(),
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            no_lesson(),
        ]),
    ]
}

/// The worker session's rendered system prompt among the started sessions
/// (the worker's SingleShot task is the only one naming its feature id).
fn worker_system_prompt(backend: &MockBackend) -> String {
    let specs: Vec<_> = backend
        .started_specs()
        .into_iter()
        .filter(|s| {
            matches!(&s.prompt, PromptMode::SingleShot(task) if task.contains("Implement feature `f-1-1`"))
        })
        .collect();
    assert_eq!(specs.len(), 1, "exactly one worker session for f-1-1");
    specs[0]
        .append_system_prompt
        .clone()
        .expect("worker sessions carry an append_system_prompt")
}

/// The prompt hash recorded on the worker role's worker.spawned event.
fn worker_prompt_hash(events: &[Event]) -> String {
    events
        .iter()
        .find_map(|e| match &e.kind {
            EventKind::WorkerSpawned {
                role: Role::Worker,
                prompt_hash,
                ..
            } => Some(prompt_hash.clone()),
            _ => None,
        })
        .expect("worker.spawned for the worker role")
}

/// (summary, detail) of every orchestrator.decision on the log.
fn pack_contract_decisions(events: &[Event]) -> Vec<(String, Option<String>)> {
    events
        .iter()
        .filter_map(|e| match &e.kind {
            EventKind::OrchestratorDecision { summary, detail } => {
                Some((summary.clone(), detail.clone()))
            }
            _ => None,
        })
        .collect()
}

/// THE acceptance fixture: the synthetic example pack loads, its gate RUNS
/// (its verdict is evaluated at the final-gate surface), and its prompt
/// reaches the targeted role's prompt — and only that role's.
#[tokio::test(flavor = "multi_thread")]
async fn pack_contract_gate_runs_and_prompt_reaches_worker() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::with_scripts(pack_contract_scripts()));
    let cfg = MissionConfig {
        pack_dir: Some(pack_contract_fixture_dir().display().to_string()),
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let decisions = pack_contract_decisions(&events);

    // Run-start audit: the pack and everything it registered is on the log
    // (short summary; the full registration list rides in the detail).
    assert!(
        decisions.iter().any(|(summary, detail)| summary
            .starts_with("pack contract: pack `zz-synthetic-fixture-pack` (schema 3) registered:")
            && detail
                .as_deref()
                .is_some_and(|d| d.contains("zz-pack-gate-synthetic")
                    && d.contains("zz-pack-prompt-synthetic"))),
        "run-start pack decision missing: {decisions:?}"
    );

    // The gate RAN and its verdict was evaluated at the final-gate surface.
    let (summary, detail) = decisions
        .iter()
        .find(|(summary, _)| {
            summary.starts_with("pack `zz-synthetic-fixture-pack` gates (final gate):")
        })
        .expect("final-gate pack decision missing");
    assert!(
        summary.contains("1 deterministic gate(s) passed"),
        "{summary}"
    );
    let detail = detail.as_deref().expect("verdict detail");
    assert!(detail.contains("zz-pack-gate-synthetic: PASS"), "{detail}");

    // The pack prompt reached the worker's rendered prompt — marked as pack
    // guidance, text sourced from the pack's textFile.
    let worker_prompt = worker_system_prompt(&backend);
    assert!(
        worker_prompt.contains("ZZ-SYNTHETIC-PACK-MARKER"),
        "pack guidance missing from the worker prompt"
    );
    assert!(
        worker_prompt
            .contains("pack `zz-synthetic-fixture-pack`, prompt `zz-pack-prompt-synthetic`"),
        "the injection is marked with its pack/prompt provenance"
    );
    // …and ONLY the worker's: no other session's system prompt carries it.
    for spec in backend.started_specs() {
        let is_worker = matches!(&spec.prompt, PromptMode::SingleShot(task) if task.contains("Implement feature `f-1-1`"));
        if !is_worker {
            let prompt = spec.append_system_prompt.as_deref().unwrap_or("");
            assert!(
                !prompt.contains("ZZ-SYNTHETIC-PACK-MARKER"),
                "pack guidance leaked into a non-target role's prompt"
            );
        }
    }

    // The recorded prompt hash names the extended text, not the bare
    // template — traceability to the exact prompt that ran.
    assert_ne!(
        worker_prompt_hash(&events),
        kranz_engine::prompts::hash(Role::Worker),
        "a pack-extended prompt must not record the bare template hash"
    );
}

/// A failing pack gate is advisory, exactly like the engine floor gates:
/// its verdict is named and recorded, and the mission still completes.
/// (Also exercises repo-relative packDir resolution.)
#[tokio::test(flavor = "multi_thread")]
async fn pack_contract_failing_gate_is_advisory_named_and_never_blocks() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    // `exit 1` fails under both `sh -c` and `cmd /C`.
    let pack_dir = root.join("zz-failing-pack");
    std::fs::create_dir_all(&pack_dir).unwrap();
    std::fs::write(
        pack_dir.join("pack.toml"),
        "[pack]\nname = \"zz-failing-pack\"\nschema = 3\n\n\
         [[gate]]\nname = \"zz-pack-gate-failing\"\ncommand = \"exit 1\"\n",
    )
    .unwrap();

    let backend = Arc::new(MockBackend::with_scripts(pack_contract_scripts()));
    let cfg = MissionConfig {
        pack_dir: Some("zz-failing-pack".to_string()),
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(
        status,
        MissionStatus::Complete,
        "advisory: a failing pack gate never blocks completion"
    );
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let (summary, detail) = pack_contract_decisions(&events)
        .into_iter()
        .find(|(summary, _)| summary.starts_with("pack `zz-failing-pack` gates (final gate):"))
        .expect("final-gate pack decision missing");
    assert!(
        summary.contains("named gate(s) failed: zz-pack-gate-failing"),
        "{summary}"
    );
    let detail = detail.expect("verdict detail");
    assert!(detail.contains("zz-pack-gate-failing: FAIL"), "{detail}");
}

/// Regression: with no packDir configured the mission is byte-identical to
/// a pack-less engine — no pack decisions, the worker prompt is the bare
/// rendered template, and the recorded hash is the template hash.
#[tokio::test(flavor = "multi_thread")]
async fn pack_contract_no_pack_behavior_is_byte_identical() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let backend = Arc::new(MockBackend::with_scripts(pack_contract_scripts()));
    let mut engine = make_engine(&backend, &root, test_cfg());
    engine.approve_plan(simple_plan(1, vec![])).unwrap();

    let status = timeout(TEST_TIMEOUT, engine.run())
        .await
        .expect("run must not hang")
        .unwrap();
    assert_eq!(status, MissionStatus::Complete);
    let paths = engine.paths().clone();
    drop(engine);

    let events = read_log(&paths);
    let decisions = pack_contract_decisions(&events);
    assert!(
        !decisions
            .iter()
            .any(|(summary, _)| summary.starts_with("pack contract:") || summary.contains("pack `")),
        "no pack decisions without a pack: {decisions:?}"
    );
    let worker_prompt = worker_system_prompt(&backend);
    assert!(
        !worker_prompt.contains("Pack guidance"),
        "no pack section without a pack"
    );
    assert_eq!(
        worker_prompt_hash(&events),
        kranz_engine::prompts::hash(Role::Worker),
        "the bare template hash is recorded without a pack"
    );
}

/// An invalid untracked pack fails CLOSED before approval — the error names
/// the offending field, no consent event lands, and no worker ever spawns.
#[tokio::test(flavor = "multi_thread")]
async fn pack_contract_invalid_pack_fails_closed_before_approval() {
    if !setup() {
        return;
    }
    let (_dir, root) = init_repo();
    let pack_dir = root.join("zz-broken-pack");
    std::fs::create_dir_all(&pack_dir).unwrap();
    std::fs::write(
        pack_dir.join("pack.toml"),
        "[pack]\nname = \"zz-broken-pack\"\nschema = 3\n\n\
         [[gate]]\nname = \"zz-dup\"\ncommand = \"cd .\"\n\n\
         [[gate]]\nname = \"zz-dup\"\ncommand = \"cd .\"\n",
    )
    .unwrap();

    let backend = Arc::new(MockBackend::with_scripts(pack_contract_scripts()));
    let cfg = MissionConfig {
        pack_dir: Some("zz-broken-pack".to_string()),
        ..test_cfg()
    };
    let mut engine = make_engine(&backend, &root, cfg);
    let err = engine
        .approve_plan(simple_plan(1, vec![]))
        .expect_err("an invalid pack must fail approval closed");
    let msg = err.to_string();
    assert!(
        msg.contains("duplicate [[gate]] name `zz-dup`"),
        "the error names the offending field: {msg}"
    );
    let paths = engine.paths().clone();
    drop(engine);
    let events = read_log(&paths);
    assert!(
        !events.iter().any(|e| matches!(
            e.kind,
            EventKind::PlanApproved { .. } | EventKind::WorkerSpawned { .. }
        )),
        "neither consent nor spend may occur when the pack cannot load"
    );
}
