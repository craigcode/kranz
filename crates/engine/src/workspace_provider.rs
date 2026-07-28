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
//!     … golden-data clone → migrate → bootstrap → readiness → skewCheck …
//! … workers/validators run in handle.cwd …
//!     … validation_round re-seeds via provider.run_data_hook(reset) when
//!       the data block opts in (resetBetweenRounds) …
//! provider.teardown(handle, mode)                  (workspace.teardown)
//! ```
//!
//! This build ships three implementations:
//!
//! - [`LocalWorktreeProvider`] (v1):
//!   - **Provision REUSES the existing isolation machinery — it does not
//!     rebuild it.** In worktree mode `run()` has already created the mission
//!     integration worktree (`setup_mission_worktree`) before the seam drive
//!     runs; in checkout mode the repo root is the execution cwd. Provision
//!     resolves that cwd into the handle and stamps the env sessions already
//!     get (`KRANZ_BASE_SHA` via the [`crate::runner::contract_env`] idiom —
//!     never secret values). Preview placeholders come from the contract's
//!     `previews[]` with their URL templates UNFILLED (D-E: previews are
//!     artifacts once the services behind them are ready; v1 records the
//!     placeholder, never a fabricated URL).
//!   - **Readiness IS the workspace bootstrap + readiness gate** (design D-C):
//!     the gate's phase execution moved under this seam
//!     ([`crate::workspace_gate`] keeps the helpers and the block/lift
//!     policy), so the provision path has a single owner. Behavior is
//!     byte-identical to the pre-seam gate: same block reasons, same
//!     `orchestrator.decision` start/pass/fail events, plus the additive
//!     `workspace.*` lifecycle events alongside.
//!   - **Teardown: [`TeardownMode::Keep`] is the only real mode.**
//!     `Hibernate`/`Destroy` are accepted and recorded but are no-ops for the
//!     local provider — the integration worktree's filesystem lifecycle stays
//!     with the existing mission-branch/merge machinery (merge semantics
//!     unchanged). A `workspace.teardown` event records the provider call, not
//!     the filesystem outcome.
//! - [`crate::workspace_container::LocalContainerProvider`] (ticket
//!   `local-container-workspace`): a per-mission compose project with
//!   dynamic ports and contract health/readiness inside the container
//!   network. See that module's docs for the network model, port policy,
//!   and real Hibernate/Destroy teardown semantics.
//! - [`crate::workspace_remote::RemoteWorkspaceProvider`] (ticket
//!   `workspace-remote-coder-provider`): a thin adapter over a Coder-shaped
//!   substrate (injectable [`crate::workspace_remote::SubstrateClient`]) —
//!   provision from a pinned template, substrate-reported readiness,
//!   preview/takeover URLs, secret NAMES injected by the substrate. See that
//!   module's docs for the config gate, owner taxonomy, and v1 honesty
//!   notes.
//!
//! The two local providers share the gate phase shapes below: `run_gate_phase` (host
//! execution) and [`report_gate_outcomes`] (the pass/fail decision lines the
//! container provider reuses after running the same commands via
//! `compose exec`).
//!
//! Provider selection: additive mission config `workspace.provider`
//! (absent = `local-worktree`). Unknown names FAIL CLOSED via [`resolve`] —
//! at plan approval (the [`pin`] consent artifact) AND again at run start —
//! never a silent fallback to local. `"remote"` additionally requires its
//! `workspace.remote.*` config block complete, failing closed with the
//! missing key named. Runtime detection for `"container"`
//! happens at PROVISION (run start, before any spend), keeping approval-time
//! resolution/pinning pure: a runtime-less host fails closed at run start
//! with the reason named.

use crate::error::{EngineError, Result};
use crate::events::EventKind;
use crate::orchestrator::MissionEngine;
use crate::types::{MissionStatus, WorkerIsolation, WorkspaceConfig, WorkspacePin};
use crate::workspace_contract::WorkspaceContract;
use crate::workspace_gate::{
    self, CommandOutcome, GatePhase, BOOTSTRAP_SUMMARY_PREFIX, READINESS_SUMMARY_PREFIX,
};
use std::collections::HashMap;
use std::path::PathBuf;

