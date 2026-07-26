//! The WorkspaceProvider seam (design D-B/D-E in
//! `docs/scoping/workspace-contract.md`, ticket
//! `.kranz/tickets/workspace-provider-seam.md`) — who supplies the mission's
//! **runnable environment**, separate from [`crate::backend::AgentBackend`]
//! (who drives model sessions) and from `sandbox.provider` (process
//! containment; M7). A container may one day implement both Sandbox and
//! Workspace, but the APIs stay separate: sandbox = blast radius, workspace =
//! bootstrap/services/readiness/previews.
//!
//! The run loop drives the seam once per `run()` invocation, BEFORE the
//! first worker/validator spawns:
//!
//! ```text
//! provider.provision(spec) -> WorkspaceHandle      (workspace.provisioned)
//! provider.readiness(handle) -> ReadinessOutcome   (the workspace gate; workspace.readiness)
//! … workers/validators run in handle.cwd …
//! provider.teardown(handle, mode)                  (workspace.teardown)
//! ```
//!
//! v1 ships exactly one implementation, [`LocalWorktreeProvider`]:
//!
//! - **Provision REUSES the existing isolation machinery — it does not
//!   rebuild it.** In worktree mode `run()` has already created the mission
//!   integration worktree (`setup_mission_worktree`) before the seam drive
//!   runs; in checkout mode the repo root is the execution cwd. Provision
//!   resolves that cwd into the handle and stamps the env sessions already
//!   get (`KRANZ_BASE_SHA` via the [`crate::runner::contract_env`] idiom —
//!   never secret values). Preview placeholders come from the contract's
//!   `previews[]` with their URL templates UNFILLED (D-E: previews are
//!   artifacts once the services behind them are ready; v1 records the
//!   placeholder, never a fabricated URL).
//! - **Readiness IS the workspace bootstrap + readiness gate** (design D-C):
//!   the gate's phase execution moved under this seam
//!   ([`crate::workspace_gate`] keeps the helpers and the block/lift
//!   policy), so the provision path has a single owner. Behavior is
//!   byte-identical to the pre-seam gate: same block reasons, same
//!   `orchestrator.decision` start/pass/fail events, plus the additive
//!   `workspace.*` lifecycle events alongside.
//! - **Teardown: [`TeardownMode::Keep`] is the only real mode v1.**
//!   `Hibernate`/`Destroy` are accepted and recorded but are no-ops for the
//!   local provider — the integration worktree's filesystem lifecycle stays
//!   with the existing mission-branch/merge machinery (merge semantics
//!   unchanged). A `workspace.teardown` event records the provider call, not
//!   the filesystem outcome.
//!
//! Provider selection: additive mission config `workspace.provider`
//! (absent = `local-worktree`). Unknown names FAIL CLOSED at run start via
//! [`resolve`] — never a silent fallback to local.

use crate::error::{EngineError, Result};
use crate::events::EventKind;
use crate::orchestrator::MissionEngine;
use crate::types::MissionStatus;
use crate::workspace_contract::WorkspaceContract;
use crate::workspace_gate::{
    self, CommandOutcome, GatePhase, BOOTSTRAP_SUMMARY_PREFIX, READINESS_SUMMARY_PREFIX,
};
use std::collections::HashMap;
use std::path::PathBuf;

/// The provider kinds this build knows. v1: local-worktree only; remote/
/// container providers are their own tickets (`local-container-workspace`,
/// `workspace-remote-coder-provider`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceProviderKind {
    /// Today's isolation cwd: the mission integration worktree (worktree
    /// mode) or the repo root (checkout mode).
    LocalWorktree,
}

impl WorkspaceProviderKind {
    /// The wire/config name (`workspace.provider`, `workspace.provisioned`).
    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceProviderKind::LocalWorktree => "local-worktree",
        }
    }
}

/// What `teardown` should do with the workspace. v1 local-worktree: only
/// [`TeardownMode::Keep`] has real semantics (trivially — keeping is doing
/// nothing); the other modes are accepted and recorded as no-ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeardownMode {
    /// Leave the workspace in place (resume, inspection, takeover).
    Keep,
    /// Provider-owned idle suspension (remote/container providers).
    Hibernate,
    /// Release the workspace entirely.
    Destroy,
}

