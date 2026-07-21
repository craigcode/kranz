//! `kranz exec -f <mission.md>` — fully headless missions for CI (roadmap M5).
//!
//! Plan file in, exit code out. There is no human on the other end: the
//! mission.md is a ticket-shaped markdown that must be self-sufficient, and
//! approval is automatic. The one place a headless run can't proceed is when
//! the orchestrator answers `request_plan` with clarifying questions instead
//! of a plan — that means the plan file was underspecified, so exec fails with
//! a distinct exit code (3) and prints the questions to stderr for the CI log.
//!
//! Flow (mirrors `cmd_draft`'s non-interactive seed→plan path, but then runs
//! the mission instead of parking it):
//!   parse file → `Ticket::mission_goal()` → build backend →
//!   `MissionEngine::create` → one `planning_turn` seeding the whole ticket →
//!   `request_plan()` → auto-approve (or exit 3) → `run()` to a terminal state.
//!
//! Events stream to stderr live (the same [`tail::tail_events`] feed `kranz
//! run` uses) so CI logs show progress; the only thing on stdout is the final
//! one-line machine-readable summary:
//!   `kranz exec <id> <STATUS> cost=$X.XX branch=<b>`
//!
//! stdin is never read and no TUI is ever opened.

use crate::commands::{augment_limit_hint, build_backend, load_config};
use crate::output;
use crate::tail::{self, EventRenderer};
use anyhow::{Context, Result};
use kranz_engine::backend::AgentBackend;
use kranz_engine::control;
use kranz_engine::orchestrator::{MissionEngine, PlanRequest};
use kranz_engine::ticket::Ticket;
use kranz_engine::types::{ControlCommand, MissionConfig, MissionStatus};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Exit code exec fails with when the plan file is underspecified: the
/// orchestrator wanted clarification it cannot get headlessly (`NotReady`).
pub const EXIT_UNDERSPECIFIED: i32 = 3;

/// Exit code when the mission COMPLETE'd but `--push` failed. Distinct from
/// mission failure (1) and underspecified (3) so CI can tell delivery apart
/// from the run itself. Stdout still reports `pushed=false`.
pub const EXIT_PUSH_FAILED: i32 = 4;

/// Map a terminal mission status to the process exit code exec returns.
///
/// `Complete` → 0, `Failed` → 1, `Blocked` → 2. Any other status is not a
/// terminal outcome of a headless run (the engine only returns Complete /
/// Failed / Blocked from `run()`), so it is treated as a failure (1). The
/// underspecified case ([`EXIT_UNDERSPECIFIED`]) is handled before the run
/// starts and never reaches this function.
pub fn exit_code_for(status: MissionStatus) -> i32 {
    match status {
        MissionStatus::Complete => 0,
        MissionStatus::Blocked => 2,
        MissionStatus::Failed => 1,
        _ => 1,
    }
}

/// The unattended scrutiny floor: `exec` runs with no human present, so a
/// mission whose config disables the scrutiny validator (`skipScrutiny`) has
/// no adversarial reader at all and can satisfy its own acceptance
/// tautologically (docs/gascity.md lesson 3 records exactly this incident).
/// Interactive `run`/`plan` are not gated — a human is present there. Passing
/// `--allow-unvalidated` (or setting `KRANZ_ALLOW_UNVALIDATED=1`) is an
/// explicit, auditable acknowledgment that overrides the floor.
pub fn scrutiny_gate(skip_scrutiny: bool, allow_unvalidated: bool) -> Result<(), String> {
    if skip_scrutiny && !allow_unvalidated {
        Err(
            "kranz exec: refusing to run an unattended mission with skipScrutiny set. \
             A headless run has no adversarial reader when the scrutiny validator is \
             disabled, so the mission can pass its own tautological acceptance (see \
             docs/gascity.md lesson 3). Pass --allow-unvalidated (or set \
             KRANZ_ALLOW_UNVALIDATED=1) to explicitly override this floor."
                .to_string(),
        )
    } else {
        Ok(())
    }
}

