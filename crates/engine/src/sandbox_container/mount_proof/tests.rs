use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::{atomic::Ordering, Arc};

const IMAGE: &str =
    "python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d";
const ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn fake(script: &str) -> (tempfile::TempDir, Helper) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("docker");
    std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut helper = Helper::new(DockerEvaluator::new(&path).unwrap(), root.path()).unwrap();
    helper.creation_started = true;
    helper.creation_settled = true;
    helper.id = Some(ID.into());
    (root, helper)
}

#[tokio::test]
async fn mount_helper_v1_inventory_errors_and_uncertain_creation_retain_evidence() {
    for (script, settled, expected) in [
        ("exit 1", true, "inventory failed"),
        ("exit 0", false, "late daemon creation"),
        ("echo short-id", true, "ambiguous"),
    ] {
        let (_root, mut helper) = fake(script);
        helper.creation_settled = settled;
        let probe = helper.probe.as_ref().unwrap().path().to_path_buf();
        assert!(helper.cleanup().await.unwrap_err().contains(expected));
        let ledger = helper.retain();
        drop(helper);
        assert!(ledger.exists());
        assert!(probe.exists());
        std::fs::remove_dir_all(probe).unwrap();
        std::fs::remove_dir_all(ledger).unwrap();
    }
}

#[tokio::test]
async fn mount_helper_v1_deletion_acknowledgement_is_not_absence() {
    let (_root, mut helper) = fake(&format!("[ \"$1\" = rm ] && exit 0\necho {ID}"));
    assert!(helper.cleanup().await.unwrap_err().contains("deadline"));
    helper.creation_started = false;
}

#[tokio::test]
async fn mount_helper_v1_deletes_only_the_created_id_and_waits_for_absence() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("removed");
    let script = format!(
        "if [ \"$1\" = rm ]; then\n [ \"$2\" = --force ] && [ \"$3\" = {ID} ] || exit 9\n touch '{}'\n exit 1\nfi\n[ -e '{}' ] || echo {ID}",
        marker.display(), marker.display()
    );
    let (_fake, mut helper) = fake(&script);
    helper.cleanup().await.unwrap();
    assert!(marker.exists());
    helper.creation_started = false;
    let (_fake, mut helper) = fake(&format!("echo {}", "b".repeat(64)));
    assert!(helper.cleanup().await.unwrap_err().contains("differs"));
    helper.creation_started = false;
}

#[test]
fn mount_helper_v1_unsupported_runtime_refuses_before_creating_probe() {
    let root = tempfile::tempdir().unwrap();
    for runtime in [
        ContainerRuntime::Podman,
        ContainerRuntime::Nerdctl,
        ContainerRuntime::AppleContainer,
    ] {
        assert!(
            matches!(super::super::prove_bind_mount(runtime, root.path(), IMAGE), MountProof::Failed(why) if why.contains("refused before spawn"))
        );
    }
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
}

fn enabled() -> bool {
    if std::env::var("KRANZ_MOUNT_CONTAINER_TESTS").as_deref() != Ok("1") {
        eprintln!("SKIP-MOUNT-HELPER: set KRANZ_MOUNT_CONTAINER_TESTS=1 for real Docker proofs");
        return false;
    }
    true
}

fn fixture() -> (tempfile::TempDir, Helper) {
    let root = tempfile::tempdir_in(crate::backend_claude::scratch_root_base()).unwrap();
    let helper = Helper::new(
        DockerEvaluator::new(&docker_path(root.path()).unwrap()).unwrap(),
        root.path(),
    )
    .unwrap();
    (root, helper)
}

fn block_write(helper: &Helper) {
    let fifo = helper.probe.as_ref().unwrap().path().join("guest.txt");
    let status = std::process::Command::new("mkfifo")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .arg(fifo)
        .status()
        .unwrap();
    assert!(status.success());
}

async fn absent(client: &DockerEvaluator, owner: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if owned_ids(client, owner).await.unwrap().is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("owned helper must be absent");
}

