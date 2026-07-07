//! M3 corruption-soak harness (roadmap M3 "done when", corruption half).
//!
//! One `#[ignore]`d test, `soak_parallel_corruption`, runs `KRANZ_SOAK_ITERS`
//! iterations (default 20; see `scripts/soak.sh`). Each iteration builds a
//! FRESH throwaway repo and cycles deterministically through three parallel-
//! mission variants (`i % 3`):
//!
//!   a. CLEAN        — 2 milestones × 2 independent features, both batches
//!                     merge cleanly → Mission Complete.
//!   b. CRASH+RESUME — the first batch is crash-simulated (a starved worker
//!                     script aborts the batch, `run()` errors), then a fresh
//!                     engine resumes the SAME log and completes — the
//!                     `crash_mid_parallel_batch_resumes_cleanly` pattern.
//!   c. CONFLICT     — the two parallel workers leave REAL conflicting files
//!                     in their worktrees (via a backend wrapper — the mock
//!                     itself never touches disk), Phase C checkpoint-commits
//!                     them, the second merge hits an add/add conflict, the
//!                     engine synthesizes `ms-1-conflict-1`, and the sequential
//!                     resolution pass completes the mission.
//!
//! After EVERY iteration the full invariant set is asserted: contiguous seq
//! from 1, clean `reducer::fold`, mission Complete, every feature terminal as
//! expected, state.json snapshot == fresh fold, no leaked worktrees, no
//! `kranz/wt/*` branches, clean `git status`. Any failure panics with the
//! iteration number, variant, and the tail of events.jsonl.
//!
//! Integration tests cannot import from each other, so the minimal fixture
//! helpers are COPIED from mission_test.rs (kept byte-small and noted here):
//! `isolate_git_env` / `git_available` / `setup`, `raw_git`, `init_repo`,
//! `GOAL`, `test_cfg`, `worker_pass`, `orch_script`, `judgement`,
//! `parallel_plan`, `read_log`. `make_engine` is adapted to take
//! `Arc<dyn AgentBackend>` (the conflict variant wraps the mock).

use kranz_engine::backend::{AgentBackend, AgentSession, SessionSpec};
use kranz_engine::backend_mock::{mock_init, mock_result_text, mock_text, MockBackend, MockScript};
use kranz_engine::error::Result as EngineResult;
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::events::{Event, EventKind};
use kranz_engine::git_ops::GitRepo;
use kranz_engine::orchestrator::MissionEngine;
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

/// Generous bound proving no single engine run can hang the soak.
const TEST_TIMEOUT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------------------
// Fixtures copied from mission_test.rs (see module docs)
// ---------------------------------------------------------------------------

static ENV_ISOLATION: Once = Once::new();

fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing =
            std::env::temp_dir().join(format!("kranz-soak-test-no-config-{}", std::process::id()));
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
        eprintln!("skipping test: git is not on PATH");
        false
    }
}

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

