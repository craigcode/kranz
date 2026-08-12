//! Tests for `kranz_engine::outcomes::compute_cost_per_merged_change` —
//! KRZ-329 (ticket `cost-per-merged-change`). The numerator is the cost fold
//! over missions closed in the window; the denominator is merged changes
//! derived at fold time via the shared landed/ancestry probe (merged.rs),
//! never stored. Fixture style mirrors `merged_test.rs`: throwaway temp
//! repos with git's global/system config masked, skipping cleanly when git
//! is missing.

use chrono::{DateTime, Utc};
use kranz_engine::events::{Event, EventKind};
use kranz_engine::git_ops::GitRepo;
use kranz_engine::outcomes::compute_cost_per_merged_change;
use kranz_engine::types::{MissionConfig, Role, RunResult, TokenUsage};
use std::path::Path;
use std::process::Command;
use std::sync::Once;
use tempfile::TempDir;

static ENV_ISOLATION: Once = Once::new();

fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing = std::env::temp_dir().join(format!(
            "kranz-outcomes-merged-test-no-config-{}",
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

fn init_repo_with_identity() -> (TempDir, GitRepo) {
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
    repo.ensure_identity().expect("ensure_identity");
    (dir, repo)
}

/// Repo with identity and one seed commit on `main`; returns the seed sha.
fn seeded_repo() -> (TempDir, GitRepo, String) {
    let (dir, repo) = init_repo_with_identity();
    std::fs::write(dir.path().join("README.md"), "hello\n").expect("write seed file");
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

/// The fixed "now" every window assertion below keys on (ms since epoch) —
/// the window is an input, so the fold stays deterministic under test.
const NOW_MS: i64 = 1_754_000_000_000;
const DAY_MS: i64 = 86_400_000;

fn now() -> DateTime<Utc> {
    DateTime::from_timestamp_millis(NOW_MS).unwrap()
}

/// Write a hand-built events.jsonl with EXPLICIT timestamps (the window
/// boundary can't be tested with Utc::now() stamping).
fn write_timed_events(repo_root: &Path, mission_id: &str, events: Vec<Event>) {
    let dir = repo_root.join(".kranz").join("missions").join(mission_id);
    std::fs::create_dir_all(&dir).unwrap();
    let lines: Vec<String> = events
        .iter()
        .map(|e| serde_json::to_string(e).unwrap())
        .collect();
    std::fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
}

fn ev(seq: u64, mission_id: &str, ts_ms: i64, kind: EventKind) -> Event {
    Event {
        seq,
        ts: DateTime::from_timestamp_millis(ts_ms).unwrap(),
        mission_id: mission_id.to_string(),
        kind,
    }
}

/// A mission that spent `cost_usd` on one worker run and closed COMPLETED at
/// `terminal_ms`, on `mission_branch` off base `main`.
fn completed_mission_events(
    mission_id: &str,
    mission_branch: &str,
    cost_usd: f64,
    terminal_ms: i64,
) -> Vec<Event> {
    vec![
        ev(
            1,
            mission_id,
            terminal_ms - 3_000,
            EventKind::MissionCreated {
                goal: "fixture mission".into(),
                base_branch: "main".into(),
                mission_branch: mission_branch.into(),
                config: MissionConfig::default(),
            },
        ),
        ev(
            2,
            mission_id,
            terminal_ms - 2_000,
            EventKind::WorkerSpawned {
                run_id: "r-1".into(),
                role: Role::Worker,
                feature_id: None,
                milestone_id: None,
                candidate: None,
                executor_route: None,
                sdk_session_id: "s".into(),
                model: "sonnet".into(),
                quant: "n/a".into(),
                weight_hash: None,
                prompt_hash: "h".into(),
                transcript_path: "t".into(),
            },
        ),
        ev(
            3,
            mission_id,
            terminal_ms - 1_000,
            EventKind::WorkerCompleted {
                run_id: "r-1".into(),
                result: RunResult::Pass,
                tokens: TokenUsage {
                    input: 1,
                    output: 1,
                    cache_read: 0,
                    cache_write: 0,
                },
                cost_usd: Some(cost_usd),
                report: None,
            },
        ),
        ev(4, mission_id, terminal_ms, EventKind::MissionCompleted {}),
    ]
}

#[test]
fn outcomes_report_cost_per_merged_change_counts_only_landed_completions() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    let terminal = NOW_MS - DAY_MS; // closed one day ago — inside the 30d window

    // All git state first, event logs second: `add_all_and_commit` is
    // `git add -A`, which would otherwise sweep the first mission's
    // events.jsonl into the second branch and delete it on checkout (the
    // engine never sees this — .kranz runtime is gitignored for real).
    seed_mission_branch(
        &dir,
        &repo,
        &seed,
        "kranz/m-landed",
        "src/a.rs",
        "fn a() {}\n",
    );
    seed_mission_branch(
        &dir,
        &repo,
        &seed,
        "kranz/m-unmerged",
        "src/b.rs",
        "fn b() {}\n",
    );

    // m-landed: branch merged into main (the ancestry probe reads true).
    repo.merge_no_ff("kranz/m-landed")
        .expect("merge landed branch");
    write_timed_events(
        dir.path(),
        "m-landed",
        completed_mission_events("m-landed", "kranz/m-landed", 10.0, terminal),
    );

    // m-unmerged: branch exists but never landed.
    write_timed_events(
        dir.path(),
        "m-unmerged",
        completed_mission_events("m-unmerged", "kranz/m-unmerged", 30.0, terminal),
    );

    let report = compute_cost_per_merged_change(dir.path(), 30, now()).unwrap();
    assert_eq!(report.window_days, 30);
    assert_eq!(report.closed_in_window, 2);
    assert_eq!(report.total_cost_usd, 40.0);
    assert_eq!(report.merged_changes, 1, "only the landed branch counts");
    assert_eq!(report.usd_per_merged_change, Some(40.0));
    assert_eq!(report.zero_intervention_share, Some(1.0));
}

#[test]
fn outcomes_report_cost_per_merged_change_window_boundary_is_inclusive() {
    if !setup() {
        return;
    }
    // No git fixture needed: the window selects on terminal-event timestamps
    // before any ancestry probe runs (and this tempdir is no repository, so
    // nothing can probe as merged — the ratio reads absent, never zero).
    let dir = tempfile::tempdir().expect("create tempdir");
    let cutoff = NOW_MS - 30 * DAY_MS;

    // Terminal exactly AT the cutoff → inside (the window is inclusive at
    // both ends, documented on DEFAULT_MERGED_CHANGE_WINDOW_DAYS).
    write_timed_events(
        dir.path(),
        "m-at",
        completed_mission_events("m-at", "kranz/m-at", 7.0, cutoff),
    );
    // One ms BEFORE the cutoff → outside.
    write_timed_events(
        dir.path(),
        "m-before",
        completed_mission_events("m-before", "kranz/m-before", 11.0, cutoff - 1),
    );
    // One ms AFTER now (clock skew) → outside.
    write_timed_events(
        dir.path(),
        "m-after",
        completed_mission_events("m-after", "kranz/m-after", 13.0, NOW_MS + 1),
    );
    // Still open (no terminal event) → in no closed window at all.
    write_timed_events(
        dir.path(),
        "m-open",
        vec![ev(
            1,
            "m-open",
            NOW_MS - DAY_MS,
            EventKind::MissionCreated {
                goal: "fixture mission".into(),
                base_branch: "main".into(),
                mission_branch: "kranz/m-open".into(),
                config: MissionConfig::default(),
            },
        )],
    );

    let report = compute_cost_per_merged_change(dir.path(), 30, now()).unwrap();
    assert_eq!(report.closed_in_window, 1, "only the at-cutoff mission");
    assert_eq!(report.total_cost_usd, 7.0);
    assert_eq!(report.merged_changes, 0);
    assert_eq!(report.usd_per_merged_change, None);
    assert_eq!(report.zero_intervention_share, Some(1.0));
}

#[test]
fn outcomes_report_cost_per_merged_change_absent_when_nothing_merged() {
    if !setup() {
        return;
    }
    let (dir, repo, seed) = seeded_repo();
    let terminal = NOW_MS - DAY_MS;

    // A completed mission whose branch never landed: the merged-change
    // denominator is 0 and the ratio must be ABSENT (None), never $0.00.
    seed_mission_branch(&dir, &repo, &seed, "kranz/m-wip", "src/c.rs", "fn c() {}\n");
    write_timed_events(
        dir.path(),
        "m-wip",
        completed_mission_events("m-wip", "kranz/m-wip", 12.0, terminal),
    );

    let report = compute_cost_per_merged_change(dir.path(), 30, now()).unwrap();
    assert_eq!(report.closed_in_window, 1);
    assert_eq!(report.total_cost_usd, 12.0);
    assert_eq!(report.merged_changes, 0);
    assert_eq!(report.usd_per_merged_change, None);
    assert_eq!(report.zero_intervention_share, Some(1.0));
}
