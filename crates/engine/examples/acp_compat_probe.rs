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
    /// Explicit opt-in to minimal native login seeding; never the child HOME.
    native_login_home: Option<PathBuf>,
    /// Explicit operator consent for the Claude CLI to use its macOS Keychain.
    #[serde(default)]
    allow_keychain: bool,
    receipt: PathBuf,
}

impl Config {
    fn validate(&self) -> Result<()> {
        let native = self.native_login_home.is_some();
        let valid = match self.provider.as_str() {
            "claude" => {
                matches!(
                    self.credential_env.as_deref(),
                    Some("ANTHROPIC_API_KEY" | "CLAUDE_CODE_OAUTH_TOKEN")
                ) || native
            }
            "codex" => {
                matches!(
                    self.credential_env.as_deref(),
                    Some("CODEX_API_KEY" | "OPENAI_API_KEY")
                ) || native
            }
            "fixture" => self.credential_env.is_none() && !native,
            _ => false,
        };
        if !valid || (native && self.credential_env.is_some()) {
            bail!("provider/credential channel is unsupported by this probe");
        }
        if self.allow_keychain
            && (!cfg!(target_os = "macos") || self.provider != "claude" || !native)
        {
            bail!("allowKeychain requires an explicit Claude native login on macOS");
        }
        if self
            .native_login_home
            .as_ref()
            .is_some_and(|p| !p.is_absolute() || !p.is_dir())
        {
            bail!("nativeLoginHome must name an existing absolute operator home");
        }
        if !self.program.is_absolute() || !self.program.is_file() || !self.receipt.is_absolute() {
            bail!("program and new receipt path must be absolute; program must exist");
        }
        Ok(())
    }
}

fn seed_native_login(
    config: &Config,
    private: &std::path::Path,
    env: &mut HashMap<String, String>,
) -> Result<()> {
    let Some(source_home) = &config.native_login_home else {
        return Ok(());
    };
    if config.provider == "claude" {
        if config.allow_keychain {
            if !source_home.join("Library/Keychains").is_dir() {
                bail!("authorized native Keychain directory is absent");
            }
            kranz_engine::backend_claude::seed_worker_scratch_home(
                private,
                Some(source_home),
                None,
            )?;
            // As with the native backend, an explicit CLAUDE_CONFIG_DIR
            // changes the CLI's credential lookup. HOME remains disposable.
            return Ok(());
        }
        // Without explicit consent, only an existing credential file is used.
        if !source_home.join(".claude/.credentials.json").is_file() {
            bail!(
                "Claude file-based login is absent; Keychain access requires explicit allowKeychain consent on macOS"
            );
        }
        let (_, config_dir) = kranz_engine::backend_claude::seed_worker_scratch_home(
            private,
            None,
            Some(&source_home.join(".claude")),
        )?;
        env.insert("CLAUDE_CONFIG_DIR".into(), config_dir.display().to_string());
    } else {
        let source = source_home.join(".codex/auth.json");
        let metadata =
            std::fs::symlink_metadata(&source).context("native Codex auth.json is absent")?;
        if !metadata.is_file() || metadata.len() > 1024 * 1024 {
            bail!("native Codex auth.json must be a bounded regular file");
        }
        let dest = private.join("home/.codex");
        std::fs::create_dir_all(&dest)?;
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(dest.join("auth.json"))?;
        file.write_all(&std::fs::read(source)?)?;
        env.insert("CODEX_HOME".into(), dest.display().to_string());
    }
    Ok(())
}

#[cfg(test)]
mod native_login_tests {
    use super::*;

    fn config(root: &std::path::Path, provider: &str) -> Config {
        Config {
            provider: provider.into(),
            program: std::env::current_exe().unwrap(),
            args: vec![],
            credential_env: None,
            native_login_home: Some(root.to_owned()),
            allow_keychain: false,
            receipt: root.join("new-receipt.jsonl"),
        }
    }