/// Parse a ticket-shaped plan file into a [`Ticket`]. The slug is derived from
/// the file stem (like [`Ticket::load`]), so the folded [`Ticket::mission_goal`]
/// carries the goal, scoping answers, acceptance hints, and context verbatim.
///
/// Pure over `(slug, markdown)` so the parse path is unit-tested without touching
/// the filesystem; [`read_mission_file`] is the thin I/O wrapper exec calls.
pub fn parse_mission_markdown(slug: &str, markdown: &str) -> Result<Ticket> {
    Ticket::parse(slug, markdown).with_context(|| format!("parsing mission plan file '{slug}'"))
}

/// Read + parse a plan file from disk. The slug is the file stem; a path with
/// no usable stem falls back to `"mission"`.
fn read_mission_file(path: &Path) -> Result<Ticket> {
    let slug = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("mission");
    let markdown = std::fs::read_to_string(path)
        .with_context(|| format!("reading mission plan file {}", path.display()))?;
    parse_mission_markdown(slug, &markdown)
}

/// `kranz exec -f <mission.md> [--repo <path>] [--yes] [--max-cycles N] [--allow-unvalidated]`.
///
/// `--yes` is accepted for symmetry with the interactive commands but is a
/// no-op: a headless run always auto-approves. `--max-cycles`, when given,
/// overrides `maxFixCyclesPerMilestone` for the run (recorded as a
/// config.changed event via the control inbox) so CI can bound spend.
///
/// Immediately after config loads and before any mission directory is
/// created, [`scrutiny_gate`] enforces the unattended scrutiny floor: see its
/// doc comment for the rationale.
pub async fn cmd_exec(
    repo: PathBuf,
    file: PathBuf,
    _yes: bool,
    max_cycles: Option<u32>,
    push: Option<String>,
    dangerously_allow_all: bool,
    allow_unvalidated: bool,
) -> Result<i32> {
    let ticket = read_mission_file(&file)?;
    let cfg = load_config(&repo, dangerously_allow_all)?;

    let allow_unvalidated =
        allow_unvalidated || std::env::var("KRANZ_ALLOW_UNVALIDATED").ok().as_deref() == Some("1");
    if let Err(msg) = scrutiny_gate(cfg.skip_scrutiny, allow_unvalidated) {
        eprintln!("{msg}");
        return Ok(1);
    }

    let backend = build_backend(&cfg)?;

    cmd_exec_with_backend(repo, file, ticket, max_cycles, push, cfg, backend).await
}

