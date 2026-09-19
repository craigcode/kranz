use super::*;
use crate::backend::{AgentBackend, AgentEvent, AgentSession, PromptMode, SessionExit};
use crate::backend_acp::AcpBackend;
use crate::sandbox::{ResolvedSandbox, SandboxBackend};
use crate::sandbox_container::ContainerSpec;
use crate::types::SandboxEnforce;
use std::process::Stdio;

const IMAGE: &str =
    "python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d";
const PEER: &str = include_str!("peer.py");

fn enabled() -> bool {
    if std::env::var("KRANZ_ACP_CONTAINER_TESTS").as_deref() != Ok("1") {
        eprintln!("SKIP-ACP-CONTAINMENT: set KRANZ_ACP_CONTAINER_TESTS=1 for real Docker proofs");
        return false;
    }
    true
}

fn spec(root: &Path, name: &str, mode: &str) -> SessionSpec {
    SessionSpec {
        cwd: root.join("workspace"),
        prompt: PromptMode::SingleShot("fixture".into()),
        append_system_prompt: None,
        model: "fixture".into(),
        effort: "default".into(),
        session_id: uuid::Uuid::new_v4().to_string(),
        resume: None,
        permission_mode: None,
        allowed_tools: vec![],
        disallowed_tools: vec![],
        tools: vec![],
        writable: true,
        settings_json: None,
        json_schema: None,
        max_budget_usd: None,
        max_turns: None,
        env: HashMap::from([("FIXTURE_MODE".into(), mode.into())]),
        sandbox: Some(ResolvedSandbox {
            backend: SandboxBackend::Container,
            inputs: SandboxInputs {
                enforce: SandboxEnforce::FsNet,
                session_cwd: root.join("workspace"),
                mission_dir: root.join("workspace/.kranz/missions/m-fixture"),
                tmpdir: root.join("home"),
                extra_write: vec![],
                egress: vec![],
                validator_read_deny_roots: vec![],
            },
            container: Some(ContainerSpec {
                runtime: ContainerRuntime::Docker,
                image: std::env::var("KRANZ_ACP_PROOF_IMAGE").unwrap_or_else(|_| IMAGE.into()),
                network: None,
                name: Some(name.into()),
            }),
        }),
        hook_status: None,
    }
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir_in(crate::backend_claude::scratch_root_base()).unwrap();
    std::fs::create_dir_all(dir.path().join("workspace/.kranz/missions/m-fixture")).unwrap();
    std::fs::create_dir_all(dir.path().join("home")).unwrap();
    std::fs::write(dir.path().join("workspace/peer.py"), PEER).unwrap();
    dir
}

async fn start(root: &Path, name: &str, mode: &str) -> Box<dyn AgentSession> {
    AcpBackend::new(
        "/usr/local/bin/python3",
        vec![root.join("workspace/peer.py").display().to_string()],
    )
    .start(spec(root, name, mode))
    .await
    .unwrap()
}

fn name() -> String {
    format!("kranz-acp-proof-{}", uuid::Uuid::new_v4().simple())
}

