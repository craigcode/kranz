//! Golden-data hooks (design D-D in `docs/scoping/workspace-contract.md`,
//! ticket `.kranz/tickets/golden-data-hooks.md`) — the optional `data` block's
//! `clone` / `migrate` / `reset` / `skewCheck` commands that provision a
//! de-identified golden dataset into the workspace before agents run.
//!
//! This module owns the hook vocabulary and the outcome shapes; EXECUTION
//! stays with the [`crate::workspace_provider`] seam (host shell for
//! local-worktree, `compose exec` for the container provider — a container
//! workspace never runs data hooks on the host), and the reset-between-rounds
//! drive lives here on [`MissionEngine`] because it fires from
//! `validation_round`, outside the provision/readiness drive.
//!
//! Lifecycle (all steps skip silently when the contract's `data` block does
//! not declare them — repos without a `data` block are byte-identical):
//!
//! ```text
//! provision → data clone → data migrate → bootstrap → readiness → data skewCheck
//!                                                                        │failure
//!                                              Blocked, owner repo-setup ◀─┘
//!                                              (the SKEW case: the reason
//!                                               names the migrate/reset
//!                                               hook to run, then resume)
//! validation_round: data reset (when resetBetweenRounds opts in) → validators
//! ```
//!
//! Owned outcomes, never flake (D-D): a skewCheck failure is
//! [`crate::workspace_provider::ReadinessOutcome::DataSkew`] — a distinct
//! outcome that Blocks with the migrate/reset action named, never a generic
//! readiness failure and never a validator finding. Clone/migrate failures
//! fold into the gate's established block shape (kind "data clone hook" /
//! "data migrate hook"). A reset failure Blocks with the same owned shape
//! before any validator spawns. All block reasons carry the
//! [`crate::workspace_gate::GATE_REASON_PREFIX`] so a fixed environment
//! lifts them on resume like any other gate block.
//!
//! Decision events reuse the `orchestrator.decision` audit channel with a
//! [`DATA_SUMMARY_PREFIX`] summary (no new event kinds): hook name, command,
//! exit, and a scrubbed bounded tail in the detail — mirroring the bootstrap
//! gate's decision shape. Secret *values* never appear: the contract carries
//! names only, hooks run with the gate's env discipline (inherited env plus
//! the handle's `KRANZ_BASE_SHA` — the existing machinery, no new secrets
//! channel), and tails are scrubbed here AND again at event-append (defense
//! in depth).

use crate::error::Result;
use crate::orchestrator::MissionEngine;
use crate::workspace_contract::DataHooks;
use crate::workspace_gate::{CommandOutcome, GATE_REASON_PREFIX};
use crate::workspace_provider::ProgressSink;

/// `orchestrator.decision` summary prefix for every data-hook step
/// (`workspace data: clone ...` / `... migrate ...` / `... reset ...` /
/// `... skewCheck ...`).
pub const DATA_SUMMARY_PREFIX: &str = "workspace data:";

/// The four golden-data hooks, in lifecycle vocabulary. Wire/decision names
/// match the contract's camelCase fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataHookKind {
    Clone,
    Migrate,
    Reset,
    SkewCheck,
}

impl DataHookKind {
    /// The decision-line name (`workspace data: {name} ...`).
    pub fn as_str(self) -> &'static str {
        match self {
            DataHookKind::Clone => "clone",
            DataHookKind::Migrate => "migrate",
            DataHookKind::Reset => "reset",
            DataHookKind::SkewCheck => "skewCheck",
        }
    }

    /// The block-reason kind for a hook failure that folds into the gate's
    /// established reason shape ("{kind} 1/1 failed"). Skew/reset failures
    /// use their own reason builders below instead.
    pub fn gate_kind(self) -> &'static str {
        match self {
            DataHookKind::Clone => "data clone hook",
            DataHookKind::Migrate => "data migrate hook",
            DataHookKind::Reset => "data reset hook",
            DataHookKind::SkewCheck => "data skewCheck hook",
        }
    }
}

