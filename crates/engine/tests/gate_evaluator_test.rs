//! Schema/pinning tests run everywhere. Containment tests require an explicit
//! Docker test opt-in; the dedicated CI job must not skip them.
use kranz_engine::gate_evaluation::{evidence::FrozenEvidence, protocol::*};
use kranz_engine::git_ops::GitRepo;
use kranz_engine::pack::{evaluator::PinnedRegistration, Pack};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const IMAGE: &str =
    "python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d";
const CHECKER: &str = include_str!("fixtures/gate-evaluator/checker.py");
fn manifest(mode: &str, root: &Path) -> String {
    format!("[pack]\nname = 'synthetic-checker'\nschema = 5\n[[evaluator]]\nname = 'synthetic-checker'\nimage = '{IMAGE}'\nexecutable = '/usr/local/bin/python3'\nargs = ['-I','-S','/checker/checker.py','{mode}','{}','{}']\nfiles = ['checker.py']\nstages = ['plan-approval']\nevidence = ['source']\nkind = 'mechanical'\nenforcement = 'blocking'\n", root.join("serve.token").display(), root.join("candidate.txt").display())
}
fn git(root: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
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
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
struct Fixture {
    tmp: tempfile::TempDir,
    registration: PinnedRegistration,
    evidence: FrozenEvidence,
}
impl Fixture {
    fn new(mode: &str, shared: bool) -> Self {
        let tmp = if shared {
            let base =
                PathBuf::from(std::env::var_os("HOME").expect("HOME required for Docker fixture"))
                    .join(".cache/kranz/evaluator-tests");
            std::fs::create_dir_all(&base).unwrap();
            tempfile::tempdir_in(base).unwrap()
        } else {
            tempfile::tempdir().unwrap()
        };
        let root = tmp.path();
        std::fs::write(root.join("pack.toml"), manifest(mode, root)).unwrap();
        std::fs::write(root.join("checker.py"), CHECKER).unwrap();
        std::fs::write(root.join("serve.token"), "synthetic authority canary").unwrap();
        std::fs::write(root.join("candidate.txt"), "synthetic candidate canary").unwrap();
        git(root, &["init", "-q"]);
        git(root, &["add", "pack.toml", "checker.py"]);
        git(root, &["commit", "-qm", "trusted checker"]);
        let repo = GitRepo::open(root).unwrap();
        let registration =
            PinnedRegistration::at_ref(&repo, "HEAD", "", "synthetic-checker").unwrap();
        let evidence = evidence(&registration, None).unwrap();
        Self {
            tmp,
            registration,
            evidence,
        }
    }
}
type EvidenceMutation = fn(&mut Value, &mut Vec<Value>, &mut BTreeMap<Id, Vec<u8>>);
fn evidence(
    reg: &PinnedRegistration,
    mutation: Option<EvidenceMutation>,
) -> Result<FrozenEvidence, String> {
    let plan = b"{\"goal\":\"synthetic only\"}".to_vec();
    let policy = b"{\"humanConsentRequired\":true}".to_vec();
    let subject = json!({"kind":"plan","revision":1,"planDigest":Digest::of(&plan),"baseCommit":{"algorithm":"sha1","value":"a".repeat(40)}});
    let subject_bytes = serde_json::to_vec(&subject).unwrap();
    let mut inputs = BTreeMap::new();
    let mut artifacts = Vec::new();
    for (id, role, bytes) in [
        ("a-plan", "plan", plan.clone()),
        ("b-policy", "policy", policy.clone()),
        ("c-registration", "registration", reg.bytes().to_vec()),
        ("d-subject", "subject", subject_bytes.clone()),
        ("e-source", "source", b"one\r\ntwo\n".to_vec()),
    ] {
        inputs.insert(Id::try_from(id.to_string()).unwrap(), bytes.clone());
        artifacts.push(json!({"id":id,"role":role,"content":{"path":format!("inputs/{id}.json"),"bytes":bytes.len(),"digest":Digest::of(&bytes)},"producer":{"kind":"engine"}}));
    }
    let binding = json!({"subjectDigest":Digest::of(&subject_bytes),"planDigest":Digest::of(&plan),"policyDigest":Digest::of(&policy),"registrationDigest":reg.digest(),"workspaceId":"workspace-1"});
    let mut request = json!({"jsonrpc":"2.0","id":"attempt-1","method":"gate/evaluate","params":{"schemaVersion":1,"evaluationId":"evaluation-1","attemptId":"attempt-1","gateId":"synthetic-checker","missionId":"mission-1","stage":"plan-approval","subject":subject,"binding":binding,"deadline":(chrono::Utc::now()+chrono::Duration::minutes(2)).format("%Y-%m-%dT%H:%M:%SZ").to_string(),"limits":{"wallTimeMs":30000,"writeTimeMs":1000,"maxFrameBytes":1048576,"maxStdoutBytes":1048576,"maxStderrBytes":1048576,"maxArtifactBytes":67108864,"maxArtifacts":128}}});
    if let Some(mutation) = mutation {
        mutation(&mut request, &mut artifacts, &mut inputs);
    }
    let manifest = serde_json::to_vec(&json!({"schemaVersion":1,"missionId":"mission-1","binding":request["params"]["binding"],"artifacts":artifacts})).unwrap();
    request["params"]["evidence"] = json!({"path":"inputs/manifest.json","digest":Digest::of(&manifest),"bytes":manifest.len()});
    FrozenEvidence::new(serde_json::to_vec(&request).unwrap(), manifest, inputs, reg)
}

#[test]
fn gate_subprocess_v1_pins_script_bytes_at_approved_git_ref() {
    let f = Fixture::new("pass", false);
    let repo = GitRepo::open(f.tmp.path()).unwrap();
    let old = f.registration.digest();
    std::fs::write(
        f.tmp.path().join("checker.py"),
        "raise RuntimeError('worker substitution')",
    )
    .unwrap();
    let pin = PinnedRegistration::at_ref(&repo, "HEAD", "", "synthetic-checker").unwrap();
    assert_eq!(old, pin.digest());
    git(f.tmp.path(), &["add", "checker.py"]);
    git(
        f.tmp.path(),
        &["commit", "-qm", "approved checker revision"],
    );
    assert_ne!(
        old,
        PinnedRegistration::at_ref(&repo, "HEAD", "", "synthetic-checker")
            .unwrap()
            .digest()
    );
}
#[test]
fn gate_subprocess_v1_pack_schema_and_mission_load_fail_closed() {
    let f = Fixture::new("pass", false);
    let text = manifest("pass", f.tmp.path());
    let pack = Pack::load(f.tmp.path()).unwrap().unwrap();
    assert_eq!(pack.evaluators.len(), 1);
    let config = kranz_engine::types::MissionConfig {
        pack_dir: Some(f.tmp.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert!(kranz_engine::pack::load_for_config(&config, f.tmp.path())
        .unwrap_err()
        .contains("S5"));
    for invalid in [
        text.replace("schema = 5", "schema = 4"),
        text.replace("schema = 5", "schema = 6"),
        text.replace(IMAGE, "python:latest"),
        text.replace("kind = 'mechanical'", "kind = 'human'"),
        text.replace("files = ['checker.py']", "files = ['../checker.py']"),
        text.replace("[[evaluator]]", "[evaluator]"),
    ] {
        std::fs::write(f.tmp.path().join("pack.toml"), invalid).unwrap();
        assert!(Pack::load(f.tmp.path()).is_err());
    }
}
#[test]
fn gate_subprocess_v1_rejects_incomplete_or_forged_authority_evidence() {
    let f = Fixture::new("pass", false);
    assert!(evidence(
        &f.registration,
        Some(|_, artifacts, inputs| {
            let artifact = artifacts.remove(1);
            inputs.remove(&serde_json::from_value(artifact["id"].clone()).unwrap());
        })
    )
    .is_err());
    assert!(evidence(
        &f.registration,
        Some(|_, artifacts, _| artifacts[1]["producer"]["kind"] = json!("worker"))
    )
    .is_err());
    assert!(evidence(
        &f.registration,
        Some(|request, _, _| request["params"]["subject"]["revision"] = json!(2))
    )
    .is_err());
    assert!(evidence(
        &f.registration,
        Some(|_, _, inputs| {
            inputs.insert(
                Id::try_from("a-plan".to_string()).unwrap(),
                b"changed".to_vec(),
            );
        })
    )
    .is_err());
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
mod container {
    use super::*;
    use kranz_engine::gate_evaluation::subprocess::{DockerEvaluator, RunOptions};
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::Duration;
    fn docker() -> Option<DockerEvaluator> {
        if std::env::var("KRANZ_GATE_CONTAINER_TESTS").as_deref() != Ok("1") {
            eprintln!("SKIP-EXTERNAL-EVALUATOR: explicit Docker test opt-in is absent");
            return None;
        }
        let executable = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .map(|p| p.join("docker"))
            .find(|p| p.is_file())
            .expect("Docker is required by KRANZ_GATE_CONTAINER_TESTS=1");
        Some(DockerEvaluator::new(&executable).unwrap())
    }
    async fn run(
        client: &DockerEvaluator,
        f: &Fixture,
    ) -> kranz_engine::gate_evaluation::subprocess::AttemptOutcome {
        client
            .evaluate(
                &f.registration,
                &f.evidence,
                RunOptions {
                    attempt_parent: f.tmp.path(),
                    retain_private_inputs: true,
                },
                &AtomicBool::new(false),
            )
            .await
            .unwrap()
    }
    #[tokio::test]
    async fn gate_subprocess_v1_container_accepts_synthetic_checker_and_denies_host_access() {
        let Some(client) = docker() else {
            return;
        };
        for mode in [
            "containment",
            "artifact",
            "finding",
            "fail",
            "escalate",
            "descendant",
        ] {
            let f = Fixture::new(mode, true);
            // A dirty worker script must never replace the approved checker.
            std::fs::write(
                f.tmp.path().join("checker.py"),
                "raise RuntimeError('worker code')",
            )
            .unwrap();
            let outcome = run(&client, &f).await;
            let accepted = outcome
                .evaluation
                .unwrap_or_else(|e| panic!("{mode}: {e}; {}", outcome.directory.display()));
            assert!(accepted.container_removed);
            assert_eq!(accepted.exit_code, 0);
            if mode == "fail" {
                assert_eq!(accepted.result.verdict, Some(Verdict::Fail));
            }
            if mode == "escalate" {
                assert_eq!(accepted.result.status, Status::Escalate);
                assert_eq!(accepted.result.verdict, None);
            }
            assert_eq!(
                std::fs::read_to_string(f.tmp.path().join("candidate.txt")).unwrap(),
                "synthetic candidate canary"
            );
            assert_eq!(
                std::fs::read_to_string(f.tmp.path().join("serve.token")).unwrap(),
                "synthetic authority canary"
            );
            if mode == "descendant" {
                tokio::time::sleep(Duration::from_secs(3)).await;
                assert!(!outcome.directory.join("outputs/escaped.txt").exists());
            }
        }
    }
    #[tokio::test]
    async fn gate_subprocess_v1_container_refuses_untrusted_terminal_claims_and_artifacts() {
        let Some(client) = docker() else {
            return;
        };
        for mode in [
            "nonzero",
            "duplicate",
            "duplicate-key",
            "no-newline",
            "missing",
            "overflow",
            "stderr-overflow",
            "wrong-binding",
            "wrong-id",
            "authority",
            "version",
            "bad-line",
            "symlink",
            "parent-symlink",
            "fifo",
            "hardlink",
            "wrong-hash",
            "binary",
            "traversal",
        ] {
            let f = Fixture::new(mode, true);
            let outcome = run(&client, &f).await;
            let error = outcome
                .evaluation
                .as_ref()
                .expect_err("invalid claim accepted");
            let expected = match mode {
                "nonzero" => "unsuccessfully",
                "duplicate" => "duplicate response",
                "duplicate-key" => "invalid gate JSON",
                "no-newline" | "missing" => "NDJSON newline",
                "overflow" | "stderr-overflow" => "stream byte limit",
                "wrong-binding" | "wrong-id" => "correlation",
                "authority" | "traversal" => "wire shape",
                "version" => "invalid judged/escalate",
                "bad-line" => "finding line",
                "symlink" => "following links",
                "parent-symlink" => "real directory",
                "fifo" => "regular file",
                "hardlink" => "hard-linked",
                "wrong-hash" => "digest",
                "binary" => "binary output",
                _ => unreachable!(),
            };
            assert!(
                error.contains(expected),
                "{mode}: expected {expected}, got {error}"
            );
            let receipt: Value = serde_json::from_slice(
                &std::fs::read(outcome.directory.join("receipt.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(receipt["containerRemoved"], true, "{mode}: {receipt}");
        }
    }
    async fn wait_started(parent: &Path) -> PathBuf {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                for entry in std::fs::read_dir(parent).unwrap().flatten() {
                    let root = entry.path();
                    if root.join("outputs/started.txt").is_file() {
                        return root;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("checker and escaped child did not start")
    }
    #[tokio::test]
    async fn gate_subprocess_v1_container_drop_removes_namespace_and_default_retention_scrubs() {
        let Some(client) = docker() else {
            return;
        };
        let f = Fixture::new("drop", true);
        let cancel = AtomicBool::new(false);
        let mut future = Box::pin(client.evaluate(
            &f.registration,
            &f.evidence,
            RunOptions {
                attempt_parent: f.tmp.path(),
                retain_private_inputs: true,
            },
            &cancel,
        ));
        let root = tokio::select! {
            root = wait_started(f.tmp.path()) => root,
            result = &mut future => panic!("drop fixture ended early: {result:?}"),
        };
        drop(future);
        let ledger: Value =
            serde_json::from_slice(&std::fs::read(root.join("container.json")).unwrap()).unwrap();
        let name = ledger["name"].as_str().unwrap();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let output = std::process::Command::new("docker")
                    .env_clear()
                    .env("PATH", std::env::var_os("PATH").unwrap_or_default())
                    .env("HOME", std::env::var_os("HOME").unwrap_or_default())
                    .args([
                        "container",
                        "ls",
                        "--all",
                        "--filter",
                        &format!("name=^/{name}$"),
                        "--format",
                        "{{.ID}}",
                    ])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "cannot verify Docker cleanup");
                if output.stdout.is_empty() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("dropped evaluator container survived");
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!root.join("outputs/escaped.txt").exists());
        let clean = Fixture::new("redaction", true);
        let outcome = client
            .evaluate(
                &clean.registration,
                &clean.evidence,
                RunOptions {
                    attempt_parent: clean.tmp.path(),
                    retain_private_inputs: false,
                },
                &cancel,
            )
            .await
            .unwrap();
        let accepted = outcome.evaluation.as_ref().unwrap();
        assert_ne!(
            accepted.artifacts[0].source.digest,
            accepted.artifacts[0].retained_digest
        );
        assert!(
            !String::from_utf8_lossy(&accepted.artifacts[0].retained_bytes)
                .contains(&"A".repeat(36))
        );
        assert!(!accepted.result.rationale.contains(&"A".repeat(36)));
        assert!(outcome.directory.join("receipt.json").is_file());
        assert!(!outcome.directory.join("inputs").exists());
        assert!(!outcome.directory.join("outputs").exists());
        assert!(!outcome.directory.join("raw-stdout.ndjson").exists());
    }
    #[tokio::test]
    async fn gate_subprocess_v1_container_timeout_and_cancel_kill_session_escaped_children() {
        let Some(client) = docker() else {
            return;
        };
        for mode in ["cancel", "stall-input"] {
            let mut f = Fixture::new(mode, true);
            if mode == "stall-input" {
                f.evidence = evidence(
                    &f.registration,
                    Some(|request, _, _| request["params"]["limits"]["wallTimeMs"] = json!(1800)),
                )
                .unwrap();
            }
            let cancelled = Arc::new(AtomicBool::new(false));
            let signal = cancelled.clone();
            let parent = f.tmp.path().to_path_buf();
            let cancellation = tokio::spawn(async move {
                if mode == "cancel" {
                    wait_started(&parent).await;
                    signal.store(true, Ordering::Release);
                }
            });
            let outcome = client
                .evaluate(
                    &f.registration,
                    &f.evidence,
                    RunOptions {
                        attempt_parent: f.tmp.path(),
                        retain_private_inputs: true,
                    },
                    &cancelled,
                )
                .await
                .unwrap();
            cancellation.await.unwrap();
            assert!(outcome.evaluation.is_err());
            tokio::time::sleep(Duration::from_secs(3)).await;
            assert!(!outcome.directory.join("outputs/escaped.txt").exists());
            let receipt: Value = serde_json::from_slice(
                &std::fs::read(outcome.directory.join("receipt.json")).unwrap(),
            )
            .unwrap();
            assert_eq!(receipt["containerRemoved"], true);
        }
    }
}