async fn absent(root: &Path, name: &str) {
    let client = DockerEvaluator::new(
        &trusted_docker(&spec(root, name, "idle").sandbox.unwrap().inputs).unwrap(),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let out = client
                .control(&[
                    "container".into(),
                    "ls".into(),
                    "--all".into(),
                    "--filter".into(),
                    format!("name=^/{name}$"),
                    "--format".into(),
                    "{{.ID}}".into(),
                ])
                .await
                .unwrap();
            assert_eq!(out.code, Some(0));
            if out.stdout.iter().all(u8::is_ascii_whitespace) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("daemon still owns the namespace");
}

async fn ready(root: &Path) {
    tokio::time::timeout(Duration::from_secs(20), async {
        while !root.join("workspace/ready").exists() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("fixture readiness deadline");
}

#[tokio::test]
async fn acp_containment_v1_abort_and_drop_remove_detached_children() {
    if !enabled() {
        return;
    }
    for dropped in [false, true] {
        let root = fixture();
        let name = name();
        let mut session = start(root.path(), &name, "idle").await;
        ready(root.path()).await;
        if dropped {
            drop(session);
        } else {
            session.abort().await.unwrap();
            assert!(matches!(session.exit_status(), Some(SessionExit::Aborted)));
        }
        absent(root.path(), &name).await;
        let heartbeat = std::fs::read(root.path().join("workspace/child-heartbeat")).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            heartbeat,
            std::fs::read(root.path().join("workspace/child-heartbeat")).unwrap()
        );
    }
}

#[tokio::test]
async fn acp_containment_v1_completion_preserves_report_and_removes_namespace() {
    if !enabled() {
        return;
    }
    let root = fixture();
    let name = name();
    let mut session = start(root.path(), &name, "complete").await;
    let mut result = false;
    while let Some(event) = session.next_event().await.unwrap() {
        if let AgentEvent::Result { text, is_error, .. } = event {
            assert!(!is_error);
            assert_eq!(text, "fixture-delivery");
            result = true;
        }
    }
    assert!(result);
    assert!(
        matches!(session.exit_status(), Some(SessionExit::Completed)),
        "{:?}",
        session.exit_status()
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("workspace/delivered.txt")).unwrap(),
        "feature"
    );
    absent(root.path(), &name).await;
}

#[tokio::test]
async fn acp_containment_v1_engine_sigkill_expires_guest_lease() {
    if !enabled() {
        return;
    }
    let root = fixture();
    let name = name();
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "acp_container::tests::acp_containment_v1_owner_process",
            "--exact",
            "--nocapture",
        ])
        .env_clear()
        .envs(ContainerRuntime::Docker.client_env())
        .env(
            crate::backend_claude::SCRATCH_ROOT_ENV,
            crate::backend_claude::scratch_root_base(),
        )
        .env("KRANZ_ACP_OWNER_ROOT", root.path())
        .env("KRANZ_ACP_OWNER_NAME", &name)
        .env(
            "KRANZ_ACP_PROOF_IMAGE",
            std::env::var("KRANZ_ACP_PROOF_IMAGE").unwrap_or_else(|_| IMAGE.into()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut owner = command.spawn().unwrap();
    ready(root.path()).await;
    owner.kill().await.unwrap(); // SIGKILL: no Rust Drop or cleanup task can run.
    absent(root.path(), &name).await;
    let heartbeat = std::fs::read(root.path().join("workspace/child-heartbeat")).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        heartbeat,
        std::fs::read(root.path().join("workspace/child-heartbeat")).unwrap()
    );
}

#[test]
fn acp_containment_v1_owner_process() {
    let Some(root) = std::env::var_os("KRANZ_ACP_OWNER_ROOT") else {
        return;
    };
    let name = std::env::var("KRANZ_ACP_OWNER_NAME").unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let root = Path::new(&root);
            if std::env::var("KRANZ_ACP_OWNER_MODE").as_deref() == Ok("prepared") {
                let (owned, _command) = OwnedContainer::prepare(
                    &spec(root, &name, "complete"),
                    Path::new("/usr/local/bin/python3"),
                    &[root.join("workspace/peer.py").display().to_string()],
                )
                .await
                .unwrap();
                std::fs::write(
                    root.join("prepared"),
                    owned
                        .root
                        .as_ref()
                        .unwrap()
                        .path()
                        .as_os_str()
                        .as_encoded_bytes(),
                )
                .unwrap();
                std::future::pending::<()>().await;
                drop(owned);
            }
            let mut spec = spec(root, &name, "blocked-stdin");
            spec.prompt = PromptMode::SingleShot("blocked".repeat(1_000_000));
            let _session = AcpBackend::new(
                "/usr/local/bin/python3",
                vec![root.join("workspace/peer.py").display().to_string()],
            )
            .start(spec)
            .await
            .unwrap();
            std::future::pending::<()>().await;
        });
}

