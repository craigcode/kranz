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

use crate::command_exec::run_shell_command_with_code_cleared;
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

/// The `milestone.unblocked` reason [`WorkspaceGate::lift_gate_block`] emits
/// when a previously failed gate passes — an ENGINE-owned unblock, not an
/// operator decision. `pub(crate)` so the flight-surgeon fold
/// (escalation_metrics.rs) excludes exactly this unblock from the operator
/// intervention count by matching the same constant the lift emits.
pub(crate) const GATE_LIFT_REASON: &str = "workspace gate now passing: bootstrap and readiness ok";

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
                reason: GATE_LIFT_REASON.to_string(),
                validator_guidance: None,
            })?;
        }
        Ok(())
    }
}

/// The scratch `HOME`/`TMPDIR`/`CARGO_HOME` every gate command of ONE
/// mission run shares, under the mission's own writable `runs/` dir.
///
/// This used to be a `GateScratch` built per PHASE with a `Drop` that
/// `remove_dir_all`'d it (follow-up review, M-1): bootstrap installed a
/// toolchain into `/tmp/kranz-workspace-gate-<A>`, that directory was deleted
/// when the phase returned, readiness ran against an empty `<B>`, and the
/// mission blocked on a readiness failure the operator could not reproduce by
/// hand. It also paid `cache_only_cargo_home`'s registry copy (bounded at 512
/// MiB, the cost that filled the disk and killed mission m-533143) once per
/// phase instead of once per run. One home per mission run fixes both:
/// bootstrap output survives into readiness and into the data hooks the
/// engine drives mid-run, and the copy happens once.
///
/// `runs/` is deliberate — it is the one part of the mission dir a sandboxed
/// session may write ([`crate::sandbox`]'s `mission_write_denies` keeps the
/// audit log, state snapshot and control inbox read-only), so a contained
/// gate can use it.
pub(crate) fn mission_gate_home(runtime_dir: &std::path::Path) -> std::path::PathBuf {
    runtime_dir.join("runs").join("workspace-gate")
}

/// Create the gate home (owner-only) if it is not there yet. Idempotent: the
/// first command of the run creates it, the rest reuse it.
fn ensure_gate_home(root: &std::path::Path) {
    let _ = std::fs::create_dir_all(root);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700));
    }
}

/// Remove the shared gate home at the END of the whole gate run (provider
/// teardown — the last point any gate command can fire). Best-effort: a
/// leftover is mission-scoped and goes with the mission dir.
pub(crate) fn remove_gate_home(root: &std::path::Path) {
    if root.as_os_str().is_empty() {
        return;
    }
    let _ = std::fs::remove_dir_all(root);
}

/// Non-secret OPERATIONAL vars that ALWAYS cross into a gate env (follow-up
/// review, M-2).
///
/// H4's cleared env dropped these along with the credentials, which broke
/// ordinary bootstraps in ways that read as repo bugs: no `SSH_AUTH_SOCK`
/// means `git clone git@…` and `git submodule update --init` fail, and no
/// proxy/CA vars means `npm ci` / `pip install` / `cargo fetch` fail behind a
/// corporate proxy or a TLS-inspecting CA. None of them is a credential: each
/// is a LOCATION (a socket path, a proxy URL, a CA bundle path).
///
/// What is deliberately NOT here: anything that names a PROGRAM.
/// `GIT_SSH_COMMAND`, `GIT_CONFIG_*` (beyond the one value this module sets
/// itself, below), `GIT_EXTERNAL_DIFF`, `LD_PRELOAD` and their kin turn a
/// later git invocation into arbitrary host execution, which is the same
/// class of hole the `.git/config` write deny exists to close.
pub(crate) const GATE_OPERATIONAL_ENV: &[&str] = &[
    "SSH_AUTH_SOCK",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "GIT_SSL_CAINFO",
];

