//! Integration tests for `kranz exec` — fully headless missions for CI.
//!
//! No installed `claude` binary is ever spawned: the SIGINT test uses an
//! explicit shell fixture, and other coverage exercises pure helpers.
//! Parsing is checked with clap `try_parse_from`; the plan-file parse path feeds
//! ticket-shaped markdown to `parse_mission_markdown` and asserts the folded
//! mission goal carries the goal / acceptance criteria; the outcome→exit-code
//! mapping is checked directly on `exit_code_for`.

use clap::Parser;
use kranz_cli::cli::{Cli, Command};
use kranz_cli::exec::{exit_code_for, parse_mission_markdown, scrutiny_gate, EXIT_UNDERSPECIFIED};
use kranz_engine::types::MissionStatus;

// ---------------------------------------------------------------------------
// clap parsing — the exec subcommand + its flags
// ---------------------------------------------------------------------------

#[test]
fn parses_exec_with_short_file_flag() {
    let cli = Cli::try_parse_from(["kranz", "exec", "-f", "mission.md"]).unwrap();
    match cli.command {
        Command::Exec {
            file,
            yes,
            max_cycles,
            ..
        } => {
            assert_eq!(file.to_str(), Some("mission.md"));
            assert!(!yes);
            assert_eq!(max_cycles, None);
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_exec_with_long_file_flag_and_all_options() {
    let cli = Cli::try_parse_from([
        "kranz",
        "exec",
        "--file",
        "plans/ci.md",
        "--yes",
        "--max-cycles",
        "5",
    ])
    .unwrap();
    match cli.command {
        Command::Exec {
            file,
            yes,
            max_cycles,
            ..
        } => {
            assert_eq!(file.to_str(), Some("plans/ci.md"));
            assert!(yes);
            assert_eq!(max_cycles, Some(5));
        }
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn exec_requires_a_file() {
    // -f/--file is mandatory: `kranz exec` alone is a parse error.
    assert!(Cli::try_parse_from(["kranz", "exec"]).is_err());
}

#[test]
fn exec_honors_global_repo_and_danger_flags() {
    let cli = Cli::try_parse_from([
        "kranz",
        "--repo",
        "/tmp/target",
        "--dangerously-allow-all",
        "exec",
        "-f",
        "mission.md",
    ])
    .unwrap();
    assert_eq!(
        cli.repo.as_deref(),
        Some(std::path::Path::new("/tmp/target"))
    );
    assert!(cli.dangerously_allow_all);
    assert!(matches!(cli.command, Command::Exec { .. }));
}

#[test]
fn exec_rejects_non_numeric_max_cycles() {
    assert!(
        Cli::try_parse_from(["kranz", "exec", "-f", "mission.md", "--max-cycles", "lots"]).is_err()
    );
}

#[test]
fn exec_enqueue_is_explicit_and_conflicts_with_push() {
    let cli = Cli::try_parse_from(["kranz", "exec", "-f", "mission.md", "--enqueue"]).unwrap();
    assert!(matches!(cli.command, Command::Exec { enqueue: true, .. }));

    assert!(Cli::try_parse_from([
        "kranz",
        "exec",
        "-f",
        "mission.md",
        "--enqueue",
        "--push",
        "origin"
    ])
    .is_err());

    let sourced = Cli::try_parse_from([
        "kranz",
        "exec",
        "-f",
        "mission.md",
        "--enqueue",
        "--enqueue-source",
        "gascity",
        "--enqueue-external-ref",
        "rig-1",
    ])
    .unwrap();
    assert!(matches!(
        sourced.command,
        Command::Exec {
            enqueue_source: Some(ref source),
            enqueue_external_ref: Some(ref external_ref),
            ..
        } if source == "gascity" && external_ref == "rig-1"
    ));
    assert!(Cli::try_parse_from([
        "kranz",
        "exec",
        "-f",
        "mission.md",
        "--enqueue-source",
        "gascity",
        "--enqueue-external-ref",
        "rig-1",
    ])
    .is_err());
}

#[test]
fn allow_unvalidated_defaults_to_false() {
    let cli = Cli::try_parse_from(["kranz", "exec", "-f", "mission.md"]).unwrap();
    match cli.command {
        Command::Exec {
            allow_unvalidated, ..
        } => assert!(!allow_unvalidated),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn parses_exec_with_allow_unvalidated_flag() {
    let cli =
        Cli::try_parse_from(["kranz", "exec", "-f", "mission.md", "--allow-unvalidated"]).unwrap();
    match cli.command {
        Command::Exec {
            allow_unvalidated, ..
        } => assert!(allow_unvalidated),
        other => panic!("unexpected: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// scrutiny_gate — the unattended scrutiny floor (pure fn)
// ---------------------------------------------------------------------------

#[test]
fn scrutiny_gate_refuses_skip_scrutiny_without_override() {
    let err = scrutiny_gate(true, false).unwrap_err();
    assert!(err.contains("--allow-unvalidated"), "message: {err}");
}

#[test]
fn scrutiny_gate_allows_skip_scrutiny_with_override() {
    assert!(scrutiny_gate(true, true).is_ok());
}

#[test]
fn scrutiny_gate_allows_normal_config() {
    assert!(scrutiny_gate(false, false).is_ok());
}

// ---------------------------------------------------------------------------
// plan-file parse path — ticket-shaped markdown → folded mission goal
// ---------------------------------------------------------------------------

const SAMPLE_MISSION: &str = "\
---
title: Rate-limit the public API
priority: 1
---

## Goal
Cap requests per API token so a single client cannot exhaust the pool.

## Context
The gateway currently forwards every request unthrottled.

## Scoping answers
- Use a token-bucket, 100 requests/minute per token.
- Return HTTP 429 with a Retry-After header when the bucket is empty.

## Acceptance hints
- A burst above the cap returns 429.
- Under the cap, requests pass through untouched.
";

#[test]
fn parses_ticket_shaped_markdown_and_extracts_goal() {
    let ticket = parse_mission_markdown("rate-limit", SAMPLE_MISSION).unwrap();

    assert_eq!(ticket.slug, "rate-limit");
    assert_eq!(ticket.title, "Rate-limit the public API");
    assert_eq!(ticket.priority, 1);
    assert!(ticket.goal.contains("Cap requests per API token"));
    assert_eq!(ticket.scoping_answers.len(), 2);
    assert_eq!(ticket.acceptance_hints.len(), 2);
}

#[test]
fn mission_goal_folds_the_whole_ticket_for_the_seed_turn() {
    // The folded mission_goal() is what exec seeds the orchestrator with; it
    // must carry the goal plus the scoping answers and acceptance hints so a
    // headless plan file is self-sufficient.
    let ticket = parse_mission_markdown("rate-limit", SAMPLE_MISSION).unwrap();
    let goal = ticket.mission_goal();

    assert!(goal.contains("Cap requests per API token"));
    assert!(goal.contains("token-bucket, 100 requests/minute"));
    assert!(goal.contains("returns 429"));
    assert!(goal.contains("## Scoping answers"));
    assert!(goal.contains("## Acceptance hints"));
    assert!(goal.contains("## Context"));
}

#[test]
fn goalless_preamble_still_parses_as_the_goal() {
    // A bare plan file with no `## Goal` heading: the preamble text is the goal.
    let md = "Add a health-check endpoint at /healthz that returns 200 OK.\n";
    let ticket = parse_mission_markdown("healthz", md).unwrap();
    assert!(ticket.goal.contains("health-check endpoint"));
    assert!(ticket.mission_goal().contains("/healthz"));
}

#[test]
fn unclosed_frontmatter_is_a_parse_error() {
    // A malformed plan file surfaces as an error (exec fails before any spend),
    // not a silently-empty mission.
    let md = "---\ntitle: broken\n\n## Goal\nnever closed the frontmatter\n";
    assert!(parse_mission_markdown("broken", md).is_err());
}

// ---------------------------------------------------------------------------
// outcome → exit-code mapping (pure fn)
// ---------------------------------------------------------------------------

#[test]
fn exit_code_mapping_matches_the_ci_contract() {
    assert_eq!(exit_code_for(MissionStatus::Complete), 0);
    assert_eq!(exit_code_for(MissionStatus::Failed), 1);
    assert_eq!(exit_code_for(MissionStatus::Blocked), 2);
    // The underspecified (NotReady) case is a distinct code handled before the
    // run and never routed through exit_code_for.
    assert_eq!(EXIT_UNDERSPECIFIED, 3);
}

#[test]
fn non_terminal_statuses_map_to_failure() {
    // run() only returns Complete/Failed/Blocked; anything else is defensive.
    for status in [
        MissionStatus::Planning,
        MissionStatus::Running,
        MissionStatus::Paused,
        MissionStatus::Validating,
        MissionStatus::Abandoned,
    ] {
        assert_eq!(exit_code_for(status), 1, "status {status:?}");
    }
}

/// SIGINT must unwind the running mission instead of exiting while its
/// separately grouped backend and tool subprocess continue changing files.
#[cfg(unix)]
#[test]
fn exec_sigint_stops_the_backend_tree_and_retains_resumable_state() {
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    use std::process::{Command as Process, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    let authority = dir.path().join("authority");
    std::fs::create_dir(&repo).unwrap();
    std::fs::create_dir(&authority).unwrap();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.name", "kranz-test"],
        vec!["config", "user.email", "test@kranz.local"],
    ] {
        assert!(Process::new("git")
            .args(args)
            .current_dir(&repo)
            .status()
            .unwrap()
            .success());
    }
    std::fs::write(repo.join("README.md"), "# Interrupt fixture\n").unwrap();
    assert!(Process::new("git")
        .args(["add", "README.md"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    assert!(Process::new("git")
        .args(["commit", "-qm", "seed"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    let ticket = dir.path().join("mission.md");
    std::fs::write(
        &ticket,
        "---\ntitle: Interrupt fixture\n---\n\n## Goal\nImplement a fixture.\n",
    )
    .unwrap();
    let pids = dir.path().join("pids 'quoted'.json");
    let fake = dir.path().join("claude");
    let staged_fake = dir.path().join(".claude.tmp");
    let quoted_pids = format!("'{}'", pids.to_str().unwrap().replace('\'', "'\\''"));
    std::fs::write(
        &staged_fake,
        format!(
            "#!/bin/sh\n\
             set -eu\n\
             if [ \"${{1-}}\" = --version ]; then\n\
               printf '%s\\n' '2.1.0 (Claude Code)'\n\
               exit 0\n\
             fi\n\
             /bin/sleep 300 &\n\
             tool=$!\n\
             printf '[%s,%s]\\n' \"$$\" \"$tool\" > {quoted_pids}\n\
             wait \"$tool\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&staged_fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(staged_fake, &fake).unwrap();
    std::fs::write(
        authority.join("config.json"),
        serde_json::json!({"claudeBinary":fake}).to_string(),
    )
    .unwrap();
    let mut child = Process::new(env!("CARGO_BIN_EXE_kranz"))
        .args([
            "--repo",
            repo.to_str().unwrap(),
            "exec",
            "-f",
            ticket.to_str().unwrap(),
        ])
        .env("KRANZ_HOME", &authority)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Drain continuously: a full stderr pipe must not prevent startup, and
    // failure diagnostics must not wait for a still-running child to exit.
    let stderr_tail = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&stderr_tail);
    let mut stderr = child.stderr.take().unwrap();
    std::thread::spawn(move || {
        const LIMIT: usize = 16 * 1024;
        let mut chunk = [0; 4096];
        while let Ok(read) = stderr.read(&mut chunk) {
            if read == 0 {
                break;
            }
            let mut tail = captured.lock().unwrap();
            tail.extend_from_slice(&chunk[..read]);
            let excess = tail.len().saturating_sub(LIMIT);
            tail.drain(..excess);
        }
    });
    struct Cleanup {
        child: std::process::Child,
        pids: std::path::PathBuf,
        stderr_tail: Arc<Mutex<Vec<u8>>>,
    }
    impl Cleanup {
        fn stderr(&self) -> String {
            String::from_utf8_lossy(&self.stderr_tail.lock().unwrap()).into_owned()
        }
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Ok(bytes) = std::fs::read(&self.pids) {
                if let Ok(ids) = serde_json::from_slice::<Vec<u32>>(&bytes) {
                    let _ = Process::new("/bin/sh")
                        .args([
                            "-c",
                            "kill -KILL \"$1\"",
                            "kranz-test",
                            &format!("-{}", ids[0]),
                        ])
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status();
                }
            }
        }
    }
    let mut running = Cleanup {
        child,
        pids: pids.clone(),
        stderr_tail,
    };
    let deadline = Instant::now() + Duration::from_secs(20);
    let ids: Vec<u32> = loop {
        if let Ok(bytes) = std::fs::read(&pids) {
            if let Ok(ids) = serde_json::from_slice(&bytes) {
                break ids;
            }
        }
        assert!(
            running.child.try_wait().unwrap().is_none(),
            "CLI exited before starting the fixture; stderr:\n{}",
            running.stderr()
        );
        assert!(
            Instant::now() < deadline,
            "fixture backend did not start; stderr:\n{}",
            running.stderr()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(ids.len(), 2, "fixture must record backend and tool PIDs");
    assert_ne!(ids[0], ids[1]);
    assert_ne!(ids[0], running.child.id());
    // The backend must own a group distinct from the CLI, otherwise killing
    // only the CLI could accidentally satisfy the descendant-cleanup check.
    // The -SIGNAL form handles negative group IDs in both dash and macOS sh.
    assert!(
        Process::new("/bin/sh")
            .args([
                "-c",
                "kill -0 \"$1\"",
                "kranz-test",
                &format!("-{}", ids[0]),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "fixture backend does not own its process group"
    );
    // POSIX shells provide kill even on minimal hosts without /bin/kill.
    assert!(Process::new("/bin/sh")
        .args([
            "-c",
            "kill -s INT \"$1\"",
            "kranz-test",
            &running.child.id().to_string(),
        ])
        .status()
        .unwrap()
        .success());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = running.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "CLI did not exit after SIGINT; stderr:\n{}",
            running.stderr()
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.code(), Some(130), "stderr:\n{}", running.stderr());
    for pid in ids {
        while Process::new("/bin/sh")
            .args(["-c", "kill -0 \"$1\"", "kranz-test", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
        {
            assert!(
                Instant::now() < deadline,
                "backend/tool process {pid} survived SIGINT"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    // The groups are gone; do not signal their numeric ids again on drop.
    std::fs::remove_file(&pids).unwrap();
    let missions = std::fs::read_dir(repo.join(".kranz/missions")).unwrap();
    let mission = missions
        .map(Result::unwrap)
        .find(|entry| entry.file_type().unwrap().is_dir())
        .unwrap()
        .path();
    assert!(mission.join("events.jsonl").is_file());
    assert!(
        !mission.join("events.jsonl.lock").exists(),
        "interruption retained the writer lock"
    );
}
