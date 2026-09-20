//! External merge gates judge a scratch integration snapshot under a held
//! event-log lock. Required merge commands are selected from the live base.
use super::MergeReport;
use crate::{
    error::Result,
    event_log::{EventLog, LockForce},
    events::{Event, EventKind},
    gate_evaluation::{
        authority::Authority, driver, input_builder::*, lifecycle::*, protocol::*,
        snapshot::SourceSnapshot,
    },
    git_ops::GitRepo,
    live_permission::Actor,
    merge_gate::GateSuite,
    pack::evaluator::PinnedRegistration,
    paths::MissionPaths,
    types::*,
};

pub(super) struct Audit {
    paths: MissionPaths,
    log: EventLog,
    state: MissionState,
    authority: Authority,
    actor: Actor,
    snapshot: Option<SourceSnapshot>,
}

impl Audit {
    pub fn open(repo: &GitRepo, paths: &MissionPaths, actor: Actor) -> Result<Option<Self>> {
        if actor == Actor::Policy {
            return Err(driver::invalid(
                "merge requires invoking operator authority",
            ));
        }
        let events = EventLog::read_events(&paths.events_file())?;
        let state = crate::reducer::fold(&events)?;
        let authority = Authority::from_events(repo, &state, &events)?;
        if authority.registrations.is_empty() {
            return Ok(None);
        }
        let log = EventLog::acquire(
            paths,
            &paths.mission_id,
            std::time::Duration::ZERO,
            LockForce::No,
        )?;
        let events = EventLog::read_events(&paths.events_file())?;
        crate::event_log::check_no_rollback(paths, &events)?;
        let state = crate::reducer::fold(&events)?;
        let authority = Authority::from_events(repo, &state, &events)?;
        if state.mission.status != MissionStatus::Complete {
            return Err(driver::invalid("merge requires a completed mission"));
        }
        let mut audit = Self {
            paths: paths.clone(),
            log,
            state,
            authority,
            actor,
            snapshot: None,
        };
        audit.close("superseded by a fresh explicit merge attempt")?;
        Ok(Some(audit))
    }

    pub fn verify_target(
        &self,
        base_branch: &str,
        base_sha: &str,
        mission_branch: &str,
    ) -> Result<()> {
        if self.state.mission.base_branch != base_branch
            || self.authority.base != base_sha
            || self.state.mission.mission_branch != mission_branch
        {
            return Err(driver::invalid(
                "merge target differs from approved mission",
            ));
        }
        Ok(())
    }

    fn emit(&mut self, kind: EventKind) -> Result<Event> {
        let mut probe = self.state.clone();
        crate::reducer::apply(
            &mut probe,
            &Event {
                seq: self.state.last_seq + 1,
                ts: chrono::Utc::now(),
                mission_id: self.paths.mission_id.clone(),
                kind: kind.clone(),
            },
        )?;
        let (event, audits) = self.log.append_with_redaction_audits(kind)?;
        crate::reducer::apply(&mut self.state, &event)?;
        for audit in audits {
            crate::reducer::apply(&mut self.state, &audit)?;
        }
        crate::reducer::write_snapshot(&self.state, &self.paths.state_file())?;
        Ok(event)
    }

    fn close(&mut self, reason: &str) -> Result<()> {
        let attempts: Vec<_> = self
            .state
            .gate_evaluations
            .values()
            .filter(|r| r.consumed.is_none() && r.closed.is_none())
            .map(|r| r.requested.request.params.attempt_id.clone())
            .collect();
        for attempt_id in attempts {
            self.emit(EventKind::GateEvaluationClosed {
                attempt_id,
                reason: reason.into(),
            })?;
        }
        Ok(())
    }

    pub fn prepare(&mut self, repo: &GitRepo, scratch: &GitRepo, live_base: &str) -> Result<()> {
        // A new base commit alone is not drift; the actual declaration,
        // checker bytes and manifest must still match the approved policy.
        let current = PinnedRegistration::configured_at_ref(repo, &self.state.config, live_base)
            .map_err(driver::invalid)?;
        let normalize = |registration: &PinnedRegistration| -> Result<serde_json::Value> {
            let mut value: serde_json::Value = serde_json::from_slice(registration.bytes())?;
            value
                .as_object_mut()
                .ok_or_else(|| driver::invalid("invalid registration"))?
                .remove("sourceCommit");
            Ok(value)
        };
        let approved = self
            .authority
            .registrations
            .iter()
            .map(normalize)
            .collect::<Result<Vec<_>>>()?;
        let current = current.iter().map(normalize).collect::<Result<Vec<_>>>()?;
        if approved != current {
            return Err(driver::invalid(
                "live base evaluator policy drifted; revalidation and approval are required",
            ));
        }
        self.snapshot = Some(SourceSnapshot::capture(scratch, live_base).map_err(driver::invalid)?);
        Ok(())
    }

