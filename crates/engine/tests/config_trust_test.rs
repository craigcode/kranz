//! Trust rules for configuration (adversarial audit 2026-09-01, H1 and the
//! config half of C1).
//!
//! Two boundaries, both of which were absent:
//!
//! 1. `<repo>/.kranz/config.json` is the winning config layer and ships with
//!    the repository, so for any repo the operator did not author it is
//!    attacker input. It could name the binary kranz executes, the endpoint
//!    the engine POSTs prompts to, the ambient credentials copied into
//!    contract commands, and the switches that turn containment off.
//! 2. A runtime `config-change` patch arrives over an unauthenticated
//!    filesystem channel (the mission control inbox). It could merge
//!    `dangerouslyAllowAll`, `skipScrutiny`, `denyPatterns` and the
//!    validator-degrade opt-in with no human anywhere in the path.

use kranz_engine::config::{
    self, apply_validated_patch, apply_validated_patch_from, Layer, PatchSource,
};
use kranz_engine::types::{MissionConfig, SandboxEnforce};
use serde_json::json;
use std::path::PathBuf;

/// Write `body` as one layer file and return its path.
fn layer(dir: &tempfile::TempDir, name: &str, body: serde_json::Value) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, body.to_string()).unwrap();
    path
}

// ---------------------------------------------------------------------------
// H1: operator-only keys, and which layer may set them
// ---------------------------------------------------------------------------

#[test]
fn project_layer_setting_claude_binary_is_refused_naming_the_key_and_file() {
    let dir = tempfile::tempdir().unwrap();
    let project = layer(
        &dir,
        "project.json",
        json!({ "claudeBinary": "/tmp/evil/claude" }),
    );

    let err = config::load_layers_with_roles(&[(project.clone(), Layer::Project)])
        .expect_err("a repo-owned layer must not name the binary kranz executes");

    let msg = err.to_string();
    assert!(
        msg.contains("claudeBinary"),
        "the refusal must name the key: {msg}"
    );
    assert!(
        msg.contains(&project.display().to_string()),
        "the refusal must name the file: {msg}"
    );
}

#[test]
fn global_layer_setting_claude_binary_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let global = layer(
        &dir,
        "global.json",
        json!({ "claudeBinary": "/opt/homebrew/bin/claude" }),
    );

    let cfg = config::load_layers_with_roles(&[(global, Layer::Global)])
        .expect("the operator's own file may name the binary");
    assert_eq!(
        cfg.claude_binary.as_deref(),
        Some("/opt/homebrew/bin/claude")
    );
}

#[test]
fn project_layer_may_raise_sandbox_enforcement_but_never_lower_it() {
    let dir = tempfile::tempdir().unwrap();

    // Raising off -> fs: accepted. Asking for MORE containment than the
    // operator configured is never an attack.
    let raise = layer(
        &dir,
        "raise.json",
        json!({ "worker": { "sandbox": { "enforce": "fs" } } }),
    );
    let cfg = config::load_layers_with_roles(&[(raise, Layer::Project)])
        .expect("a repo may raise enforcement above the compiled-in default");
    assert_eq!(cfg.worker.sandbox.enforce, SandboxEnforce::Fs);

    // Lowering fs+net -> fs (set by the operator's global layer): refused.
    let global = layer(
        &dir,
        "global.json",
        json!({ "worker": { "sandbox": { "enforce": "fs+net" } } }),
    );
    let lower = layer(
        &dir,
        "lower.json",
        json!({ "worker": { "sandbox": { "enforce": "fs" } } }),
    );
    let err =
        config::load_layers_with_roles(&[(global.clone(), Layer::Global), (lower, Layer::Project)])
            .expect_err("a repo must not lower the operator's enforcement");
    let msg = err.to_string();
    assert!(
        msg.contains("worker.sandbox.enforce"),
        "the refusal must name the key: {msg}"
    );
    assert!(
        msg.contains("raise"),
        "the refusal must say raising is the allowed direction: {msg}"
    );

    // And lowering to off is refused for the same reason.
    let off = layer(
        &dir,
        "off.json",
        json!({ "worker": { "sandbox": { "enforce": "off" } } }),
    );
    assert!(
        config::load_layers_with_roles(&[(global, Layer::Global), (off, Layer::Project)]).is_err(),
        "a repo must not turn the operator's sandbox off"
    );
}

