//! Markdown renderers for the mission artifacts — `plan.md`, `research.md`,
//! `revised-plan.md`, and `report.md` — extracted from `orchestrator.rs` in
//! the monolith split (pure code motion, no behavior change). Everything here
//! is pure and deterministic given its inputs (no wall-clock reads): the
//! orchestrator computes estimates and timestamps and passes them in.

use crate::contract_lint;
use crate::cost;
use crate::events::{Event, EventKind};
use crate::runner;
use crate::scrub;
use crate::types::*;
use std::collections::HashMap;
use std::path::Path;

/// Render the approved plan as human-readable markdown — committed to the
/// mission branch beside plan.json for later review and reference. Pure and
/// deterministic given its inputs (no wall-clock reads; the estimate is
/// computed by the caller and passed in).
pub fn render_plan_markdown(
    plan: &Plan,
    mission: &Mission,
    estimate: &cost::CostEstimate,
    two_path: Option<&cost::TwoPathEstimate>,
    missions_used: usize,
    contract_lint: &contract_lint::ContractLintReport,
) -> String {
    use std::fmt::Write as _;
    let mut md = String::new();
    let _ = writeln!(md, "# Mission plan — {}", mission.id);
    let _ = writeln!(md, "\n**Goal:** {}\n", plan.goal);
    let _ = writeln!(
        md,
        "Branch `{}` (from `{}`). Approved plan of record; the machine-readable \
         twin is [plan.json](plan.json). Live status: `kranz status` or the dashboard.\n",
        mission.mission_branch, mission.base_branch
    );

    let _ = writeln!(md, "## Cost estimate\n");
    let provenance = if missions_used == 0 {
        "built-in defaults — no completed missions yet".to_string()
    } else {
        format!("based on {missions_used} completed mission(s)")
    };
    match estimate.confidence {
        cost::Confidence::High => {
            let _ = writeln!(
                md,
                "Estimated **${:.2} – ${:.2}** (expected ~${:.2}). Rough estimate — live usage is \
                 authoritative; {provenance}.\n",
                estimate.low_usd, estimate.high_usd, estimate.expected_usd
            );
        }
        cost::Confidence::Low => {
            let _ = writeln!(
                md,
                "Estimated **${:.2} – ${:.2}** (expected ~${:.2}). Doc-heavy / judgement-heavy \
                 shape — the calibration corpus lacks a comparable mission, so this is **LOW \
                 CONFIDENCE** and ${:.2} is a soft ceiling, not a tight bound; {provenance}.\n",
                estimate.low_usd, estimate.high_usd, estimate.expected_usd, estimate.high_usd
            );
        }
    }

    // Local-tier routing shows both paths (cache-miss-tier-switch-pricing):
    // a single number is wrong in both directions when a mission can
    // complete at $0 marginal or escalate to frontier.
    if let Some(two_path) = two_path {
        let _ = writeln!(
            md,
            "This plan routes to the **local tier**: **$0 marginal** (fixed hardware + \
             electricity, not per-token) if it completes locally. If it escalates to frontier: \
             **${:.2} – ${:.2}** (expected ~${:.2}), which prices the tier switch's cache-miss \
             once (${:.2} — the first post-escalation turn re-reads the full prefix uncached; \
             escalation happens at feature/milestone edges, where no warm cache exists to lose).\n",
            two_path.escalated.low_usd,
            two_path.escalated.high_usd,
            two_path.escalated.expected_usd,
            two_path.cache_miss_usd,
        );
    }

    if let Some(alternatives) = &plan.considered_alternatives {
        let _ = writeln!(md, "## Considered alternatives\n");
        let _ = writeln!(md, "**Chosen approach:** {}\n", alternatives.chosen.trim());
        if !alternatives.rejected.is_empty() {
            let _ = writeln!(md, "Rejected shapes:");
            for rejected in &alternatives.rejected {
                let _ = writeln!(
                    md,
                    "- **{}** — {}",
                    rejected.approach.trim(),
                    rejected.trade_off.trim()
                );
            }
            let _ = writeln!(md);
        }
    }

    let _ = writeln!(md, "## Validation contract\n");
    let _ = writeln!(
        md,
        "Defined before any feature; gates mission completion.\n"
    );
    for a in &plan.validation_contract {
        match (&a.check, &a.command) {
            (AssertionCheck::Command, Some(cmd)) => {
                let _ = writeln!(
                    md,
                    "- **[{}]** {}\n  `{}`",
                    a.id.trim(),
                    a.statement.trim(),
                    cmd.trim()
                );
            }
            _ => {
                let _ = writeln!(
                    md,
                    "- **[{}]** {} *(agent judgement)*",
                    a.id.trim(),
                    a.statement.trim()
                );
            }
        }
    }

    if !contract_lint.is_empty() {
        let _ = writeln!(md, "## Contract lint\n");
        let _ = writeln!(
            md,
            "Each `check: command` assertion above was run once against the untouched base \
             tree at approval time. Suspects are assertions that already pass (or could not \
             reach a verdict) before this plan's work lands — a possible polarity/vacuity bug \
             in the assertion itself. This never blocks approval.\n"
        );
        let _ = writeln!(md, "{}\n", contract_lint.summary());
    }

    for (mi, m) in plan.milestones.iter().enumerate() {
        let _ = writeln!(md, "\n## Milestone {} — {}\n", mi + 1, m.title);
        for (fi, f) in m.features.iter().enumerate() {
            let _ = writeln!(md, "### {}.{} {}\n", mi + 1, fi + 1, f.title);
            let _ = writeln!(md, "{}\n", f.spec.trim());
            if !f.validation_criteria.is_empty() {
                let _ = writeln!(md, "Done when:");
                for c in &f.validation_criteria {
                    let _ = writeln!(md, "- {c}");
                }
                let _ = writeln!(md);
            }
        }
    }
    while md.ends_with('\n') {
        md.pop();
    }
    md.push('\n');
    md
}