async fn running(client: &DockerEvaluator, owner: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let out = client
                .control(&[
                    "container".into(),
                    "ls".into(),
                    "--filter".into(),
                    format!("label={OWNER_LABEL}={owner}"),
                    "--format".into(),
                    "{{.ID}}".into(),
                ])
                .await
                .unwrap();
            assert_eq!(out.code, Some(0));
            if !out.stdout.is_empty() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    })
    .await
    .expect("helper must start before interruption");
}

#[tokio::test]
async fn mount_helper_v1_real_round_trip_and_concurrent_helpers() {
    if !enabled() {
        return;
    }
    let mut jobs = Vec::new();
    for _ in 0..4 {
        jobs.push(tokio::spawn(async {
            let (_root, mut helper) = fixture();
            assert_eq!(
                helper.prove(IMAGE, WALL, 85, &AtomicBool::new(false)).await,
                MountProof::Proven
            );
            absent(&helper.client, &helper.owner).await;
        }));
    }
    for job in jobs {
        job.await.unwrap();
    }
    let root = tempfile::tempdir_in(crate::backend_claude::scratch_root_base()).unwrap();
    let missing = root.path().join("not-yet-created/mission-parent");
    assert!(!missing.exists());
    assert_eq!(
        super::super::prove_bind_mount(ContainerRuntime::Docker, &missing, IMAGE),
        MountProof::Proven
    );
    assert!(missing.is_dir());
    assert_eq!(std::fs::read_dir(missing).unwrap().count(), 0);
}

