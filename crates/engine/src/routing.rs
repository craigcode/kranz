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
//! (table, task class) inputs ALWAYS produce the same route — the first
//! matching exact `taskClassRules` entry wins; only when none matches are the
//! ordered `patternRules` consulted (first match wins); no match anywhere
//! falls through to [`ExecutorTier::Frontier`] (the safe default, identical
//! to the hardcoded floor's fallthrough). An EMPTY table is not resolved
//! here at all: [`crate::config::route_task_class_executor`] keeps the
//! legacy literal floor for it byte-for-byte, so configuring no table is a
//! perfect regression of today's behavior. Anything LLM-judged is
//! deliberately above this floor (the worker self-escalation layered on
//! top), never inside it. The tracked, base-branch-owned rules FILE that
//! populates the table lives in [`crate::routing_rules`] (ticket
//! `routing-rules-config`); the seed-time route record derived here
//! ([`seed_executor_route`]) rides `worker.spawned`.

use crate::types::{ExecutorRoute, ExecutorTier, RoutingConfig};

/// Normalize a task-class string for comparison: trimmed and ASCII-lowercased
/// — the SAME normalization the hardcoded floor
/// ([`crate::config::task_class_to_tier`]) applies, so a table rule matches
/// exactly the strings the literal floor would have.
pub fn normalize_task_class(task_class: &str) -> String {
    task_class.trim().to_ascii_lowercase()
}

/// Which table entry decided a route — the provenance half of the decision,
/// recorded per worker session so the effective route is never a hidden
/// implementation detail (ticket `routing-rules-config`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleMatch {
    /// `taskClassRules[i]` matched the task class exactly (after
    /// normalization).
    TaskClass(usize),
    /// `patternRules[i]` matched — consulted only when no exact rule did.
    Pattern(usize),
    /// No rule matched; the fall-through to [`ExecutorTier::Frontier`]
    /// decided.
    FallThrough,
}

impl RuleMatch {
    /// The recorded form: the table key path + index, exactly the shape
    /// [`validate_table`] errors name, so a session's route can be traced to
    /// the same rule a validation failure would name.
    pub fn describe(&self) -> String {
        match self {
            RuleMatch::TaskClass(i) => format!("taskClassRules[{i}]"),
            RuleMatch::Pattern(i) => format!("patternRules[{i}]"),
            RuleMatch::FallThrough => "fall-through".to_string(),
        }
    }
}

/// The full resolution of one task class against the table: the tier AND the
/// rule that decided it. [`table_tier`] is the tier-only view for callers
/// that predate provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteDecision {
    pub tier: ExecutorTier,
    pub rule: RuleMatch,
}

/// Resolve `task_class` against the routing table: the FIRST matching exact
/// rule wins; with no exact match the FIRST matching pattern rule wins; no
/// match anywhere (or no task class at all) falls through to
/// [`ExecutorTier::Frontier`].
///
/// Pure and total: no I/O, no time, no randomness — repeated calls with the
/// same inputs return the same decision, which is what makes the floor route
/// auditable (`mission.created` records the routed config, and each
/// `worker.spawned` can carry the recomputed decision). Callers handle the
/// empty table separately (the legacy literal floor), so this function
/// treats every rule present as intentional; [`validate_table`] has already
/// failed closed on a malformed one.
pub fn resolve(routing: &RoutingConfig, task_class: Option<&str>) -> RouteDecision {
    let Some(normalized) = task_class.map(normalize_task_class) else {
        return RouteDecision {
            tier: ExecutorTier::Frontier,
            rule: RuleMatch::FallThrough,
        };
    };
    for (i, rule) in routing.task_class_rules.iter().enumerate() {
        if normalize_task_class(&rule.task_class) == normalized {
            return RouteDecision {
                tier: rule.tier,
                rule: RuleMatch::TaskClass(i),
            };
        }
    }
    for (i, rule) in routing.pattern_rules.iter().enumerate() {
        if pattern_matches(&normalize_task_class(&rule.pattern), &normalized) {
            return RouteDecision {
                tier: rule.tier,
                rule: RuleMatch::Pattern(i),
            };
        }
    }
    RouteDecision {
        tier: ExecutorTier::Frontier,
        rule: RuleMatch::FallThrough,
    }
}