/// The `research` evidence object the orchestrator may emit alongside the plan
/// JSON (repo-knowledge-store slice 1). The [`Plan`] struct ignores it (no
/// `deny_unknown_fields`); [`extract_research`] pulls it from the raw JSON so it
/// can be rendered to `research.md` without adding a field to every `Plan`
/// literal. All fields optional so a partial object still parses.
#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Research {
    files_read: Vec<String>,
    sources: Vec<String>,
    facts: Vec<ResearchFact>,
    ambiguities: Vec<String>,
    candidate_knowledge_updates: Vec<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ResearchFact {
    fact: String,
    evidence: String,
}

impl Research {
    fn is_empty(&self) -> bool {
        self.files_read.is_empty()
            && self.sources.is_empty()
            && self.facts.is_empty()
            && self.ambiguities.is_empty()
            && self.candidate_knowledge_updates.is_empty()
    }
}

/// Pull the optional `research` object out of the orchestrator's plan JSON.
/// Returns `None` when absent, unparseable, or empty.
pub(crate) fn extract_research(plan_text: &str) -> Option<Research> {
    let value: serde_json::Value = runner::parse_report(plan_text)?;
    let research: Research = serde_json::from_value(value.get("research")?.clone()).ok()?;
    (!research.is_empty()).then_some(research)
}

