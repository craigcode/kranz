//! Cost per merged change, grouped by repo across the M8 host catalog
//! (ticket `cost-per-merged-change`, KRZ-329) — the same outcomes fold as
//! `kranz outcomes`, with the grouping switched to the catalog. Each repo's
//! numbers come from [`kranz_engine::outcomes::compute_cost_per_merged_change`]:
//! the cost fold over missions closed in the window for the numerator, and
//! merged changes (the `merged.rs` landed/ancestry probe, derived at fold
//! time, never stored) for the denominator — beside the autonomy ratio.
//!
//! Unavailable repos degrade with a reason, never silently (the
//! [`crate::ready::assess_all`] idiom); repos with no merged changes in the
//! window read absent (`—`), never zero.

use kranz_engine::outcomes::CostPerMergedChange;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// One catalog repo's row in the report.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoMergedCosts {
    pub id: String,
    pub name: String,
    pub root: PathBuf,
    /// None when the repo is unavailable (the reason is in `unavailable`).
    pub report: Option<CostPerMergedChange>,
    pub unavailable: Option<String>,
}

/// The catalog-wide report: one row per registered repo.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgMergedCosts {
    pub window_days: u64,
    pub repos: Vec<RepoMergedCosts>,
    /// Set when the catalog itself is missing or malformed (the repos vec is
    /// empty then — the report must explain rather than show an empty table).
    pub note: Option<String>,
}

/// Fold every repo in the host catalog at `config_path`
/// (`~/.kranz/config.json` for the CLI; injected for tests). `now` is an
/// input so the window boundary is deterministic under test.
pub fn assess_all(
    config_path: &Path,
    window_days: u64,
    now: chrono::DateTime<chrono::Utc>,
) -> OrgMergedCosts {
    let host = match kranz_server::load_host_config(config_path) {
        Ok(host) => host,
        Err(error) => {
            return OrgMergedCosts {
                window_days,
                repos: Vec::new(),
                note: Some(format!(
                    "cannot read host catalog {}: {error:#}",
                    config_path.display()
                )),
            };
        }
    };
    if host.repos.is_empty() {
        return OrgMergedCosts {
            window_days,
            repos: Vec::new(),
            note: Some(format!(
                "no host catalog at {} — register repos with `kranz init --register`",
                config_path.display()
            )),
        };
    }

    let mut repos = Vec::new();
    for repo in &host.repos {
        let name = repo.display_name.clone().unwrap_or_else(|| repo.id.clone());
        let unavailable = if !repo.root.is_dir() {
            Some("root missing".to_string())
        } else if !repo.root.join(".git").exists() {
            Some("not a git repository".to_string())
        } else {
            None
        };
        let report = unavailable
            .is_none()
            .then(|| {
                kranz_engine::outcomes::compute_cost_per_merged_change(&repo.root, window_days, now)
                    .ok()
            })
            .flatten();
        repos.push(RepoMergedCosts {
            id: repo.id.clone(),
            name,
            root: repo.root.clone(),
            report,
            unavailable,
        });
    }
    OrgMergedCosts {
        window_days,
        repos,
        note: None,
    }
}

/// Optional ratio as dollars ("$12.50", or "—" when the repo merged nothing
/// in the window — absent, never $0.00).
fn fmt_usd_ratio(ratio: Option<f64>) -> String {
    ratio
        .map(|r| format!("${r:.2}"))
        .unwrap_or_else(|| "—".to_string())
}

/// Optional share as a percentage ("33%", or "—" when nothing closed).
fn fmt_share_pct(share: Option<f64>) -> String {
    share
        .map(|s| format!("{:.0}%", s * 100.0))
        .unwrap_or_else(|| "—".to_string())
}

