//! Workspace bootstrap + readiness gate (design D-C/D-H, ticket
//! `.kranz/tickets/workspace-bootstrap-preflight.md`) — gate helpers plus
//! the block/lift policy. The gate's EXECUTION moved under the
//! [`crate::workspace_provider`] seam (ticket `workspace-provider-seam`):
//! `LocalWorktreeProvider::readiness` runs the phases below via
//! [`run_gate_commands`], and the run loop drives provider.provision →
//! provider.readiness (= this gate) → workers. This module remains the
//! single owner of the phase shapes, the decision-summary prefixes the
//! report and workspace endpoint derive outcome lines from, and the
//! milestone block/lift machinery.
//!
//! Behavior (unchanged from the pre-seam gate): when a valid workspace
//! contract exists, the contract's `bootstrap[]` (ordered, stop at first
//! failure) and then `readiness[]` (every check runs; all must pass) run in
//! the mission's execution cwd BEFORE the first worker/validator spawns —
//! never start spend on a half-ready app. Without a contract the gate is a
//! no-op and behavior is byte-identical to before (the seam still records
//! `workspace.provisioned`; see the provider module).
//!
//! v1 scope notes (deliberate):
//! - **Once per `run()` invocation, idempotent-by-contract.** Setup scripts
//!   are assumed re-runnable; a resume after crash re-runs them. Durable
//!   readiness state (skip-when-already-ready) is a later provider-seam
//!   concern, not v1's.
//! - **Start/pass/fail land on the established `orchestrator.decision`
//!   audit channel** (with per-command results in the detail); failures
//!   block via the existing `milestone.blocked` machinery with owner
//!   `repo-setup`. The provider seam adds the `workspace.*` lifecycle events
//!   alongside (D-E).
//! - **The contract is read from the live BASE branch** (merge.rs's
//!   `live_base_sha` idiom): base-branch-owned in BOTH isolation modes (a
//!   mission branch cannot weaken the contract that gates its own spend —
//!   checkout mode's working tree IS the mission branch mid-run), and a
//!   committed operator fix on the base branch is picked up on resume.
//! - **Blocking is this gate's difference from `preflight.rs`.** The
//!   environment preflight is advisory; readiness is a gate (D-C). A block
//!   this gate emitted is lifted automatically once the gate passes again
//!   (its precondition is gone) — blocks from any other cause keep the
//!   normal operator/orchestrator unblock flow.

use crate::command_exec::run_shell_command_with_code;
use crate::error::{EngineError, Result};
use crate::event_log::EventLog;
use crate::events::{Event, EventKind};
use crate::orchestrator::{first_incomplete, MissionEngine};
use crate::types::{MilestoneStatus, MissionStatus};
use std::collections::HashMap;

/// `orchestrator.decision` summary prefixes the report and the workspace
/// endpoint derive the bootstrap/readiness outcome lines from (a later
/// run's outcome supersedes an earlier one, mirroring the preflight line).
pub const BOOTSTRAP_SUMMARY_PREFIX: &str = "workspace bootstrap:";
pub const READINESS_SUMMARY_PREFIX: &str = "workspace readiness:";

/// Every block reason this gate emits starts here; the pass path lifts
/// gate-owned blocks by matching it (a block from ANY other cause is left
/// to the normal unblock flow). `pub(crate)` so the golden-data skew/reset
/// reasons (workspace_data.rs) provably share the prefix the lift matches.
pub(crate) const GATE_REASON_PREFIX: &str = "workspace gate:";

/// Outcome of one bootstrap command / readiness check.
#[derive(Debug, Clone)]
pub struct CommandOutcome {
    /// 1-based position within its contract list (for "2/3" reporting).
    pub(crate) ordinal: usize,
    pub(crate) total: usize,
    pub(crate) command: String,
    /// `None` when the command never produced an exit code (spawn failure,
    /// the timeout/group-kill path, or signal termination — the output tail
    /// then says which).
    pub(crate) code: Option<i32>,
    pub(crate) output_tail: String,
}

impl CommandOutcome {
    pub(crate) fn ok(&self) -> bool {
        self.code == Some(0)
    }