/// Resolve `task_class` against the routing table: the FIRST matching rule
/// wins; no rule matching (or no task class at all) falls through to
/// [`ExecutorTier::Frontier`]. Tier-only view of [`resolve`].
pub fn table_tier(routing: &RoutingConfig, task_class: Option<&str>) -> ExecutorTier {
    resolve(routing, task_class).tier
}

/// The pattern language for [`crate::types::PatternRoute`]: `*` matches any
/// (possibly empty) run of characters, every other character is a literal
/// byte. Both sides are already normalized (trim + lowercase) by the caller.
/// Deliberately NOT regex: the rules file is an operator contract, and a
/// two-wildcard-semantics glob keeps every rule auditable at a glance while
/// the classic two-pointer backtracking scan stays total and deterministic.
fn pattern_matches(pattern: &str, text: &str) -> bool {
    let (p, t) = (pattern.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    // Last `*` position and the text index it had consumed up to — the
    // backtrack point when a post-`*` literal fails to match.
    let (mut star, mut star_ti) = (None, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(sp) = star {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// The seed-time route record (ticket `routing-rules-config`): the EFFECTIVE
/// tier and the rule that decided it, folded once from `mission.created`
/// (whose `goal` still carries the folded task class — `plan.approved`
/// overwrites `state.mission.goal` with the plan's own goal, so the class
/// exists ONLY on this event) onto [`crate::types::Mission`], then replayed
/// onto every `worker.spawned`. Routing is provenance, not a hidden
/// implementation detail — and because resolution is deterministic, the
/// fold-time recomputation from the recorded (goal, config) equals the
/// seed-time decision [`crate::config::route_task_class_executor`] made, so
/// nothing new has to be persisted and a resumed engine records exactly what
/// a fresh one would.
///
/// The recorded tier is the SEEDED effective tier (derived from the routed
/// config exactly as [`crate::types::MissionState::executor_tier`] derives
/// it): a later mid-mission `config.changed` backend flip moves the live
/// tier, not this seed-time record — the flip is its own event.
///
/// `None` when the seed goal carried no task class at all: routing never
/// decided anything for such a mission, and its `worker.spawned` payload
/// stays byte-identical to the pre-provenance shape.
pub fn seed_executor_route(
    cfg: &crate::types::MissionConfig,
    mission_goal: &str,
) -> Option<ExecutorRoute> {
    let task_class = crate::ticket::parse_task_class_from_goal(mission_goal)?;
    let rule = if cfg.routing.is_empty() {
        // The legacy literal floor decided; there is no table rule to name.
        None
    } else {
        match resolve(&cfg.routing, Some(&task_class)).rule {
            RuleMatch::FallThrough => None,
            matched => Some(matched.describe()),
        }
    };
    Some(ExecutorRoute {
        tier: cfg.executor_tier(),
        rule,
    })
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
///
/// The same two checks apply to `patternRules` (ticket
/// `routing-rules-config`): a blank pattern is meaningless (spell `*`), and
/// a duplicate pattern is dead config under first-match-wins.
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
    let mut seen_patterns: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for (i, rule) in routing.pattern_rules.iter().enumerate() {
        let normalized = normalize_task_class(&rule.pattern);
        if normalized.is_empty() {
            return Err(format!(
                "routing.patternRules[{i}].pattern must be non-empty: routing matches on \
                 task-class text, so a blank pattern could never match honestly (spell \"*\" \
                 to match every class)"
            ));
        }
        if let Some(first) = seen_patterns.insert(normalized, i) {
            return Err(format!(
                "routing.patternRules[{i}].pattern {:?} duplicates rule {first} after \
                 normalization (trim + case-insensitive); first-match-wins makes the later rule \
                 dead config — remove or reword one",
                rule.pattern
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PatternRoute, TaskClassRoute};

    fn table(rules: &[(&str, ExecutorTier)]) -> RoutingConfig {
        RoutingConfig {
            task_class_rules: rules
                .iter()
                .map(|(task_class, tier)| TaskClassRoute {
                    task_class: task_class.to_string(),
                    tier: *tier,
                })
                .collect(),
            pattern_rules: vec![],
        }
    }

    fn pattern_table(rules: &[(&str, ExecutorTier)]) -> RoutingConfig {
        RoutingConfig {
            task_class_rules: vec![],
            pattern_rules: rules
                .iter()
                .map(|(pattern, tier)| PatternRoute {
                    pattern: pattern.to_string(),
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

    #[test]
    fn routing_rules_config_exact_rule_beats_pattern_and_order_decides() {
        // An exact taskClassRules entry always beats a pattern that would
        // also match — specific over general, documented precedence.
        let mut routing = pattern_table(&[("execution-*", ExecutorTier::Frontier)]);
        routing.task_class_rules.push(TaskClassRoute {
            task_class: "execution-class".to_string(),
            tier: ExecutorTier::Local,
        });
        assert_eq!(
            resolve(&routing, Some("execution-class")),
            RouteDecision {
                tier: ExecutorTier::Local,
                rule: RuleMatch::TaskClass(0),
            }
        );
        // The same class WITHOUT the exact rule is the pattern's claim.
        assert_eq!(
            resolve(&routing, Some("execution-heavy-class")),
            RouteDecision {
                tier: ExecutorTier::Frontier,
                rule: RuleMatch::Pattern(0),
            }
        );

        // Within the pattern list, order is the only precedence knob.
        let routing = pattern_table(&[
            ("docs-*", ExecutorTier::Local),
            ("*", ExecutorTier::Frontier),
        ]);
        assert_eq!(
            resolve(&routing, Some("docs-class")).rule,
            RuleMatch::Pattern(0),
            "first matching pattern wins"
        );
        assert_eq!(
            resolve(&routing, Some("anything-else")).rule,
            RuleMatch::Pattern(1),
            "the catch-all claims what the earlier pattern did not"
        );
    }

    #[test]
    fn routing_rules_config_pattern_glob_semantics() {
        let routing = pattern_table(&[("execution-*-class", ExecutorTier::Local)]);
        // `*` matches any run, empty included: the double dash of
        // "execution--class" is "execution-" + "" + "-class".
        for matching in [
            "execution-heavy-class",
            "execution--class",
            "EXECUTION-A-B-CLASS ",
        ] {
            assert_eq!(
                resolve(&routing, Some(matching)).tier,
                ExecutorTier::Local,
                "{matching:?} must match execution-*-class after normalization"
            );
        }
        // Anchored at BOTH ends: "execution-class" has no second dash for the
        // literal "-class" suffix, and prefixes/suffixes around the pattern
        // never slide.
        for non_matching in [
            "execution-class",
            "execution-class-extra",
            "xexecution-heavy-class",
            "docs-class",
        ] {
            assert_eq!(
                resolve(&routing, Some(non_matching)),
                RouteDecision {
                    tier: ExecutorTier::Frontier,
                    rule: RuleMatch::FallThrough,
                },
                "{non_matching:?} must NOT match execution-*-class"
            );
        }
        // Bare `*` matches everything, including the empty class.
        assert!(pattern_matches("*", ""));
        assert!(pattern_matches("**", "anything"));
        assert!(!pattern_matches("", "x"));
        assert!(pattern_matches("", ""));
    }

    #[test]
    fn routing_rules_config_resolution_is_deterministic() {
        // Same (table, task class) → same decision, rule included, every
        // time — the per-session provenance record recomputes this at spawn,
        // so any nondeterminism would fork the audit trail.
        let mut routing = pattern_table(&[
            ("docs-*", ExecutorTier::Frontier),
            ("execution-*", ExecutorTier::Local),
        ]);
        routing.task_class_rules.push(TaskClassRoute {
            task_class: "docs-class".to_string(),
            tier: ExecutorTier::Local,
        });
        for _ in 0..256 {
            assert_eq!(
                resolve(&routing, Some("docs-class")),
                RouteDecision {
                    tier: ExecutorTier::Local,
                    rule: RuleMatch::TaskClass(0),
                }
            );
            assert_eq!(
                resolve(&routing, Some("docs-other")),
                RouteDecision {
                    tier: ExecutorTier::Frontier,
                    rule: RuleMatch::Pattern(0),
                }
            );
            assert_eq!(
                resolve(&routing, Some("execution-class")),
                RouteDecision {
                    tier: ExecutorTier::Local,
                    rule: RuleMatch::Pattern(1),
                }
            );
            assert_eq!(
                resolve(&routing, Some("unlisted")),
                RouteDecision {
                    tier: ExecutorTier::Frontier,
                    rule: RuleMatch::FallThrough,
                }
            );
        }
    }

    #[test]
    fn routing_rules_config_validate_table_fails_closed_on_pattern_rules() {
        // Blank pattern: refused, naming the rule index + field.
        let routing = pattern_table(&[
            ("docs-*", ExecutorTier::Frontier),
            ("  ", ExecutorTier::Local),
        ]);
        let err = validate_table(&routing).expect_err("a blank pattern must be refused");
        assert!(err.contains("routing.patternRules[1].pattern"), "{err}");
        assert!(err.contains("non-empty"), "{err}");

        // Duplicate after normalization: refused, naming both rules.
        let routing = pattern_table(&[
            ("docs-*", ExecutorTier::Local),
            (" DOCS-* ", ExecutorTier::Frontier),
        ]);
        let err = validate_table(&routing).expect_err("a duplicate pattern must be refused");
        assert!(err.contains("routing.patternRules[1].pattern"), "{err}");
        assert!(err.contains("duplicates rule 0"), "{err}");

        // A clean mixed table validates.
        let mut routing = pattern_table(&[("docs-*", ExecutorTier::Frontier)]);
        routing.task_class_rules.push(TaskClassRoute {
            task_class: "execution-class".to_string(),
            tier: ExecutorTier::Local,
        });
        assert!(validate_table(&routing).is_ok(), "a clean table must pass");
    }

    #[test]
    fn routing_rules_config_seed_executor_route_records_rule_and_effective_tier() {
        // A folded ticket goal (the exact channel create parses) + a seeded,
        // frozen config → the record folded from mission.created.
        let ticket = crate::ticket::Ticket::parse(
            "bump-dep",
            "---\ntitle: Bump a dependency\ntask-class: execution-class\n---\n\n## Goal\nBump it.\n",
        )
        .expect("parse ticket");
        let goal = ticket.mission_goal();

        // Table routed the class local and the endpoint exists: the seeded
        // config was rewritten to the local backend, so the record names the
        // rule AND the effective local tier.
        let mut table = pattern_table(&[]);
        table.task_class_rules.push(TaskClassRoute {
            task_class: "execution-class".to_string(),
            tier: ExecutorTier::Local,
        });
        let mut cfg = crate::types::MissionConfig {
            routing: table,
            ..crate::types::MissionConfig::default()
        };
        cfg.worker.backend = Some("local".to_string());
        let route = seed_executor_route(&cfg, &goal).expect("a task class routes");
        assert_eq!(route.tier, ExecutorTier::Local);
        assert_eq!(route.rule.as_deref(), Some("taskClassRules[0]"));

        // Fall-through: the table exists but claims nothing — tier effective
        // from the (unrouted) config, no rule named.
        let cfg = crate::types::MissionConfig {
            routing: pattern_table(&[("docs-*", ExecutorTier::Local)]),
            ..crate::types::MissionConfig::default()
        };
        let route = seed_executor_route(&cfg, &goal).expect("a task class routes");
        assert_eq!(route.tier, ExecutorTier::Frontier);
        assert_eq!(route.rule, None, "the fall-through names no rule");

        // No table at all: the legacy literal floor decided; no rule to name,
        // and a goal WITHOUT a folded task class records nothing at all (the
        // pre-provenance byte shape).
        let cfg = crate::types::MissionConfig::default();
        let route = seed_executor_route(&cfg, &goal).expect("a task class routes");
        assert_eq!(route.tier, ExecutorTier::Frontier);
        assert_eq!(route.rule, None);
        assert!(seed_executor_route(&cfg, "ship the demo feature").is_none());
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
