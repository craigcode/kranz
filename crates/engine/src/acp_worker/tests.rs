use super::*;
#[cfg(unix)]
use crate::backend::{PromptMode, SessionSpec};
use crate::types::{BackendKind, MissionConfig, SandboxConfig};

fn profile(id: &str) -> AcpWorkerProfile {
    AcpWorkerProfile {
        id: id.into(),
        credential_file: std::env::temp_dir().join("operator-auth.json"),
    }
}
fn config(profile: AcpWorkerProfile) -> MissionConfig {
    let definition = profile.definition().unwrap();
    let mut cfg = MissionConfig {
        allow_below_default_worker_model: true,
        ..Default::default()
    };
    cfg.worker.backend = Some("acp".into());
    cfg.worker.acp_profile = Some(profile);
    cfg.worker.sandbox = SandboxConfig {
        provider: SandboxProvider::Container,
        enforce: SandboxEnforce::FsNet,
        image: Some(definition.image.into()),
        extra_write: vec![],
        egress: definition.egress.iter().map(|s| (*s).into()).collect(),
    };
    cfg
}

#[test]
fn acp_profile_configuration_is_additive_and_qualification_is_explicit() {
    let legacy = serde_json::to_value(MissionConfig::default()).unwrap();
    assert!(legacy["worker"].get("acpProfile").is_none());
    let restored: MissionConfig = serde_json::from_value(legacy).unwrap();
    assert!(restored.worker.acp_profile.is_none());
    assert!(!BackendKind::Acp.supports_sandbox_enforcement());
    for id in [CLAUDE, CODEX] {
        let p = profile(id);
        for os in ["macos", "linux", "windows"] {
            for arch in ["aarch64", "x86_64"] {
                assert_eq!(
                    p.validate_target(os, arch).is_ok(),
                    os != "windows" && arch == "aarch64"
                );
            }
        }
        let cfg = config(p);
        assert_eq!(
            crate::config::validate(&cfg).is_ok(),
            cfg!(all(
                any(target_os = "macos", target_os = "linux"),
                target_arch = "aarch64"
            ))
        );
        let roundtrip: MissionConfig =
            serde_json::from_slice(&serde_json::to_vec(&cfg).unwrap()).unwrap();
        assert_eq!(roundtrip.worker.acp_profile, cfg.worker.acp_profile);
    }
    assert!(profile("unreviewed")
        .validate_target("macos", "aarch64")
        .is_err());
    assert!(serde_json::from_value::<AcpWorkerProfile>(
        json!({"id":CODEX,"credentialFile":"/auth","args":[]})
    )
    .is_err());
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn acp_profile_configuration_refuses_boundary_and_authority_drift() {
    let good = config(profile("fixture-acp-worker-v1"));
    crate::config::validate(&good).unwrap();
    for change in 0..10 {
        let mut cfg = good.clone();
        match change {
            0 => cfg.worker.acp_command = Some("/different".into()),
            1 => cfg.worker.acp_args.push("--different".into()),
            2 => cfg.worker.sandbox.enforce = SandboxEnforce::Off,
            3 => cfg.worker.sandbox.provider = SandboxProvider::Process,
            4 => cfg.worker.sandbox.image = Some("mutable:latest".into()),
            5 => cfg.worker.sandbox.extra_write.push("/tmp".into()),
            6 => cfg.worker.sandbox.egress.push("extra.invalid:443".into()),
            7 => cfg.worker.backend = Some("claude".into()),
            8 => cfg.allow_below_default_worker_model = false,
            9 => cfg.worker.acp_profile.as_mut().unwrap().credential_file = "relative".into(),
            _ => unreachable!(),
        }
        assert!(crate::config::validate(&cfg).is_err(), "mutation {change}");
    }
    let p = good.worker.acp_profile.as_ref().unwrap();
    for role in [
        Role::Orchestrator,
        Role::ValidatorScrutiny,
        Role::ValidatorFunctional,
    ] {
        assert!(p
            .validate_config(role, &good.worker, WorkerIsolation::Worktree)
            .is_err());
    }
    let mut pool = good.clone();
    pool.worker_candidates = ["sonnet", "opus"]
        .map(|model| crate::types::CandidateSpec {
            backend: "claude".into(),
            model: model.into(),
        })
        .to_vec();
    assert!(crate::config::validate(&pool)
        .unwrap_err()
        .to_string()
        .contains("qualified ACP profiles"));
    let mut cfg = good;
    cfg.worker_isolation = WorkerIsolation::Checkout;
    assert!(crate::config::validate(&cfg).is_err());
}

#[cfg(unix)]
fn git(root: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
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
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}

#[cfg(unix)]
struct Fixture {
    _dir: tempfile::TempDir,
    primary: PathBuf,
    workspace: PathBuf,
    profile: AcpWorkerProfile,
}
#[cfg(unix)]
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in(crate::backend_claude::scratch_root_base()).unwrap();
        let primary = dir.path().join("primary");
        let workspace = dir.path().join("workspace");
        std::fs::create_dir(&primary).unwrap();
        git(&primary, &["init", "-q", "-b", "main"]);
        git(&primary, &["config", "user.name", "Fixture"]);
        git(
            &primary,
            &["config", "user.email", "fixture@example.invalid"],
        );
        std::fs::write(primary.join("source.txt"), "base\n").unwrap();
        git(&primary, &["add", "source.txt"]);
        git(&primary, &["commit", "-qm", "base"]);
        git(
            &primary,
            &[
                "worktree",
                "add",
                "-qb",
                "kranz/fixture",
                workspace.to_str().unwrap(),
            ],
        );
        std::fs::create_dir_all(primary.join(".kranz/missions/m-profile")).unwrap();
        let auth = dir.path().join("operator-auth.json");
        private_write(
            &auth,
            &serde_json::to_vec(&json!({"tokens":{"access_token":format!("synthetic-{}","login-no-authority-12345")}})).unwrap(),
        )
        .unwrap();
        Self {
            _dir: dir,
            primary,
            workspace,
            profile: AcpWorkerProfile {
                id: "fixture-acp-worker-v1".into(),
                credential_file: auth,
            },
        }
    }
    fn spec(&self) -> SessionSpec {
        SessionSpec {
            cwd: self.workspace.clone(),
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
            env: Default::default(),
            hook_status: None,
            sandbox: Some(crate::sandbox::ResolvedSandbox {
                backend: crate::sandbox::SandboxBackend::Container,
                inputs: crate::sandbox::SandboxInputs {
                    enforce: SandboxEnforce::FsNet,
                    session_cwd: self.workspace.clone(),
                    mission_dir: self.primary.join(".kranz/missions/m-profile"),
                    tmpdir: self._dir.path().join("old-home"),
                    extra_write: vec![],
                    egress: vec!["provider.invalid:443".into()],
                    validator_read_deny_roots: vec![],
                },
                container: Some(crate::sandbox_container::ContainerSpec {
                    runtime: crate::sandbox_container::ContainerRuntime::Docker,
                    image: self.profile.definition().unwrap().image.into(),
                    network: None,
                    name: None,
                }),
            }),
        }
    }
}