/// The body of [`cmd_exec`], parameterized on the backend so tests can drive
/// it with [`kranz_engine::backend_mock::MockBackend`] instead of discovering
/// a real `claude` binary.
async fn cmd_exec_with_backend(
    repo: PathBuf,
    file: PathBuf,
    ticket: Ticket,
    max_cycles: Option<u32>,
    push: Option<String>,
    cfg: MissionConfig,
    backend: Arc<dyn AgentBackend>,
) -> Result<i32> {
    let goal = ticket.mission_goal();
    let mut engine = MissionEngine::create(backend, repo.clone(), &goal, cfg)?;
    let mission_id = engine.mission_id().to_string();
    eprintln!(
        "kranz exec: mission {mission_id} created from {}",
        file.display()
    );

    // Seed the orchestrator with the whole ticket, then demand the plan — the
    // same single-turn seed the non-interactive draft path uses.
    engine
        .planning_turn(&goal)
        .await
        .map_err(|e| augment_limit_hint(e.into()))
        .with_context(|| format!("seeding the orchestrator for mission {mission_id}"))?;
    if let Some(seed) = engine.take_seed_reply() {
        eprintln!("orchestrator: {}", output::one_line(&seed, 200));
    }

    let request = engine
        .request_plan()
        .await
        .map_err(|e| augment_limit_hint(e.into()))
        .with_context(|| format!("requesting the plan for mission {mission_id}"))?;

    let plan = match request {
        PlanRequest::Ready(plan) => plan,
        PlanRequest::NotReady(questions) => {
            // No human to answer: the plan file was underspecified. Signal CI
            // with a distinct exit code and surface the questions on stderr.
            eprintln!(
                "kranz exec: mission underspecified — the orchestrator needs clarification \
                 that a headless run cannot provide. Answer these in {} and re-run:",
                file.display()
            );
            for line in questions.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    eprintln!("  - {line}");
                }
            }
            println!(
                "kranz exec {mission_id} UNDERSPECIFIED cost=${:.2} branch=-",
                engine.state().total_cost_usd
            );
            return Ok(EXIT_UNDERSPECIFIED);
        }
    };

    // Auto-approve: commits plan.json/plan.md on the mission branch.
    engine
        .approve_plan(plan)
        .with_context(|| format!("approving the plan for mission {mission_id}"))?;
    let branch = engine.state().mission.mission_branch.clone();
    eprintln!("kranz exec: plan approved on {branch}; running headlessly");

    // A --max-cycles override is applied via the control inbox so it lands as a
    // config.changed event the run loop drains (never mutating config out of
    // band). Enqueued before the engine's run() drains the inbox.
    if let Some(n) = max_cycles {
        control::enqueue(
            engine.paths(),
            &ControlCommand::ConfigChange {
                patch: serde_json::json!({ "maxFixCyclesPerMilestone": n }),
            },
        )
        .with_context(|| format!("queuing the --max-cycles override for mission {mission_id}"))?;
    }

    run_and_reconcile(engine, repo, mission_id, branch, push).await
}

