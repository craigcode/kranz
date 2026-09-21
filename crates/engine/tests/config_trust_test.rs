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

fn dotted_patch(key: &str, value: serde_json::Value) -> serde_json::Value {
    key.split('.')
        .rev()
        .fold(value, |value, segment| json!({segment: value}))
}

/// Exercise both public policy checks against an independent matrix. The
/// child cases pin subtree matching, including every normalized role.
#[test]
fn sensitive_keys_and_children_preserve_project_operator_and_inbox_policies() {
    let base = serde_json::to_value(MissionConfig::default()).unwrap();
    let source_file = std::path::Path::new("project.json");
    let mut cases = vec![
        ("skipScrutiny".to_owned(), true, false),
        ("skipFunctional".to_owned(), true, false),
        ("denyPatterns".to_owned(), true, false),
        ("dangerouslyAllowAll".to_owned(), true, false),
        ("allowValidatorCommands".to_owned(), true, false),
        ("validatorAllowUncontainedDegrade".to_owned(), true, false),
        // Existing exception: project files cannot relax this floor, while
        // either runtime source may tune it. Consolidation must not widen it.
        ("allowBelowDefaultWorkerModel".to_owned(), true, true),
    ];
    for key in [
        "workerIsolation",
        "claudeBinary",
        "packDir",
        "contractEnvPassthrough",
        "localBackendAllowedHosts",
        "slack",
        "hookStatus",
        "workspace.remote",
    ] {
        cases.push((key.to_owned(), false, false));
    }
    for role in [
        "orchestrator",
        "worker",
        "validatorScrutiny",
        "validatorFunctional",
    ] {
        for leaf in [
            "tools",
            "acpCommand",
            "acpArgs",
            "acpProfile",
            "baseUrl",
            "sandbox.extraWrite",
            "sandbox.egress",
            "sandbox.provider",
            "sandbox.image",
        ] {
            cases.push((format!("{role}.{leaf}"), false, false));
        }
    }
    for (key, operator_allowed, inbox_allowed) in cases {
        for suffix in ["", ".child"] {
            let dotted = format!("{key}{suffix}");
            let patch = dotted_patch(&dotted, json!(null));
            assert!(
                config::check_project_layer_keys(&patch, &base, source_file).is_err(),
                "project: {dotted}"
            );
            for (source, allowed) in [
                (PatchSource::Operator, operator_allowed),
                (PatchSource::Inbox, inbox_allowed),
            ] {
                assert_eq!(
                    config::check_runtime_patch(&patch, &base, source).is_ok(),
                    allowed,
                    "{source:?}: {dotted}"
                );
            }
        }
        let adjacent = dotted_patch(&format!("{key}Extra"), json!(null));
        config::check_project_layer_keys(&adjacent, &base, source_file).unwrap_or_else(|error| {
            panic!("a shared name prefix is not a child of {key}: {error}")
        });
        assert!(config::check_runtime_patch(&adjacent, &base, PatchSource::Operator).is_err());
    }
}

#[test]
fn directional_trust_policies_preserve_all_role_floors_and_reviewer_runtime_refusal() {
    let source_file = std::path::Path::new("project.json");
    for role in [
        "orchestrator",
        "worker",
        "validatorScrutiny",
        "validatorFunctional",
    ] {
        let key = format!("{role}.sandbox.enforce");
        let base = dotted_patch(&key, json!("fs"));
        for (value, project_allowed, inbox_allowed) in [
            (json!("fs+net"), true, true),
            (json!("fs"), true, true),
            (json!("off"), false, false),
        ] {
            let patch = dotted_patch(&key, value);
            assert_eq!(
                config::check_project_layer_keys(&patch, &base, source_file).is_ok(),
                project_allowed,
                "project: {key}"
            );
            assert_eq!(
                config::check_runtime_patch(&patch, &base, PatchSource::Inbox).is_ok(),
                inbox_allowed,
                "inbox: {key}"
            );
            config::check_runtime_patch(&patch, &base, PatchSource::Operator).unwrap();
        }
        let child = dotted_patch(&format!("{key}.child"), json!(true));
        for source in [PatchSource::Operator, PatchSource::Inbox] {
            assert!(
                config::check_runtime_patch(&child, &base, source).is_err(),
                "{source:?}: {key}.child"
            );
        }
    }
    let base = json!({"reviewerIndependence": {"scrutiny": true, "functional": true}});
    for key in [
        "reviewerIndependence",
        "reviewerIndependence.scrutiny",
        "reviewerIndependence.functional",
        "reviewerIndependence.scrutiny.child",
    ] {
        let patch = dotted_patch(key, json!(null));
        assert!(
            config::check_project_layer_keys(&patch, &base, source_file).is_err(),
            "project: {key}"
        );
        for source in [PatchSource::Operator, PatchSource::Inbox] {
            assert!(
                config::check_runtime_patch(&patch, &base, source).is_err(),
                "{source:?}: {key}"
            );
        }
    }
}

