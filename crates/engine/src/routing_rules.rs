//! Tracked routing rules (`.kranz/routing-rules.json`) — the base-branch-owned
//! FILE surface that populates the engine's routing table (ticket
//! `routing-rules-config`, the config-surface slice of KRZ-331).
//!
//! The routing floor ([`crate::routing`]) resolves a ticket's task class to
//! an executor capability class deterministically. This module is how a repo
//! declares that table as a tracked, reviewed artifact instead of layered
//! config: two rule forms, mirroring the reference design's "complexity
//! tiers + ordered rules" —
//!
//! ```json
//! {
//!   "taskClassRules": [
//!     {"taskClass": "execution-class", "tier": "local"}
//!   ],
//!   "patternRules": [
//!     {"pattern": "docs-*", "tier": "frontier"},
//!     {"pattern": "*", "tier": "frontier"}
//!   ]
//! }
//! ```
//!
//! - `taskClassRules` — the complexity-tier form: an exact task class
//!   (trimmed, case-insensitive) routed to a tier.
//! - `patternRules` — ordered pattern rules, consulted only when no exact
//!   rule matched: `*` matches any run of characters, everything else is
//!   literal. First match wins; no match anywhere falls through to
//!   `frontier`.
//!
//! Tiers are capability classes ([`crate::types::ExecutorTier`]: `local` |
//! `frontier`), NEVER model ids — the `tier` field deserializes the enum, so
//! a model-id-shaped value is a parse error, not a route (pinned by a test
//! below; the same no-model-id rule the floor's own grep test pins on
//! [`crate::routing`]).
//!
//! Ownership is the merge-gates idiom ([`crate::merge_gate`],
//! [`crate::merge`]): the bytes are read from the LIVE BASE BRANCH ref at
//! mission creation, never from the working tree and never from the mission
//! branch, so a mission cannot edit the rules that route it. A
//! mission-branch edit is therefore structurally ignored — and surfaced:
//! `run()` compares the mission branch's copy against the base's and records
//! an operator-visible `orchestrator.decision` when they differ
//! ([`crate::orchestrator::MissionEngine`]).
//!
//! Behavior contract, mirroring the workspace contract
//! ([`crate::workspace_contract`]):
//! - **Missing file ⇒ `Ok(None)`** — today's layered-config/per-role
//!   behavior, byte-identical. Never an error.
//! - **Present-but-empty ⇒ fail closed** the same way: a present file IS
//!   the routing table, so an empty one (`{}`, or both rule lists empty)
//!   would silently demote routing to the legacy task-class floor — delete
//!   the file to keep the layered config instead.
//! - **Present-but-invalid ⇒ fail closed** at draft (mission creation) and
//!   at approve, naming the file, the rule index, and the field.
//! - **Present and valid ⇒ the file IS the table**: it supersedes any
//!   layered-config `routing` key wholesale (one source of truth, no
//!   merge-order puzzle), and the supersession is recorded on the mission's
//!   decision log.

use crate::error::{EngineError, Result};
use crate::git_ops::GitRepo;
use crate::types::{PatternRoute, RoutingConfig, TaskClassRoute};
use serde::Deserialize;

pub const ROUTING_RULES_PATH: &str = ".kranz/routing-rules.json";

/// The on-disk shape. Strict (`deny_unknown_fields`) at the top level — a
/// typo'd key in a reviewed contract artifact is a mistake worth naming, the
/// same posture as `.kranz/merge-gates.json` and `.kranz/workspace.json`.
/// Rule-level keys reuse the engine's [`TaskClassRoute`]/[`PatternRoute`]
/// shapes, so the file IS the table after conversion: no second schema to
/// drift.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RoutingRulesFile {
    #[serde(default)]
    task_class_rules: Vec<TaskClassRoute>,
    #[serde(default)]
    pattern_rules: Vec<PatternRoute>,
}

