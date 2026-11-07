//! Exercise the provider entry points, including reset after readiness. Every
//! denied probe also runs with enforcement off to prove the command is viable.

use crate::types::{SandboxConfig, SandboxEnforce, SandboxProvider};
use crate::workspace_data::DataHookKind;
use crate::workspace_provider::{
    GateEnvPolicy, LocalWorktreeProvider, ProvisionSpec, ReadinessOutcome, WorkspaceProvider,
};

fn fixture(enforce: SandboxEnforce) -> (tempfile::TempDir, ProvisionSpec) {
    let repo = tempfile::tempdir().unwrap();
    let runtime_dir = repo.path().join(".kranz/missions/m-workspace-security");
    std::fs::create_dir_all(runtime_dir.join("runs")).unwrap();
    for name in ["serve.token", "serve.read.token"] {
        std::fs::write(repo.path().join(".kranz").join(name), "synthetic-authority").unwrap();
    }
    let spec = ProvisionSpec {
        mission_id: "m-workspace-security".into(),
        repo_root: repo.path().into(),
        gate_env: GateEnvPolicy::for_mission(
            &runtime_dir,
            &[],
            SandboxConfig {
                enforce,
                provider: SandboxProvider::Process,
                ..Default::default()
            },
        ),
        runtime_dir,
        base_sha: None,
        contract: None,
    };
    (repo, spec)
}

async fn run_lane(spec: &ProvisionSpec, lane: &str, command: &str) -> crate::error::Result<bool> {
    let mut spec = spec.clone();
    let mut contract = serde_json::json!({"schemaVersion": 1});
    match lane {
        "bootstrap" | "readiness" => contract[lane] = serde_json::json!([command]),
        _ => contract["data"] = serde_json::json!({lane: command}),
    }
    if lane == "skewCheck" {
        contract["data"]["migrate"] = serde_json::json!("true");
    }
    spec.contract = Some(
        crate::workspace_contract::parse_workspace_contract(
            &serde_json::to_vec(&contract).unwrap(),
        )
        .expect("test workspace contract"),
    );
    let provider = LocalWorktreeProvider;
    let handle = provider.provision(&spec).await?;
    let mut progress = |_: &str, _: Option<String>| Ok(());
    if lane == "reset" {
        Ok(provider
            .run_data_hook(&handle, DataHookKind::Reset, command, &mut progress)
            .await?
            .is_none())
    } else {
        Ok(matches!(
            provider.readiness(&handle, &mut progress).await?,
            ReadinessOutcome::Ready
        ))
    }
}

#[tokio::test]
async fn enforced_workspace_resolution_failure_never_runs_the_host_command() {
    let (repo, mut spec) = fixture(SandboxEnforce::Fs);
    spec.gate_env.sandbox.provider = SandboxProvider::Container;
    // This pair is refused on every host: gates cannot enforce a container
    // hostname allowlist and must never downgrade it to bridge networking.
    spec.gate_env.sandbox.enforce = SandboxEnforce::FsNet;
    spec.gate_env.sandbox.egress = vec!["example.com:443".into()];
    for lane in [
        "bootstrap",
        "readiness",
        "clone",
        "migrate",
        "skewCheck",
        "reset",
    ] {
        let command = crate::test_shell::write_line("escaped", "escaped.txt");
        let result = run_lane(&spec, lane, &command).await;
        assert!(result.is_err(), "{lane} silently degraded: {result:?}");
        assert!(
            !repo.path().join("escaped.txt").exists(),
            "{lane} executed on the host"
        );
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn native_wrap_available() -> bool {
    #[cfg(target_os = "macos")]
    let (program, args, capability) = (
        "sandbox-exec",
        vec!["-p", "(version 1)(allow default)", "/usr/bin/true"],
        crate::test_capability::capability::SANDBOX_EXEC,
    );
    #[cfg(target_os = "linux")]
    let (program, args, capability) = (
        "bwrap",
        vec![
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--",
            "/bin/true",
        ],
        crate::test_capability::capability::BWRAP,
    );
    match std::process::Command::new(program).args(args).output() {
        Ok(output) if output.status.success() => true,
        Ok(_) => {
            eprintln!("SKIP-UNDER-WRAP: workspace probe cannot apply a nested native sandbox");
            false
        }
        Err(error) => {
            crate::test_capability::skip(capability, &error.to_string());
            false
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn workspace_lanes_enforce_filesystem_and_authority_boundaries() {
    if !native_wrap_available() {
        return;
    }
    let (repo, spec) = fixture(SandboxEnforce::Fs);
    let outside = tempfile::tempdir().unwrap();
    let marker = outside.path().join("escaped");
    let mut off = spec.clone();
    off.gate_env.sandbox.enforce = SandboxEnforce::Off;
    for lane in [
        "bootstrap",
        "readiness",
        "clone",
        "migrate",
        "skewCheck",
        "reset",
    ] {
        assert!(
            run_lane(&spec, lane, "echo allowed > allowed.txt")
                .await
                .unwrap(),
            "{lane} permitted write failed"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path().join("allowed.txt"))
                .unwrap()
                .trim(),
            "allowed"
        );
        let write = format!("echo escaped > '{}'", marker.display());
        assert!(
            run_lane(&off, lane, &write).await.unwrap(),
            "{lane} negative control failed"
        );
        std::fs::remove_file(&marker).unwrap();
        assert!(
            !run_lane(&spec, lane, &write).await.unwrap(),
            "{lane} escaped the writable roots"
        );
        assert!(!marker.exists());
        for name in ["serve.token", "serve.read.token"] {
            let read = format!(
                "test -s '{}'",
                repo.path().join(".kranz").join(name).display()
            );
            assert!(run_lane(&off, lane, &read).await.unwrap());
            assert!(
                !run_lane(&spec, lane, &read).await.unwrap(),
                "{lane} read {name}"
            );
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[tokio::test]
async fn workspace_lanes_deny_network_under_fs_net() {
    if !native_wrap_available() {
        return;
    }
    let (_repo, spec) = fixture(SandboxEnforce::FsNet);
    let mut off = spec.clone();
    off.gate_env.sandbox.enforce = SandboxEnforce::Off;
    // UDP connect selects a destination without sending a packet. The fs+net
    // policy forbids this operation; unlike local TCP it cannot be an allowed
    // proxy hop on macOS. No remote service or DNS is required by the control.
    let connect = "python3 -c 'import socket; s=socket.socket(socket.AF_INET, socket.SOCK_DGRAM); s.connect((\"192.0.2.1\", 9)); s.close()'";
    for lane in [
        "bootstrap",
        "readiness",
        "clone",
        "migrate",
        "skewCheck",
        "reset",
    ] {
        assert!(
            run_lane(&off, lane, connect).await.unwrap(),
            "{lane} network negative control failed"
        );
        assert!(run_lane(&spec, lane, "echo offline > offline.txt")
            .await
            .unwrap());
        assert!(
            !run_lane(&spec, lane, connect).await.unwrap(),
            "{lane} escaped the network boundary"
        );
    }
}