    pub fn evaluate(
        &mut self,
        repo: &GitRepo,
        scratch: &GitRepo,
        live_base: &str,
        candidate: &str,
        suite: &GateSuite,
        changed: &[String],
    ) -> Result<()> {
        let snapshot = self
            .snapshot
            .take()
            .ok_or_else(|| driver::invalid("merge snapshot is absent"))?;
        snapshot.verify_current(scratch).map_err(driver::invalid)?;
        if !self.authority.applies(Stage::Merge) {
            return Ok(());
        }
        let policy = crate::command_exec::MergeGatePolicy {
            sandbox: crate::command_exec::worker_gate_sandbox(&self.state.config)?,
            mission_dir: self.paths.mission_dir(),
        };
        let environment = Digest::of(&serde_json::to_vec(&(
            std::env::consts::OS,
            std::env::consts::ARCH,
            scratch.root(),
            &policy.sandbox,
            "merge-sanitized-env-cache-only-cargo-v1",
        ))?);
        let content = Digest::of(&serde_json::to_vec(&snapshot.identity)?);
        let run_id = driver::id("merge-checks");
        let mut observed = Vec::new();
        let mut required = Vec::new();
        // The legacy ladder is preserved. Repeat its required commands here
        // through the typed runner to retain real exits on this frozen tree;
        // its injectable boolean executor cannot supply process receipts.
        for (i, gate) in suite
            .gates
            .iter()
            .enumerate()
            .filter(|(_, g)| crate::merge_gate::when_paths_match(&g.when_paths, changed))
        {
            let check_id = Id::try_from(format!("merge-{i}")).map_err(driver::invalid)?;
            let cwd = scratch.root().join(&gate.cwd);
            let (exit_code, output) =
                crate::command_exec::run_bounded_gate_command_sandboxed_with_code(
                    &cwd,
                    &gate.command,
                    &policy,
                );
            let event = self.emit(EventKind::OrchestratorDecision {
                summary: format!("external merge check {i}: exit {exit_code:?}"),
                detail: Some(crate::scrub::scrub_and_truncate(&output, 8192)),
            })?;
            let command = format!("cwd={}; {}", gate.cwd, gate.command);
            required.push(RequiredCheck {
                id: check_id.clone(),
                command: command.clone(),
                require_assertions: false,
            });
            observed.push(ObservedCheck {
                check_id,
                run_id: run_id.clone(),
                sequence: event.seq,
                checked_content: content.clone(),
                environment: environment.clone(),
                command,
                exit_code,
                assertions_executed: None,
                output_summary: Some(crate::scrub::scrub_and_truncate(&output, 8192)),
            });
        }
        snapshot.verify_current(scratch).map_err(driver::invalid)?;
        let events = EventLog::read_events(self.log.events_path())?;
        let authority = Authority::from_events(repo, &self.state, &events)?;
        self.authority.verify(&authority)?;
        let paths = self.paths.clone();
        let records = driver::evaluate_stage(
            driver::StageEvaluation {
                paths: &paths,
                source_log_range: Some(LogRange {
                    first: 1,
                    last: self.state.last_seq,
                }),
                registrations: &authority.registrations,
                plan_bytes: &authority.plan_bytes,
                mission_policy_digest: authority.policy.clone(),
                stage: StageInput::Integration {
                    snapshot: &snapshot,
                    live_base_commit: git_object(live_base).map_err(driver::invalid)?,
                    candidate_commit: git_object(candidate).map_err(driver::invalid)?,
                    integration_tree: git_object(&scratch.rev_parse("HEAD^{tree}")?)
                        .map_err(driver::invalid)?,
                },
                checks: Checks {
                    environment,
                    required: &required,
                    observed: &observed,
                },
                diagnostics: &[],
                prior_findings: &[],
                consent: Some(Consent {
                    actor: self.actor.clone(),
                    allow: true,
                    reference: format!("merge:{}:{live_base}:{candidate}", self.state.mission.id),
                }),
            },
            |kind| self.emit(kind),
        )?;
        if let Err(error) = snapshot.verify_current(scratch).map_err(driver::invalid) {
            self.close("integration bytes changed while checks ran")?;
            return Err(error);
        }
        if repo.rev_parse(&self.state.mission.base_branch)? != live_base
            || repo.rev_parse(&self.state.mission.mission_branch)? != candidate
        {
            self.close("base or candidate changed while merge checks ran")?;
            return Err(driver::invalid(
                "base or candidate changed while merge checks ran",
            ));
        }
        for record in records {
            driver::consume(
                &record,
                record.requested.request.params.binding.clone(),
                Action::AdvanceLocalBase,
                |kind| self.emit(kind),
            )?;
        }
        Ok(())
    }

    pub fn record_outcome(&mut self, report: &Result<MergeReport>) -> Result<()> {
        self.emit(EventKind::OrchestratorDecision {
            summary: "local merge attempt finished".into(),
            detail: Some(crate::scrub::scrub_and_truncate(
                &format!("{report:?}"),
                8192,
            )),
        })?;
        Ok(())
    }
}