impl TeardownMode {
    /// The wire name recorded in `workspace.teardown`.
    pub fn as_str(self) -> &'static str {
        match self {
            TeardownMode::Keep => "keep",
            TeardownMode::Hibernate => "hibernate",
            TeardownMode::Destroy => "destroy",
        }
    }
}

/// Everything a provider needs to provision one mission's workspace.
#[derive(Debug, Clone)]
pub struct ProvisionSpec {
    pub mission_id: String,
    /// The mission execution root — the integration worktree in worktree
    /// mode, the repo root in checkout mode, resolved by `run()`'s existing
    /// isolation machinery (the local provider reuses it, never recreates
    /// it).
    pub repo_root: PathBuf,
    /// Base SHA pinned at approval; the handle env carries it as
    /// `KRANZ_BASE_SHA` (the `contract_env` idiom every contract-command
    /// execution context shares).
    pub base_sha: Option<String>,
    /// The workspace contract read from the live base branch, when present
    /// (`None` = today's worktree-only behavior, readiness trivially ready).
    pub contract: Option<WorkspaceContract>,
}

/// A contracted preview with its URL template UNFILLED — the services behind
/// it do not exist yet, so v1 never fabricates a URL (D-E).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewPlaceholder {
    pub name: String,
    pub url_template: String,
}

/// The provisioned workspace: what sessions run against. Carries everything
/// `readiness`/`teardown` need, so the provider stays stateless across the
/// three calls (a future remote provider's handle would carry the workspace
/// id / connection info instead).
#[derive(Debug, Clone)]
pub struct WorkspaceHandle {
    /// Session cwd (the mission execution root).
    pub cwd: PathBuf,
    /// Extra env for sessions in this workspace — `KRANZ_BASE_SHA` when a
    /// base SHA was pinned, exactly what validation-contract commands get
    /// today. Never secret values.
    pub env: HashMap<String, String>,
    /// Contract previews with unfilled URL templates (empty without a
    /// contract or a `previews[]` block).
    pub previews: Vec<PreviewPlaceholder>,
    /// The contract this workspace was provisioned against, so `readiness`
    /// executes exactly what `provision` saw.
    pub contract: Option<WorkspaceContract>,
}

/// What `readiness` concluded. `Ready` = spend may start (no contract, or
/// bootstrap + readiness all passed). `Failed` carries the failing phase's
/// kind ("bootstrap command" / "readiness check") and first failing command
/// outcome, so the engine can block with the gate's established reason shape.
#[derive(Debug)]
pub enum ReadinessOutcome {
    Ready,
    Failed {
        kind: &'static str,
        failed: CommandOutcome,
    },
}

/// The gate progress sink handed to [`WorkspaceProvider::readiness`]: the
/// provider reports the gate's established start/pass/fail decision lines
/// through it, and the engine folds them onto the `orchestrator.decision`
/// audit channel byte-identically to the pre-seam gate (including WHEN they
/// appear relative to the commands running).
pub type ProgressSink<'a> = dyn FnMut(&str, Option<String>) -> Result<()> + Send + 'a;

/// The seam: provision a runnable environment, prove it ready, tear it down.
/// Distinct from [`crate::backend::AgentBackend`] — the backend drives model
/// sessions INSIDE the workspace this trait supplies.
#[async_trait::async_trait]
pub trait WorkspaceProvider: Send + Sync {
    /// Which kind this provider is (recorded in `workspace.provisioned`).
    fn kind(&self) -> WorkspaceProviderKind;

    /// Establish the workspace for one mission and return its handle.
    async fn provision(&self, spec: &ProvisionSpec) -> Result<WorkspaceHandle>;

    /// Prove the workspace ready for spend (see [`ProgressSink`]).
    async fn readiness(
        &self,
        handle: &WorkspaceHandle,
        progress: &mut ProgressSink<'_>,
    ) -> Result<ReadinessOutcome>;