#[test]
#[cfg(unix)]
fn acp_profile_private_sources_reject_links_permissions_and_unbounded_data() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("auth");
    private_write(&file, b"{}").unwrap();
    assert_eq!(private_credential(&file).unwrap(), b"{}");
    let link = dir.path().join("link");
    symlink(&file, &link).unwrap();
    assert!(private_credential(&link).is_err());
    std::fs::hard_link(&file, dir.path().join("hard")).unwrap();
    assert!(private_credential(&file).is_err());
    std::fs::remove_file(dir.path().join("hard")).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(private_credential(&file).is_err());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::write(&file, vec![b'x'; CREDENTIAL_LIMIT as usize + 1]).unwrap();
    assert!(private_credential(&file).is_err());
    assert!(private_credential(dir.path()).is_err());
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn acp_profile_preparation_limits_seeding_and_removes_private_state() {
    let f = Fixture::new();
    let original = std::fs::read(&f.profile.credential_file).unwrap();
    let mut spec = f.spec();
    let mut prepared = f.profile.prepare(&mut spec).unwrap();
    let home = spec.sandbox.as_ref().unwrap().inputs.tmpdir.clone();
    assert_eq!(
        std::fs::read(home.join(".codex/auth.json")).unwrap(),
        original
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".codex/config.toml")).unwrap(),
        CODEX_CONFIG
    );
    assert_eq!(spec.env["INITIAL_AGENT_MODE"], "read-only");
    assert_eq!(spec.env["NO_BROWSER"], "1");
    assert!(!spec.env.contains_key("HOME"));
    assert_eq!(std::fs::read_dir(&home).unwrap().count(), 1);
    assert!(prepared.contains_secret(r#"{"text":"synthetic-login-no-authority-12345"}"#));
    assert!(prepared.contains_secret(r#"{"text":"\u0073ynthetic-login-no-authority-12345"}"#));
    assert_eq!(
        prepared.scrub("error: synthetic-login-no-authority-12345".into()),
        "error: [REDACTED]"
    );
    prepared.close().unwrap();
    assert!(!home.exists());
    assert_eq!(std::fs::read(&f.profile.credential_file).unwrap(), original);
    let mut spec = f.spec();
    let prepared = f.profile.prepare(&mut spec).unwrap();
    let dropped_home = spec.sandbox.unwrap().inputs.tmpdir;
    drop(prepared);
    assert!(!dropped_home.exists());
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn acp_profile_runtime_refuses_drift_before_reading_credentials() {
    let f = Fixture::new();
    let mut missing = f.profile.clone();
    missing.credential_file = f._dir.path().join("absent");
    for change in 0..7 {
        let mut spec = f.spec();
        match change {
            0 => spec
                .env
                .insert("NODE_OPTIONS".into(), "--require hostile".into()),
            1 => {
                spec.sandbox.as_mut().unwrap().inputs.egress.clear();
                None
            }
            2 => {
                spec.sandbox.as_mut().unwrap().inputs.enforce = SandboxEnforce::Off;
                None
            }
            3 => {
                spec.writable = false;
                None
            }
            4 => {
                spec.resume = Some("old".into());
                None
            }
            5 => {
                spec.sandbox
                    .as_mut()
                    .unwrap()
                    .container
                    .as_mut()
                    .unwrap()
                    .image = "wrong".into();
                None
            }
            6 => {
                spec.sandbox.as_mut().unwrap().inputs.session_cwd = f.primary.clone();
                None
            }
            _ => unreachable!(),
        };
        let error = missing
            .prepare(&mut spec)
            .err()
            .expect("drift refused")
            .to_string();
        assert!(
            !error.contains("credential source is unavailable"),
            "{error}"
        );
    }
    let mut in_repo = f.profile.clone();
    in_repo.credential_file = f.workspace.join("auth.json");
    private_write(&in_repo.credential_file, b"{}").unwrap();
    assert!(in_repo
        .prepare(&mut f.spec())
        .err()
        .unwrap()
        .to_string()
        .contains("outside repository"));
    let mut api_login: Value =
        serde_json::from_slice(&std::fs::read(&f.profile.credential_file).unwrap()).unwrap();
    api_login["OPENAI_API_KEY"] = json!(true);
    let api_bytes = serde_json::to_vec(&api_login).unwrap();
    for bytes in [
        b"not-json".as_slice(),
        br#"{"tokens":{},"tokens":{}}"#,
        api_bytes.as_slice(),
        br#"{"tokens":{}}"#,
    ] {
        std::fs::write(&f.profile.credential_file, bytes).unwrap();
        assert!(f.profile.prepare(&mut f.spec()).is_err());
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn docker_enabled() -> bool {
    if std::env::var("KRANZ_ACP_CONTAINER_TESTS").as_deref() == Ok("1") {
        true
    } else {
        eprintln!(
            "SKIP-ACP-CONTAINMENT: set KRANZ_ACP_CONTAINER_TESTS=1 for profile mission proof"
        );
        false
    }
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn acp_containment_v1_profile_ordinary_mission_delivers_with_one_call_consent() {
    profile_mission(ProfileScenario::Clean).await;
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn acp_containment_v1_profile_defect_requires_repair_and_fresh_consent() {
    profile_mission(ProfileScenario::Repair).await;
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn acp_containment_v1_profile_interrupt_rejects_late_consent() {
    profile_mission(ProfileScenario::Interrupt).await;
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn acp_containment_v1_profile_policy_drift_refuses_merge() {
    profile_mission(ProfileScenario::PolicyDrift).await;
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(any(target_os = "macos", target_os = "linux"))]
async fn acp_containment_v1_profile_checker_failure_blocks_completed_worker() {
    profile_mission(ProfileScenario::CheckerFailure).await;
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[derive(Clone, Copy, PartialEq)]
enum ProfileScenario {
    Clean,
    Repair,
    Interrupt,
    PolicyDrift,
    CheckerFailure,
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[allow(clippy::await_holding_lock)]
async fn profile_mission(scenario: ProfileScenario) {
    use crate::backend_mock::{mock_init, mock_result_text, mock_text, MockBackend, MockScript};
    use crate::events::EventKind;
    use crate::live_permission::{Actor, Delivery, Resolution};
    use crate::types::{ControlCommand, MissionStatus, Plan};
    use std::sync::Arc;
    use std::time::Duration;
    if !docker_enabled() {
        return;
    }
    // Keep PATH and Docker context stable while other tests poison the environment.
    let _env = crate::agent_env::EnvTestGuard::engage(&[]);
    let f = Fixture::new();
    let complete = r#"{"decision":"complete","guidance":"","summary":"accepted fixture"}"#;
    let checkpoint = r#"{"action":"commit-as-is","note":"host checkpoint"}"#;
    let repair = scenario == ProfileScenario::Repair;
    let fix = json!({"fixFeatures":[{"title":"repair seeded defect","spec":"write changed newline to source.txt","validationCriteria":["source changed"]}],"summary":"repair the failed contract"}).to_string();
    let mut replies = vec![checkpoint, complete];
    if repair {
        replies.extend([fix.as_str(), checkpoint, complete]);
    }
    replies.push("NONE");
    let clean_review = json!({"findings":[],"summary":"scripted fresh fixture review"});
    let mut scripts = vec![
        MockScript::streaming(vec![
            mock_init("orchestrator-fixture"),
            mock_result_text("ready"),
        ])
        .responding(
            replies
                .into_iter()
                .map(|r| vec![mock_text(r), mock_result_text(r)])
                .collect(),
        ),
        MockScript::single_shot_json(&clean_review).with_session_id("independent-fixture"),
    ];
    if repair {
        scripts.extend([
            MockScript::single_shot_json(&json!({"findings":[{"subject":"source changed","severity":"major","evidence":"source.txt:1 contains defect; engine contract a-1 failed","suggestedFix":"write changed newline"}],"summary":"seeded defect rejected"})).with_session_id("functional-defect"),
            MockScript::single_shot_json(&clean_review).with_session_id("scrutiny-after-repair"),
            MockScript::single_shot_json(&clean_review).with_session_id("functional-after-repair"),
        ]);
    }
    let backend = Arc::new(MockBackend::with_scripts(scripts));
    std::fs::create_dir(f.primary.join("pack")).unwrap();
    std::fs::write(f.primary.join("pack/pack.toml"),format!("[pack]\nname='profile-checker'\nschema=5\n[[evaluator]]\nname='profile-checker'\nimage='{}'\nexecutable='/usr/local/bin/python3'\nargs=['-I','-S','/checker/checker.py','pass']\nfiles=['checker.py']\nstages=['plan-approval','milestone-validation','final-gate','merge']\nevidence=['scope','check-receipt']\nkind='mechanical'\nenforcement='blocking'\n",f.profile.definition().unwrap().image)).unwrap();
    let checker = include_str!("../../tests/fixtures/gate-evaluator/checker.py");
    let checker = if scenario == ProfileScenario::CheckerFailure {
        checker.replace("request = json.load(sys.stdin)", "request = json.load(sys.stdin)\nif request['params']['stage'] == 'milestone-validation': sys.exit(17)")
    } else {
        checker.to_string()
    };
    std::fs::write(f.primary.join("pack/checker.py"), checker).unwrap();
    std::fs::write(
        f.primary.join(".kranz/merge-gates.json"),
        r#"{"gates":[{"command":"grep -qx changed source.txt"}]}"#,
    )
    .unwrap();
    git(&f.primary, &["add", "pack", ".kranz/merge-gates.json"]);
    git(&f.primary, &["commit", "-qm", "synthetic checker policy"]);
    let mut cfg = config(f.profile.clone());
    cfg.pack_dir = Some("pack".into());
    cfg.skip_functional = !repair;
    cfg.validator_allow_uncontained_degrade = true;
    let mut engine = crate::orchestrator::MissionEngine::create(
        backend.clone(),
        &f.primary,
        "contained profile",
        cfg,
    )
    .unwrap();
    let spec = if repair {
        "fixture-seeded-defect: write changed newline to source.txt"
    } else {
        "write changed newline to source.txt"
    };
    let plan:Plan=serde_json::from_value(json!({"goal":"contained profile","touchSet":["source.txt"],"validationContract":[{"id":"a-1","statement":"source changed","check":"command","command":"grep -qx changed source.txt"}],"milestones":[{"title":"delivery","features":[{"title":"change source","spec":spec,"validationCriteria":["source changed"]}]}]})).unwrap();
    engine.approve_plan(plan).unwrap();
    let base = engine.state().mission.base_sha.clone().unwrap();
    let branch = engine.state().mission.mission_branch.clone();
    let paths = engine.paths().clone();
    let expected_workers = if repair { 2 } else { 1 };
    let observer = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(90), async {
            let mut answered = std::collections::HashSet::new();
            loop {
                let events = crate::event_log::EventLog::read_events(&paths.events_file()).unwrap();
                if let Some(request) = events.into_iter().find_map(|e| match e.kind {
                    EventKind::PermissionRequested { request }
                        if !answered.contains(&request.proposal.id) =>
                    {
                        Some(request)
                    }
                    _ => None,
                }) {
                    assert!(
                        request.proposal.prohibition.is_none(),
                        "{:?}",
                        request.proposal.prohibition
                    );
                    answered.insert(request.proposal.id.clone());
                    if scenario == ProfileScenario::Interrupt {
                        crate::control::enqueue(&paths, &ControlCommand::Pause).unwrap();
                    }
                    crate::control::enqueue(
                        &paths,
                        &ControlCommand::ResolvePermission {
                            resolution: Resolution {
                                request_id: request.proposal.id,
                                binding_digest: request.binding_digest,
                                allow: true,
                                actor: Actor::LocalRepositoryAuthority,
                                reason: "synthetic fixture consent".into(),
                            },
                        },
                    )
                    .unwrap();
                    if answered.len() == expected_workers {
                        break;
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("permission was not requested");
    });
    let log = engine.paths().events_file();
    let mut mission_run = Box::pin(engine.run());
    let result = if scenario == ProfileScenario::Interrupt {
        // run() deliberately parks on Pause until Resume. Stop polling it only
        // after the engine has closed the worker and durably entered Paused.
        tokio::time::timeout(Duration::from_secs(90), async {
            let parked = async {
                loop {
                    let events = crate::event_log::EventLog::read_events(&log).unwrap();
                    if events
                        .iter()
                        .any(|e| matches!(e.kind, EventKind::MissionPaused { .. }))
                    {
                        let paused = events
                            .iter()
                            .find(|e| matches!(e.kind, EventKind::MissionPaused { .. }))
                            .unwrap()
                            .seq;
                        // Keep polling run() after pause. Dropping it immediately
                        // would hide a retry loop that dispatches while paused.
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        let later = crate::event_log::EventLog::read_events(&log).unwrap();
                        assert!(
                            !later.iter().any(|e| e.seq > paused
                                && matches!(
                                    e.kind,
                                    EventKind::WorkerSpawned { .. }
                                        | EventKind::OrchestratorDecision { .. }
                                )),
                            "paused mission continued work"
                        );
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            };
            tokio::select! {
                result = &mut mission_run => panic!("pause must park the mission: {result:?}"),
                () = parked => Ok(MissionStatus::Paused),
            }
        })
        .await
        .expect("mission did not park after interruption")
    } else {
        tokio::time::timeout(Duration::from_secs(180), &mut mission_run)
            .await
            .expect("bounded mission")
    };
    drop(mission_run);
    if let Err(error) = &result {
        observer.abort();
        panic!("mission failed: {error}");
    }
    observer.await.unwrap();
    if scenario == ProfileScenario::Interrupt {
        assert_eq!(result.unwrap(), MissionStatus::Paused);
        assert_eq!(git(&f.primary, &["rev-parse", "HEAD"]), base);
        assert_eq!(
            git(&f.primary, &["show", &format!("{branch}:source.txt")]),
            "base"
        );
        assert_eq!(engine.state().permissions.len(), 1);
        let permission = engine.state().permissions.values().next().unwrap();
        assert!(permission.closed.is_some());
        assert!(permission.resolution.is_none());
        assert!(permission.delivery.is_none());
        let workspace = permission.request.binding.workspace.clone();
        let paths = engine.paths().clone();
        drop(engine);
        let restored = crate::orchestrator::MissionEngine::resume(
            Arc::new(MockBackend::new()),
            &f.primary,
            &paths.mission_id,
            crate::event_log::LockForce::No,
        )
        .unwrap();
        assert_eq!(restored.state().mission.status, MissionStatus::Paused);
        assert!(restored
            .state()
            .permissions
            .values()
            .all(|p| p.closed.is_some() && p.resolution.is_none()));
        drop(restored);
        git(&f.primary, &["worktree", "remove", "--force", &workspace]);
        return;
    }
    if scenario == ProfileScenario::CheckerFailure {
        assert_eq!(result.unwrap(), MissionStatus::Blocked);
        assert_eq!(git(&f.primary, &["rev-parse", "HEAD"]), base);
        assert_eq!(
            git(&f.primary, &["show", &format!("{branch}:source.txt")]),
            "changed"
        );
        let gate = engine
            .state()
            .gate_evaluations
            .values()
            .find(|r| {
                r.requested.request.params.stage
                    == crate::gate_evaluation::protocol::Stage::MilestoneValidation
            })
            .unwrap();
        assert!(gate.consumed.is_none());
        let finished = gate.finished.as_ref().unwrap();
        assert!(finished.cleanup_confirmed);
        assert!(
            matches!(&finished.outcome, crate::gate_evaluation::lifecycle::Outcome::Error { message } if message.contains("unsuccessfully"))
        );
        let events =
            crate::event_log::EventLog::read_events(&engine.paths().events_file()).unwrap();
        assert!(!events
            .iter()
            .any(|e| matches!(e.kind, EventKind::MissionCompleted {})));
        return;
    }
    assert_eq!(
        result.unwrap(),
        MissionStatus::Complete,
        "{:?}",
        engine.state()
    );
    assert_eq!(git(&f.primary, &["rev-parse", "HEAD"]), base);
    assert_eq!(
        std::fs::read_to_string(f.primary.join("source.txt")).unwrap(),
        "base\n"
    );
    assert_eq!(
        git(&f.primary, &["show", &format!("{branch}:source.txt")]),
        "changed"
    );
    let state = engine.state();
    assert_eq!(state.permissions.len(), expected_workers);
    for permission in state.permissions.values() {
        assert_eq!(permission.delivery, Some(Delivery::Sent));
        assert!(permission.resolution.as_ref().unwrap().allow);
    }
    assert!(!state.mission.milestones[0].features[0].commits.is_empty());
    let events = crate::event_log::EventLog::read_events(&engine.paths().events_file()).unwrap();
    let encoded = serde_json::to_string(&events).unwrap();
    assert!(!encoded.contains("synthetic-login-no-authority-12345"));
    let transcripts = std::fs::read_dir(engine.paths().runs_dir()).unwrap();
    let mut profile_receipts = 0;
    for entry in transcripts {
        let entry = entry.unwrap();
        if entry.path().extension().is_none_or(|s| s != "jsonl") {
            continue;
        }
        let text = std::fs::read_to_string(entry.path()).unwrap();
        assert!(!text.contains("synthetic-login-no-authority-12345"));
        if text.contains("workerProfile") {
            profile_receipts += 1;
            // The fixture's parsed report records only its disposable HOME path.
            for run in state.runs.values().filter(|r| r.role == Role::Worker) {
                let report = run.report.as_ref().expect("parsed profile worker report");
                let home = Path::new(&report.test_evidence);
                assert!(
                    home.is_absolute()
                        && home
                            .parent()
                            .unwrap()
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with("kranz-acp-profile-")
                );
                assert!(!home.exists());
            }
        }
    }
    assert_eq!(profile_receipts, expected_workers);
    if repair {
        assert_eq!(state.mission.milestones[0].fix_cycles, 1);
        let specs = backend.started_specs();
        let tasks: Vec<_> = specs
            .iter()
            .filter_map(|s| match &s.prompt {
                PromptMode::SingleShot(task) if task.contains("Validate milestone") => Some(task),
                _ => None,
            })
            .collect();
        assert!(
            tasks
                .iter()
                .any(|task| task.contains("[a-1] `grep -qx changed source.txt` → FAIL")),
            "{tasks:?}"
        );
        assert!(
            tasks
                .iter()
                .any(|task| task.contains("[a-1] `grep -qx changed source.txt` → PASS")),
            "{tasks:?}"
        );
        let finding = events.iter().position(|e| matches!(&e.kind, EventKind::ValidationFinding { finding, .. } if finding.evidence.contains("source.txt:1 contains defect"))).unwrap();
        let fix = events
            .iter()
            .position(|e| matches!(e.kind, EventKind::FixFeatureCreated { .. }))
            .unwrap();
        let requests: Vec<_> = events
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                matches!(e.kind, EventKind::PermissionRequested { .. }).then_some(i)
            })
            .collect();
        assert!(requests[0] < finding && finding < fix && fix < requests[1]);
        let sessions: std::collections::HashSet<_> = state
            .permissions
            .values()
            .map(|p| &p.request.proposal.engine_session_id)
            .collect();
        assert_eq!(sessions.len(), 2, "repair requires a fresh worker session");
    }
    use crate::gate_evaluation::protocol::Stage;
    for stage in [
        Stage::PlanApproval,
        Stage::MilestoneValidation,
        Stage::FinalGate,
    ] {
        assert!(
            state
                .gate_evaluations
                .values()
                .any(|r| r.requested.request.params.stage == stage && r.consumed.is_some()),
            "missing {stage:?}"
        );
    }
    let active_paths = engine.paths().clone();
    let gate_policy = crate::command_exec::MergeGatePolicy {
        sandbox: crate::command_exec::worker_gate_sandbox(&state.config).unwrap(),
        mission_dir: active_paths.mission_dir(),
    };
    drop(engine);
    if scenario == ProfileScenario::PolicyDrift {
        let mut log = crate::event_log::EventLog::acquire(
            &active_paths,
            &active_paths.mission_id,
            Duration::ZERO,
            crate::event_log::LockForce::No,
        )
        .unwrap();
        log.append(EventKind::ConfigChanged {
            patch: json!({"packDir":null}),
        })
        .unwrap();
        drop(log);
        let error = crate::merge::merge_mission_with_external_evidence(
            &crate::git_ops::GitRepo::open(&f.primary).unwrap(),
            "main",
            &base,
            &branch,
            None,
            None,
            &Default::default(),
            |_, _| panic!("policy drift must refuse before commands"),
            &active_paths,
            Actor::LocalRepositoryAuthority,
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("evaluator pack changed after approval"),
            "{error}"
        );
        assert_eq!(git(&f.primary, &["rev-parse", "HEAD"]), base);
        return;
    }
    let primary = f.primary.clone();
    let merged = tokio::task::spawn_blocking(move || {
        crate::merge::merge_mission_with_external_evidence(
            &crate::git_ops::GitRepo::open(&primary).unwrap(),
            "main",
            &base,
            &branch,
            None,
            None,
            &Default::default(),
            |cmd, cwd| {
                let outcome =
                    crate::command_exec::run_bounded_gate_command_sandboxed(cwd, cmd, &gate_policy);
                assert!(outcome.0, "contained merge command failed: {}", outcome.1);
                outcome
            },
            &active_paths,
            Actor::LocalRepositoryAuthority,
        )
        .unwrap()
    })
    .await
    .unwrap();
    assert!(
        matches!(merged, crate::merge::MergeReport::Merged { .. }),
        "{merged:?}"
    );
    assert_eq!(
        std::fs::read_to_string(f.primary.join("source.txt")).unwrap(),
        "changed\n"
    );
    let mission_id = events[0].mission_id.clone();
    let final_paths = crate::paths::MissionPaths::new(&f.primary, &mission_id);
    let events = crate::event_log::EventLog::read_events(&final_paths.events_file()).unwrap();
    let replay = crate::reducer::fold(&events).unwrap();
    let merge = replay
        .gate_evaluations
        .values()
        .find(|r| r.requested.request.params.stage == Stage::Merge)
        .unwrap();
    assert!(merge.consumed.is_some());
    let crate::gate_evaluation::protocol::Subject::Integration {
        integration_tree, ..
    } = &merge.requested.request.params.subject
    else {
        panic!("merge must judge an integration tree")
    };
    assert_eq!(
        integration_tree.value,
        git(&f.primary, &["rev-parse", "HEAD^{tree}"])
    );
    assert_eq!(
        merge
            .resolution
            .as_ref()
            .unwrap()
            .consent
            .as_ref()
            .unwrap()
            .actor,
        Actor::LocalRepositoryAuthority
    );
    let export = f._dir.path().join("audit-export");
    crate::evidence_bundle::export_evidence_bundle(&f.primary, &mission_id, &export).unwrap();
    assert!(export.join("manifest.json").is_file());
    let bundle = crate::evidence_bundle::assemble_evidence_bundle(&f.primary, &mission_id).unwrap();
    assert!(bundle
        .files
        .iter()
        .all(|file| !String::from_utf8_lossy(&file.bytes)
            .contains("synthetic-login-no-authority-12345")));
    if repair {
        for entry in &bundle.manifest.entries {
            if let Some(path) = &entry.path {
                let bytes = std::fs::read(export.join(path)).unwrap();
                assert_eq!(
                    entry.sha256.as_deref(),
                    crate::gate_evaluation::protocol::Digest::of(&bytes)
                        .as_str()
                        .strip_prefix("sha256:")
                );
            }
        }
        let exported_events =
            crate::event_log::EventLog::read_events(&export.join("events.jsonl")).unwrap();
        let exported = crate::reducer::fold(&exported_events).unwrap();
        assert_eq!(exported.permissions.len(), 2);
        assert_eq!(exported.mission.milestones[0].fix_cycles, 1);
        assert!(exported
            .gate_evaluations
            .values()
            .any(|r| r.requested.request.params.stage == Stage::Merge && r.consumed.is_some()));
        let before = std::fs::read(final_paths.events_file()).unwrap();
        std::fs::remove_dir_all(final_paths.runs_dir()).unwrap();
        let cleaned =
            crate::evidence_bundle::assemble_evidence_bundle(&f.primary, &mission_id).unwrap();
        let missing = |bundle: &crate::evidence_bundle::EvidenceBundle| {
            bundle
                .manifest
                .entries
                .iter()
                .filter(|e| e.status == Some(crate::provenance::ArtefactStatus::Unresolved))
                .count()
        };
        assert!(
            missing(&cleaned) > missing(&bundle),
            "cleaned runtime evidence must remain visibly unresolved"
        );
        assert_eq!(
            std::fs::read(final_paths.events_file()).unwrap(),
            before,
            "export cannot replay an effect"
        );
        assert_eq!(
            git(&f.primary, &["rev-parse", "HEAD^{tree}"]),
            integration_tree.value
        );
    }
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn acp_profile_gate_checks_are_offline_without_changing_worker_access() {
    let cfg = config(profile("fixture-acp-worker-v1"));
    let gates = crate::command_exec::worker_gate_sandbox(&cfg).unwrap();
    assert!(gates.egress.is_empty());
    assert_eq!(gates.enforce, SandboxEnforce::FsNet);
    assert_eq!(gates.image, cfg.worker.sandbox.image);
    assert_eq!(cfg.worker.sandbox.egress, ["provider.invalid:443"]);
    let plain = MissionConfig::default();
    assert_eq!(
        crate::command_exec::worker_gate_sandbox(&plain).unwrap(),
        plain.worker.sandbox
    );
    let mut drift = cfg;
    drift.worker.sandbox.extra_write.push("/".into());
    assert!(crate::command_exec::worker_gate_sandbox(&drift).is_err());
}

#[test]
#[cfg(all(any(target_os = "macos", target_os = "linux"), target_arch = "aarch64"))]
fn acp_profile_claude_uses_only_explicit_oauth_and_private_empty_home() {
    let f = Fixture::new();
    let mut p = f.profile.clone();
    p.id = CLAUDE.into();
    std::fs::write(&p.credential_file,br#"{"credentialEnv":"CLAUDE_CODE_OAUTH_TOKEN","value":"synthetic-claude-oauth-no-authority"}"#).unwrap();
    let mut spec = f.spec();
    spec.sandbox
        .as_mut()
        .unwrap()
        .container
        .as_mut()
        .unwrap()
        .image = IMAGE.into();
    spec.sandbox.as_mut().unwrap().inputs.egress = p
        .definition()
        .unwrap()
        .egress
        .iter()
        .map(|s| (*s).into())
        .collect();
    let mut prepared = p.prepare(&mut spec).unwrap();
    assert_eq!(
        spec.env["CLAUDE_CODE_OAUTH_TOKEN"],
        "synthetic-claude-oauth-no-authority"
    );
    assert_eq!(spec.env["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"], "1");
    assert_eq!(
        std::fs::read_dir(&spec.sandbox.as_ref().unwrap().inputs.tmpdir)
            .unwrap()
            .count(),
        0
    );
    prepared.close().unwrap();
    spec.env.clear();
    std::fs::write(
        &p.credential_file,
        br#"{"credentialEnv":"ANTHROPIC_API_KEY","value":"synthetic-claude-oauth-no-authority"}"#,
    )
    .unwrap();
    assert!(p.prepare(&mut spec).is_err());
}

#[tokio::test(flavor = "multi_thread")]
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[allow(clippy::await_holding_lock)]
async fn acp_containment_v1_profile_credential_echo_fails_and_cleans_home() {
    use crate::backend::{AgentBackend, AgentEvent, SessionExit};
    if !docker_enabled() {
        return;
    }
    // Keep PATH and Docker context stable while other tests poison the environment.
    let _env = crate::agent_env::EnvTestGuard::engage(&[]);
    let f = Fixture::new();
    let mut spec = f.spec();
    spec.prompt = PromptMode::SingleShot("fixture-expose-login".into());
    let paths = crate::paths::MissionPaths::new(&f.primary, "m-profile");
    let boundary = crate::egress_proxy::maybe_start_for_session(&mut spec, &paths)
        .await
        .unwrap()
        .unwrap();
    let backend =
        crate::backend_acp::AcpBackend::for_worker(&config(f.profile.clone()).worker).unwrap();
    let mut session = backend.start(spec).await.unwrap();
    let mut result_seen = false;
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            match session.next_event().await {
                Ok(Some(event)) => {
                    assert!(!format!("{event:?}").contains("synthetic-login-no-authority-12345"));
                    result_seen |= matches!(event, AgentEvent::Result { .. });
                }
                Ok(None) => break,
                Err(error) => {
                    assert!(!error
                        .to_string()
                        .contains("synthetic-login-no-authority-12345"));
                    break;
                }
            }
        }
        session.abort().await.unwrap();
    })
    .await
    .unwrap();
    assert!(!result_seen);
    assert!(!matches!(
        session.exit_status(),
        Some(SessionExit::Completed)
    ));
    assert!(boundary.shutdown().await.unwrap().is_empty());
    let home = std::fs::read_to_string(f.workspace.join("source.txt")).unwrap();
    assert!(home.starts_with('/') && home.contains("kranz-acp-profile-"));
    assert!(!Path::new(&home).exists());
    assert_eq!(
        std::fs::read_to_string(f.primary.join("source.txt")).unwrap(),
        "base\n"
    );
}