/// Parse and validate routing-rules file bytes. Every validation failure
/// names the file and the offending rule (index + field, via
/// [`crate::routing::validate_table`]); parse failures carry the serde
/// location. An EMPTY table is refused too — a present file IS the routing
/// table, so `{}` would silently demote routing to the legacy task-class
/// floor; the refusal names the file and points at the fix (delete the file
/// to keep the layered config). A valid file converts into the engine's
/// [`RoutingConfig`] verbatim — both rule forms, order preserved.
pub fn parse_routing_rules(bytes: &[u8]) -> std::result::Result<RoutingConfig, String> {
    let file: RoutingRulesFile = serde_json::from_slice(bytes)
        .map_err(|e| format!("invalid JSON in {ROUTING_RULES_PATH}: {e}"))?;
    let routing = RoutingConfig {
        task_class_rules: file.task_class_rules,
        pattern_rules: file.pattern_rules,
    };
    if routing.is_empty() {
        return Err(format!(
            "{ROUTING_RULES_PATH}: the rules table is empty: a present file IS the routing \
             table, so an empty one would silently demote routing to the legacy task-class \
             floor — delete the file to keep the layered config, or declare at least one rule"
        ));
    }
    crate::routing::validate_table(&routing)
        .map_err(|violation| format!("{ROUTING_RULES_PATH}: {violation}"))?;
    Ok(routing)
}