/// The operator's real global git config, for `GIT_CONFIG_GLOBAL` (follow-up
/// review, M-2). The gate's relocated `HOME` hides `~/.gitconfig`, so a
/// bootstrap that commits fails with "Please tell me who you are" and
/// `insteadOf` / `credential.helper` rewrites vanish. Naming the operator's
/// file explicitly restores it for READ without un-relocating HOME (which
/// would hand the contract the whole home directory back).
///
/// Resolution order matches git's own: `$GIT_CONFIG_GLOBAL`, then
/// `$XDG_CONFIG_HOME/git/config`, then `~/.config/git/config`, then
/// `~/.gitconfig`. The first that EXISTS wins; nothing is set when none does.
fn operator_global_gitconfig() -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Some(explicit) = std::env::var_os("GIT_CONFIG_GLOBAL").filter(|v| !v.is_empty()) {
        candidates.push(std::path::PathBuf::from(explicit));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        candidates.push(std::path::PathBuf::from(xdg).join("git").join("config"));
    }
    if let Some(home) = crate::agent_env::operator_home() {
        candidates.push(home.join(".config").join("git").join("config"));
        candidates.push(home.join(".gitconfig"));
    }
    candidates.into_iter().find(|path| path.is_file())
}

/// The secret names that actually cross into a gate env: the intersection of
/// what the REPO declares and what the OPERATOR allowed (follow-up review,
/// H-6).
///
/// H4 narrowed the gate env from "every ambient credential" to "every
/// credential the contract's `secrets[]` names" — which against a CHOOSING
/// attacker is the same set. `.kranz/workspace.json` is ordinary repo content
/// that runs at mission start before any agent spawns, and its `secrets[]`
/// validation is shape-only (`^[A-Z][A-Z0-9_]*$`), so `GH_TOKEN`,
/// `AWS_SECRET_ACCESS_KEY`, `ANTHROPIC_API_KEY` and `KRANZ_TOKEN` all match.
/// A hostile clone could therefore name the engine's credentials and exfil
/// them from an unsandboxed bootstrap command.
///
/// The operator-owned channel this duplicates already had the guard: mission
/// config's `contractEnvPassthrough` is refused from the project layer
/// (`config.rs`'s `PROJECT_LAYER_REFUSED`) precisely because "it copies named
/// ambient credentials verbatim into contract-command environments". Two-party
/// consent restores it: the repo says which names its commands NEED, the
/// operator says which names may LEAVE the host, and only the intersection
/// crosses. A declared name the operator did not allow is refused loudly, by
/// name, with the config key that would admit it.
///
/// Matching is case-INSENSITIVE, the same rule
/// [`crate::agent_env::contract_command_env`] applies to managed keys, so a
/// Windows casing difference cannot slip a name past the operator's list.
pub(crate) fn two_party_secrets(declared: &[String], operator_allows: &[String]) -> Vec<String> {
    let mut allowed = Vec::new();
    for name in declared {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        if operator_allows
            .iter()
            .any(|allowed| allowed.trim().eq_ignore_ascii_case(name))
        {
            allowed.push(name.to_string());
        } else {
            tracing::warn!(
                key = name,
                config_key = "contractEnvPassthrough",
                "workspace contract secrets[] entry refused: the repo declared it but the \
                 operator's contractEnvPassthrough does not list it, so it does not cross \
                 into the gate command environment"
            );
        }
    }
    allowed
}