/// Render a `research.md` audit artifact from the extracted evidence.
pub(crate) fn render_research_markdown(research: &Research, mission_id: &str) -> String {
    use std::fmt::Write as _;
    let mut md = format!(
        "# Research — {mission_id}\n\nEvidence behind the approved plan \
         (roadmap M1 / repo-knowledge-store slice 1). Candidate knowledge updates \
         feed `docs/knowledge/`.\n"
    );
    let list = |md: &mut String, title: &str, items: &[String]| {
        let items: Vec<&str> = items
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect();
        if items.is_empty() {
            return;
        }
        let _ = write!(md, "\n## {title}\n\n");
        for it in items {
            let _ = writeln!(md, "- {it}");
        }
    };
    list(&mut md, "Files & docs read", &research.files_read);
    list(&mut md, "External sources", &research.sources);
    let facts: Vec<&ResearchFact> = research
        .facts
        .iter()
        .filter(|f| !f.fact.trim().is_empty())
        .collect();
    if !facts.is_empty() {
        let _ = write!(md, "\n## Facts\n\n");
        for f in facts {
            let fact = f.fact.trim();
            let ev = f.evidence.trim();
            if ev.is_empty() {
                let _ = writeln!(md, "- {fact}");
            } else {
                let _ = writeln!(md, "- {fact} — `{ev}`");
            }
        }
    }
    list(&mut md, "Ambiguities & stale docs", &research.ambiguities);
    list(
        &mut md,
        "Candidate knowledge updates",
        &research.candidate_knowledge_updates,
    );
    md
}

/// Render the revised plan as human-readable markdown for review (roadmap M2),
/// committed to the mission branch as `revised-plan.md`. Shows the full revised
/// plan plus a "Re-plan changes" section describing the reducer's merge rules.
/// Older single-milestone re-plan callers can still pass explicit dropped/added
/// feature lists; the event-driven M2 path records the exact structural change
/// in the `plan.revised` event and folded state.
pub fn render_revised_plan_markdown(
    plan: &Plan,
    mission: &Mission,
    dropped_feature_ids: &[String],
    added_features: &[PlanFeature],
) -> String {
    use std::fmt::Write as _;
    let mut md = String::new();
    let _ = writeln!(md, "# Revised mission plan — {}", mission.id);
    let _ = writeln!(md, "\n**Goal:** {}\n", plan.goal);
    let _ = writeln!(
        md,
        "Branch `{}` (from `{}`). Mid-mission revision of the plan of record \
         ([plan.md](plan.md)); completed milestones are preserved unchanged.\n",
        mission.mission_branch, mission.base_branch
    );

    let _ = writeln!(md, "## Re-plan changes applied\n");
    let _ = writeln!(
        md,
        "Completed milestones are frozen unchanged. Remaining milestones are merged by \
         position; existing pending features match by title, omitted pending features are \
         skipped, and new feature titles are appended with revision-scoped ids. Review the \
         resulting plan below and the `plan.revised` event for the exact machine state.\n"
    );
    if !dropped_feature_ids.is_empty() {
        let _ = writeln!(
            md,
            "- Dropped (skipped) features: {}",
            dropped_feature_ids.join(", ")
        );
    }
    if !added_features.is_empty() {
        let titles: Vec<String> = added_features
            .iter()
            .map(|f| f.title.trim().to_string())
            .collect();
        let _ = writeln!(md, "- Added features: {}", titles.join(", "));
    }
    if dropped_feature_ids.is_empty() && added_features.is_empty() {
        let _ = writeln!(
            md,
            "- Per-feature legacy diff: not supplied for this revision path"
        );
    }
    let _ = writeln!(md);

    let _ = writeln!(md, "## Full revised plan\n");
    for (mi, m) in plan.milestones.iter().enumerate() {
        let _ = writeln!(md, "### Milestone {} — {}\n", mi + 1, m.title);
        for (fi, f) in m.features.iter().enumerate() {
            let _ = writeln!(md, "#### {}.{} {}\n", mi + 1, fi + 1, f.title);
            let _ = writeln!(md, "{}\n", f.spec.trim());
            if !f.validation_criteria.is_empty() {
                let _ = writeln!(md, "Done when:");
                for c in &f.validation_criteria {
                    let _ = writeln!(md, "- {c}");
                }
                let _ = writeln!(md);
            }
        }
    }
    while md.ends_with('\n') {
        md.pop();
    }
    md.push('\n');
    md
}

