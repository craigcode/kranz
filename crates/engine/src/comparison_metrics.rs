//! The industry-comparison set beside the kranz-native outcomes (ticket
//! `outcomes-comparison-metrics`, KRZ-333; design:
//! `docs/scoping/governance-evidence-layer.md`, scored-gates addendum).
//!
//! WHY this section exists: the kranz-native metrics (autonomy ratio, cost
//! per change, false-green rate, the escalation ledger — defined in
//! `docs/metrics.md`) answer "should you have let it?", but an outside
//! reader's frame is the industry-legible volume proxies: how much of the
//! landed work involved an agent, how many defects each unit of change
//! produced, how fast defects closed. Their absence makes the report
//! unreadable rather than rigorous — and publishing both sets shows that
//! volume metrics can improve while false greens go unmeasured, which makes
//! the case better than either set alone. The natives stay PRIMARY: this
//! section renders after them, clearly separated, and every metric states
//! its definition INLINE in the output because these metrics are
//! self-defined across the industry — the definition is the whole argument.
//!
//! Pure-fold style, mirroring [`crate::outcomes::compute_cost_per_merged_change`]:
//! a function over (the event logs, the ticket list, live git refs, and a
//! caller-pinned window), derived per request and never stored — there is no
//! second persisted source of truth. The honesty rule is inherited from the
//! native fold and strengthened here: a slot whose data the fold cannot see
//! renders EMPTY and names its dependency, never an approximation
//! (absent > approximated, always).
//!
//! The three dispositions, and why:
//!
//! 1. **Assisted-change share** — RENDERED. The numerator is the native
//!    merged-change derivation (missions COMPLETED in the window whose
//!    branch tip is an ancestor of the live base tip); every merged mission
//!    change is agent-involved by construction, a mission being an agent
//!    run. The denominator is the total change count the fold can see:
//!    first-parent commits landed on the windowed missions' modal base
//!    branch inside the window (git's committer dates). The event log
//!    cannot see non-mission commits — only the git probe can — and a
//!    mission landed out-of-band (fast-forward, squash, or outside its
//!    completion window) reads as unattributed; the inline definition says
//!    exactly this, so the share is a stated reading, never an inflated
//!    one.
//! 2. **Defect density per merged change** — RENDERED from the landed
//!    false-green↔defect linkage (flight surgeon): defect tickets naming a
//!    mission via `traced-from-mission` frontmatter (recorded data entry,
//!    never inference) joined to missions merged in the window, over the
//!    window's merged changes. Both sides are mission-scoped by
//!    construction — the only defect links the fold can join are
//!    mission-traced ones, and unmerged missions ship no change.
//! 3. **Defect resolution time** — EMPTY, naming its dependency. The defect
//!    record is ticket frontmatter (title/priority/`state`/
//!    `traced-from-mission`): it carries lifecycle STATE but no lifecycle
//!    TIME — no open instant, no close instant, nothing to subtract. The
//!    ticket's own acceptance rule applies: the slot stays visibly empty
//!    rather than approximating from unrelated timestamps.

use crate::escalation_metrics::traced_defects_from_tickets;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The inline definition of the assisted-change share, rendered verbatim in
/// text and JSON — the definition is the whole argument for an
/// industry-legible metric, so it travels with the data.
const ASSISTED_CHANGE_SHARE_DEFINITION: &str = "Merged mission changes \
closed in the window (missions COMPLETED whose branch tip is an ancestor of \
the live base tip — agent-involved by construction, a mission being an agent \
run) as a share of all first-parent commits landed on the windowed missions' \
base branch in the window (by commit date). Non-mission commits are invisible \
to the event log, so the denominator is the total the git probe can see; \
missions landed out-of-band (fast-forward, squash, or outside their \
completion window) read as unattributed — a stated under-read, never an \
inflation.";

/// The inline definition of defect density per merged change.
const DEFECT_DENSITY_DEFINITION: &str = "Defect tickets traced to missions \
merged in the window (traced-from-mission frontmatter — recorded data entry, \
never inference) per merged change in the same window (missions COMPLETED \
whose branch tip is an ancestor of the live base tip). Both sides are \
mission-scoped: defects without a traced mission and missions that never \
merged enter neither side.";