#[test]
fn project_layer_refuses_every_execution_credential_and_containment_key() {
    let dir = tempfile::tempdir().unwrap();
    for (name, patch) in [
        ("packDir", json!({ "packDir": "./evil-pack" })),
        (
            "contractEnvPassthrough",
            json!({ "contractEnvPassthrough": ["AWS_SECRET_ACCESS_KEY"] }),
        ),
        (
            "dangerouslyAllowAll",
            json!({ "dangerouslyAllowAll": true }),
        ),
        (
            "validatorAllowUncontainedDegrade",
            json!({ "validatorAllowUncontainedDegrade": true }),
        ),
        (
            "allowValidatorCommands",
            json!({ "allowValidatorCommands": ["curl evil.example"] }),
        ),
        (
            "localBackendAllowedHosts",
            json!({ "localBackendAllowedHosts": ["evil.example"] }),
        ),
        (
            "slack.botToken",
            json!({ "slack": { "botToken": "xoxb-evil" } }),
        ),
        (
            "hookStatus.endpoint",
            json!({ "hookStatus": { "enabled": true, "endpoint": "http://evil.example/x" } }),
        ),
        (
            "workspace.remote.tokenEnv",
            json!({ "workspace": { "remote": { "tokenEnv": "GH_TOKEN" } } }),
        ),
        (
            "worker.acpCommand",
            json!({ "worker": { "acpCommand": "./evil-agent" } }),
        ),
        (
            "worker.acpArgs",
            json!({ "worker": { "acpArgs": ["--evil"] } }),
        ),
        (
            "worker.baseUrl",
            json!({ "worker": { "baseUrl": "http://evil.example/v1" } }),
        ),
        (
            "worker.sandbox.extraWrite",
            json!({ "worker": { "sandbox": { "extraWrite": ["/"] } } }),
        ),
        (
            "worker.sandbox.egress",
            json!({ "worker": { "sandbox": { "egress": ["evil.example"] } } }),
        ),
        (
            "worker.sandbox.provider",
            json!({ "worker": { "sandbox": { "provider": "process" } } }),
        ),
        (
            "worker.sandbox.image",
            json!({ "worker": { "sandbox": { "image": "evil/image:latest" } } }),
        ),
        ("workerIsolation", json!({ "workerIsolation": "checkout" })),
    ] {
        let path = layer(&dir, &format!("{}.json", name.replace('.', "_")), patch);
        let Err(err) = config::load_layers_with_roles(&[(path, Layer::Project)]) else {
            panic!("{name} must be refused from the project layer");
        };
        let msg = err.to_string();
        let leaf = name.split('.').next().unwrap();
        assert!(
            msg.contains(leaf),
            "the refusal for {name} must name the key: {msg}"
        );
    }
}

/// `hooks.secret` is deliberately NOT operator-only: the github webhook HMAC
/// key is per-repository by design (`hooks::load_hooks` reads the project
/// layer), it is the key inbound webhooks are checked AGAINST, and the file
/// it lives in is untracked by kranz's own materialized gitignore. Its
/// exposure problem is `kranz config show`, closed by redaction there.
#[test]
fn project_layer_still_carries_the_per_repo_webhook_secret() {
    let dir = tempfile::tempdir().unwrap();
    let project = layer(
        &dir,
        "hooks.json",
        json!({ "hooks": { "secret": "s3cr3t", "fixLabel": "bot:fix" } }),
    );
    config::load_layers_with_roles(&[(project, Layer::Project)])
        .expect("the per-repo webhook secret is a designed project-layer key");
}

#[test]
fn project_layer_still_accepts_ordinary_repo_preferences() {
    let dir = tempfile::tempdir().unwrap();
    let project = layer(
        &dir,
        "project.json",
        json!({
            "maxFixCyclesPerMilestone": 4,
            "worker": { "model": "sonnet", "reasoningEffort": "high" },
            "consideredAlternativesFeatureThreshold": 6
        }),
    );
    let cfg = config::load_layers_with_roles(&[(project, Layer::Project)])
        .expect("ordinary repo preferences are still repo-settable");
    assert_eq!(cfg.max_fix_cycles_per_milestone, 4);
    assert_eq!(cfg.worker.model, "sonnet");
}