/// Render the mission completion report (roadmap M1): elapsed time and cost
/// vs the pre-mission estimate, what shipped per feature, the validation
/// history including waived findings with their justifications, and the
/// contract outcomes. Committed beside plan.md at mission completion and
/// linked from missions/index.md.
///
/// Deterministic given its inputs: every timestamp derives from `events`
/// (no wall clock — the completion instant is the mission.completed ts when
/// present, else the last event's, since the engine renders the report just
/// before emitting mission.completed). No scrubbing happens here: all
/// model-authored text in `state`/`events` was scrubbed at emit time.
pub fn render_mission_report(
    state: &MissionState,
    events: &[Event],
    plan: &Plan,
    estimate: &cost::CostEstimate,
    execution_root: &Path,
) -> String {
    use std::fmt::Write as _;
    let mission = &state.mission;
    let mut md = String::new();
    let _ = writeln!(md, "# Mission report — {}", mission.id);
    let _ = writeln!(md, "\n**Goal:** {}\n", mission.goal);
    let _ = writeln!(
        md,
        "Branch `{}` (from `{}`). Plan of record: [plan.md](plan.md).\n",
        mission.mission_branch, mission.base_branch
    );

    // Background before details (the explain-diff shape): what the mission
    // intended, in the plan's own words, before any numbers.
    let _ = writeln!(md, "## The plan\n");
    let feature_total: usize = mission.milestones.iter().map(|m| m.features.len()).sum();
    let command_assertions = plan
        .validation_contract
        .iter()
        .filter(|a| a.check == AssertionCheck::Command)
        .count();
    let judgement_assertions = plan.validation_contract.len() - command_assertions;
    let _ = writeln!(
        md,
        "{} milestone{}, {} feature{}, gated by {} contract assertion{} ({} command, {} judgement).\n",
        mission.milestones.len(),
        if mission.milestones.len() == 1 { "" } else { "s" },
        feature_total,
        if feature_total == 1 { "" } else { "s" },
        plan.validation_contract.len(),
        if plan.validation_contract.len() == 1 { "" } else { "s" },
        command_assertions,
        judgement_assertions,
    );
    for (mi, m) in plan.milestones.iter().enumerate() {
        let _ = writeln!(md, "{}. **{}**", mi + 1, m.title);
        for f in &m.features {
            let intent = first_sentence(&f.spec);
            if intent.is_empty() {
                let _ = writeln!(md, "   - {}", f.title);
            } else {
                let _ = writeln!(md, "   - {} — {intent}", f.title);
            }
        }
    }
    if let Some(alternatives) = &plan.considered_alternatives {
        let chosen = first_sentence(&alternatives.chosen);
        if !chosen.is_empty() {
            let _ = writeln!(md, "\n**Chosen approach:** {chosen}");
        }
    }

    // Elapsed wall clock: created → completed, minus paused spans.
    let completed_ts = events
        .iter()
        .rev()
        .find_map(|e| matches!(e.kind, EventKind::MissionCompleted {}).then_some(e.ts))
        .or_else(|| events.last().map(|e| e.ts))
        .unwrap_or(mission.created_at);
    let paused = paused_time(events, completed_ts);
    let elapsed = std::cmp::max(
        completed_ts - mission.created_at - paused,
        chrono::Duration::zero(),
    );
    let _ = write!(md, "**Elapsed:** {}", format_duration(elapsed));
    if paused > chrono::Duration::zero() {
        let _ = write!(md, " ({} paused)", format_duration(paused));
    }
    let _ = writeln!(md);
    let t = &state.totals;
    let _ = writeln!(
        md,
        "**Tokens:** {} in / {} out / {} cache read / {} cache write",
        t.input, t.output, t.cache_read, t.cache_write
    );
    // Distinguish local/mixed spend from paid frontier spend rather than
    // reporting a misleading bare $0 (local-inference-cost-accounting).
    let cost_note = match crate::cost::mission_cost_class(state) {
        crate::cost::MissionCostClass::Frontier => "",
        crate::cost::MissionCostClass::Local => {
            " — local tier: $0 marginal (fixed hardware + electricity, not \
             per-token); excluded from frontier-cost calibration"
        }
        crate::cost::MissionCostClass::Mixed => {
            " — mixed local→frontier (escalated mid-mission); excluded from \
             frontier-cost calibration"
        }
    };
    let _ = writeln!(
        md,
        "**Cost:** ${:.2} actual{cost_note} vs ${:.2}–${:.2} estimated (expected ${:.2})",
        state.total_cost_usd, estimate.low_usd, estimate.high_usd, estimate.expected_usd
    );

    let _ = writeln!(md, "\n## Workspace");
    let isolation = match state.config.isolation() {
        WorkerIsolation::Worktree => "worktree",
        WorkerIsolation::Checkout => "checkout",
    };
    let _ = writeln!(md, "- **Isolation:** `{isolation}`");
    let _ = writeln!(
        md,
        "- **Worker/validator cwd:** `{}`",
        execution_root.display()
    );
    let _ = writeln!(
        md,
        "- **Sandbox:** worker `{}`; scrutiny `{}`; functional `{}`",
        sandbox_enforce_label(state.config.worker.sandbox.enforce),
        sandbox_enforce_label(state.config.validator_scrutiny.sandbox.enforce),
        sandbox_enforce_label(state.config.validator_functional.sandbox.enforce),
    );
    let preflight = events.iter().rev().find_map(|event| match &event.kind {
        EventKind::OrchestratorDecision { summary, .. } if summary.starts_with("preflight:") => {
            Some(summary.as_str())
        }
        _ => None,
    });
    match preflight {
        Some(summary) => {
            let _ = writeln!(md, "- **Preflight:** {summary}");
        }
        None => {
            let _ = writeln!(md, "- **Preflight:** clear — no advisory issues recorded");
        }
    }

    // What shipped — per milestone, per feature (fix features included, in
    // the order the reducer materialized them).
    let _ = writeln!(md, "\n## What shipped");
    for (mi, m) in mission.milestones.iter().enumerate() {
        let _ = writeln!(
            md,
            "\n### Milestone {} — {} {}\n",
            mi + 1,
            m.title,
            milestone_icon(m.status)
        );
        for f in &m.features {
            let runs = f.worker_runs.len();
            let _ = write!(
                md,
                "- {} **{}**{} — {} run{}",
                feature_icon(f.status),
                f.title,
                if f.origin == FeatureOrigin::Fix {
                    " *(fix)*"
                } else {
                    ""
                },
                runs,
                if runs == 1 { "" } else { "s" },
            );
            if f.respawns > 0 {
                let _ = write!(
                    md,
                    ", {} respawn{}",
                    f.respawns,
                    if f.respawns == 1 { "" } else { "s" }
                );
            }
            let _ = writeln!(md);
            // The explain-diff walkthrough: intent before evidence, inline.
            let intent = first_sentence(&f.spec);
            if !intent.is_empty() {
                let _ = writeln!(md, "  {intent}");
            }
            for commit in &f.commits {
                let _ = writeln!(md, "  - {}", short_commit(commit));
            }
            if f.status == FeatureStatus::Complete {
                for criterion in &f.validation_criteria {
                    let _ = writeln!(md, "  - ✓ {criterion}");
                }
            }
        }
    }

    // Validation history — replayed from the event log.
    let _ = writeln!(md, "\n## Validation history");
    let rounds = collect_validation_rounds(events);
    let mut per_milestone_round: HashMap<&str, usize> = HashMap::new();
    let mut rendered_any = false;
    for round in &rounds {
        // Rounds with neither findings nor a clean completion are reopening
        // bookkeeping (the gate's fix path re-emits milestone.validating
        // before fixfeature.created), not validation rounds.
        if round.findings.is_empty() && !round.clean {
            continue;
        }
        rendered_any = true;
        match round.milestone_id {
            Some(id) => {
                let n = per_milestone_round.entry(id).or_insert(0);
                *n += 1;
                let title = mission
                    .milestones
                    .iter()
                    .find(|m| m.id == id)
                    .map(|m| m.title.as_str())
                    .unwrap_or("");
                let _ = writeln!(md, "\n### {id} round {n} — {title}\n");
            }
            None => {
                let _ = writeln!(md, "\n### Final gate\n");
            }
        }
        if round.findings.is_empty() {
            let _ = writeln!(md, "No findings.");
            continue;
        }
        for (run_id, finding) in &round.findings {
            let gate = if *run_id == crate::reducer::ENGINE_RUN_ID {
                " *(final gate)*"
            } else {
                ""
            };
            let evidence = scrub::scrub_and_truncate(
                &finding
                    .evidence
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" "),
                200,
            );
            let _ = writeln!(
                md,
                "- [{}] {}{gate} — {evidence}",
                finding.severity, finding.subject
            );
        }
        if round.fix_features > 0 {
            let _ = writeln!(
                md,
                "\nDisposition: {} fix feature(s) created.",
                round.fix_features
            );
        }
        if !round.waived.is_empty() {
            let _ = writeln!(md, "\nDisposition: waived.");
            for reasons in &round.waived {
                for line in reasons.lines() {
                    let _ = writeln!(md, "{line}");
                }
            }
        }
        if let Some(reason) = round.blocked {
            let _ = writeln!(md, "\nDisposition: milestone blocked — {reason}");
        }
    }
    if !rendered_any {
        let _ = writeln!(md, "\nNo validation rounds were recorded.");
    }

    // Contract outcomes — the mission completed, so every assertion passed
    // the final gate (or was explicitly waived; waivers are recorded above).
    let _ = writeln!(md, "\n## Contract outcomes");
    if plan.validation_contract.is_empty() {
        let _ = writeln!(md, "\nNo contract assertions were defined.");
    } else {
        let _ = writeln!(md);
        for a in &plan.validation_contract {
            let check = match (&a.check, &a.command) {
                (AssertionCheck::Command, Some(cmd)) => format!("command: `{cmd}`"),
                (AssertionCheck::Command, None) => "command".to_string(),
                _ => "agent judgement".to_string(),
            };
            let _ = writeln!(md, "- ✅ **[{}]** {} *({check})*", a.id, a.statement);
        }
        let _ = writeln!(
            md,
            "\nAll assertions passed at the final contract gate (waivers, if any, appear in \
             the validation history)."
        );
    }
    md
}