/// The inline definition of defect resolution time — including WHY it is
/// empty today: the honest output is an empty slot, not an approximation.
const DEFECT_RESOLUTION_TIME_DEFINITION: &str = "Mean wall-clock time from \
defect open to defect close over traced defect tickets. Uncomputable today: \
the defect record (ticket frontmatter with state and the traced-from-mission \
link) carries no lifecycle timestamps, so neither an open nor a close \
instant exists — the slot stays empty rather than approximating.";

/// Why the defect-resolution-time slot is empty (the named dependency).
const DEFECT_RESOLUTION_TIME_DEPENDENCY: &str = "ticket open/close timestamps \
— defect tickets record lifecycle state but no lifecycle time";

/// The industry-comparison set (KRZ-333), folded over one window. Carried on
/// [`crate::outcomes::Outcomes::comparison`] when the fold options pin a
/// window; rendered as a clearly-separated secondary section after the
/// kranz-native metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonReport {
    /// The window in effect (days, ending at the caller-pinned `now`).
    pub window_days: u64,
    pub assisted_change_share: AssistedChangeShare,
    pub defect_density: DefectDensity,
    pub defect_resolution_time: DefectResolutionTime,
}

/// Assisted-change share: merged mission changes over all landed changes the
/// fold can see, in one window. Every absent piece names its dependency via
/// [`AssistedChangeShare::dependency`] — never an approximation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistedChangeShare {
    /// [`ASSISTED_CHANGE_SHARE_DEFINITION`], carried inline in the output.
    pub definition: String,
    /// Merged mission changes in the window (the native merged-change
    /// derivation) — every one agent-involved by construction.
    pub agent_changes: u64,
    /// First-parent commits landed on `base_branch` in the window (by
    /// committer date) — the total change count the fold can see. `None`
    /// when there is no base anchor (nothing closed in the window) or the
    /// git probe failed.
    pub total_changes: Option<u64>,
    /// The modal base branch of the window's closed missions (ties broken
    /// lexicographically) — the branch `total_changes` counts. Recorded even
    /// when the count itself is unavailable, so the report shows WHAT could
    /// not be counted.
    pub base_branch: Option<String>,
    /// agent_changes / total_changes — `None` when the denominator is
    /// unavailable or zero (absent, never a fabricated percentage).
    pub share: Option<f64>,
    /// What the slot lacks when `share` is `None` (the named dependency);
    /// `None` when the share computed.
    pub dependency: Option<String>,
}

/// Defect density per merged change: traced defects joined to missions
/// merged in the window, over those merged changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefectDensity {
    /// [`DEFECT_DENSITY_DEFINITION`], carried inline in the output.
    pub definition: String,
    /// Defect tickets whose `traced-from-mission` link joins a mission
    /// merged in the window (each ticket counts once — density counts
    /// defects, unlike the false-green rate which counts missions once).
    pub traced_defects: u64,
    /// Merged changes in the window (same derivation as
    /// [`AssistedChangeShare::agent_changes`]).
    pub merged_changes: u64,
    /// traced_defects / merged_changes — `None` when nothing merged in the
    /// window (the denominator is absent, never zero-filled).
    pub defects_per_merged_change: Option<f64>,
    /// What the slot lacks when the density is `None`; `None` when computed.
    pub dependency: Option<String>,
}

/// Defect resolution time (defect open → close). EMPTY today and naming its
/// dependency: the traced defect record carries lifecycle state but no
/// lifecycle timestamps. Value fields arrive additively when defect tickets
/// record open/close instants — the wire shape below is the stable part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefectResolutionTime {
    /// [`DEFECT_RESOLUTION_TIME_DEFINITION`], carried inline in the output.
    pub definition: String,
    /// What the slot lacks (`Some` while the data does not exist — always
    /// today); `None` once the metric computes.
    pub dependency: Option<String>,
}