    /// Tear the workspace down per `mode` (see [`TeardownMode`] for v1
    /// local-worktree semantics).
    async fn teardown(&self, handle: WorkspaceHandle, mode: TeardownMode) -> Result<()>;
}

/// Resolve the configured `workspace.provider` into a provider instance.
/// Absent (or `"local-worktree"`) selects today's local worktree. Unknown
/// names FAIL CLOSED with a clear error — an unprovisioned run must never
/// silently fall back to a provider the operator did not ask for.
pub fn resolve(provider: Option<&str>) -> Result<Box<dyn WorkspaceProvider>> {
    match provider {
        None => Ok(Box::new(LocalWorktreeProvider)),
        Some(name) if name == WorkspaceProviderKind::LocalWorktree.as_str() => {
            Ok(Box::new(LocalWorktreeProvider))
        }
        Some(other) => Err(EngineError::Config(format!(
            "workspace.provider {other:?} is not a known workspace provider \
             (this build provides {:?} only); refusing to run rather than \
             silently falling back",
            WorkspaceProviderKind::LocalWorktree.as_str()
        ))),
    }
}

/// The local-worktree provider (v1's only implementation): provisions
/// today's isolation cwd via the existing worktree machinery, runs the
/// workspace gate's bootstrap/readiness phases for readiness, and treats
/// teardown as a recorded no-op (the worktree lifecycle stays with the
/// mission-branch/merge machinery).
pub struct LocalWorktreeProvider;

#[async_trait::async_trait]
impl WorkspaceProvider for LocalWorktreeProvider {
    fn kind(&self) -> WorkspaceProviderKind {
        WorkspaceProviderKind::LocalWorktree
    }

    async fn provision(&self, spec: &ProvisionSpec) -> Result<WorkspaceHandle> {
        if !spec.repo_root.is_dir() {
            return Err(EngineError::InvalidState(format!(
                "local-worktree provision: execution cwd {} does not exist",
                spec.repo_root.display()
            )));
        }
        let env = crate::runner::contract_env(spec.base_sha.as_deref());
        let previews = spec
            .contract
            .iter()
            .flat_map(|contract| &contract.previews)
            .map(|preview| PreviewPlaceholder {
                name: preview.name.clone(),
                url_template: preview.url_template.clone(),
            })
            .collect();
        Ok(WorkspaceHandle {
            cwd: spec.repo_root.clone(),
            env,
            previews,
            contract: spec.contract.clone(),
        })
    }

    async fn readiness(
        &self,
        handle: &WorkspaceHandle,
        progress: &mut ProgressSink<'_>,
    ) -> Result<ReadinessOutcome> {
        let Some(contract) = &handle.contract else {
            // No contract: the gate is a no-op (byte-identical pre-gate
            // behavior) — trivially ready, no progress lines.
            return Ok(ReadinessOutcome::Ready);
        };

        // 1. bootstrap — ordered, stop at first failure. Commands run with
        //    the same env discipline as validation-contract commands:
        //    inherited env plus the handle's `KRANZ_BASE_SHA` (bootstrap
        //    needs real toolchain/registry env; the merge gates' stripped
        //    env is deliberately NOT used here).
        if let Some(failed) = run_gate_phase(
            &GatePhase {
                kind: "bootstrap command",
                unit: "command",
                plural: "commands",
                prefix: BOOTSTRAP_SUMMARY_PREFIX,
                commands: &contract.bootstrap,
                stop_at_first_failure: true,
            },
            handle,
            progress,
        )
        .await?
        {
            return Ok(ReadinessOutcome::Failed {
                kind: "bootstrap command",
                failed,
            });
        }

        // 2. readiness — every check runs; all must pass.
        if let Some(failed) = run_gate_phase(
            &GatePhase {
                kind: "readiness check",
                unit: "check",
                plural: "checks",
                prefix: READINESS_SUMMARY_PREFIX,
                commands: &contract.readiness,
                stop_at_first_failure: false,
            },
            handle,
            progress,
        )
        .await?
        {
            return Ok(ReadinessOutcome::Failed {
                kind: "readiness check",
                failed,
            });
        }

        Ok(ReadinessOutcome::Ready)
    }