/// One validation round replayed from the event log: a `milestone.validating`
/// round, or the final contract gate (`mission.validating` / engine-attributed
/// findings).
struct ValidationRound<'a> {
    /// `None` marks the final gate.
    milestone_id: Option<&'a str>,
    /// `(run_id, finding)` in emit order.
    findings: Vec<(&'a str, &'a Finding)>,
    fix_features: usize,
    /// Waiver reason blocks: the detail of "waived …" orchestrator decisions
    /// (one `- subject: reason` line per finding), falling back to the summary.
    waived: Vec<&'a str>,
    blocked: Option<&'a str>,
    /// The round produced no findings and the milestone completed directly.
    clean: bool,
}

/// Group the log's validation traffic into [`ValidationRound`]s. Dispositions
/// (fix features, waivers, blocks) always follow the findings they answer, so
/// they attach to the last round that has findings — which also covers the
/// gate's fix path, where the reopening `milestone.validating` arrives between
/// the gate findings and their `fixfeature.created` events.
fn collect_validation_rounds(events: &[Event]) -> Vec<ValidationRound<'_>> {
    fn round(milestone_id: Option<&str>) -> ValidationRound<'_> {
        ValidationRound {
            milestone_id,
            findings: Vec::new(),
            fix_features: 0,
            waived: Vec::new(),
            blocked: None,
            clean: false,
        }
    }
    let mut rounds: Vec<ValidationRound<'_>> = Vec::new();
    for event in events {
        match &event.kind {
            EventKind::MilestoneValidating { milestone_id } => {
                rounds.push(round(Some(milestone_id)));
            }
            EventKind::MissionValidating {} => rounds.push(round(None)),
            EventKind::ValidationFinding {
                milestone_id,
                run_id,
                finding,
            } => {
                // Engine-attributed findings belong to a gate round; the
                // second gate pass runs without a fresh mission.validating
                // (the status is already Validating), so open one on demand.
                let gate = run_id == crate::reducer::ENGINE_RUN_ID;
                let fits = rounds
                    .last()
                    .is_some_and(|r| !gate || r.milestone_id.is_none());
                if !fits {
                    rounds.push(round(if gate { None } else { Some(milestone_id) }));
                }
                rounds
                    .last_mut()
                    .expect("pushed above")
                    .findings
                    .push((run_id, finding));
            }
            EventKind::FixFeatureCreated { .. } => {
                if let Some(r) = rounds.iter_mut().rev().find(|r| !r.findings.is_empty()) {
                    r.fix_features += 1;
                }
            }
            EventKind::OrchestratorDecision { summary, detail }
                if summary.starts_with("waived") =>
            {
                if let Some(r) = rounds.iter_mut().rev().find(|r| !r.findings.is_empty()) {
                    r.waived.push(detail.as_deref().unwrap_or(summary));
                }
            }
            EventKind::MilestoneBlocked { reason, .. } => {
                if let Some(r) = rounds.iter_mut().rev().find(|r| !r.findings.is_empty()) {
                    r.blocked = Some(reason);
                }
            }
            EventKind::MilestoneCompleted { milestone_id, .. } => {
                if let Some(r) = rounds.last_mut() {
                    if r.milestone_id == Some(milestone_id.as_str()) && r.findings.is_empty() {
                        r.clean = true;
                    }
                }
            }
            _ => {}
        }
    }
    rounds
}

