//! Deterministic signal triage: fingerprint deduplication within a configurable
//! window, project-to-repo mapping, rule-based outcome classification, and
//! path-overlap candidate mission discovery (`docs/scoping/signal-intake.md`
//! §§D-E, D-F; ticket `signal-intake-4-triage`).
//!
//! No model reads signal content during triage.

use crate::error::Result;
use crate::event_log::EventLog;
use crate::git_ops::GitRepo;
use crate::paths::MissionPaths;
use crate::reducer;
use crate::signals::ledger::{DecisionOutcome, SignalLedger, SignalLedgerEntry, SignalRecordKind};
use crate::signals::sentry::SentryConfig;
use crate::signals::source::NormalisedSignal;
use crate::types::MissionStatus;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Default deduplication window per `(source, fingerprint)`: 24 hours.
pub const DEFAULT_DEDUPE_WINDOW_SECS: u64 = 24 * 60 * 60;

/// Declarative classification rule evaluated in order during deterministic triage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalClassificationRule {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub severity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_prefix: Option<String>,
    pub outcome: DecisionOutcome,
}

impl SignalClassificationRule {
    pub fn matches(&self, signal: &NormalisedSignal) -> bool {
        if let Some(src) = &self.source {
            if !src.eq_ignore_ascii_case(&signal.source) {
                return false;
            }
        }
        if let Some(proj) = &self.project {
            if proj != &signal.project {
                return false;
            }
        }
        if let Some(env) = &self.environment {
            if signal.environment.as_deref() != Some(env.as_str()) {
                return false;
            }
        }
        if let Some(sev) = &self.severity {
            if !sev.eq_ignore_ascii_case(signal.severity.as_str()) {
                return false;
            }
        }
        if let Some(prefix) = &self.fingerprint_prefix {
            if !signal.fingerprint.starts_with(prefix.as_str()) {
                return false;
            }
        }
        if let Some(tag) = &self.tag {
            if !signal.tags.iter().any(|t| t == tag) {
                return false;
            }
        }
        if let Some(fp) = &self.frame_prefix {
            if !signal.frames.iter().any(|f| f.starts_with(fp.as_str())) {
                return false;
            }
        }
        true
    }
}

/// Per-repo / global `signals` configuration block in `.kranz/config.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalsConfig {
    /// Monitoring project slug -> repository id or canonical name.
    /// Any signal whose `project` is not present in `project_map` is parked
    /// with rule `"unknown-project"`.
    #[serde(default)]
    pub project_map: BTreeMap<String, String>,
    /// Default dedupe window in seconds (defaults to 86,400 = 24 hours).
    #[serde(default = "default_dedupe_window_secs")]
    pub dedupe_window_secs: u64,
    /// Optional per-source dedupe window overrides in seconds.
    #[serde(default)]
    pub source_dedupe_windows: BTreeMap<String, u64>,
    /// Ordered classification rules; first matching rule wins.
    #[serde(default)]
    pub rules: Vec<SignalClassificationRule>,
    /// Optional Sentry adapter configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentry: Option<SentryConfig>,
}

fn default_dedupe_window_secs() -> u64 {
    DEFAULT_DEDUPE_WINDOW_SECS
}

impl Default for SignalsConfig {
    fn default() -> Self {
        Self {
            project_map: BTreeMap::new(),
            dedupe_window_secs: DEFAULT_DEDUPE_WINDOW_SECS,
            source_dedupe_windows: BTreeMap::new(),
            rules: Vec::new(),
            sentry: None,
        }
    }
}

impl SignalsConfig {
    /// Load the merged `signals` configuration block from `~/.kranz/config.json`
    /// and `<repo>/.kranz/config.json`.
    pub fn load(repo_root: &Path) -> Result<Self> {
        let mut merged = SignalsConfig::default();
        if let Some(global_path) = crate::paths::global_config() {
            if let Ok(text) = std::fs::read_to_string(&global_path) {
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(sig_val) = val.get("signals") {
                        if let Ok(cfg) = serde_json::from_value::<SignalsConfig>(sig_val.clone()) {
                            merged = cfg;
                        }
                    }
                }
            }
        }
        let repo_cfg_path = repo_root.join(".kranz").join("config.json");
        if let Ok(text) = std::fs::read_to_string(&repo_cfg_path) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(sig_val) = val.get("signals") {
                    if let Ok(repo_cfg) = serde_json::from_value::<SignalsConfig>(sig_val.clone()) {
                        for (k, v) in repo_cfg.project_map {
                            merged.project_map.insert(k, v);
                        }
                        if sig_val.get("dedupeWindowSecs").is_some() {
                            merged.dedupe_window_secs = repo_cfg.dedupe_window_secs;
                        }
                        for (k, v) in repo_cfg.source_dedupe_windows {
                            merged.source_dedupe_windows.insert(k, v);
                        }
                        if !repo_cfg.rules.is_empty() {
                            merged.rules = repo_cfg.rules;
                        }
                        if repo_cfg.sentry.is_some() {
                            merged.sentry = repo_cfg.sentry;
                        }
                    }
                }
            }
        }
        Ok(merged)
    }

    pub fn dedupe_window_for(&self, source: &str) -> u64 {
        self.source_dedupe_windows
            .get(source)
            .copied()
            .unwrap_or(self.dedupe_window_secs)
    }
}

/// Detailed triage outcome returned by [`triage_signal`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TriageDecision {
    pub signal_id: String,
    pub outcome: DecisionOutcome,
    pub rule: String,
    pub target_repo: Option<String>,
    pub dedupe_count: u64,
    pub is_duplicate: bool,
    pub candidate_missions: Vec<String>,
    pub ledger_entry: SignalLedgerEntry,
}

