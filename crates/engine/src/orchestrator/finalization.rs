//! Final mission gates and approval-pinned completion eligibility.
//! Execution dispatch remains in the parent; persisted contracts are unchanged.

use super::*;

impl MissionEngine {
    pub(super) fn block_reviewer_independence(
        &mut self,
        milestone_id: &str,
        detail: String,
    ) -> Result<()> {
        let reason = format!(
            "reviewer independence blocked: {detail}; restore a compatible reviewer/worker \
             pairing or start a newly approved mission; the approved requirement cannot be waived"
        );
        self.emit_decision(&reason, None)?;
        self.emit(EventKind::MilestoneBlocked {
            milestone_id: milestone_id.to_string(),
            reason,
        })?;
        Ok(())
    }

    pub(super) fn check_completion_review(&mut self, milestone_id: Option<&str>) -> Result<bool> {
        if !self
            .state
            .mission
            .reviewer_independence
            .is_some_and(|policy| !policy.is_empty())
        {
            return Ok(true);
        }
        self.log.flush()?;
        let events = EventLog::read_events(self.log.events_path())?;
        match crate::reviewer_independence::check_completion(&self.state, &events, milestone_id) {
            Ok(()) => Ok(true),
            Err(blocked) => {
                self.block_reviewer_independence(&blocked.milestone_id, blocked.detail)?;
                Ok(false)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Final contract gate (h)
    // -----------------------------------------------------------------------

    /// All milestones complete: run every `command` assertion ourselves and
    /// put `agent-judgement` assertions to the orchestrator. Failures become
    /// findings on the last milestone. Command-assertion findings are
    /// **non-waivable** (a RED cargo test must not become COMPLETE by model
    /// discretion) but remain **fixable** through [`Self::convert_findings`] —
    /// the orchestrator may emit fix features or, if the fix-cycle cap is
    /// spent, the mission blocks. Agent-judgement / synthesized findings may
    /// still be waived.
    /// Returns `Some(status)` to end `run()`, `None` to continue the loop.
    pub(super) async fn final_gate(&mut self) -> Result<Option<MissionStatus>> {
        if self.state.mission.status != MissionStatus::Validating {
            self.emit(EventKind::MissionValidating {})?;
        }

        // Deterministic non-emptiness safety net (feature f-2-2): a mission
        // whose deliverable diff against the pinned base is empty (no
        // non-meta feature commits on the mission branch) must terminate
        // Failed, independent of and BEFORE any contract assertion — a green
        // contract can never override an empty deliverable. This runs
        // first, ahead of the command/agent-judgement assertions below.
        let base = match self.state.mission.base_sha.as_deref() {
            Some(sha) if !sha.is_empty() => sha.to_string(),
            _ => self.state.mission.base_branch.clone(),
        };
        // The meta exemption is path-verified, not subject-only: a worker
        // titling its commit "[kranz] mission report cleanup" while touching
        // real files must still count as a deliverable, or a forged subject
        // could hide worker writes from this gate (and desync it from the
        // path sweep, which applies the same check — see
        // contract_sweep::is_meta_commit_with_paths). Each commit is diffed
        // against its own FIRST parent (`commit_changed_paths`), never the
        // previous range entry — chaining interleaves merge parents and can
        // fail a genuine meta commit's path check, inflating the count.
        let commits = self.active_repo().commits_between(&base, "HEAD")?;
        let gate_mission_id = self.state.mission.id.clone();
        let mut non_meta_commit_count = 0usize;
        // The union of paths the mission diff touches — the `whenPaths`
        // scoping input for pack gates below (merge-gate idiom: a scoped
        // gate runs when at least one changed path sits under a prefix).
        let mut changed_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
        // The DELIVERABLE subset (worker-authored commits only — engine meta
        // commits like plan.json/report.md are excluded): the Flight Rules
        // envelope check's "actual changed paths" (D-E). Approval resolved
        // against the deliverable-describing touch set, so the comparison
        // basis must match — an enforced rule scoped at `.kranz/` must not
        // newly "apply" merely because the engine committed mission records.
        let mut deliverable_paths: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        for commit in &commits {
            let paths = commit_changed_paths(self.active_repo(), &commit.sha)?;
            if !contract_sweep::is_meta_commit_with_paths(&commit.subject, &gate_mission_id, &paths)
            {
                non_meta_commit_count += 1;
                deliverable_paths.extend(paths.iter().cloned());
            }
            changed_paths.extend(paths);
        }
        let mut all_changed_paths: Vec<String> = changed_paths.iter().cloned().collect();
        all_changed_paths.sort();
        let mut actual_paths: Vec<String> = deliverable_paths.iter().cloned().collect();
        actual_paths.sort();
        if non_meta_commit_count == 0 {
            // Same mission-end clear as complete_mission: no open question
            // may outlive the mission in the pending-decision projection.
            self.clear_open_questions("mission failed", |_| true)?;
            self.emit(EventKind::MissionFailed {
                reason: format!(
                    "no deliverable commits landed on the mission branch: \
                     {base}..HEAD contains 0 feature commits (only engine/meta \
                     commits). Refusing to COMPLETE on an empty deliverable diff."
                ),
            })?;
            return Ok(Some(MissionStatus::Failed));
        }

        // Also protect replay of a milestone completed by an older engine:
        // the status alone never proves the required reviewer actually ran.
        if !self.check_completion_review(None)? {
            return Ok(Some(MissionStatus::Blocked));
        }
        let reviewed_checkout = self
            .state
            .mission
            .reviewer_independence
            .filter(|policy| !policy.is_empty())
            .map(|_| validator_integrity::CheckoutFingerprint::capture(self.active_repo()))
            .transpose()?;
        if !self.check_final_review_checkout(reviewed_checkout.as_ref())? {
            return Ok(Some(MissionStatus::Blocked));
        }

        // Flight Rules envelope check (KRZ-342, design D-E): re-resolve the
        // approval-pinned source snapshot (the mission's pinned base sha —
        // immutable, so exactly the bytes approval read) against the ACTUAL
        // changed paths. A newly applicable ENFORCED rule means the mission
        // escaped its approved policy envelope (a touch-set grant widened
        // scope, or an out-of-contract write slipped the sweep): park for
        // revision/reapproval rather than judge against a moving set. The
        // declared-touch-set overlap makes approval's selection a superset of
        // anything an in-envelope diff can activate, so an in-envelope
        // mission can never false-positive here. Deterministic and cheap —
        // runs before any command/assertion spend below.
        if let Some(pin) = self.state.mission.standards_manifest.clone() {
            let pin_base = self
                .state
                .mission
                .base_sha
                .clone()
                .unwrap_or_else(|| self.state.mission.base_branch.clone());
            let envelope = crate::pack::resolution::newly_applicable_enforced(
                &self.repo,
                &pin_base,
                &pin,
                &actual_paths,
            );
            let park_reason = match envelope {
                Ok(newly) if newly.is_empty() => None,
                Ok(newly) => Some(format!(
                    "newly applicable enforced Flight Rules rule(s) outside the approved \
                     manifest pin: {} — the mission escaped its approved policy envelope; \
                     revise the plan and re-approve (D-E)",
                    newly
                        .iter()
                        .map(|rule| format!("{} r{}", rule.id, rule.revision))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
                // The approval snapshot itself is unreadable — fail closed:
                // never judge against a policy that cannot be read.
                Err(error) => Some(error),
            };
            if let Some(reason) = park_reason {
                let li = self.state.mission.milestones.len() - 1;
                let last_milestone_id = self.state.mission.milestones[li].id.clone();
                self.emit_decision(
                    "standards envelope escaped — parked for revision/reapproval",
                    Some(reason.clone()),
                )?;
                self.emit(EventKind::MilestoneBlocked {
                    milestone_id: last_milestone_id,
                    reason,
                })?;
                return Ok(None);
            }
        }

        let contract = self.state.mission.validation_contract.clone();
        let mut findings: Vec<Finding> = Vec::new();
        if let Some(review) = crate::review_artifact::parse_from_goal(&self.state.mission.goal)? {
            findings.extend(crate::review_artifact::deliverable_findings(
                self.active_repo(),
                &base,
                "HEAD",
                &actual_paths,
                &review,
            )?);
        }
        // agent-env-clear: command assertions run with a CLEARED environment
        // (minimal allowlist + scratch HOME + toolchain caches + any
        // contractEnvPassthrough names) — ambient secrets never reach them.
        let gate_base_sha = self.state.mission.base_sha.clone();
        let env = self.contract_command_env(gate_base_sha.as_deref())?;
        // engine-gates-sandbox-wrapped: the same commands then run under the
        // resolved worker sandbox profile (a no-op Disabled wrap when
        // enforce == off). Resolved ONCE for the whole final gate — every
        // assertion below shares the gate tree + contract scratch shape.
        let gate_root = self.active_root().to_path_buf();
        let gate_sandbox = self.gate_sandbox(&gate_root)?;

        // command assertions — engine-run (design.md: the hard gate).
        for assertion in contract
            .iter()
            .filter(|a| a.check == AssertionCheck::Command)
        {
            let Some(command) = assertion.command.as_deref() else {
                findings.push(Finding {
                    subject: assertion.id.clone(),
                    severity: "critical".to_string(),
                    evidence: "assertion has check=command but no command".to_string(),
                    suggested_fix: String::new(),
                    class: String::new(),
                    rule: None,
                });
                continue;
            };
            let (ok, output) =
                run_shell_command_sandboxed(&gate_root, command, &env, &gate_sandbox).await;
            if !ok {
                findings.push(Finding {
                    subject: assertion.id.clone(),
                    severity: "critical".to_string(),
                    evidence: scrub::scrub(&format!("command failed: {command}\n{output}")),
                    suggested_fix: String::new(),
                    class: "command-assertion".to_string(),
                    rule: None,
                });
            }
        }

        // Named contract gates at the final-gate surface (ticket
        // contract-validation-gates.md): the static, deterministic classes
        // (vacuous-filter, wrong-polarity, env-sensitive) re-checked against
        // the ACTIVE tree — e.g. a negated grep whose target is still absent
        // here passed vacuously, and its green contributes nothing. The
        // passes-on-base gate is absent: the work has landed, so passing on
        // base is now expected. Advisory only, same posture as at approval:
        // the command outcomes and findings above are unchanged — this
        // records the named verdicts so a vacuously-green contract is
        // visible in the event log instead of silently trusted.
        //
        // ONE shared pipeline (pack gates + KRZ-346 Flight Rules): engine
        // floors register first, then prepared deterministic pack/checker
        // outcomes, then contextual/manual outcomes in the model-judged
        // section. Commands/model turns finish before registration because
        // Gate::evaluate is synchronous and the pipeline is not Send.
        //
        // A standards mission consumes approval-pinned gate declarations —
        // never `load_for_config` from the mission worktree. Schema-2/3 packs
        // (no standards pin) retain their legacy live advisory gate path.
        let standards_pin = self.state.mission.standards_manifest.clone();
        let gate_applicability_paths = standards_pin
            .as_ref()
            .map(|pin| crate::pack::resolution::evaluation_paths(pin, &all_changed_paths))
            .unwrap_or_else(|| all_changed_paths.clone());
        let mut standards_rules = standards_pin
            .as_ref()
            .map(|pin| {
                crate::standards_enforcement::applicable_rules(
                    pin,
                    &[
                        crate::pack::standards::RuleStage::Validation,
                        crate::pack::standards::RuleStage::Merge,
                    ],
                    &actual_paths,
                )
            })
            .unwrap_or_default();
        standards_rules.sort_by(|left, right| left.id.cmp(&right.id));

        let mut gate_rule_ids: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        let mut contextual_rules = Vec::new();
        let mut prepared_standard_reports = Vec::new();
        let enforcement_events = if standards_pin.is_some() {
            self.log.flush()?;
            EventLog::read_events(self.log.events_path())?
        } else {
            Vec::new()
        };
        if let Some(pin) = standards_pin.as_ref() {
            for rule in &standards_rules {
                match crate::standards_enforcement::checker_binding(pin, rule, &actual_paths) {
                    crate::standards_enforcement::CheckerBinding::Gate(gate) => {
                        gate_rule_ids
                            .entry(gate.id.clone())
                            .or_default()
                            .push(rule.id.clone());
                    }
                    crate::standards_enforcement::CheckerBinding::AgentJudgement => {
                        contextual_rules.push(rule.clone());
                    }
                    crate::standards_enforcement::CheckerBinding::ManualAttestation => {
                        let attestation = crate::standards_attestation::active_attestation(
                            self.active_repo(),
                            &enforcement_events,
                            &self.state.mission.id,
                            pin,
                            rule,
                            &base,
                            "HEAD",
                        )?;
                        let artefact = crate::gate::ArtefactRef::new(format!(
                            "manual attestation for {} r{}",
                            rule.id, rule.revision
                        ));
                        let outcome = if let Some(attestation) = attestation {
                            crate::gate::GateOutcome::pass(artefact.with_detail(format!(
                                "standards.attestation.approved seq {} by {} via {}: {}",
                                attestation.seq,
                                attestation.approver,
                                attestation.surface,
                                attestation.reason
                            )))
                        } else {
                            crate::gate::GateOutcome::fail(artefact.with_detail(
                                "no current authorized manual attestation is recorded for \
                                 this exact pinned rule and diff",
                            ))
                        };
                        prepared_standard_reports.push(crate::gate::GateReport {
                            name: format!("standards-manual:{}", rule.id),
                            kind: crate::gate::GateKind::ModelJudged,
                            outcome: outcome.with_rule_ids(vec![rule.id.clone()]),
                        });
                    }
                    crate::standards_enforcement::CheckerBinding::Unavailable(reason) => {
                        prepared_standard_reports.push(crate::gate::GateReport {
                            name: format!("standards-binding:{}", rule.id),
                            kind: crate::gate::GateKind::Deterministic,
                            outcome: crate::gate::GateOutcome::fail(
                                crate::gate::ArtefactRef::new(format!(
                                    "checker binding for {} r{}",
                                    rule.id, rule.revision
                                ))
                                .with_detail(reason),
                            )
                            .with_rule_ids(vec![rule.id.clone()]),
                        });
                    }
                }
            }
        }

        let (pack_name, pinned_gate_decls) = if let Some(pin) = standards_pin.as_ref() {
            (Some(pin.pack_name.clone()), pin.gates.clone())
        } else {
            let pack = crate::pack::load_for_config(&self.state.config, &self.paths.repo_root)
                .map_err(EngineError::Config)?;
            let name = pack.as_ref().map(|pack| pack.name.clone());
            let gates = pack
                .as_ref()
                .map(|pack| {
                    pack.gates
                        .iter()
                        .map(|gate| crate::types::PinnedGate {
                            id: gate.name.clone(),
                            command: gate.command.clone(),
                            when_paths: gate.when_paths.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            (name, gates)
        };
        let mut pack_gates: Vec<crate::pack::PackGate> = Vec::new();
        for decl in &pinned_gate_decls {
            let linked = gate_rule_ids.contains_key(&decl.id);
            let applicability_paths = if linked {
                &gate_applicability_paths
            } else {
                &all_changed_paths
            };
            if !crate::merge_gate::when_paths_match(&decl.when_paths, applicability_paths) {
                continue;
            }
            let (ok, output) =
                run_shell_command_sandboxed(&gate_root, &decl.command, &env, &gate_sandbox).await;
            let rule_ids = gate_rule_ids.remove(&decl.id).unwrap_or_default();
            pack_gates.push(
                crate::pack::PackGate::from_run(&decl.id, &decl.command, ok, output)
                    .with_rule_ids(rule_ids),
            );
        }
        // A valid pin cannot leave a gate id unresolved (checker_binding
        // resolved against this same list). Treat any corrupt duplicate or
        // hand-edited pin conservatively anyway.
        for (gate, rule_ids) in gate_rule_ids {
            prepared_standard_reports.push(crate::gate::GateReport {
                name: format!("standards-binding:{gate}"),
                kind: crate::gate::GateKind::Deterministic,
                outcome: crate::gate::GateOutcome::fail(
                    crate::gate::ArtefactRef::new(format!("approval-pinned gate `{gate}`"))
                        .with_detail("the pinned checker declaration was unavailable at execution"),
                )
                .with_rule_ids(rule_ids),
            });
        }
        prepared_standard_reports.extend(self.judge_standards_rules(&contextual_rules).await?);

        // The pipeline is scoped to this block: it is not Send (Box<dyn
        // Gate>), so it must be fully dropped before the next await below.
        let (floor_reports, pack_reports) = {
            let mut pipeline = crate::gate::GatePipeline::new();
            contract_gates::register_contract_gates(
                &mut pipeline,
                &contract,
                None,
                self.active_root(),
            );
            let floor_gate_count = pipeline.len();
            for gate in pack_gates {
                pipeline.register(Box::new(gate));
            }
            for report in prepared_standard_reports {
                pipeline.register(Box::new(crate::standards_enforcement::PreparedGate::new(
                    report,
                )));
            }
            let final_gate_reports = pipeline.evaluate();
            let (floor, pack) = final_gate_reports.split_at(floor_gate_count);
            (floor.to_vec(), pack.to_vec())
        };
        // First-class gate results (ticket gate-results-first-class-events,
        // KRZ-312): one gate.result event per evaluation, recorded for the
        // WHOLE ladder — floor then pack, concatenated back into pipeline
        // (registration) order so the per-section indices the helper assigns
        // run continuously across both. Unconditional, pass or fail: a
        // gate's silent green is exactly as invisible as its failure, and
        // replay reconstructs the ladder from these events alone. Record-
        // only; the advisory posture of both floors is unchanged.
        let ladder: Vec<crate::gate::GateReport> = floor_reports
            .iter()
            .chain(pack_reports.iter())
            .cloned()
            .collect();
        for kind in gate_results::gate_result_events(crate::gate::GateSurface::FinalGate, &ladder) {
            self.emit(kind)?;
        }
        let failed_gates = contract_gates::failed_gate_names(&floor_reports);
        if !failed_gates.is_empty() {
            self.emit_decision(
                &format!(
                    "contract gates (final gate): named gate(s) failed: {} — advisory only; \
                     command outcomes and findings above are unchanged",
                    failed_gates.join(", ")
                ),
                Some(contract_gates::render_gate_verdicts(&floor_reports)),
            )?;
        }
        // Ordinary (unlinked) pack gates remain advisory. Linked Flight
        // Rules reports are interpreted through D-B below.
        let ordinary_pack_reports: Vec<_> = pack_reports
            .iter()
            .filter(|report| report.outcome.rule_ids.is_empty())
            .cloned()
            .collect();
        if let Some(pack_name) = &pack_name {
            if !ordinary_pack_reports.is_empty() {
                let failed_pack = contract_gates::failed_gate_names(&ordinary_pack_reports);
                let summary = if failed_pack.is_empty() {
                    format!(
                        "pack `{}` gates (final gate): {} deterministic gate(s) passed — advisory only",
                        pack_name,
                        ordinary_pack_reports.len()
                    )
                } else {
                    format!(
                        "pack `{}` gates (final gate): named gate(s) failed: {} — advisory only; \
                         command outcomes and findings above are unchanged",
                        pack_name,
                        failed_pack.join(", ")
                    )
                };
                self.emit_decision(
                    &summary,
                    Some(contract_gates::render_verdict_block(
                        &format!("pack `{pack_name}` gates:"),
                        &ordinary_pack_reports,
                    )),
                )?;
            }
        }

        // Interpret linked checker failures through the exact lifecycle ×
        // level matrix. Advisory failures are recorded as findings but never
        // enter the blocking/fix loop. Enforced MUST failures enter it unless
        // a still-live D-I waiver matches this exact finding AND the current
        // affected-path diff.
        let linked_reports: Vec<_> = pack_reports
            .iter()
            .filter(|report| !report.outcome.rule_ids.is_empty())
            .cloned()
            .collect();
        let mut advisory_standard_findings = Vec::new();
        let mut waived_standard_rules = Vec::new();
        if let Some(pin) = standards_pin.as_ref() {
            for report in &linked_reports {
                if report.outcome.passed() {
                    continue;
                }
                for rule_id in &report.outcome.rule_ids {
                    let Some(rule) = standards_rules.iter().find(|rule| &rule.id == rule_id) else {
                        continue;
                    };
                    let detail = report
                        .outcome
                        .artefact
                        .detail
                        .as_deref()
                        .unwrap_or("checker failed without detail");
                    let finding = crate::standards_enforcement::failure_finding(
                        pin,
                        rule,
                        &scrub::scrub(&format!(
                            "checker `{}` failed for {} r{}: {detail}",
                            report.name, rule.id, rule.revision
                        )),
                    );
                    match crate::standards_enforcement::rule_mode(rule) {
                        crate::standards_enforcement::RuleMode::Absent => {}
                        crate::standards_enforcement::RuleMode::Advisory => {
                            advisory_standard_findings.push(finding)
                        }
                        crate::standards_enforcement::RuleMode::Authoritative => {
                            let waiver = crate::standards_waiver::active_waiver_for_finding(
                                self.active_repo(),
                                &enforcement_events,
                                &self.state.mission.id,
                                pin,
                                rule,
                                &finding,
                                &base,
                                "HEAD",
                                chrono::Utc::now(),
                            )?;
                            if let Some(waiver) = waiver {
                                waived_standard_rules.push((rule.id.clone(), waiver.seq));
                            } else {
                                findings.push(finding);
                            }
                        }
                    }
                }
            }
        }
        for (rule_id, waiver_seq) in waived_standard_rules {
            self.emit_decision(
                &format!(
                    "Flight Rules {rule_id}: failing enforced MUST covered by exact human waiver"
                ),
                Some(format!(
                    "standards.waiver.approved seq {waiver_seq} matches the pinned revision, \
                     checker finding fingerprint, affected paths, current diff digest, approval \
                     sequence, human authority surface, and expiry"
                )),
            )?;
        }
        if !linked_reports.is_empty() {
            self.emit_decision(
                &format!(
                    "Flight Rules final enforcement: {} checker verdict(s), {} blocking failure(s), {} advisory failure(s)",
                    linked_reports.len(),
                    findings
                        .iter()
                        .filter(|finding| finding.class == "standards-authoritative")
                        .count(),
                    advisory_standard_findings.len()
                ),
                Some(contract_gates::render_verdict_block(
                    "Flight Rules checkers:",
                    &linked_reports,
                )),
            )?;
        }
        if !advisory_standard_findings.is_empty() {
            let milestone_id = self
                .state
                .mission
                .milestones
                .last()
                .expect("a final gate has a milestone")
                .id
                .clone();
            for finding in advisory_standard_findings {
                self.emit(EventKind::ValidationFinding {
                    milestone_id: milestone_id.clone(),
                    run_id: crate::reducer::ENGINE_RUN_ID.to_string(),
                    finding,
                })?;
            }
        }

        // Pty-script assertions (ticket pty-functional-validation) are NOT
        // re-run at the final gate: their verdicts are validation-round
        // evidence (the functional validator judges them there, and every
        // milestone — the last included — passes through a round before the
        // gate). Surface the posture LOUDLY rather than letting the hard
        // gate's silence read as a re-check.
        let pty_assertion_ids: Vec<&str> = contract
            .iter()
            .filter(|a| a.check == AssertionCheck::PtyScript)
            .map(|a| a.id.as_str())
            .collect();
        if !pty_assertion_ids.is_empty() {
            self.emit_decision(
                "pty-script assertions not re-run at the final gate",
                Some(format!(
                    "assertion(s) {} are terminal-interactive validations executed at each \
                     milestone's validation round (transcripts referenced from \
                     validation.pty.transcript events); the final gate re-runs only command \
                     assertions, so their last round verdict stands",
                    pty_assertion_ids.join(", ")
                )),
            )?;
            // Vacuous-green backstop (ticket pty-script-skip-vacuous-green):
            // "their last round verdict stands" is only honest when a
            // verdict EXISTS. A declared pty-script whose session SKIPPED
            // every round (this host cannot drive a pty) has no
            // validation.pty.transcript event in the log — the declared
            // functional validation never executed, so the gate must not
            // green on that silence. The finding carries the
            // command-assertion class: non-waivable, escalatable to the
            // operator as author-broken, exactly like a failed command
            // assertion.
            self.log.flush()?;
            let events = EventLog::read_events(self.log.events_path())?;
            for assertion in unexecuted_pty_assertions(&contract, &events) {
                findings.push(Finding {
                    subject: assertion.id.clone(),
                    severity: "critical".to_string(),
                    evidence: format!(
                        "declared pty-script assertion `{}` has no validation.pty.transcript \
                         verdict in the mission log: it never executed in any validation \
                         round (this host cannot drive a pty session — the round's evidence \
                         block carries the SKIP as a FAIL line naming the reason — or its \
                         transcript artifact could not be written), so the declared \
                         functional validation never ran",
                        assertion.id
                    ),
                    suggested_fix: "run the mission on a unix host that can drive pty \
                        sessions, or drop the pty-script assertion from the validation \
                        contract"
                        .to_string(),
                    class: "command-assertion".to_string(),
                    rule: None,
                });
            }
        }

        // agent-judgement assertions — one orchestrator verdicts turn.
        let judgement: Vec<&Assertion> = contract
            .iter()
            .filter(|a| a.check == AssertionCheck::AgentJudgement)
            .collect();
        if !judgement.is_empty() {
            findings.extend(self.judge_contract_assertions(&judgement).await?);
        }

        if !self.check_final_review_checkout(reviewed_checkout.as_ref())? {
            return Ok(Some(MissionStatus::Blocked));
        }

        if findings.is_empty() {
            return self
                .complete_mission(reviewed_checkout.as_ref())
                .await
                .map(Some);
        }

        // Surface gate findings on the event feed (dashboard visibility),
        // attributed to the reserved engine run id since no validator session
        // exists behind them.
        let li = self.state.mission.milestones.len() - 1;
        let last_milestone_id = self.state.mission.milestones[li].id.clone();
        for finding in &findings {
            self.emit(EventKind::ValidationFinding {
                milestone_id: last_milestone_id.clone(),
                run_id: crate::reducer::ENGINE_RUN_ID.to_string(),
                finding: finding.clone(),
            })?;
        }

        // A missing manual attestation is intentionally human-as-MUST. It
        // is neither a code defect for a worker to chase nor a judgement a
        // model may waive. Park immediately with the one authorized command;
        // resuming after that event is recorded re-runs the exact checker.
        let manual_attestation_rules: Vec<String> = findings
            .iter()
            .filter_map(|finding| {
                finding
                    .rule
                    .as_ref()
                    .filter(|rule| rule.checker.as_deref() == Some("manual-attestation"))
                    .map(|rule| rule.id.clone())
            })
            .collect();
        if !manual_attestation_rules.is_empty() {
            let rules = manual_attestation_rules.join(", ");
            self.emit_decision(
                &format!("Flight Rules manual attestation required: {rules}"),
                Some(format!(
                    "Stop the mission runner, inspect the current diff, then record each positive human verdict with `kranz --mission {} standards attest --rule <id> --reason <reason>` and resume. Any relevant diff change invalidates the attestation.",
                    self.state.mission.id
                )),
            )?;
            self.emit(EventKind::MilestoneBlocked {
                milestone_id: last_milestone_id,
                reason: format!(
                    "authorized manual attestation required for Flight Rules rule(s): {rules}"
                ),
            })?;
            return Ok(None);
        }

        // Command assertions and failing enforced Flight Rules MUSTs are not
        // waivable by model discretion. The latter have exactly one exception
        // channel: a pre-recorded, live standards.waiver.approved event,
        // consumed above. Refuse any ordinary conversion-turn waive that
        // names either class and synthesize fixes instead.
        let protected_subjects: std::collections::HashSet<String> = findings
            .iter()
            .filter(|f| f.class == "command-assertion" || f.class == "standards-authoritative")
            .map(|f| f.subject.clone())
            .collect();
        let all_findings = findings;
        match self
            .convert_findings(&last_milestone_id, &all_findings)
            .await?
        {
            FindingsConversion::Escalate { escalations, text } => {
                let subjects = escalations
                    .iter()
                    .map(|e| e.subject.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let detail = escalations
                    .iter()
                    .map(|e| {
                        let evidence = all_findings
                            .iter()
                            .find(|f| f.subject == e.subject)
                            .map(|f| f.evidence.as_str())
                            .unwrap_or("");
                        format!("- {}: {}\n  evidence: {}", e.subject, e.diagnosis, evidence)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                self.emit_decision(
                    &format!(
                        "final-gate command assertion(s) {subjects} appear author-broken \
                         (false negative); escalating to the operator with evidence attached"
                    ),
                    Some(format!("{detail}\n\n{text}")),
                )?;
                self.emit(EventKind::MilestoneBlocked {
                    milestone_id: last_milestone_id,
                    reason: format!(
                        "contract command assertion(s) {subjects} appear buggy (false \
                         negative) — command still fails but the requirement is verified met; \
                         evidence attached. Escalating to operator (gate-repair is a human \
                         decision, not a fix cycle)."
                    ),
                })?;
                Ok(None)
            }
            FindingsConversion::Waive { waived }
                if waived
                    .iter()
                    .all(|w| !protected_subjects.contains(w.subject.as_str())) =>
            {
                self.emit_waive_decision(&waived)?;
                // Report AFTER the waive decision (so the gate waiver is in
                // the replayed history) and BEFORE mission.completed.
                self.complete_mission(reviewed_checkout.as_ref())
                    .await
                    .map(Some)
            }
            FindingsConversion::Waive { waived } => {
                // Model waived a protected final-gate finding — refuse. Fix
                // every protected finding the waive covered (and any
                // other unwaived remainder is already handled by convert
                // synthesizing; here the waive emptied the set, so rebuild
                // from command findings only).
                let refuse_note = waived
                    .iter()
                    .filter(|w| protected_subjects.contains(w.subject.as_str()))
                    .map(|w| w.subject.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.emit_decision(
                    &format!(
                        "refused model waive of non-waivable final-gate finding(s): {refuse_note}; synthesizing fix feature(s)"
                    ),
                    Some(
                        waived
                            .iter()
                            .map(|w| format!("- {}: {}", w.subject, w.reason))
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                )?;
                let protected_only: Vec<&Finding> = all_findings
                    .iter()
                    .filter(|f| protected_subjects.contains(&f.subject))
                    .collect();
                let specs = synthesize_fix_specs(protected_only);
                if self.fix_cycle_exhausted(li) && !self.escalate_or_block(&last_milestone_id)? {
                    self.emit(EventKind::MilestoneBlocked {
                        milestone_id: last_milestone_id,
                        reason: format!(
                            "{} non-waivable final-gate finding(s) failed but the fix-cycle cap ({}) is reached",
                            specs.len(),
                            self.state.config.max_fix_cycles_per_milestone
                        ),
                    })?;
                    return Ok(None);
                }
                self.emit(EventKind::MilestoneValidating {
                    milestone_id: last_milestone_id,
                })?;
                self.emit_fix_features(
                    li,
                    specs,
                    &format!("fix non-waivable final-gate finding(s): {refuse_note}"),
                    refuse_note,
                )?;
                Ok(None)
            }
            FindingsConversion::Fix {
                specs,
                summary,
                text,
            } => {
                if self.fix_cycle_exhausted(li) && !self.escalate_or_block(&last_milestone_id)? {
                    self.emit_decision(
                        &format!(
                            "fix-cycle cap reached; {} fix feature(s) wanted for {last_milestone_id}: {summary}",
                            specs.len()
                        ),
                        Some(text),
                    )?;
                    self.emit(EventKind::MilestoneBlocked {
                        milestone_id: last_milestone_id,
                        reason: format!(
                            "{} final-gate finding(s) but the fix-cycle cap ({}) is reached",
                            all_findings.len(),
                            self.state.config.max_fix_cycles_per_milestone
                        ),
                    })?;
                    return Ok(None); // loop → blocked branch → Blocked
                }
                // Reopen the last milestone: milestone.validating makes the
                // following fixfeature.created bump fix_cycles and flip it
                // back to Active (reducer semantics) so the main loop picks
                // the fix features up.
                self.emit(EventKind::MilestoneValidating {
                    milestone_id: last_milestone_id,
                })?;
                self.emit_fix_features(li, specs, &summary, text)?;
                Ok(None)
            }
        }
    }

    // -----------------------------------------------------------------------
    // Completion report (roadmap M1)
    // -----------------------------------------------------------------------

    /// Require final gates and lesson preparation to preserve the reviewed
    /// checkpoint. Any drift starts a fresh, durable validation epoch.
    fn check_final_review_checkout(
        &mut self,
        before: Option<&validator_integrity::CheckoutFingerprint>,
    ) -> Result<bool> {
        let Some(before) = before else {
            return Ok(true);
        };
        // Worker checkpoints commit their retained work. A clean baseline is
        // necessary: status lines cannot distinguish two edits to the same
        // already-dirty path, and uncommitted work has no commit identity.
        let detail = if !before.status.trim().is_empty() {
            "the reviewed checkout has uncommitted changes; checkpoint them before review".into()
        } else {
            match validator_integrity::CheckoutFingerprint::capture(self.active_repo()) {
                Ok(after) => match before.drift(&after) {
                    Some(drift) => drift.summary(),
                    None => match self.active_repo().is_clean_tracked_strict() {
                        Ok(true) => return Ok(true),
                        Ok(false) => "index flags hide the reviewed checkout's contents".into(),
                        Err(error) => format!("cannot verify reviewed index: {error}"),
                    },
                },
                Err(error) => format!("cannot verify checkout identity: {error}"),
            }
        };
        let milestone_id = self
            .state
            .mission
            .milestones
            .last()
            .expect("final validation has a milestone")
            .id
            .clone();
        // Start a new review epoch before parking. This survives resume and
        // prevents a subsequent skip or final-gate retry from borrowing the
        // pre-mutation reviewer runs, even after the offending gate is fixed.
        self.emit(EventKind::MilestoneValidating {
            milestone_id: milestone_id.clone(),
        })?;
        self.block_reviewer_independence(
            &milestone_id,
            format!(
                "final validation changed the reviewed checkout ({detail}); rerun required review"
            ),
        )?;
        Ok(false)
    }

    /// Complete the mission: capture at most one cross-mission lesson, note
    /// whether one was captured (or NONE) on the event feed, then write and
    /// commit the completion report — with the lesson files (if any) folded
    /// into the SAME report commit — and finally emit `mission.completed`.
    ///
    /// Both callers (findings-empty and all-waived at the final gate) must
    /// go through this single path so the capture turn runs exactly once,
    /// only at completion. After checking mandatory review evidence, artifact
    /// capture is best-effort: a capture or commit failure must never prevent
    /// completion of a mission that already passed its final gate.
    pub(super) async fn complete_mission(
        &mut self,
        reviewed_checkout: Option<&validator_integrity::CheckoutFingerprint>,
    ) -> Result<MissionStatus> {
        if !self.check_completion_review(None)?
            || !self.check_final_review_checkout(reviewed_checkout)?
        {
            return Ok(MissionStatus::Blocked);
        }
        let lesson = self.prepare_lesson().await;
        if !self.check_final_review_checkout(reviewed_checkout)? {
            return Ok(MissionStatus::Blocked);
        }
        let lesson_paths = lesson
            .and_then(|body| {
                body.map(|body| self.write_prepared_lesson(&body))
                    .transpose()
            })
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "lesson capture failed; completing without a lesson");
                None
            });
        match &lesson_paths {
            Some(paths) => self.emit_decision(
                &format!("cross-mission lesson captured ({} file(s))", paths.len()),
                None,
            )?,
            None => self.emit_decision("no cross-mission lesson captured", None)?,
        }
        self.write_mission_report(lesson_paths);
        // Structured human questions (ticket
        // structured-human-question-events): questions do not park the run
        // loop, so an ask can still be open here — a completed mission makes
        // every one moot. Clear them so the projection never shows a
        // "your move" on a mission that can no longer act on it.
        self.clear_open_questions("mission completed", |_| true)?;
        self.emit(EventKind::MissionCompleted {})?;
        Ok(MissionStatus::Complete)
    }
}
