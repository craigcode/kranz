use super::*;

async fn terminal_session(root: &Path, name: &str, mode: &str) -> Box<dyn AgentSession> {
    std::fs::write(
        root.join("workspace/terminal_peer.py"),
        include_str!("../terminal_peer.py"),
    )
    .unwrap();
    let backend = AcpBackend::new(
        "/usr/local/bin/python3",
        vec![root
            .join("workspace/terminal_peer.py")
            .display()
            .to_string()],
    );
    let backend = if mode == "disabled" {
        backend
    } else {
        backend.with_terminal_fixture("fixture-run")
    };
    backend.start(spec(root, name, mode)).await.unwrap()
}

#[tokio::test]
async fn acp_terminal_v1_consent_concurrency_bounds_and_descendants() {
    if !enabled() {
        return;
    }
    let root = fixture();
    let name = name();
    let mut session = terminal_session(root.path(), &name, "lifecycle").await;
    let responder = session.permission_responder().unwrap();
    let mut held = None;
    let mut approvals = 0;
    let mut saw_report = false;
    let mut cleanup = false;
    let mut receipts = Vec::new();
    tokio::time::timeout(Duration::from_secs(40), async {
        while let Some(event) = session.next_event().await.unwrap() {
            match event {
                AgentEvent::PermissionRequested { proposal, .. } => {
                    assert!(proposal.action["terminalScope"]["runId"] == "fixture-run");
                    match proposal.peer_request_id.as_u64().unwrap() {
                        100 | 110 => {
                            responder.respond(&proposal, true).unwrap();
                            approvals += 1;
                        }
                        102 => held = Some(proposal),
                        _ => panic!("unexpected consent"),
                    }
                }
                AgentEvent::Other { raw } => {
                    if raw["terminalReceipt"]["requestId"] == 104 {
                        responder
                            .respond(&held.take().expect("concurrent human request"), false)
                            .unwrap();
                    }
                    if raw["terminalReceipt"].is_object() {
                        receipts.push(raw["terminalReceipt"].clone());
                    }
                    if raw["terminalSessionCleanup"]["confirmed"] == true {
                        cleanup = true;
                    }
                }
                AgentEvent::Result { text, is_error, .. } => {
                    assert!(!is_error);
                    assert_eq!(text, "terminal-proof-ok");
                    saw_report = true;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(saw_report && cleanup);
    assert_eq!(approvals, 2);
    assert!(
        matches!(session.exit_status(), Some(SessionExit::Completed)),
        "{:?}",
        session.exit_status()
    );
    assert!(receipts
        .iter()
        .any(|r| r["method"] == "terminal/kill"
            && r["evidence"]["cleanup"]["descendantsReaped"] == true));
    assert!(receipts
        .iter()
        .any(|r| r["requestId"] == 107 && r["succeeded"] == false));
    assert!(!root.path().join("workspace/unapproved").exists());
    absent(root.path(), &name).await;
}

#[tokio::test]
async fn acp_terminal_v1_paths_and_environment_fail_before_consent() {
    if !enabled() {
        return;
    }
    let root = fixture();
    let name = name();
    std::os::unix::fs::symlink("/", root.path().join("workspace/escape")).unwrap();
    let mut session = terminal_session(root.path(), &name, "paths").await;
    tokio::time::timeout(Duration::from_secs(30), async {
        while let Some(event) = session.next_event().await.unwrap() {
            assert!(!matches!(event, AgentEvent::PermissionRequested { .. }));
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(session.exit_status(), Some(SessionExit::Completed)),
        "{:?}",
        session.exit_status()
    );
    absent(root.path(), &name).await;
}

#[tokio::test]
async fn acp_terminal_v1_peer_provider_death_and_drop_stop_namespace() {
    if !enabled() {
        return;
    }
    for mode in ["provider-death", "peer-death", "drop"] {
        let root = fixture();
        let name = name();
        let mut session = terminal_session(root.path(), &name, mode).await;
        let responder = session.permission_responder().unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match session.next_event().await {
                    Ok(Some(AgentEvent::PermissionRequested { proposal, .. })) => {
                        responder.respond(&proposal, true).unwrap()
                    }
                    Ok(Some(AgentEvent::Other { raw }))
                        if mode == "drop"
                            && raw["terminalReceipt"]["evidence"]["started"] == true =>
                    {
                        break
                    }
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => {
                        assert_ne!(mode, "drop");
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
        if mode == "drop" {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !root.path().join("workspace/command-ready").exists() {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
        if mode != "drop" {
            assert!(!matches!(
                session.exit_status(),
                Some(SessionExit::Completed)
            ));
        }
        drop(session);
        absent(root.path(), &name).await;
        let beat = std::fs::read(root.path().join("workspace/heartbeat")).ok();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            beat,
            std::fs::read(root.path().join("workspace/heartbeat")).ok()
        );
    }
}

#[tokio::test]
async fn acp_terminal_v1_pending_consent_and_late_create_cannot_complete() {
    if !enabled() {
        return;
    }
    for mode in ["drop", "late-create"] {
        let root = fixture();
        let name = name();
        let mut session = terminal_session(root.path(), &name, mode).await;
        let responder = session.permission_responder().unwrap();
        let proposal = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(AgentEvent::PermissionRequested { proposal, .. }) =
                    session.next_event().await.unwrap()
                {
                    break proposal;
                }
            }
        })
        .await
        .unwrap();
        if mode == "drop" {
            session.abort().await.unwrap();
            let _ = responder.respond(&proposal, true);
            assert!(matches!(session.exit_status(), Some(SessionExit::Aborted)));
        } else {
            responder.respond(&proposal, true).unwrap();
            tokio::time::timeout(Duration::from_secs(10), async {
                while session.next_event().await.unwrap().is_some() {}
            })
            .await
            .unwrap();
            assert!(matches!(
                session.exit_status(),
                Some(SessionExit::Failed(_))
            ));
        }
        absent(root.path(), &name).await;
        assert!(!root.path().join("workspace/command-ready").exists());
    }
}

#[tokio::test]
async fn acp_terminal_v1_engine_death_expires_terminal_namespace() {
    if !enabled() {
        return;
    }
    let root = fixture();
    let name = name();
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "acp_container::tests::terminals::terminal_owner_process",
            "--exact",
            "--nocapture",
        ])
        .env_clear()
        .envs(ContainerRuntime::Docker.client_env())
        .env(
            crate::backend_claude::SCRATCH_ROOT_ENV,
            crate::backend_claude::scratch_root_base(),
        )
        .env("KRANZ_TERMINAL_OWNER_ROOT", root.path())
        .env("KRANZ_TERMINAL_OWNER_NAME", &name)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    let mut owner = command.spawn().unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !root.path().join("workspace/command-ready").exists() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    owner.kill().await.unwrap();
    absent(root.path(), &name).await;
    let heartbeat = std::fs::read(root.path().join("workspace/heartbeat")).unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        heartbeat,
        std::fs::read(root.path().join("workspace/heartbeat")).unwrap()
    );
}

#[test]
fn terminal_owner_process() {
    let Some(root) = std::env::var_os("KRANZ_TERMINAL_OWNER_ROOT") else {
        return;
    };
    let name = std::env::var("KRANZ_TERMINAL_OWNER_NAME").unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut session = terminal_session(Path::new(&root), &name, "drop").await;
            let responder = session.permission_responder().unwrap();
            loop {
                if let Some(AgentEvent::PermissionRequested { proposal, .. }) =
                    session.next_event().await.unwrap()
                {
                    responder.respond(&proposal, true).unwrap();
                }
            }
        });
}

#[tokio::test]
async fn acp_terminal_v1_default_capability_refuses_execution() {
    if !enabled() {
        return;
    }
    let root = fixture();
    let name = name();
    let mut session = terminal_session(root.path(), &name, "disabled").await;
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Some(event) = session.next_event().await.unwrap() {
            assert!(!matches!(event, AgentEvent::PermissionRequested { .. }));
        }
    })
    .await
    .unwrap();
    assert!(
        matches!(session.exit_status(), Some(SessionExit::Completed)),
        "{:?}",
        session.exit_status()
    );
    assert!(!root.path().join("workspace/unapproved").exists());
    absent(root.path(), &name).await;
}