/// Load the rules as COMMITTED on `ref_name` (the live base branch at
/// mission-creation time — merge.rs's `live_base_sha` idiom): committed
/// bytes only, so an uncommitted working-tree edit or a mission-branch edit
/// can never re-route a mission. Missing ⇒ `Ok(None)` (the no-file
/// regression: today's behavior, byte-identical); present-but-invalid or
/// present-but-empty ⇒ the fail-closed [`EngineError`] shape the workspace
/// contract uses, owner repo-setup.
pub fn load_routing_rules_at_ref(repo: &GitRepo, ref_name: &str) -> Result<Option<RoutingConfig>> {
    match repo.show_file(ref_name, ROUTING_RULES_PATH)? {
        None => Ok(None),
        Some(bytes) => parse_routing_rules(&bytes).map(Some).map_err(|violation| {
            EngineError::Config(format!(
                "routing rules {ROUTING_RULES_PATH} at {ref_name} is invalid (owner: repo-setup): {violation}"
            ))
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ExecutorTier;

    fn git_repo() -> Option<(tempfile::TempDir, GitRepo)> {
        // Mirror the engine test idiom: skip cleanly when git is unavailable.
        let dir = tempfile::tempdir().unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .ok()?;
        if !init.status.success() {
            return None;
        }
        for args in [
            &["config", "user.name", "test"][..],
            &["config", "user.email", "test@example.com"][..],
        ] {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap()
                .status
                .success());
        }
        std::fs::write(dir.path().join("README.md"), "seed\n").unwrap();
        for args in [&["add", "-A"][..], &["commit", "-m", "seed"][..]] {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap()
                .status
                .success());
        }
        let root = std::fs::canonicalize(dir.path()).unwrap();
        Some((dir, GitRepo::open(&root).unwrap()))
    }

    fn commit_rules(repo_root: &std::path::Path, branch: &str, bytes: &str) {
        let run = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(repo_root)
                .output()
                .unwrap()
                .status
                .success());
        };
        run(&["checkout", branch]);
        let path = repo_root.join(ROUTING_RULES_PATH);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, bytes).unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-m", "rules"]);
        run(&["checkout", "main"]);
    }

    const VALID: &str = r#"{
        "taskClassRules": [
            {"taskClass": "execution-class", "tier": "local"}
        ],
        "patternRules": [
            {"pattern": "docs-*", "tier": "frontier"},
            {"pattern": "*", "tier": "frontier"}
        ]
    }"#;

    #[test]
    fn routing_rules_config_parses_both_rule_forms() {
        let routing = parse_routing_rules(VALID.as_bytes()).expect("a valid file parses");
        assert_eq!(routing.task_class_rules.len(), 1);
        assert_eq!(routing.task_class_rules[0].task_class, "execution-class");
        assert_eq!(routing.task_class_rules[0].tier, ExecutorTier::Local);
        assert_eq!(routing.pattern_rules.len(), 2);
        assert_eq!(routing.pattern_rules[0].pattern, "docs-*");
        assert!(!routing.is_empty());
    }

    #[test]
    fn routing_rules_config_invalid_rule_fails_closed_naming_index_and_field() {
        // Not JSON: parse failure, file named, serde location carried.
        let err = parse_routing_rules(b"not json").unwrap_err();
        assert!(err.contains(ROUTING_RULES_PATH), "{err}");

        // Blank task class: rule index + field named.
        let err = parse_routing_rules(
            br#"{"taskClassRules": [{"taskClass": "ok-class", "tier": "local"},
                                      {"taskClass": "  ", "tier": "frontier"}]}"#,
        )
        .unwrap_err();
        assert!(err.contains(ROUTING_RULES_PATH), "{err}");
        assert!(err.contains("taskClassRules[1].taskClass"), "{err}");

        // Duplicate pattern after normalization: rule index + field named.
        let err = parse_routing_rules(
            br#"{"patternRules": [{"pattern": "docs-*", "tier": "local"},
                                  {"pattern": " DOCS-* ", "tier": "frontier"}]}"#,
        )
        .unwrap_err();
        assert!(err.contains("patternRules[1].pattern"), "{err}");
        assert!(err.contains("duplicates rule 0"), "{err}");

        // Unknown top-level key: strict schema refuses the typo.
        let err = parse_routing_rules(br#"{"taskclassRules": []}"#).unwrap_err();
        assert!(err.contains(ROUTING_RULES_PATH), "{err}");
    }

    /// The capability-classes-only rule on the FILE schema: where the field
    /// means a tier, a model-id-shaped value is a parse error — the enum has
    /// no variant for it, so no rule can ever route to a hardcoded model.
    #[test]
    fn routing_rules_config_schema_rejects_model_id_shaped_tiers() {
        for shape in [
            br#"{"taskClassRules": [{"taskClass": "execution-class", "tier": "claude-opus-4-1"}]}"#
                .as_slice(),
            br#"{"patternRules": [{"pattern": "*", "tier": "gpt-5-codex"}]}"#.as_slice(),
            br#"{"patternRules": [{"pattern": "*", "tier": "sonnet"}]}"#.as_slice(),
        ] {
            let err = parse_routing_rules(shape).unwrap_err();
            assert!(
                err.contains("unknown variant") || err.contains("tier"),
                "a model-id-shaped tier must fail the schema: {err}"
            );
        }
    }

    #[test]
    fn routing_rules_config_reads_committed_base_bytes_only() {
        let Some((dir, repo)) = git_repo() else {
            eprintln!("skipping test: git is not on PATH");
            return;
        };
        commit_rules(dir.path(), "main", VALID);

        // Present on the base ref ⇒ Some, regardless of the working tree...
        let routing = load_routing_rules_at_ref(&repo, "main")
            .expect("load must not error")
            .expect("committed rules load");
        assert_eq!(routing.task_class_rules.len(), 1);

        // ...including when the working-tree copy was edited AFTER the commit
        // (uncommitted operator edits never reach a mission).
        std::fs::write(dir.path().join(ROUTING_RULES_PATH), "not json").unwrap();
        assert!(
            load_routing_rules_at_ref(&repo, "main").unwrap().is_some(),
            "the committed bytes govern, not the dirty working tree"
        );

        // A branch WITHOUT the file ⇒ None: the no-file regression.
        assert!(load_routing_rules_at_ref(&repo, "HEAD~1")
            .unwrap()
            .is_none());
    }

    #[test]
    fn routing_rules_config_mission_branch_edit_is_ignored_at_read() {
        let Some((dir, repo)) = git_repo() else {
            eprintln!("skipping test: git is not on PATH");
            return;
        };
        commit_rules(dir.path(), "main", VALID);
        // A mission branch weakens the rules: its copy never governs the read.
        let run = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .unwrap()
                .status
                .success());
        };
        run(&["checkout", "-b", "kranz/mission-x"]);
        commit_rules(
            dir.path(),
            "kranz/mission-x",
            r#"{"patternRules": [{"pattern": "*", "tier": "local"}]}"#,
        );

        let routing = load_routing_rules_at_ref(&repo, "main")
            .unwrap()
            .expect("base rules load");
        assert!(
            routing
                .pattern_rules
                .iter()
                .all(|r| r.pattern != "*" || r.tier == ExecutorTier::Frontier),
            "the mission branch's weakened copy must never reach the base read"
        );
        // And the base read differs from the mission branch's — the drift the
        // run-time note surfaces.
        let mission = load_routing_rules_at_ref(&repo, "kranz/mission-x")
            .unwrap()
            .unwrap();
        assert_ne!(
            routing, mission,
            "fixture: the branches' copies must differ"
        );
    }

    #[test]
    fn routing_rules_config_invalid_at_ref_fails_closed_owner_repo_setup() {
        let Some((dir, repo)) = git_repo() else {
            eprintln!("skipping test: git is not on PATH");
            return;
        };
        commit_rules(
            dir.path(),
            "main",
            r#"{"taskClassRules": [{"taskClass": "", "tier": "local"}]}"#,
        );
        let err = load_routing_rules_at_ref(&repo, "main").unwrap_err();
        let text = format!("{err}");
        assert!(text.contains(ROUTING_RULES_PATH), "{text}");
        assert!(text.contains("owner: repo-setup"), "{text}");
        assert!(text.contains("taskClassRules[0].taskClass"), "{text}");
    }

    /// The empty-table wipe (ticket routing-rules-empty-table-wipe): a
    /// PRESENT empty file must fail closed exactly like an invalid one —
    /// `Some(empty)` would replace a non-empty layered table and silently
    /// demote routing to the legacy task-class floor.
    #[test]
    fn routing_rules_config_empty_table_fails_closed_at_parse() {
        for shape in [
            b"{}".as_slice(),
            br#"{"taskClassRules": [], "patternRules": []}"#.as_slice(),
            br#"{"taskClassRules": []}"#.as_slice(),
            br#"{"patternRules": []}"#.as_slice(),
        ] {
            let err = parse_routing_rules(shape).unwrap_err();
            assert!(err.contains(ROUTING_RULES_PATH), "{err}");
            assert!(err.contains("empty"), "{err}");
        }
        // And a non-empty valid file is unchanged.
        assert!(parse_routing_rules(VALID.as_bytes()).is_ok());
    }

    /// Same posture at the load seam: a committed empty file fails closed
    /// (owner: repo-setup) at the same point an invalid file does, while a
    /// MISSING file still loads as `None` — the layered-config behavior,
    /// byte-identical.
    #[test]
    fn routing_rules_config_empty_table_at_ref_fails_closed_missing_stays_none() {
        let Some((dir, repo)) = git_repo() else {
            eprintln!("skipping test: git is not on PATH");
            return;
        };
        commit_rules(dir.path(), "main", "{}");
        let err = load_routing_rules_at_ref(&repo, "main").unwrap_err();
        let text = format!("{err}");
        assert!(text.contains(ROUTING_RULES_PATH), "{text}");
        assert!(text.contains("owner: repo-setup"), "{text}");
        assert!(text.contains("empty"), "{text}");

        // The seed commit carries no file: missing ⇒ None, never an error.
        assert!(load_routing_rules_at_ref(&repo, "HEAD~1")
            .unwrap()
            .is_none());
    }

    /// The no-hardcoded-model-ids rule, pinned as a grep over this module's
    /// NON-TEST source — the same discipline [`crate::routing`]'s own test
    /// pins on the floor: the file surface resolves capability classes only,
    /// so no model-id literal may appear in its logic or docs. (The needles
    /// live down here in the test module, which the split excludes.)
    #[test]
    fn routing_rules_config_file_surface_carries_no_model_id_literals() {
        let source = include_str!("routing_rules.rs");
        let logic = source
            .split("#[cfg(test)]")
            .next()
            .expect("the test module marker exists");
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
                 rules file surface — route capability classes (ExecutorTier), \
                 not models"
            );
        }
    }
}
