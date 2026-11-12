//! How long a mission waited in the queue and how long it ran (ticket
//! `queue-wait-metrics`).
//!
//! Nothing recorded when a mission was queued: the queue entry is a file that
//! is removed once the mission finishes. [`record_enqueue`] writes a small
//! receipt beside the mission's other runtime files at the moment it is
//! queued, naming what (if anything) was running then. Everything else is
//! derived from the event log: the first `milestone.started` or
//! `worker.spawned` is when it started, the terminal event is when it ended.
//!
//! Read-only reporting. Nothing here decides anything.

use crate::events::EventKind;
use crate::paths::MissionPaths;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const RECEIPT_FILE: &str = "queue-receipt.json";

/// Written once, when a mission is first queued.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueReceipt {
    pub queued_at: DateTime<Utc>,
    /// The mission running in this repository when this one was queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_at_enqueue: Option<String>,
}

/// Under `runs/`, which is already a runtime (never committed) directory.
fn receipt_path(repo_root: &Path, mission_id: &str) -> PathBuf {
    MissionPaths::new(repo_root, mission_id)
        .runs_dir()
        .join(RECEIPT_FILE)
}

/// Record that `mission_id` was just queued. Best-effort: a mission must
/// queue even when its timing receipt cannot be written, and an existing
/// receipt (a re-queue after a release) is left as the first one.
pub(crate) fn record_enqueue(repo_root: &Path, mission_id: &str) {
    if !MissionPaths::is_safe_id(mission_id) {
        return;
    }
    let path = receipt_path(repo_root, mission_id);
    if path.exists() {
        return;
    }
    let receipt = QueueReceipt {
        queued_at: Utc::now(),
        running_at_enqueue: crate::queue::is_repo_busy(repo_root)
            .filter(|running| running != mission_id),
    };
    let written = path
        .parent()
        .map(std::fs::create_dir_all)
        .transpose()
        .and_then(|_| {
            serde_json::to_vec_pretty(&receipt)
                .map_err(std::io::Error::other)
                .and_then(|bytes| {
                    // Created exclusively: never written through a link
                    // planted at this name.
                    use std::io::Write as _;
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&path)
                        .and_then(|mut file| file.write_all(&bytes))
                })
        });
    if let Err(error) = written {
        tracing::warn!(mission = mission_id, %error, "could not record queue timing receipt");
    }
}

/// One mission's queue and run times.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MissionTiming {
    pub mission_id: String,
    /// `None` for a mission that was never queued (a hosted start), or one
    /// queued before receipts existed.
    pub queued_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub running_at_enqueue: Option<String>,
    /// Whether the approved plan declared write paths.
    pub declared_write_paths: Option<bool>,
}

impl MissionTiming {
    /// Seconds between being queued and starting. `None` when either time is
    /// unknown; never zero by default.
    pub fn waited_secs(&self) -> Option<i64> {
        let waited = (self.started_at? - self.queued_at?).num_seconds();
        Some(waited.max(0))
    }

    /// Seconds between starting and reaching a terminal state.
    pub fn ran_secs(&self) -> Option<i64> {
        Some((self.ended_at? - self.started_at?).num_seconds().max(0))
    }
}

/// Timing for one mission, from its receipt and event log.
pub fn mission_timing(repo_root: &Path, mission_id: &str) -> crate::error::Result<MissionTiming> {
    if !MissionPaths::is_safe_id(mission_id) {
        return Err(crate::error::EngineError::Config(format!(
            "unknown mission '{mission_id}'"
        )));
    }
    let paths = MissionPaths::new(repo_root, mission_id);
    let events = crate::event_log::EventLog::read_events(&paths.events_file())?;
    let receipt: Option<QueueReceipt> = std::fs::read(receipt_path(repo_root, mission_id))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());

    let mut started_at = None;
    let mut ended_at = None;
    let mut declared_write_paths = None;
    for event in &events {
        match &event.kind {
            EventKind::PlanApproved { plan, .. } => {
                declared_write_paths = Some(!plan.touch_set.is_empty());
            }
            EventKind::MilestoneStarted { .. } | EventKind::WorkerSpawned { .. }
                if started_at.is_none() =>
            {
                started_at = Some(event.ts);
            }
            EventKind::MissionCompleted {}
            | EventKind::MissionFailed { .. }
            | EventKind::MissionAbandoned { .. } => ended_at = Some(event.ts),
            _ => {}
        }
    }
    Ok(MissionTiming {
        mission_id: mission_id.to_string(),
        queued_at: receipt.as_ref().map(|r| r.queued_at),
        started_at,
        ended_at,
        running_at_enqueue: receipt.and_then(|r| r.running_at_enqueue),
        declared_write_paths,
    })
}

/// A per-repository summary over every mission that can be read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimingSummary {
    pub missions: Vec<MissionTiming>,
    /// Missions with a known queue wait.
    pub measured: usize,
    pub total_waited_secs: i64,
    pub median_waited_secs: Option<i64>,
    /// Of those, how many were queued while another mission was running.
    pub waited_behind_another: usize,
    pub plans_approved: usize,
    pub plans_declaring_write_paths: usize,
}

pub fn summary(repo_root: &Path) -> TimingSummary {
    let mut missions: Vec<MissionTiming> = MissionPaths::list_missions(repo_root)
        .iter()
        .filter_map(|id| mission_timing(repo_root, id).ok())
        .collect();
    missions.sort_by(|a, b| a.mission_id.cmp(&b.mission_id));
    let mut waits: Vec<i64> = missions
        .iter()
        .filter_map(MissionTiming::waited_secs)
        .collect();
    waits.sort_unstable();
    let plans: Vec<bool> = missions
        .iter()
        .filter_map(|m| m.declared_write_paths)
        .collect();
    TimingSummary {
        measured: waits.len(),
        total_waited_secs: waits.iter().sum(),
        median_waited_secs: (!waits.is_empty()).then(|| waits[waits.len() / 2]),
        waited_behind_another: missions
            .iter()
            .filter(|m| m.waited_secs().is_some() && m.running_at_enqueue.is_some())
            .count(),
        plans_approved: plans.len(),
        plans_declaring_write_paths: plans.iter().filter(|declared| **declared).count(),
        missions,
    }
}
