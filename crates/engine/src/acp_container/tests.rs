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
async fn acp_containment_v1_reaps_orphans_and_preserves_peer_exit() {
    if !enabled() {
        return;
    }
    for mode in ["orphans-complete", "orphans-failed"] {
        let root = fixture();
        let name = name();
        let mut session = start(root.path(), &name, mode).await;
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut result = false;
            while let Some(event) = session.next_event().await.unwrap() {
                if let AgentEvent::Result { text, .. } = event {
                    assert_eq!(text, "fixture-delivery");
                    result = true;
                }
            }
            assert!(result, "{:?}", session.exit_status());
        })
        .await
        .expect("orphan reaping deadline");
        match (mode, session.exit_status()) {
            ("orphans-complete", Some(SessionExit::Completed)) => {}
            ("orphans-failed", Some(SessionExit::Failed(message))) => {
                assert!(message.contains("23"), "{message}");
            }
            (_, exit) => panic!("unexpected peer exit: {exit:?}"),
        }
        let proof: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.path().join("workspace/orphan-probes.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(proof, serde_json::json!({"orphans": 640, "zombies": 0}));
        absent(root.path(), &name).await;
    }
}

#[test]
fn acp_containment_v1_delayed_attach_status_cannot_hide_failure() {
    use std::os::unix::fs::PermissionsExt;
    if !enabled()
        || crate::agent_env::isolated_global_home_test(
            "acp_container::tests::acp_containment_v1_delayed_attach_status_cannot_hide_failure",
        )
    {
        return;
    }
    let root = fixture();
    let docker =
        trusted_docker(&spec(root.path(), "fixture", "idle").sandbox.unwrap().inputs).unwrap();
    let bin = tempfile::tempdir().unwrap();
    let quote = |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
    let wrapper = bin.path().join("docker");
    let path = std::env::join_paths(
        std::iter::once(bin.path().to_path_buf())
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let _env = crate::agent_env::EnvTestGuard::engage(&[
        ("PATH", path.to_str().unwrap()),
        (
            crate::backend_claude::SCRATCH_ROOT_ENV,
            scratch.path().to_str().unwrap(),
        ),
    ]);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for (delay, expected) in [("0.5", "23"), ("6", "container completion deadline expired")] {
            std::fs::write(
                &wrapper,
                format!(
                    "#!/bin/sh\nif [ \"$1\" != start ]; then exec {docker} \"$@\"; fi\n{docker} \"$@\"\nstatus=$?\nprintf '%s\\n' \"$status\" > {receipt}\nsleep {delay}\nexit \"$status\"\n",
                    docker = quote(&docker),
                    receipt = quote(&bin.path().join("attach-exit")),
                ),
            )
            .unwrap();
                std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
                match std::fs::remove_file(bin.path().join("attach-exit")) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => panic!("clear previous attach receipt: {error}"),
                }
                let name = name();
            let mut session = start(root.path(), &name, "orphans-failed").await;
            tokio::time::timeout(Duration::from_secs(30), async {
                while session.next_event().await.unwrap().is_some() {}
            })
            .await
            .expect("bounded delayed attach completion");
            assert!(
                matches!(session.exit_status(), Some(SessionExit::Failed(ref message)) if message.contains(expected)),
                "delayed guest failure was lost: {:?}",
                session.exit_status()
            );
            assert_eq!(
                std::fs::read_to_string(bin.path().join("attach-exit")).unwrap().trim(),
                "23"
            );
            absent(root.path(), &name).await;
            }
        });
}