    async fn teardown(&self, _handle: WorkspaceHandle, _mode: TeardownMode) -> Result<()> {
        // v1: Keep is the only real mode; Hibernate/Destroy are accepted and
        // recorded (by the engine's workspace.teardown event) but are no-ops
        // here — the integration worktree's lifecycle stays with the
        // existing mission-branch/merge machinery.
        Ok(())
    }
}

/// Run one gate phase (bootstrap or readiness), reporting the start and
/// pass/fail lines through `progress` at the same points the pre-seam gate
/// emitted its decisions. Returns the first failing [`CommandOutcome`]
/// (bootstrap: also the last one run; readiness: the first of possibly
/// several, all of which ran).
async fn run_gate_phase(
    phase: &GatePhase<'_>,
    handle: &WorkspaceHandle,
    progress: &mut ProgressSink<'_>,
) -> Result<Option<CommandOutcome>> {
    let n = phase.commands.len();
    progress(
        &format!("{} running {n} {}", phase.prefix, phase.plural),
        None,
    )?;
    let outcomes = workspace_gate::run_gate_commands(&handle.cwd, phase, &handle.env).await;
    match outcomes.iter().find(|o| !o.ok()) {
        None => {
            progress(
                &format!("{} {n}/{n} {} ok", phase.prefix, phase.plural),
                Some(workspace_gate::outcomes_detail(phase.kind, &outcomes)),
            )?;
            Ok(None)
        }
        Some(failed) => {
            progress(
                &format!(
                    "{} FAILED at {} {}/{n} — blocking mission (owner: repo-setup)",
                    phase.prefix, phase.unit, failed.ordinal
                ),
                Some(workspace_gate::outcomes_detail(phase.kind, &outcomes)),
            )?;
            Ok(Some(failed.clone()))
        }
    }
}

impl MissionEngine {
    /// The seam drive (design D-B/D-C), run once at the top of `run_loop`
    /// before any worker/validator spawns: provider.provision →
    /// provider.readiness (= the workspace gate) → workers. Returns
    /// `Ok(None)` when the run may proceed; `Ok(Some(Blocked))` when a gate
    /// failure blocked the first incomplete milestone with owner
    /// `repo-setup`.
    ///
    /// Byte-identical to the pre-seam gate's semantics — same block reasons,
    /// same `orchestrator.decision` events — PLUS the additive `workspace.*`
    /// lifecycle events (D-E): `workspace.provisioned` on every run (the
    /// workspace exists, contract or not), `workspace.readiness` only when a
    /// contract drove a real bootstrap/readiness execution (a contract-less
    /// run must not imply a runnable environment — D-H).
    pub(crate) async fn provision_workspace(
        &mut self,
        provider: &dyn WorkspaceProvider,
    ) -> Result<Option<MissionStatus>> {
        // The contract is read from the LIVE BASE branch (merge.rs's
        // `live_base_sha` idiom): base-branch-owned in BOTH isolation modes
        // (a mission branch cannot weaken the contract that gates its own
        // spend), and a committed operator fix on base is picked up on
        // resume.
        let base_branch = self.state.mission.base_branch.clone();
        let contract =
            crate::workspace_contract::load_workspace_contract_at_ref(&self.repo, &base_branch)?;
        let spec = ProvisionSpec {
            mission_id: self.state.mission.id.clone(),
            repo_root: self.active_root().to_path_buf(),
            base_sha: self.state.mission.base_sha.clone(),
            contract,
        };
        let handle = provider.provision(&spec).await?;
        self.emit(EventKind::WorkspaceProvisioned {
            provider: provider.kind().as_str().to_string(),
            cwd: handle.cwd.display().to_string(),
        })?;
        let has_contract = handle.contract.is_some();
        let outcome = {
            // The gate's start/pass/fail lines land on the established
            // orchestrator.decision audit channel, scrubbed by emit_decision.
            let mut progress = |summary: &str, detail: Option<String>| -> Result<()> {
                self.emit_decision(summary, detail)
            };
            provider.readiness(&handle, &mut progress).await?
        };
        // The handle outlives readiness so run() can record teardown at the
        // end of the run (a blocked run keeps its workspace for resume).
        self.workspace_handle = Some(handle);
        match outcome {
            // No contract: byte-identical pre-gate behavior — no readiness
            // artifact and no gate-owned block lifting.
            ReadinessOutcome::Ready if !has_contract => Ok(None),
            ReadinessOutcome::Ready => {
                self.emit(EventKind::WorkspaceReadinessReport {
                    outcome: "ready".to_string(),
                    detail: None,
                })?;
                // Gate passed: lift a gate-owned block left by a previous
                // run() (the operator fixed the environment; resume must not
                // wedge on a block whose precondition is gone).
                self.lift_gate_block()?;
                Ok(None)
            }
            ReadinessOutcome::Failed { kind, failed } => {
                self.emit(EventKind::WorkspaceReadinessReport {
                    outcome: "failed".to_string(),
                    // The gate's scrubbed reason shape (command, exit code,
                    // owner, bounded tail) — the same string the block below
                    // records; event-append scrubs again (defense in depth).
                    detail: Some(workspace_gate::gate_block_reason(kind, &failed)),
                })?;
                self.block_on_gate_failure(kind, &failed)
            }
        }
    }

