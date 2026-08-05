//! Layered mission configuration (plan §6).
//!
//! Configuration is resolved from three layers, later layers winning:
//!
//! 1. [`MissionConfig::default()`] — compiled-in defaults
//! 2. `~/.kranz/config.json` — the user's global config ([`crate::paths::global_config`])
//! 3. `<repo>/.kranz/config.json` — per-project config ([`crate::paths::project_config`])
//!
//! Files may be *partial*: any subset of keys. The merge happens on
//! `serde_json::Value` trees so a project file can override a single nested
//! field (e.g. only `worker.model`) without restating the rest. Unknown keys
//! are ignored on deserialization.

use crate::cost::{
    DEFAULT_CODEX_MODEL, DEFAULT_CURSOR_MODEL, DEFAULT_DROID_MODEL, DEFAULT_KIMI_MODEL,
};
use crate::error::{EngineError, Result};
use crate::paths;
use crate::types::{BackendKind, ExecutorTier, MissionConfig, Role, SandboxEnforce};
use std::path::{Path, PathBuf};

/// Reasoning-effort values accepted by `claude --effort`.
const VALID_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// Maximum dispatch-pool size (`workerCandidates`, KRZ-303). Each candidate
/// is a full paid worker session per unit of work, so the same 8-wide bound
/// as `maxParallelWorkers` applies — well past any useful fan-out.
pub const MAX_WORKER_CANDIDATES: usize = 8;

/// Coarse model capability tiers used by config safety floors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModelTier {
    BelowDefault,
    Default,
    Frontier,
}

/// Parse the optional role backend field.
pub fn parse_backend(raw: Option<&str>) -> std::result::Result<BackendKind, String> {
    match raw {
        None | Some("claude") => Ok(BackendKind::Claude),
        Some("codex") => Ok(BackendKind::Codex),
        Some("droid") => Ok(BackendKind::Droid),
        Some("kimi") => Ok(BackendKind::Kimi),
        Some("local") => Ok(BackendKind::Local),
        Some("acp") => Ok(BackendKind::Acp),
        Some("cursor") => Ok(BackendKind::Cursor),
        Some(other) => Err(other.to_string()),
    }
}

/// Deterministically map a ticket's `task-class` frontmatter to an executor
/// tier. Literal table only — no heuristics: `execution-class` (case- and
/// whitespace-insensitive) routes to [`ExecutorTier::Local`]; every other
/// value, including absence, stays on [`ExecutorTier::Frontier`].
pub fn task_class_to_tier(task_class: Option<&str>) -> ExecutorTier {
    match task_class.map(|s| s.trim().to_ascii_lowercase()) {
        Some(ref s) if s == "execution-class" => ExecutorTier::Local,
        _ => ExecutorTier::Frontier,
    }
}

/// An operator-configured OpenAI-compatible endpoint the Worker can be routed
/// to for the local tier. Mirrors [`crate::types::RoleConfig`]'s local-backend
/// fields (`base_url`/`context_budget`/`temperature`).
#[derive(Debug, Clone, PartialEq)]
pub struct LocalEndpoint {
    pub base_url: String,
    pub context_budget: u32,
    pub temperature: Option<f64>,
}

/// Apply executor-tier routing to a mission config at seed time, so a fresh
/// mission's `mission.created` config already reflects the routing decision.
/// Pure: never touches `config.validator_scrutiny` or `config.validator_functional`.
///
/// Returns the APPLIED tier, which may differ from the requested `tier`: a
/// `Local` request with no configured endpoint fails safe to `Frontier`
/// (leaving the Worker on its frontier default) rather than routing to an
/// endpoint that doesn't exist. A configured dispatch pool
/// (`worker_candidates`) also pins `Frontier`: the pool is an explicit
/// per-candidate backend declaration, and local routing's rewrite of
/// `worker.backend` would sit next to it as a dead, misleading key (pool
/// candidates are never local-backed — validation rejects `local` entries).
pub fn apply_executor_routing(
    config: &mut MissionConfig,
    tier: ExecutorTier,
    local_endpoint: Option<&LocalEndpoint>,
) -> ExecutorTier {
    if !config.worker_candidates.is_empty() {
        return ExecutorTier::Frontier;
    }
    match (tier, local_endpoint) {
        (ExecutorTier::Frontier, _) => ExecutorTier::Frontier,
        (ExecutorTier::Local, None) => ExecutorTier::Frontier,
        (ExecutorTier::Local, Some(endpoint)) => {
            config.worker.backend = Some("local".to_string());
            config.worker.base_url = Some(endpoint.base_url.clone());
            config.worker.context_budget = Some(endpoint.context_budget);
            config.worker.temperature = endpoint.temperature;
            config.allow_below_default_worker_model = true;
            ExecutorTier::Local
        }
    }
}

/// Route the executor tier for a mission seeded from `ticket`, so the
/// resulting `mission.created` config already reflects the routing decision
/// — the single engine-side entry point both the `kranz draft` and
/// `kranz exec` seed paths call before [`crate::orchestrator::MissionEngine::create`].
/// Returns the applied tier and a decision summary to record against the
/// mission once it exists.
pub fn route_ticket_executor(
    cfg: &mut MissionConfig,
    ticket: &crate::ticket::Ticket,
) -> (ExecutorTier, &'static str) {
    route_task_class_executor(cfg, ticket.task_class.as_deref())
}

/// Core of [`route_ticket_executor`], taking the raw `task-class` string
/// directly. [`crate::orchestrator::MissionEngine::create`] calls this with
/// the class recovered from its `goal` argument via
/// [`crate::ticket::parse_task_class_from_goal`] — `create` only ever sees a
/// folded goal string, never the originating [`crate::ticket::Ticket`], so
/// the class has to travel through that one channel.
///
/// The routing table (KRZ-331): when `cfg.routing` declares rules, they are
/// the floor — resolved deterministically by [`crate::routing::table_tier`]
/// (first match wins, no match stays Frontier). An EMPTY table keeps the
/// hardcoded literal floor ([`task_class_to_tier`]) byte-for-byte, so a
/// config that never heard of the table routes exactly as before.
pub fn route_task_class_executor(
    cfg: &mut MissionConfig,
    task_class: Option<&str>,
) -> (ExecutorTier, &'static str) {
    let table_configured = !cfg.routing.task_class_rules.is_empty();
    let requested = if table_configured {
        crate::routing::table_tier(&cfg.routing, task_class)
    } else {
        task_class_to_tier(task_class)
    };
    let local_endpoint = match (&cfg.worker.base_url, cfg.worker.context_budget) {
        (Some(base_url), Some(context_budget)) => Some(LocalEndpoint {
            base_url: base_url.clone(),
            context_budget,
            temperature: cfg.worker.temperature,
        }),
        _ => None,
    };
    let applied = apply_executor_routing(cfg, requested, local_endpoint.as_ref());
    let summary = match (requested, applied, table_configured) {
        (ExecutorTier::Local, ExecutorTier::Local, true) => {
            "executor routed local (routing-table rule)"
        }
        (ExecutorTier::Local, ExecutorTier::Local, false) => {
            "executor routed local (execution-class)"
        }
        (ExecutorTier::Local, ExecutorTier::Frontier, true) => {
            "routing-table rule routes local but no local endpoint configured; executor stays frontier"
        }
        (ExecutorTier::Local, ExecutorTier::Frontier, false) => {
            "execution-class ticket but no local endpoint configured; executor stays frontier"
        }
        _ => "executor stays frontier",
    };
    (applied, summary)
}

/// The backend-native model used when an older config selected a non-Claude
/// backend but left the role's Claude default model in place. `Local` has no
/// backend default: local model ids are free-form and sent to the endpoint
/// verbatim, with no Claude→backend rewrite.
fn backend_default_model(kind: BackendKind) -> Option<&'static str> {
    match kind {
        BackendKind::Claude => None,
        BackendKind::Codex => Some(DEFAULT_CODEX_MODEL),
        BackendKind::Droid => Some(DEFAULT_DROID_MODEL),
        BackendKind::Kimi => Some(DEFAULT_KIMI_MODEL),
        BackendKind::Local => None,
        // ACP has no standard model-selection parameter in v1: the peer's
        // model is its own concern (encoded in acpCommand/acpArgs), so there
        // is no backend default to rewrite to.
        BackendKind::Acp => None,
        BackendKind::Cursor => Some(DEFAULT_CURSOR_MODEL),
    }
}

fn role_default_model(role: Role) -> &'static str {
    match role {
        Role::Orchestrator | Role::ValidatorScrutiny => "opus",
        Role::Worker | Role::ValidatorFunctional => "sonnet",
    }
}

/// Return the model actually sent to the backend for this role selection.
///
/// This preserves the existing scrutiny-backend backcompat: a config that set
/// only `validatorScrutiny.backend = "codex"` or `"droid"` used to inherit
/// the Claude default model and then be rewritten to the backend default at
/// dispatch. The same rule is now role-wide.
pub fn effective_model(role: Role, kind: BackendKind, configured: &str) -> String {
    if kind != BackendKind::Claude && configured == role_default_model(role) {
        if let Some(default_model) = backend_default_model(kind) {
            return default_model.to_string();
        }
    }
    configured.to_string()
}