pub type TriageOutcome = DecisionOutcome;

/// Deterministically triage a normalised signal, record the `Triaged` decision
/// in `.kranz/signals/ledger.jsonl`, and return [`TriageDecision`].
pub fn triage_signal(
    repo_root: &Path,
    signal_id: &str,
    signal: &NormalisedSignal,
    config: &SignalsConfig,
    now: DateTime<Utc>,
) -> Result<TriageDecision> {
    let entries = SignalLedger::read_entries(repo_root)?;
    let window_secs = config.dedupe_window_for(&signal.source);
    let cutoff = now - Duration::seconds(window_secs as i64);

    // Count existing occurrences of `(source, fingerprint)` within the dedupe window.
    let prior_count: u64 = entries
        .iter()
        .filter(|e| e.timestamp >= cutoff)
        .filter_map(|e| match &e.record {
            SignalRecordKind::Triaged {
                source,
                fingerprint,
                dedupe_count,
                ..
            } if source == &signal.source && fingerprint == &signal.fingerprint => {
                Some(*dedupe_count)
            }
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let dedupe_count = prior_count + 1;
    let is_duplicate = prior_count > 0;

    // 1. Project-to-repo mapping: an unknown project always parks.
    let target_repo = config.project_map.get(&signal.project).cloned();
    let (outcome, rule) = if target_repo.is_none() {
        (DecisionOutcome::Parked, "unknown-project".to_string())
    } else if let Some(matched) = config.rules.iter().find(|r| r.matches(signal)) {
        (matched.outcome.clone(), matched.name.clone())
    } else {
        (DecisionOutcome::Proposed, "default-propose".to_string())
    };

    // 2. Discover path-overlap candidate missions from recently completed/merged missions.
    let candidate_missions = if outcome == DecisionOutcome::Proposed {
        suggest_traced_missions(repo_root, &signal.frames)
    } else {
        Vec::new()
    };

    let ledger_entry = SignalLedger::append(
        repo_root,
        SignalRecordKind::Triaged {
            signal_id: signal_id.to_string(),
            source: signal.source.clone(),
            fingerprint: signal.fingerprint.clone(),
            outcome: outcome.clone(),
            rule: rule.clone(),
            dedupe_count,
            candidate_missions: candidate_missions.clone(),
            project: Some(signal.project.clone()),
            severity: Some(signal.severity.as_str().to_string()),
            environment: signal.environment.clone(),
        },
    )?;

    Ok(TriageDecision {
        signal_id: signal_id.to_string(),
        outcome,
        rule,
        target_repo,
        dedupe_count,
        is_duplicate,
        candidate_missions,
        ledger_entry,
    })
}

/// Suggest `traced-from-mission` candidate mission IDs whose touched paths
/// overlap `signal_frames`.
///
/// Inspects completed missions under `.kranz/missions/` (both `touch_set` in the
/// folded state/plan and git diff paths between `base_sha` and `mission_branch`
/// when available). Nothing writes `traced-from-mission` automatically — a
/// human confirms a candidate at consent/draft time (`signal-intake-8-linkage`).
pub fn suggest_traced_missions(repo_root: &Path, signal_frames: &[String]) -> Vec<String> {
    if signal_frames.is_empty() {
        return Vec::new();
    }
    let normalised_frames: Vec<String> = signal_frames
        .iter()
        .map(|f| {
            f.trim()
                .trim_start_matches("./")
                .trim_start_matches('/')
                .to_string()
        })
        .filter(|f| !f.is_empty())
        .collect();
    if normalised_frames.is_empty() {
        return Vec::new();
    }

    let git = GitRepo::open(repo_root).ok();
    let mut candidates = Vec::new();

    for mission_id in MissionPaths::list_missions(repo_root) {
        let paths = MissionPaths::new(repo_root, &mission_id);
        let Ok(events) = EventLog::read_events(&paths.events_file()) else {
            continue;
        };
        let Ok(state) = reducer::fold(&events) else {
            continue;
        };
        if state.mission.status != MissionStatus::Complete {
            continue;
        }

        let mut touched: BTreeSet<String> = BTreeSet::new();
        for p in &state.mission.touch_set {
            touched.insert(
                p.trim_start_matches("./")
                    .trim_start_matches('/')
                    .to_string(),
            );
        }
        for run in state.runs.values() {
            if let Some(report) = &run.report {
                for p in &report.files_touched {
                    touched.insert(
                        p.trim_start_matches("./")
                            .trim_start_matches('/')
                            .to_string(),
                    );
                }
            }
        }
        if let (Some(repo), Some(base_sha)) = (&git, state.mission.base_sha.as_deref()) {
            let branch = &state.mission.mission_branch;
            if !branch.is_empty() {
                if let Ok(diff_paths) = repo.changed_paths(base_sha, branch) {
                    for p in diff_paths {
                        touched.insert(
                            p.trim_start_matches("./")
                                .trim_start_matches('/')
                                .to_string(),
                        );
                    }
                }
            }
        }

        let overlaps = normalised_frames.iter().any(|frame| {
            touched.iter().any(|touched_path| {
                touched_path == frame
                    || (frame.ends_with(touched_path.as_str())
                        && frame.as_bytes().get(frame.len() - touched_path.len() - 1)
                            == Some(&b'/'))
                    || (touched_path.ends_with(frame.as_str())
                        && touched_path
                            .as_bytes()
                            .get(touched_path.len() - frame.len() - 1)
                            == Some(&b'/'))
            })
        });
        if overlaps {
            candidates.push(mission_id);
        }
    }

    candidates.sort();
    candidates.dedup();
    candidates
}