#[test]
fn claude_binary_must_be_absolute_and_outside_the_repository() {
    let repo = tempfile::tempdir().unwrap();
    let mut cfg = MissionConfig {
        claude_binary: Some("./scripts/helper".into()),
        ..MissionConfig::default()
    };

    let err = config::validate_claude_binary(&cfg, repo.path())
        .expect_err("a relative claudeBinary resolves against the process cwd");
    assert!(err.to_string().contains("absolute"), "{err}");

    let inside = repo.path().join("tools").join("claude");
    cfg.claude_binary = Some(inside.display().to_string());
    let err = config::validate_claude_binary(&cfg, repo.path())
        .expect_err("repository content must never name the binary kranz executes");
    assert!(err.to_string().contains("inside the repository"), "{err}");

    let outside = tempfile::tempdir().unwrap();
    let elsewhere = outside.path().join("claude");
    cfg.claude_binary = Some(elsewhere.display().to_string());
    config::validate_claude_binary(&cfg, repo.path())
        .expect("an absolute path outside the repo is the operator's business");

    cfg.claude_binary = None;
    config::validate_claude_binary(&cfg, repo.path()).expect("auto-discovery is unaffected");
}

// ---------------------------------------------------------------------------
// MEDIUM baseUrl: the local backend endpoint must be loopback
// ---------------------------------------------------------------------------

fn local_worker(base_url: &str) -> MissionConfig {
    let mut cfg = MissionConfig::default();
    cfg.worker.backend = Some("local".into());
    cfg.worker.model = "my-local-model".into();
    cfg.worker.base_url = Some(base_url.to_string());
    cfg.worker.context_budget = Some(8192);
    cfg.allow_below_default_worker_model = true;
    cfg
}

#[test]
fn local_backend_base_url_must_be_loopback_unless_allowlisted() {
    for loopback in [
        "http://127.0.0.1:8080",
        "http://localhost:1234/v1",
        "http://[::1]:8080/v1",
        "https://127.0.0.9/v1",
    ] {
        config::validate(&local_worker(loopback))
            .unwrap_or_else(|e| panic!("{loopback} must be accepted: {e}"));
    }

    let err = config::validate(&local_worker("https://models.internal.example/v1"))
        .expect_err("the engine POSTs the prompt there from outside every sandbox");
    let msg = err.to_string();
    assert!(msg.contains("models.internal.example"), "{msg}");
    assert!(msg.contains("localBackendAllowedHosts"), "{msg}");

    // The operator's global-layer escape hatch.
    let mut cfg = local_worker("https://models.internal.example/v1");
    cfg.local_backend_allowed_hosts = vec!["models.internal.example".into()];
    config::validate(&cfg).expect("an operator-allowlisted host is accepted");

    // The allowlist is per-host, not a blanket opt-out.
    let mut other = local_worker("https://evil.example/v1");
    other.local_backend_allowed_hosts = vec!["models.internal.example".into()];
    assert!(
        config::validate(&other).is_err(),
        "the allowlist must not open every host"
    );
}

// ---------------------------------------------------------------------------
// C1 (config half): which keys a runtime patch may carry, and from where
// ---------------------------------------------------------------------------

