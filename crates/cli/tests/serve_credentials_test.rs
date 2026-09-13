//! The CLI must publish exactly the credentials its server accepts.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

fn serve(repo: &Path, operator: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_kranz"));
    command
        .arg("--repo")
        .arg(repo)
        .args(["serve", "--host", "127.0.0.1", "--read-auth"])
        .env("KRANZ_HOME", operator)
        .env_remove("KRANZ_TOKEN")
        .env_remove("KRANZ_READ_TOKEN")
        .kill_on_drop(true);
    command
}

#[tokio::test]
async fn serve_rejects_invalid_read_credentials_before_binding_or_publishing() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    let operator = temp.path().join("operator");
    let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = occupied.local_addr().unwrap().port().to_string();
    for from_env in [false, true] {
        for read in ["", "dummy-mutation", "dummy read"] {
            let mut command = serve(&repo, &operator);
            command.args(["--port", &port, "--token", "dummy-mutation"]);
            if from_env {
                command.env("KRANZ_READ_TOKEN", read);
            } else {
                command.args(["--read-token", read]);
            }
            let output = tokio::time::timeout(Duration::from_secs(10), command.output())
                .await
                .expect("invalid credentials must exit promptly")
                .unwrap();
            assert!(!output.status.success());
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.contains("--read-token / KRANZ_READ_TOKEN"),
                "{stderr}"
            );
            assert!(
                !stderr.contains("failed to bind"),
                "validate before binding"
            );
            assert!(
                !stderr.contains("dummy-mutation"),
                "do not echo credentials"
            );
            assert!(
                output.stdout.is_empty(),
                "do not announce an invalid server"
            );
            assert!(!repo.join(".kranz/serve.token").exists());
            assert!(!repo.join(".kranz/serve.read.token").exists());
            assert!(!operator.join("serve").exists());
        }
    }
}

#[tokio::test]
async fn serve_published_read_credential_matches_live_server() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    assert!(std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(&repo)
        .status()
        .unwrap()
        .success());
    let mut command = serve(&repo, &temp.path().join("operator"));
    // Explicit flags win over an invalid environment value.
    command
        .args([
            "--port",
            "0",
            "--token",
            "dummy-mutation",
            "--read-token",
            "dummy-read",
        ])
        .env("KRANZ_READ_TOKEN", "")
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let url = tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            if let Some(url) = line.strip_prefix("kranz server on ") {
                return url.to_owned();
            }
        }
        panic!("server exited without announcing its address");
    })
    .await
    .unwrap();
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let response = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match client
                .get(format!("{url}api/read-token"))
                .header("x-kranz-token", "dummy-read")
                .send()
                .await
            {
                Ok(response) => break response,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = response.json().await.unwrap();
    let published = std::fs::read_to_string(repo.join(".kranz/serve.read.token")).unwrap();
    assert_eq!(published, "dummy-read");
    assert_eq!(body["token"], published);
    let response = client
        .post(format!("{url}api/queue/drain"))
        .header("x-kranz-token", &published)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
    child.kill().await.unwrap();
}