#[tokio::test]
async fn acp_containment_v1_input_eof_supervisor_stops_lingering_peer() {
    if !enabled() {
        return;
    }
    let root = fixture();
    let name = name();
    let mut session = start(root.path(), &name, "report-linger").await;
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut result = false;
        while let Some(event) = session.next_event().await.unwrap() {
            if let AgentEvent::Result { is_error, .. } = event {
                assert!(!is_error);
                result = true;
            }
        }
        assert!(result);
    })
    .await
    .expect("bounded EOF completion");
    assert!(
        matches!(session.exit_status(), Some(SessionExit::Completed)),
        "{:?}",
        session.exit_status()
    );
    absent(root.path(), &name).await;
    let heartbeat = std::fs::read(root.path().join("workspace/child-heartbeat")).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        heartbeat,
        std::fs::read(root.path().join("workspace/child-heartbeat")).unwrap()
    );
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
                    None,
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
            let backend = AcpBackend::new(
                "/usr/local/bin/python3",
                vec![root.join("workspace/peer.py").display().to_string()],
            );
            let backend = if std::env::var("KRANZ_ACP_OWNER_MODE").as_deref() == Ok("resources") {
                backend.with_resource_fixture(fixture_resources())
            } else {
                backend
            };
            let _session = backend.start(spec).await.unwrap();
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
    let mut session_spec = spec(root.path(), &name, "idle");
    let marker = "synthetic-launch-secret-never-retained";
    session_spec
        .env
        .insert("CLAUDE_CODE_OAUTH_TOKEN".into(), marker.into());
    let (mut owned, _) = OwnedContainer::prepare(
        &session_spec,
        Path::new("/usr/local/bin/python3"),
        &[root.path().join("workspace/peer.py").display().to_string()],
        None,
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
    let ledger = owned.root.as_ref().unwrap().path();
    assert!(!ledger.join("launch.json").exists());
    for entry in std::fs::read_dir(ledger).unwrap() {
        let bytes = std::fs::read(entry.unwrap().path()).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains(marker));
    }
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
    let result =
        OwnedContainer::prepare(&spec, Path::new("/usr/local/bin/python3"), &[], None).await;
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
    hostile_worktree_delivery("hostile").await;
}

#[tokio::test]
async fn acp_containment_v1_mcp_child_cannot_bypass_boundary() {
    tokio::time::timeout(Duration::from_secs(90), hostile_worktree_delivery("mcp"))
        .await
        .expect("bounded MCP descendant proof");
}

async fn hostile_worktree_delivery(mode: &str) {
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
    let mut spec = spec(root, &name, mode);
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
    while let Some(event) = session.next_event().await.unwrap() {
        assert!(
            !matches!(
                event,
                AgentEvent::PermissionRequested { .. } | AgentEvent::ToolUse { .. }
            ),
            "fixture must bypass ACP callbacks: {event:?}"
        );
    }
    assert!(
        matches!(session.exit_status(), Some(SessionExit::Completed)),
        "{:?}",
        session.exit_status()
    );
    let probes: HashMap<String, bool> =
        serde_json::from_slice(&std::fs::read(workspace.join("probes.json")).unwrap()).unwrap();
    assert_eq!(probes.len(), 13);
    assert!(probes.values().all(|v| *v), "{probes:?}");
    if mode == "mcp" {
        let mut mcp: HashMap<String, bool> =
            serde_json::from_slice(&std::fs::read(workspace.join("mcp-probes.json")).unwrap())
                .unwrap();
        assert_eq!(mcp.remove("private-home"), Some(true));
        assert_eq!(mcp.remove("control-env-absent"), Some(true));
        assert_eq!(mcp, probes, "tool response must carry all boundary probes");
        assert_eq!(
            std::fs::read_to_string(root.join("home/mcp-private-state")).unwrap(),
            "private fixture state"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("delivered.txt")).unwrap(),
            "feature from MCP child"
        );
    }
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
            OwnedContainer::prepare(&spec, Path::new("/usr/local/bin/python3"), &[], None)
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

