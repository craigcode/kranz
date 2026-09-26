//! Real stage drivers for the external evaluation lifecycle.
use super::MissionEngine;
use crate::error::Result;
use crate::events::EventKind;
use crate::gate_evaluation::{authority, snapshot::SourceSnapshot};
use crate::gate_evaluation::{driver, input_builder::*, lifecycle::*, protocol::*};
use crate::pack::evaluator::PinnedRegistration;
use crate::types::AssertionCheck;
use crate::types::Plan;

impl MissionEngine {
    pub(super) fn external_authority(
        &mut self,
    ) -> Result<(authority::Authority, Vec<crate::events::Event>)> {
        self.log.flush()?;
        let events = crate::event_log::EventLog::read_events(self.log.events_path())?;
        let authority = authority::Authority::from_events(&self.repo, &self.state, &events)?;
        Ok((authority, events))
    }

    /// Existing milestone commands are advisory; final command assertions are
    /// nonwaivable. Both carry actual process exits, never worker assertions.
    pub(super) async fn external_completion_checks(
        &mut self,
        milestone: Option<usize>,
    ) -> Result<bool> {
        let result = Box::pin(self.external_completion_attempt(milestone)).await;
        match result {
            Ok(()) => Ok(true),
            Err(error) => {
                let id = milestone
                    .and_then(|i| self.state.mission.milestones.get(i))
                    .or_else(|| self.state.mission.milestones.last())
                    .ok_or_else(|| driver::invalid("gate failure without a milestone"))?
                    .id
                    .clone();
                self.emit(EventKind::MilestoneBlocked {
                    block_context: None,
                    milestone_id: id,
                    reason: crate::scrub::scrub_and_truncate(&error.to_string(), 8192),
                })?;
                Ok(false)
            }
        }
    }