pub fn render_org(report: &OrgMergedCosts) -> String {
    let mut out = String::new();
    if let Some(note) = &report.note {
        out.push_str(&format!("kranz outcomes --all: {note}\n"));
        return out;
    }
    out.push_str(&format!(
        "kranz outcomes --all: cost per merged change by repo ({}d window)\n",
        report.window_days
    ));
    for repo in &report.repos {
        if let Some(reason) = &repo.unavailable {
            out.push_str(&format!(
                "  {:<20} —   unavailable: {reason} ({})\n",
                repo.name,
                repo.root.display()
            ));
            continue;
        }
        let report = repo.report.as_ref().expect("available repos have a report");
        out.push_str(&format!(
            "  {:<20} {:>8} per merged change  ({} merged, ${:.2} spend, {} closed; autonomy {})\n",
            repo.name,
            fmt_usd_ratio(report.usd_per_merged_change),
            report.merged_changes,
            report.total_cost_usd,
            report.closed_in_window,
            fmt_share_pct(report.zero_intervention_share),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;
    use kranz_engine::events::{Event, EventKind};
    use kranz_engine::types::MissionConfig;
    use tempfile::TempDir;

    const NOW_MS: i64 = 1_754_000_000_000;
    const DAY_MS: i64 = 86_400_000;

    fn now() -> chrono::DateTime<chrono::Utc> {
        DateTime::from_timestamp_millis(NOW_MS).unwrap()
    }

    /// A repo-shaped dir (`.git` present so the availability check passes —
    /// not a real repository, so nothing probes as merged: the ratio reads
    /// absent, never zero) with one mission closed inside the window.
    fn seed_repo_with_closed_mission(root: &Path) {
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let dir = root.join(".kranz").join("missions").join("m-1");
        std::fs::create_dir_all(&dir).unwrap();
        let terminal = NOW_MS - DAY_MS;
        let events = [
            Event {
                seq: 1,
                ts: DateTime::from_timestamp_millis(terminal - 1_000).unwrap(),
                mission_id: "m-1".into(),
                kind: EventKind::MissionCreated {
                    goal: "g".into(),
                    base_branch: "main".into(),
                    mission_branch: "kranz/m-1".into(),
                    config: MissionConfig::default(),
                },
            },
            Event {
                seq: 2,
                ts: DateTime::from_timestamp_millis(terminal).unwrap(),
                mission_id: "m-1".into(),
                kind: EventKind::MissionCompleted {},
            },
        ];
        let lines: Vec<String> = events
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        std::fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn outcomes_report_org_catalog_aggregates_rows_and_degrades_unavailable() {
        let tmp = TempDir::new().unwrap();
        let repo_a = tmp.path().join("repo-a");
        std::fs::create_dir_all(&repo_a).unwrap();
        seed_repo_with_closed_mission(&repo_a);
        let missing = tmp.path().join("does-not-exist");

        let config_path = tmp.path().join("config.json");
        std::fs::write(
            &config_path,
            serde_json::json!({
                "host": {
                    "repos": [
                        { "id": "a", "root": repo_a },
                        { "id": "b", "root": missing }
                    ]
                }
            })
            .to_string(),
        )
        .unwrap();

        let report = assess_all(&config_path, 30, now());
        assert!(report.note.is_none());
        assert_eq!(report.repos.len(), 2);

        let a = &report.repos[0];
        assert_eq!(a.id, "a");
        assert!(a.unavailable.is_none());
        let row = a.report.as_ref().expect("available repo has a report");
        assert_eq!(row.closed_in_window, 1);
        assert_eq!(row.merged_changes, 0);
        assert_eq!(row.usd_per_merged_change, None, "absent, never zero");

        let b = &report.repos[1];
        assert_eq!(b.unavailable.as_deref(), Some("root missing"));
        assert!(b.report.is_none());

        let text = render_org(&report);
        assert!(
            text.contains("cost per merged change by repo (30d window)"),
            "{text}"
        );
        assert!(text.contains("per merged change"), "{text}");
        assert!(text.contains("unavailable: root missing"), "{text}");
        // The absent ratio renders as an em dash, never $0.00.
        assert!(!text.contains("$0.00 per merged change"), "{text}");
    }

    #[test]
    fn outcomes_report_org_catalog_missing_file_explains_itself() {
        let tmp = TempDir::new().unwrap();
        let report = assess_all(&tmp.path().join("nope.json"), 30, now());
        assert_eq!(report.repos.len(), 0);
        let text = render_org(&report);
        assert!(text.contains("no host catalog"), "{text}");
    }
}