/// The COMPLETE environment one workspace bootstrap / readiness / data-hook /
/// disk-prune command runs with (2026-09-01 adversarial audit, H4; follow-up
/// review H-6, M-1, M-2, M-3).
///
/// These commands used to spawn with `clear_env = false`: the workspace
/// contract's own doc comment justified the ambient environment by pointing
/// at the contract's declared `secrets[]` list, but nothing filtered to that
/// list — `contract.secrets` was referenced only by the remote provider. So
/// `.kranz/workspace.json`, which runs at mission start before any agent
/// spawns and is ordinary repo content a merged worker commit can edit,
/// executed host commands with every ambient credential the engine holds.
/// That was strictly weaker containment than the validation-contract path in
/// the same binary.
///
/// The env is built the way [`crate::agent_env::contract_command_env`] builds
/// a contract command's, in this order (later wins):
///
/// 1. cleared, with `HOME`/`TMPDIR`/`CARGO_HOME` relocated to the mission's
///    ONE shared gate home ([`mission_gate_home`], M-1) and `KRANZ_BASE_SHA`
///    pinned, plus exactly the ambient vars BOTH parties named
///    ([`two_party_secrets`], H-6);
/// 2. the fixed operational allowlist ([`GATE_OPERATIONAL_ENV`], M-2) and
///    `GIT_CONFIG_GLOBAL`. These are set AFTER the secrets deliberately: a
///    repo-declared secret named `HTTPS_PROXY` or `GIT_CONFIG_GLOBAL` must not
///    be able to point the gate's git or TLS at somewhere of the repo's
///    choosing;
/// 3. the handle env last, so a provider-supplied value (the
///    container/remote providers' endpoints) still reaches the command.
///
/// Known boundary, documented rather than papered over: `~/.ssh` does NOT
/// cross. The relocated HOME hides it and no key file is copied, so ssh
/// authentication for gate commands works through the FORWARDED AGENT
/// (`SSH_AUTH_SOCK`) only — key-file auth without an agent is unsupported
/// here, because admitting it means either handing the contract the operator's
/// private keys or letting it name an ssh program.
///
/// Remaining gap, named rather than papered over: these commands are still
/// not SANDBOX-WRAPPED. Every other engine-run command goes through
/// `command_exec::run_shell_command_sandboxed` with the mission's resolved
/// `GateSandbox`, but the [`crate::workspace_provider::WorkspaceProvider`]
/// seam carries neither the mission's sandbox config nor its mission dir,
/// and both are needed to resolve a target. Closing it means widening
/// `WorkspaceHandle`, which every provider constructs. The credential half
/// of H4 — the half the audit confirmed — is closed here.
pub(crate) fn gate_command_env(
    policy: &crate::workspace_provider::GateEnvPolicy,
    handle_env: &HashMap<String, String>,
    contract: Option<&crate::workspace_contract::WorkspaceContract>,
) -> HashMap<String, String> {
    ensure_gate_home(&policy.home);
    let declared = contract.map(|c| c.secrets.as_slice()).unwrap_or(&[]);
    let secrets = two_party_secrets(declared, &policy.passthrough);
    let mut env = crate::agent_env::contract_command_env(
        &policy.home,
        handle_env.get("KRANZ_BASE_SHA").map(String::as_str),
        &secrets,
    );
    for name in GATE_OPERATIONAL_ENV {
        if let Some(value) = std::env::var_os(name).filter(|value| !value.is_empty()) {
            env.insert((*name).to_string(), value.to_string_lossy().into_owned());
        }
    }
    if let Some(gitconfig) = operator_global_gitconfig() {
        env.insert(
            "GIT_CONFIG_GLOBAL".to_string(),
            gitconfig.display().to_string(),
        );
    }
    for (key, value) in handle_env {
        env.insert(key.clone(), value.clone());
    }
    env
}

