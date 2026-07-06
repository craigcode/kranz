//! Tests for the `workerIsolation` mission-config key (M7 tier 1, feature f-1-1).
//!
//! This feature only adds the config surface; nothing consumes it yet.

use kranz_engine::config::load_layers;
use kranz_engine::types::{MissionConfig, WorkerIsolation};
use std::path::PathBuf;

fn write_layer(dir: &tempfile::TempDir, name: &str, contents: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, contents).expect("write layer");
    path
}

#[test]
fn worker_isolation_config_defaults_to_checkout() {
    assert_eq!(
        MissionConfig::default().worker_isolation,
        WorkerIsolation::Checkout
    );

    let dir = tempfile::tempdir().expect("tempdir");
    let layer = write_layer(&dir, "config.json", r#"{"maxRespawns":3}"#);

    let cfg = load_layers(&[layer]).expect("load layers");
    assert_eq!(cfg.worker_isolation, WorkerIsolation::Checkout);
    assert_eq!(cfg.max_respawns, 3);
}

#[test]
fn worker_isolation_config_parses_worktree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let layer = write_layer(&dir, "config.json", r#"{"workerIsolation":"worktree"}"#);

    let cfg = load_layers(&[layer]).expect("load layers");
    assert_eq!(cfg.worker_isolation, WorkerIsolation::Worktree);
}

#[test]
fn worker_isolation_config_serializes_camel_case() {
    let value = serde_json::to_value(MissionConfig::default()).expect("serialize");
    assert_eq!(value["workerIsolation"], "checkout");
}

#[test]
fn worker_isolation_config_rejects_unknown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let layer = write_layer(&dir, "config.json", r#"{"workerIsolation":"sandbox"}"#);

    let result = load_layers(&[layer]);
    assert!(result.is_err());
}