#[tokio::test]
async fn acp_containment_v1_launch_prelude_delivers_environment_without_consuming_acp() {
    if !enabled() {
        return;
    }
    let root = fixture();
    let name = name();
    let marker = "synthetic-launch-secret";
    std::fs::write(
        root.path().join("workspace/peer.py"),
        format!("import os\nassert os.environ['CLAUDE_CODE_OAUTH_TOKEN'] == '{marker}'\n{PEER}"),
    )
    .unwrap();
    let mut session_spec = spec(root.path(), &name, "complete");
    session_spec
        .env
        .insert("CLAUDE_CODE_OAUTH_TOKEN".into(), marker.into());
    let mut session = AcpBackend::new(
        "/usr/local/bin/python3",
        vec![root.path().join("workspace/peer.py").display().to_string()],
    )
    .start(session_spec)
    .await
    .unwrap();
    let mut delivered = false;
    while let Some(event) = session.next_event().await.unwrap() {
        if let AgentEvent::Result { text, is_error, .. } = event {
            assert!(!is_error);
            assert_eq!(text, "fixture-delivery");
            delivered = true;
        }
    }
    assert!(delivered);
    assert!(matches!(
        session.exit_status(),
        Some(SessionExit::Completed)
    ));
    absent(root.path(), &name).await;
}

#[tokio::test]
async fn acp_containment_v1_launch_prelude_rejects_overflow_eof_and_expired_owner() {
    if !enabled() {
        return;
    }
    for mode in ["overflow", "eof", "expired-owner"] {
        let root = fixture();
        let name = name();
        let (mut owned, mut command) = OwnedContainer::prepare(
            &spec(root.path(), &name, "complete"),
            Path::new("/usr/local/bin/python3"),
            &[root.path().join("workspace/peer.py").display().to_string()],
            None,
        )
        .await
        .unwrap();
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let prelude = if mode == "overflow" {
            u32::MAX.to_be_bytes()
        } else {
            10_u32.to_be_bytes()
        };
        stdin.write_all(&prelude).await.unwrap();
        if mode == "eof" {
            stdin.shutdown().await.unwrap();
            drop(stdin);
        } else {
            // Keep input open while waiting: a stalled prelude cannot hold the lease alive.
            if mode == "expired-owner" {
                tokio::time::sleep(Duration::from_secs(1)).await;
                owned.stop_lease();
            }
            let output = tokio::time::timeout(Duration::from_secs(12), child.wait_with_output())
                .await
                .unwrap()
                .unwrap();
            owned.remove().await.unwrap();
            assert_eq!(
                output.status.code(),
                Some(if mode == "overflow" { 125 } else { 124 }),
                "{mode}"
            );
            assert!(!root.path().join("workspace/delivered.txt").exists());
            continue;
        }
        let output = tokio::time::timeout(Duration::from_secs(12), child.wait_with_output())
            .await
            .unwrap()
            .unwrap();
        owned.remove().await.unwrap();
        assert_eq!(output.status.code(), Some(125));
        assert!(!root.path().join("workspace/delivered.txt").exists());
    }
}

fn fixture_resources() -> crate::acp_worker::Resources {
    crate::acp_worker::Resources {
        memory_mib: 256,
        cpu_millis: 1_500,
        pids: 64,
        nofile: 256,
        fsize_mib: 64,
        tmpfs_mib: 64,
        session_seconds: 3_600,
    }
}