/// Fold the industry-comparison set for one repo over `window_days` ending
/// at `now`. Pure over (event logs, ticket list, live git refs, the pinned
/// window): the same inputs always yield identical data, and nothing is
/// persisted. Degrades per-row exactly like
/// [`crate::outcomes::compute_cost_per_merged_change`]: a mission with an
/// unreadable/corrupt log is skipped; a log the strict reducer rejects
/// still anchors its base branch (recovered from `mission.created`
/// directly) but yields no merged change; and a repo git fails to open
/// simply yields no merged changes and no landed-changes count (the slots
/// read absent and name why, never zero). A `window_days` over
/// [`crate::outcomes::MAX_MERGED_CHANGE_WINDOW_DAYS`] is an honest error,
/// never a wrapped computation.
///
/// The per-mission log reads ride the native fold's memoized per-mission
/// struct ([`crate::outcomes::cached_mission_outcomes`], 14th-pass review —
/// this path used to re-read every events.jsonl `compute_outcomes` had just
/// parsed, scanning each log twice per request); only the git probes run
/// live here, since branch tips move independently of the logs.
pub fn compute_comparison_report(
    repo_root: &std::path::Path,
    window_days: u64,
    now: DateTime<Utc>,
) -> anyhow::Result<ComparisonReport> {
    let index_contents = std::fs::read_to_string(
        crate::paths::MissionPaths::new(repo_root, "_")
            .missions_dir()
            .join("index.md"),
    )
    .unwrap_or_default();

    let mut ids = crate::paths::MissionPaths::list_missions(repo_root);
    for id in crate::mission_catalog::mission_index_ids(&index_contents) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.sort();

    let mut inputs: Vec<(String, crate::outcomes::ComparisonInputs)> = Vec::new();
    for id in ids {
        let paths = crate::paths::MissionPaths::new(repo_root, &id);
        let events_path = paths.events_file();
        if !events_path.is_file() {
            continue;
        }
        if paths.require_no_follow().is_err() {
            continue;
        }
        let Some(out) = crate::outcomes::cached_mission_outcomes(&id, &events_path) else {
            continue; // corrupt log degrades per-mission, never fails
        };
        inputs.push((id, out.comparison));
    }
    comparison_report_from_inputs(repo_root, &inputs, window_days, now)
}

