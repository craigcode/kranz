//! Planning turns — extracted from `orchestrator.rs` in the monolith split
//! (pure code motion, no behavior change). The plan-request turn family:
//! demanding the initial plan JSON and the revised plan for a running mission,
//! the large-scope prompt policies those turns embed (considered-alternatives
//! and research), the pre-emit validation gates a proposed (revised) plan must
//! pass, the plan JSON Schema, and the missions-catalog index upsert.

use crate::cost;
use crate::error::{EngineError, Result};
use crate::orchestrator::{MissionEngine, PlanRequest, JSON_RETRY_MSG};
use crate::reducer;
use crate::report_render::extract_research;
use crate::runner;
use crate::types::*;

impl MissionEngine {
    /// Demand the plan JSON (types::Plan, camelCase). Lenient parse with one
    /// retry demanding bare JSON; a plan parses to [`PlanRequest::Ready`]
    /// (unapproved). When the retry ALSO answers with prose, the orchestrator
    /// is simply not ready to emit (it wants answers first) — that text comes
    /// back as [`PlanRequest::NotReady`], never as an error.
    pub async fn request_plan(&mut self) -> Result<PlanRequest> {
        let message = format!(
            "Emit the plan now. Output ONLY a JSON object conforming exactly to this JSON \
             Schema — no prose before or after:\n{}\n\n{}\n\n{}",
            plan_schema(),
            considered_alternatives_prompt_policy(&self.state.config),
            research_prompt_policy(&self.state.config)
        );
        let text = self.orch_turn(&message).await?;
        if let Some(plan) = runner::parse_report::<Plan>(&text) {
            self.pending_research = extract_research(&text);
            return Ok(PlanRequest::Ready(plan));
        }
        let retry = self.orch_turn(JSON_RETRY_MSG).await?;
        match runner::parse_report::<Plan>(&retry) {
            Some(plan) => {
                self.pending_research = extract_research(&retry);
                Ok(PlanRequest::Ready(plan))
            }
            // Prefer the retry's text (the model's latest word); fall back to
            // the first turn's when the retry came back empty. Both are
            // already scrubbed by pump_turn.
            None => Ok(PlanRequest::NotReady(if retry.trim().is_empty() {
                text
            } else {
                retry
            })),
        }
    }

    /// Propose a REVISED plan for the not-yet-complete work of a running or
    /// blocked mission (roadmap M2). An orchestrator turn — digest + the
    /// current milestone/feature status + a revise-the-remainder instruction —
    /// that returns a full [`Plan`] (completed milestones unchanged and first,
    /// then the revised remainder). Reuses the streaming orchestrator, the
    /// lenient JSON parse, and the [`PlanRequest`] `Ready`/`NotReady` enum
    /// exactly like [`Self::request_plan`]; prose (the orchestrator wants to
    /// discuss first) comes back as `NotReady`, never an error.
    ///
    /// This only PROPOSES; [`Self::approve_revised_plan`] validates and applies
    /// the subset the event vocabulary can express (see the contract note
    /// above).
    pub async fn request_revised_plan(&mut self) -> Result<PlanRequest> {
        self.request_revised_plan_with_instructions("").await
    }