#[test]
fn project_role_sequences_cannot_replace_guarded_fields() {
    let role_sequence = json!([
        "sonnet",
        "high",
        null,
        null,
        ["Bash"],
        null,
        null,
        null,
        [],
        null,
        null,
        ["off", "process", null, [], []]
    ]);
    let role: kranz_engine::types::RoleConfig = serde_json::from_value(role_sequence.clone())
        .expect("fixture: serde accepts positional role structs");
    assert_eq!(role.tools, ["Bash"]);
    assert_eq!(role.sandbox.enforce, SandboxEnforce::Off);

    let dir = tempfile::tempdir().unwrap();
    let global = layer(
        &dir,
        "global.json",
        json!({"worker": {"sandbox": {"enforce": "fs+net"}}}),
    );
    let project = layer(&dir, "project.json", json!({"worker": role_sequence}));
    let error =
        config::load_layers_with_roles(&[(global, Layer::Global), (project, Layer::Project)])
            .expect_err("a positional role replacement must not bypass protected fields");
    assert!(error.to_string().contains("worker"));

    let mut inherited_sequence = role_sequence;
    inherited_sequence[11][0] = json!("fs+net");
    let global = layer(&dir, "global.json", json!({"worker": inherited_sequence}));
    let project = layer(
        &dir,
        "project.json",
        json!({"worker": {"model": "sonnet", "reasoningEffort": "high"}}),
    );
    let error =
        config::load_layers_with_roles(&[(global, Layer::Global), (project, Layer::Project)])
            .expect_err("an object patch must not erase the sandbox in a positional base role");
    assert!(error
        .to_string()
        .contains("non-object inherited field \"worker\""));
}

#[test]
fn project_objects_cannot_erase_a_positional_reviewer_floor() {
    let sequence = json!([true, true]);
    let policy: kranz_engine::types::ReviewerIndependence =
        serde_json::from_value(sequence.clone())
            .expect("fixture: serde accepts positional reviewer policy structs");
    assert!(policy.scrutiny && policy.functional);
    let dir = tempfile::tempdir().unwrap();
    let global = layer(
        &dir,
        "global.json",
        json!({"reviewerIndependence": sequence}),
    );
    let project = layer(
        &dir,
        "project.json",
        json!({"reviewerIndependence": {"scrutiny": false, "functional": false}}),
    );
    let error =
        config::load_layers_with_roles(&[(global, Layer::Global), (project, Layer::Project)])
            .expect_err(
                "a positional base policy must not hide the operator's floor from project checks",
            );
    assert!(error
        .to_string()
        .contains("non-object inherited field \"reviewerIndependence\""));
}

