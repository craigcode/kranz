//! Test-only receipts: a runtime failure is never an access-denial proof.

use super::ContainerRuntime;
use crate::command_exec::run_bounded_argv;
use std::{collections::HashMap, path::Path, time::Duration};

pub(crate) struct AccessProbe {
    witness: std::path::PathBuf,
    nonce: String,
    receipt: String,
}

impl AccessProbe {
    pub(crate) fn new(scratch: &Path, name: &str) -> Self {
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        Self {
            witness: scratch.join(format!("denial-{nonce}.started")),
            receipt: format!("KRANZ_ACCESS:{nonce}:{name}"),
            nonce,
        }
    }

    /// The shell exits successfully only after observing the expected failure.
    /// Diagnostic matching excludes missing tools, syntax errors and unrelated
    /// command failures. The host checks a separate, unique start witness.
    pub(crate) fn command(&self, operation: &str, status: i32, diagnostic: &str) -> String {
        let errors = self.witness.with_extension("stderr");
        format!(
            "set -eu; export LC_ALL=C; command -v cat >/dev/null; \
             printf '%s\\n' {nonce} > {witness}; \
             if ( set +e; {operation} ) >/dev/null 2>{errors}; then exit 40; else result=$?; fi; \
             test \"$result\" -eq {status} || exit 41; \
             case \"$(cat {errors})\" in *{diagnostic}*) ;; *) cat {errors}; exit 42;; esac; \
             printf '%s\\n' {receipt}",
            nonce = quote(&self.nonce),
            witness = quote(&self.witness.display().to_string()),
            errors = quote(&errors.display().to_string()),
            diagnostic = quote(diagnostic),
            receipt = quote(&self.receipt),
        )
    }

    pub(crate) fn socket_args(&self, mode: &str, address: &str) -> Vec<String> {
        vec![
            "python".into(),
            "-c".into(),
            include_str!("socket_probe.py").into(),
            mode.into(),
            address.into(),
            "18080".into(),
            self.witness.display().to_string(),
            self.nonce.clone(),
            self.receipt.clone(),
        ]
    }

    pub(crate) fn proved(&self, code: Option<i32>, output: &str, cleaned: bool) -> bool {
        code == Some(0)
            && cleaned
            && std::fs::read_to_string(&self.witness).ok().as_deref()
                == Some(&format!("{}\n", self.nonce))
            && output.lines().any(|line| line == self.receipt)
    }
}

pub(crate) fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Run the real production argv with an observation-only label. Confirm an
/// empty, successful inventory after --rm; on failure remove only inspected,
/// full IDs bearing this fixture's random label. Never remove by name or prune.
/// Forced cleanup cannot turn a failed probe into a passing receipt.
pub(crate) async fn run_observed(
    cwd: &Path,
    runtime: ContainerRuntime,
    mut args: Vec<String>,
    timeout: Duration,
) -> (Option<i32>, String, bool) {
    assert_eq!(args.first().map(String::as_str), Some("run"));
    let owner = uuid::Uuid::new_v4().simple().to_string();
    let label = format!("io.kranz.denial-test={owner}");
    args.splice(1..1, ["--label".into(), label.clone()]);
    println!("KRANZ_PROBE_OWNER={label}");
    // Capture the client endpoint settings once for execution and cleanup.
    let env = runtime.client_env();
    let (code, mut output) =
        run_bounded_argv(cwd, Path::new(runtime.binary()), &args, timeout, &env).await;
    let absence = if code == Some(0) {
        wait_empty(cwd, runtime, &env, &label).await
    } else {
        Err("guest did not complete successfully".into())
    };
    let cleaned = absence.is_ok();
    if !cleaned || code != Some(0) {
        let recovery = remove_owned(cwd, runtime, &env, &owner).await;
        output.push_str(&format!(
            "\nprobe failed or cleanup was not confirmed; label={label}; absence={absence:?}; recovery={recovery:?}"
        ));
    }
    (code, output, cleaned)
}