/// Fresh repo on branch `main` with one seed commit (canonicalized root).
fn init_repo() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let init = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .expect("spawn git init");
    if !init.status.success() {
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

const GOAL: &str = "ship the demo feature";

fn test_cfg() -> MissionConfig {
    MissionConfig {
        skip_scrutiny: true,
        skip_functional: true,
        ..MissionConfig::default()
    }
}

fn make_engine(backend: Arc<dyn AgentBackend>, root: &Path, cfg: MissionConfig) -> MissionEngine {
    MissionEngine::create(backend, root, GOAL, cfg).expect("create mission engine")
}

/// Worker script: completed single-shot run with a passing WorkerReport.
/// Writes a unique file into the session cwd so the worker leaves a dirty
/// tree behind (§4.4), which the engine checkpoints as a real, non-meta
/// commit on the mission branch. The path is unique per worker (atomic
/// counter) so parallel-batch merges never collide on the same path.
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

/// Orchestrator streaming script: seed turn, then one batch per engine turn.
fn orch_script(replies: Vec<String>) -> MockScript {
    MockScript::streaming(vec![mock_init("orch-session"), mock_result_text("ready")]).responding(
        replies
            .iter()
            .map(|reply| vec![mock_text(reply), mock_result_text(reply)])
            .collect(),
    )
}

fn judgement(decision: &str, guidance: &str) -> String {
    json!({ "decision": decision, "guidance": guidance, "summary": format!("worker judged: {decision}") })
        .to_string()
}

/// Dirty-tree-turn reply (§4.4): the worker left uncommitted changes; commit
/// them as-is so they land on the mission branch. Only sequential-path
/// workers need this turn — parallel-batch workers' dirty trees are
/// checkpoint-committed onto their per-feature branch during the merge, with
/// no explicit orchestrator turn.
fn dirty_tree_commit_as_is() -> String {
    json!({ "action": "commit-as-is", "note": "worker delivered files" }).to_string()
}

fn parallel_plan(ids: &[&str]) -> String {
    json!({
        "independent": ids,
        "mergeOrder": ids,
        "summary": format!("{} features are independent", ids.len())
    })
    .to_string()
}

fn read_log(paths: &MissionPaths) -> Vec<Event> {
    EventLog::read_events(&paths.events_file()).expect("read events.jsonl")
}

// ---------------------------------------------------------------------------
// Soak-specific fixtures
// ---------------------------------------------------------------------------

/// A plan of `milestones` milestones × `features` features each, empty
/// contract (validators are skipped; no final-gate verdict turn needed).
fn soak_plan(milestones: usize, features: usize) -> Plan {
    Plan {
        goal: GOAL.to_string(),
        validation_contract: vec![],
        milestones: (1..=milestones)
            .map(|m| PlanMilestone {
                title: format!("M{m}"),
                features: (1..=features)
                    .map(|i| PlanFeature {
                        title: format!("feature {m}.{i}"),
                        spec: format!("build part {m}.{i}"),
                        validation_criteria: vec![format!("part {m}.{i} works")],
                    })
                    .collect(),
            })
            .collect(),
        command_grants: vec![],
        touch_set: vec![],
    }
}

/// Backend wrapper for the CONFLICT variant: delegates to the inner mock, but
/// whenever a session starts inside a parallel per-feature worktree (cwd
/// basename `kranz-wt-…`) it first drops a `CONFLICT.txt` whose content is
/// keyed to that worktree. Phase C checkpoint-commits the dirty tree onto the
/// feature branch, so the two branches both add the same path with different
/// content → a REAL add/add merge conflict on the second merge. Workers run
/// at the repo root (the sequential resolution pass) are untouched.
struct ConflictBackend {
    inner: MockBackend,
}

#[async_trait::async_trait]
impl AgentBackend for ConflictBackend {
    async fn start(&self, spec: SessionSpec) -> EngineResult<Box<dyn AgentSession>> {
        let worktree_name = spec
            .cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .filter(|n| n.starts_with("kranz-wt-"));
        if let Some(name) = worktree_name {
            std::fs::write(spec.cwd.join("CONFLICT.txt"), format!("edit from {name}\n"))
                .expect("write conflicting file into the parallel worktree");
        }
        self.inner.start(spec).await
    }
}

// ---------------------------------------------------------------------------
// Failure context: every assertion routes through here so any failure names
// the iteration + variant and dumps the tail of events.jsonl.
// ---------------------------------------------------------------------------

struct SoakCtx {
    iter: usize,
    variant: &'static str,
    paths: MissionPaths,
}

impl SoakCtx {
    fn fail(&self, msg: impl std::fmt::Display) -> ! {
        let tail = std::fs::read_to_string(self.paths.events_file())
            .map(|s| {
                let lines: Vec<&str> = s.lines().collect();
                let start = lines.len().saturating_sub(12);
                format!(
                    "(last {} of {} events)\n{}",
                    lines.len() - start,
                    lines.len(),
                    lines[start..].join("\n")
                )
            })
            .unwrap_or_else(|e| format!("<events.jsonl unreadable: {e}>"));
        panic!(
            "soak iteration {} [{}] FAILED: {}\n--- events.jsonl tail ---\n{}",
            self.iter, self.variant, msg, tail
        );
    }

    fn ensure(&self, cond: bool, msg: impl std::fmt::Display) {
        if !cond {
            self.fail(msg);
        }
    }
}

// ---------------------------------------------------------------------------
// The per-iteration invariant set (roadmap M3 corruption bar)
// ---------------------------------------------------------------------------

/// Structural JSON comparison for the snapshot-vs-fold invariant: exact on
/// everything except numbers, which compare within 1e-9. The tolerance exists
/// ONLY because serde_json's default (non-`float_roundtrip`) parser is lossy
/// in the last ulp when state.json is read back (e.g. an accumulated cost of
/// 0.10999999999999999 on disk parses as 0.11) — a reader artifact, not log
/// corruption. Returns `Some("<path>: <a> != <b>")` for the first mismatch.
fn json_diff(a: &serde_json::Value, b: &serde_json::Value, path: &str) -> Option<String> {
    use serde_json::Value;
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            let (x, y) = (
                x.as_f64().unwrap_or(f64::NAN),
                y.as_f64().unwrap_or(f64::NAN),
            );
            if (x - y).abs() > 1e-9 {
                Some(format!("{path}: {x} != {y}"))
            } else {
                None
            }
        }
        (Value::Array(xs), Value::Array(ys)) => {
            if xs.len() != ys.len() {
                return Some(format!(
                    "{path}: array lengths {} != {}",
                    xs.len(),
                    ys.len()
                ));
            }
            xs.iter()
                .zip(ys)
                .enumerate()
                .find_map(|(i, (x, y))| json_diff(x, y, &format!("{path}[{i}]")))
        }
        (Value::Object(xs), Value::Object(ys)) => {
            for key in xs.keys().chain(ys.keys()) {
                match (xs.get(key), ys.get(key)) {
                    (Some(x), Some(y)) => {
                        if let Some(d) = json_diff(x, y, &format!("{path}.{key}")) {
                            return Some(d);
                        }
                    }
                    (x, y) => {
                        return Some(format!(
                            "{path}.{key}: {} != {}",
                            x.map(|v| v.to_string())
                                .unwrap_or_else(|| "<absent>".into()),
                            y.map(|v| v.to_string())
                                .unwrap_or_else(|| "<absent>".into())
                        ))
                    }
                }
            }
            None
        }
        _ => {
            if a == b {
                None
            } else {
                Some(format!("{path}: {a} != {b}"))
            }
        }
    }
}

