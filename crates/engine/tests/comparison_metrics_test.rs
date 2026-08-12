//! Tests for `kranz_engine::comparison_metrics::compute_comparison_report` —
//! KRZ-333 (ticket `outcomes-comparison-metrics`), the industry-comparison
//! set beside the kranz-native outcomes. Fixture style mirrors
//! `outcomes_report_merged_test.rs`: throwaway temp repos with git's
//! global/system config masked, skipping cleanly when git is missing.
//!
//! Unlike the merged-cost fold, the assisted-change-share denominator reads
//! COMMITTER DATES (first-parent landings inside the window), so these
//! fixtures pin the fold's `now` to the real clock and let git stamp real
//! dates — the 30-day window then contains exactly what the fixture landed
//! moments ago. The one boundary test dates a commit by hand via
//! `GIT_COMMITTER_DATE`.

use chrono::{DateTime, Utc};
use kranz_engine::comparison_metrics::compute_comparison_report;
use kranz_engine::events::{Event, EventKind};
use kranz_engine::git_ops::GitRepo;
use kranz_engine::types::MissionConfig;
use std::path::Path;
use std::process::Command;
use std::sync::Once;
use tempfile::TempDir;

static ENV_ISOLATION: Once = Once::new();

fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing = std::env::temp_dir().join(format!(
            "kranz-comparison-metrics-test-no-config-{}",
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

/// Repo with identity and one seed commit on `main`.
fn seeded_repo() -> (TempDir, GitRepo) {
    let (dir, repo) = init_repo_with_identity();
    std::fs::write(dir.path().join("README.md"), "hello\n").expect("write seed file");
    repo.add_all_and_commit("initial commit")
        .expect("seed commit");
    (dir, repo)
}

/// Creates `mission_branch` off `main`'s seed with one commit touching
/// `path`, leaving the repo checked out back on `main`.
fn seed_mission_branch(
    dir: &TempDir,
    repo: &GitRepo,
    mission_branch: &str,
    path: &str,
    content: &str,
) {
    let seed = repo.rev_parse("main").unwrap();
    repo.create_branch(mission_branch, Some(&seed)).unwrap();
    repo.checkout(mission_branch).unwrap();
    let full = dir.path().join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&full, content).unwrap();
    repo.add_all_and_commit("mission work").unwrap();
    repo.checkout("main").unwrap();
}

/// The fold's pinned "now": the real clock, so git's real committer dates
/// (stamped moments ago) land inside the 30-day window.
fn now() -> DateTime<Utc> {
    Utc::now()
}

fn ev(seq: u64, mission_id: &str, ts_ms: i64, kind: EventKind) -> Event {
    Event {
        seq,
        ts: DateTime::from_timestamp_millis(ts_ms).unwrap(),
        mission_id: mission_id.to_string(),
        kind,
    }
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

/// A mission on `mission_branch` off base `main`, closed COMPLETED at
/// `terminal_ms` (the same reducer-foldable shape the merged-cost fixtures
/// use).
fn completed_mission_events(
    mission_id: &str,
    mission_branch: &str,
    terminal_ms: i64,
) -> Vec<Event> {
    vec![
        ev(
            1,
            mission_id,
            terminal_ms - 2_000,
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
            terminal_ms - 1_000,
            EventKind::PlanApproved {
                plan: kranz_engine::types::Plan {
                    goal: "g".into(),
                    validation_contract: vec![],
                    milestones: vec![],
                    considered_alternatives: None,
                    command_grants: vec![],
                    touch_set: vec![],
                    standards_manifest: None,
                },
                base_sha: None,
            },
        ),
        ev(3, mission_id, terminal_ms, EventKind::MissionCompleted {}),
    ]
}

/// Write a defect/regular ticket under `.kranz/tickets/` — the
/// `traced-from-mission` frontmatter is the recorded defect→mission link the
/// density fold joins (data entry, never inference).
fn write_ticket(repo_root: &Path, slug: &str, traced_from_mission: Option<&str>) {
    let dir = repo_root.join(".kranz").join("tickets");
    std::fs::create_dir_all(&dir).unwrap();
    let trace = traced_from_mission
        .map(|m| format!("traced-from-mission: {m}\n"))
        .unwrap_or_default();
    std::fs::write(
        dir.join(format!("{slug}.md")),
        format!("---\ntitle: {slug}\npriority: 2\n{trace}---\n\n## Goal\n\nFixture.\n"),
    )
    .unwrap();
}

/// Repo with one landed mission (`m-landed`, merged `--no-ff`) and one
/// completed-but-unmerged mission (`m-unmerged`) — all git state first, all
/// `.kranz` content second (an `add -A` mid-fixture would sweep the logs
/// into a branch, the same hazard the merged-cost fixtures document).
fn landed_and_unmerged_repo() -> (TempDir, GitRepo) {
    let (dir, repo) = seeded_repo();
    seed_mission_branch(&dir, &repo, "kranz/m-landed", "src/a.rs", "fn a() {}\n");
    seed_mission_branch(&dir, &repo, "kranz/m-unmerged", "src/b.rs", "fn b() {}\n");
    repo.merge_no_ff("kranz/m-landed")
        .expect("merge landed branch");
    let terminal = now().timestamp_millis() - 86_400_000; // closed a day ago — inside the window
    write_timed_events(
        dir.path(),
        "m-landed",
        completed_mission_events("m-landed", "kranz/m-landed", terminal),
    );
    write_timed_events(
        dir.path(),
        "m-unmerged",
        completed_mission_events("m-unmerged", "kranz/m-unmerged", terminal),
    );
    (dir, repo)
}

#[test]
fn comparison_metrics_assisted_share_counts_mission_landings_against_all_landings() {
    if !setup() {
        return;
    }
    let (dir, repo) = landed_and_unmerged_repo();
    // A hand-written change landed directly on main (a NON-mission landing
    // the event log cannot see — only the git probe counts it).
    std::fs::write(dir.path().join("docs.md"), "hand-written\n").unwrap();
    repo.add_all_and_commit("hand-written change").unwrap();

    let report = compute_comparison_report(dir.path(), 30, now()).unwrap();
    let share = &report.assisted_change_share;
    // First-parent landings on main in the window: the seed, the mission's
    // --no-ff merge, and the hand-written commit — 3. The agent side counts
    // the ONE merged mission; the unmerged mission enters neither side.
    assert_eq!(share.agent_changes, 1, "only the landed mission");
    assert_eq!(share.base_branch.as_deref(), Some("main"));
    assert_eq!(share.total_changes, Some(3));
    assert_eq!(share.share, Some(1.0 / 3.0));
    assert_eq!(
        share.dependency, None,
        "a computed slot names no dependency"
    );
    assert!(share.definition.contains("agent-involved by construction"));
}

#[test]
fn comparison_metrics_defect_density_counts_traced_defects_per_merged_change() {
    if !setup() {
        return;
    }
    let (dir, _repo) = landed_and_unmerged_repo();
    // One defect traced to the landed mission (joins); one traced to the
    // unmerged mission (no shipped change — cannot join); one traced to an
    // unknown mission; one plain ticket with no link at all.
    write_ticket(dir.path(), "defect-login-regression", Some("m-landed"));
    write_ticket(dir.path(), "defect-on-unmerged-work", Some("m-unmerged"));
    write_ticket(dir.path(), "defect-unknown-mission", Some("m-999"));
    write_ticket(dir.path(), "plain-feature-idea", None);

    let report = compute_comparison_report(dir.path(), 30, now()).unwrap();
    let density = &report.defect_density;
    assert_eq!(density.merged_changes, 1);
    assert_eq!(
        density.traced_defects, 1,
        "only the defect on a merged-in-window mission joins"
    );
    assert_eq!(density.defects_per_merged_change, Some(1.0));
    assert_eq!(density.dependency, None);
    assert!(density
        .definition
        .contains("traced-from-mission frontmatter"));
}

#[test]
fn comparison_metrics_window_excludes_landings_dated_before_the_cutoff() {
    if !setup() {
        return;
    }
    // The seed is the OLD commit (dated 40 days ago, outside the 30d
    // window). It must be the repo's ROOT: `git rev-list --since` prunes
    // traversal at the first commit older than the cutoff, so a
    // back-dated commit with NEWER ancestors would silently empty the
    // walk — back-dating only works chronologically.
    let (dir, repo) = init_repo_with_identity();
    std::fs::write(dir.path().join("README.md"), "hello\n").unwrap();
    let old_date = (now() - chrono::Duration::days(40)).to_rfc3339();
    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(dir.path())
        .output()
        .expect("spawn git add");
    assert!(add.status.success());
    let commit = Command::new("git")
        .args(["commit", "-m", "initial commit"])
        .current_dir(dir.path())
        .env("GIT_AUTHOR_DATE", &old_date)
        .env("GIT_COMMITTER_DATE", &old_date)
        .output()
        .expect("spawn git commit");
    assert!(commit.status.success());
    seed_mission_branch(&dir, &repo, "kranz/m-landed", "src/a.rs", "fn a() {}\n");
    repo.merge_no_ff("kranz/m-landed")
        .expect("merge landed branch");
    std::fs::write(dir.path().join("docs.md"), "hand-written\n").unwrap();
    repo.add_all_and_commit("hand-written change").unwrap();
    let terminal = now().timestamp_millis() - 86_400_000;
    write_timed_events(
        dir.path(),
        "m-landed",
        completed_mission_events("m-landed", "kranz/m-landed", terminal),
    );

    let report = compute_comparison_report(dir.path(), 30, now()).unwrap();
    let share = &report.assisted_change_share;
    // The mission merge + the hand-written commit = 2 landings inside the
    // window; the back-dated seed is excluded by its committer date.
    assert_eq!(share.total_changes, Some(2));
    assert_eq!(share.agent_changes, 1);
    assert_eq!(share.share, Some(0.5));
}

#[test]
fn comparison_metrics_resolution_time_slot_names_its_missing_timestamps() {
    if !setup() {
        return;
    }
    let (dir, _repo) = landed_and_unmerged_repo();
    write_ticket(dir.path(), "defect-login-regression", Some("m-landed"));

    let report = compute_comparison_report(dir.path(), 30, now()).unwrap();
    // Even with a joined defect on record, the resolution slot stays EMPTY:
    // the ticket carries no open/close instants — the dependency is named,
    // never approximated from unrelated timestamps.
    let resolution = &report.defect_resolution_time;
    let dependency = resolution
        .dependency
        .as_deref()
        .expect("the empty slot names its dependency");
    assert!(dependency.contains("open/close timestamps"), "{dependency}");
    assert!(
        resolution.definition.contains("no lifecycle timestamps"),
        "the inline definition states why the slot is empty"
    );
}
