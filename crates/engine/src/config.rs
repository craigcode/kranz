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

use crate::cost::{DEFAULT_CODEX_MODEL, DEFAULT_DROID_MODEL};
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
        Some(other) => Err(other.to_string()),
    }
}

/// The backend-native model used when an older config selected a non-Claude
/// backend but left the role's Claude default model in place.
fn backend_default_model(kind: BackendKind) -> Option<&'static str> {
    match kind {
        BackendKind::Claude => None,
        BackendKind::Codex => Some(DEFAULT_CODEX_MODEL),
        BackendKind::Droid => Some(DEFAULT_DROID_MODEL),
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
        backend_default_model(kind)
            .expect("non-Claude backends have defaults")
            .to_string()
    } else {
        configured.to_string()
    }
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
                "{name}.backend must be one of None, \"claude\", \"codex\", \"droid\", got {other:?}"
            ))
        })?;
        let effective = effective_model(role, kind, &role_cfg.model);
        let tier = model_tier(kind, &effective).ok_or_else(|| {
            EngineError::Config(format!(
                "{name}.model {:?} is not supported by backend {:?}",
                role_cfg.model,
                kind.as_str()
            ))
        })?;

        if role == Role::Worker
            && tier < ModelTier::Default
            && !cfg.allow_below_default_worker_model
        {
            return Err(EngineError::Config(format!(
                "worker.model {:?} on backend {:?} is below the default worker tier; set \
                 allowBelowDefaultWorkerModel=true on this mission to opt in",
                role_cfg.model,
                kind.as_str()
            )));
        }

        if role == Role::Orchestrator && tier < ModelTier::Frontier {
            return Err(EngineError::Config(format!(
                "orchestrator.model {:?} on backend {:?} is below the frontier-model floor",
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
        cfg.worker.sandbox.enforce = crate::types::SandboxEnforce::Fs;
        cfg.worker.sandbox.extra_write = vec!["~/.cargo".into(), "~/.npm".into()];

        let value = serde_json::to_value(&cfg).unwrap();
        assert_eq!(value["worker"]["sandbox"]["enforce"], "fs");
        assert_eq!(
            value["worker"]["sandbox"]["extraWrite"],
            serde_json::json!(["~/.cargo", "~/.npm"])
        );

        let roundtripped: MissionConfig = serde_json::from_value(value).unwrap();
        assert_eq!(roundtripped, cfg);
    }

    #[test]
    fn sandbox_config_rejects_fs_plus_net() {
        let dir = tempfile::tempdir().unwrap();
        let layer_path = dir.path().join("config.json");
        std::fs::write(
            &layer_path,
            r#"{"worker":{"sandbox":{"enforce":"fs+net"}}}"#,
        )
        .unwrap();

        let err = load_layers(&[layer_path]).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("fs+net") || message.contains("enforce"),
            "error should name the offending value or field, got: {message}"
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
