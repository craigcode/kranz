//! Engine-observed ACP resource evidence. Configured ceilings are not evidence
//! of exhaustion; peer text and process exit codes never select a class.
use crate::acp_worker::Resources;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FailureClass {
    Memory,
    Pids,
    WallClock,
    TerminalBudget,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Counters {
    pub oom_kill: Option<u64>,
    pub pids_max: Option<u64>,
    pub throttled_usec: Option<u64>,
}

/// Read through an engine-held descriptor, never through a worker-writable
/// pathname. Times are milliseconds since the trusted supervisor started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Sample {
    pub owner: String,
    pub elapsed_ms: u64,
    pub complete: bool,
    pub wall_clock_expired: bool,
    pub baseline: Counters,
    pub current: Counters,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerState {
    pub running: bool,
    pub oom_killed: bool,
    pub exit_code: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceEvidence {
    pub container_id: String,
    pub image: String,
    pub supervisor_sha256: String,
    pub owner: String,
    pub limits: Resources,
    pub observation_started_at: chrono::DateTime<chrono::Utc>,
    pub observation_finished_at: chrono::DateTime<chrono::Utc>,
    pub state: Option<ContainerState>,
    pub sample: Option<Sample>,
    /// Missing observations are explicit, never a zero counter or clean bill.
    pub unavailable: Vec<String>,
    pub namespace_cleanup_confirmed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<FailureClass>,
}

fn increased(before: Option<u64>, after: Option<u64>) -> bool {
    matches!((before, after), (Some(before), Some(after)) if after > before)
}

impl ResourceEvidence {
    fn memory_hit(&self) -> bool {
        self.state.as_ref().is_some_and(|s| s.oom_killed)
            || self
                .sample
                .as_ref()
                .is_some_and(|s| increased(s.baseline.oom_kill, s.current.oom_kill))
    }

    fn wall_hit(&self) -> bool {
        self.sample.as_ref().is_some_and(|s| s.wall_clock_expired)
    }

    pub(crate) fn requires_failure(&self) -> bool {
        self.memory_hit() || self.wall_hit()
    }

    pub(crate) fn classify(&mut self, failed: bool) {
        self.failure = if failed || self.requires_failure() {
            let pids = self
                .sample
                .as_ref()
                .is_some_and(|s| increased(s.baseline.pids_max, s.current.pids_max));
            Some(match (self.memory_hit(), pids, self.wall_hit()) {
                (true, false, false) => FailureClass::Memory,
                (false, true, false) => FailureClass::Pids,
                (false, false, true) => FailureClass::WallClock,
                // Multiple hits do not identify which ended the attempt.
                _ => FailureClass::Other,
            })
        } else {
            None
        };
    }

    pub(crate) fn failure_message(&self) -> Option<String> {
        let (name, value, key) = match self.failure? {
            FailureClass::Memory => ("memory", format!("{} MiB (no swap)", self.limits.memory_mib), "memoryMib"),
            FailureClass::Pids => ("process count", self.limits.pids.to_string(), "pids"),
            FailureClass::WallClock => ("session wall clock", format!("{} seconds", self.limits.session_seconds), "sessionSeconds"),
            FailureClass::TerminalBudget | FailureClass::Other => return Some("ACP attempt failed; resource observations do not establish a unique exhausted limit. Inspect resourceEvidence; exit codes alone are not resource attribution.".into()),
        };
        Some(format!("ACP {name} exhaustion observed during this failed attempt; configured ceiling: {value}; operator setting worker.acpProfile.resources.{key} in ~/.kranz/config.json (within the profile maximum)."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> ResourceEvidence {
        let counters = Counters {
            oom_kill: Some(0),
            pids_max: Some(0),
            throttled_usec: Some(0),
        };
        ResourceEvidence {
            container_id: "a".repeat(64),
            image: "sha256:fixture".into(),
            supervisor_sha256: "b".repeat(64),
            owner: "owned-fixture".into(),
            limits: Resources {
                memory_mib: 256,
                cpu_millis: 1_000,
                pids: 64,
                nofile: 256,
                fsize_mib: 64,
                tmpfs_mib: 64,
                session_seconds: 3_600,
            },
            observation_started_at: chrono::Utc::now(),
            observation_finished_at: chrono::Utc::now(),
            state: Some(ContainerState {
                running: false,
                oom_killed: false,
                exit_code: 137,
            }),
            sample: Some(Sample {
                owner: "owned-fixture".into(),
                elapsed_ms: 200,
                complete: true,
                wall_clock_expired: false,
                baseline: counters.clone(),
                current: counters,
            }),
            unavailable: vec![],
            namespace_cleanup_confirmed: true,
            failure: None,
        }
    }

    #[test]
    fn acp_resource_classification_requires_observations_not_status_or_config() {
        let mut e = evidence();
        e.classify(true);
        assert_eq!(e.failure, Some(FailureClass::Other));
        e.state.as_mut().unwrap().oom_killed = true;
        e.classify(true);
        assert_eq!(e.failure, Some(FailureClass::Memory));
        assert!(e.failure_message().unwrap().contains("256 MiB"));
        e.state.as_mut().unwrap().oom_killed = false;
        e.sample.as_mut().unwrap().current.pids_max = Some(1);
        e.classify(true);
        assert_eq!(e.failure, Some(FailureClass::Pids));
        // A handled fork failure is not a failed session.
        e.classify(false);
        assert_eq!(e.failure, None);
        e.sample.as_mut().unwrap().current.oom_kill = Some(1);
        e.classify(true);
        assert_eq!(
            e.failure,
            Some(FailureClass::Other),
            "concurrent hits are ambiguous"
        );
        assert!(e.requires_failure());
        e.sample = None;
        e.state = None;
        e.classify(true);
        assert_eq!(e.failure, Some(FailureClass::Other));
    }

    #[test]
    fn acp_resource_throttling_and_counter_resets_never_manufacture_failures() {
        let mut e = evidence();
        let s = e.sample.as_mut().unwrap();
        s.current.throttled_usec = Some(900_000);
        s.baseline.pids_max = Some(10);
        s.current.pids_max = Some(9);
        e.classify(false);
        assert_eq!(e.failure, None);
        e.classify(true);
        assert_eq!(e.failure, Some(FailureClass::Other));
        e.sample.as_mut().unwrap().wall_clock_expired = true;
        e.classify(false);
        assert_eq!(e.failure, Some(FailureClass::WallClock));
        assert!(e
            .failure_message()
            .unwrap()
            .contains("worker.acpProfile.resources.sessionSeconds"));
    }

    #[test]
    fn acp_resource_event_contract_reads_old_logs_and_round_trips_new_evidence() {
        use crate::events::EventKind;
        let old = serde_json::json!({"type":"worker.completed","payload":{"runId":"r-fixture","result":"fail","tokens":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}}});
        let legacy: EventKind = serde_json::from_value(old.clone()).unwrap();
        assert!(matches!(
            legacy,
            EventKind::WorkerCompleted {
                resource_evidence: None,
                ..
            }
        ));
        assert_eq!(serde_json::to_value(&legacy).unwrap(), old);
        let mut e = evidence();
        e.classify(true);
        let new = EventKind::WorkerCompleted {
            resource_evidence: Some(Box::new(e.clone())),
            run_id: "r-fixture".into(),
            result: crate::types::RunResult::Fail,
            tokens: Default::default(),
            cost_usd: None,
            report: None,
        };
        let decoded: EventKind =
            serde_json::from_slice(&serde_json::to_vec(&new).unwrap()).unwrap();
        assert!(
            matches!(decoded, EventKind::WorkerCompleted { resource_evidence: Some(actual), .. } if *actual == e)
        );
    }
}
