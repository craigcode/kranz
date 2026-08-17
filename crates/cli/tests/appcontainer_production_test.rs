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
        "tokenIsLpac",
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