/// Asserts the full invariant set against a finished iteration's repo + log.
/// `allowed_failed` lists feature ids that are EXPECTED to end Failed (the
/// conflicted feature in the CONFLICT variant — its work is redone by the
/// `-conflict-` resolution feature); every other feature must be Complete.
fn assert_invariants(ctx: &SoakCtx, root: &Path, mission_id: &str, allowed_failed: &[&str]) {
    // 1. The log reads back at all (read_events itself refuses seq gaps) and
    //    seq is explicitly contiguous from 1.
    let events = match EventLog::read_events(&ctx.paths.events_file()) {
        Ok(events) => events,
        Err(e) => ctx.fail(format!("event log unreadable/corrupt: {e}")),
    };
    ctx.ensure(!events.is_empty(), "event log is empty");
    for (i, event) in events.iter().enumerate() {
        ctx.ensure(
            event.seq == (i + 1) as u64,
            format!(
                "seq gap: position {i} carries seq {} (want {})",
                event.seq,
                i + 1
            ),
        );
    }

    // 2. The reducer folds the whole log cleanly …
    let state = match reducer::fold(&events) {
        Ok(state) => state,
        Err(e) => ctx.fail(format!("reducer::fold failed on the final log: {e}")),
    };

    // 3. … to a Complete mission …
    ctx.ensure(
        state.mission.status == MissionStatus::Complete,
        format!("mission status {:?}, want Complete", state.mission.status),
    );

    // 4. … with every feature in its expected terminal state.
    for ms in &state.mission.milestones {
        for f in &ms.features {
            let ok = if allowed_failed.contains(&f.id.as_str()) {
                f.status == FeatureStatus::Failed
            } else {
                f.status == FeatureStatus::Complete
            };
            ctx.ensure(
                ok,
                format!(
                    "feature {} ended {:?} (allowed_failed: {allowed_failed:?})",
                    f.id, f.status
                ),
            );
        }
    }

    // 5. The state.json snapshot matches a fresh fold of the log exactly
    //    (MissionState has no PartialEq; compare the serialized values).
    let snapshot = match reducer::read_snapshot(&ctx.paths.state_file()) {
        Ok(snapshot) => snapshot,
        Err(e) => ctx.fail(format!("state.json snapshot unreadable: {e}")),
    };
    let snapshot_json = serde_json::to_value(&snapshot).expect("serialize snapshot");
    let fold_json = serde_json::to_value(&state).expect("serialize fold");
    if let Some(diff) = json_diff(&snapshot_json, &fold_json, "$") {
        ctx.fail(format!(
            "state.json snapshot diverges from a fresh fold at {diff}"
        ));
    }

    // 6. Exactly the main worktree remains — no per-feature worktree leaked.
    let repo = match GitRepo::open(root) {
        Ok(repo) => repo,
        Err(e) => ctx.fail(format!("GitRepo::open failed: {e}")),
    };
    match repo.list_worktrees() {
        Ok(worktrees) => ctx.ensure(
            worktrees.len() == 1,
            format!("leaked worktrees: {worktrees:?}"),
        ),
        Err(e) => ctx.fail(format!("git worktree list failed: {e}")),
    }
    // No per-feature worktree dir for THIS mission left in the temp dir.
    let leak_prefix = format!("kranz-wt-{mission_id}-");
    for entry in std::fs::read_dir(std::env::temp_dir())
        .expect("read temp dir")
        .flatten()
    {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        ctx.ensure(
            !name.starts_with(&leak_prefix),
            format!("parallel worktree dir leaked into temp: {name}"),
        );
    }

    // 7. No kranz/wt/<mission>/* branches remain.
    let wt_branches = raw_git(root, &["branch", "--list", "kranz/wt/*"]);
    ctx.ensure(
        wt_branches.trim().is_empty(),
        format!("leftover worktree branches: {}", wt_branches.trim()),
    );

    // 8. No uncommitted mess in the repo (aborted merges must leave it clean).
    let status = raw_git(root, &["status", "--porcelain"]);
    ctx.ensure(
        status.trim().is_empty(),
        format!("repo left dirty: {}", status.trim()),
    );
}

