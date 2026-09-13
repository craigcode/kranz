//! Plan-time feature context-fit check (plan-feature-context-fit-check
//! ticket): warn at approval when a feature looks bigger than one worker
//! session's context budget. kranz already enforces fresh-context-per-feature
//! at run time — this moves the sizing signal to plan time, where splitting
//! is cheap. Advisory only: a decision event and a plan.md note, never a
//! gate (v1 is a heuristic and must not hard-block plans).

use crate::paths::MissionPaths;
use crate::types::Plan;
use serde::Serialize;

/// Default anchor when a repo has no plan history yet: the p90 fallbacks.
/// Documented as defaults, not fitted numbers — a repo's own corpus replaces
/// them as soon as one historical plan exists.
pub const DEFAULT_SPEC_CHARS_P90: usize = 1_200;
pub const DEFAULT_FILE_MENTIONS_P90: usize = 5;

/// The corpus anchor: historical p90s of per-feature spec length and
/// mentioned-file counts, computed from the repo's own plan.json files.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitAnchor {
    pub spec_chars_p90: usize,
    pub file_mentions_p90: usize,
    pub plans_used: usize,
}

/// One feature that looks bigger than one worker session.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FeatureFitWarning {
    pub feature_id: String,
    pub title: String,
    pub spec_chars: usize,
    pub file_mentions: usize,
    pub reasons: Vec<String>,
}

/// Count file mentions in a spec: whitespace-separated tokens that look like
/// a repo path (contain `/` or a glob `*` or end in a common source
/// extension). Naive by design — it counts "how many files does this spec
/// point at", not prose length.
fn count_file_mentions(spec: &str) -> usize {
    const EXTENSIONS: &[&str] = &[
        ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".md", ".json", ".toml", ".yml",
        ".yaml", ".sh", ".mjs",
    ];
    spec.split_whitespace()
        .filter(|token| {
            let token = token.trim_matches(|c: char| {
                matches!(
                    c,
                    '`' | '"' | '\'' | '(' | ')' | '[' | ']' | ',' | ';' | ':'
                )
            });
            !token.is_empty()
                && (token.contains('/')
                    || token.contains('*')
                    || EXTENSIONS.iter().any(|ext| token.ends_with(ext)))
        })
        .count()
}

/// p90 of a small usize series (nearest-rank, matching cost.rs's percentile).
fn p90(values: &mut [usize]) -> usize {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let idx = ((values.len() as f64) * 0.9).ceil() as usize;
    values[idx.saturating_sub(1).min(values.len() - 1)]
}

/// Compute the anchor from the repo's historical plan.json files. Falls back
/// to the documented defaults when no plans exist (a fresh repo gets the
/// same check, just unfitted).
pub fn corpus_fit_anchor(repo_root: &std::path::Path) -> FitAnchor {
    let mut spec_chars = Vec::new();
    let mut file_mentions = Vec::new();
    let mut plans_used = 0usize;
    for mission_id in MissionPaths::list_missions(repo_root) {
        let plan_path = MissionPaths::new(repo_root, &mission_id).plan_file();
        let Ok(text) = std::fs::read_to_string(&plan_path) else {
            continue;
        };
        let Ok(plan) = serde_json::from_str::<Plan>(&text) else {
            continue;
        };
        plans_used += 1;
        for milestone in &plan.milestones {
            for feature in &milestone.features {
                spec_chars.push(feature.spec.chars().count());
                file_mentions.push(count_file_mentions(&feature.spec));
            }
        }
    }
    if plans_used == 0 {
        return FitAnchor {
            spec_chars_p90: DEFAULT_SPEC_CHARS_P90,
            file_mentions_p90: DEFAULT_FILE_MENTIONS_P90,
            plans_used: 0,
        };
    }
    FitAnchor {
        spec_chars_p90: p90(&mut spec_chars).max(200),
        file_mentions_p90: p90(&mut file_mentions).max(2),
        plans_used,
    }
}

