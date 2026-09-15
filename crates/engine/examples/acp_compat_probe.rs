//! Explicit, single-prompt ACP compatibility probe. See docs/acp-compatibility.md.
//! Building or running --check never starts an adapter. --run requires a fresh
//! receipt path and an operator-approved call budget; it never retries.
use anyhow::{bail, Context, Result};
use kranz_engine::backend::{AgentBackend, AgentEvent, PromptMode, SessionExit, SessionSpec};
use kranz_engine::backend_acp::AcpBackend;
use kranz_engine::runner::parse_worker_report;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const REPORT: &str = r#"{"result":"partial","summary":"kranz-acp-live-fixture-v1","filesTouched":[],"testsAdded":[],"dependenciesAdded":[],"knownGaps":["Protocol fixture only; no feature implemented or mission completion claimed."],"commits":[],"commandsRun":[],"escalation":null,"questions":[]}"#;
const PREFIX: &str = "Protocol compatibility fixture only. Do not use any tools, read files, change files, call the network, create commits, or carry out another task. Return exactly this JSON object as the final assistant message, without Markdown fences:\n";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    provider: String,
    program: PathBuf,
    args: Vec<String>,
    credential_env: Option<String>,
    receipt: PathBuf,
}

impl Config {
    fn validate(&self) -> Result<()> {
        let valid = match self.provider.as_str() {
            "claude" => self.credential_env.as_deref() == Some("ANTHROPIC_API_KEY"),
            "codex" => matches!(
                self.credential_env.as_deref(),
                Some("CODEX_API_KEY" | "OPENAI_API_KEY")
            ),
            "fixture" => self.credential_env.is_none(),
            _ => false,
        };
        if !valid {
            bail!("provider/credential channel is unsupported by this probe");
        }
        if !self.program.is_absolute() || !self.program.is_file() || !self.receipt.is_absolute() {
            bail!("program and new receipt path must be absolute; program must exist");
        }
        Ok(())
    }
}

fn clean_text(text: &str, credential: Option<&str>, private_root: &str) -> String {
    let text = match credential {
        Some(value) if !value.is_empty() => text.replace(value, "[PROBE_CREDENTIAL]"),
        _ => text.to_string(),
    };
    kranz_engine::scrub::scrub(&text.replace(private_root, "[PROBE_ROOT]"))
}

