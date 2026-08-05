//! The backend routing table (ticket `backend-routing-abstraction`, KRZ-331):
//! deterministic resolution of a ticket's task class to an executor
//! CAPABILITY CLASS ([`ExecutorTier`]) under the operator-configured table
//! ([`RoutingConfig`]).
//!
//! This module is the floor of the routing abstraction: local endpoints,
//! hosted frontier models, and hosted fine-tunes are peers behind one
//! interface because the table routes to capability classes only — NEVER to
//! model ids (docs/reviews/local-llm-and-triumvirate.md §1; the hardcoded-id
//! trap is "dead on arrival on anyone else's machine"). Which concrete
//! endpoint a `local` route resolves to is ordinary local-backend role
//! config (`baseUrl` + `model` + `contextBudget`): a hosted fine-tune behind
//! an OpenAI-compatible endpoint is exactly that config shape, not a new
//! backend kind and not routing-table content. The grep-style test below
//! pins the no-model-id rule on this file's logic.
//!
//! Determinism is the contract the rest of the engine relies on: the SAME
//! (table, task class) inputs ALWAYS produce the same route — first matching
//! rule wins, no match falls through to [`ExecutorTier::Frontier`] (the safe
//! default, identical to the hardcoded floor's fallthrough). An EMPTY table
//! is not resolved here at all: [`crate::config::route_task_class_executor`]
//! keeps the legacy literal floor for it byte-for-byte, so configuring no
//! table is a perfect regression of today's behavior. Anything LLM-judged is
//! deliberately above this floor (the worker self-escalation layered on top,
//! and the follow-up `routing-rules-config` tracked file surface), never
//! inside it.

use crate::types::{ExecutorTier, RoutingConfig};

/// Normalize a task-class string for comparison: trimmed and ASCII-lowercased
/// — the SAME normalization the hardcoded floor
/// ([`crate::config::task_class_to_tier`]) applies, so a table rule matches
/// exactly the strings the literal floor would have.
pub fn normalize_task_class(task_class: &str) -> String {
    task_class.trim().to_ascii_lowercase()
}

/// Resolve `task_class` against the routing table: the FIRST matching rule
/// wins; no rule matching (or no task class at all) falls through to
/// [`ExecutorTier::Frontier`].
///
/// Pure and total: no I/O, no time, no randomness — repeated calls with the
/// same inputs return the same tier, which is what makes the floor route
/// auditable (`mission.created` records the routed config, and the decision
/// summary names the table). Callers handle the empty table separately (the
/// legacy literal floor), so this function treats every rule present as
/// intentional; [`validate_table`] has already failed closed on a malformed
/// one.
pub fn table_tier(routing: &RoutingConfig, task_class: Option<&str>) -> ExecutorTier {
    let Some(normalized) = task_class.map(normalize_task_class) else {
        return ExecutorTier::Frontier;
    };
    for rule in &routing.task_class_rules {
        if normalize_task_class(&rule.task_class) == normalized {
            return rule.tier;
        }
    }
    ExecutorTier::Frontier
}