/// Build the [`CommandOutcome`] for one executed data hook and report its
/// decision line — the shared tail of every provider's `run_data_hook` (the
/// providers differ only in HOW the command runs: host shell vs
/// `compose exec`). Returns the outcome only when the hook FAILED, mirroring
/// `report_gate_outcomes`.
pub(crate) fn hook_outcome(
    hook: DataHookKind,
    command: &str,
    code: Option<i32>,
    output_tail: String,
    progress: &mut ProgressSink<'_>,
) -> Result<Option<CommandOutcome>> {
    let outcome = CommandOutcome {
        ordinal: 1,
        total: 1,
        command: command.to_string(),
        code,
        output_tail,
    };
    report_hook_outcome(hook, &outcome, progress)?;
    Ok((!outcome.ok()).then_some(outcome))
}

/// The pass/fail decision line for one data hook — one line per hook (hooks
/// are single commands, so the gate's "running n / n/n ok" pair collapses):
/// `workspace data: {hook} `{command}` → ok|FAILED ({exit phrase})`, with the
/// scrubbed bounded tail as detail when non-empty (scrubbed again by
/// `emit_decision`, same as the gate's details).
fn report_hook_outcome(
    hook: DataHookKind,
    outcome: &CommandOutcome,
    progress: &mut ProgressSink<'_>,
) -> Result<()> {
    let summary = if outcome.ok() {
        format!(
            "{DATA_SUMMARY_PREFIX} {} `{}` → ok ({})",
            hook.as_str(),
            outcome.command,
            outcome.exit_phrase()
        )
    } else {
        format!(
            "{DATA_SUMMARY_PREFIX} {} `{}` → FAILED ({}) — blocking mission (owner: repo-setup)",
            hook.as_str(),
            outcome.command,
            outcome.exit_phrase()
        )
    };
    let tail = outcome.output_tail.trim();
    let detail = (!tail.is_empty()).then(|| tail.to_string());
    progress(&summary, detail)
}

/// The skew Block reason (D-D): names the data block's skewCheck, its exit,
/// a scrubbed bounded tail, the repo-setup owner, AND the action — run the
/// declared migrate/reset hook (named), then resume. Carries the gate
/// prefix so a fixed environment lifts the block on resume
/// ([`GATE_REASON_PREFIX`]); it is a distinct outcome, never a readiness
/// flake's generic reason.
pub(crate) fn skew_block_reason(data: Option<&DataHooks>, failed: &CommandOutcome) -> String {
    let action = match (data.and_then(|d| d.migrate.as_ref()), data.and_then(|d| d.reset.as_ref())) {
        (Some(migrate), Some(reset)) => format!(
            "run the data migrate hook (`{migrate}`) or the data reset hook (`{reset}`), then resume"
        ),
        (Some(migrate), None) => {
            format!("run the data migrate hook (`{migrate}`), then resume")
        }
        (None, Some(reset)) => format!("run the data reset hook (`{reset}`), then resume"),
        // Unreachable: contract validation refuses skewCheck without a
        // migrate or reset hook. Defensive only — stay actionable anyway.
        (None, None) => "fix the data skew, then resume".to_string(),
    };
    crate::scrub::scrub(&format!(
        "{GATE_REASON_PREFIX} data skewCheck failed (owner: repo-setup): `{}` {}: {} — {action}",
        failed.command,
        failed.exit_phrase(),
        failed.output_tail.trim(),
    ))
}

/// The reset-failure Block reason — the same owned shape as skew (hook,
/// exit, scrubbed bounded tail, owner, action). A failed reset means the
/// re-seed itself is broken, so the action is to fix the hook or the
/// dataset it restores; the gate prefix keeps the block liftable on resume.
pub(crate) fn reset_block_reason(failed: &CommandOutcome) -> String {
    crate::scrub::scrub(&format!(
        "{GATE_REASON_PREFIX} data reset hook failed (owner: repo-setup): `{}` {}: {} — \
         fix the data reset hook or the dataset it restores, then resume",
        failed.command,
        failed.exit_phrase(),
        failed.output_tail.trim(),
    ))
}