    #[test]
    fn native_login_requires_explicit_exclusive_auth_source() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config(root.path(), "codex");
        cfg.validate().unwrap();
        cfg.credential_env = Some("OPENAI_API_KEY".into());
        assert!(cfg.validate().is_err());
        cfg.credential_env = None;
        cfg.provider = "fixture".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn claude_oauth_requires_an_explicit_credential_channel() {
        let root = tempfile::tempdir().unwrap();
        let mut cfg = config(root.path(), "claude");
        cfg.credential_env = Some("CLAUDE_CODE_OAUTH_TOKEN".into());
        assert!(
            cfg.validate().is_err(),
            "native and explicit auth must not mix"
        );
        cfg.native_login_home = None;
        cfg.validate().unwrap();
        cfg.provider = "codex".into();
        assert!(cfg.validate().is_err());
        cfg.provider = "claude".into();
        cfg.credential_env = Some("ARBITRARY_SECRET".into());
        assert!(cfg.validate().is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn native_keychain_requires_consent_and_keeps_the_private_home() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("Library/Keychains")).unwrap();
        let private = tempfile::tempdir().unwrap();
        let mut cfg = config(source.path(), "claude");
        let mut env = HashMap::new();
        assert!(seed_native_login(&cfg, private.path(), &mut env).is_err());
        assert!(!private.path().join("home/Library/Keychains").exists());
        cfg.allow_keychain = true;
        cfg.validate().unwrap();
        seed_native_login(&cfg, private.path(), &mut env).unwrap();
        assert_eq!(
            std::fs::read_link(private.path().join("home/Library/Keychains")).unwrap(),
            source.path().join("Library/Keychains")
        );
        assert!(!env.contains_key("CLAUDE_CONFIG_DIR"));
        assert!(!env.contains_key("HOME"));
        cfg.provider = "codex".into();
        assert!(cfg.validate().is_err());
        cfg.provider = "claude".into();
        cfg.native_login_home = None;
        cfg.credential_env = Some("ANTHROPIC_API_KEY".into());
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn native_login_seeds_credentials_without_settings_or_history() {
        let source = tempfile::tempdir().unwrap();
        for provider in ["codex", "claude"] {
            let dir = source.path().join(format!(".{provider}"));
            std::fs::create_dir(&dir).unwrap();
            let name = if provider == "codex" {
                "auth.json"
            } else {
                ".credentials.json"
            };
            std::fs::write(dir.join(name), b"opaque-fixture-credential").unwrap();
            for unwanted in ["settings.json", "config.toml", "history.jsonl"] {
                std::fs::write(dir.join(unwanted), b"must-not-cross").unwrap();
            }
            let private = tempfile::tempdir().unwrap();
            let mut env = HashMap::new();
            seed_native_login(&config(source.path(), provider), private.path(), &mut env).unwrap();
            let dest = private.path().join(format!("home/.{provider}"));
            assert_eq!(
                std::fs::read(dest.join(name)).unwrap(),
                b"opaque-fixture-credential"
            );
            for unwanted in ["settings.json", "config.toml", "history.jsonl"] {
                assert!(!dest.join(unwanted).exists());
            }
            assert!(!env
                .values()
                .any(|value| value == &source.path().display().to_string()));
        }
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
            json!({"adapterStarted":false,"provider":config.provider,"credentialVariable":config.credential_env,"credentialPresent":config.credential_env.as_ref().map(|_| credential_present),"nativeLogin":config.native_login_home.is_some(),"keychainAuthorized":config.allow_keychain,"nativeLoginStateAvailable":config.native_login_home.as_ref().map(|home| if config.allow_keychain { home.join("Library/Keychains").is_dir() } else { home.join(if config.provider == "codex" { ".codex/auth.json" } else { ".claude/.credentials.json" }).is_file() }),"authenticationVerified":false,"receiptAvailable":!config.receipt.exists(),"promptLimit":1,"promptSeconds":120,"overallSeconds":180,"hardDollarCap":false})
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
    seed_native_login(&config, private.path(), &mut env)?;
    if let (Some(key), Some(value)) = (&config.credential_env, &credential) {
        env.insert(key.clone(), value.clone());
    }
    if config.provider == "codex" {
        if config.credential_env.is_some() {
            env.insert(
                "DEFAULT_AUTH_REQUEST".into(),
                r#"{"methodId":"api-key"}"#.into(),
            );
        }
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
        json!({"event":"probe.started","provider":config.provider,"authentication":if config.native_login_home.is_some() { "existing-cli-login" } else if config.credential_env.as_deref() == Some("CLAUDE_CODE_OAUTH_TOKEN") { "explicit-oauth-token" } else { "api-key-or-fixture" },"keychainAuthorized":config.allow_keychain,"engineSessionId":engine_id,"platform":std::env::consts::OS,"arch":std::env::consts::ARCH,"program":config.program,"args":config.args,"environmentKeys":env_keys,"prompt":format!("{PREFIX}{REPORT}"),"promptLimit":1,"promptSeconds":120,"overallSeconds":180,"hardDollarCap":false,"proof":"basic_text_report_only"}),
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
                    AgentEvent::ToolUse { raw, .. }
                    | AgentEvent::ToolResult { raw, .. }
                    | AgentEvent::PermissionRequested { raw, .. }
                    | AgentEvent::PermissionResponded { raw, .. } => ("tool", raw, true, None),
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
