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
use kranz_engine::control;
use kranz_engine::orchestrator::{MissionEngine, PlanRequest};
use kranz_engine::ticket::Ticket;
use kranz_engine::types::{ControlCommand, MissionStatus};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Exit code exec fails with when the plan file is underspecified: the
/// orchestrator wanted clarification it cannot get headlessly (`NotReady`).
pub const EXIT_UNDERSPECIFIED: i32 = 3;

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

    // Cloud handoff: on a COMPLETE run, push the mission's kranz/* branch to the
    // requested remote so a human reviews it and opens the PR. GitRepo enforces
    // the kranz/* guard — this never pushes main or force-pushes. Failure to
    // push is surfaced but does not change the mission's own exit code (the work
    // is done and committed locally; the push is a delivery step).
    let mut pushed = false;
    if let (Some(remote), MissionStatus::Complete) = (&push, status) {
        match kranz_engine::git_ops::GitRepo::open(&repo)
            .and_then(|r| r.push_mission_branch(remote, &branch))
        {
            Ok(()) => {
                pushed = true;
                eprintln!("kranz exec: pushed {branch} to {remote}");
            }
            Err(e) => eprintln!("kranz exec: WARNING failed to push {branch} to {remote}: {e}"),
        }
    }

    // The only line on stdout: machine-readable, one line, always emitted.
    println!(
        "kranz exec {mission_id} {} cost=${cost:.2} branch={branch} pushed={pushed}",
        output::mission_status_label(status)
    );
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_code_maps_terminal_statuses() {
        assert_eq!(exit_code_for(MissionStatus::Complete), 0);
        assert_eq!(exit_code_for(MissionStatus::Failed), 1);
        assert_eq!(exit_code_for(MissionStatus::Blocked), 2);
        // Non-terminal statuses (should not arise from run()) map to failure.
        assert_eq!(exit_code_for(MissionStatus::Running), 1);
        assert_eq!(exit_code_for(MissionStatus::Abandoned), 1);
    }
}