/// Run one phase's command lines in the workspace cwd — bounded,
/// process-tree-killed, output-tailed (the shared `command_exec` runner
/// used by validation-contract commands), with the CLEARED gate env
/// [`gate_command_env`] builds.
pub(crate) async fn run_gate_commands(
    cwd: &std::path::Path,
    phase: &GatePhase<'_>,
    policy: &crate::workspace_provider::GateEnvPolicy,
    handle_env: &HashMap<String, String>,
    contract: Option<&crate::workspace_contract::WorkspaceContract>,
) -> Vec<CommandOutcome> {
    let env = gate_command_env(policy, handle_env, contract);
    let total = phase.commands.len();
    let mut outcomes = Vec::with_capacity(total);
    for (i, command) in phase.commands.iter().enumerate() {
        let (code, output_tail) = run_shell_command_with_code_cleared(cwd, command, &env).await;
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

    fn policy(
        home: &std::path::Path,
        passthrough: &[&str],
    ) -> crate::workspace_provider::GateEnvPolicy {
        crate::workspace_provider::GateEnvPolicy {
            home: home.to_path_buf(),
            passthrough: passthrough.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn contract_declaring(secrets: &[&str]) -> crate::workspace_contract::WorkspaceContract {
        let json = format!(
            r#"{{"schemaVersion": 1, "readiness": ["true"], "secrets": {}}}"#,
            serde_json::to_string(secrets).unwrap()
        );
        crate::workspace_contract::parse_workspace_contract(json.as_bytes()).expect("contract")
    }

    /// H-6 (follow-up review): H4 narrowed the gate env from "every ambient
    /// credential" to "every credential the CONTRACT names" — which against a
    /// choosing attacker is the same set, because `.kranz/workspace.json` is
    /// repo content that runs at mission start before any agent spawns and
    /// its `secrets[]` validation is shape-only. A secret now needs TWO
    /// parties: the repo declares the need, the operator's
    /// `contractEnvPassthrough` grants it.
    #[test]
    fn gate_env_crosses_a_secret_only_with_both_repo_and_operator_consent() {
        let home = tempfile::tempdir().expect("tempdir");
        let _guard = crate::agent_env::EnvTestGuard::engage(&[("GH_TOKEN", "ghp-operator-secret")]);
        let contract = contract_declaring(&["GH_TOKEN"]);
        let handle_env = HashMap::new();

        // The repo asks and the operator has granted NOTHING: refused.
        let env = gate_command_env(&policy(home.path(), &[]), &handle_env, Some(&contract));
        assert!(
            !env.contains_key("GH_TOKEN"),
            "a repo-chosen credential must not cross on the repo's say-so alone: {env:?}"
        );

        // The operator names it too: it crosses.
        let env = gate_command_env(
            &policy(home.path(), &["GH_TOKEN"]),
            &handle_env,
            Some(&contract),
        );
        assert_eq!(
            env.get("GH_TOKEN").map(String::as_str),
            Some("ghp-operator-secret"),
            "both parties consented, so the named credential crosses"
        );

        // The operator's grant alone is not enough either — the contract has
        // to have declared the need, or the gate env stays narrow.
        let env = gate_command_env(&policy(home.path(), &["GH_TOKEN"]), &handle_env, None);
        assert!(
            !env.contains_key("GH_TOKEN"),
            "an operator grant does not push a credential into a contract that never asked"
        );
    }

    /// The intersection is case-insensitive (the rule `contract_command_env`
    /// already applies to managed keys), so a Windows casing difference
    /// cannot slip a name past the operator's list in either direction.
    #[test]
    fn two_party_secrets_intersects_case_insensitively_and_drops_the_rest() {
        let declared = ["GH_TOKEN", "AWS_SECRET_ACCESS_KEY", "KRANZ_TOKEN", "  "]
            .map(str::to_string)
            .to_vec();
        let allowed = two_party_secrets(&declared, &["gh_token".to_string()]);
        assert_eq!(allowed, vec!["GH_TOKEN".to_string()]);
        assert!(two_party_secrets(&declared, &[]).is_empty());
    }

    /// M-2 (follow-up review): H4's cleared env also dropped the non-secret
    /// OPERATIONAL vars, which breaks ordinary bootstraps as if they were
    /// repo bugs — no `SSH_AUTH_SOCK` means `git clone git@…` fails, no
    /// proxy/CA vars means `npm ci` fails behind a corporate proxy. These are
    /// locations, not credentials, and cross unconditionally. Anything that
    /// names a PROGRAM does not.
    #[test]
    fn gate_env_always_carries_the_operational_allowlist_but_never_a_program_var() {
        let home = tempfile::tempdir().expect("tempdir");
        let _guard = crate::agent_env::EnvTestGuard::engage(&[
            ("SSH_AUTH_SOCK", "/tmp/ssh-agent.sock"),
            ("HTTPS_PROXY", "http://proxy.corp.example:3128"),
            ("NO_PROXY", "localhost"),
            ("SSL_CERT_FILE", "/etc/ssl/corp-bundle.pem"),
            ("GIT_SSH_COMMAND", "/tmp/evil-ssh"),
        ]);

        let env = gate_command_env(&policy(home.path(), &[]), &HashMap::new(), None);

        assert_eq!(
            env.get("SSH_AUTH_SOCK").map(String::as_str),
            Some("/tmp/ssh-agent.sock")
        );
        assert_eq!(
            env.get("HTTPS_PROXY").map(String::as_str),
            Some("http://proxy.corp.example:3128")
        );
        assert_eq!(env.get("NO_PROXY").map(String::as_str), Some("localhost"));
        assert_eq!(
            env.get("SSL_CERT_FILE").map(String::as_str),
            Some("/etc/ssl/corp-bundle.pem")
        );
        assert!(
            !env.contains_key("GIT_SSH_COMMAND"),
            "a var that names a PROGRAM turns a later git call into host execution: {env:?}"
        );
        // Unset operational names are simply absent — never an empty value a
        // tool would read as "no proxy configured differently".
        assert!(!env.contains_key("GIT_SSL_CAINFO"));
    }

    /// The gate's relocated HOME hides `~/.gitconfig`, so `git commit` in a
    /// bootstrap fails "Please tell me who you are" and `insteadOf` /
    /// `credential.helper` rewrites vanish (M-2). `GIT_CONFIG_GLOBAL` names
    /// the operator's real file for READ — unconditionally, with no contract
    /// declaration and no operator passthrough entry involved.
    #[test]
    fn gate_env_points_git_at_the_operator_global_config() {
        let home = tempfile::tempdir().expect("tempdir");
        let operator = tempfile::tempdir().expect("tempdir");
        let gitconfig = operator.path().join("gitconfig");
        std::fs::write(&gitconfig, "[user]\n\tname = Operator\n").expect("write");
        let _guard = crate::agent_env::EnvTestGuard::engage(&[
            ("GIT_CONFIG_GLOBAL", gitconfig.to_str().unwrap()),
            ("HOME", operator.path().to_str().unwrap()),
        ]);

        let env = gate_command_env(&policy(home.path(), &[]), &HashMap::new(), None);
        assert_eq!(
            env.get("GIT_CONFIG_GLOBAL").map(String::as_str),
            Some(gitconfig.to_str().unwrap()),
            "git reads the operator's own global config: {env:?}"
        );
        assert_ne!(
            env.get("HOME").map(String::as_str),
            Some(operator.path().to_str().unwrap()),
            "naming the config file must not un-relocate HOME"
        );
    }

    /// M-1 (follow-up review): the gate home is per MISSION RUN and lives
    /// under the mission's own writable `runs/` dir, so bootstrap's output
    /// survives into readiness and into the data hooks the engine drives
    /// mid-run. It used to be a fresh temp dir per phase with a `Drop` that
    /// deleted it.
    #[test]
    fn the_gate_home_is_one_stable_dir_under_the_mission_runs_dir() {
        let mission = tempfile::tempdir().expect("tempdir");
        let home = mission_gate_home(mission.path());
        assert_eq!(home, mission.path().join("runs").join("workspace-gate"));
        assert_eq!(
            home,
            mission_gate_home(mission.path()),
            "the same mission resolves to the same home on every phase"
        );

        // Building an env creates it; a second build reuses what is there.
        let env = gate_command_env(&policy(&home, &[]), &HashMap::new(), None);
        assert_eq!(env.get("HOME").map(String::as_str), home.to_str());
        std::fs::write(home.join("installed-by-bootstrap"), "x").expect("write");
        let _ = gate_command_env(&policy(&home, &[]), &HashMap::new(), None);
        assert!(
            home.join("installed-by-bootstrap").is_file(),
            "a later phase must find what an earlier phase installed"
        );

        remove_gate_home(&home);
        assert!(!home.exists(), "teardown removes the shared home");
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
