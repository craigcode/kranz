//! Opt-in live acceptance fixture. This is not a production controller.
//! One real ACP worker; explicitly scripted controller/reviewer; real gates.
use anyhow::{bail, ensure, Context, Result};
use kranz_engine::acp_worker::{AcpWorkerProfile, CLAUDE, CODEX, IMAGE};
use kranz_engine::backend::{AgentBackend, AgentEvent, AgentSession, SessionExit, SessionSpec};
use kranz_engine::backend_mock::{mock_init, mock_result_text, mock_text, MockBackend, MockScript};
use kranz_engine::event_log::EventLog;
use kranz_engine::events::EventKind;
use kranz_engine::gate_evaluation::protocol::{Digest, Stage, Subject};
use kranz_engine::live_permission::{Actor, Delivery, Resolution};
use kranz_engine::orchestrator::MissionEngine;
use kranz_engine::types::{
    ControlCommand, MissionConfig, MissionStatus, Plan, Role, SandboxConfig, SandboxEnforce,
    SandboxProvider,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

// Share the already qualified exact command and conservative permission matcher.
#[allow(dead_code)]
#[path = "acp_compat_probe/tool_fixture.rs"]
mod tool_fixture;

const FILE: &str = "fixture-result.txt";
const OUTPUT: &[u8] = b"kranz-acp-tool-fixture-v1\n";
const CHECK: &str = r#"/usr/local/bin/python3 -c "from pathlib import Path; assert Path('fixture-result.txt').read_bytes() == b'kranz-acp-tool-fixture-v1\n'; print('1 assertion passed')""#;
const GOAL: &str = "Run the fixed ACP governed-mission acceptance fixture";
const CHECKER_IMAGE: &str =
    "python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Preparation {
    provider: String,
    credential_file: PathBuf,
    executable_digest: Digest,
    plan: Plan,
    config: MissionConfig,
    command: String,
    worker_sessions: u32,
    worker_seconds: u64,
    mission_seconds: u64,
    overall_seconds: u64,
    hard_dollar_cap: Option<f64>,
    controller_and_reviewer: String,
}

fn configuration(provider: &str, credential_file: PathBuf) -> Result<MissionConfig> {
    let (id, egress) = match provider {
        "claude" => (CLAUDE, vec!["api.anthropic.com:443", "claude.ai:443"]),
        "codex" => (
            CODEX,
            vec![
                "chatgpt.com:443",
                "auth.openai.com:443",
                "api.openai.com:443",
            ],
        ),
        _ => bail!("expected claude or codex"),
    };
    let mut cfg = MissionConfig {
        allow_below_default_worker_model: true,
        max_respawns: 0,
        // The config floor is one; the fixture controller refuses the
        // conversion turn before the engine can synthesize a repair plan.
        max_fix_cycles_per_milestone: 1,
        max_parallel_workers: 1,
        validator_allow_uncontained_degrade: true,
        pack_dir: Some("pack".into()),
        ..Default::default()
    };
    cfg.worker.backend = Some("acp".into());
    cfg.worker.max_budget_usd = None;
    cfg.worker.max_turns = Some(4);
    cfg.worker.acp_profile = Some(AcpWorkerProfile {
        id: id.into(),
        credential_file,
    });
    cfg.worker.sandbox = SandboxConfig {
        provider: SandboxProvider::Container,
        enforce: SandboxEnforce::FsNet,
        image: Some(IMAGE.into()),
        egress: egress.into_iter().map(String::from).collect(),
        ..Default::default()
    };
    kranz_engine::config::validate(&cfg)?;
    Ok(cfg)
}