#[tokio::test]
async fn mount_helper_v1_real_timeout_and_guest_watchdog_remove_blocked_writer() {
    if !enabled() {
        return;
    }
    for (wall, guest, reason) in [(10, 85, "timed out"), (20, 2, "124")] {
        let (_root, mut helper) = fixture();
        block_write(&helper);
        let started = std::time::Instant::now();
        let proof = helper
            .prove(
                IMAGE,
                Duration::from_secs(wall),
                guest,
                &AtomicBool::new(false),
            )
            .await;
        assert!(
            matches!(proof, MountProof::Failed(ref why) if why.contains(reason) && why.contains("absence confirmed")),
            "{proof:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(wall + 20));
        absent(&helper.client, &helper.owner).await;
    }
}

#[tokio::test]
async fn mount_helper_v1_real_cancellation_and_dropped_future_cleanup() {
    if !enabled() {
        return;
    }
    for drop_future in [false, true] {
        let (_root, mut helper) = fixture();
        block_write(&helper);
        let client = helper.client.clone();
        let owner = helper.owner.clone();
        let ledger = helper.ledger.as_ref().unwrap().path().to_path_buf();
        let probe = helper.probe.as_ref().unwrap().path().to_path_buf();
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancelled.clone();
        let job = tokio::spawn(async move { helper.prove(IMAGE, WALL, 85, &worker_cancel).await });
        running(&client, &owner).await;
        if drop_future {
            job.abort();
            assert!(job.await.unwrap_err().is_cancelled());
        } else {
            cancelled.store(true, Ordering::Release);
            let proof = job.await.unwrap();
            assert!(
                matches!(proof, MountProof::Failed(why) if why.contains("cancelled") && why.contains("absence confirmed"))
            );
        }
        absent(&client, &owner).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            while ledger.exists() || probe.exists() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn mount_helper_v1_real_interrupted_creation_removes_the_daemon_object() {
    if !enabled() {
        return;
    }
    let (root, mut helper) = fixture();
    let docker = docker_path(root.path()).unwrap();
    let wrapper = root.path().join("docker-control");
    let receipt = root.path().join("created-id");
    // Let the daemon create the object, but interrupt its host client before
    // it can return the creation receipt. Recovery has only the owner label.
    std::fs::write(&wrapper, format!(
        "#!/bin/sh\nif [ \"$1\" = create ]; then\n '{}' \"$@\" > '{}' || exit $?\n sleep 60\nelse\n exec '{}' \"$@\"\nfi\n",
        docker.display(), receipt.display(), docker.display()
    )).unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    helper.client = DockerEvaluator::new(&wrapper).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let cancel = cancelled.clone();
    let created_receipt = receipt.clone();
    let cancellation = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if std::fs::read_to_string(&created_receipt)
                    .ok()
                    .is_some_and(|id| full_id(id.trim()))
                {
                    cancel.store(true, Ordering::Release);
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
    });
    let proof = helper.prove(IMAGE, WALL, 85, &cancelled).await;
    cancellation.await.unwrap();
    assert!(
        matches!(proof, MountProof::Failed(ref why) if why.contains("cancelled") && why.contains("absence confirmed")),
        "{proof:?}"
    );
    assert!(!helper.creation_settled);
    let created_id = std::fs::read_to_string(receipt).unwrap();
    assert!(full_id(created_id.trim()));
    absent(&helper.client, &helper.owner).await;
}

#[tokio::test]
async fn mount_helper_v1_real_name_collision_preserves_unrelated_container() {
    if !enabled() {
        return;
    }
    let (_root, mut helper) = fixture();
    let created = helper
        .client
        .control(&[
            "create".into(),
            "--name".into(),
            helper.name.clone(),
            IMAGE.into(),
            "/bin/true".into(),
        ])
        .await
        .unwrap();
    assert_eq!(created.code, Some(0));
    let unrelated = String::from_utf8(created.stdout)
        .unwrap()
        .trim()
        .to_string();
    assert!(full_id(&unrelated));
    let proof = helper.prove(IMAGE, WALL, 85, &AtomicBool::new(false)).await;
    let inspected = helper
        .client
        .control(&["inspect".into(), unrelated.clone()])
        .await
        .unwrap();
    // Always remove only the fixture we created, even if an assertion fails.
    let removed = helper
        .client
        .control(&["rm".into(), "--force".into(), unrelated])
        .await
        .unwrap();
    assert_eq!(inspected.code, Some(0));
    assert_eq!(removed.code, Some(0));
    assert!(
        matches!(proof, MountProof::Failed(ref why) if why.contains("creation failed") && why.contains("UNCONFIRMED")),
        "{proof:?}"
    );
    // This test knows create returned a name collision; production keeps the
    // recovery intent conservatively rather than parsing daemon error prose.
    let MountProof::Failed(why) = proof else {
        unreachable!()
    };
    let ledger = why.split("retained recovery ledger: ").nth(1).unwrap();
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(Path::new(ledger).join("owner.json")).unwrap())
            .unwrap();
    std::fs::remove_dir_all(record["probe"].as_str().unwrap()).unwrap();
    std::fs::remove_dir_all(ledger).unwrap();

    let (_root, mut helper) = fixture();
    assert_eq!(
        helper.prove(IMAGE, WALL, 85, &AtomicBool::new(false)).await,
        MountProof::Proven
    );
    let old_id = helper.id.clone().unwrap();
    let replacement = helper
        .client
        .control(&[
            "create".into(),
            "--name".into(),
            helper.name.clone(),
            IMAGE.into(),
            "/bin/true".into(),
        ])
        .await
        .unwrap();
    assert_eq!(replacement.code, Some(0));
    let replacement = String::from_utf8(replacement.stdout)
        .unwrap()
        .trim()
        .to_string();
    assert_ne!(replacement, old_id);
    helper.creation_started = true;
    helper.cleanup().await.unwrap();
    helper.creation_started = false;
    let inspected = helper
        .client
        .control(&["inspect".into(), replacement.clone()])
        .await
        .unwrap();
    let removed = helper
        .client
        .control(&["rm".into(), "--force".into(), replacement])
        .await
        .unwrap();
    assert_eq!(
        inspected.code,
        Some(0),
        "same-name replacement must survive old-owner cleanup"
    );
    assert_eq!(removed.code, Some(0));
}

#[tokio::test]
async fn mount_helper_v1_real_unavailable_inventory_fails_with_private_recovery() {
    if !enabled() {
        return;
    }
    let (root, mut helper) = fixture();
    let client = helper.client.clone();
    let owner = helper.owner.clone();
    let docker = docker_path(root.path()).unwrap();
    let wrapper = root.path().join("docker-control");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n[ \"$1\" = container ] && exit 1\nexec '{}' \"$@\"\n",
            docker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    helper.client = DockerEvaluator::new(&wrapper).unwrap();
    let proof = helper.prove(IMAGE, WALL, 85, &AtomicBool::new(false)).await;
    assert!(
        matches!(proof, MountProof::Failed(ref why) if why.contains("sentinel round trip succeeded") && why.contains("cleanup UNCONFIRMED")),
        "{proof:?}"
    );
    let MountProof::Failed(why) = proof else {
        unreachable!()
    };
    let ledger = Path::new(why.split("retained recovery ledger: ").nth(1).unwrap());
    let record_path = ledger.join("owner.json");
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(record["owner"], owner);
    assert_eq!(record["cleanupConfirmed"], false);
    assert_eq!(
        std::fs::metadata(&record_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    // An independent successful query is required; a healthy daemon must not
    // let the deliberately broken production query become a passing proof.
    absent(&client, &owner).await;
    std::fs::remove_dir_all(record["probe"].as_str().unwrap()).unwrap();
    std::fs::remove_dir_all(ledger).unwrap();
}

#[tokio::test]
async fn mount_helper_v1_real_engine_death_bounds_guest_and_retains_intent() {
    if !enabled() {
        return;
    }
    const CHILD: &str = "KRANZ_MOUNT_DEATH_CHILD";
    if let Ok(record_path) = std::env::var(CHILD) {
        let (_root, mut helper) = fixture();
        block_write(&helper);
        std::fs::write(
            record_path,
            serde_json::to_vec(&serde_json::json!({
                "owner":helper.owner, "ledger":helper.ledger.as_ref().unwrap().path(),
                "probe":helper.probe.as_ref().unwrap().path(), "root":helper.root,
            }))
            .unwrap(),
        )
        .unwrap();
        let _ = helper.prove(IMAGE, WALL, 8, &AtomicBool::new(false)).await;
        panic!("parent must kill the engine fixture");
    }
    let root = tempfile::tempdir().unwrap();
    let record_path = root.path().join("child.json");
    let client = DockerEvaluator::new(&docker_path(root.path()).unwrap()).unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .env_clear().envs(ContainerRuntime::Docker.client_env())
        .env("KRANZ_MOUNT_CONTAINER_TESTS", "1").env(CHILD, &record_path)
        .env(crate::backend_claude::SCRATCH_ROOT_ENV, crate::backend_claude::scratch_root_base())
        .args(["--exact", "sandbox_container::mount_proof::tests::mount_helper_v1_real_engine_death_bounds_guest_and_retains_intent", "--nocapture"])
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().unwrap();
    let record: serde_json::Value = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(bytes) = std::fs::read(&record_path) {
                if let Ok(record) = serde_json::from_slice(&bytes) {
                    return record;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let owner = record["owner"].as_str().unwrap();
    running(&client, owner).await;
    child.kill().unwrap();
    child.wait().unwrap();
    absent(&client, owner).await;
    let ledger = Path::new(record["ledger"].as_str().unwrap());
    let intent: serde_json::Value =
        serde_json::from_slice(&std::fs::read(ledger.join("owner.json")).unwrap()).unwrap();
    assert_eq!(intent["cleanupConfirmed"], false);
    assert_eq!(intent["owner"], owner);
    std::fs::remove_dir_all(ledger).unwrap();
    std::fs::remove_dir_all(record["root"].as_str().unwrap()).unwrap();
}

#[tokio::test]
async fn mount_helper_v1_missing_configured_image_never_pulls() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("pulled");
    let (_fake, mut helper) = fake(&format!(
        "[ \"$1\" = pull ] && touch '{}'\nexit 1",
        marker.display()
    ));
    helper.creation_started = false;
    let result = helper
        .round_trip(
            IMAGE,
            Duration::from_secs(2),
            85,
            &AtomicBool::new(false),
            "/host",
            "/guest",
        )
        .await;
    assert!(result.unwrap_err().contains("must already be installed"));
    assert!(!marker.exists());
}