    /// Record provider teardown at the end of a `run()` — v1 always
    /// [`TeardownMode::Keep`] (the local provider never destroys; the
    /// integration worktree's filesystem lifecycle stays with the existing
    /// mission machinery). Best-effort and never fatal, mirroring
    /// `teardown_mission_worktree`: the run's result is already decided, so
    /// a teardown/append failure is logged, not propagated.
    pub(crate) async fn teardown_workspace(&mut self, provider: &dyn WorkspaceProvider) {
        let Some(handle) = self.workspace_handle.take() else {
            return; // never provisioned this run (e.g. resolve/provision failed)
        };
        if let Err(e) = provider.teardown(handle, TeardownMode::Keep).await {
            tracing::warn!(error = %e, "workspace provider teardown failed");
            return;
        }
        if let Err(e) = self.emit(EventKind::WorkspaceTeardown {
            mode: TeardownMode::Keep.as_str().to_string(),
        }) {
            tracing::warn!(error = %e, "workspace.teardown append failed");
        }
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_contract::parse_workspace_contract;

    fn spec(
        root: PathBuf,
        base_sha: Option<&str>,
        contract: Option<WorkspaceContract>,
    ) -> ProvisionSpec {
        ProvisionSpec {
            mission_id: "m-test".to_string(),
            repo_root: root,
            base_sha: base_sha.map(|s| s.to_string()),
            contract,
        }
    }

    fn contract(json: &[u8]) -> WorkspaceContract {
        parse_workspace_contract(json).expect("valid contract")
    }

    /// Collect progress lines the gate would emit.
    #[derive(Default)]
    struct Progress(Vec<(String, Option<String>)>);

    impl Progress {
        fn sink(&mut self) -> impl FnMut(&str, Option<String>) -> Result<()> + Send + use<'_> {
            |summary, detail| {
                self.0.push((summary.to_string(), detail));
                Ok(())
            }
        }

        fn summaries(&self) -> Vec<&str> {
            self.0.iter().map(|(s, _)| s.as_str()).collect()
        }
    }

    #[test]
    fn resolve_defaults_to_local_worktree_and_fails_closed_on_unknown_names() {
        assert_eq!(
            resolve(None).expect("absent = local-worktree").kind(),
            WorkspaceProviderKind::LocalWorktree
        );
        assert_eq!(
            resolve(Some("local-worktree"))
                .expect("explicit local-worktree")
                .kind(),
            WorkspaceProviderKind::LocalWorktree
        );
        for unknown in ["coder", "local-container", "remote", "Local-Worktree"] {
            let err = resolve(Some(unknown))
                .err()
                .expect("unknown providers fail closed");
            let msg = err.to_string();
            assert!(msg.contains("workspace.provider"), "{msg}");
            assert!(msg.contains(&format!("{unknown:?}")), "{msg}");
            assert!(msg.contains("\"local-worktree\" only"), "{msg}");
        }
    }