    pub(crate) fn exit_phrase(&self) -> String {
        match self.code {
            Some(code) => format!("exit code {code}"),
            None => "no exit code (spawn failure, timeout, or signal)".to_string(),
        }
    }
}

/// One gate phase's static shape (bootstrap or readiness).
pub(crate) struct GatePhase<'a> {
    /// "bootstrap command" / "readiness check" — the block-reason kind.
    pub(crate) kind: &'static str,
    /// "command" / "check" — singular, for "FAILED at {unit} i/n".
    pub(crate) unit: &'static str,
    /// "commands" / "checks" — for "running n {plural}" / "n/n {plural} ok".
    pub(crate) plural: &'static str,
    /// Decision-summary prefix the report/endpoint derive outcomes from.
    pub(crate) prefix: &'static str,
    pub(crate) commands: &'a [String],
    /// bootstrap stops at the first failure; readiness runs every check.
    pub(crate) stop_at_first_failure: bool,
}

impl MissionEngine {
    /// Block the first incomplete milestone on a gate failure and return
    /// `Blocked` (D-C: failures are Blocked, not preflight-warnings).
    pub(crate) fn block_on_gate_failure(
        &mut self,
        kind: &str,
        failed: &CommandOutcome,
    ) -> Result<Option<MissionStatus>> {
        self.block_with_gate_reason(gate_block_reason(kind, failed))
    }

    /// Block the first incomplete milestone with a pre-built gate-owned
    /// reason (the `workspace gate:` prefix is what the pass path's
    /// [`Self::lift_gate_block`] matches). Shared by the bootstrap/readiness
    /// gate and the golden-data hooks (design D-D: skew and reset failures
    /// block with their own actionable reason shapes) — and by provider-owned
    /// failures (`workspace_remote::provider_block_reason`), whose DISTINCT
    /// `workspace provider:` prefix deliberately keeps them OUT of the
    /// gate's auto-lift path.
    ///
    /// When that milestone was never started (a fresh run blocked pre-loop),
    /// start it first so the event stream keeps the invariant that
    /// `milestone.started` precedes any block/unblock cycle — validation
    /// reads `start_sha`, and a later unblock folds the milestone back to
    /// Active, skipping the loop's Pending-only start emit.
    pub(crate) fn block_with_gate_reason(
        &mut self,
        reason: String,
    ) -> Result<Option<MissionStatus>> {
        let Some(mi) = first_incomplete(&self.state) else {
            // Nothing left to block (all milestones complete; only the final
            // gate remained). Still loud and honest: the decisions above are
            // on the log and the run errors instead of spending in a
            // half-ready workspace.
            return Err(EngineError::InvalidState(format!(
                "{reason} — and no incomplete milestone remains to block; \
                 fix the workspace setup (owner: repo-setup) and re-run"
            )));
        };
        if self.state.mission.milestones[mi].status == MilestoneStatus::Pending {
            let start_sha = self.active_repo().head_sha()?;
            let milestone_id = self.state.mission.milestones[mi].id.clone();
            self.emit(EventKind::MilestoneStarted {
                milestone_id,
                start_sha,
            })?;
        }
        let milestone_id = self.state.mission.milestones[mi].id.clone();
        self.emit(EventKind::MilestoneBlocked {
            milestone_id,
            reason,
        })?;
        Ok(Some(MissionStatus::Blocked))
    }

    /// Lift a gate-owned block on the first incomplete milestone now that
    /// the gate passes. Reads the (flushed) event log rather than ephemeral
    /// memory, so it works across process restarts; a block from any other
    /// cause (validation, grants, …) is left to the normal operator flow.
    pub(crate) fn lift_gate_block(&mut self) -> Result<()> {
        if self.state.mission.status != MissionStatus::Blocked {
            return Ok(());
        }
        let Some(mi) = first_incomplete(&self.state) else {
            return Ok(());
        };
        if self.state.mission.milestones[mi].status != MilestoneStatus::Blocked {
            return Ok(());
        }
        let milestone_id = self.state.mission.milestones[mi].id.clone();
        self.log.flush()?;
        let events = EventLog::read_events(&self.paths.events_file())?;
        if latest_block_is_gate_owned(&events, &milestone_id) {
            self.emit(EventKind::MilestoneUnblocked {
                milestone_id,
                reason: "workspace gate now passing: bootstrap and readiness ok".to_string(),
                validator_guidance: None,
            })?;
        }
        Ok(())
    }
}

