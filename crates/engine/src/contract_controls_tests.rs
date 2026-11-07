use super::*;

#[path = "contract_controls_pair_tests.rs"]
mod pair_tests;

// Libtest runs unrelated evaluation fixtures concurrently. Wait only for the
// explicitly tested admission refusal, never retry an execution/receipt error.
fn evaluate_available(
    repo: &GitRepo,
    paths: &MissionPaths,
    revision: &str,
    assertions: &[Assertion],
    config: &MissionConfig,
) -> Vec<GateReport> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let reports = evaluate(repo, paths, revision, assertions, config);
        if !reports.iter().any(|report| {
            report
                .outcome
                .artefact
                .detail
                .as_deref()
                .is_some_and(|text| text.contains("control evaluator busy"))
        }) {
            return reports;
        }
        assert!(
            Instant::now() < deadline,
            "control fixtures did not release admission"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn contract_readback_control_admission_and_cancellation_prevent_new_launches() {
    let (_dir, repo, paths, sha) = fixture(CHECKER);
    let permit = CONTROL_EXECUTIONS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let reports = evaluate(
        &repo,
        &paths,
        &sha,
        &[assertion(CHECKER)],
        &MissionConfig::default(),
    );
    let evidence = receipt(&paths, &reports[0]);
    assert!(evidence.detail.contains("control evaluator busy"));
    assert!(evidence.valid.is_none());
    drop(permit);
    let guard = CancellationGuard::default();
    let cancelled = guard.flag();
    assert!(check_budget(Instant::now() + Duration::from_secs(1), &cancelled).is_ok());
    drop(guard);
    assert!(
        check_budget(Instant::now() + Duration::from_secs(1), &cancelled)
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    assert!(check_budget(Instant::now(), &AtomicBool::new(false))
        .unwrap_err()
        .to_string()
        .contains("budget exhausted"));
    assert_eq!(repo.list_worktrees().unwrap().len(), 1);
}

const CHECKER: &str = r#"set -eu
. ./authorization.sh
state=0
mutate() { if authorize "$1"; then state=1; fi; }
mutate ''
[ "$state" = 0 ] || exit 5
mutate wrong-nonempty-credential
if [ "$state" != 0 ]; then
  printf '%s' '{"checksRun":2,"outcome":"failed","failureId":"invalid-credential-authorized"}' > "$KRANZ_CONTROL_RESULT"
  exit 1
fi
mutate correct-credential
[ "$state" = 1 ] || exit 6
printf '%s' '{"checksRun":3,"outcome":"passed"}' > "$KRANZ_CONTROL_RESULT"
"#;

const VALID: &str = "authorize() { [ \"$1\" = correct-credential ]; }\n";
const DEFECTIVE: &str = "authorize() { [ -n \"$1\" ]; }\n";

fn assertion(checker: &str) -> Assertion {
    Assertion {
        id: "a-controls".into(),
        statement: "An invalid credential cannot authorize a mutation".into(),
        check: AssertionCheck::Command,
        command: Some("sh check.sh".into()),
        pty_script: None,
        negative_control: Some(ControlSpec {
            checker_files: vec![ControlFile {
                path: "check.sh".into(),
                content: checker.into(),
            }],
            valid_files: vec![ControlFile {
                path: "authorization.sh".into(),
                content: VALID.into(),
            }],
            defective_files: vec![ControlFile {
                path: "authorization.sh".into(),
                content: DEFECTIVE.into(),
            }],
            expected_failure: "invalid-credential-authorized".into(),
            timeout_seconds: 5,
            baseline_pair: None,
        }),
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/nonexistent-kranz-hooks",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
        ])
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", root.join("no-global"))
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture(checker: &str) -> (tempfile::TempDir, GitRepo, MissionPaths, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    git(&root, &["init", "-b", "main"]);
    std::fs::write(root.join("check.sh"), checker).unwrap();
    std::fs::write(root.join("authorization.sh"), VALID).unwrap();
    std::fs::write(root.join(".gitignore"), ".kranz/\n").unwrap();
    git(
        &root,
        &["add", "check.sh", "authorization.sh", ".gitignore"],
    );
    git(&root, &["commit", "-m", "control inputs"]);
    let repo = GitRepo::open(&root).unwrap();
    let sha = repo.head_sha().unwrap();
    let paths = MissionPaths::new(root, "m-controls");
    paths.open_mission_dir_nofollow(true).unwrap();
    (dir, repo, paths, sha)
}

fn receipt(paths: &MissionPaths, report: &GateReport) -> ControlEvidence {
    let reference = report
        .outcome
        .artefact
        .reference
        .strip_prefix("file:")
        .expect("file receipt");
    serde_json::from_slice(&std::fs::read(paths.mission_dir().join(reference)).unwrap()).unwrap()
}

/// Probe only the capability itself. Production-wrap failures after this
/// positive control are test failures, never skipped as unavailable evidence.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn containment_available() -> bool {
    #[cfg(target_os = "macos")]
    let (tool, args, capability) = (
        "sandbox-exec",
        vec!["-p", "(version 1)(allow default)", "/usr/bin/true"],
        crate::test_capability::capability::SANDBOX_EXEC,
    );
    #[cfg(target_os = "linux")]
    let (tool, args, capability) = (
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
    let probe = std::process::Command::new(tool).args(args).output();
    match probe {
        Ok(output) if output.status.success() => true,
        #[cfg(target_os = "macos")]
        Ok(output) if String::from_utf8_lossy(&output.stderr).contains("sandbox_apply") => {
            eprintln!("SKIP-UNDER-WRAP (gate-sandbox-supervision-dogfood): negative controls cannot nest sandbox_apply");
            false
        }
        other => {
            crate::test_capability::skip(
                capability,
                &format!("control containment probe failed: {other:?}"),
            );
            false
        }
    }
}

#[test]
fn contract_readback_control_legacy_wire_and_bounded_definition() {
    let old =
        serde_json::json!({"id":"a-1","statement":"works","check":"command","command":"true"});
    let parsed: Assertion = serde_json::from_value(old.clone()).unwrap();
    assert!(parsed.negative_control.is_none());
    assert_eq!(serde_json::to_value(parsed).unwrap(), old);
    let good = assertion(CHECKER);
    validate(std::slice::from_ref(&good)).unwrap();
    let mut roundtrip = serde_json::to_value(&good).unwrap();
    roundtrip["negativeControl"]
        .as_object_mut()
        .unwrap()
        .remove("timeoutSeconds");
    assert_eq!(
        serde_json::from_value::<Assertion>(roundtrip)
            .unwrap()
            .negative_control
            .unwrap()
            .timeout_seconds,
        60
    );
    for path in [
        "../outside",
        "/outside",
        "check.sh",
        "a/.git/config",
        "A/.KRANZ/serve.token",
        "a\\b",
        "C:evil",
        "a/../b",
        "a//b",
        "a/",
        "a.",
    ] {
        let mut bad = good.clone();
        let spec = bad.negative_control.as_mut().unwrap();
        spec.valid_files[0].path = path.into();
        spec.defective_files[0].path = path.into();
        assert!(validate(&[bad]).is_err(), "path {path}");
    }
    let mut bad = good.clone();
    bad.negative_control.as_mut().unwrap().timeout_seconds = 181;
    assert!(validate(&[bad]).is_err());
    let mut bad = good.clone();
    bad.negative_control.as_mut().unwrap().valid_files[0].content = "x".repeat(MAX_FILE_BYTES + 1);
    assert!(validate(&[bad]).is_err());
    let mut bad = good.clone();
    bad.negative_control.as_mut().unwrap().defective_files[0].path = "AUTHORIZATION.sh".into();
    assert!(validate(&[bad]).is_err());
    assert!(validate(&vec![good.clone(); MAX_CONTROLS + 1]).is_err());
    let mut bad = good;
    bad.negative_control.as_mut().unwrap().defective_files[0].content = VALID.into();
    assert!(validate(&[bad]).is_err());
}

#[test]
fn contract_readback_control_receipts_require_behavioral_checks_and_matching_failure() {
    let case = |code, checks, outcome, failure: Option<&str>| CaseEvidence {
        exit_code: code,
        receipt: Some(CheckReceipt {
            checks_run: checks,
            outcome,
            failure_id: failure.map(str::to_string),
            diagnostic: None,
        }),
        output_tail: String::new(),
        elapsed_ms: 0,
        environment_names: Vec::new(),
        binding: None,
    };
    let valid = case(Some(0), 3, CheckOutcome::Passed, None);
    let good = case(Some(1), 2, CheckOutcome::Failed, Some("bad-auth"));
    assert_eq!(classify(&valid, &good, "bad-auth"), ControlStatus::Verified);
    assert_eq!(
        classify(&valid, &valid, "bad-auth"),
        ControlStatus::NotRejected
    );
    for defective in [
        case(None, 2, CheckOutcome::Failed, Some("bad-auth")),
        case(Some(1), 0, CheckOutcome::Failed, Some("bad-auth")),
        case(Some(1), 2, CheckOutcome::Failed, Some("compiler-error")),
        case(Some(0), 2, CheckOutcome::Failed, Some("bad-auth")),
    ] {
        assert_eq!(
            classify(&valid, &defective, "bad-auth"),
            ControlStatus::Inconclusive
        );
    }
    assert_eq!(
        classify(&good, &good, "bad-auth"),
        ControlStatus::Inconclusive
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn contract_readback_control_authorization_positive_negative_and_missing_coverage() {
    if !containment_available() {
        return;
    }
    let (_dir, repo, paths, sha) = fixture(CHECKER);
    let a = assertion(CHECKER);
    let reports = evaluate_available(
        &repo,
        &paths,
        &sha,
        std::slice::from_ref(&a),
        &MissionConfig::default(),
    );
    let evidence = receipt(&paths, &reports[0]);
    assert_eq!(evidence.status, ControlStatus::Verified, "{evidence:?}");
    assert!(reports[0].outcome.passed());
    assert_eq!(evidence.source_revision, sha);
    assert_eq!(evidence.assertion_sha256, identity(&a));
    assert_eq!(repo.head_sha().unwrap(), sha);
    assert!(repo.is_clean_tracked_strict().unwrap());
    assert_eq!(
        std::fs::read_to_string(repo.root().join("authorization.sh")).unwrap(),
        VALID
    );
    assert_eq!(repo.list_worktrees().unwrap().len(), 1);

    let weak = CHECKER.replace(
        "mutate wrong-nonempty-credential",
        "# wrong nonempty credentials are never tried",
    );
    let (_dir, repo, paths, sha) = fixture(&weak);
    let reports = evaluate_available(
        &repo,
        &paths,
        &sha,
        &[assertion(&weak)],
        &MissionConfig::default(),
    );
    let evidence = receipt(&paths, &reports[0]);
    assert_eq!(evidence.status, ControlStatus::NotRejected, "{evidence:?}");
    assert!(!reports[0].outcome.passed());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn contract_readback_control_setup_zero_checks_and_timeout_are_inconclusive() {
    if !containment_available() {
        return;
    }
    for checker in [
        "exit 2\n",
        "printf '%s' '{\"checksRun\":0,\"outcome\":\"passed\"}' > \"$KRANZ_CONTROL_RESULT\"\n",
        "sleep 30\n",
    ] {
        let (_dir, repo, paths, sha) = fixture(checker);
        let mut a = assertion(checker);
        a.negative_control.as_mut().unwrap().timeout_seconds = 1;
        let reports = evaluate_available(&repo, &paths, &sha, &[a], &MissionConfig::default());
        let evidence = receipt(&paths, &reports[0]);
        assert_eq!(evidence.status, ControlStatus::Inconclusive, "{evidence:?}");
        assert!(!reports[0].outcome.passed());
        if checker.starts_with("sleep") {
            assert!(evidence.valid.unwrap().exit_code.is_none());
        }
    }
}

#[test]
fn contract_readback_control_changed_inputs_fail_closed_and_receipts_are_never_reused() {
    let (_dir, repo, paths, sha) = fixture(CHECKER);
    let mut a = assertion(CHECKER);
    a.negative_control.as_mut().unwrap().checker_files[0]
        .content
        .push_str("# changed approved checker\n");
    let reports = evaluate_available(
        &repo,
        &paths,
        &sha,
        std::slice::from_ref(&a),
        &MissionConfig::default(),
    );
    let evidence = receipt(&paths, &reports[0]);
    assert_eq!(evidence.status, ControlStatus::Inconclusive);
    assert!(evidence.valid.is_none());
    let second = evaluate_available(
        &repo,
        &paths,
        &sha,
        std::slice::from_ref(&a),
        &MissionConfig::default(),
    );
    assert_ne!(
        reports[0].outcome.artefact.reference,
        second[0].outcome.artefact.reference
    );
    assert_eq!(repo.list_worktrees().unwrap().len(), 1);

    // The production artifact reference survives export; deleting its bytes
    // becomes explicit unresolved evidence, never a fabricated verified pair.
    let mut log = crate::event_log::EventLog::acquire(
        &paths,
        &paths.mission_id,
        Duration::ZERO,
        crate::event_log::LockForce::No,
    )
    .unwrap();
    log.append(crate::events::EventKind::MissionCreated {
        goal: a.statement.clone(),
        base_branch: "main".into(),
        mission_branch: "kranz/control-fixture".into(),
        config: MissionConfig::default(),
    })
    .unwrap();
    for event in
        crate::gate_results::gate_result_events(crate::gate::GateSurface::Approval, &reports)
    {
        log.append(event).unwrap();
    }
    drop(log);
    let bundle =
        crate::evidence_bundle::assemble_evidence_bundle(repo.root(), &paths.mission_id).unwrap();
    let reference = &reports[0].outcome.artefact.reference;
    let entry = bundle
        .manifest
        .entries
        .iter()
        .find(|entry| &entry.source == reference)
        .unwrap();
    assert_eq!(
        entry.status,
        Some(crate::provenance::ArtefactStatus::Resolved)
    );
    let bytes = bundle
        .files
        .iter()
        .find(|file| Some(&file.path) == entry.path.as_ref())
        .unwrap();
    let exported: ControlEvidence = serde_json::from_slice(&bytes.bytes).unwrap();
    assert_eq!(exported.assertion_sha256, identity(&a));
    assert_eq!(exported.status, ControlStatus::Inconclusive);
    std::fs::remove_file(
        paths
            .mission_dir()
            .join(reference.strip_prefix("file:").unwrap()),
    )
    .unwrap();
    let bundle =
        crate::evidence_bundle::assemble_evidence_bundle(repo.root(), &paths.mission_id).unwrap();
    let entry = bundle
        .manifest
        .entries
        .iter()
        .find(|entry| &entry.source == reference)
        .unwrap();
    assert_eq!(
        entry.status,
        Some(crate::provenance::ArtefactStatus::Unresolved)
    );
    assert!(entry.path.is_none());
}

#[test]
fn contract_readback_control_unverified_provider_is_inconclusive_without_execution() {
    let (_dir, repo, paths, sha) = fixture(CHECKER);
    let mut config = MissionConfig::default();
    config.worker.sandbox.provider = SandboxProvider::Container;
    let reports = evaluate_available(&repo, &paths, &sha, &[assertion(CHECKER)], &config);
    let evidence = receipt(&paths, &reports[0]);
    assert_eq!(evidence.status, ControlStatus::Inconclusive);
    assert!(evidence.valid.is_none());
    assert_eq!(repo.list_worktrees().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn contract_readback_control_refuses_symlinked_checker_fixture_and_receipt() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("victim"), "intact").unwrap();
    symlink(outside.path(), dir.path().join("link")).unwrap();
    let file = ControlFile {
        path: "link/victim".into(),
        content: "changed".into(),
    };
    assert!(apply_files(dir.path(), std::slice::from_ref(&file)).is_err());
    assert!(check_inputs(dir.path(), &[file]).is_err());
    assert_eq!(
        std::fs::read_to_string(outside.path().join("victim")).unwrap(),
        "intact"
    );
    symlink(
        outside.path().join("victim"),
        dir.path().join("receipt.json"),
    )
    .unwrap();
    let cap = Dir::open_ambient_dir(dir.path(), cap_std::ambient_authority()).unwrap();
    assert!(crate::paths::read_regular_file_under(
        &cap,
        Path::new("receipt.json"),
        MAX_FILE_BYTES as u64
    )
    .is_err());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn contract_readback_control_runtime_cannot_change_checks_source_or_authority() {
    if !containment_available() {
        return;
    }
    let (_dir, repo, paths, _sha) = fixture(CHECKER);
    std::fs::write(paths.kranz_dir().join("serve.token"), "private-authority").unwrap();
    let checker = format!(
        r#"set -eu
if (printf tampered > check.sh) 2>/dev/null; then exit 9; fi
if (printf tampered > authorization.sh) 2>/dev/null; then exit 9; fi
if (printf tampered > '{source}/authorization.sh') 2>/dev/null; then exit 9; fi
if (printf tampered > '{source}/.kranz/serve.token') 2>/dev/null; then exit 9; fi
if [ -s '{source}/.kranz/serve.token' ] && cat '{source}/.kranz/serve.token' 2>/dev/null; then exit 9; fi
{CHECKER}"#,
        source = repo.root().display()
    );
    std::fs::write(repo.root().join("check.sh"), &checker).unwrap();
    git(repo.root(), &["add", "check.sh"]);
    git(repo.root(), &["commit", "-m", "containment checker"]);
    let sha = repo.head_sha().unwrap();
    let mut config = MissionConfig::default();
    config.worker.sandbox.extra_write = vec![repo.root().display().to_string()];
    let reports = evaluate_available(&repo, &paths, &sha, &[assertion(&checker)], &config);
    let evidence = receipt(&paths, &reports[0]);
    assert_eq!(evidence.status, ControlStatus::Verified, "{evidence:?}");
    assert_eq!(
        std::fs::read_to_string(paths.kranz_dir().join("serve.token")).unwrap(),
        "private-authority"
    );
    assert_eq!(
        std::fs::read_to_string(repo.root().join("check.sh")).unwrap(),
        checker
    );
    assert_eq!(
        std::fs::read_to_string(repo.root().join("authorization.sh")).unwrap(),
        VALID
    );
    assert!(repo.is_clean_tracked_strict().unwrap());
}

fn readback_receipt(paths: &MissionPaths, report: &GateReport) -> readback::ReadbackEvidence {
    let reference = report
        .outcome
        .artefact
        .reference
        .strip_prefix("file:")
        .expect("file receipt");
    serde_json::from_slice(&std::fs::read(paths.mission_dir().join(reference)).unwrap()).unwrap()
}

#[test]
fn contract_readback_control_authorization_fixture_exposes_omitted_wrong_credential_and_state_check(
) {
    let mission_goal = "Prevent unauthorized state mutations when callers supply invalid tokens";
    let reviewer = readback::FixtureReadbackReviewer::authorization_fixture();

    // 1. Complete authorization checker: exercises empty, wrong nonempty, and
    // correct credentials and verifies state stays 0 on rejected attempts.
    let (_dir, repo, paths, sha) = fixture(CHECKER);
    let complete_assertion = assertion(CHECKER);
    let rb_reports = readback::evaluate(
        &repo,
        &paths,
        &sha,
        Some(mission_goal),
        std::slice::from_ref(&complete_assertion),
        &[],
        &reviewer,
    );
    assert_eq!(rb_reports.len(), 1);
    assert_eq!(rb_reports[0].name, "contract-readback:a-controls");
    assert!(rb_reports[0].outcome.passed());
    let complete_ev = readback_receipt(&paths, &rb_reports[0]);
    assert_eq!(
        complete_ev.original_claim,
        "An invalid credential cannot authorize a mutation"
    );
    assert_eq!(
        complete_ev.translation.status,
        readback::ReadbackStatus::Complete
    );
    assert!(complete_ev
        .translation
        .exercised
        .iter()
        .any(|item| item.contains("wrong-nonempty-credential")));
    assert!(complete_ev
        .translation
        .asserted
        .iter()
        .any(|item| item.contains("state remains 0 after wrong-nonempty-credential")));
    assert!(complete_ev.input_isolation.goal_excluded);
    assert!(complete_ev.input_isolation.statement_excluded);
    assert!(complete_ev.input_isolation.session_tools_disabled);
    assert!(complete_ev.input_isolation.isolated_cwd);
    assert_eq!(
        complete_ev.input_isolation.included_fields,
        vec!["command".to_string(), "checkerFiles".to_string()]
    );

    // 2. Weakened checker covering only missing credentials: read-back exposes
    // that wrong nonempty credentials and unchanged-state verification after a
    // wrong nonempty credential are omitted.
    let weak = CHECKER.replace(
        "mutate wrong-nonempty-credential",
        "# wrong nonempty credentials are never tried",
    );
    let (_dir, repo, paths, sha) = fixture(&weak);
    let weak_assertion = assertion(&weak);
    let rb_reports = readback::evaluate(
        &repo,
        &paths,
        &sha,
        Some(mission_goal),
        std::slice::from_ref(&weak_assertion),
        &[],
        &reviewer,
    );
    let weak_ev = readback_receipt(&paths, &rb_reports[0]);
    assert_eq!(
        weak_ev.translation.status,
        readback::ReadbackStatus::Complete
    );
    assert!(
        weak_ev
            .translation
            .omissions
            .iter()
            .any(|o| o.contains("wrong nonempty credentials")),
        "omissions: {:?}",
        weak_ev.translation.omissions
    );
    assert!(
        weak_ev
            .translation
            .omissions
            .iter()
            .any(|o| o.contains("unchanged state")),
        "omissions: {:?}",
        weak_ev.translation.omissions
    );

    // 3. Verify the captured reviewer inputs exclude the mission goal, the
    // assertion statement, validFiles, defectiveFiles, and expectedFailure.
    let captured = reviewer.captured_inputs.lock().unwrap();
    assert_eq!(captured.len(), 2);
    for (input, prompt) in captured.iter() {
        assert!(!prompt.contains(mission_goal));
        assert!(!prompt.contains(&complete_assertion.statement));
        assert!(!prompt.contains(VALID.trim()));
        assert!(!prompt.contains(DEFECTIVE.trim()));
        assert_eq!(input.command, "sh check.sh");
        assert_eq!(input.checker_files.len(), 1);
        assert_eq!(input.checker_files[0].path, "check.sh");
    }
}

#[test]
fn contract_readback_control_agent_backend_session_enforces_isolated_cwd_and_disabled_tools() {
    struct MockSession {
        response: Option<String>,
    }
    #[async_trait::async_trait]
    impl crate::backend::AgentSession for MockSession {
        fn session_id(&self) -> String {
            "s-readback".into()
        }
        async fn next_event(&mut self) -> Result<Option<crate::backend::AgentEvent>> {
            Ok(self
                .response
                .take()
                .map(|text| crate::backend::AgentEvent::Result {
                    text,
                    is_error: false,
                    usage: crate::types::TokenUsage::default(),
                    cost_usd: Some(0.01),
                    num_turns: Some(1),
                    raw: serde_json::json!({}),
                }))
        }
        async fn send_user_message(&mut self, _text: &str) -> Result<()> {
            Ok(())
        }
        async fn abort(&mut self) -> Result<()> {
            Ok(())
        }
        fn exit_status(&self) -> Option<crate::backend::SessionExit> {
            Some(crate::backend::SessionExit::Completed)
        }
    }
    struct InspectingBackend {
        repo_root: PathBuf,
        goal: String,
        statement: String,
        observed_specs: Mutex<Vec<crate::backend::SessionSpec>>,
    }
    #[async_trait::async_trait]
    impl crate::backend::AgentBackend for InspectingBackend {
        async fn start(
            &self,
            spec: crate::backend::SessionSpec,
        ) -> Result<Box<dyn crate::backend::AgentSession>> {
            assert!(!spec.writable);
            assert!(spec.tools.is_empty());
            assert!(spec.allowed_tools.is_empty());
            assert!(spec.disallowed_tools.contains(&"Read(*)".to_string()));
            assert!(spec.disallowed_tools.contains(&"Bash(*)".to_string()));
            assert_ne!(spec.cwd, self.repo_root);
            assert!(!spec.cwd.starts_with(&self.repo_root));
            let crate::backend::PromptMode::SingleShot(ref prompt) = spec.prompt else {
                panic!("expected single-shot read-back prompt");
            };
            assert!(!prompt.contains(&self.goal));
            assert!(!prompt.contains(&self.statement));
            self.observed_specs.lock().unwrap().push(spec);
            Ok(Box::new(MockSession {
                response: Some(
                    readback::REVIEWED_REAL_MODEL_AUTHORIZATION_COMPLETE_JSON.to_string(),
                ),
            }))
        }
    }

    let (_dir, repo, paths, sha) = fixture(CHECKER);
    let a = assertion(CHECKER);
    let backend = InspectingBackend {
        repo_root: repo.root().to_path_buf(),
        goal: "Secret mission goal that must not reach read-back".into(),
        statement: a.statement.clone(),
        observed_specs: Mutex::new(Vec::new()),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let reviewer = readback::AgentBackendReviewer {
        backend: &backend,
        handle: rt.handle().clone(),
        backend_name: "mock-claude".into(),
        model: "claude-sonnet-4-6".into(),
        effort: "medium".into(),
    };
    let reports = readback::evaluate(
        &repo,
        &paths,
        &sha,
        Some("Secret mission goal that must not reach read-back"),
        &[a],
        &[],
        &reviewer,
    );
    assert_eq!(reports.len(), 1);
    assert!(reports[0].outcome.passed());
    assert_eq!(backend.observed_specs.lock().unwrap().len(), 1);
}

#[test]
fn contract_readback_control_unavailable_checker_or_dependency_is_incomplete_without_inference() {
    let reviewer = readback::FixtureReadbackReviewer::authorization_fixture();
    let (_dir, repo, paths, sha) = fixture(CHECKER);

    // 1. Command references an unprovided script `checks/future_auth.sh`:
    // read-back must report Incomplete without calling the model to guess from
    // the script's name.
    let mut missing_script = assertion(CHECKER);
    missing_script.command = Some("sh checks/future_auth.sh".into());
    let reports = readback::evaluate(
        &repo,
        &paths,
        &sha,
        Some("Verify authorization"),
        &[missing_script],
        &[],
        &reviewer,
    );
    assert!(!reports[0].outcome.passed());
    let ev = readback_receipt(&paths, &reports[0]);
    assert_eq!(ev.translation.status, readback::ReadbackStatus::Incomplete);
    assert!(
        reviewer.captured_inputs.lock().unwrap().is_empty(),
        "reviewer must not be called to infer behavior when command script is absent from checkerFiles"
    );
    assert!(
        readback::record_disposition(
            &paths,
            &ev,
            readback::DispositionStatus::Accepted,
            "operator",
            Some("cannot accept incomplete evidence".into()),
        )
        .is_err(),
        "incomplete read-back evidence cannot be accepted as verified"
    );

    // 2. Approved checker file on disk differs from pinned checkerFiles.
    let mut drifted = assertion(CHECKER);
    drifted.negative_control.as_mut().unwrap().checker_files[0]
        .content
        .push_str("# unapproved edit\n");
    let reports = readback::evaluate(
        &repo,
        &paths,
        &sha,
        Some("Verify authorization"),
        &[drifted],
        &[],
        &reviewer,
    );
    let ev = readback_receipt(&paths, &reports[0]);
    assert_eq!(ev.translation.status, readback::ReadbackStatus::Incomplete);
}

#[test]
fn contract_readback_control_disposition_stales_on_changed_inputs_and_exports_with_redaction() {
    let (_dir, repo, paths, sha) = fixture(CHECKER);
    let a = assertion(CHECKER);
    let config = MissionConfig::default();
    let reviewer = readback::FixtureReadbackReviewer::authorization_fixture();
    let deadline = Instant::now() + Duration::from_secs(30);
    let reports = loop {
        let reports = evaluate_with_readback(
            &repo,
            &paths,
            &sha,
            Some("Ensure invalid credentials cannot mutate state"),
            std::slice::from_ref(&a),
            &config,
            &reviewer,
        );
        if !reports.iter().any(|report| {
            report
                .outcome
                .artefact
                .detail
                .as_deref()
                .is_some_and(|text| text.contains("control evaluator busy"))
        }) {
            break reports;
        }
        assert!(
            Instant::now() < deadline,
            "control fixtures did not release admission"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(reports.len(), 2);
    let rb_report = reports
        .iter()
        .find(|r| r.name == "contract-readback:a-controls")
        .unwrap();
    let initial_ev = readback_receipt(&paths, rb_report);
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    if containment_available() {
        assert_eq!(
            initial_ev.control_outcome.as_ref().unwrap().status,
            ControlStatus::Verified
        );
    }
    assert!(initial_ev.control_outcome.is_some());
    assert_eq!(
        initial_ev.disposition.status,
        readback::DispositionStatus::Pending
    );

    // Operator compares original claim with read-back and records Accepted
    // disposition (including a synthetic secret in the note to verify export
    // redaction).
    let secret_token = format!("sk-ant-api03-{}", "A".repeat(48));
    let (accepted_ev, accepted_report) = readback::record_disposition(
        &paths,
        &initial_ev,
        readback::DispositionStatus::Accepted,
        "operator@example.com",
        Some(format!(
            "Reviewed exercised/asserted cases against claim; token {secret_token}"
        )),
    )
    .unwrap();
    assert_eq!(
        accepted_ev.disposition.status,
        readback::DispositionStatus::Accepted
    );

    let all_reports = vec![reports[0].clone(), accepted_report.clone()];
    let mut log = crate::event_log::EventLog::acquire(
        &paths,
        &paths.mission_id,
        Duration::ZERO,
        crate::event_log::LockForce::No,
    )
    .unwrap();
    log.append(crate::events::EventKind::MissionCreated {
        goal: "Ensure invalid credentials cannot mutate state".into(),
        base_branch: "main".into(),
        mission_branch: "main".into(),
        config: config.clone(),
    })
    .unwrap();
    log.append(crate::events::EventKind::PlanApproved {
        plan: serde_json::from_value(serde_json::json!({
            "goal": "Ensure invalid credentials cannot mutate state",
            "validationContract": [a],
            "milestones": []
        }))
        .unwrap(),
        base_sha: Some(sha.clone()),
    })
    .unwrap();
    for event in
        crate::gate_results::gate_result_events(crate::gate::GateSurface::Approval, &all_reports)
    {
        log.append(event).unwrap();
    }
    drop(log);

    let logged = crate::event_log::EventLog::read_events(&paths.events_file()).unwrap();
    let state = crate::reducer::fold(&logged).unwrap();
    let packet = crate::review_packet::project(
        &paths.mission_dir(),
        &state,
        &logged,
        Some(&repo),
        chrono::Utc::now(),
    )
    .unwrap();
    assert_eq!(packet.contract_readbacks.len(), 1);
    assert!(packet.contract_readbacks[0].available);
    assert!(packet.contract_readbacks[0].fresh);
    assert_eq!(
        packet.contract_readbacks[0].effective_disposition,
        readback::DispositionStatus::Accepted
    );
    let markdown = crate::review_packet::render_markdown(&packet);
    assert!(markdown.contains("Contract read-back and operator comparison"));
    assert!(markdown.contains("An invalid credential cannot authorize a mutation"));
    assert!(markdown.contains("digest verified"));

    // Changing the assertion statement, command, checker file on disk, or
    // source revision invalidates freshness and prevents the prior Accepted
    // disposition from remaining active.
    let mut changed_stmt = a.clone();
    changed_stmt.statement = "An invalid or expired credential cannot authorize a mutation".into();
    let stale_stmt = readback::reviews(&paths.mission_dir(), &logged, Some(&repo), &[changed_stmt]);
    assert!(!stale_stmt[0].fresh);
    assert_eq!(
        stale_stmt[0].effective_disposition,
        readback::DispositionStatus::Pending
    );

    std::fs::write(repo.root().join("check.sh"), "# modified checker\n").unwrap();
    let stale_disk = readback::reviews(
        &paths.mission_dir(),
        &logged,
        Some(&repo),
        std::slice::from_ref(&a),
    );
    assert!(!stale_disk[0].fresh);
    assert_eq!(
        stale_disk[0].effective_disposition,
        readback::DispositionStatus::Pending
    );
    git(repo.root(), &["restore", "check.sh"]);

    // Export evidence bundle and verify claim, read-back, input identities,
    // control outcomes, operator disposition, secret redaction, and
    // missing-artifact handling.
    let bundle =
        crate::evidence_bundle::assemble_evidence_bundle(repo.root(), &paths.mission_id).unwrap();
    let rb_ref = &accepted_report.outcome.artefact.reference;
    let entry = bundle
        .manifest
        .entries
        .iter()
        .find(|e| &e.source == rb_ref)
        .unwrap();
    assert_eq!(
        entry.status,
        Some(crate::provenance::ArtefactStatus::Resolved)
    );
    let rb_file = bundle
        .files
        .iter()
        .find(|f| Some(&f.path) == entry.path.as_ref())
        .unwrap();
    let rb_text = String::from_utf8_lossy(&rb_file.bytes);
    assert!(!rb_text.contains(&secret_token));
    assert!(rb_text.contains("[REDACTED]"));
    let exported_ev: readback::ReadbackEvidence = serde_json::from_slice(&rb_file.bytes).unwrap();
    assert_eq!(exported_ev.original_claim, a.statement);
    assert_eq!(
        exported_ev.translation.status,
        readback::ReadbackStatus::Complete
    );
    assert_eq!(exported_ev.control_outcome, initial_ev.control_outcome);
    assert_eq!(
        exported_ev.disposition.status,
        readback::DispositionStatus::Accepted
    );

    let summary_md = bundle
        .files
        .iter()
        .find(|f| f.path == crate::evidence_bundle::SUMMARY_FILE)
        .map(|f| String::from_utf8_lossy(&f.bytes).to_string())
        .unwrap();
    assert!(summary_md.contains("## Contract read-back and operator comparison"));
    assert!(summary_md.contains("An invalid credential cannot authorize a mutation"));

    // Deleting or tampering with the retained read-back artifact marks it
    // Unresolved in the bundle and unavailable in the review packet.
    let disk_path = paths
        .mission_dir()
        .join(rb_ref.strip_prefix("file:").unwrap());
    std::fs::remove_file(&disk_path).unwrap();
    let after_delete = readback::reviews(
        &paths.mission_dir(),
        &logged,
        Some(&repo),
        std::slice::from_ref(&a),
    );
    assert!(!after_delete[0].available);
    let bundle_after =
        crate::evidence_bundle::assemble_evidence_bundle(repo.root(), &paths.mission_id).unwrap();
    let entry_after = bundle_after
        .manifest
        .entries
        .iter()
        .find(|e| &e.source == rb_ref)
        .unwrap();
    assert_eq!(
        entry_after.status,
        Some(crate::provenance::ArtefactStatus::Unresolved)
    );
    assert!(entry_after.path.is_none());
}