    #[tokio::test]
    async fn local_worktree_provision_carries_cwd_env_previews_and_contract() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contract = contract(
            br#"{
                "schemaVersion": 1,
                "readiness": ["echo ok"],
                "previews": [{ "name": "app", "urlTemplate": "http://localhost:{port}/" }]
            }"#,
        );
        let handle = LocalWorktreeProvider
            .provision(&spec(
                dir.path().to_path_buf(),
                Some("deadbeefcafe"),
                Some(contract),
            ))
            .await
            .expect("provision");

        assert_eq!(handle.cwd, dir.path());
        assert_eq!(
            handle.env.get("KRANZ_BASE_SHA").map(String::as_str),
            Some("deadbeefcafe"),
            "the handle env carries the pinned base sha (contract_env idiom)"
        );
        assert_eq!(handle.env.len(), 1, "nothing but KRANZ_BASE_SHA");
        // Preview placeholders keep the URL template UNFILLED — never a
        // fabricated URL for services that do not exist (D-E).
        assert_eq!(
            handle.previews,
            vec![PreviewPlaceholder {
                name: "app".to_string(),
                url_template: "http://localhost:{port}/".to_string(),
            }]
        );
        assert!(handle.contract.is_some());
    }

    #[tokio::test]
    async fn local_worktree_provision_without_base_sha_or_contract_is_minimal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = LocalWorktreeProvider
            .provision(&spec(dir.path().to_path_buf(), None, None))
            .await
            .expect("provision");
        assert!(handle.env.is_empty(), "no base sha pinned ⇒ no env");
        assert!(handle.previews.is_empty());
        assert!(handle.contract.is_none());
    }

    #[tokio::test]
    async fn local_worktree_provision_refuses_a_missing_cwd() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("no-such-worktree");
        let err = LocalWorktreeProvider
            .provision(&spec(missing.clone(), None, None))
            .await
            .expect_err("a missing execution cwd is a provision error");
        assert!(err.to_string().contains("does not exist"), "{err}");
    }

    #[tokio::test]
    async fn readiness_without_contract_is_ready_and_silent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let handle = LocalWorktreeProvider
            .provision(&spec(dir.path().to_path_buf(), None, None))
            .await
            .expect("provision");
        let mut progress = Progress::default();
        let outcome = LocalWorktreeProvider
            .readiness(&handle, &mut progress.sink())
            .await
            .expect("readiness");
        assert!(matches!(outcome, ReadinessOutcome::Ready));
        assert!(
            progress.0.is_empty(),
            "no contract ⇒ no gate decision lines (byte-identical pre-gate behavior)"
        );
    }

    /// Native-shell assertion that the handle's env reaches gate commands
    /// (`sh` on Unix, `cmd` on Windows — same idiom as the validation
    /// contract's command environment tests).
    fn base_sha_assertion_command(expected: &str) -> String {
        #[cfg(unix)]
        {
            format!("test \"$KRANZ_BASE_SHA\" = '{expected}'")
        }
        #[cfg(windows)]
        {
            format!("if \"%KRANZ_BASE_SHA%\"==\"{expected}\" (exit /b 0) else (exit /b 1)")
        }
    }

    /// Readiness IS the gate (single owner of the provision path): the
    /// bootstrap phase runs before the readiness phase in the handle's cwd
    /// WITH the handle's env, and the progress lines are the gate's
    /// established shapes.
    #[tokio::test]
    async fn readiness_runs_bootstrap_then_readiness_with_gate_progress() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contract_json = format!(
            r#"{{
                "schemaVersion": 1,
                "bootstrap": ["echo boot > .boot-marker"],
                "readiness": [{}]
            }}"#,
            serde_json::to_string(&base_sha_assertion_command("deadbeefcafe")).unwrap()
        );
        let contract = contract(contract_json.as_bytes());
        let handle = LocalWorktreeProvider
            .provision(&spec(
                dir.path().to_path_buf(),
                Some("deadbeefcafe"),
                Some(contract),
            ))
            .await
            .expect("provision");
        let mut progress = Progress::default();
        let outcome = LocalWorktreeProvider
            .readiness(&handle, &mut progress.sink())
            .await
            .expect("readiness");

        assert!(
            matches!(outcome, ReadinessOutcome::Ready),
            "readiness passed ⇒ the KRANZ_BASE_SHA assertion saw the handle env: {outcome:?}"
        );
        assert!(
            dir.path().join(".boot-marker").exists(),
            "bootstrap ran in the provisioned cwd"
        );
        assert_eq!(
            progress.summaries(),
            vec![
                "workspace bootstrap: running 1 commands",
                "workspace bootstrap: 1/1 commands ok",
                "workspace readiness: running 1 checks",
                "workspace readiness: 1/1 checks ok",
            ]
        );
    }

    #[tokio::test]
    async fn readiness_bootstrap_failure_stops_before_readiness() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contract = contract(
            br#"{
                "schemaVersion": 1,
                "bootstrap": ["exit 42"],
                "readiness": ["echo never-runs > .readiness-marker"]
            }"#,
        );
        let handle = LocalWorktreeProvider
            .provision(&spec(dir.path().to_path_buf(), None, Some(contract)))
            .await
            .expect("provision");
        let mut progress = Progress::default();
        let outcome = LocalWorktreeProvider
            .readiness(&handle, &mut progress.sink())
            .await
            .expect("readiness");

        let ReadinessOutcome::Failed { kind, failed } = outcome else {
            panic!("bootstrap failure must be Failed, got {outcome:?}");
        };
        assert_eq!(kind, "bootstrap command");
        assert_eq!(failed.code, Some(42));
        assert!(
            !dir.path().join(".readiness-marker").exists(),
            "bootstrap stop-at-first-failure: readiness never ran"
        );
        assert_eq!(
            progress.summaries(),
            vec![
                "workspace bootstrap: running 1 commands",
                "workspace bootstrap: FAILED at command 1/1 — blocking mission (owner: repo-setup)",
            ]
        );
    }

    #[tokio::test]
    async fn readiness_check_failure_reports_after_bootstrap_passed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contract = contract(
            br#"{
                "schemaVersion": 1,
                "bootstrap": ["echo boot > .boot-marker"],
                "readiness": ["exit 3"]
            }"#,
        );
        let handle = LocalWorktreeProvider
            .provision(&spec(dir.path().to_path_buf(), None, Some(contract)))
            .await
            .expect("provision");
        let mut progress = Progress::default();
        let outcome = LocalWorktreeProvider
            .readiness(&handle, &mut progress.sink())
            .await
            .expect("readiness");

        let ReadinessOutcome::Failed { kind, failed } = outcome else {
            panic!("readiness failure must be Failed, got {outcome:?}");
        };
        assert_eq!(kind, "readiness check");
        assert_eq!(failed.code, Some(3));
        assert!(
            dir.path().join(".boot-marker").exists(),
            "bootstrap ran to completion first"
        );
        assert_eq!(
            progress.summaries(),
            vec![
                "workspace bootstrap: running 1 commands",
                "workspace bootstrap: 1/1 commands ok",
                "workspace readiness: running 1 checks",
                "workspace readiness: FAILED at check 1/1 — blocking mission (owner: repo-setup)",
            ]
        );
    }

    #[tokio::test]
    async fn teardown_accepts_every_mode_as_a_recorded_no_op() {
        let dir = tempfile::tempdir().expect("tempdir");
        for mode in [
            TeardownMode::Keep,
            TeardownMode::Hibernate,
            TeardownMode::Destroy,
        ] {
            let handle = LocalWorktreeProvider
                .provision(&spec(dir.path().to_path_buf(), None, None))
                .await
                .expect("provision");
            LocalWorktreeProvider
                .teardown(handle, mode)
                .await
                .expect("teardown is always Ok for local-worktree");
            assert!(
                dir.path().exists(),
                "teardown never touches the filesystem ({mode:?})"
            );
        }
        assert_eq!(TeardownMode::Keep.as_str(), "keep");
        assert_eq!(TeardownMode::Hibernate.as_str(), "hibernate");
        assert_eq!(TeardownMode::Destroy.as_str(), "destroy");
        assert_eq!(
            WorkspaceProviderKind::LocalWorktree.as_str(),
            "local-worktree"
        );
    }
}
