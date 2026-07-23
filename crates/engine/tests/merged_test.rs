//! Tests for `kranz_engine::merged::ticket_merged` — the shared
//! Delivered/Landed derivation reused by mission rows and the ticket
//! projection. Fixture style mirrors `git_ops_test.rs`/`merge_test.rs`:
//! throwaway temp repos with git's global/system config masked, skipping
//! cleanly when git is missing.

use chrono::Utc;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::git_ops::GitRepo;
use kranz_engine::merged::ticket_merged;
use kranz_engine::ticket::{Ticket, TicketState};
use kranz_engine::types::MissionConfig;
use kranz_engine::work::reconcile_ticket_for_mission;
use std::path::Path;
use std::process::Command;
use std::sync::Once;
use tempfile::TempDir;

static ENV_ISOLATION: Once = Once::new();

fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing = std::env::temp_dir().join(format!(
            "kranz-merged-test-no-config-{}",
            std::process::id()
        ));
        std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
        std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
        if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
            std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
        }
    });
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn setup() -> bool {
    isolate_git_env();
    if git_available() {
        true
    } else {
        eprintln!("skipping test: git is not on PATH");
        false
    }
}

fn write(dir: &TempDir, name: &str, content: &str) {
    std::fs::write(dir.path().join(name), content).expect("write file");
}

fn init_repo() -> (TempDir, GitRepo) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let init = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .expect("spawn git init");
    if !init.status.success() {
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git init (fallback)");
        Command::new("git")
            .args(["symbolic-ref", "HEAD", "refs/heads/main"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git symbolic-ref");
    }
    let repo = GitRepo::open(dir.path()).expect("open freshly-initialized repo");
    (dir, repo)
}

fn init_repo_with_identity() -> (TempDir, GitRepo) {
    let (dir, repo) = init_repo();
    repo.ensure_identity().expect("ensure_identity");
    (dir, repo)
}

/// Repo with identity and one seed commit on `main`; returns the seed sha.
fn seeded_repo() -> (TempDir, GitRepo, String) {
    let (dir, repo) = init_repo_with_identity();
    write(&dir, "README.md", "hello\n");
    let sha = repo
        .add_all_and_commit("initial commit")
        .expect("seed commit");
    (dir, repo, sha)
}

/// Creates `mission_branch` off `seed` with one commit touching `path`,
/// leaving the repo checked out back on `main`.
fn seed_mission_branch(
    dir: &TempDir,
    repo: &GitRepo,
    seed: &str,
    mission_branch: &str,
    path: &str,
    content: &str,
) {
    repo.create_branch(mission_branch, Some(seed)).unwrap();
    repo.checkout(mission_branch).unwrap();
    let full = dir.path().join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, content).unwrap();
    repo.add_all_and_commit("mission work").unwrap();
    repo.checkout("main").unwrap();
}

/// Write a hand-built events.jsonl for `mission_id` under `repo_root` (seq
/// assigned 1..; same shape the engine's single writer produces) — mirrors
/// `config_cost_prompts_test.rs::write_events`.
fn write_events(repo_root: &Path, mission_id: &str, kinds: Vec<EventKind>) {
    let dir = repo_root.join(".kranz").join("missions").join(mission_id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut lines = String::new();
    for (i, kind) in kinds.into_iter().enumerate() {
        let event = Event {
            seq: (i + 1) as u64,
            ts: Utc::now(),
            mission_id: mission_id.to_string(),
            kind,
        };
        lines.push_str(&serde_json::to_string(&event).unwrap());
        lines.push('\n');
    }
    std::fs::write(dir.join("events.jsonl"), lines).unwrap();
}

fn created(mission_branch: &str) -> EventKind {
    EventKind::MissionCreated {
        goal: "fixture mission".to_string(),
        base_branch: "main".to_string(),
        mission_branch: mission_branch.to_string(),
        config: MissionConfig::default(),
    }
}

fn scaffold_done_ticket(repo_root: &Path, slug: &str) {
    Ticket::scaffold(repo_root, slug, "fixture ticket", None, None).unwrap();
    Ticket::write_state(repo_root, slug, TicketState::Done, None).unwrap();
}

#[test]
fn ticket_projection_merged_some_false_when_mission_branch_unmerged() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    let mission_branch = "kranz/mission-m1";
    seed_mission_branch(
        &dir,
        &repo,
        &seed,
        mission_branch,
        "src/a.rs",
        "fn a() {}\n",
    );

    scaffold_done_ticket(dir.path(), "my-ticket");
    Ticket::record_mission(dir.path(), "my-ticket", "m1").unwrap();
    write_events(
        dir.path(),
        "m1",
        vec![created(mission_branch), EventKind::MissionCompleted {}],
    );

    assert_eq!(
        ticket_merged(dir.path(), "my-ticket"),
        Some(false),
        "Complete mission whose branch is not an ancestor of base => Delivered"
    );
}