/// Fixture cleanup is successful only after inspected owner IDs are removed
/// and a new inventory proves absence. An unavailable inventory is an error.
pub(crate) async fn remove_owned(
    cwd: &Path,
    runtime: ContainerRuntime,
    env: &HashMap<String, String>,
    owner: &str,
) -> Result<(), String> {
    let label = format!("io.kranz.denial-test={owner}");
    for id in inventory(cwd, runtime, env, &label).await? {
        let (code, output) = run_bounded_argv(
            cwd,
            Path::new(runtime.binary()),
            &[
                "inspect".into(),
                "--format".into(),
                r#"{"Id":{{json .Id}},"Owner":{{json (index .Config.Labels "io.kranz.denial-test")}}}"#.into(),
                id.clone(),
            ],
            Duration::from_secs(5),
            env,
        )
        .await;
        let inspected = serde_json::from_str::<serde_json::Value>(&output).ok();
        if code != Some(0)
            || !inspected.as_ref().is_some_and(|v| {
                v["Id"].as_str() == Some(id.as_str()) && v["Owner"].as_str() == Some(owner)
            })
        {
            return Err(format!("cannot confirm ownership of {id}: {output}"));
        }
        let (code, output) = run_bounded_argv(
            cwd,
            Path::new(runtime.binary()),
            &["rm".into(), "-f".into(), id],
            Duration::from_secs(5),
            env,
        )
        .await;
        if code != Some(0) {
            return Err(format!("owned removal failed: {output}"));
        }
    }
    wait_empty(cwd, runtime, env, &label).await
}

