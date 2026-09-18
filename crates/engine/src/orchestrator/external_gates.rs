//! Real stage drivers for the external evaluation lifecycle.
use super::MissionEngine;
use crate::error::Result;
use crate::events::EventKind;
use crate::gate_evaluation::{driver, input_builder::*, lifecycle::*, protocol::*};
use crate::pack::evaluator::PinnedRegistration;
use crate::types::Plan;

impl MissionEngine {
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
        if registrations.iter().any(|r| {
            r.declaration()
                .stages
                .iter()
                .any(|stage| *stage != Stage::PlanApproval)
        }) {
            return Err(driver::invalid("mission integration currently supports plan-approval only; other configured stages are refused"));
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
