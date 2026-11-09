//! Deterministic review-pilot fixture driver. No provider backend is constructed.
//! Run only against a newly prepared scratch case; see scripts/review-effort-pilot.py.
use anyhow::{ensure, Context, Result};
use kranz_engine::auth_verify::AuthVerdict;
use kranz_engine::backend::{AgentBackend, AgentEvent, AgentSession, PromptMode, SessionSpec};
use kranz_engine::backend_mock::{mock_init, mock_result_text, mock_text, MockBackend, MockScript};
use kranz_engine::gate_evaluation::protocol::Digest;
use kranz_engine::orchestrator::{mission_worktree_path, MissionEngine};
use kranz_engine::types::{MissionConfig, MissionStatus, Plan, SandboxEnforce};
use serde_json::{json, Value};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

fn write_json(path: &Path, value: &impl serde::Serialize) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn validate_relative_case_path(rel: &str) -> Result<&Path> {
    ensure!(!rel.is_empty(), "case file path must not be empty");
    let path = Path::new(rel);
    let mut count = 0usize;
    for comp in path.components() {
        match comp {
            Component::Normal(seg) => {
                ensure!(
                    !seg.eq_ignore_ascii_case(".git"),
                    "case file path must not traverse .git: {rel}"
                );
                count += 1;
            }
            _ => anyhow::bail!("case file path must use relative normal components only: {rel}"),
        }
    }
    ensure!(count > 0, "case file path must contain a filename: {rel}");
    Ok(path)
}

fn write_case_change(base: &Path, rel: &str, content: &str) -> Result<()> {
    let rel_path = validate_relative_case_path(rel)?;
    let mut cur = base.to_path_buf();
    let comps: Vec<_> = rel_path.components().collect();
    for (idx, comp) in comps.iter().enumerate() {
        cur.push(comp);
        if let Ok(meta) = std::fs::symlink_metadata(&cur) {
            ensure!(
                !meta.file_type().is_symlink(),
                "refusing to traverse or overwrite symlink in case tree: {}",
                cur.display()
            );
        }
        if idx + 1 < comps.len() {
            std::fs::create_dir_all(&cur)?;
        }
    }
    std::fs::write(&cur, content)?;
    Ok(())
}

fn ensure_pilot_env_with<F>(root: &Path, mut get_var: F) -> Result<PathBuf>
where
    F: FnMut(&str) -> Option<std::ffi::OsString>,
{
    ensure!(root.is_absolute(), "absolute prepared case path required");
    let pilot_root = root.parent().context("case parent")?;
    let home = pilot_root.join("empty-home");
    let docker = pilot_root.join("docker-config");
    let scratch = pilot_root.join("scratch");

    ensure!(
        get_var("HOME").as_deref() == Some(home.as_os_str()),
        "run with the prepared empty HOME"
    );
    ensure!(
        get_var("CLAUDE_CONFIG_DIR").as_deref() == Some(home.join(".claude").as_os_str()),
        "isolated CLI configuration required"
    );
    ensure!(
        get_var("GIT_CONFIG_NOSYSTEM").as_deref() == Some(std::ffi::OsStr::new("1")),
        "GIT_CONFIG_NOSYSTEM=1 required"
    );
    ensure!(
        get_var("GIT_CONFIG_GLOBAL").as_deref() == Some(std::ffi::OsStr::new("/dev/null")),
        "GIT_CONFIG_GLOBAL=/dev/null required"
    );
    ensure!(
        get_var("DOCKER_CONFIG").as_deref() == Some(docker.as_os_str()),
        "isolated DOCKER_CONFIG required"
    );
    ensure!(
        get_var("KRANZ_SCRATCH_ROOT").as_deref() == Some(scratch.as_os_str()),
        "prepared KRANZ_SCRATCH_ROOT required"
    );
    ensure!(
        get_var("TMPDIR").as_deref() == Some(scratch.as_os_str()),
        "prepared TMPDIR required"
    );
    ensure!(
        get_var("KRANZ_SGIAN_BIN").as_deref() == Some(std::ffi::OsStr::new("")),
        "KRANZ_SGIAN_BIN must be empty in pilot environment"
    );
    for key in [
        "ANTHROPIC_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "OPENAI_API_KEY",
    ] {
        ensure!(
            get_var(key).is_none(),
            "provider credentials must be absent ({key})"
        );
    }
    Ok(home)
}

fn ensure_pilot_env(root: &Path) -> Result<PathBuf> {
    ensure_pilot_env_with(root, |k| std::env::var_os(k))
}