/// Total time the mission spent paused: fold `mission.paused`/`mission.resumed`
/// spans; a pause still open at `end` counts up to `end` (defensive — a
/// completed mission always resumed).
fn paused_time(events: &[Event], end: chrono::DateTime<chrono::Utc>) -> chrono::Duration {
    let mut total = chrono::Duration::zero();
    let mut paused_at: Option<chrono::DateTime<chrono::Utc>> = None;
    for event in events {
        match &event.kind {
            EventKind::MissionPaused {} => {
                if paused_at.is_none() {
                    paused_at = Some(event.ts);
                }
            }
            EventKind::MissionResumed {} => {
                if let Some(start) = paused_at.take() {
                    total += event.ts - start;
                }
            }
            _ => {}
        }
    }
    if let Some(start) = paused_at {
        total += end - start;
    }
    total
}

/// `4h 02m 09s` / `4m 02s` / `42s` (whole seconds; sub-second missions say 0s).
fn format_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// One `feature.completed` commit entry (`<sha> <subject>`) as markdown:
/// short sha in backticks + the subject. Entries that don't look like a sha
/// pass through verbatim.
fn short_commit(entry: &str) -> String {
    let (sha, subject) = match entry.split_once(' ') {
        Some((sha, subject)) => (sha, subject.trim()),
        None => (entry, ""),
    };
    let looks_sha = sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit());
    match (looks_sha, subject.is_empty()) {
        (true, false) => format!("`{}` {}", &sha[..7], subject),
        (true, true) => format!("`{}`", &sha[..7]),
        _ => entry.to_string(),
    }
}