/// Features whose footprint exceeds the anchor, with the reasons attached.
/// Pure and deterministic — the caller decides how to surface them.
pub fn feature_fit_warnings(plan: &Plan, anchor: &FitAnchor) -> Vec<FeatureFitWarning> {
    let mut warnings = Vec::new();
    for (mi, milestone) in plan.milestones.iter().enumerate() {
        for (fi, feature) in milestone.features.iter().enumerate() {
            let chars = feature.spec.chars().count();
            let mentions = count_file_mentions(&feature.spec);
            let mut reasons = Vec::new();
            if chars > anchor.spec_chars_p90 {
                reasons.push(format!(
                    "spec is {chars} chars (corpus p90: {})",
                    anchor.spec_chars_p90
                ));
            }
            if mentions > anchor.file_mentions_p90 {
                reasons.push(format!(
                    "spec points at {mentions} files (corpus p90: {})",
                    anchor.file_mentions_p90
                ));
            }
            if !reasons.is_empty() {
                warnings.push(FeatureFitWarning {
                    feature_id: format!("f-{}-{}", mi + 1, fi + 1),
                    title: feature.title.clone(),
                    spec_chars: chars,
                    file_mentions: mentions,
                    reasons,
                });
            }
        }
    }
    warnings
}

/// One-line-per-feature advisory text for the plan.md note and the decision
/// detail.
pub fn render_fit_note(warnings: &[FeatureFitWarning], anchor: &FitAnchor) -> String {
    let provenance = if anchor.plans_used == 0 {
        format!(
            "defaults (no plan history; p90 ≈ {} chars / {} files)",
            anchor.spec_chars_p90, anchor.file_mentions_p90
        )
    } else {
        format!(
            "corpus p90 over {} plan(s): {} chars / {} files",
            anchor.plans_used, anchor.spec_chars_p90, anchor.file_mentions_p90
        )
    };
    let mut out = format!("Context-fit check ({provenance}):");
    for warning in warnings {
        out.push_str(&format!(
            "\n- **{}** ({}) — {}",
            warning.feature_id,
            warning.title,
            warning.reasons.join("; ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{PlanFeature, PlanMilestone};

    fn plan_with(features: Vec<PlanFeature>) -> Plan {
        Plan {
            goal: "g".into(),
            validation_contract: vec![],
            milestones: vec![PlanMilestone {
                title: "m".into(),
                features,
            }],
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
            standards_manifest: None,
            reviewer_independence: None,
        }
    }

    fn feature(title: &str, spec: &str) -> PlanFeature {
        PlanFeature {
            title: title.into(),
            spec: spec.into(),
            validation_criteria: vec![],
        }
    }

    fn anchor() -> FitAnchor {
        FitAnchor {
            spec_chars_p90: 100,
            file_mentions_p90: 3,
            plans_used: 4,
        }
    }

    #[test]
    fn right_sized_features_stay_quiet() {
        let plan = plan_with(vec![feature("small", "add a flag to config.rs")]);
        assert!(feature_fit_warnings(&plan, &anchor()).is_empty());
    }

    #[test]
    fn oversized_features_warn_with_reasons() {
        let big_spec = format!(
            "{} touch crates/engine/src/orchestrator.rs and crates/cli/src/commands.rs and docs/a.md docs/b.md",
            "word ".repeat(40)
        );
        let plan = plan_with(vec![
            feature("small", "add a flag"),
            feature("huge", &big_spec),
        ]);
        let warnings = feature_fit_warnings(&plan, &anchor());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].feature_id, "f-1-2");
        assert_eq!(warnings[0].title, "huge");
        assert!(warnings[0]
            .reasons
            .iter()
            .any(|r| r.contains("chars (corpus p90: 100)")));
        assert!(warnings[0]
            .reasons
            .iter()
            .any(|r| r.contains("files (corpus p90: 3)")));
    }

    #[test]
    fn file_mentions_counts_paths_globs_and_source_extensions() {
        assert_eq!(count_file_mentions("edit `src/a.rs` and docs/b.md"), 2);
        assert_eq!(count_file_mentions("crates/engine/**/*.rs"), 1);
        assert_eq!(count_file_mentions("no files here at all"), 0);
        assert_eq!(count_file_mentions("src/lib.rs:42 has the fn"), 1);
    }

    #[test]
    fn render_fit_note_lists_features_and_provenance() {
        let warning = FeatureFitWarning {
            feature_id: "f-1-1".into(),
            title: "big".into(),
            spec_chars: 500,
            file_mentions: 8,
            reasons: vec!["spec is 500 chars (corpus p90: 100)".into()],
        };
        let note = render_fit_note(&[warning], &anchor());
        assert!(note.contains("corpus p90 over 4 plan(s)"));
        assert!(note.contains("f-1-1"));
        assert!(note.contains("big"));
    }
}