/// Run one phase's command lines in the workspace cwd — bounded,
/// process-tree-killed, output-tailed (the shared `command_exec` runner
/// used by validation-contract commands).
pub(crate) async fn run_gate_commands(
    cwd: &std::path::Path,
    phase: &GatePhase<'_>,
    env: &HashMap<String, String>,
) -> Vec<CommandOutcome> {
    let total = phase.commands.len();
    let mut outcomes = Vec::with_capacity(total);
    for (i, command) in phase.commands.iter().enumerate() {
        let (code, output_tail) = run_shell_command_with_code(cwd, command, env).await;
        let outcome = CommandOutcome {
            ordinal: i + 1,
            total,
            command: command.clone(),
            code,
            output_tail,
        };
        let failed = !outcome.ok();
        outcomes.push(outcome);
        if failed && phase.stop_at_first_failure {
            break;
        }
    }
    outcomes
}

/// Per-command lines for the decision's `detail` (the audit trail): one
/// status line per command that ran, plus the failing command's output
/// tail. Bounded — tails are already capped by the runner.
pub(crate) fn outcomes_detail(kind: &str, outcomes: &[CommandOutcome]) -> String {
    use std::fmt::Write as _;
    let mut detail = String::new();
    for o in outcomes {
        let verdict = if o.ok() { "ok" } else { "FAILED" };
        let _ = writeln!(
            detail,
            "{kind} {}/{} `{}` → {verdict} ({})",
            o.ordinal,
            o.total,
            o.command,
            o.exit_phrase()
        );
    }
    if let Some(failed) = outcomes.iter().find(|o| !o.ok()) {
        let tail = failed.output_tail.trim();
        if !tail.is_empty() {
            let _ = write!(detail, "\noutput tail:\n{tail}");
        }
    }
    detail
}

/// The `milestone.blocked` reason for a failed command/check: names the
/// failing command, its ordinal, its exit code, the repo-setup owner, and a
/// scrubbed, bounded output tail. Credential-scrubbed here AND again at
/// event-append (defense in depth) — a bootstrap log line must never put a
/// registry token into events.jsonl.
pub(crate) fn gate_block_reason(kind: &str, failed: &CommandOutcome) -> String {
    crate::scrub::scrub(&format!(
        "{GATE_REASON_PREFIX} {kind} {}/{} failed (owner: repo-setup): `{}` {}: {}",
        failed.ordinal,
        failed.total,
        failed.command,
        failed.exit_phrase(),
        failed.output_tail.trim(),
    ))
}

