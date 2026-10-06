//! Explicit, one-attempt live measurement fixture. Not built into kranz.
use super::*;
use crate::backend::{AgentBackend, AgentEvent, SessionExit};
use crate::live_permission::{Delivery, Proposal};
use anyhow::{ensure, Context};
use std::time::Duration;

const COMMAND: &str = "/usr/local/bin/python3 -I -S /kranz-owned-session/qualification.py";
const OVERALL_SECONDS: u64 = 780;

fn sha(bytes: &[u8]) -> String {
    crate::standards_waiver::sha256_hex(bytes)
}

fn executable_sha() -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};
    let mut file = std::fs::File::open(std::env::current_exe()?)?;
    let mut hash = Sha256::new();
    let mut block = [0; 65536];
    loop {
        let count = file.read(&mut block)?;
        if count == 0 {
            break;
        }
        hash.update(&block[..count]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn prompt() -> String {
    format!("Bounded resource qualification fixture. Use your native shell tool exactly once to run exactly: {COMMAND}\nRequest explicit one-time approval before execution (Codex: require_escalated with no prefix rule). Allow up to 300000 milliseconds for the command. Do not run preliminary commands, read files, use other tools, change permissions, retry, install dependencies, call the network or create commits. The fixed read-only script builds and tests small offline Rust and Node projects. If permission or execution fails, stop. After it succeeds reply exactly: kranz-resource-workload-passed")
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Preparation {
    schema: u32,
    provider: String,
    credential_file: PathBuf,
    profile: AcpWorkerProfile,
    image: String,
    definition_sha256: String,
    executable_sha256: String,
    supervisor_sha256: String,
    workload_sha256: String,
    limits: Resources,
    command: String,
    prompt: String,
    prompts: u32,
    permissions: u32,
    retries: u32,
    overall_seconds: u64,
    hard_dollar_cap: Option<f64>,
    model_selection: String,
    keychain: bool,
}

fn preparation(provider: &str, credential_file: &Path) -> anyhow::Result<Preparation> {
    ensure!(
        matches!(provider, "claude" | "codex"),
        "unsupported provider"
    );
    ensure!(
        credential_file.is_absolute(),
        "absolute credential path required"
    );
    let profile = AcpWorkerProfile {
        id: format!("qualification-{provider}-resource-r3"),
        credential_file: credential_file.into(),
        resources: None,
    };
    let definition = profile.definition()?;
    Ok(Preparation {
        schema: 1,
        provider: provider.into(),
        credential_file: credential_file.into(),
        image: definition.image.into(),
        definition_sha256: definition.sha256(),
        profile,
        executable_sha256: executable_sha()?,
        supervisor_sha256: sha(include_bytes!(
            "../../acp_container/supervisor_resources.py"
        )),
        workload_sha256: sha(include_bytes!(
            "../../acp_container/qualification_workload.py"
        )),
        limits: RESOURCE_CANDIDATE_LIMITS,
        command: COMMAND.into(),
        prompt: prompt(),
        prompts: 1,
        permissions: 1,
        retries: 0,
        overall_seconds: OVERALL_SECONDS,
        hard_dollar_cap: None,
        model_selection: "peer default; record initialization; not pinned".into(),
        keychain: false,
    })
}

fn write_new(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

fn bounded_bytes(path: &Path) -> anyhow::Result<Vec<u8>> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let stat = file.metadata()?;
    ensure!(
        stat.is_file() && stat.nlink() == 1 && stat.len() <= 65536,
        "not a bounded regular file"
    );
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65536, "file exceeded bound");
    Ok(bytes)
}

fn consume(root: &Path, authorized: &str) -> anyhow::Result<Preparation> {
    let bytes = bounded_bytes(&root.join("preparation.json"))?;
    ensure!(sha(&bytes) == authorized, "preparation digest changed");
    let p: Preparation = serde_json::from_value(crate::strict_json::parse(&bytes)?)?;
    ensure!(
        p == preparation(&p.provider, &p.credential_file)?,
        "runner, bounds or fixed inputs changed"
    );
    // Exclusive consumption precedes all credential reads and adapter startup.
    write_new(
        &root.join("attempt.json"),
        &json!({"authorizedPreparation":authorized,
        "startedAt":chrono::Utc::now(),"harnessRetries":0,"keychain":false}),
    )?;
    Ok(p)
}

fn exact_input(value: &Value, workspace: &Path) -> anyhow::Result<()> {
    let input = value.as_object().context("missing exact tool input")?;
    let command = input
        .get("command")
        .and_then(Value::as_str)
        .context("missing command")?;
    ensure!(
        command == COMMAND || command == format!("/usr/bin/bash -lc '{COMMAND}'"),
        "unapproved command"
    );
    for (key, value) in input {
        let accepted = match key.as_str() {
            "command" => true,
            "cwd" => value.as_str() == workspace.to_str(),
            "description" => value.as_str().is_some_and(|s| s.len() <= 1024),
            "timeout" => value.as_u64().is_some_and(|n| n > 0 && n <= 300_000),
            "run_in_background" | "dangerouslyDisableSandbox" => value == false,
            _ => false,
        };
        ensure!(accepted, "unsupported tool input field");
    }
    Ok(())
}

fn authorize(proposal: &Proposal, workspace: &Path) -> anyhow::Result<String> {
    proposal.validate()?;
    let now = chrono::Utc::now();
    ensure!(
        proposal.prohibition.is_none()
            && !proposal.ambiguous_display()
            && now >= proposal.observed_at
            && now < proposal.deadline,
        "permission unavailable or stale"
    );
    ensure!(
        proposal.action["kind"] == "execute",
        "only fixed execution is authorized"
    );
    exact_input(&proposal.action["rawInput"], workspace)?;
    let selected = proposal.option(true).context("missing unique allow_once")?;
    let option = proposal
        .options
        .iter()
        .find(|o| o["optionId"] == selected)
        .unwrap();
    ensure!(
        option
            .as_object()
            .context("invalid option")?
            .keys()
            .all(|k| matches!(k.as_str(), "optionId" | "name" | "kind")),
        "permission extensions refused"
    );
    Ok(selected)
}

fn verify_measurement(e: &crate::acp_resources::ResourceEvidence) -> anyhow::Result<()> {
    ensure!(
        e.namespace_cleanup_confirmed && e.unavailable.is_empty() && e.failure.is_none(),
        "incomplete resource/cleanup evidence"
    );
    ensure!(
        e.image == RESOURCE_CANDIDATE_IMAGE && e.limits == RESOURCE_CANDIDATE_LIMITS,
        "candidate differs"
    );
    let s = e.sample.as_ref().context("missing protected sample")?;
    ensure!(
        s.complete && !s.wall_clock_expired,
        "incomplete or expired interval"
    );
    ensure!(
        s.current
            .oom_kill
            .zip(s.baseline.oom_kill)
            .is_some_and(|(a, b)| a == b)
            && s.current
                .pids_max
                .zip(s.baseline.pids_max)
                .is_some_and(|(a, b)| a == b),
        "exhaustion during useful work"
    );
    let u = s.usage.as_ref().context("missing kernel accounting")?;
    ensure!(
        u.memory_peak_bytes.is_some_and(|n| n > 0) && u.pids_peak.is_some_and(|n| n > 0),
        "missing peaks"
    );
    ensure!(
        u.cpu_usage_usec
            .zip(u.cpu_usage_at_start_usec)
            .is_some_and(|(a, b)| a > b),
        "missing positive CPU interval"
    );
    let l = &u.limits;
    let r = RESOURCE_CANDIDATE_LIMITS;
    ensure!(
        l.memory_max_bytes == Some(r.memory_mib as u64 * 1024 * 1024)
            && l.swap_max_bytes == Some(0)
            && l.pids_max == Some(r.pids as u64)
            && l.nofile == Some([r.nofile as u64; 2])
            && l.fsize_bytes == Some([r.fsize_mib as u64 * 1024 * 1024; 2]),
        "kernel limits differ"
    );
    ensure!(
        l.cpu_quota_usec
            .zip(l.cpu_period_usec)
            .is_some_and(|(q, p)| p > 0 && q as u128 * 1000 == p as u128 * r.cpu_millis as u128),
        "CPU ceiling differs"
    );
    Ok(())
}

#[derive(Default)]
struct Observation {
    request: Option<Proposal>,
    tool_id: Option<String>,
    sent: bool,
    saw_tool: bool,
    completed: bool,
    reported: bool,
    model: Option<String>,
}

impl Observation {
    fn accept(
        &mut self,
        event: &AgentEvent,
        workspace: &Path,
    ) -> anyhow::Result<Option<(Proposal, String)>> {
        match event {
            AgentEvent::PermissionRequested { proposal, .. } => {
                ensure!(
                    self.request.is_none() && !self.completed,
                    "more than one permission request"
                );
                let option = authorize(proposal, workspace)?;
                if let Some(id) = &self.tool_id {
                    ensure!(id == &proposal.tool_call_id, "tool identity changed");
                }
                self.tool_id = Some(proposal.tool_call_id.clone());
                self.request = Some((**proposal).clone());
                return Ok(Some((self.request.clone().unwrap(), option)));
            }
            AgentEvent::PermissionResponded {
                request_id,
                delivery,
                ..
            } => {
                ensure!(
                    !self.sent
                        && self.request.as_ref().is_some_and(|r| r.id == *request_id)
                        && *delivery == Delivery::Sent,
                    "uncertain/repeated permission delivery"
                );
                self.sent = true;
            }
            AgentEvent::Init { model, .. } => {
                self.model = Some(model.clone());
            }
            AgentEvent::Result { text, is_error, .. } => {
                ensure!(
                    !self.reported
                        && !is_error
                        && self.completed
                        && text.trim() == "kranz-resource-workload-passed",
                    "invalid completion report"
                );
                self.reported = true;
            }
            _ => {}
        }
        let raw = match event {
            AgentEvent::ToolUse { raw, .. }
            | AgentEvent::ToolResult { raw, .. }
            | AgentEvent::Other { raw } => Some(raw),
            _ => None,
        };
        if let Some(raw) = raw {
            let update = &raw["params"]["update"];
            if matches!(
                update["sessionUpdate"].as_str(),
                Some("tool_call" | "tool_call_update")
            ) {
                ensure!(!self.completed, "tool activity after completion");
                let id = update["toolCallId"].as_str().context("missing tool id")?;
                ensure!(
                    !id.is_empty() && self.tool_id.as_deref().is_none_or(|old| old == id),
                    "more than one tool"
                );
                self.tool_id = Some(id.into());
                ensure!(
                    update.get("kind").is_none_or(|k| k == "execute"),
                    "unexpected tool kind"
                );
                if let Some(input) = update.get("rawInput") {
                    if !(self.request.is_none() && input.as_object().is_some_and(|o| o.is_empty()))
                    {
                        exact_input(input, workspace)?;
                    }
                }
                if matches!(event, AgentEvent::ToolUse { .. }) {
                    ensure!(!self.saw_tool, "repeated tool start");
                    self.saw_tool = true;
                }
                if let AgentEvent::ToolResult { denied, .. } = event {
                    ensure!(
                        self.sent && self.saw_tool && !denied && update["status"] == "completed",
                        "tool failed or lacked consent"
                    );
                    ensure!(
                        update["rawOutput"].get("exit_code").is_none_or(|v| v == 0)
                            && update["rawOutput"]
                                .get("isError")
                                .is_none_or(|v| v == false),
                        "tool error"
                    );
                    self.completed = true;
                }
            } else {
                ensure!(
                    !matches!(
                        event,
                        AgentEvent::ToolUse { .. } | AgentEvent::ToolResult { .. }
                    ),
                    "tool event lacks protocol identity"
                );
            }
        }
        Ok(None)
    }

    fn finish(&self) -> anyhow::Result<()> {
        ensure!(
            self.reported && self.sent && self.completed,
            "missing report/permission/tool evidence"
        );
        Ok(())
    }
}

async fn live(root: &Path, p: &Preparation, authorized: &str) -> anyhow::Result<Value> {
    let mut f = Fixture::new();
    f.profile = p.profile.clone();
    let baseline = git(&f.primary, &["rev-parse", "HEAD"]);
    let mut spec = f.spec();
    spec.prompt = PromptMode::SingleShot(p.prompt.clone());
    spec.sandbox.as_mut().unwrap().inputs.egress = f
        .profile
        .definition()?
        .egress
        .iter()
        .map(|s| (*s).into())
        .collect();
    let paths = crate::paths::MissionPaths::new(&f.primary, "m-profile");
    let boundary = crate::egress_proxy::maybe_start_for_session(&mut spec, &paths)
        .await?
        .context("missing egress boundary")?;
    let backend = crate::backend_acp::AcpBackend::for_worker(&config(f.profile.clone()).worker)?;
    let started = backend.start(spec).await;
    let mut session = match started {
        Ok(s) => s,
        Err(error) => {
            boundary.shutdown().await?;
            return Err(error.into());
        }
    };
    let responder = session
        .permission_responder()
        .context("missing permission channel")?;
    let mut observed = Observation::default();
    let outcome: anyhow::Result<()> = async {
        while let Some(event) = session.next_event().await? {
            if let Some((proposal, option)) = observed.accept(&event, &f.workspace)? {
                write_new(
                    &root.join("permission.json"),
                    &json!({"requestId":proposal.id,
                    "actionDigest":proposal.action_digest,"optionsDigest":proposal.options_digest,
                    "selectedOption":option,"authorizedPreparation":authorized,
                    "decidedAt":chrono::Utc::now()}),
                )?;
                responder.respond(&proposal, true)?;
            }
        }
        observed.finish()?;
        ensure!(
            session.exit_status() == Some(SessionExit::Completed),
            "session failed"
        );
        Ok(())
    }
    .await;
    if outcome.is_err() {
        session.abort().await?;
    }
    let denials = boundary.shutdown().await?;
    let evidence = session
        .resource_evidence()
        .context("missing resource evidence")?;
    write_new(&root.join("observations.json"), &evidence)?;
    outcome?;
    ensure!(denials.is_empty(), "unexpected egress");
    verify_measurement(&evidence)?;
    let result: Value = serde_json::from_slice(&bounded_bytes(
        &f.workspace.join("qualification-output/receipt.json"),
    )?)?;
    ensure!(
        result["schema"] == 1
            && result["passed"] == true
            && result["rustTests"] == 2
            && result["nodeTests"] == 2
            && result["dependencyDownloads"] == 0,
        "workload receipt invalid"
    );
    ensure!(
        git(&f.primary, &["rev-parse", "HEAD"]) == baseline
            && git(&f.workspace, &["rev-parse", "HEAD"]) == baseline
            && bounded_bytes(&f.primary.join("source.txt"))? == b"base\n"
            && git(
                &f.primary,
                &["status", "--porcelain", "--untracked-files=no"]
            )
            .is_empty(),
        "primary changed"
    );
    // No raw provider text or environment values are included in the receipt.
    Ok(
        json!({"passed":true,"provider":p.provider,"measurementOnly":true,
        "productionQualified":false,"workerSessions":1,"permissionDeliveries":1,
        "workload":result,"resourceEvidence":evidence,"model":observed.model,
        "primaryUnchanged":true,"hardDollarCap":null,"keychain":false}),
    )
}

#[test]
#[ignore = "prepare a concrete live measurement; reads no credential"]
fn acp_resource_qualification_prepare_live() -> anyhow::Result<()> {
    let root = PathBuf::from(std::env::var("KRANZ_RESOURCE_PREPARATION")?);
    let credential = PathBuf::from(std::env::var("KRANZ_RESOURCE_CREDENTIAL_FILE")?);
    ensure!(
        root.is_absolute() && !credential.starts_with(&root),
        "absolute, separate preparation/credential paths required"
    );
    let p = preparation(&std::env::var("KRANZ_RESOURCE_PROVIDER")?, &credential)?;
    std::fs::create_dir(&root)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    write_new(&root.join("preparation.json"), &p)?;
    println!(
        "{}",
        json!({"prepared":true,"providerCalls":0,"preparationDigest":sha(&bounded_bytes(&root.join("preparation.json"))?)})
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "requires separate digest-bound operator authorization; consumes one live attempt"]
async fn acp_resource_qualification_authorized_live() -> anyhow::Result<()> {
    ensure!(
        std::env::consts::ARCH == "aarch64",
        "candidate is ARM64 only"
    );
    ensure!(
        std::env::var("KRANZ_ACP_CONTAINER_TESTS").as_deref() == Ok("1"),
        "explicit Docker opt-in required"
    );
    let root = PathBuf::from(std::env::var("KRANZ_RESOURCE_PREPARATION")?);
    let authorized = std::env::var("KRANZ_RESOURCE_AUTHORIZED_DIGEST")?;
    let p = consume(&root, &authorized)?;
    let outcome = tokio::time::timeout(
        Duration::from_secs(OVERALL_SECONDS),
        live(&root, &p, &authorized),
    )
    .await;
    let record = match &outcome {
        Ok(Ok(record)) => record.clone(),
        _ => {
            json!({"passed":false,"productionQualified":false,"attemptConsumed":true,"cleanupRequiresInventory":true})
        }
    };
    write_new(&root.join("result.json"), &record)?;
    outcome.context("overall qualification deadline expired")??;
    Ok(())
}

#[test]
fn acp_resource_qualification_refuses_changed_or_replayed_allowances() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let credential = root.path().join("absent-credential.json");
    let p = preparation("codex", &credential)?;
    write_new(&root.path().join("preparation.json"), &p)?;
    let bytes = bounded_bytes(&root.path().join("preparation.json"))?;
    assert!(consume(root.path(), "wrong-digest").is_err());
    assert!(!root.path().join("attempt.json").exists());
    consume(root.path(), &sha(&bytes))?;
    assert!(consume(root.path(), &sha(&bytes)).is_err());
    assert!(
        !credential.exists(),
        "preparation/consumption must not access credentials"
    );
    let other = tempfile::tempdir()?;
    let mut changed = p;
    changed.limits.memory_mib += 1;
    write_new(&other.path().join("preparation.json"), &changed)?;
    let bytes = bounded_bytes(&other.path().join("preparation.json"))?;
    assert!(consume(other.path(), &sha(&bytes)).is_err());
    assert!(!other.path().join("attempt.json").exists());
    Ok(())
}

#[test]
fn acp_resource_qualification_grant_refuses_extra_command_or_authority() {
    let cwd = Path::new("/fixture");
    for value in [
        json!({"command":COMMAND}),
        json!({"command":format!("/usr/bin/bash -lc '{COMMAND}'"),"cwd":"/fixture","timeout":300000}),
    ] {
        exact_input(&value, cwd).unwrap();
    }
    for value in [
        json!({"command":format!("{COMMAND}; whoami")}),
        json!({"command":COMMAND,"cwd":"/elsewhere"}),
        json!({"command":COMMAND,"env":{"TOKEN":"extra"}}),
        json!({"command":COMMAND,"run_in_background":true}),
        json!({"command":COMMAND,"timeout":300001}),
    ] {
        assert!(exact_input(&value, cwd).is_err());
    }
}

#[test]
fn acp_resource_qualification_rejects_missing_consent_and_extra_tool_activity() {
    let workspace = Path::new("/fixture");
    let now = chrono::Utc::now();
    let action = json!({"kind":"execute","rawInput":{"command":COMMAND}});
    let options = vec![json!({"optionId":"once","name":"Allow once","kind":"allow_once"})];
    let proposal = Proposal {
        id: "permission".into(),
        engine_session_id: "engine".into(),
        peer_session_id: "peer".into(),
        peer_request_id: json!(1),
        tool_call_id: "tool".into(),
        action_digest: crate::live_permission::digest(&action).unwrap(),
        options_digest: crate::live_permission::digest(&options).unwrap(),
        action,
        options,
        observed_at: now,
        deadline: now + chrono::Duration::seconds(30),
        prohibition: None,
    };
    let permission = AgentEvent::PermissionRequested {
        proposal: Box::new(proposal),
        raw: Value::Null,
    };
    let delivered = AgentEvent::PermissionResponded {
        request_id: "permission".into(),
        delivery: Delivery::Sent,
        raw: Value::Null,
    };
    let tool = AgentEvent::ToolUse {
        tool: "execute".into(),
        summary: String::new(),
        raw: json!({"params":{"update":{"sessionUpdate":"tool_call","toolCallId":"tool","kind":"execute","rawInput":{}}}}),
    };
    let done = AgentEvent::ToolResult {
        tool: Some("execute".into()),
        denied: false,
        summary: String::new(),
        raw: json!({"params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"tool","status":"completed","rawOutput":{"exit_code":0}}}}),
    };
    let report = AgentEvent::Result {
        text: "kranz-resource-workload-passed".into(),
        is_error: false,
        usage: Default::default(),
        cost_usd: None,
        num_turns: None,
        raw: Value::Null,
    };
    for reverse_announcement in [false, true] {
        let mut observed = Observation::default();
        let mut exact_tool = tool.clone();
        if let AgentEvent::ToolUse { raw, .. } = &mut exact_tool {
            raw["params"]["update"]["rawInput"] = json!({"command":COMMAND});
        }
        let events = if reverse_announcement {
            vec![&permission, &delivered, &exact_tool, &done, &report]
        } else {
            vec![&tool, &permission, &delivered, &done, &report]
        };
        let mut grants = 0;
        for event in events {
            grants += usize::from(observed.accept(event, workspace).unwrap().is_some());
        }
        observed.finish().unwrap();
        assert_eq!(grants, 1);
        assert!(observed.accept(&tool, workspace).is_err());
        assert!(observed.accept(&permission, workspace).is_err());
    }
    for sequence in [
        vec![&tool, &done],
        vec![&tool, &permission, &done],
        vec![&tool, &permission, &permission],
        vec![&delivered],
        vec![&report],
        vec![&tool, &permission, &delivered, &delivered],
    ] {
        let mut observed = Observation::default();
        assert!(sequence
            .into_iter()
            .any(|event| observed.accept(event, workspace).is_err()));
        assert!(observed.finish().is_err());
    }
    let mut observed = Observation::default();
    observed.accept(&tool, workspace).unwrap();
    let mut other = tool;
    if let AgentEvent::ToolUse { raw, .. } = &mut other {
        raw["params"]["update"]["toolCallId"] = json!("second");
    }
    assert!(observed.accept(&other, workspace).is_err());
}

#[test]
fn acp_resource_qualification_requires_observed_limits_and_complete_accounting() {
    let limits = RESOURCE_CANDIDATE_LIMITS;
    let counters = json!({"oomKill":0,"pidsMax":0,"throttledUsec":10});
    let record = json!({"containerId":"a".repeat(64),"image":RESOURCE_CANDIDATE_IMAGE,
        "supervisorSha256":"b".repeat(64),"owner":"fixture","limits":limits,
        "observationStartedAt":chrono::Utc::now(),"observationFinishedAt":chrono::Utc::now(),
        "state":{"running":false,"oomKilled":false,"exitCode":0},"unavailable":[],
        "namespaceCleanupConfirmed":true,"sample":{"owner":"fixture","elapsedMs":10,
        "complete":true,"wallClockExpired":false,"baseline":counters,"current":counters,
        "usage":{"memoryPeakBytes":1000,"pidsPeak":3,"cpuUsageUsec":1000,"cpuUsageAtStartUsec":1,
        "limits":{"memoryMaxBytes":limits.memory_mib as u64 * 1024 * 1024,"swapMaxBytes":0,
        "pidsMax":limits.pids,"cpuQuotaUsec":200000,"cpuPeriodUsec":100000,
        "nofile":[limits.nofile,limits.nofile],
        "fsizeBytes":[limits.fsize_mib as u64 * 1024 * 1024,limits.fsize_mib as u64 * 1024 * 1024]}}}});
    let evidence = |value: Value| serde_json::from_value(value).unwrap();
    verify_measurement(&evidence(record.clone())).unwrap();
    for (path, value) in [
        ("/namespaceCleanupConfirmed", json!(false)),
        ("/sample/complete", json!(false)),
        ("/sample/usage/memoryPeakBytes", Value::Null),
        ("/sample/usage/pidsPeak", Value::Null),
        ("/sample/usage/cpuUsageUsec", json!(0)),
        ("/sample/usage/limits/swapMaxBytes", Value::Null),
        ("/sample/usage/limits/memoryMaxBytes", json!(1)),
        ("/sample/usage/limits/cpuQuotaUsec", json!(u64::MAX)),
        ("/sample/current/oomKill", json!(1)),
    ] {
        let mut changed = record.clone();
        *changed.pointer_mut(path).unwrap() = value;
        assert!(verify_measurement(&evidence(changed)).is_err(), "{path}");
    }
    let mut missing = record;
    missing["sample"]["baseline"]["oomKill"] = Value::Null;
    missing["sample"]["current"]["oomKill"] = Value::Null;
    assert!(
        verify_measurement(&evidence(missing)).is_err(),
        "two missing counters are not a clean interval"
    );
}