/// The first sentence (or first line, whichever is shorter) of a spec/prose
/// block, trimmed and capped — the explain-diff "intent" line for a feature
/// or approach. Returns "" for empty input so callers can omit the line.
fn first_sentence(text: &str) -> String {
    const MAX: usize = 140;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let first_line = trimmed.lines().next().unwrap_or("").trim();
    let sentence_end = first_line
        .find(". ")
        .map(|i| i + 1)
        .unwrap_or(first_line.len());
    let candidate = first_line[..sentence_end].trim();
    let candidate = if candidate.is_empty() {
        first_line
    } else {
        candidate
    };
    if candidate.chars().count() <= MAX {
        candidate.to_string()
    } else {
        let mut out: String = candidate.chars().take(MAX - 1).collect();
        out.push('…');
        out
    }
}

fn feature_icon(status: FeatureStatus) -> &'static str {
    match status {
        FeatureStatus::Complete => "✅",
        FeatureStatus::Failed => "❌",
        FeatureStatus::Skipped => "⏭",
        FeatureStatus::Active | FeatureStatus::Pending => "⏳",
    }
}

fn milestone_icon(status: MilestoneStatus) -> &'static str {
    match status {
        MilestoneStatus::Complete => "✅",
        MilestoneStatus::Blocked => "⛔",
        _ => "⏳",
    }
}