/// Classify a validated backend/model pair. `None` means this model is not a
/// supported model for the selected backend.
pub fn model_tier(kind: BackendKind, model: &str) -> Option<ModelTier> {
    let m = model.trim().to_ascii_lowercase();
    if m.is_empty() {
        return None;
    }
    match kind {
        BackendKind::Claude => {
            if m == "haiku" || m.contains("haiku") {
                Some(ModelTier::BelowDefault)
            } else if m == "sonnet" || m.contains("sonnet") {
                Some(ModelTier::Default)
            } else if m == "opus" || m.contains("opus") || m == "fable" || m.contains("fable") {
                Some(ModelTier::Frontier)
            } else {
                None
            }
        }
        BackendKind::Codex => {
            if m == "codex" || m == DEFAULT_CODEX_MODEL || m.starts_with("gpt-5") {
                Some(ModelTier::Frontier)
            } else {
                None
            }
        }
        BackendKind::Droid => {
            if m == DEFAULT_DROID_MODEL || m.contains("glm") || m.contains("fireworks") {
                Some(ModelTier::BelowDefault)
            } else if m == "fable" || m.contains("fable") {
                Some(ModelTier::Frontier)
            } else {
                None
            }
        }
        BackendKind::Kimi => {
            if m == DEFAULT_KIMI_MODEL {
                Some(ModelTier::Frontier)
            } else if m == "kimi-code/kimi-for-coding" || m == "kimi-code/kimi-for-coding-highspeed"
            {
                Some(ModelTier::BelowDefault)
            } else {
                None
            }
        }
        // Local model ids are free-form and cannot be allowlisted, so every
        // non-empty model classifies uniformly below-default: workers need
        // the allowBelowDefaultWorkerModel opt-in, and a local orchestrator
        // always fails the frontier floor.
        BackendKind::Local => Some(ModelTier::BelowDefault),
        // ACP model ids are equally free-form (the string is recorded for
        // attribution only; ACP v1 has no model-selection parameter), so the
        // same uniform below-default classification applies.
        BackendKind::Acp => Some(ModelTier::BelowDefault),
        // Cursor model ids are drawn from an account-specific catalog
        // (~190 entries on the probe account; `--list-models` output varies
        // by entitlement), so no client-side allowlist is possible and every
        // non-empty id classifies uniformly below-default: a cursor worker
        // needs the allowBelowDefaultWorkerModel opt-in (a deliberate gate
        // for a validator-first backend), and the orchestrator stays on its
        // frontier floor. Model-availability failures themselves are
        // diagnosed deterministically at session start (probe item 5).
        BackendKind::Cursor => Some(ModelTier::BelowDefault),
    }
}

/// Classify the role's configured selection after applying legacy/default
/// model normalization.
pub fn role_model_tier(cfg: &MissionConfig, role: Role) -> Option<ModelTier> {
    let kind = cfg.backend_kind(role);
    let model = effective_model(role, kind, &cfg.role(role).model);
    model_tier(kind, &model)
}

/// Load the effective config for a repo: defaults, then the global file,
/// then the project file (later layers win). Missing files are fine;
/// unreadable or unparseable files are a [`EngineError::Config`] naming the
/// offending path.
pub fn load(repo_root: &Path) -> Result<MissionConfig> {
    let mut layers: Vec<PathBuf> = Vec::new();
    if let Some(global) = paths::global_config() {
        layers.push(global);
    }
    layers.push(paths::project_config(repo_root));
    load_layers(&layers)
}

/// Merge the given config files (in order, later wins) over the compiled-in
/// defaults. Exposed so callers (and tests) can supply explicit layer paths
/// instead of the real home directory.
pub fn load_layers(layers: &[PathBuf]) -> Result<MissionConfig> {
    let mut merged = serde_json::to_value(MissionConfig::default())?;

    for path in layers {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            // Absent layers are simply skipped; anything else is an error.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(EngineError::Config(format!(
                    "cannot read config file {}: {e}",
                    path.display()
                )))
            }
        };

        let patch: serde_json::Value = serde_json::from_str(&text).map_err(|e| {
            EngineError::Config(format!(
                "invalid JSON in config file {}: {e}",
                path.display()
            ))
        })?;

        if !patch.is_object() {
            return Err(EngineError::Config(format!(
                "config file {} must contain a JSON object at the top level",
                path.display()
            )));
        }

        deep_merge(&mut merged, &patch);
    }

    serde_json::from_value(merged)
        .map_err(|e| EngineError::Config(format!("merged configuration does not deserialize: {e}")))
}

/// Recursively merge `patch` into `base`: objects merge key-wise, everything
/// else (scalars, arrays, nulls) is replaced wholesale by the patch value.
///
/// Public because the server/CLI reuse it for `config.changed` patches.
pub fn deep_merge(base: &mut serde_json::Value, patch: &serde_json::Value) {
    match (base, patch) {
        (serde_json::Value::Object(base_map), serde_json::Value::Object(patch_map)) => {
            for (key, patch_val) in patch_map {
                match base_map.get_mut(key) {
                    Some(slot) => deep_merge(slot, patch_val),
                    None => {
                        base_map.insert(key.clone(), patch_val.clone());
                    }
                }
            }
        }
        (slot, patch_val) => *slot = patch_val.clone(),
    }
}

/// Apply a partial JSON patch to an effective mission config and validate the
/// merged result exactly as the engine would before accepting it.
///
/// Submission surfaces use this before enqueueing `config-change`, while the
/// engine repeats the check when it drains the command. The second check is
/// still required because another queued patch may win the race in between.
pub fn apply_validated_patch(
    current: &MissionConfig,
    patch: &serde_json::Value,
) -> Result<MissionConfig> {
    let mut value = serde_json::to_value(current)?;
    deep_merge(&mut value, patch);
    let merged: MissionConfig = serde_json::from_value(value)
        .map_err(|e| EngineError::Config(format!("patch produces invalid config: {e}")))?;
    validate(&merged)?;
    Ok(merged)
}