/// The provider kinds this build knows: local-worktree, the local-container
/// provider (ticket `local-container-workspace`), and the remote
/// Coder-shaped substrate adapter (ticket `workspace-remote-coder-provider`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceProviderKind {
    /// Today's isolation cwd: the mission integration worktree (worktree
    /// mode) or the repo root (checkout mode).
    LocalWorktree,
    /// Per-mission compose project (dynamic ports, in-network readiness) —
    /// [`crate::workspace_container::LocalContainerProvider`].
    Container,
    /// Thin Coder-shaped substrate adapter —
    /// [`crate::workspace_remote::RemoteWorkspaceProvider`].
    Remote,
}

impl WorkspaceProviderKind {
    /// The wire/config name (`workspace.provider`, `workspace.provisioned`).
    pub fn as_str(self) -> &'static str {
        match self {
            WorkspaceProviderKind::LocalWorktree => "local-worktree",
            WorkspaceProviderKind::Container => "container",
            WorkspaceProviderKind::Remote => "remote",
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
    /// The mission-owned runtime dir (`.kranz/missions/<id>/` on the primary
    /// side, gitignored): container providers write their compose project
    /// files under it — never inside the worktree, whose lifecycle belongs
    /// to the mission-branch machinery. Unused by the local provider.
    pub runtime_dir: PathBuf,
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
    /// Contract previews. Local-worktree keeps the URL templates UNFILLED
    /// (D-E); the container provider substitutes `{port}` ONLY with an
    /// actually-assigned dynamic host port (never fabricated).
    pub previews: Vec<PreviewPlaceholder>,
    /// The contract this workspace was provisioned against, so `readiness`
    /// executes exactly what `provision` saw.
    pub contract: Option<WorkspaceContract>,
    /// Provider-specific detail recorded on `workspace.provisioned` — the
    /// container provider's compose project name. `None` for local-worktree
    /// and for contract-less provisions.
    pub detail: Option<String>,
    /// Container-provider state (compose project/file, assigned ports);
    /// `None` for local-worktree and contract-less provisions.
    pub container: Option<crate::workspace_container::ContainerWorkspace>,
    /// Remote-provider state (substrate workspace id/name, takeover URL,
    /// name-matched previews, poll outcome); `None` for local kinds and
    /// contract-less provisions.
    pub remote: Option<crate::workspace_remote::RemoteWorkspace>,
}

/// What `readiness` concluded. `Ready` = spend may start (no contract, or
/// bootstrap + readiness all passed). `Failed` carries the failing phase's
/// kind ("bootstrap command" / "readiness check" / "data clone hook" /
/// "data migrate hook") and first failing command outcome, so the engine can
/// block with the gate's established reason shape.
#[derive(Debug)]
pub enum ReadinessOutcome {
    Ready,
    Failed {
        kind: &'static str,
        failed: CommandOutcome,
    },
    /// The data block's `skewCheck` failed: migration/version skew between
    /// the golden dataset and the workspace code (design D-D). A DISTINCT
    /// outcome from a readiness flake — the engine Blocks with the skew
    /// reason (the migrate/reset hook named as the action), never with the
    /// generic readiness shape.
    DataSkew {
        failed: CommandOutcome,
    },
    /// The provider/substrate itself failed (remote substrate reported the
    /// workspace failed or never became ready inside the poll bound). A
    /// DISTINCT outcome from a contract-command failure: the engine Blocks
    /// with owner `provider` (never `repo-setup`), and the block is NOT
    /// gate-prefixed — a later gate pass does not auto-lift it; the operator
    /// unblocks after the substrate recovers.
    ProviderFailed {
        /// The provider's scrubbed failure detail (workspace name + reason).
        detail: String,
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

    /// Run one declared golden-data hook (design D-D) inside this workspace
    /// and report its `workspace data:` decision line through `progress`.
    /// Used by `readiness` (clone/migrate/skewCheck) and by the engine's
    /// reset-between-rounds drive from `validation_round` — one method so
    /// the reset fires through the same execution path as the provision-time
    /// hooks. Returns the failing [`CommandOutcome`] only on failure.
    ///
    /// The default runs the command with the gate's env discipline (the
    /// shared bounded shell runner in the handle's cwd, inherited env plus
    /// the handle's env — never a new secrets channel). The container
    /// provider overrides to exec INSIDE the container network, so data
    /// hooks never run on the host when a container workspace exists.
    async fn run_data_hook(
        &self,
        handle: &WorkspaceHandle,
        hook: crate::workspace_data::DataHookKind,
        command: &str,
        progress: &mut ProgressSink<'_>,
    ) -> Result<Option<CommandOutcome>> {
        let (code, output_tail) =
            crate::command_exec::run_shell_command_with_code(&handle.cwd, command, &handle.env)
                .await;
        crate::workspace_data::hook_outcome(hook, command, code, output_tail, progress)
    }
}

/// Resolve the configured `workspace.provider` into a provider instance.
/// Absent (or `"local-worktree"`) selects today's local worktree;
/// `"container"` selects the local-container provider; `"remote"` selects
/// the Coder-shaped substrate adapter — ONLY with complete
/// `workspace.remote.*` config, failing closed with the missing key named.
/// Unknown names FAIL CLOSED with a clear error naming the
/// `workspace.provider` config key and its operator owner — an unprovisioned
/// run must never silently fall back to a provider the operator did not ask
/// for. Resolution stays pure (no host detection, no env reads, no network):
/// a runtime-less host selecting `"container"`, or a token-less environment
/// selecting `"remote"`, fails closed at provision (run start, before any
/// spend).
pub fn resolve(config: &WorkspaceConfig) -> Result<Box<dyn WorkspaceProvider>> {
    match config.provider.as_deref() {
        None => Ok(Box::new(LocalWorktreeProvider)),
        Some(name) if name == WorkspaceProviderKind::LocalWorktree.as_str() => {
            Ok(Box::new(LocalWorktreeProvider))
        }
        Some(name) if name == WorkspaceProviderKind::Container.as_str() => Ok(Box::new(
            crate::workspace_container::LocalContainerProvider::new(),
        )),
        Some(name) if name == WorkspaceProviderKind::Remote.as_str() => Ok(Box::new(
            crate::workspace_remote::RemoteWorkspaceProvider::from_config(config.remote.as_ref())?,
        )),
        Some(other) => Err(EngineError::Config(format!(
            "workspace.provider {other:?} is not a known workspace provider \
             (this build provides {:?}, {:?}, and {:?} only; owner: operator — fix the \
             workspace.provider config key); refusing rather than silently \
             falling back",
            WorkspaceProviderKind::LocalWorktree.as_str(),
            WorkspaceProviderKind::Container.as_str(),
            WorkspaceProviderKind::Remote.as_str()
        ))),
    }
}

/// Pin the effective workspace provider identity at plan approval (design
/// D-B, ticket `workspace-provider-pin-at-approval`) — the consent artifact
/// `approve_plan` emits as `workspace.provider.pinned` immediately before
/// `plan.approved`. Resolution IS the seam's fail-closed [`resolve`], so an
/// unknown `workspace.provider` name (or incomplete `workspace.remote.*`
/// config) refuses here, at approval time, BEFORE any branch/commit side
/// effect — a misspelled name never silently defaults (owner: operator).
/// Local-worktree-only missions pin too: the pin makes the default explicit
/// and honest (D-H: source isolation, not a runnable workspace).
///
/// Per-kind field meanings (see [`WorkspacePin`]): local kinds pin the
/// isolation mode + contract schemaVersion; the remote kind pins the
/// CONFIGURED substrate template/image id and the ADAPTER version — the pin
/// stays pure, with no substrate contact at approval.
pub fn pin(
    config: &WorkspaceConfig,
    isolation: WorkerIsolation,
    contract: Option<&WorkspaceContract>,
) -> Result<WorkspacePin> {
    let resolved = resolve(config)?;
    if resolved.kind() == WorkspaceProviderKind::Remote {
        // resolve() already failed closed on incomplete remote config; the
        // re-validation here is the same pure check.
        let remote = crate::workspace_remote::RemoteConfig::require(config.remote.as_ref())?;
        return Ok(WorkspacePin {
            provider: resolved.kind().as_str().to_string(),
            template: remote.template,
            version: crate::workspace_remote::ADAPTER_VERSION.to_string(),
        });
    }
    let template = match isolation {
        WorkerIsolation::Worktree => "worktree",
        WorkerIsolation::Checkout => "checkout",
    };
    let version = match contract {
        Some(contract) => contract.schema_version.to_string(),
        None => "none".to_string(),
    };
    Ok(WorkspacePin {
        provider: resolved.kind().as_str().to_string(),
        template: template.to_string(),
        version,
    })
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
            detail: None,
            container: None,
            remote: None,
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

        // 0. golden data clone/migrate (design D-D) — after provision,
        //    before bootstrap. Undeclared hooks skip silently. A failure
        //    folds into the gate's block shape with a data-hook kind.
        if let Some(data) = &contract.data {
            for (hook, command) in [
                (crate::workspace_data::DataHookKind::Clone, &data.clone),
                (crate::workspace_data::DataHookKind::Migrate, &data.migrate),
            ] {
                if let Some(command) = command {
                    if let Some(failed) =
                        self.run_data_hook(handle, hook, command, progress).await?
                    {
                        return Ok(ReadinessOutcome::Failed {
                            kind: hook.gate_kind(),
                            failed,
                        });
                    }
                }
            }
        }

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

        // 3. golden data skewCheck (design D-D) — the last readiness step.
        //    Its failure is the SKEW case: a distinct outcome the engine
        //    Blocks on with the migrate/reset action named, never a generic
        //    readiness failure.
        if let Some(command) = contract
            .data
            .as_ref()
            .and_then(|data| data.skew_check.as_ref())
        {
            if let Some(failed) = self
                .run_data_hook(
                    handle,
                    crate::workspace_data::DataHookKind::SkewCheck,
                    command,
                    progress,
                )
                .await?
            {
                return Ok(ReadinessOutcome::DataSkew { failed });
            }
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
    progress(
        &format!(
            "{} running {} {}",
            phase.prefix,
            phase.commands.len(),
            phase.plural
        ),
        None,
    )?;
    let outcomes = workspace_gate::run_gate_commands(&handle.cwd, phase, &handle.env).await;
    report_gate_outcomes(phase, outcomes, progress)
}

/// The pass/fail half of a gate phase, split from execution so the
/// container provider — which runs the same phases via `compose exec`
/// instead of on the host — reports byte-identical decision lines. Takes
/// the phase's already-computed outcomes; returns the first failing one.
pub(crate) fn report_gate_outcomes(
    phase: &GatePhase<'_>,
    outcomes: Vec<CommandOutcome>,
    progress: &mut ProgressSink<'_>,
) -> Result<Option<CommandOutcome>> {
    let n = phase.commands.len();
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
            runtime_dir: self.paths.mission_dir(),
            base_sha: self.state.mission.base_sha.clone(),
            contract,
        };
        let handle = provider.provision(&spec).await?;
        self.emit(EventKind::WorkspaceProvisioned {
            provider: provider.kind().as_str().to_string(),
            cwd: handle.cwd.display().to_string(),
            detail: handle.detail.clone(),
            // Remote kind (ticket workspace-remote-coder-provider): the
            // substrate's takeover URL and name-matched previews ride the
            // provisioned event so the endpoint/report can surface them;
            // absent for local kinds.
            takeover: handle
                .remote
                .as_ref()
                .and_then(|remote| remote.takeover.clone()),
            previews: handle.remote.as_ref().map(|remote| remote.previews.clone()),
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
            ReadinessOutcome::DataSkew { failed } => {
                // The SKEW case (design D-D): a distinct, owned Block — the
                // reason names the data block's skewCheck, its exit, a
                // scrubbed tail, the repo-setup owner, and the action (run
                // the declared migrate/reset hook, then resume). Never a
                // generic readiness failure and never a validator finding.
                // The gate prefix keeps the block liftable on resume once
                // the environment is fixed.
                let data = self
                    .workspace_handle
                    .as_ref()
                    .and_then(|handle| handle.contract.as_ref())
                    .and_then(|contract| contract.data.clone());
                let reason = crate::workspace_data::skew_block_reason(data.as_ref(), &failed);
                self.emit(EventKind::WorkspaceReadinessReport {
                    outcome: "skew".to_string(),
                    detail: Some(reason.clone()),
                })?;
                self.block_with_gate_reason(reason)
            }
            ReadinessOutcome::ProviderFailed { detail } => {
                // The PROVIDER-owned case (design D-C's owner taxonomy): the
                // substrate itself failed — never a contract-command
                // (repo-setup) failure and never a config (operator) one.
                // The reason's distinct prefix keeps the block OUT of the
                // gate's auto-lift path: the operator unblocks once the
                // substrate recovers.
                let reason = crate::workspace_remote::provider_block_reason(&detail);
                self.emit(EventKind::WorkspaceReadinessReport {
                    outcome: "failed".to_string(),
                    detail: Some(reason.clone()),
                })?;
                self.block_with_gate_reason(reason)
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

    fn ws_config(provider: Option<&str>) -> WorkspaceConfig {
        WorkspaceConfig {
            provider: provider.map(str::to_string),
            remote: None,
        }
    }

    fn remote_block() -> crate::types::RemoteWorkspaceConfig {
        crate::types::RemoteWorkspaceConfig {
            base_url: Some("https://coder.internal.example.com".to_string()),
            template: Some("tmpl-baked-ami".to_string()),
            token_env: Some("CODER_SESSION_TOKEN".to_string()),
        }
    }

    fn spec(
        root: PathBuf,
        base_sha: Option<&str>,
        contract: Option<WorkspaceContract>,
    ) -> ProvisionSpec {
        ProvisionSpec {
            mission_id: "m-test".to_string(),
            runtime_dir: root.join(".kranz").join("missions").join("m-test"),
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
            resolve(&ws_config(None))
                .expect("absent = local-worktree")
                .kind(),
            WorkspaceProviderKind::LocalWorktree
        );
        assert_eq!(
            resolve(&ws_config(Some("local-worktree")))
                .expect("explicit local-worktree")
                .kind(),
            WorkspaceProviderKind::LocalWorktree
        );
        for unknown in ["coder", "local-container", "Local-Worktree"] {
            let err = resolve(&ws_config(Some(unknown)))
                .err()
                .expect("unknown providers fail closed");
            let msg = err.to_string();
            assert!(msg.contains("workspace.provider"), "{msg}");
            assert!(msg.contains(&format!("{unknown:?}")), "{msg}");
            assert!(msg.contains("\"local-worktree\""), "{msg}");
            assert!(msg.contains("\"container\""), "{msg}");
            assert!(msg.contains("\"remote\""), "{msg}");
            assert!(msg.contains("only"), "{msg}");
            assert!(msg.contains("owner: operator"), "{msg}");
        }
    }

    /// `"container"` resolves to the local-container provider (ticket
    /// `local-container-workspace`) — purely, with no host runtime
    /// detection, so approval-time pinning works on runtime-less hosts and
    /// the no-runtime refusal lands at provision (run start, before spend).
    #[test]
    fn resolve_container_picks_the_local_container_provider() {
        assert_eq!(
            resolve(&ws_config(Some("container")))
                .expect("container is a known provider")
                .kind(),
            WorkspaceProviderKind::Container
        );
        assert_eq!(WorkspaceProviderKind::Container.as_str(), "container");
    }

    /// `"remote"` resolves to the substrate adapter ONLY with complete
    /// `workspace.remote.*` config; each missing key (or an absent `remote`
    /// block) fails CLOSED with the key named and the operator owner —
    /// never a silent fallback to local (ticket
    /// `workspace-remote-coder-provider`).
    #[test]
    fn remote_workspace_resolve_picks_the_adapter_only_with_complete_config() {
        let complete = WorkspaceConfig {
            provider: Some("remote".to_string()),
            remote: Some(remote_block()),
        };
        assert_eq!(
            resolve(&complete)
                .expect("complete remote config resolves")
                .kind(),
            WorkspaceProviderKind::Remote
        );
        assert_eq!(WorkspaceProviderKind::Remote.as_str(), "remote");

        for (remote, missing_key) in [
            (None, "workspace.remote.baseUrl"),
            (
                Some(crate::types::RemoteWorkspaceConfig {
                    template: Some("tmpl".to_string()),
                    token_env: Some("CODER_SESSION_TOKEN".to_string()),
                    ..Default::default()
                }),
                "workspace.remote.baseUrl",
            ),
            (
                Some(crate::types::RemoteWorkspaceConfig {
                    base_url: Some("https://coder.internal.example.com".to_string()),
                    token_env: Some("CODER_SESSION_TOKEN".to_string()),
                    ..Default::default()
                }),
                "workspace.remote.template",
            ),
            (
                Some(crate::types::RemoteWorkspaceConfig {
                    base_url: Some("https://coder.internal.example.com".to_string()),
                    template: Some("tmpl".to_string()),
                    ..Default::default()
                }),
                "workspace.remote.tokenEnv",
            ),
        ] {
            let config = WorkspaceConfig {
                provider: Some("remote".to_string()),
                remote,
            };
            let err = resolve(&config)
                .err()
                .expect("incomplete remote config fails closed");
            let msg = err.to_string();
            assert!(msg.contains(missing_key), "names the missing key: {msg}");
            assert!(msg.contains("owner: operator"), "{msg}");
            assert!(
                msg.contains("refusing rather than silently falling back"),
                "{msg}"
            );
        }
    }

    /// The approval-time pin (D-B): local-worktree pins the isolation mode as
    /// its template and the contract's schemaVersion as its version (`"none"`
    /// without a contract) — the default made explicit and honest (D-H).
    /// Unknown provider names fail closed with the config key + owner named,
    /// exactly like run-start resolution.
    #[test]
    fn pin_records_isolation_mode_and_contract_version() {
        let contract = contract(br#"{"schemaVersion": 1, "readiness": ["true"]}"#);

        let pinned =
            pin(&ws_config(None), WorkerIsolation::Worktree, Some(&contract)).expect("pin");
        assert_eq!(
            pinned,
            WorkspacePin {
                provider: "local-worktree".to_string(),
                template: "worktree".to_string(),
                version: "1".to_string(),
            }
        );

        let pinned = pin(
            &ws_config(Some("local-worktree")),
            WorkerIsolation::Checkout,
            Some(&contract),
        )
        .expect("explicit local-worktree pins too");
        assert_eq!(pinned.template, "checkout");
        assert_eq!(pinned.version, "1");

        // No contract ⇒ the honest "none" version — never imply a contract
        // schema that does not exist.
        let pinned =
            pin(&ws_config(None), WorkerIsolation::Worktree, None).expect("pin without contract");
        assert_eq!(pinned.provider, "local-worktree");
        assert_eq!(pinned.version, "none");

        let pinned = pin(
            &ws_config(Some("container")),
            WorkerIsolation::Worktree,
            Some(&contract),
        )
        .expect("container pins at approval (pure resolution)");
        assert_eq!(pinned.provider, "container");
        assert_eq!(pinned.version, "1");

        let err = pin(&ws_config(Some("coder")), WorkerIsolation::Worktree, None)
            .expect_err("a misspelled provider never silently defaults");
        let msg = err.to_string();
        assert!(msg.contains("workspace.provider"), "{msg}");
        assert!(msg.contains("\"coder\""), "{msg}");
        assert!(msg.contains("owner: operator"), "{msg}");
    }

    /// The remote pin (D-B, ticket `workspace-remote-coder-provider`):
    /// provider `"remote"`, template = the CONFIGURED substrate
    /// template/image id, version = the adapter version — all populated at
    /// approval, purely (no substrate contact). Incomplete remote config
    /// refuses approval with the missing key named.
    #[test]
    fn remote_workspace_pin_populates_provider_template_and_adapter_version() {
        let contract = contract(br#"{"schemaVersion": 1, "readiness": ["true"]}"#);
        let config = WorkspaceConfig {
            provider: Some("remote".to_string()),
            remote: Some(remote_block()),
        };
        let pinned = pin(&config, WorkerIsolation::Worktree, Some(&contract))
            .expect("remote pins at approval");
        assert_eq!(
            pinned,
            WorkspacePin {
                provider: "remote".to_string(),
                template: "tmpl-baked-ami".to_string(),
                version: "coder-v1".to_string(),
            },
            "configured template + adapter version, not the contract schema"
        );

        let incomplete = WorkspaceConfig {
            provider: Some("remote".to_string()),
            remote: Some(crate::types::RemoteWorkspaceConfig {
                template: Some("tmpl".to_string()),
                token_env: Some("CODER_SESSION_TOKEN".to_string()),
                ..Default::default()
            }),
        };
        let err = pin(&incomplete, WorkerIsolation::Worktree, Some(&contract))
            .expect_err("incomplete remote config refuses approval");
        assert!(
            err.to_string().contains("workspace.remote.baseUrl"),
            "{err}"
        );
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
        assert!(
            handle.detail.is_none() && handle.container.is_none(),
            "local-worktree records no provider detail or container state"
        );
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

    // -----------------------------------------------------------------------
    // Golden-data hooks (design D-D, ticket golden-data-hooks)
    // -----------------------------------------------------------------------

    /// Lifecycle order (D-D), proven by markers each step asserts before
    /// writing its own: clone → migrate → bootstrap → readiness → skewCheck.
    /// Shell lines stay sh/cmd portable (echo / > / && / test -f only).
    #[tokio::test]
    async fn readiness_runs_data_hooks_in_lifecycle_order() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contract = contract(
            br#"{
                "schemaVersion": 1,
                "data": {
                    "clone": "echo cloned > .clone-marker",
                    "migrate": "test -f .clone-marker && echo mig > .migrate-marker",
                    "skewCheck": "test -f .boot-marker && echo checked > .skew-marker"
                },
                "bootstrap": ["test -f .migrate-marker && echo boot > .boot-marker"],
                "readiness": ["test -f .boot-marker"]
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

        assert!(
            matches!(outcome, ReadinessOutcome::Ready),
            "every marker assertion passed ⇒ the hooks ran in order: {outcome:?}"
        );
        for marker in [".clone-marker", ".migrate-marker", ".skew-marker"] {
            assert!(dir.path().join(marker).exists(), "{marker} written");
        }
        assert_eq!(
            progress.summaries(),
            vec![
                "workspace data: clone `echo cloned > .clone-marker` → ok (exit code 0)",
                "workspace data: migrate `test -f .clone-marker && echo mig > .migrate-marker` → ok (exit code 0)",
                "workspace bootstrap: running 1 commands",
                "workspace bootstrap: 1/1 commands ok",
                "workspace readiness: running 1 checks",
                "workspace readiness: 1/1 checks ok",
                "workspace data: skewCheck `test -f .boot-marker && echo checked > .skew-marker` → ok (exit code 0)",
            ]
        );
    }

    /// A clone/migrate failure folds into the gate's block shape with a
    /// data-hook kind, stopping before every later phase.
    #[tokio::test]
    async fn readiness_data_clone_failure_stops_before_migrate_and_bootstrap() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contract = contract(
            br#"{
                "schemaVersion": 1,
                "data": {
                    "clone": "exit 42",
                    "migrate": "echo mig > .migrate-marker"
                },
                "bootstrap": ["echo boot > .boot-marker"],
                "readiness": ["test -f .boot-marker"]
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
            panic!("a clone failure must be Failed, got {outcome:?}");
        };
        assert_eq!(kind, "data clone hook");
        assert_eq!(failed.code, Some(42));
        assert!(
            !dir.path().join(".migrate-marker").exists(),
            "the data phase stops at the first failure: migrate never ran"
        );
        assert!(
            !dir.path().join(".boot-marker").exists(),
            "bootstrap never ran after a data-hook failure"
        );
        assert_eq!(
            progress.summaries(),
            vec![
                "workspace data: clone `exit 42` → FAILED (exit code 42) — blocking mission (owner: repo-setup)",
            ]
        );
    }

    /// skewCheck failure AFTER bootstrap+readiness passed is the distinct
    /// DataSkew outcome (D-D) — never a generic readiness failure.
    #[tokio::test]
    async fn readiness_skew_failure_is_the_distinct_skew_outcome() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contract = contract(
            br#"{
                "schemaVersion": 1,
                "data": {
                    "migrate": "echo mig > .migrate-marker",
                    "skewCheck": "exit 1"
                },
                "bootstrap": ["echo boot > .boot-marker"],
                "readiness": ["test -f .boot-marker"]
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

        let ReadinessOutcome::DataSkew { failed } = outcome else {
            panic!("a skewCheck failure must be DataSkew, got {outcome:?}");
        };
        assert_eq!(failed.code, Some(1));
        assert!(
            dir.path().join(".boot-marker").exists(),
            "bootstrap and readiness passed before the skew check ran"
        );
        assert_eq!(
            progress.summaries(),
            vec![
                "workspace data: migrate `echo mig > .migrate-marker` → ok (exit code 0)",
                "workspace bootstrap: running 1 commands",
                "workspace bootstrap: 1/1 commands ok",
                "workspace readiness: running 1 checks",
                "workspace readiness: 1/1 checks ok",
                "workspace data: skewCheck `exit 1` → FAILED (exit code 1) — blocking mission (owner: repo-setup)",
            ]
        );
    }

    /// The default `run_data_hook` (the reset-between-rounds path) runs in
    /// the handle's cwd with the handle's env.
    #[tokio::test]
    async fn run_data_hook_executes_in_the_workspace_with_handle_env() {
        let dir = tempfile::tempdir().expect("tempdir");
        let contract = contract(
            br#"{"schemaVersion": 1, "data": {"reset": "seed", "resetBetweenRounds": true}}"#,
        );
        let handle = LocalWorktreeProvider
            .provision(&spec(
                dir.path().to_path_buf(),
                Some("deadbeefcafe"),
                Some(contract),
            ))
            .await
            .expect("provision");
        let mut progress = Progress::default();
        let failed = LocalWorktreeProvider
            .run_data_hook(
                &handle,
                crate::workspace_data::DataHookKind::Reset,
                &base_sha_assertion_command("deadbeefcafe"),
                &mut progress.sink(),
            )
            .await
            .expect("run_data_hook");
        assert!(
            failed.is_none(),
            "the KRANZ_BASE_SHA assertion saw the handle env: {failed:?}"
        );
        assert_eq!(progress.summaries().len(), 1);
        assert!(
            progress.summaries()[0].starts_with("workspace data: reset `"),
            "{:?}",
            progress.summaries()
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