    pub(crate) async fn request_revised_plan_with_instructions(
        &mut self,
        instructions: &str,
    ) -> Result<PlanRequest> {
        let instructions = instructions.trim();
        let instructions_block = if instructions.is_empty() {
            "No extra operator instructions were supplied.".to_string()
        } else {
            format!("Operator revision request:\n{instructions}")
        };
        let mut message = format!(
            "The mission is already underway. Propose a REVISED plan for the work that is \
             NOT yet complete. Rules: keep every already-COMPLETE milestone exactly as it is \
             and list those completed milestones FIRST and unchanged (same title, same \
             features, same order); then revise the remaining milestones' features as the \
             current situation warrants (drop features no longer needed, add features now \
             required). Output ONLY a JSON object conforming exactly to this JSON Schema — \
             no prose before or after:\n{}\n\n{}\n\n{}\n\n{}",
            plan_schema(),
            considered_alternatives_prompt_policy(&self.state.config),
            research_prompt_policy(&self.state.config),
            instructions_block
        );
        // Revised-planning is in scope for knowledge injection (D-C); lessons
        // stay on the initial planning-seed path only.
        if let Some(block) = self.render_knowledge_for_planning() {
            message.push_str("\n\n");
            message.push_str(&block);
        }
        let text = self.orch_turn(&message).await?;
        if let Some(plan) = runner::parse_report::<Plan>(&text) {
            self.pending_research = extract_research(&text);
            return Ok(PlanRequest::Ready(plan));
        }
        let retry = self.orch_turn(JSON_RETRY_MSG).await?;
        match runner::parse_report::<Plan>(&retry) {
            Some(plan) => {
                self.pending_research = extract_research(&retry);
                Ok(PlanRequest::Ready(plan))
            }
            None => Ok(PlanRequest::NotReady(if retry.trim().is_empty() {
                text
            } else {
                retry
            })),
        }
    }
}