fn plan() -> Result<Plan> {
    let report: kranz_engine::types::WorkerReport = serde_json::from_value(
        json!({"result":"pass","summary":"kranz-acp-governed-fixture-v1",
        "filesTouched":[FILE],"testsAdded":[],"dependenciesAdded":[],"knownGaps":[],
        "commits":[],"commandsRun":[tool_fixture::COMMAND],"escalation":null,"questions":[]}),
    )?;
    let report = serde_json::to_string(&report)?;
    let spec = format!(
        "Bounded acceptance fixture. Use your native shell tool exactly once to run exactly this command in the session working directory: {}\nRequest explicit one-time approval before executing it (Codex: require_escalated with no prefix rule). Do not run preliminary commands, read files, use other tools, change modes, add permissions, call the network, retry or create commits. The host owns checks and checkpointing. If approval or execution fails, stop and report failure. After successful execution return exactly this JSON, without Markdown fences:\n{report}",
        tool_fixture::COMMAND
    );
    Ok(serde_json::from_value(
        json!({"goal":GOAL,"touchSet":[FILE],
            "validationContract":[{"id":"a-1","statement":"fixed fixture file delivered","check":"command","command":CHECK}],
            "milestones":[{"title":"fixed delivery","features":[{"title":"write fixture file","spec":spec,"validationCriteria":["fixed fixture file delivered"]}]}]
        }),
    )?)
}