impl MissionEngine {
    /// The reset-between-rounds drive (design D-D): when the provisioned
    /// workspace's contract data block opts in (`resetBetweenRounds`) and
    /// declares a `reset` hook, re-seed the golden dataset BEFORE the
    /// validation round's first validator spawn, so every round judges the
    /// same baseline. Returns `Ok(true)` when a reset failure Blocked the
    /// first incomplete milestone (same owned shape as skew — never a
    /// validator finding); the caller returns early and the loop's blocked
    /// branch parks the mission. `Ok(false)` when no reset ran (no handle,
    /// no contract, flag off, or hook undeclared — byte-identical behavior)
    /// or the reset passed.
    ///
    /// Execution routes through the resolved provider's `run_data_hook`, so
    /// a container workspace re-seeds INSIDE the container, never on the
    /// host. Idempotent-by-contract (same discipline as bootstrap): a resume
    /// re-runs the hook on the next validation round.
    pub(crate) async fn run_data_reset_between_rounds(&mut self) -> Result<bool> {
        let reset = self
            .workspace_handle
            .as_ref()
            .and_then(|handle| handle.contract.as_ref())
            .and_then(|contract| contract.data.as_ref())
            .filter(|data| data.reset_between_rounds)
            .and_then(|data| data.reset.clone());
        let Some(command) = reset else {
            return Ok(false);
        };
        // Both cloned/Arc-shared out of `self` so the progress closure can
        // emit decisions inline (same WHEN discipline as the gate).
        let Some(provider) = self.workspace_provider.clone() else {
            // Defensive: validation rounds outside run() (unit tests) carry
            // no provider — treat as no reset.
            return Ok(false);
        };
        let handle = self
            .workspace_handle
            .clone()
            .expect("a reset hook implies a provisioned handle");
        let failed = {
            let mut progress = |summary: &str, detail: Option<String>| -> Result<()> {
                self.emit_decision(summary, detail)
            };
            provider
                .run_data_hook(&handle, DataHookKind::Reset, &command, &mut progress)
                .await?
        };
        match failed {
            None => Ok(false),
            Some(failed) => {
                self.block_with_gate_reason(reset_block_reason(&failed))?;
                Ok(true)
            }
        }
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_contract::parse_workspace_contract;

    fn outcome(command: &str, code: Option<i32>, tail: &str) -> CommandOutcome {
        CommandOutcome {
            ordinal: 1,
            total: 1,
            command: command.to_string(),
            code,
            output_tail: tail.to_string(),
        }
    }

    fn data(json: &[u8]) -> DataHooks {
        parse_workspace_contract(json)
            .expect("valid contract")
            .data
            .expect("data hooks")
    }

    /// The decision line mirrors the gate's shape: hook name, command, exit,
    /// and the tail as detail; failures say who owns the block.
    #[test]
    fn data_hook_decision_lines_carry_command_exit_and_tail() {
        let mut lines: Vec<(String, Option<String>)> = Vec::new();
        let mut sink = |summary: &str, detail: Option<String>| -> Result<()> {
            lines.push((summary.to_string(), detail));
            Ok(())
        };

        let passed = hook_outcome(
            DataHookKind::Clone,
            "pg_dump golden | psql workspace",
            Some(0),
            "100 rows copied".to_string(),
            &mut sink,
        )
        .expect("report");
        assert!(passed.is_none(), "a passing hook reports and returns None");
        let failed = hook_outcome(
            DataHookKind::Migrate,
            "sqlx migrate run",
            Some(1),
            "relation already exists".to_string(),
            &mut sink,
        )
        .expect("report");
        assert_eq!(
            failed.expect("a failing hook returns its outcome").code,
            Some(1)
        );

        assert_eq!(
            lines,
            vec![
                (
                    "workspace data: clone `pg_dump golden | psql workspace` → ok (exit code 0)"
                        .to_string(),
                    Some("100 rows copied".to_string()),
                ),
                (
                    "workspace data: migrate `sqlx migrate run` → FAILED (exit code 1) — blocking mission (owner: repo-setup)"
                        .to_string(),
                    Some("relation already exists".to_string()),
                ),
            ]
        );

        // An empty tail carries no detail line.
        let mut lines: Vec<(String, Option<String>)> = Vec::new();
        let mut sink = |summary: &str, detail: Option<String>| -> Result<()> {
            lines.push((summary.to_string(), detail));
            Ok(())
        };
        hook_outcome(
            DataHookKind::Reset,
            "seed",
            Some(0),
            String::new(),
            &mut sink,
        )
        .expect("report");
        assert_eq!(lines[0].1, None, "no tail ⇒ no detail: {lines:?}");
    }

    /// The skew reason is the distinct, owned, actionable outcome (D-D): the
    /// gate prefix (so resume lifts it), the skewCheck named with its exit,
    /// a scrubbed tail, the repo-setup owner, and the migrate/reset action
    /// naming whichever hooks are declared.
    #[test]
    fn skew_block_reason_is_owned_actionable_and_scrubbed() {
        let hooks = data(
            br#"{"schemaVersion": 1, "data": {
                "migrate": "sqlx migrate run",
                "reset": "reseed",
                "skewCheck": "sqlx migrate info --check"
            }}"#,
        );
        let failed = outcome(
            "sqlx migrate info --check",
            Some(1),
            "token sk-ant-api03-a1b2c3d4e5f6 rejected",
        );
        let reason = skew_block_reason(Some(&hooks), &failed);
        assert!(reason.starts_with("workspace gate:"), "{reason}");
        assert!(reason.contains("data skewCheck failed"), "{reason}");
        assert!(reason.contains("owner: repo-setup"), "{reason}");
        assert!(reason.contains("`sqlx migrate info --check`"), "{reason}");
        assert!(reason.contains("exit code 1"), "{reason}");
        assert!(
            reason.contains("run the data migrate hook (`sqlx migrate run`) or the data reset hook (`reseed`), then resume"),
            "the action names both declared hooks: {reason}"
        );
        assert!(
            !reason.contains("sk-ant-api03-a1b2c3d4e5f6"),
            "the tail is scrubbed: {reason}"
        );
        assert!(reason.contains("[REDACTED]"), "{reason}");