#[test]
fn acp_resource_argv_replaces_pids_once_and_leaves_unbounded_revisions_untouched() {
    let reviewed: Vec<String> = [
        "create",
        "--rm",
        "-i",
        "--pids-limit",
        "512",
        "--network",
        "none",
        "IMAGE",
        "-I",
        "-S",
        "-u",
        "supervisor.py",
        "/kranz-owned-session",
    ]
    .map(String::from)
    .to_vec();
    let image_index = reviewed.len() - 6;
    let mut unbounded = reviewed.clone();
    apply_resources(&mut unbounded, image_index, None);
    assert_eq!(unbounded, reviewed);

    let mut bounded = reviewed.clone();
    apply_resources(&mut bounded, image_index, Some(fixture_resources()));
    assert_eq!(bounded.iter().filter(|a| *a == "--pids-limit").count(), 1);
    let pids = bounded.iter().position(|a| a == "--pids-limit").unwrap();
    assert_eq!(bounded[pids + 1], "64");
    let image = bounded.iter().position(|a| a == "IMAGE").unwrap();
    assert_eq!(bounded[image..], reviewed[image_index..]);
    for flag in [
        "--memory=256m",
        "--memory-swap=256m",
        "--cpus=1.500",
        "nofile=256:256",
        "fsize=67108864:67108864",
    ] {
        let at = bounded.iter().position(|a| a == flag).unwrap();
        assert!(at < image, "{flag} must precede the image");
    }

    // A prologue without a pids flag still gets exactly one, before the image.
    let mut without: Vec<String> = reviewed
        .iter()
        .filter(|a| *a != "--pids-limit" && *a != "512")
        .cloned()
        .collect();
    let image_index = without.len() - 6;
    apply_resources(&mut without, image_index, Some(fixture_resources()));
    let pids = without.iter().position(|a| a == "--pids-limit").unwrap();
    assert_eq!(without[pids + 1], "64");
    assert!(pids < without.iter().position(|a| a == "IMAGE").unwrap());
}

#[tokio::test]
async fn acp_containment_v1_resource_ceilings_reach_the_namespace() {
    if !enabled() {
        return;
    }
    let root = fixture();
    let name = name();
    let (mut owned, _) = OwnedContainer::prepare(
        &spec(root.path(), &name, "idle"),
        Path::new("/usr/local/bin/python3"),
        &[root.path().join("workspace/peer.py").display().to_string()],
        Some(fixture_resources()),
    )
    .await
    .unwrap();
    let inspected = owned
        .client
        .control(&["inspect".into(), owned.id.clone().unwrap()])
        .await
        .unwrap();
    owned.remove().await.unwrap();
    absent(root.path(), &name).await;
    let metadata: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    let host = &metadata[0]["HostConfig"];
    assert_eq!(host["Memory"], 256 * 1024 * 1024);
    assert_eq!(host["MemorySwap"], 256 * 1024 * 1024);
    assert_eq!(host["NanoCpus"], 1_500_000_000_u64);
    assert_eq!(host["PidsLimit"], 64);
    let ulimit = |name: &str| {
        host["Ulimits"]
            .as_array()
            .unwrap()
            .iter()
            .find(|u| u["Name"] == name)
            .map(|u| (u["Soft"].clone(), u["Hard"].clone()))
            .unwrap()
    };
    assert_eq!(
        ulimit("nofile"),
        (serde_json::json!(256), serde_json::json!(256))
    );
    assert_eq!(
        ulimit("fsize"),
        (serde_json::json!(67108864), serde_json::json!(67108864))
    );
    assert_eq!(owned.receipt()["resources"]["memoryMib"], 256);
}

#[tokio::test]
async fn acp_containment_v1_create_uses_startup_budget_and_preserves_uncertain_state() {
    use std::os::unix::fs::PermissionsExt;
    for deadline in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let wrapper = root.path().join("slow-docker");
        std::fs::write(&wrapper, "#!/bin/sh\nif [ \"$1\" = create ]; then\n touch \"$0.created\"\n sleep 6\n printf '%064d\\n' 1\nfi\n").unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut owned = OwnedContainer {
            id: None,
            alive: std::sync::Arc::new(AtomicBool::new(true)),
            client: DockerEvaluator::new(&wrapper).unwrap(),
            name: "kranz-fixture".into(),
            owner: "fixture".into(),
            image: IMAGE.into(),
            root: Some(tempfile::tempdir().unwrap()),
            launch: None,
            heartbeat: None,
            creation_started: false,
            creation_finished: false,
            removed: false,
            resources: None,
            observation_file: None,
            observation_started_at: chrono::Utc::now(),
            resource_evidence: None,
        };
        let started = std::time::Instant::now();
        let result = owned
            .create(
                &["create".into()],
                if deadline {
                    // Allow process startup under the full workspace's load,
                    // while still interrupting the fixture's six-second create.
                    Duration::from_secs(3)
                } else {
                    CREATE_TIMEOUT
                },
            )
            .await;
        assert!(root.path().join("slow-docker.created").exists());
        assert!(owned.creation_started);
        if deadline {
            assert!(result.is_err());
            assert!(!owned.creation_finished);
            assert!(started.elapsed() < Duration::from_secs(5));
            assert!(
                owned.remove().await.is_err(),
                "an empty inventory cannot prove an interrupted create is gone"
            );
            assert!(owned.root.as_ref().unwrap().path().exists());
            // No real daemon exists in this fixture; remove its synthetic recovery root.
            owned.creation_started = false;
        } else {
            result.unwrap();
            assert!(owned.creation_finished);
            assert!(started.elapsed() >= Duration::from_secs(6));
            owned.remove().await.unwrap();
        }
    }
}