/// Whether `milestone_id`'s LATEST block/unblock event is a workspace-gate
/// block not yet lifted — the pass path unblocks exactly those.
fn latest_block_is_gate_owned(events: &[Event], milestone_id: &str) -> bool {
    events.iter().rev().find_map(|event| match &event.kind {
        EventKind::MilestoneBlocked {
            milestone_id: id,
            reason,
        } if id == milestone_id => Some(reason.starts_with(GATE_REASON_PREFIX)),
        EventKind::MilestoneUnblocked {
            milestone_id: id, ..
        } if id == milestone_id => Some(false),
        _ => None,
    }) == Some(true)
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(
        ordinal: usize,
        total: usize,
        command: &str,
        code: Option<i32>,
        tail: &str,
    ) -> CommandOutcome {
        CommandOutcome {
            ordinal,
            total,
            command: command.to_string(),
            code,
            output_tail: tail.to_string(),
        }
    }

    fn ev(seq: u64, kind: EventKind) -> Event {
        Event {
            seq,
            ts: chrono::Utc::now(),
            mission_id: "m-test".to_string(),
            kind,
        }
    }

    fn blocked(seq: u64, milestone_id: &str, reason: &str) -> Event {
        ev(
            seq,
            EventKind::MilestoneBlocked {
                milestone_id: milestone_id.to_string(),
                reason: reason.to_string(),
            },
        )
    }

    #[test]
    fn gate_block_reason_names_command_exit_owner_and_scrubs_the_tail() {
        let failed = outcome(
            2,
            3,
            "npm ci",
            Some(42),
            "registry auth token sk-ant-api03-a1b2c3d4e5f6 failed",
        );
        let reason = gate_block_reason("bootstrap command", &failed);
        assert!(reason.starts_with("workspace gate:"), "{reason}");
        assert!(reason.contains("bootstrap command 2/3 failed"), "{reason}");
        assert!(reason.contains("owner: repo-setup"), "{reason}");
        assert!(reason.contains("`npm ci`"), "{reason}");
        assert!(reason.contains("exit code 42"), "{reason}");
        assert!(
            !reason.contains("sk-ant-api03-a1b2c3d4e5f6"),
            "the output tail must be scrubbed: {reason}"
        );
        assert!(reason.contains("[REDACTED]"), "{reason}");
    }

    #[test]
    fn gate_block_reason_without_exit_code_says_so() {
        let failed = outcome(1, 1, "./setup.sh", None, "timed out after 600s");
        let reason = gate_block_reason("readiness check", &failed);
        assert!(reason.contains("readiness check 1/1 failed"), "{reason}");
        assert!(reason.contains("no exit code"), "{reason}");
        assert!(reason.contains("timed out after 600s"), "{reason}");
    }

    #[test]
    fn outcomes_detail_lists_every_command_that_ran_plus_the_failing_tail() {
        let outcomes = vec![
            outcome(1, 3, "cargo fetch", Some(0), ""),
            outcome(2, 3, "npm ci", Some(1), "npm ERR! 401"),
        ];
        let detail = outcomes_detail("bootstrap command", &outcomes);
        assert!(
            detail.contains("bootstrap command 1/3 `cargo fetch` → ok (exit code 0)"),
            "{detail}"
        );
        assert!(
            detail.contains("bootstrap command 2/3 `npm ci` → FAILED (exit code 1)"),
            "{detail}"
        );
        assert!(detail.contains("output tail:\nnpm ERR! 401"), "{detail}");
        // Stop-at-first-failure: command 3 never ran, so it is not listed.
        assert!(!detail.contains("3/3"), "{detail}");
    }

    #[test]
    fn latest_block_is_gate_owned_only_for_an_unlifted_gate_block() {
        let gate_reason = "workspace gate: bootstrap command 1/1 failed (owner: repo-setup): `x` exit code 1: boom";
        let other_reason = "validator command denied: `rm -rf /` — deny-default";

        // Gate block, never lifted ⇒ owned.
        let events = vec![blocked(1, "ms-1", gate_reason)];
        assert!(latest_block_is_gate_owned(&events, "ms-1"));

        // Gate block later unblocked ⇒ no longer owned.
        let events = vec![
            blocked(1, "ms-1", gate_reason),
            ev(
                2,
                EventKind::MilestoneUnblocked {
                    milestone_id: "ms-1".to_string(),
                    reason: "workspace gate now passing".to_string(),
                    validator_guidance: None,
                },
            ),
        ];
        assert!(!latest_block_is_gate_owned(&events, "ms-1"));

        // A block from another cause is never gate-owned (stays on the
        // normal operator/orchestrator unblock flow).
        let events = vec![blocked(1, "ms-1", other_reason)];
        assert!(!latest_block_is_gate_owned(&events, "ms-1"));

        // Blocks on OTHER milestones do not count; no block at all either.
        let events = vec![blocked(1, "ms-2", gate_reason)];
        assert!(!latest_block_is_gate_owned(&events, "ms-1"));
        assert!(!latest_block_is_gate_owned(&[], "ms-1"));

        // Latest event wins: gate block → unblock → non-gate block ⇒ not owned.
        let events = vec![
            blocked(1, "ms-1", gate_reason),
            ev(
                2,
                EventKind::MilestoneUnblocked {
                    milestone_id: "ms-1".to_string(),
                    reason: "workspace gate now passing".to_string(),
                    validator_guidance: None,
                },
            ),
            blocked(3, "ms-1", other_reason),
        ];
        assert!(!latest_block_is_gate_owned(&events, "ms-1"));
    }
}
