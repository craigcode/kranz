use super::*;
use crate::contract_controls::pair::{ExpectedOutcome, PairSpec};

fn pair_spec(base: &str) -> PairSpec {
    PairSpec {
        baseline_revision: base.into(),
        expected_baseline: ExpectedOutcome::Failed {
            failure_id: "invalid-credential-authorized".into(),
        },
        expected_candidate: ExpectedOutcome::Passed,
        environment_label: "native fixture; no external services".into(),
        overlay_checker_on_baseline: true,
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn pair_fixture() -> (tempfile::TempDir, GitRepo, MissionPaths, String, Assertion) {
    let (dir, repo, paths, _) = fixture(CHECKER);
    std::fs::write(repo.root().join("authorization.sh"), DEFECTIVE).unwrap();
    git(repo.root(), &["rm", "check.sh"]);
    git(
        repo.root(),
        &["commit", "-am", "actual bug without regression check"],
    );
    let baseline = repo.head_sha().unwrap();
    std::fs::write(repo.root().join("authorization.sh"), VALID).unwrap();
    std::fs::write(repo.root().join("check.sh"), CHECKER).unwrap();
    git(repo.root(), &["add", "authorization.sh", "check.sh"]);
    git(repo.root(), &["commit", "-m", "fix and regression check"]);
    let candidate = repo.head_sha().unwrap();
    let mut assertion = assertion(CHECKER);
    assertion.negative_control.as_mut().unwrap().baseline_pair = Some(pair_spec(&baseline));
    (dir, repo, paths, candidate, assertion)
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn events(reports: &[GateReport]) -> Vec<crate::events::Event> {
    crate::gate_results::gate_result_events(crate::gate::GateSurface::FinalGate, reports)
        .into_iter()
        .enumerate()
        .map(|(i, kind)| crate::events::Event {
            seq: i as u64 + 1,
            ts: chrono::Utc::now(),
            mission_id: "m-controls".into(),
            kind,
        })
        .collect()
}

#[test]
fn baseline_pair_contract_round_trip_and_expectations_fail_closed() {
    let old = assertion(CHECKER);
    let bytes = serde_json::to_vec(&old).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("baselinePair"));
    assert_eq!(
        serde_json::to_vec(&serde_json::from_slice::<Assertion>(&bytes).unwrap()).unwrap(),
        bytes
    );
    let mut selected = old.clone();
    selected.negative_control.as_mut().unwrap().baseline_pair = Some(pair_spec(&"a".repeat(40)));
    validate(&[selected.clone()]).unwrap();
    let encoded = serde_json::to_value(&selected).unwrap();
    for (field, value) in [
        ("baselineRevision", serde_json::json!("main")),
        ("baselineRevision", serde_json::json!("a".repeat(39))),
        ("environmentLabel", serde_json::json!("")),
        (
            "expectedBaseline",
            serde_json::json!({"outcome":"failed", "failureId":""}),
        ),
        (
            "expectedBaseline",
            serde_json::json!({"outcome":"diagnostic", "failureId":"compile", "diagnostic":""}),
        ),
    ] {
        let mut invalid = encoded.clone();
        invalid["negativeControl"]["baselinePair"][field] = value;
        assert!(
            validate(&[serde_json::from_value(invalid).unwrap()]).is_err(),
            "{field}"
        );
    }
    let passed = ExpectedOutcome::Passed;
    let failed = ExpectedOutcome::Failed {
        failure_id: "wanted".into(),
    };
    let diagnostic = ExpectedOutcome::Diagnostic {
        failure_id: "wanted".into(),
        diagnostic: "E0425".into(),
    };
    let mut case = CaseEvidence {
        exit_code: Some(1),
        receipt: Some(CheckReceipt {
            checks_run: 1,
            outcome: CheckOutcome::Failed,
            failure_id: Some("wanted".into()),
            diagnostic: None,
        }),
        output_tail: String::new(),
        elapsed_ms: 0,
        environment_names: vec![],
        binding: None,
    };
    assert!(failed.matches(&case));
    assert!(!passed.matches(&case));
    assert!(!diagnostic.matches(&case));
    for wrong in [
        "missing-dependency",
        "authentication-required",
        "unrelated-compiler-error",
    ] {
        case.receipt.as_mut().unwrap().failure_id = Some(wrong.into());
        assert!(!failed.matches(&case));
    }
    case.receipt.as_mut().unwrap().failure_id = Some("wanted".into());
    case.receipt.as_mut().unwrap().diagnostic = Some("E0425".into());
    assert!(diagnostic.matches(&case));
    assert!(!failed.matches(&case));
    for wrong in [
        "missing-dependency",
        "authentication-required",
        "unrelated-compiler-error",
    ] {
        case.receipt.as_mut().unwrap().diagnostic = Some(wrong.into());
        assert!(!diagnostic.matches(&case));
    }
    case.receipt.as_mut().unwrap().diagnostic = None;
    case.receipt.as_mut().unwrap().checks_run = 0;
    assert!(!failed.matches(&case));
    case.receipt.as_mut().unwrap().checks_run = 1;
    case.exit_code = None;
    assert!(!failed.matches(&case));
    case.exit_code = Some(1);
    case.receipt = None;
    assert!(!failed.matches(&case));
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn baseline_pair_actual_revisions_overlay_and_tamper_evidence() {
    if !containment_available() {
        return;
    }
    let _environment = crate::agent_env::ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_dir, repo, paths, candidate, assertion) = pair_fixture();
    let assertions = [assertion];
    let config = MissionConfig::default();
    let before =
        crate::gate_evaluation::snapshot::SourceSnapshot::capture(&repo, &candidate).unwrap();
    let reports = evaluate_available(&repo, &paths, &candidate, &assertions, &config);
    assert_eq!(reports.len(), 2);
    let evidence = receipt(&paths, &reports[0]);
    let pair = evidence.baseline_pair.as_ref().unwrap();
    assert_eq!(
        pair.status,
        ControlStatus::Verified,
        "{} {:?}",
        pair.detail,
        pair.baseline
    );
    assert_eq!(pair.baseline.as_ref().unwrap().exit_code, Some(1));
    assert_eq!(pair.candidate.as_ref().unwrap().exit_code, Some(0));
    assert_eq!(
        pair.checker_overlay_sha256.as_ref(),
        Some(&evidence.checker_sha256)
    );
    assert_ne!(
        pair.baseline
            .as_ref()
            .unwrap()
            .binding
            .as_ref()
            .unwrap()
            .source
            .inventory_digest,
        pair.candidate
            .as_ref()
            .unwrap()
            .binding
            .as_ref()
            .unwrap()
            .source
            .inventory_digest
    );
    before.verify_current(&repo).unwrap();
    assert!(repo.is_clean_tracked_strict().unwrap());
    assert_eq!(repo.list_worktrees().unwrap().len(), 1);

    let events = events(&reports);
    let review = pair::reviews(
        &paths.mission_dir(),
        &events,
        Some(&repo),
        &assertions,
        &config,
    );
    assert!(review[0].available);
    assert!(review[0].source_and_config_match, "{}", review[0].detail);
    let diagnostics = pair::diagnostics(&paths.mission_dir(), &events, &repo, &assertions, &config);
    assert_eq!(diagnostics.len(), 1);
    assert!(pair::descriptor(diagnostics[0].outcome.artefact.detail.as_deref()).is_some());
    let mut unavailable = events.clone();
    let mut last = events[0].clone();
    last.seq = 3;
    if let crate::events::EventKind::GateResult {
        artefact_detail,
        artefact_ref,
        verdict,
        ..
    } = &mut last.kind
    {
        *artefact_detail = Some("INCONCLUSIVE: could not retain evidence".into());
        *artefact_ref = "baseline/candidate evidence unavailable".into();
        *verdict = crate::gate::GateVerdict::Fail;
    }
    unavailable.push(last);
    assert!(
        pair::diagnostics(
            &paths.mission_dir(),
            &unavailable,
            &repo,
            &assertions,
            &config
        )
        .is_empty(),
        "a newer unavailable observation must not fall back to an older pass"
    );
    let mutable_revision = evaluate_available(&repo, &paths, "HEAD", &assertions, &config);
    assert_eq!(
        receipt(&paths, &mutable_revision[0])
            .baseline_pair
            .unwrap()
            .status,
        ControlStatus::Inconclusive
    );
    let mut changed_config = config.clone();
    changed_config.worker.sandbox.enforce = SandboxEnforce::FsNet;
    assert!(
        !pair::reviews(
            &paths.mission_dir(),
            &events,
            Some(&repo),
            &assertions,
            &changed_config
        )[0]
        .source_and_config_match
    );
    for name in ["check.sh", "authorization.sh", "new-source.txt"] {
        std::fs::write(repo.root().join(name), "later edit").unwrap();
        assert!(
            !pair::reviews(
                &paths.mission_dir(),
                &events,
                Some(&repo),
                &assertions,
                &config
            )[0]
            .source_and_config_match,
            "{name}"
        );
        if name == "new-source.txt" {
            std::fs::remove_file(repo.root().join(name)).unwrap();
        } else {
            git(repo.root(), &["restore", name]);
        }
    }
    let path = paths.mission_dir().join(
        reports[0]
            .outcome
            .artefact
            .reference
            .strip_prefix("file:")
            .unwrap(),
    );
    let original = std::fs::read(&path).unwrap();
    {
        use crate::event_log::{EventLog, LockForce};
        use crate::events::EventKind;
        let mut log =
            EventLog::acquire(&paths, "m-controls", Duration::ZERO, LockForce::No).unwrap();
        log.append(EventKind::MissionCreated {
            goal: "pair evidence".into(),
            base_branch: "main".into(),
            mission_branch: "main".into(),
            config: config.clone(),
        })
        .unwrap();
        log.append(EventKind::PlanApproved {
            plan: serde_json::from_value(serde_json::json!({
                "goal": "pair evidence", "validationContract": assertions, "milestones": []
            }))
            .unwrap(),
            base_sha: Some(pair.spec.baseline_revision.clone()),
        })
        .unwrap();
        for event in &events {
            log.append(event.kind.clone()).unwrap();
        }
    }
    let exported_status = || {
        let bundle =
            crate::evidence_bundle::assemble_evidence_bundle(repo.root(), "m-controls").unwrap();
        bundle
            .manifest
            .entries
            .iter()
            .find(|e| e.source == reports[0].outcome.artefact.reference)
            .unwrap()
            .status
            .unwrap()
    };
    assert_eq!(
        exported_status(),
        crate::provenance::ArtefactStatus::Resolved
    );
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
    assert!(packet.baseline_candidates[0].source_and_config_match);
    let markdown = crate::review_packet::render_markdown(&packet);
    assert!(markdown.contains("Baseline and candidate observations"));
    assert!(markdown.contains("digest verified"));
    std::fs::write(&path, b"{}").unwrap();
    assert!(
        !pair::reviews(
            &paths.mission_dir(),
            &events,
            Some(&repo),
            &assertions,
            &config
        )[0]
        .available
    );
    assert_eq!(
        exported_status(),
        crate::provenance::ArtefactStatus::Unresolved
    );
    std::fs::remove_file(&path).unwrap();
    assert!(
        !pair::reviews(
            &paths.mission_dir(),
            &events,
            Some(&repo),
            &assertions,
            &config
        )[0]
        .available
    );
    assert_eq!(
        exported_status(),
        crate::provenance::ArtefactStatus::Unresolved
    );
    #[cfg(unix)]
    {
        let other = path.with_extension("other");
        std::fs::write(&other, original).unwrap();
        std::os::unix::fs::symlink(&other, &path).unwrap();
        assert!(
            !pair::reviews(
                &paths.mission_dir(),
                &events,
                Some(&repo),
                &assertions,
                &config
            )[0]
            .available
        );
        std::fs::remove_file(&path).unwrap();
        std::fs::hard_link(&other, &path).unwrap();
        assert!(
            !pair::reviews(
                &paths.mission_dir(),
                &events,
                Some(&repo),
                &assertions,
                &config
            )[0]
            .available
        );
    }
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn baseline_pair_compile_time_assertion_requires_exact_diagnostic_and_positive_control() {
    if !containment_available() {
        return;
    }
    let _environment = crate::agent_env::ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let checker = r#"set -eu
if rustc --edition=2021 --crate-type lib --error-format=json api.rs -o "$KRANZ_CONTROL_SCRATCH/lib.rlib" 2> "$KRANZ_CONTROL_SCRATCH/diagnostics"; then
  printf '%s' '{"checksRun":1,"outcome":"passed"}' > "$KRANZ_CONTROL_RESULT"
elif grep -F 'cannot find value `missing_answer` in this scope' "$KRANZ_CONTROL_SCRATCH/diagnostics" | grep -q '"code":"E0425"'; then
  printf '%s' '{"checksRun":1,"outcome":"failed","failureId":"missing-answer","diagnostic":"E0425: cannot find value missing_answer in this scope"}' > "$KRANZ_CONTROL_RESULT"
  exit 1
else
  cat "$KRANZ_CONTROL_SCRATCH/diagnostics"
  exit 7
fi
"#;
    let valid = "pub fn answer() -> u32 { 42 }\n";
    let defective = "pub fn answer() -> u32 { missing_answer }\n";
    let (_dir, repo, paths, _) = fixture(checker);
    std::fs::write(repo.root().join("api.rs"), defective).unwrap();
    git(repo.root(), &["add", "api.rs"]);
    git(repo.root(), &["commit", "-m", "baseline API defect"]);
    let baseline = repo.head_sha().unwrap();
    std::fs::write(repo.root().join("api.rs"), valid).unwrap();
    git(repo.root(), &["commit", "-am", "candidate API fix"]);
    let candidate = repo.head_sha().unwrap();
    let mut selected = assertion(checker);
    let control = selected.negative_control.as_mut().unwrap();
    control.valid_files = vec![ControlFile {
        path: "api.rs".into(),
        content: valid.into(),
    }];
    control.defective_files = vec![ControlFile {
        path: "api.rs".into(),
        content: defective.into(),
    }];
    control.expected_failure = "missing-answer".into();
    control.timeout_seconds = 15;
    let mut spec = pair_spec(&baseline);
    spec.expected_baseline = ExpectedOutcome::Diagnostic {
        failure_id: "missing-answer".into(),
        diagnostic: "E0425: cannot find value missing_answer in this scope".into(),
    };
    control.baseline_pair = Some(spec);
    let reports = evaluate_available(
        &repo,
        &paths,
        &candidate,
        &[selected.clone()],
        &MissionConfig::default(),
    );
    let evidence = receipt(&paths, &reports[0]);
    assert_eq!(
        evidence.status,
        ControlStatus::Verified,
        "{} {:?}",
        evidence.detail,
        evidence.valid
    );
    let pair = evidence.baseline_pair.unwrap();
    assert_eq!(
        pair.status,
        ControlStatus::Verified,
        "{} {:?}",
        pair.detail,
        pair.baseline
    );
    assert_eq!(
        pair.baseline
            .unwrap()
            .receipt
            .unwrap()
            .diagnostic
            .as_deref(),
        Some("E0425: cannot find value missing_answer in this scope")
    );
    std::fs::write(
        repo.root().join("api.rs"),
        "pub fn answer() -> u32 { another_missing_symbol }\n",
    )
    .unwrap();
    git(
        repo.root(),
        &[
            "commit",
            "-am",
            "unrelated error with the same compiler code",
        ],
    );
    let unrelated = repo.head_sha().unwrap();
    std::fs::write(repo.root().join("api.rs"), valid).unwrap();
    git(
        repo.root(),
        &["commit", "-am", "candidate after unrelated baseline"],
    );
    selected
        .negative_control
        .as_mut()
        .unwrap()
        .baseline_pair
        .as_mut()
        .unwrap()
        .baseline_revision = unrelated;
    let reports = evaluate_available(
        &repo,
        &paths,
        &repo.head_sha().unwrap(),
        &[selected],
        &MissionConfig::default(),
    );
    let evidence = receipt(&paths, &reports[0]);
    assert_eq!(evidence.status, ControlStatus::Verified);
    let pair = evidence.baseline_pair.unwrap();
    assert_eq!(pair.status, ControlStatus::Inconclusive);
    assert!(pair.baseline.unwrap().receipt.is_none());
}

#[test]
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn baseline_pair_parity_and_broken_checker_do_not_change_policy() {
    if !containment_available() {
        return;
    }
    let _environment = crate::agent_env::ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let (_dir, repo, paths, sha) = fixture(CHECKER);
    let mut assertion = assertion(CHECKER);
    let mut spec = pair_spec(&sha);
    spec.expected_baseline = ExpectedOutcome::Passed;
    spec.overlay_checker_on_baseline = false;
    assertion.negative_control.as_mut().unwrap().baseline_pair = Some(spec);
    let config = MissionConfig::default();
    let reports = evaluate_available(&repo, &paths, &sha, &[assertion.clone()], &config);
    let evidence = receipt(&paths, &reports[0]);
    assert_eq!(
        evidence.baseline_pair.unwrap().status,
        ControlStatus::Verified
    );
    let broken = "printf '%s' '{\"checksRun\":1,\"outcome\":\"failed\",\"failureId\":\"invalid-credential-authorized\"}' > \"$KRANZ_CONTROL_RESULT\"; exit 1\n";
    std::fs::write(repo.root().join("check.sh"), broken).unwrap();
    git(repo.root(), &["commit", "-am", "broken checker"]);
    assertion.negative_control.as_mut().unwrap().checker_files[0].content = broken.into();
    let reports = evaluate_available(
        &repo,
        &paths,
        &repo.head_sha().unwrap(),
        &[assertion],
        &config,
    );
    let evidence = receipt(&paths, &reports[0]);
    assert_eq!(evidence.status, ControlStatus::Inconclusive);
    let pair = evidence.baseline_pair.unwrap();
    assert_eq!(pair.status, ControlStatus::Inconclusive);
    assert!(pair.baseline.is_none() && pair.candidate.is_none());
}

#[test]
fn baseline_pair_environment_identity_tracks_values_and_normalizes_only_case_context() {
    let _environment = crate::agent_env::ENV_TEST_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let config = MissionConfig::default();
    let root = tempfile::tempdir().unwrap();
    let first_path = root.path().join("a");
    let second_path = root.path().join("b");
    let a = first_path.as_path();
    let b = second_path.as_path();
    let first = pair::case_environment(&config, a, "revision-a");
    let mut second = pair::case_environment(&config, b, "revision-b");
    assert_eq!(
        pair::environment_identity(&config, a, &first),
        pair::environment_identity(&config, b, &second)
    );
    second.insert("REVIEW_FIXTURE_VALUE".into(), "different".into());
    assert_ne!(
        pair::environment_identity(&config, a, &first),
        pair::environment_identity(&config, b, &second)
    );
}