        // Whichever single hook is declared is the one the action names
        // (validation guarantees at least one).
        let migrate_only =
            data(br#"{"schemaVersion": 1, "data": {"migrate": "m", "skewCheck": "c"}}"#);
        let reason = skew_block_reason(Some(&migrate_only), &failed);
        assert!(
            reason.contains("run the data migrate hook (`m`), then resume"),
            "{reason}"
        );
        assert!(!reason.contains("reset hook"), "{reason}");

        let reset_only = data(br#"{"schemaVersion": 1, "data": {"reset": "r", "skewCheck": "c"}}"#);
        let reason = skew_block_reason(Some(&reset_only), &failed);
        assert!(
            reason.contains("run the data reset hook (`r`), then resume"),
            "{reason}"
        );
    }

    /// The reset-failure reason carries the same owned shape (gate prefix,
    /// hook, exit, scrubbed tail, owner, action).
    #[test]
    fn reset_block_reason_matches_the_owned_shape() {
        let failed = outcome("dropdb workspace", None, "connection refused");
        let reason = reset_block_reason(&failed);
        assert!(reason.starts_with("workspace gate:"), "{reason}");
        assert!(reason.contains("data reset hook failed"), "{reason}");
        assert!(reason.contains("owner: repo-setup"), "{reason}");
        assert!(reason.contains("`dropdb workspace`"), "{reason}");
        assert!(reason.contains("no exit code"), "{reason}");
        assert!(reason.contains("connection refused"), "{reason}");
        assert!(reason.contains("then resume"), "{reason}");
    }

    #[test]
    fn data_hook_kind_names_match_the_contract_fields() {
        assert_eq!(DataHookKind::Clone.as_str(), "clone");
        assert_eq!(DataHookKind::Migrate.as_str(), "migrate");
        assert_eq!(DataHookKind::Reset.as_str(), "reset");
        assert_eq!(DataHookKind::SkewCheck.as_str(), "skewCheck");
        assert_eq!(DataHookKind::Clone.gate_kind(), "data clone hook");
        assert_eq!(DataHookKind::Migrate.gate_kind(), "data migrate hook");
        assert_eq!(DataHookKind::Reset.gate_kind(), "data reset hook");
        assert_eq!(DataHookKind::SkewCheck.gate_kind(), "data skewCheck hook");
    }
}
