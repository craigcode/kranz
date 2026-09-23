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
use std::path::{Path, PathBuf};
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

fn scripts(prepared: &Value) -> Vec<MockScript> {
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
    worker
        .events
        .insert(1, mock_text(prepared["transcriptMarker"].as_str().unwrap()));
    for (path, content) in prepared["writes"].as_object().unwrap() {
        worker = worker.writes_file(path, content.as_str().unwrap());
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
    scripts
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
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
    ensure!(root.is_absolute(), "absolute prepared case path required");
    let home = root.parent().context("case parent")?.join("empty-home");
    ensure!(
        std::env::var_os("HOME").as_deref() == Some(home.as_os_str()),
        "run with the prepared empty HOME"
    );
    ensure!(
        std::env::var_os("CLAUDE_CONFIG_DIR").as_deref() == Some(home.join(".claude").as_os_str()),
        "isolated CLI configuration required"
    );
    for key in [
        "ANTHROPIC_API_KEY",
        "CLAUDE_CODE_OAUTH_TOKEN",
        "OPENAI_API_KEY",
    ] {
        ensure!(
            std::env::var_os(key).is_none(),
            "provider credentials must be absent"
        );
    }
    let prepared_bytes = std::fs::read(root.join("prepared.json"))?;
    let prepared: Value = serde_json::from_slice(&prepared_bytes)?;
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
    let mut scripts = scripts(&prepared);
    let worker = scripts.remove(1);
    let controller = scripts.remove(0);
    let backend = Arc::new(RecordedMock {
        worker: MockBackend::with_scripts(vec![worker]),
        controller: MockBackend::with_scripts(vec![controller]),
        reviewers: MockBackend::with_scripts(scripts),
        out: sessions,
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
            &["worktree", "add", worktree.to_str().unwrap(), &branch],
        )?;
    }
    // Preserve the actual checked packet before the deliberately later D1 edit.
    write_json(
        &root.join("checked-packet.json"),
        &kranz_engine::review_packet::compute_review_packet(&primary, engine.mission_id())?,
    )?;
    if let Some(edits) = prepared["laterEdits"].as_object() {
        for (path, content) in edits {
            std::fs::write(worktree.join(path), content.as_str().unwrap())?;
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
            std::fs::write(variant_root.join(&file.path), &file.content)?;
        }
        git(&variant_root, &["add", "check.sh", "authorization.sh"])?;
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
    std::fs::write(
        root.join("candidate.diff"),
        git(
            &worktree,
            &[
                "diff",
                "--no-ext-diff",
                "--binary",
                prepared["baseline"].as_str().unwrap(),
            ],
        )?,
    )?;
    let marker = prepared["transcriptMarker"].as_str().unwrap().as_bytes();
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
    write_json(
        &root.join("validator-input-verification.json"),
        &json!({"markerAbsent":true,
        "workerTranscriptMarkerPresent":true,"retainedInputs":inventory,
        "limits":"Fresh external checker inputs verified; mock reviewers are scripted, not independent model judgment."}),
    )?;
    write_json(
        &root.join("execution-complete.json"),
        &json!({"candidate":git(&worktree,&["rev-parse","HEAD"])?,
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