#[tokio::test]
async fn acp_containment_v1_delayed_start_requires_a_live_owner() {
    if !enabled() {
        return;
    }
    let root = fixture();
    let name = name();
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "acp_container::tests::acp_containment_v1_owner_process",
            "--exact",
            "--nocapture",
        ])
        .env_clear()
        .envs(ContainerRuntime::Docker.client_env())
        .env(
            crate::backend_claude::SCRATCH_ROOT_ENV,
            crate::backend_claude::scratch_root_base(),
        )
        .env("KRANZ_ACP_OWNER_ROOT", root.path())
        .env("KRANZ_ACP_OWNER_NAME", &name)
        .env(
            "KRANZ_ACP_PROOF_IMAGE",
            std::env::var("KRANZ_ACP_PROOF_IMAGE").unwrap_or_else(|_| IMAGE.into()),
        )
        .env("KRANZ_ACP_OWNER_MODE", "prepared")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut owner = command.spawn().unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !root.path().join("prepared").exists() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
    owner.kill().await.unwrap();
    let client = DockerEvaluator::new(
        &trusted_docker(&spec(root.path(), &name, "idle").sandbox.unwrap().inputs).unwrap(),
    )
    .unwrap();
    let mut start = client.attached_command(&[
        "start".into(),
        "--attach".into(),
        "--interactive".into(),
        name.clone(),
    ]);
    start.stdin(Stdio::null()).kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(12), start.output())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(output.status.code(), Some(124));
    assert!(!root.path().join("workspace/delivered.txt").exists());
    absent(root.path(), &name).await;
    let ledger_root = PathBuf::from(std::fs::read_to_string(root.path().join("prepared")).unwrap());
    assert!(
        ledger_root.join("container.json").exists(),
        "owner death must leave a recovery receipt"
    );
    assert_eq!(
        ledger_root.parent().unwrap().canonicalize().unwrap(),
        crate::backend_claude::scratch_root_base()
            .canonicalize()
            .unwrap()
    );
    assert!(ledger_root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("kranz-acp-owned-"));
    std::fs::remove_dir_all(ledger_root).unwrap();
}

#[tokio::test]
async fn acp_containment_v1_cleanup_failure_retains_recovery_evidence() {
    if !enabled() {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let root = fixture();
    let name = name();
    let (mut owned, _) = OwnedContainer::prepare(
        &spec(root.path(), &name, "idle"),
        Path::new("/usr/local/bin/python3"),
        &[root.path().join("workspace/peer.py").display().to_string()],
    )
    .await
    .unwrap();
    let client = owned.client.clone();
    let unavailable = root.path().join("unavailable-docker");
    std::fs::write(&unavailable, "#!/bin/sh\nexit 99\n").unwrap();
    std::fs::set_permissions(&unavailable, std::fs::Permissions::from_mode(0o700)).unwrap();
    owned.client = DockerEvaluator::new(&unavailable).unwrap();
    let failed = owned.remove().await;
    let retained = owned
        .root
        .as_ref()
        .is_some_and(|r| r.path().join("container.json").exists());
    owned.client = client;
    owned.remove().await.unwrap();
    assert!(failed.is_err());
    assert!(retained);
    assert!(owned.removed);
    absent(root.path(), &name).await;
}

#[tokio::test]
async fn acp_containment_v1_removal_waits_for_confirmed_daemon_absence() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let docker = root.path().join("fake-docker");
    // The daemon rejects a concurrent rm, then lists the deleting container
    // once more before it disappears. There must be no repeated rm request.
    std::fs::write(
        &docker,
        r#"#!/bin/sh
case "$1" in
    container)
        if [ ! -e "$0.removing" ]; then
            printf '%064d\n' 1
        elif [ ! -e "$0.observed" ]; then
            touch "$0.observed"
            printf '%064d\n' 1
        fi
        ;;
    rm)
        printf 'request\n' >> "$0.requests"
        [ ! -e "$0.removing" ] || exit 99
        touch "$0.removing"
        echo 'removal is already in progress' >&2
        exit 1
        ;;
    *) exit 98 ;;
esac
"#,
    )
    .unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o700)).unwrap();
    let client = DockerEvaluator::new(&docker).unwrap();
    remove(&client, "fixture-owner", true, true).await.unwrap();
    assert!(root.path().join("fake-docker.observed").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("fake-docker.requests")).unwrap(),
        "request\n"
    );
}

#[tokio::test]
async fn acp_containment_v1_removal_never_accepts_a_persisting_namespace() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let docker = root.path().join("fake-docker");
    std::fs::write(
        &docker,
        "#!/bin/sh\ncase \"$1\" in\ncontainer) printf '%064d\\n' 1 ;;\nrm) exit 0 ;;\n*) exit 98 ;;\nesac\n",
    )
    .unwrap();
    std::fs::set_permissions(&docker, std::fs::Permissions::from_mode(0o700)).unwrap();
    let client = DockerEvaluator::new(&docker).unwrap();
    let result = remove(&client, "fixture-owner", true, true).await;
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("daemon removal could not be confirmed within deadline"));
}