fn verify_mock_session_inputs(sessions_dir: &Path, marker: &[u8]) -> Result<Vec<Value>> {
    ensure!(!marker.is_empty(), "transcriptMarker must not be empty");
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(sessions_dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_file()
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("session-") && n.ends_with(".json"))
        {
            paths.push(path);
        }
    }
    paths.sort();

    let mut non_writable = Vec::new();
    for path in paths {
        let bytes = std::fs::read(&path)?;
        let record: Value = serde_json::from_slice(&bytes)?;
        let writable = record["writable"]
            .as_bool()
            .context("session-inputs record missing boolean writable field")?;
        if !writable {
            ensure!(
                !bytes.windows(marker.len()).any(|w| w == marker),
                "worker transcript leaked into non-writable session input {}",
                path.display()
            );
            non_writable.push(json!({
                "path": path,
                "index": record["index"],
                "bytes": bytes.len(),
                "digest": Digest::of(&bytes),
            }));
        }
    }
    ensure!(
        !non_writable.is_empty(),
        "no non-writable session-inputs/session-*.json files recorded"
    );
    Ok(non_writable)
}

// Retain exactly the prompt, system message and schema supplied to each mock.
// Its writes are trusted fixture setup, not a claim of worker OS containment.
struct RecordedMock {
    worker: MockBackend,
    controller: MockBackend,
    reviewers: MockBackend,
    out: PathBuf,
    count: Mutex<usize>,
}

#[async_trait::async_trait]
impl AgentBackend for RecordedMock {
    async fn start(&self, spec: SessionSpec) -> kranz_engine::error::Result<Box<dyn AgentSession>> {
        let count = {
            let mut counter = self.count.lock().unwrap();
            *counter += 1;
            *counter
        };
        let prompt = match &spec.prompt {
            PromptMode::SingleShot(p) | PromptMode::Streaming(p) => p,
        };
        let record = json!({"index":count,"cwd":spec.cwd,"prompt":prompt,
            "system":spec.append_system_prompt,"schema":spec.json_schema,
            "writable":spec.writable,"sandboxResolved":spec.sandbox.is_some(),
            "execution":"MockBackend; no model process or judgment"});
        write_json(&self.out.join(format!("session-{count}.json")), &record)
            .map_err(|e| kranz_engine::error::EngineError::Backend(e.to_string()))?;
        if spec.writable {
            self.worker.start(spec).await
        } else if matches!(spec.prompt, PromptMode::Streaming(_)) {
            self.controller.start(spec).await
        } else {
            self.reviewers.start(spec).await
        }
    }
}

fn zero_usage(mut event: AgentEvent) -> AgentEvent {
    if let AgentEvent::Result {
        usage,
        cost_usd,
        raw,
        ..
    } = &mut event
    {
        *usage = Default::default();
        *cost_usd = Some(0.0);
        raw["total_cost_usd"] = json!(0);
        raw["usage"] = json!({"input_tokens":0,"output_tokens":0,
            "cache_read_input_tokens":0,"cache_creation_input_tokens":0});
    }
    event
}

fn scripts(prepared: &Value) -> Result<Vec<MockScript>> {
    let marker_str = prepared["transcriptMarker"]
        .as_str()
        .context("transcriptMarker must be a string")?;
    ensure!(!marker_str.is_empty(), "transcriptMarker must not be empty");
    let replies = [
        r#"{"action":"commit-as-is","note":"scripted pilot checkpoint"}"#,
        r#"{"decision":"complete","guidance":"","summary":"scripted pilot completion; human review pending"}"#,
        "NONE",
    ];
    let controller = MockScript::streaming(vec![
        mock_init("pilot-controller"),
        mock_result_text("ready"),
    ])
    .responding(
        replies
            .into_iter()
            .map(|r| vec![mock_text(r), mock_result_text(r)])
            .collect(),
    );
    let mut worker = MockScript::single_shot_json(&prepared["report"]);
    worker.events.insert(1, mock_text(marker_str));
    for (path, content) in prepared["writes"]
        .as_object()
        .context("prepared writes must be an object")?
    {
        validate_relative_case_path(path)?;
        worker = worker.writes_file(
            path,
            content.as_str().context("write content must be a string")?,
        );
    }
    let reviewer = || {
        MockScript::single_shot_json(&json!({"findings":[],
        "summary":"Scripted fixture response. Human judgment remains pending."}))
    };
    let mut scripts = vec![controller, worker, reviewer(), reviewer()];
    for script in &mut scripts {
        script.events = std::mem::take(&mut script.events)
            .into_iter()
            .map(zero_usage)
            .collect();
        for reply in &mut script.on_message {
            *reply = std::mem::take(reply).into_iter().map(zero_usage).collect();
        }
    }
    Ok(scripts)
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(root)
        .output()?;
    ensure!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().into())
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() && entry.file_name() != ".git" {
            collect_files(&entry.path(), files)?;
        } else if ty.is_file() && entry.file_name() != ".git" {
            files.push(entry.path());
        }
    }
    Ok(())
}