/// Drives `engine.run()` under the timeout and unwraps to a status, failing
/// through the soak context on hang or error.
async fn run_to_status(ctx: &SoakCtx, engine: &mut MissionEngine) -> MissionStatus {
    match timeout(TEST_TIMEOUT, engine.run()).await {
        Err(_) => ctx.fail("engine.run() hung past the test timeout"),
        Ok(Err(e)) => ctx.fail(format!("engine.run() errored: {e}")),
        Ok(Ok(status)) => status,
    }
}

// ---------------------------------------------------------------------------
// Variant a: CLEAN — 2 milestones × 2 independent features, both batches merge
// ---------------------------------------------------------------------------

async fn run_clean_iteration(iter: usize) {
    let (_dir, root) = init_repo();

    // Session order: orchestrator first (parallel plan for ms-1), then the two
    // ms-1 workers, then (after ms-1 completes) the two ms-2 workers.
    // Orchestrator turns: seed, parallel plan ms-1, judgement ×2, parallel
    // plan ms-2, judgement ×2.
    let backend = Arc::new(MockBackend::with_scripts(vec![
        orch_script(vec![
            parallel_plan(&["f-1-1", "f-1-2"]),
            judgement("complete", ""),
            judgement("complete", ""),
            parallel_plan(&["f-2-1", "f-2-2"]),
            judgement("complete", ""),
            judgement("complete", ""),
        ]),
        worker_pass(),
        worker_pass(),
        worker_pass(),
        worker_pass(),
    ]));

    let cfg = MissionConfig {
        max_parallel_workers: 2,
        ..test_cfg()
    };
    let mut engine = make_engine(backend, &root, cfg);
    engine.approve_plan(soak_plan(2, 2)).expect("approve plan");
    let mission_id = engine.mission_id().to_string();
    let ctx = SoakCtx {
        iter,
        variant: "CLEAN",
        paths: engine.paths().clone(),
    };

    let status = run_to_status(&ctx, &mut engine).await;
    ctx.ensure(
        status == MissionStatus::Complete,
        format!("run() returned {status:?}, want Complete"),
    );
    drop(engine); // flush + release the lock before reading the log

    assert_invariants(&ctx, &root, &mission_id, &[]);
}