/// The comparison fold over PRE-FOLDED per-mission inputs — shared by
/// [`compute_comparison_report`] and the [`crate::outcomes`] report path, so
/// a request that already folded every log never scans one again. All
/// log-derived data comes in via `inputs`; only the git probes
/// (landed-changes denominator, per-mission merged bits) run here, live.
pub(crate) fn comparison_report_from_inputs(
    repo_root: &std::path::Path,
    inputs: &[(String, crate::outcomes::ComparisonInputs)],
    window_days: u64,
    now: DateTime<Utc>,
) -> anyhow::Result<ComparisonReport> {
    // Bound the window BEFORE any arithmetic — the same guard and rationale
    // as compute_cost_per_merged_change (the `u64 → i64` conversion and the
    // chrono subtraction must never wrap, for ANY caller).
    if window_days > crate::outcomes::MAX_MERGED_CHANGE_WINDOW_DAYS {
        return Err(crate::error::EngineError::InvalidState(format!(
            "window_days {window_days} exceeds the maximum {} days",
            crate::outcomes::MAX_MERGED_CHANGE_WINDOW_DAYS
        ))
        .into());
    }
    let days = i64::try_from(window_days).map_err(|_| {
        crate::error::EngineError::InvalidState(format!(
            "window_days {window_days} is out of range"
        ))
    })?;
    let window = chrono::Duration::try_days(days).ok_or_else(|| {
        crate::error::EngineError::InvalidState(format!(
            "window_days {window_days} is out of range"
        ))
    })?;
    let cutoff = now - window;
    let repo = crate::git_ops::GitRepo::open(repo_root).ok();

    // Merged mission changes in the window (the native derivation: closed
    // COMPLETE with the branch landed), plus every closed mission's base
    // branch — the anchor pool for the landed-changes denominator.
    let mut merged_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut base_counts: std::collections::BTreeMap<String, u64> =
        std::collections::BTreeMap::new();

    for (id, input) in inputs {
        // The window keys on the terminal event's own timestamp, inclusive at
        // both ends (the same rule as the sibling fold).
        let Some(terminal_ts) = input.terminal_ts else {
            continue; // still open — in no closed window
        };
        if terminal_ts < cutoff || terminal_ts > now {
            continue;
        }
        // The base-branch anchor comes from `mission.created` DIRECTLY (the
        // same recovery mission_outcomes uses for the config and goal): a
        // log the strict reducer rejects — hand-edited, or carrying an
        // event the reducer rules out — still anchors the denominator.
        if let Some(base) = &input.base_branch {
            *base_counts.entry(base.clone()).or_insert(0) += 1;
        }
        // Merged change: closed COMPLETE and the mission branch landed on the
        // live base (merged.rs's probe at fold time, never stored) — the
        // reducer-backed derivation exactly as compute_cost_per_merged_change
        // runs it; a log the reducer rejects simply yields no merged change
        // (degrade per-mission, never fail the fold).
        if let (Some(repo), Some(folded)) = (repo.as_ref(), input.folded.as_ref()) {
            if folded.status == crate::types::MissionStatus::Complete
                && crate::merged::merged_bit_for_branches(
                    repo,
                    &folded.mission_branch,
                    &folded.base_branch,
                ) == Some(true)
            {
                merged_ids.insert(id.clone());
            }
        }
    }

    // The landed-changes denominator: first-parent commits on the windowed
    // missions' modal base branch (most closed missions; ties resolve to the
    // lexicographically largest name — `max_by` keeps the last maximum over
    // the BTreeMap's ascending order — deterministic either way).
    // git's --since is exclusive and --until inclusive — the mission side of
    // the window is inclusive at both ends, a one-instant asymmetry at the
    // cutoff that the inline definition's "by commit date" phrasing covers.
    let base_branch = base_counts
        .iter()
        .max_by(|a, b| a.1.cmp(b.1))
        .map(|(branch, _)| branch.clone());
    let (total_changes, share_dependency) = match (&base_branch, repo.as_ref()) {
        (None, _) => (
            None,
            Some(
                "a base-branch anchor from windowed mission data — no missions \
                 closed in the window"
                    .to_string(),
            ),
        ),
        (Some(base), Some(repo)) => match repo.count_first_parent_commits(base, &cutoff, &now) {
            Ok(count) => (Some(count), None),
            Err(_) => (None, Some(git_denominator_dependency())),
        },
        (Some(_), None) => (None, Some(git_denominator_dependency())),
    };

    let agent_changes = merged_ids.len() as u64;
    let share = total_changes
        .filter(|total| *total > 0)
        .map(|total| agent_changes as f64 / total as f64);

    // Defect density: the landed false-green↔defect linkage
    // (escalation_metrics::traced_defects_from_tickets) joined to the
    // window's merged missions, over the merged changes themselves.
    let traced_defects = traced_defects_from_tickets(repo_root)
        .iter()
        .filter(|defect| merged_ids.contains(&defect.mission_id))
        .count() as u64;
    let merged_changes = agent_changes;
    let (defects_per_merged_change, density_dependency) = if merged_changes > 0 {
        (Some(traced_defects as f64 / merged_changes as f64), None)
    } else {
        (
            None,
            Some("merged changes in the window — the density denominator".to_string()),
        )
    };

    Ok(ComparisonReport {
        window_days,
        assisted_change_share: AssistedChangeShare {
            definition: ASSISTED_CHANGE_SHARE_DEFINITION.to_string(),
            agent_changes,
            total_changes,
            base_branch,
            share,
            dependency: share_dependency,
        },
        defect_density: DefectDensity {
            definition: DEFECT_DENSITY_DEFINITION.to_string(),
            traced_defects,
            merged_changes,
            defects_per_merged_change,
            dependency: density_dependency,
        },
        // EMPTY, naming its dependency: traced defect records carry no
        // lifecycle timestamps (see the module doc) — never approximated.
        defect_resolution_time: DefectResolutionTime {
            definition: DEFECT_RESOLUTION_TIME_DEFINITION.to_string(),
            dependency: Some(DEFECT_RESOLUTION_TIME_DEPENDENCY.to_string()),
        },
    })
}