    async fn external_completion_attempt(&mut self, milestone: Option<usize>) -> Result<()> {
        let (authority, events) = self.external_authority()?;
        let stage = if milestone.is_some() {
            Stage::MilestoneValidation
        } else {
            Stage::FinalGate
        };
        if !authority.applies(stage) {
            return Ok(());
        }
        self.close_external_gates("superseded by a fresh stage attempt")?;
        let base = match milestone {
            Some(i) => self.state.mission.milestones[i]
                .start_sha
                .as_deref()
                .ok_or_else(|| driver::invalid("milestone start commit is absent"))?,
            None => &authority.base,
        };
        let snapshot =
            SourceSnapshot::capture(self.active_repo(), base).map_err(driver::invalid)?;
        snapshot
            .verify_candidate(self.active_repo(), &self.state.mission.id)
            .map_err(driver::invalid)?;
        let root = self.active_root().to_path_buf();
        let env = self.contract_command_env(Some(&authority.base))?;
        let mut sandbox = self.gate_sandbox(&root)?;
        let environment = Digest::of(&serde_json::to_vec(&(
            std::env::consts::OS,
            std::env::consts::ARCH,
            &root,
            &crate::command_exec::worker_gate_sandbox(&self.state.config)?,
            &self.state.workspace_pin,
            &env,
        ))?);
        let content = Digest::of(&serde_json::to_vec(&snapshot.identity)?);
        let contract = self.state.mission.validation_contract.clone();
        let command_run = driver::id("checks");
        let mut observed = Vec::new();
        let mut required = Vec::new();
        let checks_result = async {
            for (i, assertion) in contract
                .iter()
                .enumerate()
                .filter(|(_, a)| a.check == AssertionCheck::Command)
            {
                let command = assertion
                    .command
                    .as_deref()
                    .ok_or_else(|| driver::invalid("command assertion has no command"))?;
                let check_id = Id::try_from(format!("contract-{i}")).map_err(driver::invalid)?;
                let (exit_code, output) =
                    crate::command_exec::run_shell_command_sandboxed_with_code(
                        &root,
                        command,
                        std::time::Duration::from_secs(300),
                        &env,
                        &sandbox,
                    )
                    .await;
                let event = self.emit(EventKind::OrchestratorDecision {
                    summary: format!("external gate check {}: exit {exit_code:?}", assertion.id),
                    detail: Some(crate::scrub::scrub_and_truncate(&output, 8192)),
                })?;
                observed.push(ObservedCheck {
                    check_id: check_id.clone(),
                    run_id: command_run.clone(),
                    sequence: event.seq,
                    checked_content: content.clone(),
                    environment: environment.clone(),
                    command: command.into(),
                    exit_code,
                    assertions_executed: None,
                    output_summary: Some(crate::scrub::scrub_and_truncate(&output, 8192)),
                });
                if milestone.is_none() {
                    required.push(RequiredCheck {
                        id: check_id,
                        command: command.into(),
                        require_assertions: false,
                    });
                }
            }
            // A final evaluator cannot cover a later source edit with an old
            // PTY pass. Run declared sessions against this same snapshot too.
            if milestone.is_none() {
                let pty = crate::pty_harness::run_pty_assertions(
                    &contract,
                    &root,
                    &env,
                    &sandbox,
                    &self.paths.runs_dir(),
                )
                .await;
                let milestone_id = self
                    .state
                    .mission
                    .milestones
                    .last()
                    .ok_or_else(|| driver::invalid("no final milestone"))?
                    .id
                    .clone();
                for artifact in &pty.artifacts {
                    self.emit(EventKind::ValidationPtyTranscript {
                        milestone_id: milestone_id.clone(),
                        assertion_id: artifact.assertion_id.clone(),
                        verdict: if artifact.pass {
                            crate::gate::GateVerdict::Pass
                        } else {
                            crate::gate::GateVerdict::Fail
                        },
                        artefact_ref: crate::gate_results::file_artefact_ref(
                            &artifact.transcript_rel,
                        ),
                        detail: Some(artifact.detail.clone()),
                    })?;
                }
                if !pty.skipped.is_empty()
                    || pty.artifacts.iter().any(|a| !a.pass)
                    || pty.artifacts.len()
                        != contract
                            .iter()
                            .filter(|a| a.check == AssertionCheck::PtyScript)
                            .count()
                {
                    return Err(driver::invalid(
                        "fresh final PTY evidence is missing or failed",
                    ));
                }
            }
            snapshot
                .verify_candidate(self.active_repo(), &self.state.mission.id)
                .map_err(driver::invalid)
        }
        .await;
        sandbox.cleanup()?;
        checks_result?;
        let features = if milestone.is_none() {
            authority::accepted_features(self.active_repo(), &self.state, &events)?
        } else {
            vec![]
        };
        let input_stage = match milestone {
            Some(i) => StageInput::Milestone {
                milestone_id: Id::try_from(self.state.mission.milestones[i].id.clone())
                    .map_err(driver::invalid)?,
                plan_index: i,
                snapshot: &snapshot,
            },
            None => StageInput::Deliverable {
                snapshot: &snapshot,
                features: &features,
            },
        };
        let paths = self.paths.clone();
        let prior: Vec<_> = self
            .state
            .gate_evaluations
            .values()
            .filter(|r| {
                r.consumed.is_some()
                    && r.requested.policy.kind == crate::pack::evaluator::Kind::Judgment
            })
            .cloned()
            .collect();
        let prior_refs: Vec<_> = prior.iter().collect();
        let diagnostics = crate::contract_controls::pair::diagnostics(
            &paths.mission_dir(),
            &events,
            self.active_repo(),
            &contract,
            &self.state.config,
        );
        let records = Box::pin(driver::evaluate_stage_async(
            driver::StageEvaluation {
                paths: &paths,
                source_log_range: Some(LogRange {
                    first: 1,
                    last: self.state.last_seq,
                }),
                registrations: &authority.registrations,
                plan_bytes: &authority.plan_bytes,
                mission_policy_digest: authority.policy.clone(),
                stage: input_stage,
                checks: Checks {
                    environment,
                    required: &required,
                    observed: &observed,
                },
                diagnostics: &diagnostics,
                prior_findings: &prior_refs,
                consent: None,
            },
            |kind| self.emit(kind),
        ))
        .await?;
        let current = self.external_authority()?.0;
        if let Err(error) = snapshot
            .verify_candidate(self.active_repo(), &self.state.mission.id)
            .map_err(driver::invalid)
            .and_then(|()| authority.verify(&current))
        {
            self.close_external_gates("stage inputs changed while checks ran")?;
            return Err(error);
        }
        let action = if milestone.is_some() {
            Action::AcceptMilestone
        } else {
            Action::AcceptDeliverable
        };
        for record in records {
            driver::consume(
                &record,
                record.requested.request.params.binding.clone(),
                action.clone(),
                |kind| self.emit(kind),
            )?;
        }
        Ok(())
    }

    pub(super) fn external_revision_checks(
        &mut self,
        plan: &Plan,
        revision: u32,
        diagnostics: &[crate::gate::GateReport],
    ) -> Result<()> {
        let (authority, _) = self.external_authority()?;
        if authority.registrations.is_empty() {
            return Ok(());
        }
        self.close_external_gates("superseded by explicit revised-plan approval")?;
        let branch = self.state.mission.mission_branch.clone();
        let tip = self.repo.rev_parse(&branch)?;
        let bytes = serde_json::to_vec(plan)?;
        let paths = self.paths.clone();
        let records = driver::evaluate_stage(
            driver::StageEvaluation {
                paths: &paths,
                source_log_range: Some(LogRange {
                    first: 1,
                    last: self.state.last_seq,
                }),
                registrations: &authority.registrations,
                plan_bytes: &bytes,
                mission_policy_digest: authority.policy.clone(),
                stage: StageInput::Plan {
                    revision: u64::from(revision) + 1,
                    base_commit: git_object(&authority.base).map_err(driver::invalid)?,
                },
                checks: Checks {
                    environment: Digest::of(b"revised-plan-approval"),
                    required: &[],
                    observed: &[],
                },
                diagnostics,
                prior_findings: &[],
                consent: Some(Consent {
                    actor: crate::live_permission::Actor::LocalRepositoryAuthority,
                    allow: true,
                    reference: format!(
                        "approve-revision:{revision}:{}",
                        Digest::of(&bytes).as_str()
                    ),
                }),
            },
            |kind| self.emit(kind),
        )?;
        let current = self.external_authority()?.0;
        if authority.verify(&current).is_err() || self.repo.rev_parse(&branch)? != tip {
            self.close_external_gates("revision authority or candidate changed while checks ran")?;
            return Err(driver::invalid(
                "revision authority or candidate changed while checks ran",
            ));
        }
        for record in records {
            driver::consume(
                &record,
                record.requested.request.params.binding.clone(),
                Action::ApprovePlan,
                |kind| self.emit(kind),
            )?;
        }
        Ok(())
    }

