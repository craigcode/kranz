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

use crate::cost::{DEFAULT_CODEX_MODEL, DEFAULT_DROID_MODEL, DEFAULT_KIMI_MODEL};
use crate::error::{EngineError, Result};
use crate::paths;
use crate::types::{BackendKind, MissionConfig, Role};
use std::path::{Path, PathBuf};

/// Reasoning-effort values accepted by `claude --effort`.
const VALID_EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

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
        Some(other) => Err(other.to_string()),
    }
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

    for (role, name) in [
        (Role::Orchestrator, "orchestrator"),
        (Role::Worker, "worker"),
        (Role::ValidatorScrutiny, "validatorScrutiny"),
        (Role::ValidatorFunctional, "validatorFunctional"),
    ] {
        let role_cfg = cfg.role(role);
        let kind = parse_backend(role_cfg.backend.as_deref()).map_err(|other| {
            EngineError::Config(format!(
                "{name}.backend must be one of None, \"claude\", \"codex\", \"droid\", \"kimi\", \"local\", got {other:?}"
            ))
        })?;
        let effective = effective_model(role, kind, &role_cfg.model);
        let tier = model_tier(kind, &effective).ok_or_else(|| {
            EngineError::Config(format!(
                "{name} effective model {effective:?} (configured as {:?}) is not supported by backend {:?}",
                role_cfg.model,
                kind.as_str()
            ))
        })?;

        if kind == BackendKind::Local {
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
    fn absent_auto_work_key_keeps_default() {
        let dir = tempfile::tempdir().unwrap();
        let layer_path = dir.path().join("config.json");
        std::fs::write(&layer_path, r#"{"maxRespawns": 3}"#).unwrap();

        let cfg = load_layers(&[layer_path]).unwrap();
        assert!(!cfg.auto_work);
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