mod terminals;

#[test]
fn acp_resource_observation_rejects_spoofed_namespaces_and_replaced_paths() {
    use sha2::{Digest, Sha256};
    let id = "a".repeat(64);
    let mut state = serde_json::json!([{"Id":id,"Config":{"Labels":{"com.kranz.acp-owner":"owner"}},"State":{"Running":false,"OOMKilled":true,"ExitCode":137}}]);
    assert!(inspected_state(&state, &id, "owner").unwrap().oom_killed);
    assert!(inspected_state(&state, &"b".repeat(64), "owner").is_none());
    assert!(inspected_state(&state, &id, "other").is_none());
    state[0]["State"]["OOMKilled"] = serde_json::Value::Null;
    assert!(inspected_state(&state, &id, "owner").is_none());

    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sample");
    let mut original = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(&path)
        .unwrap();
    let sample = serde_json::json!({"owner":"owner", "elapsedMs":10,"complete":true,"wallClockExpired":false,
        "baseline":{"oomKill":0,"pidsMax":0,"throttledUsec":0},"current":{"oomKill":0,"pidsMax":0,"throttledUsec":12}});
    let bytes = serde_json::to_vec(&sample).unwrap();
    original.write_all(&Sha256::digest(&bytes)).unwrap();
    original.write_all(&bytes).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, b"forged replacement").unwrap();
    assert_eq!(
        read_resource_sample(&original, "owner")
            .unwrap()
            .current
            .oom_kill,
        Some(0)
    );
    assert!(read_resource_sample(&original, "wrong-owner").is_none());
    original.set_len(4129).unwrap();
    assert!(read_resource_sample(&original, "owner").is_none());
}

async fn resource_case(
    mode: &str,
) -> (
    tempfile::TempDir,
    crate::acp_resources::ResourceEvidence,
    SessionExit,
) {
    let root = fixture();
    let name = name();
    let peer = root.path().join("workspace/resource-peer.py");
    std::fs::write(&peer, include_str!("resource_peer.py")).unwrap();
    let mut limits = fixture_resources();
    limits.session_seconds = if matches!(mode, "wall" | "startup-wall") {
        2
    } else {
        30
    };
    if mode == "cpu" {
        limits.cpu_millis = 100;
    }
    if mode == "nofile" {
        limits.nofile = 32;
    }
    if mode == "fsize" {
        limits.fsize_mib = 1;
    }
    if mode == "workload" {
        limits = crate::acp_worker::RESOURCE_CANDIDATE_LIMITS;
    }
    let backend = AcpBackend::new("/usr/local/bin/python3", vec![peer.display().to_string()])
        .with_resource_fixture(limits);
    let mut session = backend.start(spec(root.path(), &name, mode)).await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(if mode == "workload" { 300 } else { 45 }),
        async { while session.next_event().await.unwrap().is_some() {} },
    )
    .await
    .expect("resource fixture deadline");
    let exit = session.exit_status().unwrap();
    let mut evidence = session.resource_evidence().expect("resource receipt");
    evidence.classify(matches!(exit, SessionExit::Failed(_)));
    assert!(evidence.namespace_cleanup_confirmed);
    assert!(evidence.unavailable.is_empty(), "{evidence:?}");
    assert_eq!(evidence.container_id.len(), 64);
    assert_eq!(evidence.sample.as_ref().unwrap().owner, evidence.owner);
    assert!(evidence.observation_finished_at >= evidence.observation_started_at);
    assert!(!evidence.state.as_ref().unwrap().running);
    absent(root.path(), &name).await;
    (root, evidence, exit)
}