// ---------------------------------------------------------------------------
// Variant b: CRASH+RESUME — starved batch aborts, a fresh engine finishes
// ---------------------------------------------------------------------------

async fn run_crash_resume_iteration(iter: usize) {
    let (_dir, root) = init_repo();

    // Phase 1: the orchestrator marks both features independent; the batch
    // fans out two workers but only ONE worker script is queued, so the other
    // session's start errors and the batch aborts — run() errors mid-batch
    // (the in-process crash simulation from mission_test.rs).
    let backend1 = Arc::new(MockBackend::with_scripts(vec![
        orch_script(vec![parallel_plan(&["f-1-1", "f-1-2"])]),
        worker_pass(), // only ONE worker script for a TWO-worker batch
    ]));

    let cfg = MissionConfig {
        max_parallel_workers: 2,
        ..test_cfg()
    };
    let mut engine = make_engine(backend1, &root, cfg.clone());
    engine.approve_plan(soak_plan(1, 2)).expect("approve plan");
    let mission_id = engine.mission_id().to_string();
    let ctx = SoakCtx {
        iter,
        variant: "CRASH+RESUME",
        paths: engine.paths().clone(),
    };

    match timeout(TEST_TIMEOUT, engine.run()).await {
        Err(_) => ctx.fail("crash phase hung instead of erroring"),
        Ok(Ok(status)) => ctx.fail(format!(
            "crash phase unexpectedly succeeded with {status:?} (starved worker should abort)"
        )),
        Ok(Err(_)) => {} // the scripted crash
    }
    drop(engine); // releases the lock, flushes buffered deltas

    // Mid-crash sanity: the log must already read + fold cleanly.
    let phase1 = match EventLog::read_events(&ctx.paths.events_file()) {
        Ok(events) => events,
        Err(e) => ctx.fail(format!("post-crash log unreadable/corrupt: {e}")),
    };
    if let Err(e) = reducer::fold(&phase1) {
        ctx.fail(format!("post-crash log does not fold: {e}"));
    }

    // Phase 2: resume on the SAME log. Both features are Active respawn
    // candidates → the sequential path finishes them (worker → judgement ×2).
    let backend2: Arc<dyn AgentBackend> = Arc::new(MockBackend::with_scripts(vec![
        worker_pass(), // f-1-1 rerun (sequential)
        orch_script(vec![
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
        ]),
        worker_pass(), // f-1-2 rerun (sequential)
    ]));
    let mut engine = match MissionEngine::resume(backend2, &root, &mission_id, LockForce::No) {
        Ok(engine) => engine,
        Err(e) => ctx.fail(format!("resume after crash failed: {e}")),
    };
    let status = run_to_status(&ctx, &mut engine).await;
    ctx.ensure(
        status == MissionStatus::Complete,
        format!("resumed run() returned {status:?}, want Complete"),
    );
    drop(engine);

    assert_invariants(&ctx, &root, &mission_id, &[]);
}

// ---------------------------------------------------------------------------
// Variant c: CONFLICT — real add/add merge conflict → resolution fix-feature
// ---------------------------------------------------------------------------