#[tokio::test]
async fn acp_containment_v1_name_collision_never_removes_an_unowned_container() {
    if !enabled() {
        return;
    }
    let root = fixture();
    let name = name();
    let spec = spec(root.path(), &name, "idle");
    let client =
        DockerEvaluator::new(&trusted_docker(&spec.sandbox.as_ref().unwrap().inputs).unwrap())
            .unwrap();
    let created = client
        .control(&[
            "create".into(),
            "--name".into(),
            name.clone(),
            "--network=none".into(),
            IMAGE.into(),
            "/bin/true".into(),
        ])
        .await
        .unwrap();
    assert_eq!(created.code, Some(0));
    let id = std::str::from_utf8(&created.stdout)
        .unwrap()
        .trim()
        .to_string();
    let result = OwnedContainer::prepare(&spec, Path::new("/usr/local/bin/python3"), &[]).await;
    tokio::time::sleep(Duration::from_millis(300)).await; // Let error-path Drop attempt cleanup.
    let inspection = client
        .control(&["container".into(), "inspect".into(), id.clone()])
        .await
        .unwrap();
    client
        .control(&["rm".into(), "--force".into(), id])
        .await
        .unwrap();
    assert!(result.is_err());
    assert_eq!(
        inspection.code,
        Some(0),
        "a colliding name must not confer ownership"
    );
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

#[tokio::test]
async fn acp_containment_v1_hostile_io_and_worktree_delivery() {
    if !enabled() {
        return;
    }
    let root = tempfile::tempdir_in(crate::backend_claude::scratch_root_base()).unwrap();
    let root = root.path();
    git(root, &["init", "-b", "main", "primary"]);
    let primary = root.join("primary");
    std::fs::write(primary.join("README"), "base\n").unwrap();
    git(&primary, &["add", "README"]);
    git(&primary, &["commit", "-m", "base"]);
    let base = git(&primary, &["rev-parse", "HEAD"]);
    let workspace = root.join("workspace");
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-b",
            "fixture",
            workspace.to_str().unwrap(),
        ],
    );
    std::fs::create_dir_all(workspace.join(".kranz/missions/m-fixture")).unwrap();
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::write(workspace.join("peer.py"), PEER).unwrap();
    for name in [
        "serve.token",
        "serve.read.token",
        "config.json",
        "missions/m-fixture/state.json",
    ] {
        std::fs::write(workspace.join(".kranz").join(name), "fixture-authority").unwrap();
    }
    let outside = root.join("outside");
    std::fs::write(&outside, "unchanged").unwrap();
    let config = std::fs::read(primary.join(".git/config")).unwrap();
    let name = name();
    let mut spec = spec(root, &name, "hostile");
    for (key, path) in [
        ("FIXTURE_OUTSIDE", outside.clone()),
        ("FIXTURE_GIT_CONFIG", primary.join(".git/config")),
        ("FIXTURE_BASE_REF", primary.join(".git/refs/heads/main")),
    ] {
        spec.env.insert(key.into(), path.display().to_string());
    }
    let mut session = AcpBackend::new(
        "/usr/local/bin/python3",
        vec![workspace.join("peer.py").display().to_string()],
    )
    .start(spec)
    .await
    .unwrap();
    while session.next_event().await.unwrap().is_some() {}
    assert!(
        matches!(session.exit_status(), Some(SessionExit::Completed)),
        "{:?}",
        session.exit_status()
    );
    let probes: HashMap<String, bool> =
        serde_json::from_slice(&std::fs::read(workspace.join("probes.json")).unwrap()).unwrap();
    assert_eq!(probes.len(), 13);
    assert!(probes.values().all(|v| *v), "{probes:?}");
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "unchanged");
    assert_eq!(std::fs::read(primary.join(".git/config")).unwrap(), config);
    assert_eq!(git(&primary, &["rev-parse", "HEAD"]), base);
    assert_eq!(git(&primary, &["status", "--porcelain"]), "");
    assert_eq!(
        std::fs::read_to_string(workspace.join(".kranz/missions/m-fixture/state.json")).unwrap(),
        "fixture-authority"
    );
    // As in the worker runner, the engine records the worker's actual change
    // as a feature commit. The primary checkout and base ref stay untouched.
    git(&workspace, &["add", "delivered.txt"]);
    git(&workspace, &["commit", "-m", "fixture feature"]);
    assert_eq!(
        git(
            &workspace,
            &["rev-list", "--count", &format!("{base}..HEAD")]
        ),
        "1"
    );
    assert_eq!(git(&primary, &["rev-parse", "HEAD"]), base);
    assert_eq!(
        std::fs::read_to_string(primary.join("README")).unwrap(),
        "base\n"
    );
    absent(root, &name).await;
}