/// Validate invariants the engine relies on (plan §6). Returns
/// [`EngineError::Config`] describing the first violation found.
pub fn validate(cfg: &MissionConfig) -> Result<()> {
    let roles = [
        ("orchestrator", &cfg.orchestrator),
        ("worker", &cfg.worker),
        ("validatorScrutiny", &cfg.validator_scrutiny),
        ("validatorFunctional", &cfg.validator_functional),
    ];
    for (name, role) in roles {
        if !VALID_EFFORTS.contains(&role.reasoning_effort.as_str()) {
            return Err(EngineError::Config(format!(
                "{name}.reasoningEffort must be one of {VALID_EFFORTS:?}, got {:?}",
                role.reasoning_effort
            )));
        }
    }

    if cfg.max_fix_cycles_per_milestone < 1 {
        return Err(EngineError::Config(
            "maxFixCyclesPerMilestone must be at least 1".into(),
        ));
    }

    if cfg.max_respawns > 5 {
        return Err(EngineError::Config(format!(
            "maxRespawns must be at most 5, got {}",
            cfg.max_respawns
        )));
    }

    if !(10..=5000).contains(&cfg.event_stream_throttle_ms) {
        return Err(EngineError::Config(format!(
            "eventStreamThrottleMs must be in 10..=5000, got {}",
            cfg.event_stream_throttle_ms
        )));
    }
    if !cfg.considered_alternatives_high_usd_threshold.is_finite()
        || cfg.considered_alternatives_high_usd_threshold < 0.0
    {
        return Err(EngineError::Config(format!(
            "consideredAlternativesHighUsdThreshold must be finite and non-negative, got {}",
            cfg.considered_alternatives_high_usd_threshold
        )));
    }

    // Parallel workers (roadmap M3): `1` (the default) keeps the sequential
    // run loop byte-for-byte; `2..=8` opts into parallel-within-milestone
    // execution (independent features run concurrently, each in its own git
    // worktree, then merge in declared order). `0` is meaningless (no worker
    // can ever run) and anything above 8 is well past any useful fan-out for a
    // single repo, so both are rejected.
    if !(1..=8).contains(&cfg.max_parallel_workers) {
        return Err(EngineError::Config(format!(
            "maxParallelWorkers must be in 1..=8 (1 = sequential; >1 opts into M3 \
             parallel workers), got {}",
            cfg.max_parallel_workers
        )));
    }

    // Heterogeneous dispatch pool (ticket heterogeneous-dispatch-pool,
    // KRZ-303): `workerCandidates` is the COMPLETE backend list the worker
    // role fans out to (worker.backend applies only when the list is empty).
    // Every entry is checked by the same rules as the worker role itself —
    // known backend, supported backend/model pair, the worker model floor,
    // and the sandbox fail-closed pairs — because each one WILL drive real
    // worker sessions.
    if cfg.worker_candidates.len() == 1 {
        return Err(EngineError::Config(
            "workerCandidates with exactly one entry is a roundabout worker.backend; \
             use worker.backend (the pool exists for N >= 2 heterogeneous candidates)"
                .into(),
        ));
    }
    if cfg.worker_candidates.len() > MAX_WORKER_CANDIDATES {
        return Err(EngineError::Config(format!(
            "workerCandidates supports at most {MAX_WORKER_CANDIDATES} candidates, got {}",
            cfg.worker_candidates.len()
        )));
    }
    // The pool and M3 parallel features are two different fan-out models
    // (same unit to N backends vs N units to one backend each). Combining
    // them has no defined semantics in this pass — reject rather than pick
    // one silently.
    if !cfg.worker_candidates.is_empty() && cfg.max_parallel_workers > 1 {
        return Err(EngineError::Config(
            "workerCandidates (dispatch pool: one unit to N backends) and \
             maxParallelWorkers > 1 (M3: N independent units concurrently) are mutually \
             exclusive in this pass; configure one fan-out model"
                .into(),
        ));
    }
    for (i, candidate) in cfg.worker_candidates.iter().enumerate() {
        let kind = parse_backend(Some(&candidate.backend)).map_err(|other| {
            EngineError::Config(format!(
                "workerCandidates[{i}].backend must be one of \"claude\", \"codex\", \"droid\", \"kimi\", \"cursor\", got {other:?}"
            ))
        })?;
        // local/acp need per-role endpoint/command config (baseUrl /
        // contextBudget / acpCommand) that has no per-candidate home in this
        // pass; refuse rather than silently share the worker role's.
        if matches!(kind, BackendKind::Local | BackendKind::Acp) {
            return Err(EngineError::Config(format!(
                "workerCandidates[{i}].backend {:?} is not supported in this pass: local/acp \
                 need per-candidate endpoint/command config (a deliberate widening); use \
                 claude, codex, droid, kimi, or cursor candidates",
                candidate.backend
            )));
        }
        // Same fail-closed sandbox pair as the worker role: a candidate that
        // cannot honor the requested enforcement must never run with the
        // operator believing it contained.
        if cfg.worker.sandbox.enforce != SandboxEnforce::Off && !kind.supports_sandbox_enforcement()
        {
            return Err(EngineError::Config(format!(
                "workerCandidates[{i}].backend {:?} cannot honor sandbox.enforce={:?}: only the \
                 claude backend applies the resolved OS sandbox; run with sandbox.enforce=off, \
                 or drop the non-claude candidate",
                candidate.backend,
                cfg.worker.sandbox.enforce.as_str()
            )));
        }
        let effective = effective_model(Role::Worker, kind, &candidate.model);
        let tier = model_tier(kind, &effective).ok_or_else(|| {
            EngineError::Config(format!(
                "workerCandidates[{i}] effective model {effective:?} (configured as {:?}) is not supported by backend {:?}",
                candidate.model,
                candidate.backend
            ))
        })?;
        // The worker model floor applies per candidate — a below-default
        // stream is exactly as much a worker session as the role's own.
        if tier < ModelTier::Default && !cfg.allow_below_default_worker_model {
            return Err(EngineError::Config(format!(
                "workerCandidates[{i}] effective model {effective:?} (configured as {:?}) on backend {:?} is below the default worker tier; set \
                 allowBelowDefaultWorkerModel=true on this mission to opt in",
                candidate.model,
                candidate.backend
            )));
        }
    }

    // Backend routing table (ticket `backend-routing-abstraction`, KRZ-331):
    // shape-only checks (blank/duplicate task classes) live in
    // `routing::validate_table` and fail closed naming the offending rule. A
    // rule routing `local` with no endpoint configured is NOT an error here:
    // `apply_executor_routing` already fails safe to Frontier for exactly
    // that case, with the decision recorded against the mission.
    if let Err(err) = crate::routing::validate_table(&cfg.routing) {
        return Err(EngineError::Config(err));
    }

    for (role, name) in [
        (Role::Orchestrator, "orchestrator"),
        (Role::Worker, "worker"),
        (Role::ValidatorScrutiny, "validatorScrutiny"),
        (Role::ValidatorFunctional, "validatorFunctional"),
    ] {
        let role_cfg = cfg.role(role);
        let kind = parse_backend(role_cfg.backend.as_deref()).map_err(|other| {
            EngineError::Config(format!(
                "{name}.backend must be one of None, \"claude\", \"codex\", \"droid\", \"kimi\", \"local\", \"acp\", \"cursor\", got {other:?}"
            ))
        })?;
        // Fail closed on a silently-unenforced sandbox: only the claude
        // backend wraps its sessions in the engine-resolved OS sandbox
        // (fs/extraWrite/egress policy, container provider); every other
        // backend spawns unsandboxed and discards the requested enforcement.
        // Reject the pair at validation so a mission never runs with the
        // operator believing workers are contained when they are not.
        if role_cfg.sandbox.enforce != SandboxEnforce::Off && !kind.supports_sandbox_enforcement() {
            return Err(EngineError::Config(format!(
                "{name}.backend {:?} cannot honor sandbox.enforce={:?}: only the claude backend \
                 applies the resolved OS sandbox; run with sandbox.enforce=off to proceed \
                 unsandboxed, or use the claude backend",
                kind.as_str(),
                role_cfg.sandbox.enforce.as_str()
            )));
        }
        // Fail closed on an advisory-only network boundary, the same posture
        // as the backend check above: container `fs+net` with a non-empty
        // egress list keeps the runtime's default bridge and routes egress by
        // proxy env vars only, so a process that ignores them opens a direct
        // socket past the "enforced" allowlist. Reject the pair until the
        // internal-network/sidecar boundary exists; an empty egress list
        // (`--network none`) stays accepted because that boundary is hard.
        if role_cfg.sandbox.enforce == SandboxEnforce::FsNet
            && !role_cfg
                .sandbox
                .provider
                .enforces_hard_net_boundary(&role_cfg.sandbox.egress)
        {
            return Err(EngineError::Config(format!(
                "{name}.sandbox provider {:?} with sandbox.enforce={:?} and a non-empty egress \
                 list is advisory-only: the runtime's default bridge lets a process that ignores \
                 the proxy env vars open a direct socket past the egress filter; use an empty \
                 egress list (the hard `--network none` boundary), sandbox.provider \"process\", \
                 or sandbox.enforce=off until the internal-network/sidecar boundary lands \
                 (docs/scoping/worker-sandboxing.md tier 3)",
                role_cfg.sandbox.provider.as_str(),
                role_cfg.sandbox.enforce.as_str()
            )));
        }
        let effective = effective_model(role, kind, &role_cfg.model);
        let tier = model_tier(kind, &effective).ok_or_else(|| {
            EngineError::Config(format!(
                "{name} effective model {effective:?} (configured as {:?}) is not supported by backend {:?}",
                role_cfg.model,
                kind.as_str()
            ))
        })?;

        if kind == BackendKind::Local {
            // Guarded validator role split (ticket
            // `local-inference-validator-guarded`, KRZ-206b; review addendum
            // §4 of docs/scoping/local-inference-executor-tier.md): the local
            // validator tier exists for DETERMINISTIC mechanical checks only
            // — compile/test/lint exit codes and contract-command pass/fail,
            // where the engine runs the command itself and the model only
            // reads verbatim PASS/FAIL evidence. Scrutiny is judgment (diff
            // review against criteria), and routing judgment local is exactly
            // the "silent green" attack the split exists to prevent: a weak
            // local validator that wrongly PASSES bad work never looks like a
            // failure, so no escalation valve ever fires on it. Only the
            // functional role may pair with the local backend — and every
            // local functional PASS is frontier-confirmed before it greens a
            // gate (confirm-on-pass in the validation round); the scrutiny
            // role is rejected outright here. Checked FIRST, before the
            // endpoint fields, so the error names the real problem.
            if role == Role::ValidatorScrutiny {
                return Err(EngineError::Config(format!(
                    "{name}.backend \"local\" is rejected: scrutiny is judgment, not a \
                     deterministic mechanical check, and the local validator tier is the \
                     functional role only (KRZ-206b) — a local judgment PASS is the \
                     silent-green failure mode the guarded role split exists to prevent"
                )));
            }
            match role_cfg.base_url.as_deref() {
                Some(url) if !url.trim().is_empty() => {
                    let rest = url
                        .strip_prefix("http://")
                        .or_else(|| url.strip_prefix("https://"));
                    let has_host = rest.is_some_and(|rest| {
                        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
                        let after_userinfo = match authority.rfind('@') {
                            Some(idx) => &authority[idx + 1..],
                            None => authority,
                        };
                        let host = after_userinfo.split(':').next().unwrap_or(after_userinfo);
                        !host.is_empty()
                    });
                    if !has_host {
                        return Err(EngineError::Config(format!(
                            "{name}.baseUrl {url:?} is not a valid http/https URL"
                        )));
                    }
                }
                _ => {
                    return Err(EngineError::Config(format!(
                        "{name}.baseUrl is required when {name}.backend is \"local\""
                    )));
                }
            }

            match role_cfg.context_budget {
                Some(budget) if (1024..=200_000).contains(&budget) => {}
                Some(budget) => {
                    return Err(EngineError::Config(format!(
                        "{name}.contextBudget must be in 1024..=200000, got {budget}"
                    )));
                }
                None => {
                    return Err(EngineError::Config(format!(
                        "{name}.contextBudget is required when {name}.backend is \"local\""
                    )));
                }
            }

            if let Some(temperature) = role_cfg.temperature {
                if !temperature.is_finite() || !(0.0..=2.0).contains(&temperature) {
                    return Err(EngineError::Config(format!(
                        "{name}.temperature must be finite and in 0.0..=2.0, got {temperature}"
                    )));
                }
            }
        }

        if kind == BackendKind::Acp {
            // KRZ-301 lands worker-first: the orchestrator needs
            // streaming-input + resume semantics this backend deliberately
            // rejects at the seam, and the validator roles wait for live
            // soak — refuse those pairings here rather than degrading
            // mid-mission.
            if role != Role::Worker {
                return Err(EngineError::Config(format!(
                    "{name}.backend \"acp\" is supported for the worker role only in this pass \
                     (KRZ-301); validators and the orchestrator stay on their existing backends"
                )));
            }
            match role_cfg.acp_command.as_deref() {
                Some(command) if !command.trim().is_empty() => {}
                _ => {
                    return Err(EngineError::Config(format!(
                        "{name}.acpCommand is required when {name}.backend is \"acp\" \
                         (the ACP agent executable; extra argv goes in {name}.acpArgs)"
                    )));
                }
            }
        }

        // Kimi is the first backend where reasoning effort is model-constrained:
        // k3 (the thinking-capable flagship) only supports low/high/max, while
        // kimi-for-coding[-highspeed] (not thinking-capable) impose no effort
        // constraint.
        if kind == BackendKind::Kimi
            && effective == DEFAULT_KIMI_MODEL
            && !["low", "high", "max"].contains(&role_cfg.reasoning_effort.as_str())
        {
            return Err(EngineError::Config(format!(
                "{name}.reasoningEffort must be one of [\"low\", \"high\", \"max\"] for kimi model {DEFAULT_KIMI_MODEL:?}, got {:?}",
                role_cfg.reasoning_effort
            )));
        }

        if role == Role::Worker
            && tier < ModelTier::Default
            && !cfg.allow_below_default_worker_model
        {
            return Err(EngineError::Config(format!(
                "worker effective model {effective:?} (configured as {:?}) on backend {:?} is below the default worker tier; set \
                 allowBelowDefaultWorkerModel=true on this mission to opt in",
                role_cfg.model,
                kind.as_str()
            )));
        }

        if role == Role::Orchestrator && tier < ModelTier::Frontier {
            return Err(EngineError::Config(format!(
                "orchestrator effective model {effective:?} (configured as {:?}) on backend {:?} is below the frontier-model floor",
                role_cfg.model,
                kind.as_str()
            )));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_planning_idle_release_minutes_is_30() {
        assert_eq!(MissionConfig::default().planning_idle_release_minutes, 30);
    }

    #[test]
    fn default_serializes_camel_case_planning_idle_release_minutes() {
        let value = serde_json::to_value(MissionConfig::default()).unwrap();
        assert_eq!(value["planningIdleReleaseMinutes"], 30);
    }

    #[test]
    fn layer_overrides_planning_idle_release_minutes() {
        let dir = tempfile::tempdir().unwrap();
        let layer_path = dir.path().join("config.json");
        std::fs::write(&layer_path, r#"{"planningIdleReleaseMinutes": 5}"#).unwrap();

        let cfg = load_layers(&[layer_path]).unwrap();
        assert_eq!(cfg.planning_idle_release_minutes, 5);
    }

    #[test]
    fn absent_key_in_layer_keeps_default() {
        let dir = tempfile::tempdir().unwrap();
        let layer_path = dir.path().join("config.json");
        std::fs::write(&layer_path, r#"{"maxRespawns": 3}"#).unwrap();

        let cfg = load_layers(&[layer_path]).unwrap();
        assert_eq!(cfg.planning_idle_release_minutes, 30);
    }

    #[test]
    fn default_auto_work_is_false() {
        assert!(!MissionConfig::default().auto_work);
    }

    #[test]
    fn default_serializes_camel_case_auto_work() {
        let value = serde_json::to_value(MissionConfig::default()).unwrap();
        assert_eq!(value["autoWork"], false);
    }

    #[test]
    fn default_serializes_camel_case_considered_alternatives_thresholds() {
        let value = serde_json::to_value(MissionConfig::default()).unwrap();
        assert_eq!(value["consideredAlternativesFeatureThreshold"], 4);
        assert_eq!(value["consideredAlternativesTouchSetThreshold"], 4);
        assert_eq!(value["consideredAlternativesHighUsdThreshold"], 0.0);
    }

    #[test]
    fn layer_overrides_considered_alternatives_thresholds() {
        let dir = tempfile::tempdir().unwrap();
        let layer_path = dir.path().join("config.json");
        std::fs::write(
            &layer_path,
            r#"{
                "consideredAlternativesFeatureThreshold": 2,
                "consideredAlternativesTouchSetThreshold": 3,
                "consideredAlternativesHighUsdThreshold": 9.5
            }"#,
        )
        .unwrap();

        let cfg = load_layers(&[layer_path]).unwrap();
        assert_eq!(cfg.considered_alternatives_feature_threshold, 2);
        assert_eq!(cfg.considered_alternatives_touch_set_threshold, 3);
        assert_eq!(cfg.considered_alternatives_high_usd_threshold, 9.5);
    }

    #[test]
    fn default_serializes_camel_case_worker_floor_opt_in() {
        let value = serde_json::to_value(MissionConfig::default()).unwrap();
        assert_eq!(value["allowBelowDefaultWorkerModel"], false);
    }

    #[test]
    fn layer_overrides_auto_work() {
        let dir = tempfile::tempdir().unwrap();
        let layer_path = dir.path().join("config.json");
        std::fs::write(&layer_path, r#"{"autoWork": true}"#).unwrap();

        let cfg = load_layers(&[layer_path]).unwrap();
        assert!(cfg.auto_work);
    }

    #[test]
    fn contract_env_passthrough_defaults_empty_and_parses_camel_case() {
        // Additive contract change: absent key (every pre-existing config and
        // every old mission.created event payload) deserializes to empty.
        assert!(MissionConfig::default().contract_env_passthrough.is_empty());
        let value = serde_json::to_value(MissionConfig::default()).unwrap();
        assert_eq!(value["contractEnvPassthrough"], serde_json::json!([]));

        let dir = tempfile::tempdir().unwrap();
        let layer_path = dir.path().join("config.json");
        std::fs::write(
            &layer_path,
            r#"{"contractEnvPassthrough": ["NPM_TOKEN", "REGISTRY_BASIC_AUTH"]}"#,
        )
        .unwrap();
        let cfg = load_layers(&[layer_path]).unwrap();
        assert_eq!(
            cfg.contract_env_passthrough,
            vec!["NPM_TOKEN".to_string(), "REGISTRY_BASIC_AUTH".to_string()]
        );
        // A layer naming unrelated keys only (the old-config shape) leaves
        // the passthrough empty.
        let layer_path = dir.path().join("config-old.json");
        std::fs::write(&layer_path, r#"{"maxRespawns": 3}"#).unwrap();
        let cfg = load_layers(&[layer_path]).unwrap();
        assert!(cfg.contract_env_passthrough.is_empty());
    }

    #[test]
    fn absent_auto_work_key_keeps_default() {
        let dir = tempfile::tempdir().unwrap();
        let layer_path = dir.path().join("config.json");
        std::fs::write(&layer_path, r#"{"maxRespawns": 3}"#).unwrap();

        let cfg = load_layers(&[layer_path]).unwrap();
        assert!(!cfg.auto_work);
    }

    /// Composition audit (ticket `config-fail-open-audit`): layered config
    /// arrays REPLACE wholesale (deep_merge semantics — a project layer
    /// overrides a global layer's list). That replace is safe ONLY because
    /// the deny floor is compiled in: `denyPatterns` from any layer can
    /// replace another layer's entries but can never strip the built-in
    /// worker deny list, which `permissions::for_role` appends to. This pins
    /// both halves of the contract: the documented replace semantics, and
    /// the floor's unreachability by replacement.
    #[test]
    fn composition_audit_layered_deny_patterns_replace_but_never_strip_the_builtin_floor() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.json");
        std::fs::write(&global, r#"{"denyPatterns": ["git push --force"]}"#).unwrap();
        let project = dir.path().join("project.json");
        std::fs::write(&project, r#"{"denyPatterns": ["rm -rf *"]}"#).unwrap();

        let cfg = load_layers(&[global, project]).unwrap();
        // Replace semantics across layers: the later list wins wholesale.
        assert_eq!(cfg.deny_patterns, vec!["rm -rf *".to_string()]);

        // The built-in §4.7 floor is compiled in, so no layer shape can
        // remove it: the worker profile carries every built-in rule plus
        // (only) the winning layer's custom entry.
        let profile = crate::permissions::for_role(Role::Worker, &cfg, &[], &[], &[]);
        for builtin in [
            "Bash(git push*)",
            "Bash(sudo*)",
            "Bash(curl*)",
            "WebFetch",
            "WebSearch",
        ] {
            assert!(
                profile.disallowed_tools.iter().any(|r| r == builtin),
                "the built-in deny {builtin} must survive layered replacement"
            );
        }
        assert!(profile
            .disallowed_tools
            .iter()
            .any(|r| r == "Bash(rm -rf *)"));
        assert!(!profile
            .disallowed_tools
            .iter()
            .any(|r| r == "Bash(git push --force*)"));
    }

    #[test]
    fn default_config_serializes_without_backend_field() {
        let value = serde_json::to_value(MissionConfig::default()).unwrap();
        for role in [
            "orchestrator",
            "worker",
            "validatorScrutiny",
            "validatorFunctional",
        ] {
            let obj = value[role].as_object().unwrap();
            assert!(
                !obj.contains_key("backend"),
                "{role} should not serialize a backend key by default"
            );
        }
    }

    #[test]
    fn validate_accepts_known_backends_for_each_role() {
        for role in [
            Role::Orchestrator,
            Role::Worker,
            Role::ValidatorScrutiny,
            Role::ValidatorFunctional,
        ] {
            for backend in [None, Some("claude"), Some("codex")] {
                let mut cfg = MissionConfig::default();
                cfg.role_mut_for_test(role).backend = backend.map(|s| s.to_string());
                assert!(
                    validate(&cfg).is_ok(),
                    "{role:?} backend {backend:?} should be accepted"
                );
            }
        }
    }

    #[test]
    fn validate_accepts_droid_scrutiny_backend_with_legacy_default_model() {
        let mut cfg = MissionConfig::default();
        cfg.validator_scrutiny.backend = Some("droid".into());
        assert!(validate(&cfg).is_ok());
        assert_eq!(
            effective_model(
                Role::ValidatorScrutiny,
                BackendKind::Droid,
                &cfg.validator_scrutiny.model
            ),
            DEFAULT_DROID_MODEL
        );
    }

    #[test]
    fn validate_rejects_unknown_backend_on_any_role() {
        for role in [
            Role::Orchestrator,
            Role::Worker,
            Role::ValidatorScrutiny,
            Role::ValidatorFunctional,
        ] {
            let mut cfg = MissionConfig::default();
            cfg.role_mut_for_test(role).backend = Some("gemini".into());
            assert!(validate(&cfg).is_err(), "{role:?} should reject gemini");
        }
    }

    #[test]
    fn validate_rejects_unknown_backend_model_combos() {
        let mut cfg = MissionConfig::default();
        cfg.worker.model = "kranz-test-model".into();
        assert!(validate(&cfg).is_err());

        let mut cfg = MissionConfig::default();
        cfg.validator_functional.backend = Some("codex".into());
        cfg.validator_functional.model = "claude-sonnet-5".into();
        assert!(validate(&cfg).is_err());

        let mut cfg = MissionConfig::default();
        cfg.validator_scrutiny.backend = Some("droid".into());
        cfg.validator_scrutiny.model = "gpt-5-codex".into();
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn validate_enforces_worker_floor_with_explicit_opt_in() {
        let mut cfg = MissionConfig::default();
        cfg.worker.model = "haiku".into();
        assert!(validate(&cfg).is_err());
        cfg.allow_below_default_worker_model = true;
        assert!(validate(&cfg).is_ok());

        let mut cfg = MissionConfig::default();
        cfg.worker.backend = Some("droid".into());
        assert!(
            validate(&cfg).is_err(),
            "droid's legacy default GLM worker is below the default tier"
        );
        cfg.allow_below_default_worker_model = true;
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn floor_violations_lead_with_the_effective_model() {
        // A role that keeps its default model on a non-Claude backend runs
        // the backend default, not the configured name — floor messages must
        // lead with that effective model so a revert to naming only the
        // configured model cannot ship silently.
        let mut cfg = MissionConfig::default();
        cfg.worker.backend = Some("droid".into());
        let configured = cfg.worker.model.clone();
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(
            err.contains(&format!("worker effective model {DEFAULT_DROID_MODEL:?}")),
            "{err}"
        );
        assert!(
            err.contains(&format!("(configured as {configured:?})")),
            "{err}"
        );

        let mut cfg = MissionConfig::default();
        cfg.orchestrator.backend = Some("droid".into());
        let configured = cfg.orchestrator.model.clone();
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(
            err.contains(&format!(
                "orchestrator effective model {DEFAULT_DROID_MODEL:?}"
            )),
            "{err}"
        );
        assert!(
            err.contains(&format!("(configured as {configured:?})")),
            "{err}"
        );
    }

    #[test]
    fn validate_enforces_orchestrator_frontier_floor() {
        let mut cfg = MissionConfig::default();
        cfg.orchestrator.model = "sonnet".into();
        assert!(validate(&cfg).is_err());

        let mut cfg = MissionConfig::default();
        cfg.orchestrator.backend = Some("droid".into());
        assert!(
            validate(&cfg).is_err(),
            "droid's legacy default GLM model is not a planner frontier model"
        );

        let mut cfg = MissionConfig::default();
        cfg.orchestrator.backend = Some("droid".into());
        cfg.orchestrator.model = "claude-fable-5".into();
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn validate_allows_scrutiny_on_any_supported_tier() {
        for (backend, model) in [
            (Some("claude"), "haiku"),
            (Some("claude"), "sonnet"),
            (Some("claude"), "opus"),
            (Some("codex"), DEFAULT_CODEX_MODEL),
            (Some("droid"), DEFAULT_DROID_MODEL),
            (Some("droid"), "claude-fable-5"),
            (Some("kimi"), DEFAULT_KIMI_MODEL),
            (Some("kimi"), "kimi-code/kimi-for-coding"),
        ] {
            let mut cfg = MissionConfig::default();
            cfg.validator_scrutiny.backend = backend.map(|s| s.to_string());
            cfg.validator_scrutiny.model = model.to_string();
            assert!(
                validate(&cfg).is_ok(),
                "scrutiny should accept {backend:?} / {model}"
            );
        }
    }

    #[test]
    fn validate_accepts_kimi_k3_for_supported_efforts() {
        for effort in ["low", "high", "max"] {
            let mut cfg = MissionConfig::default();
            cfg.validator_scrutiny.backend = Some("kimi".into());
            cfg.validator_scrutiny.model = DEFAULT_KIMI_MODEL.into();
            cfg.validator_scrutiny.reasoning_effort = effort.into();
            assert!(
                validate(&cfg).is_ok(),
                "kimi k3 should accept effort {effort}"
            );
        }
    }

    #[test]
    fn guarded_local_validator_scrutiny_cannot_be_configured_local() {
        // KRZ-206b: scrutiny is judgment; the local validator tier is the
        // functional role only. The rejection names the role, and fires
        // whether or not the endpoint fields are present (the role guard is
        // the real problem, never the missing baseUrl).
        let mut cfg = MissionConfig::default();
        cfg.validator_scrutiny.backend = Some("local".into());
        cfg.validator_scrutiny.base_url = Some("http://127.0.0.1:8080".into());
        cfg.validator_scrutiny.context_budget = Some(8192);
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(
            err.contains("validatorScrutiny.backend \"local\" is rejected"),
            "the rejection must name the role: {err}"
        );

        let mut cfg = MissionConfig::default();
        cfg.validator_scrutiny.backend = Some("local".into());
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(
            err.contains("validatorScrutiny.backend \"local\" is rejected"),
            "the role guard must fire before the endpoint checks: {err}"
        );
    }

    #[test]
    fn guarded_local_validator_functional_may_be_configured_local() {
        // KRZ-206b: the functional role may select the local backend for
        // deterministic mechanical checks (contract-command pass/fail); the
        // same endpoint requirements as any local-backed role apply, and
        // every local PASS is frontier-confirmed at the validation round.
        let mut cfg = MissionConfig::default();
        cfg.validator_functional.backend = Some("local".into());
        cfg.validator_functional.base_url = Some("http://127.0.0.1:8080".into());
        cfg.validator_functional.context_budget = Some(8192);
        assert!(
            validate(&cfg).is_ok(),
            "functional + local with a valid endpoint must be accepted"
        );

        // The endpoint fields stay required — a local functional validator
        // with nowhere to point is a config error, exactly as before.
        let mut cfg = MissionConfig::default();
        cfg.validator_functional.backend = Some("local".into());
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(
            err.contains("validatorFunctional.baseUrl is required"),
            "endpoint requirements must still apply to the functional role: {err}"
        );
    }

    #[test]
    fn validate_rejects_kimi_k3_for_unsupported_efforts() {
        for effort in ["medium", "xhigh"] {
            let mut cfg = MissionConfig::default();
            cfg.validator_scrutiny.backend = Some("kimi".into());
            cfg.validator_scrutiny.model = DEFAULT_KIMI_MODEL.into();
            cfg.validator_scrutiny.reasoning_effort = effort.into();
            let err = validate(&cfg).unwrap_err().to_string();
            assert!(
                err.contains("reasoningEffort"),
                "kimi k3 should reject effort {effort}: {err}"
            );
        }
    }

    #[test]
    fn validate_kimi_for_coding_imposes_no_effort_constraint() {
        for effort in ["low", "medium", "high", "xhigh", "max"] {
            let mut cfg = MissionConfig::default();
            cfg.validator_scrutiny.backend = Some("kimi".into());
            cfg.validator_scrutiny.model = "kimi-code/kimi-for-coding".into();
            cfg.validator_scrutiny.reasoning_effort = effort.into();
            assert!(
                validate(&cfg).is_ok(),
                "kimi-for-coding should accept any effort, got {effort} err"
            );
        }
    }

    #[test]
    fn validate_rejects_unsupported_kimi_model() {
        let mut cfg = MissionConfig::default();
        cfg.validator_scrutiny.backend = Some("kimi".into());
        cfg.validator_scrutiny.model = "kimi-unknown-model".into();
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn sandbox_config_defaults_to_off() {
        let cfg = MissionConfig::default();
        for role in [
            &cfg.orchestrator,
            &cfg.worker,
            &cfg.validator_scrutiny,
            &cfg.validator_functional,
        ] {
            assert_eq!(role.sandbox.enforce, crate::types::SandboxEnforce::Off);
            assert!(role.sandbox.extra_write.is_empty());
            assert!(role.sandbox.egress.is_empty());
        }
        assert!(validate(&cfg).is_ok());
    }

    #[test]
    fn sandbox_config_parses_fs() {
        let dir = tempfile::tempdir().unwrap();
        let layer_path = dir.path().join("config.json");
        std::fs::write(
            &layer_path,
            r#"{"worker":{"sandbox":{"enforce":"fs","extraWrite":["~/.cargo"]}}}"#,
        )
        .unwrap();

        let cfg = load_layers(&[layer_path]).unwrap();
        assert_eq!(cfg.worker.sandbox.enforce, crate::types::SandboxEnforce::Fs);
        assert_eq!(cfg.worker.sandbox.extra_write, vec!["~/.cargo".to_string()]);
        // Other roles remain untouched by the partial patch.
        assert_eq!(
            cfg.orchestrator.sandbox.enforce,
            crate::types::SandboxEnforce::Off
        );
    }

    #[test]
    fn sandbox_config_extra_write_roundtrips() {
        let mut cfg = MissionConfig::default();
        cfg.worker.sandbox.enforce = crate::types::SandboxEnforce::FsNet;
        cfg.worker.sandbox.extra_write = vec!["~/.cargo".into(), "~/.npm".into()];
        cfg.worker.sandbox.egress = vec!["registry.npmjs.org:443".into()];

        let value = serde_json::to_value(&cfg).unwrap();
        assert_eq!(value["worker"]["sandbox"]["enforce"], "fs+net");
        assert_eq!(
            value["worker"]["sandbox"]["extraWrite"],
            serde_json::json!(["~/.cargo", "~/.npm"])
        );
        assert_eq!(
            value["worker"]["sandbox"]["egress"],
            serde_json::json!(["registry.npmjs.org:443"])
        );

        let roundtripped: MissionConfig = serde_json::from_value(value).unwrap();
        assert_eq!(roundtripped, cfg);
    }

    #[test]
    fn sandbox_config_parses_fs_plus_net() {
        let dir = tempfile::tempdir().unwrap();
        let layer_path = dir.path().join("config.json");
        std::fs::write(
            &layer_path,
            r#"{"worker":{"sandbox":{"enforce":"fs+net","egress":["crates.io:443"]}}}"#,
        )
        .unwrap();

        let cfg = load_layers(&[layer_path]).unwrap();
        assert_eq!(
            cfg.worker.sandbox.enforce,
            crate::types::SandboxEnforce::FsNet
        );
        assert_eq!(cfg.worker.sandbox.egress, vec!["crates.io:443"]);
    }

    #[test]
    fn only_claude_declares_sandbox_enforcement_support() {
        assert!(BackendKind::Claude.supports_sandbox_enforcement());
        for kind in [
            BackendKind::Codex,
            BackendKind::Droid,
            BackendKind::Kimi,
            BackendKind::Local,
            BackendKind::Cursor,
        ] {
            assert!(
                !kind.supports_sandbox_enforcement(),
                "{kind:?} must not claim sandbox enforcement support"
            );
        }
    }

    #[test]
    fn validate_rejects_enforced_sandbox_on_non_claude_backends() {
        for backend in ["codex", "droid", "kimi", "cursor"] {
            for enforce in [
                crate::types::SandboxEnforce::Fs,
                crate::types::SandboxEnforce::FsNet,
            ] {
                let mut cfg = MissionConfig::default();
                cfg.validator_scrutiny.backend = Some(backend.into());
                cfg.validator_scrutiny.sandbox.enforce = enforce;
                let err = validate(&cfg).unwrap_err().to_string();
                // The error must name the backend, the requested enforce
                // mode, and the remedy.
                assert!(err.contains("validatorScrutiny"), "{err}");
                assert!(err.contains(backend), "{err}");
                assert!(err.contains(enforce.as_str()), "{err}");
                assert!(err.contains("sandbox.enforce=off"), "{err}");
                assert!(err.contains("claude"), "{err}");
            }
        }
    }

    #[test]
    fn validate_rejects_enforced_sandbox_on_codex_worker() {
        let mut cfg = MissionConfig::default();
        cfg.worker.backend = Some("codex".into());
        cfg.worker.sandbox.enforce = crate::types::SandboxEnforce::Fs;
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("worker.backend"), "{err}");
        assert!(err.contains("codex"), "{err}");
        assert!(err.contains("sandbox.enforce=off"), "{err}");
    }

    #[test]
    fn validate_rejects_enforced_sandbox_on_local_backend() {
        // The local backend makes its HTTP call in the engine process — no
        // child to wrap — so an enforced sandbox would be silently ignored.
        let mut cfg = local_worker_cfg();
        cfg.worker.sandbox.enforce = crate::types::SandboxEnforce::FsNet;
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("local"), "{err}");
        assert!(err.contains("fs+net"), "{err}");
    }

    #[test]
    fn validate_rejects_container_provider_on_non_claude_backend() {
        // `provider = "container"` with an enforced mode is still an enforced
        // sandbox the backend cannot honor.
        let mut cfg = MissionConfig::default();
        cfg.validator_scrutiny.backend = Some("droid".into());
        cfg.validator_scrutiny.sandbox.enforce = crate::types::SandboxEnforce::Fs;
        cfg.validator_scrutiny.sandbox.provider = crate::types::SandboxProvider::Container;
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn container_net_boundary_is_hard_only_for_process_or_empty_egress() {
        // The process provider's fs+net boundary is the OS profile itself
        // (Seatbelt loopback-only on macOS, bwrap --unshare-net on Linux), so
        // the proxy hop is the only reachable way out regardless of the list.
        assert!(crate::types::SandboxProvider::Process
            .enforces_hard_net_boundary(&["crates.io:443".to_string()]));
        // The container provider's only hard net boundary today is
        // `--network none` (empty egress); a non-empty list is proxy-env
        // advisory on the runtime bridge.
        assert!(crate::types::SandboxProvider::Container.enforces_hard_net_boundary(&[]));
        assert!(!crate::types::SandboxProvider::Container
            .enforces_hard_net_boundary(&["crates.io:443".to_string()]));
    }

    #[test]
    fn validate_rejects_container_fs_net_with_egress_list() {
        // Container fs+net with a non-empty egress list is proxy-env advisory
        // on the default bridge — fail closed until the sidecar boundary.
        let mut cfg = MissionConfig::default();
        cfg.worker.sandbox.enforce = crate::types::SandboxEnforce::FsNet;
        cfg.worker.sandbox.provider = crate::types::SandboxProvider::Container;
        cfg.worker.sandbox.egress = vec!["crates.io:443".into()];
        let err = validate(&cfg).unwrap_err().to_string();
        // The error must name the role, the provider, the enforce mode, the
        // remedies, and point at the boundary work.
        assert!(err.contains("worker"), "{err}");
        assert!(err.contains("container"), "{err}");
        assert!(err.contains("fs+net"), "{err}");
        assert!(err.contains("--network none"), "{err}");
        assert!(err.contains("\"process\""), "{err}");
        assert!(err.contains("sandbox.enforce=off"), "{err}");
        assert!(err.contains("sidecar"), "{err}");
    }

    #[test]
    fn validate_accepts_container_fs_and_container_fs_net_with_empty_egress() {
        // `fs` claims no network enforcement at all, and fs+net with an empty
        // egress list maps to the hard `--network none` boundary — both
        // honest postures for the container provider.
        for enforce in [
            crate::types::SandboxEnforce::Fs,
            crate::types::SandboxEnforce::FsNet,
        ] {
            let mut cfg = MissionConfig::default();
            cfg.worker.sandbox.enforce = enforce;
            cfg.worker.sandbox.provider = crate::types::SandboxProvider::Container;
            assert!(
                validate(&cfg).is_ok(),
                "container provider with sandbox.enforce={} and an empty egress list must validate",
                enforce.as_str()
            );
        }
    }

    #[test]
    fn validate_accepts_process_fs_net_with_egress_list() {
        // The process provider keeps its kernel boundary (Seatbelt loopback /
        // bwrap --unshare-net) regardless of the egress list — unchanged.
        let mut cfg = MissionConfig::default();
        cfg.worker.sandbox.enforce = crate::types::SandboxEnforce::FsNet;
        cfg.worker.sandbox.egress = vec!["crates.io:443".into()];
        assert!(
            validate(&cfg).is_ok(),
            "process provider fs+net with an egress list must still validate"
        );
    }

    #[test]
    fn validate_accepts_enforced_sandbox_on_claude_backend() {
        for backend in [None, Some("claude")] {
            for enforce in [
                crate::types::SandboxEnforce::Fs,
                crate::types::SandboxEnforce::FsNet,
            ] {
                let mut cfg = MissionConfig::default();
                cfg.worker.backend = backend.map(|s| s.to_string());
                cfg.worker.sandbox.enforce = enforce;
                assert!(
                    validate(&cfg).is_ok(),
                    "claude worker with sandbox.enforce={} must validate",
                    enforce.as_str()
                );
            }
        }
    }

    #[test]
    fn validate_accepts_sandbox_off_on_every_backend() {
        for backend in ["codex", "droid", "kimi", "cursor"] {
            let mut cfg = MissionConfig::default();
            cfg.validator_scrutiny.backend = Some(backend.into());
            assert_eq!(
                cfg.validator_scrutiny.sandbox.enforce,
                crate::types::SandboxEnforce::Off
            );
            assert!(
                validate(&cfg).is_ok(),
                "{backend} with sandbox.enforce=off must validate"
            );
        }
        assert!(
            validate(&local_worker_cfg()).is_ok(),
            "local with sandbox.enforce=off must validate"
        );
    }

    fn local_role_cfg() -> crate::types::RoleConfig {
        crate::types::RoleConfig {
            backend: Some("local".into()),
            model: "my-local-model".into(),
            base_url: Some("http://localhost:8080".into()),
            context_budget: Some(8192),
            ..MissionConfig::default().worker
        }
    }

    fn local_worker_cfg() -> MissionConfig {
        MissionConfig {
            worker: local_role_cfg(),
            allow_below_default_worker_model: true,
            ..MissionConfig::default()
        }
    }

    /// ACP (KRZ-301): the worker-role-only backend wiring — parse, role
    /// restriction, required command, model-tier opt-in.
    fn acp_worker_cfg() -> MissionConfig {
        let mut cfg = MissionConfig {
            allow_below_default_worker_model: true,
            ..MissionConfig::default()
        };
        cfg.worker.backend = Some("acp".into());
        cfg.worker.acp_command = Some("/opt/bin/my-acp-agent".into());
        cfg.worker.acp_args = vec!["--serve".into()];
        cfg
    }

    #[test]
    fn backend_acp_config_round_trips_and_parses() {
        assert_eq!(parse_backend(Some("acp")), Ok(BackendKind::Acp));
        let cfg = acp_worker_cfg();
        assert_eq!(cfg.backend_kind(Role::Worker), BackendKind::Acp);
        assert_eq!(BackendKind::Acp.as_str(), "acp");
        assert!(!BackendKind::Acp.supports_sandbox_enforcement());
        assert!(!BackendKind::Acp.reports_cache_read_tokens());
        assert!(!BackendKind::Acp.reports_cache_write_tokens());
        assert!(
            validate(&cfg).is_ok(),
            "worker + acpCommand + the below-default opt-in must validate"
        );
    }

    #[test]
    fn backend_acp_config_requires_worker_role_and_command() {
        // Validators and the orchestrator are refused (KRZ-301 lands
        // worker-first).
        for role in [
            Role::Orchestrator,
            Role::ValidatorScrutiny,
            Role::ValidatorFunctional,
        ] {
            let mut cfg = acp_worker_cfg();
            let role_cfg = match role {
                Role::Orchestrator => &mut cfg.orchestrator,
                Role::ValidatorScrutiny => &mut cfg.validator_scrutiny,
                Role::ValidatorFunctional => &mut cfg.validator_functional,
                Role::Worker => unreachable!("loop excludes the worker"),
            };
            role_cfg.backend = Some("acp".into());
            role_cfg.acp_command = Some("/opt/bin/my-acp-agent".into());
            let err = validate(&cfg).expect_err("non-worker acp must be refused");
            assert!(
                err.to_string().contains("worker role only"),
                "refusal must name the role restriction: {err}"
            );
        }

        // The command is required (and a blank one is as good as absent).
        let mut cfg = acp_worker_cfg();
        cfg.worker.acp_command = None;
        let err = validate(&cfg).expect_err("missing acpCommand must be refused");
        assert!(err.to_string().contains("acpCommand"), "{err}");
        cfg.worker.acp_command = Some("   ".into());
        assert!(validate(&cfg).is_err(), "blank acpCommand must be refused");

        // ACP model ids are free-form → uniformly below-default → the
        // worker needs the explicit opt-in, same as local.
        let mut cfg = acp_worker_cfg();
        cfg.allow_below_default_worker_model = false;
        let err = validate(&cfg).expect_err("below-default acp worker needs the opt-in");
        assert!(
            err.to_string().contains("allowBelowDefaultWorkerModel"),
            "{err}"
        );
    }

    #[test]
    fn local_config_requires_base_url() {
        let mut cfg = local_worker_cfg();

        cfg.worker.base_url = None;
        assert!(
            validate(&cfg).is_err(),
            "missing baseUrl should be rejected"
        );

        cfg.worker.base_url = Some("not a url".into());
        assert!(
            validate(&cfg).is_err(),
            "unparseable baseUrl should be rejected"
        );

        cfg.worker.base_url = Some("http://localhost:8080".into());
        assert!(
            validate(&cfg).is_ok(),
            "valid http baseUrl should be accepted"
        );

        cfg.worker.base_url = Some("https://models.internal/v1".into());
        assert!(
            validate(&cfg).is_ok(),
            "valid https baseUrl should be accepted"
        );

        cfg.worker.base_url = Some("http://127.0.0.1".into());
        assert!(
            validate(&cfg).is_ok(),
            "bare ip host baseUrl should be accepted"
        );

        cfg.worker.base_url = Some("http://:8080".into());
        assert!(
            validate(&cfg).is_err(),
            "host-less authority with port should be rejected"
        );

        cfg.worker.base_url = Some("http://@".into());
        assert!(
            validate(&cfg).is_err(),
            "userinfo-only authority should be rejected"
        );

        cfg.worker.base_url = Some("http://@:8080".into());
        assert!(
            validate(&cfg).is_err(),
            "userinfo with port and no host should be rejected"
        );
    }

    #[test]
    fn local_config_requires_context_budget_in_range() {
        let mut cfg = local_worker_cfg();

        cfg.worker.context_budget = Some(1023);
        assert!(validate(&cfg).is_err(), "1023 is below the floor");

        cfg.worker.context_budget = Some(200_001);
        assert!(validate(&cfg).is_err(), "200001 is above the ceiling");

        cfg.worker.context_budget = None;
        assert!(validate(&cfg).is_err(), "missing contextBudget is rejected");

        cfg.worker.context_budget = Some(8192);
        assert!(validate(&cfg).is_ok(), "8192 is in range");
    }

    #[test]
    fn local_config_rejects_out_of_range_temperature() {
        let mut cfg = local_worker_cfg();

        cfg.worker.temperature = Some(2.1);
        assert!(validate(&cfg).is_err(), "2.1 is above the ceiling");

        cfg.worker.temperature = Some(-0.1);
        assert!(validate(&cfg).is_err(), "-0.1 is below the floor");

        cfg.worker.temperature = Some(0.7);
        assert!(validate(&cfg).is_ok(), "0.7 is in range");

        cfg.worker.temperature = None;
        assert!(validate(&cfg).is_ok(), "absent temperature is fine");
    }

    #[test]
    fn local_config_worker_below_default_needs_optin() {
        let mut cfg = local_worker_cfg();
        cfg.allow_below_default_worker_model = false;
        assert!(
            validate(&cfg).is_err(),
            "local worker below-default tier requires opt-in"
        );

        cfg.allow_below_default_worker_model = true;
        assert!(
            validate(&cfg).is_ok(),
            "local worker accepted once opted in"
        );
    }

    #[test]
    fn local_config_orchestrator_local_always_rejected() {
        let cfg = MissionConfig {
            orchestrator: local_role_cfg(),
            allow_below_default_worker_model: true,
            ..MissionConfig::default()
        };
        assert!(
            validate(&cfg).is_err(),
            "local orchestrator always fails the frontier floor"
        );
    }

    #[test]
    fn local_config_model_tier_below_default_for_any_nonempty() {
        assert_eq!(
            model_tier(BackendKind::Local, "any-model-id"),
            Some(ModelTier::BelowDefault)
        );
        assert_eq!(model_tier(BackendKind::Local, ""), None);
        assert_eq!(model_tier(BackendKind::Local, "   "), None);
    }

    #[test]
    fn local_config_effective_model_passes_through_verbatim_and_never_panics() {
        assert_eq!(
            effective_model(Role::Worker, BackendKind::Local, "my-local-model"),
            "my-local-model"
        );
        // Even if the configured string happens to equal the Claude role
        // default, Local has no backend default to rewrite to.
        assert_eq!(
            effective_model(Role::Worker, BackendKind::Local, "sonnet"),
            "sonnet"
        );
    }

    #[test]
    fn task_class_routing_maps_execution_class_to_local() {
        assert_eq!(
            task_class_to_tier(Some("execution-class")),
            ExecutorTier::Local
        );
    }

    #[test]
    fn task_class_routing_defaults_to_frontier() {
        assert_eq!(
            task_class_to_tier(Some("planning-class")),
            ExecutorTier::Frontier
        );
        assert_eq!(
            task_class_to_tier(Some("some-arbitrary-value")),
            ExecutorTier::Frontier
        );
        assert_eq!(task_class_to_tier(None), ExecutorTier::Frontier);
    }

    #[test]
    fn task_class_routing_is_case_and_whitespace_insensitive() {
        assert_eq!(
            task_class_to_tier(Some("  Execution-Class ")),
            ExecutorTier::Local
        );
    }

    fn test_local_endpoint() -> LocalEndpoint {
        LocalEndpoint {
            base_url: "http://127.0.0.1:8080".to_string(),
            context_budget: 16_384,
            temperature: Some(0.2),
        }
    }

    #[test]
    fn executor_routing_applies_local_backend_when_execution_class_and_endpoint_configured() {
        let mut cfg = MissionConfig::default();
        let validator_scrutiny_before = cfg.validator_scrutiny.clone();
        let validator_functional_before = cfg.validator_functional.clone();
        let endpoint = test_local_endpoint();

        let applied = apply_executor_routing(&mut cfg, ExecutorTier::Local, Some(&endpoint));

        assert_eq!(applied, ExecutorTier::Local);
        assert_eq!(cfg.worker.backend.as_deref(), Some("local"));
        assert_eq!(
            cfg.worker.base_url.as_deref(),
            Some(endpoint.base_url.as_str())
        );
        assert_eq!(cfg.worker.context_budget, Some(endpoint.context_budget));
        assert_eq!(cfg.worker.temperature, endpoint.temperature);
        assert!(cfg.allow_below_default_worker_model);
        assert_eq!(cfg.validator_scrutiny, validator_scrutiny_before);
        assert_eq!(cfg.validator_functional, validator_functional_before);
    }

    #[test]
    fn executor_routing_applies_fail_safe_frontier_when_no_endpoint_configured() {
        let mut cfg = MissionConfig::default();
        let worker_backend_before = cfg.worker.backend.clone();

        let applied = apply_executor_routing(&mut cfg, ExecutorTier::Local, None);

        assert_eq!(applied, ExecutorTier::Frontier);
        assert_eq!(cfg.worker.backend, worker_backend_before);
        assert!(!cfg.allow_below_default_worker_model);
    }

    #[test]
    fn executor_routing_applies_no_change_for_frontier_tier() {
        let mut cfg = MissionConfig::default();
        let before = cfg.clone();
        let endpoint = test_local_endpoint();

        let applied = apply_executor_routing(&mut cfg, ExecutorTier::Frontier, Some(&endpoint));

        assert_eq!(applied, ExecutorTier::Frontier);
        assert_eq!(cfg, before);
    }

    #[test]
    fn route_task_class_executor_routes_local_when_endpoint_configured() {
        // Mirrors what `MissionEngine::create` calls with the class recovered
        // from a folded goal string (f-1-2: this is the single engine-side
        // wiring point every seed path — draft, exec, REST, Slack — shares).
        let mut cfg = MissionConfig::default();
        cfg.worker.base_url = Some("http://127.0.0.1:8080".to_string());
        cfg.worker.context_budget = Some(16_384);

        let (applied, summary) = route_task_class_executor(&mut cfg, Some("execution-class"));

        assert_eq!(applied, ExecutorTier::Local);
        assert_eq!(cfg.worker.backend.as_deref(), Some("local"));
        assert_eq!(summary, "executor routed local (execution-class)");
    }

    #[test]
    fn route_task_class_executor_stays_frontier_without_endpoint() {
        let mut cfg = MissionConfig::default();
        let (applied, summary) = route_task_class_executor(&mut cfg, Some("execution-class"));

        assert_eq!(applied, ExecutorTier::Frontier);
        assert_eq!(cfg.worker.backend, None);
        assert!(summary.contains("no local endpoint configured"));
    }

    #[test]
    fn route_task_class_executor_stays_frontier_for_non_execution_class() {
        let mut cfg = MissionConfig::default();
        cfg.worker.base_url = Some("http://127.0.0.1:8080".to_string());
        cfg.worker.context_budget = Some(16_384);

        let (applied, summary) = route_task_class_executor(&mut cfg, None);

        assert_eq!(applied, ExecutorTier::Frontier);
        assert_eq!(cfg.worker.backend, None);
        assert_eq!(summary, "executor stays frontier");
    }

    #[test]
    fn validator_stays_frontier_after_local_executor_routing() {
        let mut cfg = MissionConfig::default();
        let endpoint = test_local_endpoint();

        apply_executor_routing(&mut cfg, ExecutorTier::Local, Some(&endpoint));

        assert_ne!(cfg.validator_scrutiny.backend.as_deref(), Some("local"));
        assert_ne!(cfg.validator_functional.backend.as_deref(), Some("local"));
    }

    // -----------------------------------------------------------------------
    // Backend routing table (ticket backend-routing-abstraction, KRZ-331)
    // -----------------------------------------------------------------------

    use crate::types::TaskClassRoute;

    fn routing_table(rules: &[(&str, ExecutorTier)]) -> Vec<TaskClassRoute> {
        rules
            .iter()
            .map(|(task_class, tier)| TaskClassRoute {
                task_class: task_class.to_string(),
                tier: *tier,
            })
            .collect()
    }

    #[test]
    fn routing_abstraction_table_defaults_empty_and_parses_camel_case() {
        // Additive contract change: an absent key (every pre-existing config
        // and every old mission.created event payload) deserializes to the
        // EMPTY table — the byte-identical literal floor.
        assert!(MissionConfig::default().routing.task_class_rules.is_empty());
        let value = serde_json::to_value(MissionConfig::default()).unwrap();
        assert_eq!(value["routing"]["taskClassRules"], serde_json::json!([]));

        let dir = tempfile::tempdir().unwrap();
        let layer_path = dir.path().join("config.json");
        std::fs::write(
            &layer_path,
            r#"{"routing": {"taskClassRules": [{"taskClass": "execution-class", "tier": "local"}, {"taskClass": "docs-class", "tier": "frontier"}]}}"#,
        )
        .unwrap();
        let cfg = load_layers(&[layer_path]).unwrap();
        assert_eq!(cfg.routing.task_class_rules.len(), 2);
        assert_eq!(
            cfg.routing.task_class_rules[0].task_class,
            "execution-class"
        );
        assert_eq!(cfg.routing.task_class_rules[0].tier, ExecutorTier::Local);
        assert_eq!(cfg.routing.task_class_rules[1].tier, ExecutorTier::Frontier);

        // A layer naming unrelated keys only (the old-config shape) leaves
        // the table empty.
        let layer_path = dir.path().join("config-old.json");
        std::fs::write(&layer_path, r#"{"maxRespawns": 3}"#).unwrap();
        let cfg = load_layers(&[layer_path]).unwrap();
        assert!(cfg.routing.task_class_rules.is_empty());
    }

    #[test]
    fn routing_abstraction_unconfigured_table_keeps_byte_identical_floor() {
        // The regression pin: with NO table configured, routing a task class
        // must produce exactly the pre-table behavior — the literal floor
        // (`task_class_to_tier`) fed through `apply_executor_routing` —
        // including the applied config edits, for every input shape.
        for task_class in [
            None,
            Some("execution-class"),
            Some("  Execution-Class "),
            Some("planning-class"),
            Some("some-arbitrary-value"),
        ] {
            for endpoint_configured in [false, true] {
                let wire = |cfg: &mut MissionConfig| {
                    if endpoint_configured {
                        cfg.worker.base_url = Some("http://127.0.0.1:8080".to_string());
                        cfg.worker.context_budget = Some(16_384);
                    }
                };
                let mut cfg = MissionConfig::default();
                wire(&mut cfg);
                assert!(cfg.routing.task_class_rules.is_empty());
                let (applied, _) = route_task_class_executor(&mut cfg, task_class);

                // The pre-table reference computation.
                let mut reference = MissionConfig::default();
                wire(&mut reference);
                let endpoint = match (&reference.worker.base_url, reference.worker.context_budget) {
                    (Some(base_url), Some(context_budget)) => Some(LocalEndpoint {
                        base_url: base_url.clone(),
                        context_budget,
                        temperature: reference.worker.temperature,
                    }),
                    _ => None,
                };
                let expected = apply_executor_routing(
                    &mut reference,
                    task_class_to_tier(task_class),
                    endpoint.as_ref(),
                );

                assert_eq!(applied, expected, "task class {task_class:?}");
                assert_eq!(
                    cfg, reference,
                    "an empty table must apply byte-identical config changes for {task_class:?}"
                );
            }
        }
    }

    #[test]
    fn routing_abstraction_table_routes_configured_class_to_local() {
        // A configured table is the complete floor: it routes the classes it
        // names — beyond the literal floor's single hardcoded class...
        let mut cfg = MissionConfig::default();
        cfg.routing.task_class_rules = routing_table(&[("docs-class", ExecutorTier::Local)]);
        cfg.worker.base_url = Some("http://127.0.0.1:8080".to_string());
        cfg.worker.context_budget = Some(16_384);

        let (applied, summary) = route_task_class_executor(&mut cfg, Some("docs-class"));

        assert_eq!(applied, ExecutorTier::Local);
        assert_eq!(cfg.worker.backend.as_deref(), Some("local"));
        assert_eq!(summary, "executor routed local (routing-table rule)");

        // ...and the literal floor's own class stays frontier when the table
        // does not name it (the table replaces the literal map, it does not
        // amend it).
        let mut cfg = MissionConfig::default();
        cfg.routing.task_class_rules = routing_table(&[("docs-class", ExecutorTier::Local)]);
        cfg.worker.base_url = Some("http://127.0.0.1:8080".to_string());
        cfg.worker.context_budget = Some(16_384);

        let (applied, summary) = route_task_class_executor(&mut cfg, Some("execution-class"));

        assert_eq!(applied, ExecutorTier::Frontier);
        assert_eq!(cfg.worker.backend, None);
        assert_eq!(summary, "executor stays frontier");
    }

    #[test]
    fn routing_abstraction_table_local_route_fails_safe_without_endpoint() {
        // The pre-table fail-safe is unchanged under a table: a local route
        // with no configured endpoint stays frontier rather than routing to
        // an endpoint that doesn't exist.
        let mut cfg = MissionConfig::default();
        cfg.routing.task_class_rules = routing_table(&[("execution-class", ExecutorTier::Local)]);

        let (applied, summary) = route_task_class_executor(&mut cfg, Some("execution-class"));

        assert_eq!(applied, ExecutorTier::Frontier);
        assert_eq!(cfg.worker.backend, None);
        assert!(
            summary.contains("no local endpoint configured"),
            "{summary}"
        );
    }

    #[test]
    fn routing_abstraction_validate_fails_closed_on_malformed_table() {
        // Duplicate after normalization: refused, naming the rule (a
        // shadowed rule is dead config under first-match-wins).
        let mut cfg = MissionConfig::default();
        cfg.routing.task_class_rules = routing_table(&[
            ("execution-class", ExecutorTier::Local),
            (" Execution-Class", ExecutorTier::Frontier),
        ]);
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("routing.taskClassRules[1].taskClass"), "{err}");
        assert!(err.contains("duplicates rule 0"), "{err}");

        // Blank class: refused (it could never match honestly).
        let mut cfg = MissionConfig::default();
        cfg.routing.task_class_rules = routing_table(&[("   ", ExecutorTier::Local)]);
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("routing.taskClassRules[0].taskClass"), "{err}");

        // A clean table validates.
        let mut cfg = MissionConfig::default();
        cfg.routing.task_class_rules = routing_table(&[
            ("execution-class", ExecutorTier::Local),
            ("docs-class", ExecutorTier::Frontier),
        ]);
        assert!(validate(&cfg).is_ok(), "a clean table must validate");
    }

    #[test]
    fn routing_abstraction_hosted_fine_tune_is_plain_local_endpoint_config() {
        // KRZ-331: a hosted fine-tune is configuration of the
        // OpenAI-compatible local backend (baseUrl + model), NOT a new
        // backend kind — an https endpoint carrying a free-form
        // fine-tune-shaped model id validates exactly like a localhost one,
        // and a table can route a task class to it by capability class.
        let mut cfg = local_worker_cfg();
        cfg.worker.base_url = Some("https://models.internal.example/v1".into());
        cfg.worker.model = "ft:some-model:some-org:some-id".into();
        assert!(
            validate(&cfg).is_ok(),
            "a hosted fine-tune endpoint is ordinary local-backend config"
        );

        cfg.routing.task_class_rules = routing_table(&[("execution-class", ExecutorTier::Local)]);
        let (applied, _) = route_task_class_executor(&mut cfg, Some("execution-class"));
        assert_eq!(applied, ExecutorTier::Local);
        assert_eq!(cfg.worker.backend.as_deref(), Some("local"));
    }

    // -----------------------------------------------------------------------
    // Heterogeneous dispatch pool (ticket heterogeneous-dispatch-pool, KRZ-303)
    // -----------------------------------------------------------------------

    use crate::types::CandidateSpec;

    fn dispatch_pool_pair() -> Vec<CandidateSpec> {
        vec![
            CandidateSpec {
                backend: "claude".into(),
                model: "sonnet".into(),
            },
            CandidateSpec {
                backend: "codex".into(),
                model: DEFAULT_CODEX_MODEL.into(),
            },
        ]
    }

    #[test]
    fn dispatch_pool_defaults_empty_and_parses_camel_case() {
        // Additive contract change: absent key (every pre-existing config and
        // every old mission.created event payload) deserializes to empty —
        // today's single-backend behavior exactly.
        assert!(MissionConfig::default().worker_candidates.is_empty());
        let value = serde_json::to_value(MissionConfig::default()).unwrap();
        assert_eq!(value["workerCandidates"], serde_json::json!([]));

        let dir = tempfile::tempdir().unwrap();
        let layer_path = dir.path().join("config.json");
        std::fs::write(
            &layer_path,
            r#"{"workerCandidates": [{"backend": "claude", "model": "sonnet"}, {"backend": "codex", "model": "gpt-5-codex"}]}"#,
        )
        .unwrap();
        let cfg = load_layers(&[layer_path]).unwrap();
        assert_eq!(cfg.worker_candidates, dispatch_pool_pair());

        // A layer naming unrelated keys only (the old-config shape) leaves
        // the pool empty.
        let layer_path = dir.path().join("config-old.json");
        std::fs::write(&layer_path, r#"{"maxRespawns": 3}"#).unwrap();
        let cfg = load_layers(&[layer_path]).unwrap();
        assert!(cfg.worker_candidates.is_empty());
    }

    #[test]
    fn dispatch_pool_validate_accepts_heterogeneous_pair() {
        let cfg = MissionConfig {
            worker_candidates: dispatch_pool_pair(),
            ..MissionConfig::default()
        };
        validate(&cfg).unwrap();
    }

    #[test]
    fn dispatch_pool_validate_rejects_single_entry() {
        let cfg = MissionConfig {
            worker_candidates: vec![CandidateSpec {
                backend: "claude".into(),
                model: "sonnet".into(),
            }],
            ..MissionConfig::default()
        };
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(
            err.contains("workerCandidates with exactly one entry"),
            "{err}"
        );
    }

    #[test]
    fn dispatch_pool_validate_rejects_local_and_acp_candidates() {
        for backend in ["local", "acp"] {
            let cfg = MissionConfig {
                worker_candidates: vec![
                    CandidateSpec {
                        backend: "claude".into(),
                        model: "sonnet".into(),
                    },
                    CandidateSpec {
                        backend: backend.into(),
                        model: "anything".into(),
                    },
                ],
                ..MissionConfig::default()
            };
            let err = validate(&cfg).unwrap_err().to_string();
            assert!(
                err.contains("not supported in this pass"),
                "{backend}: {err}"
            );
        }
    }

    #[test]
    fn dispatch_pool_validate_rejects_parallel_workers_combination() {
        let cfg = MissionConfig {
            worker_candidates: dispatch_pool_pair(),
            max_parallel_workers: 2,
            ..MissionConfig::default()
        };
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn dispatch_pool_validate_rejects_unknown_backend_and_model() {
        let cfg = MissionConfig {
            worker_candidates: vec![
                CandidateSpec {
                    backend: "claude".into(),
                    model: "sonnet".into(),
                },
                CandidateSpec {
                    backend: "gemini".into(),
                    model: "sonnet".into(),
                },
            ],
            ..MissionConfig::default()
        };
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("workerCandidates[1].backend"), "{err}");

        let cfg = MissionConfig {
            worker_candidates: vec![
                CandidateSpec {
                    backend: "claude".into(),
                    model: "sonnet".into(),
                },
                CandidateSpec {
                    backend: "codex".into(),
                    model: "kranz-test-model".into(),
                },
            ],
            ..MissionConfig::default()
        };
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("not supported by backend"), "{err}");
    }

    #[test]
    fn dispatch_pool_validate_enforces_worker_floor_per_candidate() {
        // droid's default GLM is below the default worker tier — rejected
        // without the opt-in, accepted with it, exactly like the role check.
        let mut cfg = MissionConfig {
            worker_candidates: vec![
                CandidateSpec {
                    backend: "claude".into(),
                    model: "sonnet".into(),
                },
                CandidateSpec {
                    backend: "droid".into(),
                    model: DEFAULT_DROID_MODEL.into(),
                },
            ],
            ..MissionConfig::default()
        };
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("below the default worker tier"), "{err}");
        cfg.allow_below_default_worker_model = true;
        validate(&cfg).unwrap();
    }

    #[test]
    fn dispatch_pool_validate_rejects_sandboxed_non_claude_candidate() {
        let mut cfg = MissionConfig {
            worker_candidates: dispatch_pool_pair(),
            ..MissionConfig::default()
        };
        cfg.worker.sandbox.enforce = crate::types::SandboxEnforce::Fs;
        let err = validate(&cfg).unwrap_err().to_string();
        assert!(err.contains("cannot honor sandbox.enforce"), "{err}");
    }

    #[test]
    fn dispatch_pool_executor_routing_never_goes_local() {
        // An execution-class ticket with a configured pool must NOT get
        // worker.backend rewritten to local: the pool is the explicit
        // per-candidate backend declaration, and the local key would sit
        // next to it dead and misleading.
        let mut cfg = MissionConfig {
            worker_candidates: dispatch_pool_pair(),
            ..MissionConfig::default()
        };
        let endpoint = test_local_endpoint();
        let applied = apply_executor_routing(&mut cfg, ExecutorTier::Local, Some(&endpoint));
        assert_eq!(applied, ExecutorTier::Frontier);
        assert!(cfg.worker.backend.is_none());
    }

    trait RoleConfigTestExt {
        fn role_mut_for_test(&mut self, role: Role) -> &mut crate::types::RoleConfig;
    }

    impl RoleConfigTestExt for MissionConfig {
        fn role_mut_for_test(&mut self, role: Role) -> &mut crate::types::RoleConfig {
            match role {
                Role::Orchestrator => &mut self.orchestrator,
                Role::Worker => &mut self.worker,
                Role::ValidatorScrutiny => &mut self.validator_scrutiny,
                Role::ValidatorFunctional => &mut self.validator_functional,
            }
        }
    }
}