#[test]
fn project_trust_checks_refuse_nonobject_roots_and_protected_containers() {
    let base = serde_json::to_value(MissionConfig::default()).unwrap();
    let source_file = std::path::Path::new("project.json");
    for value in [json!([]), json!(null), json!("replacement")] {
        assert!(config::check_project_layer_keys(&value, &base, source_file).is_err());
        for key in [
            "orchestrator",
            "worker",
            "validatorScrutiny",
            "validatorFunctional",
            "orchestrator.sandbox",
            "worker.sandbox",
            "validatorScrutiny.sandbox",
            "validatorFunctional.sandbox",
            "workspace",
            "reviewerIndependence",
        ] {
            let patch = dotted_patch(key, value.clone());
            let error = config::check_project_layer_keys(&patch, &base, source_file).unwrap_err();
            assert!(error.to_string().contains(key), "{key}: {error}");

            let inherited = dotted_patch(key, value.clone());
            let patch = dotted_patch(key, json!({}));
            let error =
                config::check_project_layer_keys(&patch, &inherited, source_file).unwrap_err();
            assert!(
                error.to_string().contains("non-object inherited field"),
                "{key}: {error}"
            );
        }
    }
    // Leaf lists still follow their declared policy; this shape check must
    // not ban ordinary arrays merely because a JSON patch contains one.
    config::check_project_layer_keys(
        &json!({"routing": {"taskClassRules": []}}),
        &base,
        source_file,
    )
    .unwrap();
    config::check_runtime_patch(
        &json!({"allowValidatorCommands": []}),
        &base,
        PatchSource::Operator,
    )
    .unwrap();
}

#[test]
fn project_sandbox_floor_matches_serde_enum_representations() {
    let inherited = json!({"fs+net": null});
    let enforce: SandboxEnforce = serde_json::from_value(inherited.clone())
        .expect("fixture: serde accepts a map for a unit enum variant");
    assert_eq!(enforce, SandboxEnforce::FsNet);
    let dir = tempfile::tempdir().unwrap();
    let global = layer(
        &dir,
        "global.json",
        json!({"worker": {"sandbox": {"enforce": inherited}}}),
    );
    let project = layer(
        &dir,
        "project.json",
        json!({"worker": {"sandbox": {"enforce": "off"}}}),
    );
    assert!(
        config::load_layers_with_roles(&[(global, Layer::Global), (project, Layer::Project)])
            .is_err(),
        "a map-encoded inherited sandbox mode must retain the same floor as its string form"
    );
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
fn project_layer_reviewer_independence_may_strengthen_but_never_weaken_operator_floor() {
    let dir = tempfile::tempdir().unwrap();
    for role in ["scrutiny", "functional"] {
        let global = layer(
            &dir,
            "global.json",
            json!({"reviewerIndependence": {role: true}}),
        );
        for policy in [json!({role: false}), json!(null), json!([])] {
            let project = layer(
                &dir,
                "project.json",
                json!({"reviewerIndependence": policy}),
            );
            let error = config::load_layers_with_roles(&[
                (global.clone(), Layer::Global),
                (project.clone(), Layer::Project),
            ])
            .unwrap_err()
            .to_string();
            let field = if policy.is_object() {
                format!("reviewerIndependence.{role}")
            } else {
                // Malformed containers fail at the shape boundary before
                // their individual reviewer flags can be compared.
                "reviewerIndependence".to_string()
            };
            assert!(error.contains(&field), "{error}");
            assert!(error.contains(&project.display().to_string()), "{error}");
        }
        for policy in [json!({}), json!({"scrutiny": true, "functional": true})] {
            let project = layer(
                &dir,
                "project.json",
                json!({"reviewerIndependence": policy}),
            );
            let cfg = config::load_layers_with_roles(&[
                (global.clone(), Layer::Global),
                (project, Layer::Project),
            ])
            .unwrap();
            let actual = serde_json::to_value(cfg.reviewer_independence).unwrap();
            assert_eq!(actual[role], true);
        }
    }
    let project = layer(
        &dir,
        "project.json",
        json!({"reviewerIndependence": {"scrutiny": true, "functional": true}}),
    );
    let cfg = config::load_layers_with_roles(&[(project, Layer::Project)]).unwrap();
    assert!(cfg.reviewer_independence.scrutiny && cfg.reviewer_independence.functional);
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
            json!({ "slack": { "botToken": format!("xox{}-evil", "b") } }),
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