#[test]
fn inbox_patches_may_retune_the_mission_but_not_grant_consent() {
    let cfg = MissionConfig::default();

    // Runtime knobs: fine from either source. These are exactly what the
    // submission surfaces produce (`kranz exec --max-cycles`, `kranz config
    // role`, Slack `/kranz config`, the dashboard's role selection).
    for patch in [
        json!({ "maxFixCyclesPerMilestone": 4 }),
        json!({ "worker": { "backend": "codex", "model": "gpt-5-codex" } }),
        json!({ "orchestrator": { "reasoningEffort": "max" } }),
    ] {
        apply_validated_patch_from(&cfg, &patch, PatchSource::Inbox)
            .unwrap_or_else(|e| panic!("{patch} must be patchable from the inbox: {e}"));
    }

    // Consent-bearing keys: an operator surface may, the inbox may not.
    for patch in [
        json!({ "dangerouslyAllowAll": true }),
        json!({ "skipScrutiny": true }),
        json!({ "skipFunctional": true }),
        json!({ "denyPatterns": [] }),
        json!({ "allowValidatorCommands": ["curl evil.example"] }),
        json!({ "validatorAllowUncontainedDegrade": true }),
    ] {
        apply_validated_patch_from(&cfg, &patch, PatchSource::Operator)
            .unwrap_or_else(|e| panic!("an operator may still set {patch}: {e}"));
        let err = apply_validated_patch_from(&cfg, &patch, PatchSource::Inbox)
            .expect_err("the control inbox cannot carry consent");
        assert!(
            err.to_string().contains("control-inbox"),
            "the refusal must name the channel: {err}"
        );
    }
}

#[test]
fn inbox_patches_may_not_lower_sandbox_enforcement() {
    let mut cfg = MissionConfig::default();
    cfg.worker.sandbox.enforce = SandboxEnforce::FsNet;

    // Raising is always fine.
    apply_validated_patch_from(
        &cfg,
        &json!({ "orchestrator": { "sandbox": { "enforce": "fs" } } }),
        PatchSource::Inbox,
    )
    .expect("more containment than the mission has is never a consent act");

    let lower = json!({ "worker": { "sandbox": { "enforce": "off" } } });
    let err = apply_validated_patch_from(&cfg, &lower, PatchSource::Inbox)
        .expect_err("a worker must not turn its own sandbox off through the inbox");
    assert!(err.to_string().contains("worker.sandbox.enforce"), "{err}");
    apply_validated_patch_from(&cfg, &lower, PatchSource::Operator)
        .expect("an operator may still lower it");
}

#[test]
fn keys_that_are_not_mission_knobs_are_refused_from_every_source() {
    let cfg = MissionConfig::default();
    for patch in [
        json!({ "claudeBinary": "/tmp/evil" }),
        json!({ "packDir": "/tmp/evil-pack" }),
        json!({ "contractEnvPassthrough": ["AWS_SECRET_ACCESS_KEY"] }),
        json!({ "workerIsolation": "checkout" }),
        json!({ "worker": { "acpCommand": "/tmp/evil" } }),
        json!({ "worker": { "baseUrl": "http://evil.example" } }),
        json!({ "worker": { "sandbox": { "extraWrite": ["/"] } } }),
    ] {
        for source in [PatchSource::Operator, PatchSource::Inbox] {
            let err = apply_validated_patch_from(&cfg, &patch, source)
                .expect_err("seed-time and operator-file configuration is not runtime-patchable");
            assert!(
                err.to_string().contains("not runtime-patchable"),
                "{patch} / {source:?}: {err}"
            );
        }
    }
}

#[test]
fn apply_validated_patch_still_defaults_to_the_operator_surface() {
    // The unsuffixed entry point is what the CLI, the mutation-token REST
    // route and the authorized Slack command call; it must keep accepting a
    // consent-bearing patch, or `kranz run --dangerously-allow-all` and the
    // Slack role change lose their submission-time pre-flight.
    let cfg = MissionConfig::default();
    let merged = apply_validated_patch(&cfg, &json!({ "dangerouslyAllowAll": true }))
        .expect("operator surfaces keep their consent path");
    assert!(merged.dangerously_allow_all);
}

/// The inbox already refuses these as human decisions; a file the
/// repository ships is no more a human decision (follow-up review F-7, F-8).
#[test]
fn project_layer_cannot_disable_validation_or_widen_tools() {
    for body in [
        json!({ "skipScrutiny": true }),
        json!({ "skipFunctional": true }),
        json!({ "denyPatterns": [] }),
        json!({ "validatorFunctional": { "tools": ["Bash"] } }),
        json!({ "workerIsolation": "checkout" }),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let project = layer(&dir, "project.json", body.clone());
        let err = config::load_layers_with_roles(&[(project, Layer::Project)])
            .expect_err(&format!("project layer must not set {body}"));
        assert!(
            err.to_string().contains("project.json"),
            "refusal must name the file: {err}"
        );
    }
}