fn sandbox_enforce_label(enforce: SandboxEnforce) -> &'static str {
    match enforce {
        SandboxEnforce::Off => "off",
        SandboxEnforce::Fs => "fs",
        SandboxEnforce::FsNet => "fs+net",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_research_pulls_the_optional_object() {
        let text = r#"{
            "goal": "g",
            "milestones": [{"title":"m","features":[{"title":"f","spec":"s","validationCriteria":["c"]}]}],
            "validationContract": [],
            "research": {
                "filesRead": ["crates/engine/src/orchestrator.rs"],
                "sources": ["https://example.com"],
                "facts": [{"fact":"the run loop folds events","evidence":"reducer.rs"}],
                "ambiguities": ["stale doc X"],
                "candidateKnowledgeUpdates": ["add architecture/run-loop.md"]
            }
        }"#;
        let r = extract_research(text).expect("research present");
        assert_eq!(r.files_read, vec!["crates/engine/src/orchestrator.rs"]);
        assert_eq!(r.facts.len(), 1);
        assert_eq!(r.facts[0].evidence, "reducer.rs");
        assert_eq!(r.candidate_knowledge_updates.len(), 1);

        // Absent research -> None.
        let none = r#"{"goal":"g","milestones":[{"title":"m","features":[{"title":"f","spec":"s","validationCriteria":["c"]}]}],"validationContract":[]}"#;
        assert!(extract_research(none).is_none());
        // Empty research object -> None (nothing to render).
        let empty = r#"{"goal":"g","milestones":[],"validationContract":[],"research":{}}"#;
        assert!(extract_research(empty).is_none());
    }

    #[test]
    fn render_research_markdown_lays_out_sections() {
        let r = Research {
            files_read: vec!["a.rs".into(), "  ".into()],
            sources: vec![],
            facts: vec![
                ResearchFact {
                    fact: "x holds".into(),
                    evidence: "a.rs:10".into(),
                },
                ResearchFact {
                    fact: "  ".into(),
                    evidence: "".into(),
                },
            ],
            ambiguities: vec!["doc drift".into()],
            candidate_knowledge_updates: vec!["note Y".into()],
        };
        let md = render_research_markdown(&r, "m-1");
        assert!(md.contains("# Research — m-1"), "{md}");
        assert!(md.contains("## Files & docs read"));
        assert!(md.contains("- a.rs"));
        assert!(md.contains("## Facts"));
        assert!(md.contains("- x holds — `a.rs:10`"), "{md}");
        assert!(md.contains("## Ambiguities & stale docs"));
        assert!(md.contains("- note Y"));
        // Empty sources section is omitted; the blank fact is skipped.
        assert!(!md.contains("## External sources"));
        assert!(!md.contains("-  \n"));
    }
}