async fn run(root: &Path) -> Result<()> {
    let home = ensure_pilot_env(root)?;
    let prepared_bytes = std::fs::read(root.join("prepared.json"))?;
    let prepared: Value = serde_json::from_slice(&prepared_bytes)?;
    let marker_str = prepared["transcriptMarker"]
        .as_str()
        .context("transcriptMarker must be a string")?;
    let marker = marker_str.as_bytes();
    ensure!(!marker.is_empty(), "transcriptMarker must not be empty");
    write_json(
        &root.join("attempt.json"),
        &json!({"preparedDigest":Digest::of(&prepared_bytes),
        "startedAt":chrono::Utc::now(),"providerCalls":0,"auth":"test seam, inconclusive; no credential probe"}),
    )?;
    let primary = root.join("primary");
    let mut config = MissionConfig {
        max_respawns: 0,
        max_parallel_workers: 1,
        max_fix_cycles_per_milestone: 1,
        pack_dir: Some("pack".into()),
        ..Default::default()
    };
    config.worker.sandbox.enforce = SandboxEnforce::FsNet;
    config.validator_scrutiny.sandbox.enforce = SandboxEnforce::FsNet;
    config.validator_functional.sandbox.enforce = SandboxEnforce::FsNet;
    kranz_engine::config::validate(&config)?;
    write_json(&root.join("configuration.json"), &config)?;
    // Read-only packet generation reloads current operator configuration.
    // Persist this fixture's policy in its empty HOME so that observation
    // sees the same policy the engine was given directly.
    std::fs::create_dir_all(home.join(".kranz"))?;
    let config_path = home.join(".kranz/config.json");
    if config_path.exists() {
        let existing: MissionConfig = serde_json::from_slice(&std::fs::read(&config_path)?)?;
        ensure!(existing == config, "fixture operator policy changed");
    } else {
        write_json(&config_path, &config)?;
    }
    ensure!(
        kranz_engine::config::load(&primary)? == config,
        "fixture reader/runner policies differ"
    );
    let sessions = root.join("session-inputs");
    std::fs::create_dir(&sessions)?;
    let mut scripts = scripts(&prepared)?;
    let worker = scripts.remove(1);
    let controller = scripts.remove(0);
    let backend = Arc::new(RecordedMock {
        worker: MockBackend::with_scripts(vec![worker]),
        controller: MockBackend::with_scripts(vec![controller]),
        reviewers: MockBackend::with_scripts(scripts),
        out: sessions.clone(),
        count: Mutex::new(0),
    });
    let plan: Plan = serde_json::from_value(prepared["plan"].clone())?;
    let mut engine = MissionEngine::create(backend, &primary, &plan.goal, config.clone())?;
    // Inconclusive suppresses the real auth probe AND scratch credential copying.
    // The only agent implementation in this program is the scripted mock.
    engine.seed_worker_auth_verdict_for_test(AuthVerdict::Inconclusive);
    engine.set_grant_request_cap(0);
    engine.approve_plan(plan.clone())?;
    let paths = engine.paths().clone();
    write_json(
        &root.join("mission.json"),
        &json!({"mission":engine.mission_id(),
        "base":engine.state().mission.base_sha,"branch":engine.state().mission.mission_branch}),
    )?;
    let status = tokio::time::timeout(std::time::Duration::from_secs(600), engine.run()).await??;
    write_json(
        &root.join("engine-result.json"),
        &json!({"status":status,"finishedAt":chrono::Utc::now()}),
    )?;
    ensure!(
        status == MissionStatus::Complete,
        "fixture incomplete: {status:?}"
    );
    let branch = engine.state().mission.mission_branch.clone();
    let worktree = mission_worktree_path(&primary, engine.mission_id());
    if !worktree.exists() {
        git(
            &primary,
            &[
                "worktree",
                "add",
                "--end-of-options",
                worktree.to_str().context("utf-8 worktree path")?,
                &branch,
            ],
        )?;
    }
    // Preserve the actual checked packet before the deliberately later D1 edit.
    write_json(
        &root.join("checked-packet.json"),
        &kranz_engine::review_packet::compute_review_packet(&primary, engine.mission_id())?,
    )?;
    if let Some(edits) = prepared["laterEdits"].as_object() {
        for (path, content) in edits {
            write_case_change(
                &worktree,
                path,
                content
                    .as_str()
                    .context("laterEdit content must be a string")?,
            )?;
        }
    }
    if prepared["case"] == "B1" {
        let mut assertions = plan.validation_contract.clone();
        let control = assertions[0]
            .negative_control
            .as_mut()
            .context("B1 control")?;
        control.checker_files[0].content = format!(
            ". ./missing-checker-dependency.sh\n{}",
            control.checker_files[0].content
        );
        control.baseline_pair = None;
        let variant_root = root.join("setup-variant");
        std::fs::create_dir(&variant_root)?;
        git(&variant_root, &["init", "--template=", "-q", "-b", "main"])?;
        git(&variant_root, &["config", "user.name", "Pilot Fixture"])?;
        git(
            &variant_root,
            &["config", "user.email", "fixture@example.invalid"],
        )?;
        git(&variant_root, &["config", "commit.gpgsign", "false"])?;
        for file in control.checker_files.iter().chain(&control.valid_files) {
            write_case_change(&variant_root, &file.path, &file.content)?;
        }
        git(
            &variant_root,
            &["add", "--end-of-options", "check.sh", "authorization.sh"],
        )?;
        git(
            &variant_root,
            &["commit", "-qm", "freeze broken-dependency checker variant"],
        )?;
        let variant = kranz_engine::paths::MissionPaths::new(&variant_root, "m-b1-setup-variant");
        std::fs::create_dir_all(variant.mission_dir())?;
        let repo = kranz_engine::git_ops::GitRepo::open(&variant_root)?;
        let reports = kranz_engine::contract_controls::evaluate(
            &repo,
            &variant,
            &repo.head_sha()?,
            &assertions,
            &config,
        );
        let mut retained = Vec::new();
        for report in reports {
            let relative = report
                .outcome
                .artefact
                .reference
                .strip_prefix("file:")
                .context("control receipt reference")?;
            let bytes = std::fs::read(variant.mission_dir().join(relative))?;
            let receipt: Value = serde_json::from_slice(&bytes)?;
            ensure!(
                receipt["status"] == "inconclusive",
                "missing dependency must be inconclusive"
            );
            ensure!(
                receipt["valid"]["outputTail"]
                    .as_str()
                    .is_some_and(|s| s.contains("missing-checker-dependency.sh")),
                "variant must actually run and expose its missing dependency: {receipt}"
            );
            retained.push(
                json!({"name":report.name,"receiptReference":report.outcome.artefact.reference,
                "receiptDigest":Digest::of(&bytes),"receipt":receipt}),
            );
        }
        ensure!(!retained.is_empty(), "no setup-variant evidence");
        write_json(&root.join("setup-variant-reports.json"), &retained)?;
    }
    let packet = kranz_engine::review_packet::compute_review_packet(&primary, engine.mission_id())?;
    write_json(&root.join("review-packet.json"), &packet)?;
    std::fs::write(
        root.join("review-packet.md"),
        kranz_engine::review_packet::render_markdown(&packet),
    )?;
    write_json(
        &root.join("outcomes.json"),
        &kranz_engine::outcomes::compute_outcomes(&primary)?,
    )?;
    kranz_engine::evidence_bundle::export_evidence_bundle(
        &primary,
        engine.mission_id(),
        &root.join("export"),
    )?;
    let baseline = prepared["baseline"]
        .as_str()
        .context("prepared baseline must be a string")?;
    std::fs::write(
        root.join("candidate.diff"),
        git(
            &worktree,
            &[
                "diff",
                "--no-ext-diff",
                "--binary",
                "--end-of-options",
                baseline,
            ],
        )?,
    )?;
    let mut files = Vec::new();
    collect_files(&paths.runs_dir(), &mut files)?;
    let mut inventory = Vec::new();
    for event in kranz_engine::event_log::EventLog::read_events(&paths.events_file())? {
        if let kranz_engine::events::EventKind::GateEvaluationRequested { evaluation } = event.kind
        {
            for input in evaluation.retained_inputs {
                let path = paths.mission_dir().join(input.path.as_str());
                let bytes = std::fs::read(&path)?;
                ensure!(
                    Digest::of(&bytes) == input.retained_digest
                        && input.raw_digest == input.retained_digest,
                    "fixture input changed during retention; cannot infer raw input contents"
                );
                ensure!(
                    !bytes.windows(marker.len()).any(|w| w == marker),
                    "worker transcript leaked into gate inputs"
                );
                inventory
                    .push(json!({"path":path,"bytes":bytes.len(),"digest":Digest::of(&bytes)}));
            }
        }
    }
    ensure!(!inventory.is_empty(), "no retained evaluator inputs");
    ensure!(
        files
            .iter()
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .any(|p| std::fs::read(p).is_ok_and(|b| b.windows(marker.len()).any(|w| w == marker))),
        "worker transcript marker was never recorded"
    );
    let non_writable_sessions = verify_mock_session_inputs(&sessions, marker)?;
    write_json(
        &root.join("mock-review-input-verification.json"),
        &json!({
            "markerAbsent": true,
            "workerTranscriptMarkerPresent": true,
            "nonWritableSessions": non_writable_sessions,
            "limits": "Mock controller and reviewer session inputs verified free of worker transcript marker; mock sessions remain scripted, not independent model judgment.",
        }),
    )?;
    write_json(
        &root.join("validator-input-verification.json"),
        &json!({"markerAbsent":true,
        "workerTranscriptMarkerPresent":true,"retainedInputs":inventory,
        "limits":"Fresh external checker inputs verified; mock reviewers are scripted, not independent model judgment."}),
    )?;
    write_json(
        &root.join("execution-complete.json"),
        &json!({"candidate":git(&worktree,&["rev-parse","--end-of-options","HEAD"])?,
        "worktree":worktree,"humanDecision":null,"localMerge":false,"productRelease":false}),
    )?;
    println!(
        "Prepared review packet: {}",
        root.join("review-packet.md").display()
    );
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let root = std::env::args_os()
        .nth(1)
        .context("usage: review_effort_pilot ABSOLUTE_PREPARED_CASE")?;
    run(Path::new(&root)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;

    #[test]
    fn write_case_change_accepts_normal_relative_paths_and_rejects_traversal_or_git() {
        let tmp = tempfile::tempdir().unwrap();
        write_case_change(tmp.path(), "nested/dir/file.txt", "hello").unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("nested/dir/file.txt")).unwrap(),
            "hello"
        );

        for bad in [
            "",
            "/abs.txt",
            "../escape.txt",
            "a/../b.txt",
            "./cur.txt",
            ".git/config",
            "sub/.GIT/hooks",
        ] {
            assert!(
                write_case_change(tmp.path(), bad, "nope").is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn ensure_pilot_env_matches_preparer_contract() {
        let tmp = tempfile::tempdir().unwrap();
        let case_dir = tmp.path().join("D1");
        let home = tmp.path().join("empty-home");
        let mut env: HashMap<String, OsString> = HashMap::from([
            ("HOME".into(), home.clone().into_os_string()),
            (
                "CLAUDE_CONFIG_DIR".into(),
                home.join(".claude").into_os_string(),
            ),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            (
                "DOCKER_CONFIG".into(),
                tmp.path().join("docker-config").into_os_string(),
            ),
            (
                "KRANZ_SCRATCH_ROOT".into(),
                tmp.path().join("scratch").into_os_string(),
            ),
            ("TMPDIR".into(), tmp.path().join("scratch").into_os_string()),
            ("KRANZ_SGIAN_BIN".into(), "".into()),
        ]);

        assert_eq!(
            ensure_pilot_env_with(&case_dir, |k| env.get(k).cloned()).unwrap(),
            home
        );

        env.insert("OPENAI_API_KEY".into(), "sk-secret".into());
        assert!(ensure_pilot_env_with(&case_dir, |k| env.get(k).cloned()).is_err());
        env.remove("OPENAI_API_KEY");
        env.remove("DOCKER_CONFIG");
        assert!(ensure_pilot_env_with(&case_dir, |k| env.get(k).cloned()).is_err());
    }

    #[test]
    fn verify_mock_session_inputs_rejects_empty_or_leaked_marker() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(verify_mock_session_inputs(tmp.path(), b"").is_err());

        write_json(
            &tmp.path().join("session-1.json"),
            &json!({"index": 1, "writable": true, "prompt": "PILOT-MARKER-123"}),
        )
        .unwrap();
        write_json(
            &tmp.path().join("session-2.json"),
            &json!({"index": 2, "writable": false, "prompt": "clean reviewer prompt"}),
        )
        .unwrap();

        let ok = verify_mock_session_inputs(tmp.path(), b"PILOT-MARKER-123").unwrap();
        assert_eq!(ok.len(), 1);

        write_json(
            &tmp.path().join("session-3.json"),
            &json!({"index": 3, "writable": false, "prompt": "leaked PILOT-MARKER-123"}),
        )
        .unwrap();
        assert!(verify_mock_session_inputs(tmp.path(), b"PILOT-MARKER-123").is_err());
    }
}