    pub(super) fn refuse_legacy_external_revision(&mut self) -> Result<()> {
        let (authority, _) = self.external_authority()?;
        if !authority.registrations.is_empty() {
            return Err(driver::invalid("external gates require the proposed-revision approval flow; legacy partial re-plan cannot replace the approved plan"));
        }
        Ok(())
    }

    pub(super) fn close_external_gates(&mut self, reason: &str) -> Result<()> {
        let ids: Vec<_> = self
            .state
            .gate_evaluations
            .values()
            .filter(|r| r.closed.is_none() && r.consumed.is_none())
            .map(|r| r.requested.request.params.attempt_id.clone())
            .collect();
        for attempt_id in ids {
            self.emit(EventKind::GateEvaluationClosed {
                attempt_id,
                reason: reason.into(),
            })?;
        }
        Ok(())
    }
    pub(super) fn external_plan_checks(
        &mut self,
        plan: &Plan,
        base: &str,
        diagnostics: &[crate::gate::GateReport],
        actor: crate::live_permission::Actor,
    ) -> Result<()> {
        self.close_external_gates("superseded by a fresh explicit approval attempt")?;
        let registrations =
            PinnedRegistration::configured_at_ref(&self.repo, &self.state.config, base)
                .map_err(driver::invalid)?;
        if registrations.is_empty() {
            return Ok(());
        }
        // This checkpoint must not admit a declaration for an unconnected
        // stage; otherwise a successful approval could silently skip checks.
        if registrations
            .iter()
            .any(|r| r.declaration().stages.contains(&Stage::CommandPermission))
        {
            return Err(driver::invalid("external command-permission evaluators are not connected; the live consent path remains authoritative"));
        }
        let bytes = serde_json::to_vec(plan)?;
        let policy_digest = Digest::of(&serde_json::to_vec(&self.state.config)?);
        let paths = self.paths.clone();
        let consent = Consent {
            actor,
            allow: true,
            reference: format!("approve-plan:{}", Digest::of(&bytes).as_str()),
        };
        let records = driver::evaluate_stage(
            driver::StageEvaluation {
                paths: &paths,
                source_log_range: Some(LogRange {
                    first: 1,
                    last: self.state.last_seq,
                }),
                registrations: &registrations,
                plan_bytes: &bytes,
                mission_policy_digest: policy_digest.clone(),
                stage: StageInput::Plan {
                    revision: 1,
                    base_commit: git_object(base).map_err(driver::invalid)?,
                },
                checks: Checks {
                    environment: Digest::of(
                        format!(
                            "{}:{}:approval",
                            std::env::consts::OS,
                            std::env::consts::ARCH
                        )
                        .as_bytes(),
                    ),
                    required: &[],
                    observed: &[],
                },
                diagnostics,
                prior_findings: &[],
                consent: Some(consent),
            },
            |kind| self.emit(kind),
        )?;
        // Consume only after the whole ladder has succeeded and every binding
        // is still current. A later gate failure consumes none of its peers.
        if self.repo.rev_parse(&self.state.mission.base_branch)? != base
            || Digest::of(&serde_json::to_vec(&self.state.config)?) != policy_digest
            || Digest::of(&serde_json::to_vec(plan)?) != Digest::of(&bytes)
        {
            self.close_external_gates("approval inputs changed while checks ran; retry against the current base and policy")?;
            return Err(driver::invalid("approval inputs changed while checks ran; retry against the current base and policy"));
        }
        let branch = &self.state.mission.mission_branch;
        if self.repo.branch_exists(branch)? && self.repo.rev_parse(branch)? != base {
            self.close_external_gates("mission branch changed while approval checks ran")?;
            return Err(driver::invalid(
                "mission branch changed while approval checks ran",
            ));
        }
        let current = PinnedRegistration::configured_at_ref(&self.repo, &self.state.config, base)
            .map_err(driver::invalid)?;
        if current.iter().map(|r| r.digest()).collect::<Vec<_>>()
            != registrations.iter().map(|r| r.digest()).collect::<Vec<_>>()
        {
            self.close_external_gates("approval checker registration drifted")?;
            return Err(driver::invalid("approval checker registration drifted"));
        }
        for record in records {
            let binding = record.requested.request.params.binding.clone();
            driver::consume(&record, binding, Action::ApprovePlan, |kind| {
                self.emit(kind)
            })?;
        }
        Ok(())
    }
}