#[tokio::test]
async fn acp_containment_v1_resource_failures_bind_memory_pids_and_wall_observations() {
    if !enabled() {
        return;
    }
    use crate::acp_resources::FailureClass;
    for (mode, class, key) in [
        ("memory", FailureClass::Memory, "memoryMib"),
        ("pids", FailureClass::Pids, "pids"),
        ("wall", FailureClass::WallClock, "sessionSeconds"),
        ("startup-wall", FailureClass::WallClock, "sessionSeconds"),
    ] {
        let (root, evidence, exit) = resource_case(mode).await;
        assert!(matches!(exit, SessionExit::Failed(_)), "{mode}: {exit:?}");
        assert_eq!(evidence.failure, Some(class), "{mode}: {evidence:?}");
        assert!(evidence.failure_message().unwrap().contains(key));
        assert!(!root.path().join("workspace/delivered.txt").exists());
    }
}

#[tokio::test]
async fn acp_containment_v1_resource_exit_codes_and_forged_observations_stay_unattributed() {
    if !enabled() {
        return;
    }
    for mode in ["exit137", "forged-observation"] {
        let (_, evidence, exit) = resource_case(mode).await;
        assert!(matches!(exit, SessionExit::Failed(_)));
        assert_eq!(
            evidence.failure,
            Some(crate::acp_resources::FailureClass::Other),
            "{mode}: {evidence:?}"
        );
        assert!(!evidence.sample.as_ref().unwrap().wall_clock_expired);
        assert_eq!(evidence.sample.as_ref().unwrap().current.oom_kill, Some(0));
    }
}

#[tokio::test]
async fn acp_containment_v1_resource_throttling_and_tmpfs_caps_preserve_honest_success() {
    if !enabled() {
        return;
    }
    for mode in ["cpu", "tmpfs"] {
        let (root, evidence, exit) = resource_case(mode).await;
        assert_eq!(exit, SessionExit::Completed, "{mode}: {evidence:?}");
        assert_eq!(evidence.failure, None);
        assert_eq!(
            std::fs::read_to_string(root.path().join("workspace/delivered.txt")).unwrap(),
            "resource fixture delivery"
        );
        if mode == "cpu" {
            let sample = evidence.sample.unwrap();
            assert!(
                sample.current.throttled_usec.unwrap() > sample.baseline.throttled_usec.unwrap()
            );
        }
    }
}

#[tokio::test]
async fn acp_containment_v1_resource_descriptor_and_file_limits_are_enforced() {
    if !enabled() {
        return;
    }
    for mode in ["nofile", "fsize"] {
        let (root, evidence, exit) = resource_case(mode).await;
        assert_eq!(exit, SessionExit::Completed, "{mode}: {evidence:?}");
        assert_eq!(evidence.failure, None, "handled exhaustion is not failure");
        assert_eq!(
            std::fs::read_to_string(root.path().join("workspace/delivered.txt")).unwrap(),
            "resource fixture delivery"
        );
        let observed = &evidence.sample.unwrap().usage.unwrap().limits;
        assert_eq!(observed.nofile, Some([evidence.limits.nofile as u64; 2]));
        assert_eq!(
            observed.fsize_bytes,
            Some([evidence.limits.fsize_mib as u64 * 1024 * 1024; 2])
        );
    }
}