/// The tail of [`cmd_exec_with_backend`]: run the (already planned and
/// approved) `engine` to a terminal state, reconcile the linked ticket, then
/// handle the `--push` handoff and print the machine-readable summary line.
///
/// Split out so tests can drive it against an `engine` whose mission id was
/// already used to link a ticket — proving the `reconcile_ticket_for_mission`
/// call actually fires from this code path, not merely that the helper works
/// in isolation.
async fn run_and_reconcile(
    mut engine: MissionEngine,
    repo: PathBuf,
    mission_id: String,
    branch: String,
    push: Option<String>,
) -> Result<i32> {
    // Live stderr feed for CI logs: tail events.jsonl from the pre-run head seq.
    let color = std::io::stderr().is_terminal();
    let renderer = EventRenderer::seeded(engine.state(), color);
    let stop = Arc::new(AtomicBool::new(false));
    let printer = tokio::spawn(tail::tail_events(
        engine.paths().events_file(),
        engine.state().last_seq,
        renderer,
        Arc::clone(&stop),
    ));

    let run_result = engine.run().await;
    // Read the final cost off state before dropping the engine, then drop it
    // (flushes buffered deltas + releases the lock) so the printer's catch-up
    // read sees every event.
    let cost = engine.state().total_cost_usd;
    drop(engine);
    stop.store(true, Ordering::Relaxed);
    let _ = printer.await;

    let status = run_result.map_err(|e| augment_limit_hint(e.into()))?;
    let code = exit_code_for(status);

    // Reconcile the linked ticket's .status sidecar to match the mission's
    // terminal/blocked status. Non-fatal: a reconcile failure must never
    // change the exit code or the push behaviour below.
    if let Err(e) = kranz_engine::work::reconcile_ticket_for_mission(&repo, &mission_id) {
        eprintln!("kranz exec: warning: failed to reconcile linked ticket: {e}");
    }

    // Cloud handoff: on a COMPLETE run, push the mission's kranz/* branch to the
    // requested remote so a human reviews it and opens the PR. GitRepo enforces
    // the kranz/* guard — this never pushes main or force-pushes. A push failure
    // keeps stdout `pushed=false` and returns [`EXIT_PUSH_FAILED`] (distinct
    // from the mission's own exit code) so CI can detect a delivery miss.
    let mut pushed = false;
    let mut push_failed = false;
    if let (Some(remote), MissionStatus::Complete) = (&push, status) {
        match kranz_engine::git_ops::GitRepo::open(&repo)
            .and_then(|r| r.push_mission_branch(remote, &branch))
        {
            Ok(()) => {
                pushed = true;
                eprintln!("kranz exec: pushed {branch} to {remote}");
            }
            Err(e) => {
                push_failed = true;
                eprintln!("kranz exec: WARNING failed to push {branch} to {remote}: {e}");
            }
        }
    }

    // The only line on stdout: machine-readable, one line, always emitted.
    println!(
        "kranz exec {mission_id} {} cost=${cost:.2} branch={branch} pushed={pushed}",
        output::mission_status_label(status)
    );
    if push_failed {
        return Ok(EXIT_PUSH_FAILED);
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kranz_engine::types::MissionConfig;

    #[test]
    fn exit_code_maps_terminal_statuses() {
        assert_eq!(exit_code_for(MissionStatus::Complete), 0);
        assert_eq!(exit_code_for(MissionStatus::Failed), 1);
        assert_eq!(exit_code_for(MissionStatus::Blocked), 2);
        // Non-terminal statuses (should not arise from run()) map to failure.
        assert_eq!(exit_code_for(MissionStatus::Running), 1);
        assert_eq!(exit_code_for(MissionStatus::Abandoned), 1);
    }

    #[test]
    fn push_failure_exit_code_is_distinct() {
        // Mission COMPLETE → 0; push failure must not reuse that (or 1/2/3).
        assert_eq!(exit_code_for(MissionStatus::Complete), 0);
        assert_eq!(EXIT_PUSH_FAILED, 4);
        assert_ne!(EXIT_PUSH_FAILED, exit_code_for(MissionStatus::Complete));
        assert_ne!(EXIT_PUSH_FAILED, exit_code_for(MissionStatus::Failed));
        assert_ne!(EXIT_PUSH_FAILED, EXIT_UNDERSPECIFIED);
    }

    /// Pure helper mirroring the post-run push decision in [`cmd_exec`]: when
    /// `--push` is set and the push Errs after COMPLETE, the process exit is
    /// [`EXIT_PUSH_FAILED`] while the summary still reports `pushed=false`.
    fn exit_after_push(mission_code: i32, push_requested: bool, push_ok: bool) -> (i32, bool) {
        let mut pushed = false;
        let mut push_failed = false;
        if push_requested {
            if push_ok {
                pushed = true;
            } else {
                push_failed = true;
            }
        }
        let code = if push_failed {
            EXIT_PUSH_FAILED
        } else {
            mission_code
        };
        (code, pushed)
    }

    #[test]
    fn push_failure_returns_exit_4_with_pushed_false() {
        let (code, pushed) = exit_after_push(0, true, false);
        assert_eq!(code, EXIT_PUSH_FAILED);
        assert!(!pushed);
    }

    #[test]
    fn push_success_keeps_mission_exit_and_pushed_true() {
        let (code, pushed) = exit_after_push(0, true, true);
        assert_eq!(code, 0);
        assert!(pushed);
    }

    #[test]
    fn no_push_flag_leaves_mission_exit_unchanged() {
        let (code, pushed) = exit_after_push(0, false, false);
        assert_eq!(code, 0);
        assert!(!pushed);
    }

    // -----------------------------------------------------------------------
    // reconcile-on-terminal: `kranz exec`'s post-run step heals a linked ticket
    // -----------------------------------------------------------------------

    fn reconcile_turn(reply: &str) -> Vec<kranz_engine::backend::AgentEvent> {
        vec![
            kranz_engine::backend_mock::mock_text(reply),
            kranz_engine::backend_mock::mock_result_text(reply),
        ]
    }

    fn reconcile_worker_pass() -> kranz_engine::backend_mock::MockScript {
        kranz_engine::backend_mock::MockScript::single_shot_json(&serde_json::json!({
            "result": "pass",
            "summary": "implemented and tested",
            "filesTouched": ["delivered.txt"],
            "testsAdded": [],
            "testEvidence": "all green",
            "commits": []
        }))
        .writes_file("delivered.txt", "delivered by the mock worker\n")
    }

    fn reconcile_plan_json() -> serde_json::Value {
        serde_json::json!({
            "goal": "ship the demo",
            "validationContract": [],
            "milestones": [{
                "title": "M1",
                "features": [{
                    "title": "F1",
                    "spec": "build the thing",
                    "validationCriteria": ["it works"]
                }]
            }]
        })
    }

    /// `run_and_reconcile`'s post-run reconcile call must heal the linked
    /// ticket's stale `.status` sidecar once the headless mission reaches
    /// Complete — proving the f-1-3 wiring in `exec.rs` (not just the
    /// engine-level helper unit tests). Links the ticket to the mission and
    /// seeds it at Failed (a stale mismatch) before calling the function
    /// under test, so the assertion only passes if the reconcile call
    /// actually ran, not merely if the ticket happened to already be Done.
    /// Fails if the `reconcile_ticket_for_mission` call is removed from
    /// `run_and_reconcile`.
    #[tokio::test]
    async fn reconcile_on_terminal_after_cli_exec_marks_ticket_done() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        let status = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        std::process::Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(&repo)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&repo)
            .status()
            .unwrap();
        std::fs::write(repo.join("README.md"), "seed\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&repo)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(&repo)
            .status()
            .unwrap();
        let repo = std::fs::canonicalize(&repo).unwrap();

        let judgement = serde_json::json!({
            "decision": "complete",
            "guidance": "",
            "summary": "worker did the job"
        });
        // A single continuous orchestrator session: `cmd_exec_with_backend`
        // never drops/resumes the engine between planning and run, unlike
        // `kranz run`'s loop.
        let orch = kranz_engine::backend_mock::MockScript::streaming(vec![
            kranz_engine::backend_mock::mock_init("orch-session"),
            kranz_engine::backend_mock::mock_result_text("seed-hi"),
        ])
        .responding(vec![
            reconcile_turn("let's scope the demo"),
            reconcile_turn(&reconcile_plan_json().to_string()),
            reconcile_turn("ack"),
            reconcile_turn(&serde_json::json!({"action": "commit-as-is", "note": "worker delivered files"}).to_string()),
            reconcile_turn(&judgement.to_string()),
            reconcile_turn("NONE"),
        ]);
        let backend: Arc<dyn AgentBackend> = Arc::new(
            kranz_engine::backend_mock::MockBackend::with_scripts(vec![
                orch,
                reconcile_worker_pass(),
            ]),
        );

        let cfg = MissionConfig {
            skip_scrutiny: true,
            skip_functional: true,
            ..Default::default()
        };
        let mut engine =
            MissionEngine::create(Arc::clone(&backend), repo.clone(), "ship the demo", cfg)
                .unwrap();
        let mission_id = engine.mission_id().to_string();
        engine.planning_turn("ship the demo").await.unwrap();
        let request = engine.request_plan().await.unwrap();
        let plan = match request {
            PlanRequest::Ready(plan) => plan,
            PlanRequest::NotReady(text) => panic!("expected a ready plan, got: {text}"),
        };
        engine.approve_plan(plan).unwrap();
        let branch = engine.state().mission.mission_branch.clone();

        // Link a ticket to this mission and stamp it Failed — a stale
        // mismatch the drove-to-Complete run must heal.
        kranz_engine::ticket::Ticket::record_mission(&repo, "my-ticket", &mission_id).unwrap();
        kranz_engine::ticket::Ticket::write_state(
            &repo,
            "my-ticket",
            kranz_engine::ticket::TicketState::Failed,
            None,
        )
        .unwrap();

        let exit_code = run_and_reconcile(engine, repo.clone(), mission_id, branch, None)
            .await
            .unwrap();
        assert_eq!(exit_code, 0);

        assert_eq!(
            kranz_engine::ticket::Ticket::read_state(&repo, "my-ticket"),
            kranz_engine::ticket::TicketState::Done,
            "run_and_reconcile must reconcile the linked ticket to Done on Complete"
        );
    }
}
