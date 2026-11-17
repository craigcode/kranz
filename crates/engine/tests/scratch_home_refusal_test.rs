//! A worker whose session is refused before it starts leaves no scratch home.
//!
//! This file holds exactly one test, and must keep to one. The test changes
//! `PATH` for the whole process and then lists the process's scratch
//! directory, so a second test running beside it would see the changed
//! `PATH` and would add its own entries to the listing.
//!
//! Unix only: scratch homes are not removed on other platforms (see
//! `kranz_engine::session_scratch`).

#![cfg(unix)]

use kranz_engine::auth_verify::AuthVerdict;
use kranz_engine::backend_mock::{MockBackend, MockScript};
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::events::EventKind;
use kranz_engine::paths::MissionPaths;
use kranz_engine::runner::run_worker;
use kranz_engine::types::{
    Feature, FeatureOrigin, FeatureStatus, MissionConfig, SandboxEnforce, SandboxProvider,
};
use std::time::{Duration, Instant};

const MISSION: &str = "m-refused";

/// The names of the session scratch homes now in the scratch directory.
fn scratch_homes() -> Vec<String> {
    let base = kranz_engine::backend_claude::scratch_root_base();
    let Ok(entries) = std::fs::read_dir(&base) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.starts_with("kranz-worker-home-"))
        .collect()
}

/// The worker spec builder seeds the session's scratch home, which can hold a
/// copy of the credential file, and only then resolves the sandbox. When the
/// sandbox is required and cannot be resolved the session is refused and
/// never starts, so nothing downstream would remove that home. The builder
/// owns it from before the seeding, and the refusal removes it.
#[tokio::test]
async fn a_worker_refused_for_its_sandbox_leaves_no_scratch_home() {
    kranz_engine::test_dirs::isolate_worker_scratch();
    let dir = tempfile::tempdir().unwrap();
    let paths = MissionPaths::new(dir.path(), MISSION);
    let mut log =
        EventLog::acquire(&paths, MISSION, Duration::from_millis(0), LockForce::No).unwrap();
    log.append(EventKind::MissionCreated {
        goal: "test a refused worker".to_string(),
        base_branch: "main".to_string(),
        mission_branch: format!("kranz/mission-{MISSION}"),
        config: MissionConfig::default(),
    })
    .unwrap();

    // A container sandbox is required, and no container runtime can be found
    // because `PATH` names one empty directory. That refuses the session on
    // every platform.
    let empty_path = tempfile::tempdir().unwrap();
    std::env::set_var("PATH", empty_path.path());
    let mut cfg = MissionConfig::default();
    cfg.worker.sandbox.enforce = SandboxEnforce::Fs;
    cfg.worker.sandbox.provider = SandboxProvider::Container;

    let feature = Feature {
        id: "f-1".to_string(),
        title: "Add login".to_string(),
        spec: "Build the login endpoint".to_string(),
        validation_criteria: vec!["users can log in".to_string()],
        origin: FeatureOrigin::Plan,
        status: FeatureStatus::Pending,
        worker_runs: vec![],
        commits: vec![],
        respawns: 0,
    };
    // A script is queued, so a session that wrongly started would succeed.
    let backend = MockBackend::with_scripts(vec![MockScript::single_shot("unreachable")]);
    assert_eq!(scratch_homes(), Vec::<String>::new());

    let result = run_worker(
        &backend,
        &mut log,
        &paths,
        &cfg,
        &feature,
        "ship the auth system",
        "Auth",
        None,
        None,
        None,
        &[],
        &[],
        &[],
        // Authenticated is the verdict under which the home is seeded with
        // the credential copy before the sandbox is resolved.
        AuthVerdict::Authenticated,
        &[],
        None,
        None,
    )
    .await;

    let error = result.expect_err("a required sandbox that cannot be resolved refuses the run");
    assert!(
        backend.started_specs().is_empty(),
        "the session must never start: {error}"
    );

    // The owner was dropped by the refusal, which hands the removal to the
    // runtime's blocking pool, so allow it a moment.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !scratch_homes().is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        scratch_homes(),
        Vec::<String>::new(),
        "the refused session's scratch home must be removed"
    );
}