async fn run_conflict_iteration(iter: usize) {
    let (_dir, root) = init_repo();

    // Both parallel workers drop conflicting CONFLICT.txt files in their
    // worktrees (ConflictBackend); the checkpoint commit carries them onto the
    // per-feature branches. Merge order f-1-1 then f-1-2: the first merge is
    // clean, the second conflicts → feature.failed(f-1-2) + fixfeature
    // ms-1-conflict-1 → the sequential pass runs the resolution worker.
    // Orchestrator turns: seed, parallel plan, judgement f-1-1, judgement
    // f-1-2 (both Phase C), judgement ms-1-conflict-1 (sequential).
    let inner = MockBackend::with_scripts(vec![
        orch_script(vec![
            parallel_plan(&["f-1-1", "f-1-2"]),
            judgement("complete", ""),
            judgement("complete", ""),
            dirty_tree_commit_as_is(),
            judgement("complete", ""),
        ]),
        worker_pass(), // f-1-1 (parallel worktree)
        worker_pass(), // f-1-2 (parallel worktree)
        worker_pass(), // ms-1-conflict-1 (sequential, repo root)
    ]);
    let backend: Arc<dyn AgentBackend> = Arc::new(ConflictBackend { inner });

    let cfg = MissionConfig {
        max_parallel_workers: 2,
        ..test_cfg()
    };
    let mut engine = make_engine(backend, &root, cfg);
    engine.approve_plan(soak_plan(1, 2)).expect("approve plan");
    let mission_id = engine.mission_id().to_string();
    let ctx = SoakCtx {
        iter,
        variant: "CONFLICT",
        paths: engine.paths().clone(),
    };

    let status = run_to_status(&ctx, &mut engine).await;
    ctx.ensure(
        status == MissionStatus::Complete,
        format!("run() returned {status:?}, want Complete"),
    );
    drop(engine);

    // The conflict path really fired (guards against a silently-clean merge
    // making this variant a second CLEAN): f-1-2 failed on a conflicted merge
    // and the resolution fix-feature was created.
    let events = read_log(&ctx.paths);
    ctx.ensure(
        events.iter().any(|e| matches!(
            &e.kind,
            EventKind::FeatureFailed { feature_id, reason }
                if feature_id == "f-1-2" && reason.contains("conflicted")
        )),
        "no feature.failed(f-1-2) with a conflicted-merge reason — the merge conflict never happened",
    );
    ctx.ensure(
        events.iter().any(|e| {
            matches!(
                &e.kind,
                EventKind::FixFeatureCreated { feature, .. } if feature.id == "ms-1-conflict-1"
            )
        }),
        "no fixfeature.created for ms-1-conflict-1 — the resolution feature was never synthesized",
    );

    // f-1-2 is the one feature allowed (expected) to end Failed; the
    // resolution feature and f-1-1 must be Complete (checked in the shared
    // invariant walk).
    assert_invariants(&ctx, &root, &mission_id, &["f-1-2"]);
}

// ---------------------------------------------------------------------------
// The soak driver
// ---------------------------------------------------------------------------

/// Roadmap M3 corruption soak: `KRANZ_SOAK_ITERS` iterations (default 20),
/// cycling CLEAN → CRASH+RESUME → CONFLICT, full invariant set after each.
/// Run via `scripts/soak.sh` (release) or directly:
/// `KRANZ_SOAK_ITERS=3 cargo test -p kranz-engine --test soak_test -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread")]
#[ignore = "corruption-soak harness; run explicitly via scripts/soak.sh"]
async fn soak_parallel_corruption() {
    if !setup() {
        return;
    }
    let iters: usize = std::env::var("KRANZ_SOAK_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);

    let (mut clean, mut crash, mut conflict) = (0usize, 0usize, 0usize);
    for i in 0..iters {
        match i % 3 {
            0 => {
                run_clean_iteration(i).await;
                clean += 1;
                eprintln!("soak iter {}/{iters} [CLEAN] ok", i + 1);
            }
            1 => {
                run_crash_resume_iteration(i).await;
                crash += 1;
                eprintln!("soak iter {}/{iters} [CRASH+RESUME] ok", i + 1);
            }
            _ => {
                run_conflict_iteration(i).await;
                conflict += 1;
                eprintln!("soak iter {}/{iters} [CONFLICT] ok", i + 1);
            }
        }
    }
    eprintln!(
        "soak PASS: {iters} iterations, zero event-log corruption \
         ({clean} clean, {crash} crash+resume, {conflict} conflict)"
    );
}