#[tokio::test]
async fn acp_containment_v1_resource_usage_and_kernel_limits_are_retained() {
    if !enabled() {
        return;
    }
    let (_, evidence, exit) = resource_case("usage").await;
    assert_eq!(exit, SessionExit::Completed);
    let sample = evidence.sample.unwrap();
    assert!(sample.complete);
    let usage = sample.usage.unwrap();
    assert!(usage.memory_peak_bytes.unwrap() >= 24 * 1024 * 1024);
    assert!(usage.pids_peak.unwrap() >= 5);
    assert!(usage.cpu_usage_usec.unwrap() > usage.cpu_usage_at_start_usec.unwrap());
    let limits = usage.limits;
    assert_eq!(limits.memory_max_bytes, Some(256 * 1024 * 1024));
    assert_eq!(limits.swap_max_bytes, Some(0));
    assert_eq!(limits.pids_max, Some(64));
    assert_eq!(
        limits.cpu_quota_usec.unwrap() * 1000 / limits.cpu_period_usec.unwrap(),
        1500
    );
    assert_eq!(limits.nofile, Some([256; 2]));
    assert_eq!(limits.fsize_bytes, Some([64 * 1024 * 1024; 2]));
}

#[tokio::test]
#[ignore = "operator-selected installed image, no credentials or provider call"]
async fn acp_resource_qualification_toolchain_preflight() {
    assert!(enabled(), "explicit container opt-in required");
    let (root, evidence, exit) = resource_case("toolchains").await;
    assert_eq!(exit, SessionExit::Completed);
    let bytes = std::fs::read(root.path().join("workspace/toolchains.json")).unwrap();
    let versions: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    println!(
        "{}",
        serde_json::json!({"toolchains":versions,"resourceEvidence":evidence})
    );
    for tool in ["rustc", "cargo", "node", "npm"] {
        assert_eq!(
            versions[tool]["exitCode"], 0,
            "required toolchain {tool} unavailable"
        );
    }
}

