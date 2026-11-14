//! `signals.sentry` config-layer trust (mission `m-c5cd5f`, feature `f-2-2`):
//! `apiBaseUrl`, `tokenEnv` and `allowedHosts` control where triage
//! write-back is sent and which credentials it uses, so they are
//! operator-only (global layer); the project layer may set `secret` (the
//! inbound HMAC key) only, merged field-by-field so it can never wipe the
//! operator's endpoint config. All tests are `signals_sentry_layer_`-prefixed
//! so `cargo test --workspace signals_sentry_layer_` matches this work and
//! nothing else.

use kranz_engine::signals::triage::SignalsConfig;
use serde_json::json;
use std::sync::Mutex;
use tempfile::TempDir;

const G_KEY: &str = "g-sec";

/// `SignalsConfig::load` reads `crate::paths::global_config()`, which
/// resolves `$KRANZ_HOME` (or `$HOME`/`.kranz`) through a process-wide
/// `OnceLock`: the directory is fixed on first read and never changes again
/// in this process. So every test in this file is pinned to one global
/// kranz dir (set once, below) and must serialise through this mutex because
/// they overwrite that dir's shared `config.json` between scenarios.
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn global_dir() -> std::path::PathBuf {
    static DIR: std::sync::OnceLock<TempDir> = std::sync::OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("KRANZ_HOME", dir.path());
        // Force paths::global_kranz_dir()'s OnceLock to resolve now, while
        // KRANZ_HOME is set, before any test races it from another thread.
        assert_eq!(
            kranz_engine::paths::global_kranz_dir(),
            Some(dir.path().to_path_buf())
        );
        dir
    });
    dir.path().to_path_buf()
}

fn write_global_config(value: &serde_json::Value) {
    std::fs::write(
        global_dir().join("config.json"),
        serde_json::to_string(value).unwrap(),
    )
    .unwrap();
}

fn clear_global_config() {
    let _ = std::fs::remove_file(global_dir().join("config.json"));
}

fn write_project_config(repo_root: &std::path::Path, value: &serde_json::Value) {
    let kranz = repo_root.join(".kranz");
    std::fs::create_dir_all(&kranz).unwrap();
    std::fs::write(
        kranz.join("config.json"),
        serde_json::to_string(value).unwrap(),
    )
    .unwrap();
}

#[test]
fn signals_sentry_layer_project_endpoint_fields_refused() {
    let _guard = TEST_LOCK.lock().unwrap();
    clear_global_config();
    let repo = tempfile::tempdir().unwrap();

    for (field, value) in [
        ("apiBaseUrl", json!("https://evil.example")),
        ("tokenEnv", json!("EVIL_TOKEN")),
        ("allowedHosts", json!(["evil.example"])),
    ] {
        let mut sentry = json!({ "secret": "s" });
        sentry
            .as_object_mut()
            .unwrap()
            .insert(field.to_string(), value);
        write_project_config(repo.path(), &json!({ "signals": { "sentry": sentry } }));

        let project_cfg_path = repo.path().join(".kranz").join("config.json");
        let err = SignalsConfig::load(repo.path())
            .expect_err(&format!("project-layer {field} must be refused"));
        let message = err.to_string();
        assert!(
            message.contains(field),
            "error for {field} should name the field, got: {message}"
        );
        assert!(
            message.contains(project_cfg_path.to_str().unwrap()),
            "error for {field} should name the project config path, got: {message}"
        );
    }
}

#[test]
fn signals_sentry_layer_project_secret_merges_with_global() {
    let _guard = TEST_LOCK.lock().unwrap();
    write_global_config(&json!({
        "signals": {
            "sentry": {
                "apiBaseUrl": "https://sentry.io",
                "tokenEnv": "SENTRY_TOKEN",
                "allowedHosts": ["sentry.io", "sentry.example.com"]
            }
        }
    }));
    let repo = tempfile::tempdir().unwrap();
    write_project_config(
        repo.path(),
        &json!({ "signals": { "sentry": { "secret": "s" } } }),
    );

    let cfg = SignalsConfig::load(repo.path()).expect("merge should succeed");
    let sentry = cfg.sentry.expect("sentry config should be present");
    assert_eq!(sentry.secret, Some("s".to_string()));
    assert_eq!(sentry.api_base_url, Some("https://sentry.io".to_string()));
    assert_eq!(sentry.token_env, Some("SENTRY_TOKEN".to_string()));
    assert_eq!(
        sentry.allowed_hosts,
        vec!["sentry.io".to_string(), "sentry.example.com".to_string()]
    );

    clear_global_config();
}

#[test]
fn signals_sentry_layer_non_object_project_block_refused() {
    let _guard = TEST_LOCK.lock().unwrap();
    clear_global_config();
    let repo = tempfile::tempdir().unwrap();

    for bad_value in [json!(["not", "an", "object"]), json!("also-not-an-object")] {
        write_project_config(repo.path(), &json!({ "signals": { "sentry": bad_value } }));
        let err = SignalsConfig::load(repo.path())
            .expect_err("non-object signals.sentry must be refused");
        assert!(
            err.to_string().contains("signals.sentry"),
            "error should name signals.sentry, got: {err}"
        );
    }
}

#[test]
fn signals_sentry_layer_global_may_set_endpoint_fields() {
    let _guard = TEST_LOCK.lock().unwrap();
    write_global_config(&json!({
        "signals": {
            "sentry": {
                "secret": G_KEY,
                "apiBaseUrl": "https://sentry.io",
                "tokenEnv": "SENTRY_TOKEN",
                "allowedHosts": ["sentry.io"]
            }
        }
    }));
    let repo = tempfile::tempdir().unwrap();

    let cfg = SignalsConfig::load(repo.path()).expect("global-only config should load");
    let sentry = cfg.sentry.expect("sentry config should be present");
    assert_eq!(sentry.secret, Some(G_KEY.to_string()));
    assert_eq!(sentry.api_base_url, Some("https://sentry.io".to_string()));
    assert_eq!(sentry.token_env, Some("SENTRY_TOKEN".to_string()));
    assert_eq!(sentry.allowed_hosts, vec!["sentry.io".to_string()]);

    clear_global_config();
}