async fn wait_empty(
    cwd: &Path,
    runtime: ContainerRuntime,
    env: &HashMap<String, String>,
    label: &str,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let remaining = inventory(cwd, runtime, env, label).await?;
        if remaining.is_empty() {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!("owned containers remain: {remaining:?}"));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn inventory(
    cwd: &Path,
    runtime: ContainerRuntime,
    env: &HashMap<String, String>,
    label: &str,
) -> Result<Vec<String>, String> {
    let (code, output) = run_bounded_argv(
        cwd,
        Path::new(runtime.binary()),
        &[
            "ps".into(),
            "--all".into(),
            "--quiet".into(),
            "--no-trunc".into(),
            "--filter".into(),
            format!("label={label}"),
        ],
        Duration::from_secs(5),
        env,
    )
    .await;
    if code != Some(0) {
        return Err(format!("inventory failed: {code:?}: {output}"));
    }
    output
        .lines()
        .map(|id| {
            if id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
                Ok(id.to_owned())
            } else {
                Err("inventory did not return full container IDs".into())
            }
        })
        .collect()
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn denial_receipt_rejects_startup_timeout_and_incomplete_evidence() {
    let _env = crate::agent_env::EnvTestGuard::engage(&[]);
    use crate::command_exec::{run_shell_command_sandboxed_with_code, GateSandbox};
    let dir = tempfile::tempdir().unwrap();
    let probe = AccessProbe::new(dir.path(), "synthetic-hidden-read");
    let missing = dir.path().join("absent");
    let command = probe.command(
        &format!("cat {}", quote(&missing.display().to_string())),
        1,
        "No such file or directory",
    );
    let env = crate::agent_env::contract_command_env(dir.path(), None, &[]);
    let run = |command: String, timeout| {
        let env = env.clone();
        let cwd = dir.path().to_path_buf();
        async move {
            run_shell_command_sandboxed_with_code(
                &cwd,
                &command,
                timeout,
                &env,
                &GateSandbox::Disabled,
            )
            .await
        }
    };
    let (code, output) = run(command.clone(), Duration::from_secs(5)).await;
    assert!(probe.proved(code, &output, true));
    assert!(!probe.proved(code, &output, false), "cleanup is mandatory");
    assert!(
        !probe.proved(None, &output, true),
        "even a receipt cannot excuse supervision failure"
    );
    std::fs::remove_file(&probe.witness).unwrap();
    assert!(
        !probe.proved(code, &output, true),
        "output alone is insufficient"
    );

    let (code, output) = run("exit 125".into(), Duration::from_secs(5)).await;
    assert_eq!(code, Some(125));
    assert!(!probe.proved(code, &output, true));
    // Emit a valid receipt before hanging: a timeout must still fail closed.
    let (code, output) = run(format!("{command}; sleep 30"), Duration::from_millis(100)).await;
    assert_eq!(code, None);
    assert!(!probe.proved(code, &output, true));
    // Missing executable and unexpected success must not produce a denial.
    for operation in [
        "/kranz-no-such-command".to_string(),
        "true".to_string(),
        "false".to_string(),
    ] {
        let command = probe.command(&operation, 1, "No such file or directory");
        let (code, output) = run(command, Duration::from_secs(5)).await;
        assert!(
            !probe.proved(code, &output, true),
            "operation={operation}; code={code:?}; output={output}"
        );
    }
    std::fs::write(&missing, "allowed").unwrap();
    let (code, output) = run(command, Duration::from_secs(5)).await;
    assert_eq!(
        code,
        Some(40),
        "an allowed read must reject the denial: {output}"
    );
    assert!(!probe.proved(code, &output, true));
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn socket_denial_receipt_rejects_arbitrary_errors_and_unexpected_success() {
    let _env = crate::agent_env::EnvTestGuard::engage(&[]);
    let cwd = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let env = HashMap::from([
        ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
        ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
    ]);
    let (code, output) = run_bounded_argv(
        cwd,
        Path::new("python3"),
        &["src/sandbox_container/socket_probe_test.py".into()],
        Duration::from_secs(10),
        &env,
    )
    .await;
    assert_eq!(code, Some(0), "{output}");
    assert!(output.contains("Ran 6 tests"), "{output}");
}

#[tokio::test]
async fn denial_cleanup_requires_owned_full_ids_and_confirmed_absence() {
    use std::os::unix::fs::PermissionsExt;
    for case in ["good", "unowned", "unavailable", "malformed", "persists"] {
        let root = tempfile::tempdir().unwrap();
        let cli = root.path().join("docker");
        let removed = root.path().join("removed");
        let id = "a".repeat(64);
        let owner = "fixture-owner";
        let inspection = serde_json::json!({
            "Id": id,
            "Owner": if case == "unowned" { "somebody-else" } else { owner }
        });
        std::fs::write(
            &cli,
            format!(
                "#!/bin/sh\ncase \"$1\" in\n\
             ps)\n [ {case} != unavailable ] || exit 125\n\
             if [ {case} = malformed ]; then printf 'short-id\\n'; exit 0; fi\n\
             if [ {case} = persists ] || [ ! -e {removed} ]; then printf '%s\\n' {id}; fi;;\n\
             inspect)\n [ \"$2\" = --format ] && [ \"$4\" = {id} ] || exit 127\n\
             printf '%s\\n' {inspection};;\n\
             rm) printf '%s\\n' \"$@\" > {removed};;\n\
             *) exit 126;;\nesac\n",
                removed = quote(&removed.display().to_string()),
                inspection = quote(&inspection.to_string()),
            ),
        )
        .unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700)).unwrap();
        let env = HashMap::from([("PATH".into(), root.path().display().to_string())]);
        let result = remove_owned(root.path(), ContainerRuntime::Docker, &env, owner).await;
        assert_eq!(result.is_ok(), case == "good", "{case}: {result:?}");
        if matches!(case, "good" | "persists") {
            assert_eq!(
                std::fs::read_to_string(&removed).unwrap(),
                format!("rm\n-f\n{id}\n")
            );
        } else {
            assert!(!removed.exists(), "{case} must never authorize removal");
        }
    }
}