/// Upsert one mission's line in the missions catalog (`missions/index.md`):
/// `- <date> · [<id>](<id>/plan.md) — <goal>`. Newest last; a re-approval
/// replaces the mission's existing line instead of appending a duplicate.
pub fn upsert_mission_index(
    existing: &str,
    mission_id: &str,
    goal: &str,
    date: chrono::NaiveDate,
) -> String {
    const HEADER: &str = "# Kranz missions\n\nApproved plans, newest last.\n";
    let goal = crate::scrub::truncate_chars(goal.trim(), 120).replace('\n', " ");
    let line = format!("- {date} · [{mission_id}]({mission_id}/plan.md) — {goal}");
    let marker = format!("[{mission_id}](");

    let mut out = String::new();
    let mut replaced = false;
    let body = if existing.trim().is_empty() {
        HEADER
    } else {
        existing
    };
    for l in body.lines() {
        if l.contains(&marker) {
            out.push_str(&line);
            replaced = true;
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    if !replaced {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// Prompt policy asking the orchestrator to document its research for
/// broad/expensive plans (mirrors [`considered_alternatives_prompt_policy`]'s
/// triggers). The evidence lands in `research.md` beside `plan.md`.
fn research_prompt_policy(cfg: &MissionConfig) -> String {
    let mut triggers = Vec::new();
    if cfg.considered_alternatives_feature_threshold > 0 {
        triggers.push(format!(
            "{}+ features",
            cfg.considered_alternatives_feature_threshold
        ));
    }
    if cfg.considered_alternatives_touch_set_threshold > 0 {
        triggers.push(format!(
            "{}+ touchSet patterns",
            cfg.considered_alternatives_touch_set_threshold
        ));
    }
    if cfg.considered_alternatives_high_usd_threshold > 0.0 {
        triggers.push(format!(
            "likely high estimate at or above ${:.2}",
            cfg.considered_alternatives_high_usd_threshold
        ));
    }
    if triggers.is_empty() {
        return "The optional research object may be omitted.".to_string();
    }
    format!(
        "Policy: if this plan crosses any large-scope trigger ({}) include a research object \
         documenting filesRead, sources, facts (each with concise evidence: a path, command, or \
         URL), ambiguities/stale docs found, and candidateKnowledgeUpdates (proposed docs/knowledge/ \
         notes). Small plans may omit it.",
        triggers.join(", ")
    )
}

fn considered_alternatives_prompt_policy(cfg: &MissionConfig) -> String {
    let feature_threshold = cfg.considered_alternatives_feature_threshold;
    let touch_threshold = cfg.considered_alternatives_touch_set_threshold;
    let cost_threshold = cfg.considered_alternatives_high_usd_threshold;
    let mut triggers = Vec::new();
    if feature_threshold > 0 {
        triggers.push(format!("{feature_threshold}+ features"));
    }
    if touch_threshold > 0 {
        triggers.push(format!("{touch_threshold}+ touchSet patterns"));
    }
    if cost_threshold > 0.0 {
        triggers.push(format!(
            "likely high estimate at or above ${cost_threshold:.2}"
        ));
    }
    if triggers.is_empty() {
        return "The optional consideredAlternatives field may be omitted.".to_string();
    }
    format!(
        "Policy: if this plan crosses any large-scope trigger ({}) include \
         consideredAlternatives with a non-empty chosen approach and at least two rejected \
         approaches, each with a one-line tradeOff. Small plans may omit it.",
        triggers.join(", ")
    )
}

pub(crate) fn validate_considered_alternatives(
    plan: &Plan,
    estimate: &cost::CostEstimate,
    cfg: &MissionConfig,
) -> Result<()> {
    let required = considered_alternatives_requirement(plan, estimate, cfg);
    match (&plan.considered_alternatives, required) {
        (None, Some(reason)) => Err(EngineError::InvalidState(format!(
            "considered alternatives required: {reason}. Add consideredAlternatives with a \
             chosen approach and at least two rejected approaches with tradeOff."
        ))),
        (Some(alternatives), _) => validate_considered_alternatives_body(alternatives),
        (None, None) => Ok(()),
    }
}

pub(crate) fn considered_alternatives_requirement(
    plan: &Plan,
    estimate: &cost::CostEstimate,
    cfg: &MissionConfig,
) -> Option<String> {
    let features = plan_feature_count(plan);
    if cfg.considered_alternatives_feature_threshold > 0
        && features >= cfg.considered_alternatives_feature_threshold
    {
        return Some(format!(
            "{features} feature(s) >= feature threshold {}",
            cfg.considered_alternatives_feature_threshold
        ));
    }
    let touch_set = plan.touch_set.len();
    if cfg.considered_alternatives_touch_set_threshold > 0
        && touch_set >= cfg.considered_alternatives_touch_set_threshold
    {
        return Some(format!(
            "{touch_set} touchSet pattern(s) >= touchSet threshold {}",
            cfg.considered_alternatives_touch_set_threshold
        ));
    }
    if cfg.considered_alternatives_high_usd_threshold > 0.0
        && estimate.high_usd >= cfg.considered_alternatives_high_usd_threshold
    {
        return Some(format!(
            "estimated high cost ${:.2} >= cost threshold ${:.2}",
            estimate.high_usd, cfg.considered_alternatives_high_usd_threshold
        ));
    }
    None
}

fn validate_considered_alternatives_body(alternatives: &ConsideredAlternatives) -> Result<()> {
    if alternatives.chosen.trim().is_empty() {
        return Err(EngineError::InvalidState(
            "consideredAlternatives.chosen must not be empty".to_string(),
        ));
    }
    let valid_rejected = alternatives
        .rejected
        .iter()
        .filter(|r| !r.approach.trim().is_empty() && !r.trade_off.trim().is_empty())
        .count();
    if valid_rejected < 2 {
        return Err(EngineError::InvalidState(
            "consideredAlternatives.rejected must include at least two entries with approach \
             and tradeOff"
                .to_string(),
        ));
    }
    Ok(())
}

fn plan_feature_count(plan: &Plan) -> usize {
    plan.milestones.iter().map(|m| m.features.len()).sum()
}

pub(crate) fn validate_revised_plan_for_gate(mission: &Mission, plan: &Plan) -> Result<()> {
    if plan.milestones.is_empty() {
        return Err(EngineError::InvalidState(
            "revised plan has no milestones".to_string(),
        ));
    }
    if let Some(empty) = plan.milestones.iter().find(|m| m.features.is_empty()) {
        return Err(EngineError::InvalidState(format!(
            "revised plan milestone '{}' has no features",
            empty.title
        )));
    }
    validate_contract_extends(&mission.validation_contract, &plan.validation_contract)?;
    validate_vec_extends(
        "commandGrants",
        &mission.command_grants,
        &plan.command_grants,
    )?;
    validate_vec_extends("touchSet", &mission.touch_set, &plan.touch_set)?;

    let completed: Vec<&Milestone> = mission
        .milestones
        .iter()
        .filter(|m| m.status == MilestoneStatus::Complete)
        .collect();
    for (i, done) in completed.iter().enumerate() {
        let revised = plan.milestones.get(i).ok_or_else(|| {
            EngineError::InvalidState(format!(
                "revised plan drops completed milestone '{}' (must appear first, unchanged)",
                done.title
            ))
        })?;
        if revised.title.trim() != done.title.trim() {
            return Err(EngineError::InvalidState(format!(
                "revised plan milestone {} is '{}' but completed milestone '{}' must appear \
                 there unchanged",
                i + 1,
                revised.title,
                done.title
            )));
        }
        if !completed_features_unchanged(done, revised) {
            return Err(EngineError::InvalidState(format!(
                "revised plan alters the features of completed milestone '{}'",
                done.title
            )));
        }
    }
    Ok(())
}

fn validate_contract_extends(existing: &[Assertion], revised: &[Assertion]) -> Result<()> {
    for old in existing {
        let Some(new) = revised.iter().find(|a| a.id == old.id) else {
            return Err(EngineError::InvalidState(format!(
                "revised plan removes validation assertion '{}'",
                old.id
            )));
        };
        if old.statement != new.statement || old.check != new.check || old.command != new.command {
            return Err(EngineError::InvalidState(format!(
                "revised plan changes validation assertion '{}'",
                old.id
            )));
        }
    }
    Ok(())
}

fn validate_vec_extends(label: &str, existing: &[String], revised: &[String]) -> Result<()> {
    for old in existing {
        if !revised.iter().any(|new| new == old) {
            return Err(EngineError::InvalidState(format!(
                "revised plan removes {label} entry '{old}'"
            )));
        }
    }
    Ok(())
}

/// Whether a completed milestone's feature set is reproduced UNCHANGED in the
/// revised plan milestone (roadmap M2 re-planning guard). Delegates to the
/// reducer's canonical [`reducer::completed_features_match`] so this pre-emit
/// gate and the reducer's fold can never disagree: if they did, the gate could
/// accept a revision the reducer rejects, and `emit` (which appends before it
/// folds) would leave an unfoldable event in the append-only log.
pub(crate) fn completed_features_unchanged(done: &Milestone, revised: &PlanMilestone) -> bool {
    reducer::completed_features_match(&done.features, &revised.features)
}

/// Normalize a feature title for matching across a re-plan (trim + lowercase):
/// trivial editorial differences must not spuriously drop or re-add a feature.
pub(crate) fn norm_title(title: &str) -> String {
    title.trim().to_lowercase()
}

/// Assign `a-1..` ids to contract assertions with missing ids and
/// de-duplicate colliding ones (unique-ish, plan §4.5).
pub(crate) fn assign_assertion_ids(contract: &mut [Assertion]) {
    let mut seen = std::collections::HashSet::new();
    let mut counter = 0usize;
    for assertion in contract.iter_mut() {
        let id = assertion.id.trim().to_string();
        let id = if id.is_empty() || seen.contains(&id) {
            loop {
                counter += 1;
                let candidate = format!("a-{counter}");
                if !seen.contains(&candidate) {
                    break candidate;
                }
            }
        } else {
            id
        };
        seen.insert(id.clone());
        assertion.id = id;
    }
}

/// JSON Schema for [`Plan`] (camelCase), embedded in the request_plan turn.
fn plan_schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["goal", "validationContract", "milestones"],
        "properties": {
            "goal": { "type": "string" },
            "consideredAlternatives": {
                "type": "object",
                "additionalProperties": false,
                "required": ["chosen", "rejected"],
                "properties": {
                    "chosen": { "type": "string" },
                    "rejected": {
                        "type": "array",
                        "minItems": 2,
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "required": ["approach", "tradeOff"],
                            "properties": {
                                "approach": { "type": "string" },
                                "tradeOff": { "type": "string" }
                            }
                        }
                    }
                }
            },
            "research": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "filesRead": { "type": "array", "items": { "type": "string" } },
                    "sources": { "type": "array", "items": { "type": "string" } },
                    "facts": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "fact": { "type": "string" },
                                "evidence": { "type": "string" }
                            }
                        }
                    },
                    "ambiguities": { "type": "array", "items": { "type": "string" } },
                    "candidateKnowledgeUpdates": { "type": "array", "items": { "type": "string" } }
                }
            },
            "validationContract": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["id", "statement", "check"],
                    "properties": {
                        "id": { "type": "string" },
                        "statement": { "type": "string" },
                        "check": { "type": "string", "enum": ["command", "agent-judgement"] },
                        "command": { "type": "string" }
                    }
                }
            },
            "milestones": {
                "type": "array",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["title", "features"],
                    "properties": {
                        "title": { "type": "string" },
                        "features": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "additionalProperties": false,
                                "required": ["title", "spec", "validationCriteria"],
                                "properties": {
                                    "title": { "type": "string" },
                                    "spec": { "type": "string" },
                                    "validationCriteria": {
                                        "type": "array",
                                        "items": { "type": "string" }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            "commandGrants": {
                "type": "array",
                "items": { "type": "string" }
            },
            "touchSet": {
                "type": "array",
                "items": { "type": "string" }
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Unit tests for the tricky pure helpers
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn assertion(id: &str) -> Assertion {
        Assertion {
            id: id.to_string(),
            statement: "s".to_string(),
            check: AssertionCheck::AgentJudgement,
            command: None,
        }
    }

    #[test]
    fn assign_assertion_ids_fills_missing_and_dedupes() {
        let mut contract = vec![
            assertion(""),
            assertion("x"),
            assertion("x"),
            assertion("a-2"),
        ];
        assign_assertion_ids(&mut contract);
        let ids: Vec<&str> = contract.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids[0], "a-1", "missing id gets a-1");
        assert_eq!(ids[1], "x", "explicit unique id is kept");
        assert_ne!(ids[2], "x", "duplicate must be renamed");
        let unique: std::collections::HashSet<&&str> = ids.iter().collect();
        assert_eq!(unique.len(), 4, "all ids unique: {ids:?}");
    }

    #[test]
    fn plan_schema_matches_plan_shape() {
        // A Plan serialized to JSON uses exactly the keys the schema names.
        let plan = Plan {
            goal: "g".into(),
            validation_contract: vec![Assertion {
                id: "a-1".into(),
                statement: "s".into(),
                check: AssertionCheck::Command,
                command: Some("true".into()),
            }],
            milestones: vec![PlanMilestone {
                title: "m".into(),
                features: vec![PlanFeature {
                    title: "f".into(),
                    spec: "s".into(),
                    validation_criteria: vec!["c".into()],
                }],
            }],
            considered_alternatives: Some(ConsideredAlternatives {
                chosen: "single safe slice".into(),
                rejected: vec![
                    RejectedAlternative {
                        approach: "big bang".into(),
                        trade_off: "too broad".into(),
                    },
                    RejectedAlternative {
                        approach: "docs only".into(),
                        trade_off: "does not deliver behavior".into(),
                    },
                ],
            }),
            command_grants: vec!["gc lint".into()],
            touch_set: vec!["src/**".into()],
        };
        let value = serde_json::to_value(&plan).unwrap();
        let schema = plan_schema();
        let props = schema["properties"].as_object().unwrap();
        for key in value.as_object().unwrap().keys() {
            assert!(
                props.contains_key(key),
                "schema missing top-level key {key}"
            );
        }
    }
}
