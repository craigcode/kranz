//! M7 phase-4 production-path receipt. Windows-only because the assertion is
//! the real AppContainer token/ACL/Job boundary, never a mocked syscall test.

#[cfg(windows)]
#[test]
fn windows_production_appcontainer_helper_enforces_and_restores_boundary() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_kranz"))
        .arg("__kranz-appcontainer-self-test")
        .output()
        .expect("production AppContainer self-test binary must spawn");
    assert!(
        output.status.success(),
        "production AppContainer self-test failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("production AppContainer receipt must be JSON");
    for field in [
        "tokenIsAppcontainer",
        "allApplicationPackagesDenied",
        "toolchainRead",
        "toolchainWriteDenied",
        "worktreeWrite",
        "scratchWrite",
        "outsideWriteDenied",
        "authorityReadDenied",
        "realCheckoutReadDenied",
        "sharedGitRead",
        "overlappingLeaseSafe",
        "tamperedGitPointerRefused",
        "networkDenied",
        "daclRestored",
    ] {
        assert_eq!(
            receipt.get(field),
            Some(&serde_json::Value::Bool(true)),
            "{field}"
        );
    }
}

/// Phase 5 complements the hostile boundary proof with ordinary toolchain
/// commands through the exact production merge-gate wrapper. The private
/// CLI entry point retains seven interleaved warm-cache timing pairs and
/// fails before returning a receipt if either median misses the roadmap's
/// approximate ten-percent target.
#[cfg(windows)]
#[test]
fn windows_production_appcontainer_runs_normal_node_and_rust_gates_within_target() {
    // Stderr is INHERITED, not captured: the self test reports each retired
    // timing sample there, and thirty-two samples across two gates run long
    // enough that a buffered stream only surfaces after a CI step timeout
    // has already killed the run. The receipt stays on stdout.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_kranz"))
        .arg("__kranz-appcontainer-gate-self-test")
        .stderr(std::process::Stdio::inherit())
        .output()
        .expect("production AppContainer normal-gate self-test binary must spawn");
    assert!(
        output.status.success(),
        "production AppContainer normal-gate self-test failed (its stderr streamed above): stdout={}",
        String::from_utf8_lossy(&output.stdout)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("production AppContainer normal-gate receipt must be JSON");
    println!("{}", String::from_utf8_lossy(&output.stdout));
    assert_eq!(receipt["host"]["hostOs"], "windows");
    assert_eq!(receipt["enforcement"], "fs+net");
    assert_eq!(receipt["provider"], "process/AppContainer-LPAC");
    let target = receipt["overheadTargetPercent"]
        .as_f64()
        .expect("receipt carries a numeric overhead target");
    assert_eq!(target, 10.0);
    for gate in ["node", "rust"] {
        assert_eq!(receipt[gate]["repetitions"], 7, "{gate}");
        assert_eq!(receipt[gate]["withinTarget"], true, "{gate}");
        assert_eq!(
            receipt[gate]["offSamplesMs"].as_array().map(Vec::len),
            Some(7),
            "{gate} off samples"
        );
        assert_eq!(
            receipt[gate]["appcontainerSamplesMs"]
                .as_array()
                .map(Vec::len),
            Some(7),
            "{gate} AppContainer samples"
        );
        assert!(
            receipt[gate]["overheadPercent"]
                .as_f64()
                .is_some_and(|overhead| overhead <= target),
            "{gate} overhead: {}",
            receipt[gate]
        );
    }
}