/// Fail-closed validation of the routing table, called by
/// [`crate::config::validate`] so a malformed table never reaches a mission.
/// Returns `Err` naming the offending rule (the config-file key path), never
/// a silently-corrected table:
///
/// - a blank `taskClass` could never match honestly (routing matches on
///   task-class text), and
/// - a duplicate class after normalization is dead config under
///   first-match-wins — almost always a mistake the operator meant to
///   reorder or merge, so it is refused rather than quietly shadowed.
pub fn validate_table(routing: &RoutingConfig) -> std::result::Result<(), String> {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for (i, rule) in routing.task_class_rules.iter().enumerate() {
        let normalized = normalize_task_class(&rule.task_class);
        if normalized.is_empty() {
            return Err(format!(
                "routing.taskClassRules[{i}].taskClass must be non-empty: routing matches on \
                 task-class text, so a blank rule could never match honestly"
            ));
        }
        if let Some(first) = seen.insert(normalized, i) {
            return Err(format!(
                "routing.taskClassRules[{i}].taskClass {:?} duplicates rule {first} after \
                 normalization (trim + case-insensitive); first-match-wins makes the later rule \
                 dead config — remove or reword one",
                rule.task_class
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TaskClassRoute;

    fn table(rules: &[(&str, ExecutorTier)]) -> RoutingConfig {
        RoutingConfig {
            task_class_rules: rules
                .iter()
                .map(|(task_class, tier)| TaskClassRoute {
                    task_class: task_class.to_string(),
                    tier: *tier,
                })
                .collect(),
        }
    }

    #[test]
    fn routing_abstraction_first_matching_rule_wins() {
        let routing = table(&[
            ("execution-class", ExecutorTier::Local),
            ("docs-class", ExecutorTier::Frontier),
        ]);
        assert_eq!(
            table_tier(&routing, Some("execution-class")),
            ExecutorTier::Local
        );
        assert_eq!(
            table_tier(&routing, Some("docs-class")),
            ExecutorTier::Frontier
        );

        // A class two rules could have claimed is decided by ORDER: the
        // earlier rule wins, so table order is the only precedence knob.
        let routing = table(&[
            ("execution-class", ExecutorTier::Frontier),
            ("execution-class", ExecutorTier::Local),
        ]);
        assert_eq!(
            table_tier(&routing, Some("execution-class")),
            ExecutorTier::Frontier,
            "first match wins — the shadowed later rule never decides"
        );
    }

    #[test]
    fn routing_abstraction_matching_uses_the_floors_normalization() {
        let routing = table(&[("Execution-Class", ExecutorTier::Local)]);
        // Same case- and whitespace-insensitivity as the hardcoded floor.
        assert_eq!(
            table_tier(&routing, Some("  EXECUTION-CLASS ")),
            ExecutorTier::Local
        );
    }

    #[test]
    fn routing_abstraction_no_matching_rule_stays_frontier() {
        let routing = table(&[("execution-class", ExecutorTier::Local)]);
        assert_eq!(
            table_tier(&routing, Some("planning-class")),
            ExecutorTier::Frontier
        );
        assert_eq!(table_tier(&routing, None), ExecutorTier::Frontier);
        assert_eq!(table_tier(&routing, Some("")), ExecutorTier::Frontier);
    }

    #[test]
    fn routing_abstraction_resolution_is_deterministic() {
        // The floor's contract: same (table, task class) → same route, every
        // time. Iterate enough to catch accidental nondeterminism (hash
        // iteration, time, randomness) — the implementation is an ordered
        // scan, so any deviation here is a regression.
        let routing = table(&[
            ("alpha-class", ExecutorTier::Local),
            ("beta-class", ExecutorTier::Frontier),
            ("gamma-class", ExecutorTier::Local),
        ]);
        for _ in 0..256 {
            assert_eq!(
                table_tier(&routing, Some("alpha-class")),
                ExecutorTier::Local
            );
            assert_eq!(
                table_tier(&routing, Some("beta-class")),
                ExecutorTier::Frontier
            );
            assert_eq!(
                table_tier(&routing, Some("gamma-class")),
                ExecutorTier::Local
            );
            assert_eq!(
                table_tier(&routing, Some("unlisted-class")),
                ExecutorTier::Frontier
            );
        }
    }

    #[test]
    fn routing_abstraction_validate_table_fails_closed_naming_the_rule() {
        // Blank class: refused, naming the offending rule index.
        let routing = table(&[
            ("docs-class", ExecutorTier::Frontier),
            ("   ", ExecutorTier::Local),
        ]);
        let err = validate_table(&routing).expect_err("a blank class must be refused");
        assert!(err.contains("routing.taskClassRules[1].taskClass"), "{err}");
        assert!(err.contains("non-empty"), "{err}");

        // Duplicate after normalization: refused, naming both rules — a
        // shadowed rule is dead config under first-match-wins.
        let routing = table(&[
            ("execution-class", ExecutorTier::Local),
            (" Execution-Class ", ExecutorTier::Frontier),
        ]);
        let err = validate_table(&routing).expect_err("a duplicate class must be refused");
        assert!(err.contains("routing.taskClassRules[1].taskClass"), "{err}");
        assert!(err.contains("duplicates rule 0"), "{err}");

        // A clean table validates.
        let routing = table(&[
            ("execution-class", ExecutorTier::Local),
            ("docs-class", ExecutorTier::Frontier),
        ]);
        assert!(validate_table(&routing).is_ok(), "a clean table must pass");
        assert!(validate_table(&RoutingConfig::default()).is_ok());
    }

    /// The no-hardcoded-model-ids rule (KRZ-331, citing
    /// docs/reviews/local-llm-and-triumvirate.md §1), pinned as a grep over
    /// this module's NON-TEST source: the routing table resolves capability
    /// classes ([`ExecutorTier`]) only, so no model-id literal may appear in
    /// the resolution logic or its docs. Model ids legitimately live
    /// ELSEWHERE in core and are the documented allowlist, all outside the
    /// routing table code paths: the per-role default model names in
    /// `config.rs` (`role_default_model`) and `types.rs`
    /// (`MissionConfig::default`), the backend default model constants in
    /// `cost.rs`, tier classification in `config::model_tier`, and test
    /// fixtures everywhere (including this file's own test module, which the
    /// split below excludes — the needles themselves live there).
    #[test]
    fn routing_abstraction_routing_logic_carries_no_model_id_literals() {
        let source = include_str!("routing.rs");
        let logic = source
            .split("#[cfg(test)]")
            .next()
            .expect("the test module marker exists");
        // Quoted alias forms (what a literal in code would look like) and
        // distinctive id substrings (provider-qualified or versioned ids).
        for needle in [
            "\"opus\"",
            "\"sonnet\"",
            "\"haiku\"",
            "\"fable\"",
            "gpt-",
            "glm-",
            "kimi-code",
            "claude-",
            "fireworks",
            "qwen",
            "mistral",
            "deepseek",
        ] {
            assert!(
                !logic.contains(needle),
                "model-id literal {needle:?} must never appear in the routing \
                 table code paths — route capability classes (ExecutorTier), \
                 not models"
            );
        }
    }
}