fn write_new(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn prepare(provider: &str, credential: &Path, root: &Path) -> Result<Digest> {
    ensure!(
        root.is_absolute() && credential.is_absolute(),
        "absolute paths required"
    );
    ensure!(
        !credential.starts_with(root),
        "credential must be outside fixture"
    );
    let prepared = Preparation {
        provider: provider.into(),
        credential_file: credential.into(),
        executable_digest: Digest::of(&std::fs::read(std::env::current_exe()?)?),
        plan: plan()?,
        config: configuration(provider, credential.into())?,
        command: tool_fixture::COMMAND.into(),
        worker_sessions: 1,
        worker_seconds: 120,
        mission_seconds: 240,
        overall_seconds: 480,
        hard_dollar_cap: None,
        controller_and_reviewer: "scripted fixture; no live model judgment".into(),
    };
    // Preparation reads neither the credential nor a provider's configuration.
    std::fs::create_dir(root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
    }
    write_new(&root.join("preparation.json"), &prepared)?;
    Ok(Digest::of(&std::fs::read(root.join("preparation.json"))?))
}

fn consume(root: &Path, authorized_digest: &str) -> Result<Preparation> {
    let bytes = std::fs::read(root.join("preparation.json"))?;
    ensure!(
        Digest::of(&bytes).as_str() == authorized_digest,
        "preparation changed"
    );
    let prepared: Preparation = serde_json::from_slice(&bytes)?;
    ensure!(
        prepared.executable_digest == Digest::of(&std::fs::read(std::env::current_exe()?)?),
        "runner changed"
    );
    ensure!(
        prepared.command == tool_fixture::COMMAND
            && prepared.worker_sessions == 1
            && prepared.worker_seconds == 120
            && prepared.mission_seconds == 240
            && prepared.overall_seconds == 480
            && prepared.hard_dollar_cap.is_none(),
        "unsupported bounds"
    );
    ensure!(
        serde_json::to_value(&prepared.plan)? == serde_json::to_value(plan()?)?,
        "plan changed"
    );
    ensure!(
        prepared.config == configuration(&prepared.provider, prepared.credential_file.clone())?,
        "config changed"
    );
    // Consume before repository creation, credential reads or adapter startup.
    write_new(
        &root.join("attempt.json"),
        &json!({"authorizedPreparation":authorized_digest,
        "startedAt":chrono::Utc::now(),"retries":0,"keychain":false}),
    )?;
    Ok(prepared)
}

fn git(root: &Path, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git")
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .args([
            "-c",
            "user.name=ACP Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(root)
        .output()?;
    ensure!(out.status.success(), "fixture git command failed");
    Ok(String::from_utf8(out.stdout)?.trim().into())
}

struct FixtureController {
    inner: MockBackend,
    stopped: Arc<AtomicBool>,
}
struct FixtureSession {
    inner: Box<dyn AgentSession>,
    stopped: Arc<AtomicBool>,
}
#[async_trait::async_trait]
impl AgentBackend for FixtureController {
    async fn start(&self, spec: SessionSpec) -> kranz_engine::error::Result<Box<dyn AgentSession>> {
        if self.stopped.load(Ordering::Acquire) {
            return Err(kranz_engine::error::EngineError::Backend(
                "fixture stopped before repair; reseeding is not authorized".into(),
            ));
        }
        Ok(Box::new(FixtureSession {
            inner: self.inner.start(spec).await?,
            stopped: self.stopped.clone(),
        }))
    }
}
#[async_trait::async_trait]
impl AgentSession for FixtureSession {
    fn session_id(&self) -> String {
        self.inner.session_id()
    }
    async fn next_event(&mut self) -> kranz_engine::error::Result<Option<AgentEvent>> {
        self.inner.next_event().await
    }
    async fn send_user_message(&mut self, text: &str) -> kranz_engine::error::Result<()> {
        // Malformed/empty conversion JSON causes the engine to synthesize fix
        // features. Refuse the call itself, and all reseeds, before that path.
        if text.contains("produced these findings:") {
            self.stopped.store(true, Ordering::Release);
            return Err(kranz_engine::error::EngineError::Backend(
                "fixture does not authorize repair or another worker".into(),
            ));
        }
        self.inner.send_user_message(text).await
    }
    async fn abort(&mut self) -> kranz_engine::error::Result<()> {
        self.inner.abort().await
    }
    fn exit_status(&self) -> Option<SessionExit> {
        self.inner.exit_status()
    }
}

fn scripted_backend() -> Arc<FixtureController> {
    let replies = [
        r#"{"action":"commit-as-is","note":"scripted acceptance checkpoint"}"#,
        r#"{"decision":"complete","guidance":"","summary":"scripted acceptance controller"}"#,
        "NONE",
    ];
    Arc::new(FixtureController {stopped:Arc::new(AtomicBool::new(false)),inner:MockBackend::with_scripts(vec![
        MockScript::streaming(vec![
            mock_init("scripted-controller"),
            mock_result_text("ready"),
        ])
        .responding(
            replies
                .into_iter()
                .map(|r| vec![mock_text(r), mock_result_text(r)])
                .collect(),
        ),
        MockScript::single_shot_json(
            &json!({"findings":[],"summary":"scripted fixture reviewer; no model judgment"}),
        )
        .with_session_id("scripted-reviewer"),
        MockScript::single_shot_json(
            &json!({"findings":[],"summary":"scripted functional fixture; command evidence remains engine-owned"}),
        )
        .with_session_id("scripted-functional-reviewer"),
    ])})
}

fn verify_worker_transcript(
    paths: &kranz_engine::paths::MissionPaths,
    state: &kranz_engine::types::MissionState,
) -> Result<()> {
    let worker = state
        .runs
        .values()
        .find(|r| r.role == Role::Worker)
        .context("missing worker")?;
    let permission = state
        .permissions
        .values()
        .next()
        .context("missing permission")?;
    let workspace = Path::new(&permission.request.binding.workspace);
    let mut proof = tool_fixture::Evidence::default();
    for line in std::fs::read_to_string(paths.transcript_file(&worker.id))?.lines() {
        let raw: serde_json::Value = serde_json::from_str(line)?;
        if raw["method"] == "session/request_permission" {
            let mut action = raw["params"]["toolCall"].clone();
            if action.get("kind").is_none() {
                action["kind"] = permission.request.proposal.action["kind"].clone();
            }
            ensure!(
                action == permission.request.proposal.action,
                "permission transcript differs"
            );
            proof.authorize_at(
                &permission.request.proposal,
                workspace,
                permission.resolved_at.context("missing resolution time")?,
            )?;
            continue;
        }
        let event = if let Some(request) = raw["permissionResponse"].as_str() {
            AgentEvent::PermissionResponded {
                request_id: request.into(),
                delivery: serde_json::from_value(raw["delivery"].clone())?,
                raw,
            }
        } else {
            match raw["params"]["update"]["sessionUpdate"].as_str() {
                Some("tool_call") => AgentEvent::ToolUse {
                    tool: "execute".into(),
                    summary: String::new(),
                    raw,
                },
                Some("tool_call_update")
                    if matches!(
                        raw["params"]["update"]["status"].as_str(),
                        Some("completed" | "failed")
                    ) =>
                {
                    AgentEvent::ToolResult {
                        tool: None,
                        denied: false,
                        summary: String::new(),
                        raw,
                    }
                }
                _ => AgentEvent::Other { raw },
            }
        };
        proof.observe(&event, workspace)?;
    }
    proof.finish()
}

async fn run(root: &Path, prepared: Preparation) -> Result<serde_json::Value> {
    let primary = root.join("primary");
    std::fs::create_dir(&primary)?;
    git(&primary, &["init", "--template=", "-q", "-b", "main"])?;
    git(&primary, &["config", "user.name", "ACP Fixture"])?;
    git(
        &primary,
        &["config", "user.email", "fixture@example.invalid"],
    )?;
    git(&primary, &["config", "commit.gpgsign", "false"])?;
    std::fs::create_dir_all(primary.join(".kranz"))?;
    std::fs::create_dir(primary.join("pack"))?;
    std::fs::write(
        primary.join("README"),
        "Governed ACP live worker / scripted controller fixture\n",
    )?;
    std::fs::write(
        primary.join("pack/checker.py"),
        include_str!("../tests/fixtures/gate-evaluator/checker.py"),
    )?;
    std::fs::write(primary.join("pack/pack.toml"),format!("[pack]\nname='governed-probe'\nschema=5\n[[evaluator]]\nname='governed-probe'\nimage='{CHECKER_IMAGE}'\nexecutable='/usr/local/bin/python3'\nargs=['-I','-S','/checker/checker.py','pass']\nfiles=['checker.py']\nstages=['plan-approval','milestone-validation','final-gate','merge']\nevidence=['scope','check-receipt']\nkind='mechanical'\nenforcement='blocking'\n"))?;
    std::fs::write(
        primary.join(".kranz/merge-gates.json"),
        serde_json::to_vec(&json!({"gates":[{"command":CHECK}]}))?,
    )?;
    git(
        &primary,
        &["add", "README", "pack", ".kranz/merge-gates.json"],
    )?;
    git(
        &primary,
        &["commit", "-qm", "fixed governed acceptance baseline"],
    )?;
    let mut engine = MissionEngine::create(scripted_backend(), &primary, GOAL, prepared.config)?;
    engine.set_grant_request_cap(0);
    engine.approve_plan(prepared.plan)?;
    let base = engine
        .state()
        .mission
        .base_sha
        .clone()
        .context("missing base")?;
    let branch = engine.state().mission.mission_branch.clone();
    let paths = engine.paths().clone();
    write_new(
        &root.join("mission.json"),
        &json!({"mission":paths.mission_id,"base":base,"branch":branch}),
    )?;
    let baseline = git(&primary, &["diff", "--binary", "HEAD"])?;
    let monitor = async {
        let mut evidence = tool_fixture::Evidence::default();
        let mut seen = 0;
        let mut worker_deadline = None;
        let mut worker_id = None;
        let mut permission = false;
        loop {
            for event in EventLog::read_events(&paths.events_file())? {
                if event.seq <= seen {
                    continue;
                }
                seen = event.seq;
                match event.kind {
                    EventKind::FeatureStarted { .. } => {
                        // Controlled ACP buffers worker.spawned until its first
                        // permission packet. Start the clock at dispatch so a
                        // silent peer cannot borrow the longer mission budget.
                        ensure!(
                            worker_deadline.is_none() && worker_id.is_none(),
                            "second feature is not authorized"
                        );
                        let elapsed = (chrono::Utc::now() - event.ts).to_std().unwrap_or_default();
                        worker_deadline = Some(
                            tokio::time::Instant::now()
                                + Duration::from_secs(prepared.worker_seconds)
                                    .saturating_sub(elapsed),
                        );
                    }
                    EventKind::WorkerSpawned {
                        role: Role::Worker,
                        run_id,
                        ..
                    } => {
                        ensure!(worker_id.is_none(), "second worker is not authorized");
                        worker_id = Some(run_id);
                        ensure!(worker_deadline.is_some(), "worker has no dispatch deadline");
                    }
                    EventKind::PermissionRequested { request } => {
                        request.validate()?;
                        ensure!(
                            !permission && worker_id.as_ref() == Some(&request.binding.run_id),
                            "unexpected permission"
                        );
                        let workspace = Path::new(&request.binding.workspace);
                        ensure!(
                            git(workspace, &["rev-parse", "--git-common-dir"])?
                                == primary.join(".git").display().to_string(),
                            "foreign workspace"
                        );
                        evidence.authorize(&request.proposal, workspace)?;
                        permission = true;
                        kranz_engine::control::enqueue(&paths, &ControlCommand::ResolvePermission {
                            resolution:Resolution {request_id:request.proposal.id,binding_digest:request.binding_digest,
                                allow:true,actor:Actor::LocalRepositoryAuthority,
                                reason:"operator-authorized fixed acceptance command; one call only".into()}
                        })?;
                    }
                    EventKind::WorkerCompleted { run_id, .. }
                        if worker_id.as_ref() == Some(&run_id) =>
                    {
                        ensure!(
                            worker_deadline
                                .is_some_and(|deadline| tokio::time::Instant::now() < deadline),
                            "worker finished after its fixed wall budget"
                        );
                        worker_deadline = None;
                    }
                    _ => {}
                }
            }
            if worker_deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) {
                bail!("worker exceeded its fixed wall budget");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        #[allow(unreachable_code)]
        Ok::<(), anyhow::Error>(())
    };
    let status = {
        let mut mission = Box::pin(engine.run());
        tokio::select! {
            result = tokio::time::timeout(Duration::from_secs(prepared.mission_seconds), &mut mission) => result.context("mission wall budget exhausted")??,
            result = monitor => { result?; bail!("permission monitor stopped"); }
        }
    };
    ensure!(
        status == MissionStatus::Complete,
        "mission did not complete: {status:?}"
    );
    ensure!(
        git(&primary, &["rev-parse", "HEAD"])? == base,
        "primary ref changed before merge"
    );
    ensure!(
        git(&primary, &["diff", "--binary", "HEAD"])? == baseline,
        "primary bytes changed before merge"
    );
    ensure!(
        !primary.join(FILE).exists(),
        "worker wrote primary deliverable"
    );
    let state = engine.state();
    ensure!(state.permissions.len() == 1, "expected one consent");
    ensure!(
        state
            .permissions
            .values()
            .all(|p| p.delivery == Some(Delivery::Sent)
                && p.resolution.as_ref().is_some_and(|r| r.allow)),
        "consent was not delivered"
    );
    ensure!(
        state
            .runs
            .values()
            .filter(|r| r.role == Role::Worker)
            .count()
            == 1,
        "expected one worker"
    );
    ensure!(
        !state.mission.milestones[0].features[0].commits.is_empty(),
        "empty deliverable"
    );
    verify_worker_transcript(&paths, state)?;
    for changed in git(&primary, &["diff", "--name-only", &base, &branch])?.lines() {
        ensure!(
            changed == FILE
                || changed == ".kranz/missions/index.md"
                || changed.starts_with(&format!(".kranz/missions/{}/", paths.mission_id)),
            "unexpected changed path"
        );
    }
    for stage in [
        Stage::PlanApproval,
        Stage::MilestoneValidation,
        Stage::FinalGate,
    ] {
        ensure!(
            state
                .gate_evaluations
                .values()
                .any(|r| r.requested.request.params.stage == stage && r.consumed.is_some()),
            "missing stage {stage:?}"
        );
    }
    let policy = kranz_engine::command_exec::MergeGatePolicy {
        sandbox: kranz_engine::command_exec::worker_gate_sandbox(&state.config)?,
        mission_dir: paths.mission_dir(),
    };
    drop(engine);
    let merge_root = primary.clone();
    let merge_paths = paths.clone();
    let merged = tokio::task::spawn_blocking(move || {
        kranz_engine::merge::merge_mission_with_external_evidence(
            &kranz_engine::git_ops::GitRepo::open(&merge_root)?,
            "main",
            &base,
            &branch,
            None,
            None,
            &Default::default(),
            |cmd, cwd| {
                kranz_engine::command_exec::run_bounded_gate_command_sandboxed(cwd, cmd, &policy)
            },
            &merge_paths,
            Actor::LocalRepositoryAuthority,
        )
    })
    .await??;
    ensure!(
        matches!(merged, kranz_engine::merge::MergeReport::Merged { .. }),
        "merge refused"
    );
    ensure!(
        std::fs::read(primary.join(FILE))? == OUTPUT,
        "wrong deliverable bytes"
    );
    let events = EventLog::read_events(&paths.events_file())?;
    let replay = kranz_engine::reducer::fold(&events)?;
    let merge = replay
        .gate_evaluations
        .values()
        .find(|r| r.requested.request.params.stage == Stage::Merge && r.consumed.is_some())
        .context("missing consumed merge gate")?;
    let Subject::Integration {
        integration_tree, ..
    } = &merge.requested.request.params.subject
    else {
        bail!("wrong merge subject")
    };
    ensure!(
        integration_tree.value == git(&primary, &["rev-parse", "HEAD^{tree}"])?,
        "merge tree differs from judged tree"
    );
    kranz_engine::evidence_bundle::export_evidence_bundle(
        &primary,
        &paths.mission_id,
        &root.join("evidence"),
    )?;
    let exported =
        kranz_engine::evidence_bundle::assemble_evidence_bundle(&primary, &paths.mission_id)?;
    for entry in &exported.manifest.entries {
        if let Some(path) = &entry.path {
            let bytes = std::fs::read(root.join("evidence").join(path))?;
            ensure!(
                entry.sha256.as_deref() == Digest::of(&bytes).as_str().strip_prefix("sha256:"),
                "export hash mismatch"
            );
        }
    }
    Ok(
        json!({"provider":prepared.provider,"passed":true,"mission":paths.mission_id,
        "workerSessions":1,"permissionDeliveries":1,"primaryUnchangedBeforeMerge":true,
        "integrationTree":integration_tree.value,"evidenceExported":true,
        "controllerAndReviewer":"scripted; not independent model judgment","hardDollarCap":null,
        "cleanupRequiresSeparateInventory":true}),
    )
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [mode,provider,credential,root] if mode == "--prepare" => {
            let digest = prepare(provider,Path::new(credential),Path::new(root))?;
            println!("{}",json!({"prepared":true,"preparationDigest":digest,"providerCalls":0}));
        }
        [mode,root,digest] if mode == "--run" => {
            let root = Path::new(root);
            let prepared = consume(root,digest)?;
            let outcome = tokio::time::timeout(Duration::from_secs(prepared.overall_seconds),run(root,prepared))
                .await.context("overall fixture wall budget exhausted").and_then(|result|result);
            let record = match &outcome {Ok(record)=>record.clone(),Err(error)=>json!({"passed":false,"error":error.to_string(),"cleanupRequiresSeparateInventory":true})};
            write_new(&root.join("result.json"),&record)?;
            println!("{}",json!({"passed":outcome.is_ok(),"resultWritten":true}));
            outcome?;
        }
        _ => bail!("usage: acp_governed_probe --prepare PROVIDER CREDENTIAL_FILE NEW_ABSOLUTE_DIRECTORY | --run DIRECTORY AUTHORIZED_PREPARATION_DIGEST"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn governed_missing_credential_refuses_before_a_provider_session() {
        if std::env::var("KRANZ_GATE_CONTAINER_TESTS").as_deref() != Ok("1")
            || !cfg!(all(
                any(target_os = "macos", target_os = "linux"),
                target_arch = "aarch64"
            ))
        {
            eprintln!("SKIP-GOVERNED-PREFLIGHT: requires opt-in Docker and an admitted ARM64 host");
            return;
        }
        let tmp = tempfile::tempdir_in(kranz_engine::backend_claude::scratch_root_base()).unwrap();
        let root = tmp.path().join("missing-credential");
        let digest = prepare("codex", &tmp.path().join("absent-auth.json"), &root).unwrap();
        let prepared = consume(&root, digest.as_str()).unwrap();
        let error = tokio::time::timeout(Duration::from_secs(180), Box::pin(run(&root, prepared)))
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("credential"), "{error}");
        let record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(root.join("mission.json")).unwrap()).unwrap();
        let primary = root.join("primary");
        let paths =
            kranz_engine::paths::MissionPaths::new(&primary, record["mission"].as_str().unwrap());
        let events = EventLog::read_events(&paths.events_file()).unwrap();
        let state = kranz_engine::reducer::fold(&events).unwrap();
        assert!(state.permissions.is_empty());
        assert!(state
            .gate_evaluations
            .values()
            .any(
                |g| g.requested.request.params.stage == Stage::PlanApproval && g.consumed.is_some()
            ));
        let policy = kranz_engine::command_exec::MergeGatePolicy {
            sandbox: kranz_engine::command_exec::worker_gate_sandbox(&state.config).unwrap(),
            mission_dir: paths.mission_dir(),
        };
        for (bytes, expected) in [(OUTPUT, true), (b"wrong\n".as_slice(), false)] {
            std::fs::write(primary.join(FILE), bytes).unwrap();
            let gate_root = primary.clone();
            let gate_policy = kranz_engine::command_exec::MergeGatePolicy {
                sandbox: policy.sandbox.clone(),
                mission_dir: policy.mission_dir.clone(),
            };
            let checked = tokio::task::spawn_blocking(move || {
                kranz_engine::command_exec::run_bounded_gate_command_sandboxed(
                    &gate_root,
                    CHECK,
                    &gate_policy,
                )
            })
            .await
            .unwrap();
            assert_eq!(checked.0, expected, "{}", checked.1);
            assert!(!checked.1.contains("not found"), "{}", checked.1);
        }
        std::fs::remove_file(primary.join(FILE)).unwrap();
        for entry in std::fs::read_dir(paths.runs_dir()).unwrap() {
            let p = entry.unwrap().path();
            if p.extension().is_some_and(|e| e == "jsonl") {
                assert!(!std::fs::read_to_string(p)
                    .unwrap()
                    .contains("workerProfile"));
            }
        }
        for entry in git(&primary, &["worktree", "list", "--porcelain"])
            .unwrap()
            .lines()
        {
            if let Some(path) = entry.strip_prefix("worktree ") {
                if Path::new(path) != primary {
                    git(&primary, &["worktree", "remove", "--force", path]).unwrap();
                }
            }
        }
    }
    #[test]
    fn governed_prompt_report_uses_the_worker_contract() {
        let p = plan().unwrap();
        let text = p.milestones[0].features[0]
            .spec
            .rsplit_once('\n')
            .unwrap()
            .1;
        let report = kranz_engine::runner::parse_worker_report(text).unwrap();
        assert_eq!(report.result, kranz_engine::types::RunResult::Pass);
        assert_eq!(report.files_touched, vec![FILE]);
        assert_eq!(report.commands_run, vec![tool_fixture::COMMAND]);
    }

    #[tokio::test]
    async fn governed_controller_refuses_findings_before_conversion_or_reseed() {
        let backend = scripted_backend();
        let spec = SessionSpec {
            cwd: std::env::temp_dir(),
            prompt: kranz_engine::backend::PromptMode::Streaming("fixture".into()),
            append_system_prompt: None,
            model: "mock".into(),
            effort: "default".into(),
            session_id: "fixture".into(),
            resume: None,
            permission_mode: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
            tools: vec![],
            writable: false,
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: Default::default(),
            sandbox: None,
            hook_status: None,
        };
        let mut session = backend.start(spec.clone()).await.unwrap();
        assert!(session
            .send_user_message("Validation of milestone ms-1 produced these findings:\n[]")
            .await
            .is_err());
        assert!(backend.start(spec).await.is_err());
        assert!(backend.inner.injected_messages()[0].is_empty());
    }
    #[test]
    fn governed_preparation_is_pinned_single_use_and_reads_no_credential() {
        if !cfg!(all(
            any(target_os = "macos", target_os = "linux"),
            target_arch = "aarch64"
        )) {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("batch");
        let digest = prepare("codex", &tmp.path().join("absent-auth.json"), &root).unwrap();
        assert!(consume(&root, "sha256:wrong").is_err());
        assert!(!root.join("attempt.json").exists());
        let p = consume(&root, digest.as_str()).unwrap();
        assert_eq!(p.worker_sessions, 1);
        assert_eq!(p.config.max_respawns, 0);
        assert_eq!(p.config.max_fix_cycles_per_milestone, 1);
        assert!(consume(&root, digest.as_str()).is_err());
        assert!(!tmp.path().join("absent-auth.json").exists());
        assert!(!root.join("primary").exists());
    }

    #[test]
    fn governed_preparation_refuses_resealed_budget_config_and_plan_changes() {
        if !cfg!(all(
            any(target_os = "macos", target_os = "linux"),
            target_arch = "aarch64"
        )) {
            assert!(configuration("codex", std::env::temp_dir().join("absent-auth.json")).is_err());
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        for mutation in 0..4 {
            let root = tmp.path().join(format!("batch-{mutation}"));
            prepare("claude", &tmp.path().join("absent-auth.json"), &root).unwrap();
            let path = root.join("preparation.json");
            let mut value: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            match mutation {
                0 => value["workerSessions"] = json!(2),
                1 => value["config"]["maxRespawns"] = json!(1),
                2 => value["plan"]["touchSet"] = json!(["unapproved.txt"]),
                3 => value["executableDigest"] = json!(Digest::of(b"unrelated runner")),
                _ => unreachable!(),
            }
            let bytes = serde_json::to_vec(&value).unwrap();
            std::fs::write(&path, &bytes).unwrap();
            assert!(
                consume(&root, Digest::of(&bytes).as_str()).is_err(),
                "mutation {mutation}"
            );
            assert!(!root.join("attempt.json").exists());
        }
    }

    #[test]
    fn governed_permission_replay_checks_the_actual_resolution_time_and_effect() {
        use kranz_engine::live_permission::{digest, Proposal};
        let at = chrono::Utc::now() - chrono::Duration::seconds(600);
        let action = json!({"kind":"execute","toolCallId":"fixed-tool","rawInput":{"command":tool_fixture::COMMAND}});
        let options = vec![json!({"optionId":"once","kind":"allow_once","name":"Once"})];
        let proposal = Proposal {
            id: "fixed-request".into(),
            engine_session_id: "engine".into(),
            peer_session_id: "peer".into(),
            peer_request_id: json!(1),
            tool_call_id: "fixed-tool".into(),
            action_digest: digest(&action).unwrap(),
            options_digest: digest(&options).unwrap(),
            action,
            options,
            observed_at: at,
            deadline: at + chrono::Duration::seconds(300),
            prohibition: None,
        };
        let workspace = std::env::temp_dir();
        assert!(tool_fixture::Evidence::default()
            .authorize(&proposal, &workspace)
            .is_err());
        let mut evidence = tool_fixture::Evidence::default();
        assert!(evidence
            .authorize_at(&proposal, &workspace, at + chrono::Duration::seconds(1))
            .is_ok());
        assert!(evidence
            .authorize_at(&proposal, &workspace, at + chrono::Duration::seconds(2))
            .is_err());
        assert!(tool_fixture::Evidence::default()
            .authorize_at(&proposal, &workspace, at - chrono::Duration::seconds(1))
            .is_err());
        let mut different = proposal.clone();
        different.action["rawInput"]["command"] = json!("echo unrelated > other.txt");
        different.action_digest = digest(&different.action).unwrap();
        assert!(tool_fixture::Evidence::default()
            .authorize_at(&different, &workspace, at + chrono::Duration::seconds(1))
            .is_err());
    }
}