fn append_receipt(
    file: &mut std::fs::File,
    value: Value,
    credential: Option<&str>,
    root: &str,
) -> Result<()> {
    let text = clean_text(&serde_json::to_string(&value)?, credential, root);
    // scrub preserves JSON in ordinary cases; do not persist ambiguous output.
    let _: Value = serde_json::from_str(&text).context("receipt redaction damaged JSON")?;
    writeln!(file, "{text}")?;
    file.flush()?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 || !matches!(args[1].as_str(), "--check" | "--run") {
        bail!("usage: acp_compat_probe --check|--run /absolute/config.json");
    }
    let config: Config = serde_json::from_slice(&std::fs::read(&args[2])?)?;
    config.validate()?;
    let credential_present = config
        .credential_env
        .as_ref()
        .is_none_or(|key| std::env::var_os(key).is_some_and(|value| !value.is_empty()));
    if args[1] == "--check" {
        println!(
            "{}",
            json!({"adapterStarted":false,"provider":config.provider,"credentialVariable":config.credential_env,"credentialPresent":credential_present,"receiptAvailable":!config.receipt.exists(),"promptLimit":1,"promptSeconds":120,"overallSeconds":180,"hardDollarCap":false})
        );
        return Ok(());
    }
    if !credential_present {
        bail!("approved credential variable is absent; adapter was not started");
    }
    let credential = config
        .credential_env
        .as_ref()
        .map(std::env::var)
        .transpose()?;
    let private = tempfile::tempdir()?;
    let root = private.path().display().to_string();
    let workspace = private.path().join("workspace");
    let home = private.path().join("home");
    std::fs::create_dir(&workspace)?;
    std::fs::create_dir(&home)?;
    let mut env = HashMap::from([("HOME".into(), home.display().to_string())]);
    if let (Some(key), Some(value)) = (&config.credential_env, &credential) {
        env.insert(key.clone(), value.clone());
    }
    if config.provider == "codex" {
        env.insert(
            "DEFAULT_AUTH_REQUEST".into(),
            r#"{"methodId":"api-key"}"#.into(),
        );
        env.insert("NO_BROWSER".into(), "1".into());
        env.insert("INITIAL_AGENT_MODE".into(), "read-only".into());
    }
    let engine_id = format!("acp-probe-{}", uuid::Uuid::new_v4());
    let effective = kranz_engine::agent_env::agent_session_env(&env, &engine_id, None);
    let mut env_keys: Vec<_> = effective.keys().cloned().collect();
    env_keys.sort();
    let mut open = std::fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.mode(0o600);
    }
    let mut receipt = open
        .open(&config.receipt)
        .context("receipt must be new; this probe never retries an attempt")?;
    let started = Instant::now();
    append_receipt(
        &mut receipt,
        json!({"event":"probe.started","provider":config.provider,"engineSessionId":engine_id,"platform":std::env::consts::OS,"arch":std::env::consts::ARCH,"program":config.program,"args":config.args,"environmentKeys":env_keys,"prompt":format!("{PREFIX}{REPORT}"),"promptLimit":1,"promptSeconds":120,"overallSeconds":180,"hardDollarCap":false,"proof":"basic_text_report_only"}),
        credential.as_deref(),
        &root,
    )?;
    let spec = SessionSpec {
        cwd: workspace.clone(),
        prompt: PromptMode::SingleShot(format!("{PREFIX}{REPORT}")),
        append_system_prompt: None,
        model: "unselected-probe".into(),
        effort: String::new(),
        session_id: engine_id,
        resume: None,
        permission_mode: None,
        allowed_tools: vec![],
        disallowed_tools: [
            "Bash",
            "Read",
            "Write",
            "Edit",
            "NotebookEdit",
            "Glob",
            "Grep",
            "WebSearch",
            "WebFetch",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        tools: vec![],
        writable: false,
        settings_json: None,
        json_schema: None,
        max_budget_usd: None,
        max_turns: None,
        env,
        sandbox: None,
        hook_status: None,
    };
    let mut events = 0usize;
    let mut captured_bytes = 0usize;
    let run = async {
        let mut session = AcpBackend::new(&config.program, config.args.clone())
            .start(spec)
            .await?;
        let mut final_text = None;
        let turn = async {
            while let Some(event) = session.next_event().await? {
                events += 1;
                let (kind, raw, tool, result) = match event {
                    AgentEvent::Init { raw, .. } => ("init", raw, false, None),
                    AgentEvent::Text { raw, .. } => ("text", raw, false, None),
                    AgentEvent::Other { raw } => ("other", raw, false, None),
                    AgentEvent::ToolUse { raw, .. } | AgentEvent::ToolResult { raw, .. } => {
                        ("tool", raw, true, None)
                    }
                    AgentEvent::Result {
                        text,
                        is_error,
                        cost_usd,
                        raw,
                        ..
                    } => (
                        "result",
                        json!({"raw":raw,"costUsd":cost_usd,"isError":is_error}),
                        false,
                        Some((text, is_error)),
                    ),
                };
                captured_bytes += serde_json::to_vec(&raw)?.len();
                if events > 512 || captured_bytes > 2 * 1024 * 1024 {
                    bail!("probe output exceeded its capture budget");
                }
                append_receipt(
                    &mut receipt,
                    json!({"event":kind,"raw":raw}),
                    credential.as_deref(),
                    &root,
                )?;
                if tool {
                    bail!("tool activity invalidates this no-tools fixture");
                }
                if let Some((text, is_error)) = result {
                    if is_error {
                        bail!("prompt did not finish with end_turn");
                    }
                    final_text = Some(text);
                }
            }
            Result::<()>::Ok(())
        };
        let turn_result = tokio::time::timeout(Duration::from_secs(120), turn).await;
        if !matches!(turn_result, Ok(Ok(()))) {
            let _ = session.abort().await;
        }
        turn_result.context("prompt exceeded 120 seconds")??;
        if session.exit_status() != Some(SessionExit::Completed) {
            bail!(
                "adapter did not complete cleanly: {:?}",
                session.exit_status()
            );
        }
        let text = final_text.context("no terminal report")?;
        let report = parse_worker_report(&text).context("engine did not parse WorkerReport")?;
        if serde_json::from_str::<Value>(&text)? != serde_json::from_str::<Value>(REPORT)?
            || report.summary != "kranz-acp-live-fixture-v1"
        {
            bail!("report differs from the fixed fixture");
        }
        if std::fs::read_dir(&workspace)?.next().is_some() {
            bail!("probe workspace changed");
        }
        Result::<()>::Ok(())
    };
    let result = tokio::time::timeout(Duration::from_secs(180), run)
        .await
        .context("session exceeded 180 seconds")
        .and_then(|result| result);
    append_receipt(
        &mut receipt,
        json!({"event":"probe.finished","passed":result.is_ok(),"elapsedMillis":started.elapsed().as_millis(),"events":events,"capturedBytes":captured_bytes,"error":result.as_ref().err().map(|error|format!("{error:#}")),"productionReadinessProven":false,"containmentProven":false}),
        credential.as_deref(),
        &root,
    )?;
    if result.is_err() {
        bail!("probe failed; see the redacted receipt (no retry was attempted)");
    }
    println!(
        "Basic ACP report probe passed; production readiness and containment remain unproven."
    );
    Ok(())
}