/// The named dependency when the landed-changes denominator cannot be
/// produced: it is git-derived, so an unopenable repo or a missing base ref
/// leaves the slot empty with this reason.
fn git_denominator_dependency() -> String {
    "a git probe for the landed-changes denominator — the repository or the \
     base ref is unavailable"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, EventKind};
    use crate::types::MissionConfig;

    /// The fixed "now" every window assertion keys on (ms since epoch) — the
    /// window is an input, so the fold stays deterministic under test.
    const NOW_MS: i64 = 1_754_000_000_000;
    const DAY_MS: i64 = 86_400_000;

    fn now() -> DateTime<Utc> {
        DateTime::from_timestamp_millis(NOW_MS).unwrap()
    }

    fn ev(seq: u64, mission_id: &str, ts_ms: i64, kind: EventKind) -> Event {
        Event {
            seq,
            ts: DateTime::from_timestamp_millis(ts_ms).unwrap(),
            mission_id: mission_id.to_string(),
            kind,
        }
    }

    /// A mission created and closed COMPLETED at `terminal_ms` (the minimal
    /// log the reducer will fold).
    fn completed_mission_events(mission_id: &str, terminal_ms: i64) -> Vec<Event> {
        vec![
            ev(
                1,
                mission_id,
                terminal_ms - 1_000,
                EventKind::MissionCreated {
                    goal: "fixture mission".into(),
                    base_branch: "main".into(),
                    mission_branch: format!("kranz/{mission_id}"),
                    config: MissionConfig::default(),
                },
            ),
            ev(2, mission_id, terminal_ms, EventKind::MissionCompleted {}),
        ]
    }

    fn write_timed_events(repo_root: &std::path::Path, mission_id: &str, events: Vec<Event>) {
        let dir = repo_root.join(".kranz").join("missions").join(mission_id);
        std::fs::create_dir_all(&dir).unwrap();
        let lines: Vec<String> = events
            .iter()
            .map(|e| serde_json::to_string(e).unwrap())
            .collect();
        std::fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
    }

    #[test]
    fn comparison_metrics_empty_repo_yields_absent_slots_and_named_dependencies() {
        let tmp = tempfile::TempDir::new().unwrap();
        let report = compute_comparison_report(tmp.path(), 30, now()).unwrap();

        assert_eq!(report.window_days, 30);
        // Assisted-change share: no missions closed in the window, so there
        // is no base anchor — the slot is absent and names why.
        let share = &report.assisted_change_share;
        assert_eq!(share.agent_changes, 0);
        assert_eq!(share.total_changes, None);
        assert_eq!(share.base_branch, None);
        assert_eq!(share.share, None);
        assert!(
            share
                .dependency
                .as_deref()
                .is_some_and(|d| d.contains("no missions closed in the window")),
            "the empty slot names its dependency: {share:?}"
        );
        // Defect density: no merged changes — the denominator is absent,
        // never zero-filled.
        let density = &report.defect_density;
        assert_eq!(density.traced_defects, 0);
        assert_eq!(density.merged_changes, 0);
        assert_eq!(density.defects_per_merged_change, None);
        assert!(
            density
                .dependency
                .as_deref()
                .is_some_and(|d| d.contains("the density denominator")),
            "{density:?}"
        );
        // Defect resolution time: EMPTY, naming the missing lifecycle
        // timestamps — never approximated.
        let resolution = &report.defect_resolution_time;
        assert!(
            resolution
                .dependency
                .as_deref()
                .is_some_and(|d| d.contains("open/close timestamps")),
            "{resolution:?}"
        );
        // Each metric carries its inline definition (tested as content).
        assert!(share.definition.contains("agent-involved by construction"));
        assert!(density
            .definition
            .contains("traced-from-mission frontmatter"));
        assert!(resolution.definition.contains("no lifecycle timestamps"));
    }

    #[test]
    fn comparison_metrics_no_git_repo_degrades_the_git_derived_denominator() {
        let tmp = tempfile::TempDir::new().unwrap();
        // A mission closed inside the window, but the tempdir is no git
        // repository: the mission-derived anchor is recorded while the
        // git-derived count reads absent with its dependency named.
        write_timed_events(
            tmp.path(),
            "m-1",
            completed_mission_events("m-1", NOW_MS - DAY_MS),
        );

        let report = compute_comparison_report(tmp.path(), 30, now()).unwrap();
        let share = &report.assisted_change_share;
        assert_eq!(share.agent_changes, 0, "no merged probe without git");
        assert_eq!(share.base_branch.as_deref(), Some("main"));
        assert_eq!(share.total_changes, None);
        assert_eq!(share.share, None);
        assert!(
            share
                .dependency
                .as_deref()
                .is_some_and(|d| d.contains("a git probe")),
            "{share:?}"
        );
    }

    #[test]
    fn comparison_metrics_window_over_max_is_an_honest_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let result = compute_comparison_report(
            tmp.path(),
            crate::outcomes::MAX_MERGED_CHANGE_WINDOW_DAYS + 1,
            now(),
        );
        assert!(result.is_err(), "an over-bound window errors, never wraps");
    }
}