#[test]
fn acp_containment_v1_image_reference_requires_an_immutable_digest() {
    assert!(pinned_image(IMAGE));
    assert!(pinned_image(&format!("sha256:{}", "a".repeat(64))));
    for bad in ["python:latest", "python@sha256:abc", "sha256:../../escape"] {
        assert!(!pinned_image(bad));
    }
}

#[tokio::test]
async fn acp_containment_v1_unqualified_inputs_are_refused_before_creation() {
    let root = fixture();
    for invalid in ["tag", "runtime", "off", "cwd", "network"] {
        let mut spec = spec(root.path(), &name(), "complete");
        let sandbox = spec.sandbox.as_mut().unwrap();
        match invalid {
            "tag" => sandbox.container.as_mut().unwrap().image = "python:latest".into(),
            "runtime" => sandbox.container.as_mut().unwrap().runtime = ContainerRuntime::Podman,
            "off" => sandbox.inputs.enforce = SandboxEnforce::Off,
            "cwd" => spec.cwd = root.path().into(),
            "network" => {
                sandbox.inputs.egress = vec!["allowed.invalid:443".into()];
                sandbox.container.as_mut().unwrap().network = Some("host".into());
            }
            _ => unreachable!(),
        }
        assert!(
            OwnedContainer::prepare(&spec, Path::new("/usr/local/bin/python3"), &[])
                .await
                .is_err()
        );
    }
    assert!(!root.path().join("workspace/delivered.txt").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_containment_v1_filtered_egress_uses_existing_relay_and_records_denial() {
    if !enabled() {
        return;
    }
    let root = fixture();
    let name = name();
    let allowed = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = allowed.local_addr().unwrap().port();
    let echo = tokio::spawn(async move {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut stream, _) = allowed.accept().await.unwrap();
        let mut request = [0; 4];
        stream.read_exact(&mut request).await.unwrap();
        assert_eq!(&request, b"ping");
        stream.write_all(b"pong").await.unwrap();
    });
    let mut spec = spec(root.path(), &name, "egress");
    spec.sandbox.as_mut().unwrap().inputs.egress = vec![format!("127.0.0.1:{port}")];
    spec.env
        .insert("FIXTURE_ALLOWED".into(), format!("127.0.0.1:{port}"));
    let paths = crate::paths::MissionPaths::new(&spec.cwd, "m-fixture");
    let boundary = crate::egress_proxy::maybe_start_for_session(&mut spec, &paths)
        .await
        .unwrap()
        .unwrap();
    let worker = spec
        .sandbox
        .as_ref()
        .unwrap()
        .container
        .as_ref()
        .unwrap()
        .name
        .clone()
        .unwrap();
    let mut session = AcpBackend::new(
        "/usr/local/bin/python3",
        vec![root.path().join("workspace/peer.py").display().to_string()],
    )
    .start(spec)
    .await
    .unwrap();
    while session.next_event().await.unwrap().is_some() {}
    let status = session.exit_status();
    let denials = boundary.shutdown().await.unwrap();
    assert!(matches!(status, Some(SessionExit::Completed)), "{status:?}");
    tokio::time::timeout(Duration::from_secs(5), echo)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(denials.len(), 1);
    assert_eq!(denials[0].host, "denied.invalid");
    let evidence: HashMap<String, bool> =
        serde_json::from_slice(&std::fs::read(root.path().join("workspace/egress.json")).unwrap())
            .unwrap();
    assert_eq!(evidence.len(), 3);
    assert!(evidence.values().all(|v| *v));
    absent(root.path(), &worker).await;
}