#[tokio::test]
#[ignore = "installed R3 candidate image; no credentials or provider call"]
async fn acp_resource_qualification_offline_workload() {
    assert!(enabled(), "explicit container opt-in required");
    assert_eq!(
        std::env::var("KRANZ_ACP_PROOF_IMAGE").unwrap(),
        crate::acp_worker::RESOURCE_CANDIDATE_IMAGE
    );
    let (root, evidence, exit) = resource_case("workload").await;
    assert_eq!(exit, SessionExit::Completed);
    let receipt: serde_json::Value = serde_json::from_slice(
        &std::fs::read(
            root.path()
                .join("workspace/qualification-output/receipt.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["passed"], true);
    assert_eq!(receipt["rustTests"], 2);
    assert_eq!(receipt["nodeTests"], 2);
    println!(
        "{}",
        serde_json::json!({"workload":receipt,"resourceEvidence":evidence})
    );
}

#[tokio::test]
async fn acp_containment_v1_resource_failures_survive_runner_completion_and_replay() {
    if !enabled() {
        return;
    }
    use crate::events::{Event, EventKind};
    use crate::runner::{LogTarget, RunMeta};
    use crate::types::{BackendKind, MissionConfig, Role, RunResult};
    let root = fixture();
    let name = name();
    let peer = root.path().join("workspace/resource-peer.py");
    std::fs::write(&peer, include_str!("resource_peer.py")).unwrap();
    let backend = AcpBackend::new("/usr/local/bin/python3", vec![peer.display().to_string()])
        .with_resource_fixture(crate::acp_worker::Resources {
            session_seconds: 2,
            ..fixture_resources()
        });
    let session_spec = spec(root.path(), &name, "wall");
    let paths = crate::paths::MissionPaths::new(&session_spec.cwd, "m-fixture");
    let mut log = LogTarget::Buffer(vec![EventKind::MissionCreated {
        goal: "synthetic resource failure".into(),
        base_branch: "main".into(),
        mission_branch: "kranz/resource-fixture".into(),
        config: MissionConfig::default(),
    }]);
    let outcome = crate::runner::run_session_to(
        &backend,
        session_spec,
        &mut log,
        &paths,
        RunMeta {
            run_id: "resource-run".into(),
            role: Role::Worker,
            feature_id: None,
            milestone_id: None,
            model: "synthetic".into(),
            backend: Some(BackendKind::Acp),
            prompt_hash: "fixture".into(),
            executor_route: None,
        },
        None,
    )
    .await
    .unwrap();
    assert_eq!(outcome.result, RunResult::Fail);
    let LogTarget::Buffer(kinds) = log else {
        unreachable!()
    };
    assert!(kinds.iter().any(|kind| matches!(kind, EventKind::WorkerMessage { content, .. } if content.contains("resources.sessionSeconds"))));
    let events: Vec<Event> = kinds
        .into_iter()
        .enumerate()
        .map(|(i, kind)| Event {
            seq: i as u64 + 1,
            ts: chrono::Utc::now(),
            mission_id: "m-fixture".into(),
            kind,
        })
        .collect();
    let retained: Vec<Event> =
        serde_json::from_slice(&serde_json::to_vec(&events).unwrap()).unwrap();
    let state = crate::reducer::fold(&retained).unwrap();
    let run = &state.runs["resource-run"];
    assert_eq!(run.result, Some(RunResult::Fail));
    let evidence = run.resource_evidence.as_ref().unwrap();
    assert_eq!(
        evidence.failure,
        Some(crate::acp_resources::FailureClass::WallClock)
    );
    assert!(evidence.namespace_cleanup_confirmed);
    assert!(evidence.sample.as_ref().unwrap().wall_clock_expired);
    assert!(!root.path().join("workspace/delivered.txt").exists());
    absent(root.path(), &name).await;
}

#[tokio::test]
async fn acp_containment_v1_resource_abort_drop_and_owner_death_keep_cleanup_honest() {
    if !enabled() {
        return;
    }
    for dropped in [false, true] {
        let root = fixture();
        let name = name();
        let mut session = AcpBackend::new(
            "/usr/local/bin/python3",
            vec![root.path().join("workspace/peer.py").display().to_string()],
        )
        .with_resource_fixture(fixture_resources())
        .start(spec(root.path(), &name, "idle"))
        .await
        .unwrap();
        ready(root.path()).await;
        if dropped {
            drop(session);
        } else {
            session.abort().await.unwrap();
            assert_eq!(session.exit_status(), Some(SessionExit::Aborted));
            assert!(
                session
                    .resource_evidence()
                    .unwrap()
                    .namespace_cleanup_confirmed
            );
        }
        absent(root.path(), &name).await;
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
        .env("KRANZ_ACP_OWNER_MODE", "resources")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut owner_process = command.spawn().unwrap();
    ready(root.path()).await;
    owner_process.kill().await.unwrap();
    let client = DockerEvaluator::new(
        &trusted_docker(&spec(root.path(), &name, "idle").sandbox.unwrap().inputs).unwrap(),
    )
    .unwrap();
    let stopped = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let inspected = client
                .control(&["inspect".into(), name.clone()])
                .await
                .unwrap();
            assert_eq!(
                inspected.code,
                Some(0),
                "resource namespace must remain inspectable after owner death"
            );
            let metadata: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
            if metadata[0]["State"]["Running"] == false {
                break metadata;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("owner death did not expire the guest lease");
    let heartbeat = std::fs::read(root.path().join("workspace/child-heartbeat")).unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        heartbeat,
        std::fs::read(root.path().join("workspace/child-heartbeat")).unwrap()
    );
    let owner = stopped[0]["Config"]["Labels"]["com.kranz.acp-owner"]
        .as_str()
        .unwrap();
    let ledger = stopped[0]["Mounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["Destination"] == GUEST_CONTROL)
        .unwrap()["Source"]
        .as_str()
        .unwrap();
    assert!(Path::new(ledger).join("container.json").is_file());
    assert!(!Path::new(ledger).join("launch.json").exists());
    remove(&client, owner, true, true).await.unwrap();
    absent(root.path(), &name).await;
    std::fs::remove_dir_all(ledger).unwrap();
}