#[test]
fn ticket_projection_merged_some_true_when_mission_branch_merged() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    let mission_branch = "kranz/mission-m2";
    seed_mission_branch(
        &dir,
        &repo,
        &seed,
        mission_branch,
        "src/b.rs",
        "fn b() {}\n",
    );
    repo.merge_no_ff(mission_branch).unwrap();

    scaffold_done_ticket(dir.path(), "my-ticket");
    Ticket::record_mission(dir.path(), "my-ticket", "m2").unwrap();
    write_events(
        dir.path(),
        "m2",
        vec![created(mission_branch), EventKind::MissionCompleted {}],
    );

    assert_eq!(
        ticket_merged(dir.path(), "my-ticket"),
        Some(true),
        "Complete mission whose branch is merged into base => Landed"
    );
}

#[test]
fn ticket_projection_merged_none_when_no_linked_mission() {
    if !setup() {
        return;
    }
    let (dir, _repo, _seed) = seeded_repo();
    scaffold_done_ticket(dir.path(), "my-ticket");

    assert_eq!(
        ticket_merged(dir.path(), "my-ticket"),
        None,
        "Done ticket with no linked mission => split not applicable"
    );
}

#[test]
fn ticket_projection_merged_none_when_mission_abandoned() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    let mission_branch = "kranz/mission-m3";
    seed_mission_branch(
        &dir,
        &repo,
        &seed,
        mission_branch,
        "src/c.rs",
        "fn c() {}\n",
    );

    scaffold_done_ticket(dir.path(), "my-ticket");
    Ticket::record_mission(dir.path(), "my-ticket", "m3").unwrap();
    write_events(
        dir.path(),
        "m3",
        vec![
            created(mission_branch),
            EventKind::MissionAbandoned {
                reason: "no longer needed".to_string(),
            },
        ],
    );

    assert_eq!(
        ticket_merged(dir.path(), "my-ticket"),
        None,
        "Abandoned mission is not the Complete/unmerged split => None"
    );
}

#[test]
fn ticket_projection_merged_none_when_ticket_not_done() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    let mission_branch = "kranz/mission-m4";
    seed_mission_branch(
        &dir,
        &repo,
        &seed,
        mission_branch,
        "src/d.rs",
        "fn d() {}\n",
    );

    Ticket::scaffold(dir.path(), "my-ticket", "fixture ticket", None, None).unwrap();
    Ticket::write_state(dir.path(), "my-ticket", TicketState::Queued, None).unwrap();
    Ticket::record_mission(dir.path(), "my-ticket", "m4").unwrap();
    write_events(
        dir.path(),
        "m4",
        vec![created(mission_branch), EventKind::MissionCompleted {}],
    );

    assert_eq!(
        ticket_merged(dir.path(), "my-ticket"),
        None,
        "non-Done ticket => split not applicable"
    );
}

#[test]
fn reconcile_preserves_delivered_landed_split() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    let mission_branch = "kranz/mission-m5";
    seed_mission_branch(
        &dir,
        &repo,
        &seed,
        mission_branch,
        "src/e.rs",
        "fn e() {}\n",
    );

    Ticket::scaffold(dir.path(), "my-ticket", "fixture ticket", None, None).unwrap();
    Ticket::write_state(dir.path(), "my-ticket", TicketState::Running, None).unwrap();
    Ticket::record_mission(dir.path(), "my-ticket", "m5").unwrap();
    write_events(
        dir.path(),
        "m5",
        vec![created(mission_branch), EventKind::MissionCompleted {}],
    );

    let result = reconcile_ticket_for_mission(dir.path(), "m5").unwrap();
    assert_eq!(result, Some(("my-ticket".to_string(), TicketState::Done)));

    assert_eq!(
        ticket_merged(dir.path(), "my-ticket"),
        Some(false),
        "reconcile writing Done must not disturb the Delivered/Landed split \
         (branch not merged => Delivered)"
    );
}
