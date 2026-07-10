//! Engine-computed out-of-contract-write sweep (M7 tier 1, feature f-1-2).
//!
//! Two checks, both deterministic and read-only, run in `validation_round`
//! alongside the spawned validator sessions (orchestrator.rs):
//!
//! 1. **Path sweep**: worker-authored paths changed in a milestone's commit
//!    range, compared against the mission's declared `touch_set` globs.
//! 2. **Primary-checkout cleanliness**: in worktree mode, the primary
//!    checkout must stay clean and on its original branch — the M7 tier-1
//!    guarantee that mission-branch work never touches the primary.
//!
//! Both surface as `Finding { class: "out-of-contract-write", .. }`, run
//! through the same `convert_findings` machinery as any other finding
//! (docs/scoping/worker-sandboxing.md: this is honestly-labeled detection,
//! not containment).

use crate::git_ops::CommitInfo;
use crate::types::Finding;
use globset::{Glob, GlobBuilder};

pub const FINDING_CLASS: &str = "out-of-contract-write";

/// Commit message prefix shared by every engine/meta commit template.
const ENGINE_COMMIT_PREFIX: &str = "[kranz]";

/// Known engine-authored commit subject templates. A bare `[kranz]` prefix is
/// NOT enough to match — but even a full template match is only NECESSARY,
/// never sufficient: a worker with `git commit` can title a commit
/// "[kranz] mission report cleanup" just as easily. The enforced exemption is
/// [`is_meta_commit_with_paths`], which additionally requires every touched
/// path to be mission-record metadata (see [`is_mission_record_path`]).
///
/// Dirty-tree checkpoints are intentionally NOT meta: they carry real worker
/// file changes and must count as deliverables / be path-swept.
const ENGINE_META_TEMPLATES: &[&str] = &[
    "[kranz] approved plan for ",
    "[kranz] revised plan for ",
    "[kranz] mission report",
];

/// Whether a contract `command` assertion that runs `cargo test` includes the
/// anti-vacuity guard (`test result: ok. [1-9]`) so a filter matching zero
/// tests cannot pass. Returns `true` when the command is not a cargo-test
/// gate, or when it already has the guard.
pub fn cargo_test_has_anti_vacuity(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if !lower.contains("cargo test") && !lower.contains("cargo\ttest") {
        return true;
    }
    // AGENTS.md rule 5: grep must require at least one passed test.
    command.contains("[1-9]") || command.contains("ok\\. [1-9]") || command.contains("ok. [1-9]")
}

/// Message used when the engine checkpoints a dirty worker tree.
pub fn checkpoint_commit_message(feature_id: &str) -> String {
    format!("[{feature_id}] checkpoint (engine commit)")
}

/// Message used when a parallel worktree checkpoints a dirty feature branch.
pub fn parallel_checkpoint_commit_message(feature_id: &str) -> String {
    format!("[{feature_id}] parallel worktree checkpoint (engine commit)")
}

/// Whether `subject` matches a known engine/meta commit template.
///
/// Subject templates are SPOOFABLE by a worker's own `git commit`, so this
/// alone must never exempt a commit from the deliverable count or the path
/// sweep — use [`is_meta_commit_with_paths`] wherever the commit's changed
/// paths are available (the orchestrator's sweep and final gate both do).
pub fn is_meta_commit(subject: &str) -> bool {
    if !subject.starts_with(ENGINE_COMMIT_PREFIX) {
        return false;
    }
    ENGINE_META_TEMPLATES
        .iter()
        .any(|tmpl| subject == *tmpl || subject.starts_with(tmpl))
}

/// Whether a commit is a GENUINE engine/meta commit (never attributed to a
/// worker, never counted as a deliverable, never path-swept): the subject
/// must match a known engine template AND every path the commit touches must
/// be mission-record metadata (see [`is_mission_record_path`]). A
/// spoofed-subject worker commit touching anything else (src/, docs/, …) is
/// treated as a worker commit — counted and swept like any other.
///
/// An empty `changed_paths` slice counts as meta when the subject matches:
/// engine commits are never empty (`commit_paths` no-ops instead of
/// committing), and an empty spoof carries no deliverable content to count
/// or sweep anyway.
pub fn is_meta_commit_with_paths(
    subject: &str,
    mission_id: &str,
    changed_paths: &[String],
) -> bool {
    is_meta_commit(subject)
        && changed_paths
            .iter()
            .all(|path| is_mission_record_path(mission_id, path))
}

/// The record files a genuine engine meta commit writes inside the CURRENT
/// mission's `.kranz/missions/<id>/` dir. Enumerated (not a prefix match)
/// from the engine's actual commit sites — see [`is_mission_record_path`].
const MISSION_RECORD_FILES: &[&str] = &[
    "plan.json",       // approve_plan / commit_revised_plan_record
    "plan.md",         // approve_plan / commit_revised_plan_record
    "revised-plan.md", // commit_revised_plan_record / approve_revised_plan
    "research.md",     // approve_plan / commit_revised_plan_record
    "report.md",       // try_write_mission_report
];

/// Whether `path` (relative to the repo root, forward-slash, as
/// `git diff --name-only` reports) is mission-record metadata a genuine
/// engine meta commit writes. Enumerated from the engine's actual commit
/// sites (orchestrator.rs `approve_plan` / `commit_revised_plan_record` /
/// `approve_revised_plan` / `try_write_mission_report` / `capture_lesson`):
/// the missions catalog (`.kranz/missions/index.md`), the exact record files
/// in the CURRENT mission's dir ([`MISSION_RECORD_FILES`]).
///
/// Deliberately no broader than that: a prefix match on all of
/// `.kranz/lessons/` or `.kranz/missions/<id>/` would let a worker commit
/// with a spoofed meta subject smuggle arbitrary files past the deliverable
/// count and the path sweep. No lesson path is exempt, including the current
/// mission's lesson and index: lessons are ingested into FUTURE missions'
/// planning prompts, and a worker can spoof every commit-subject template.
pub fn is_mission_record_path(mission_id: &str, path: &str) -> bool {
    if path == ".kranz/missions/index.md" {
        return true;
    }
    match path.strip_prefix(&format!(".kranz/missions/{mission_id}/")) {
        Some(record) => MISSION_RECORD_FILES.contains(&record),
        None => false,
    }
}

/// Mission meta paths (relative to repo root, forward-slash, matching
/// `git diff --name-only` output) that are never flagged even when they
/// fall outside the declared touch-set.
pub fn meta_paths(mission_id: &str) -> Vec<String> {
    vec![
        format!(".kranz/missions/{mission_id}/plan.json"),
        format!(".kranz/missions/{mission_id}/plan.md"),
        format!(".kranz/missions/{mission_id}/report.md"),
        ".kranz/missions/index.md".to_string(),
    ]
}

/// Whether `path` is one of the mission's meta files (see [`meta_paths`]).
pub fn is_meta_path(mission_id: &str, path: &str) -> bool {
    meta_paths(mission_id).iter().any(|p| p == path)
}

/// One compiled touch-set pattern: a glob plus whether it's a `!`-negated
/// (gitignore-style) exclusion.
struct TouchPattern {
    glob: globset::GlobMatcher,
    negate: bool,
}

/// Compile the mission's `touch_set` patterns (gitignore-style: `!` negates,
/// last match wins, `**` crosses path separators, plain `*` does not).
fn compile_touch_set(patterns: &[String]) -> Result<Vec<TouchPattern>, globset::Error> {
    patterns
        .iter()
        .map(|raw| {
            let (negate, pat) = match raw.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, raw.as_str()),
            };
            let glob: Glob = GlobBuilder::new(pat).literal_separator(true).build()?;
            Ok(TouchPattern {
                glob: glob.compile_matcher(),
                negate,
            })
        })
        .collect()
}

/// Whether `path` is inside the touch-set: gitignore semantics, the LAST
/// matching pattern decides; a plain (non-negated) match includes the path,
/// a `!`-prefixed match excludes it; no match at all excludes by default.
pub fn touch_set_includes(patterns: &[String], path: &str) -> Result<bool, globset::Error> {
    let compiled = compile_touch_set(patterns)?;
    let mut included = false;
    for pat in &compiled {
        if pat.glob.is_match(path) {
            included = !pat.negate;
        }
    }
    Ok(included)
}

/// One worker-authored path change, attributed to the commit that made it.
pub struct AttributedChange<'a> {
    pub path: &'a str,
    pub commit: &'a CommitInfo,
}

/// Build out-of-contract-write findings from a milestone's worker-authored
/// path changes (engine/meta commits already excluded by the caller — see
/// [`is_meta_commit_with_paths`]) against the mission's declared touch-set.
///
/// Returns one finding per distinct out-of-contract path (first attribution
/// wins when a path is touched by more than one commit). Empty `touch_set`
/// means the sweep is advisory-off: callers must not invoke this in that
/// case (checked by the caller so the "skip entirely" log line has a home).
pub fn path_findings(touch_set: &[String], changes: &[AttributedChange<'_>]) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for change in changes {
        if !seen.insert(change.path) {
            continue;
        }
        match touch_set_includes(touch_set, change.path) {
            Ok(true) => {}
            Ok(false) => findings.push(Finding {
                subject: change.path.to_string(),
                severity: "major".to_string(),
                evidence: format!(
                    "commit {} ({}) touched {} which matches none of the declared touch-set globs",
                    change.commit.sha, change.commit.subject, change.path
                ),
                suggested_fix: format!(
                    "relocate the change under a declared touch-set path, or add a glob for {} to the mission's touchSet",
                    change.path
                ),
                class: FINDING_CLASS.to_string(),
            }),
            Err(e) => findings.push(Finding {
                subject: change.path.to_string(),
                severity: "major".to_string(),
                evidence: format!(
                    "touch-set glob compile error while checking {}: {e}",
                    change.path
                ),
                suggested_fix: "fix the mission's touchSet globs".to_string(),
                class: FINDING_CLASS.to_string(),
            }),
        }
    }
    findings
}

/// Finding when the primary checkout is dirty or has moved off the branch it
/// was on when the mission started (worktree-mode invariant).
pub fn primary_checkout_finding(
    is_clean: bool,
    current_branch: &str,
    branch_at_start: &str,
) -> Option<Finding> {
    if is_clean && current_branch == branch_at_start {
        return None;
    }
    let evidence = if !is_clean && current_branch != branch_at_start {
        format!(
            "primary checkout is dirty and moved from '{branch_at_start}' to '{current_branch}'"
        )
    } else if !is_clean {
        "primary checkout has tracked changes while a worktree-mode mission is running".to_string()
    } else {
        format!("primary checkout moved from '{branch_at_start}' to '{current_branch}'")
    };
    Some(Finding {
        subject: "primary-checkout".to_string(),
        severity: "critical".to_string(),
        evidence,
        suggested_fix: "restore the primary checkout to a clean state on the starting branch"
            .to_string(),
        class: FINDING_CLASS.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(sha: &str, subject: &str) -> CommitInfo {
        CommitInfo {
            sha: sha.to_string(),
            subject: subject.to_string(),
        }
    }

    // -- out_of_contract: touch-set matching ---------------------------------

    #[test]
    fn out_of_contract_path_outside_touch_set_produces_one_finding() {
        let touch_set = vec!["src/**".to_string()];
        let c = commit("abc123", "[f-1] add");
        let changes = [AttributedChange {
            path: "docs/oops.md",
            commit: &c,
        }];
        let findings = path_findings(&touch_set, &changes);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].subject, "docs/oops.md");
        assert_eq!(findings[0].class, FINDING_CLASS);
        assert_eq!(findings[0].severity, "major");
    }

    #[test]
    fn out_of_contract_path_inside_touch_set_produces_no_finding() {
        let touch_set = vec!["src/**".to_string()];
        let c = commit("abc123", "[f-1] add");
        let changes = [AttributedChange {
            path: "src/lib.rs",
            commit: &c,
        }];
        assert!(path_findings(&touch_set, &changes).is_empty());
    }

    #[test]
    fn out_of_contract_duplicate_path_produces_exactly_one_finding() {
        let touch_set = vec!["src/**".to_string()];
        let c1 = commit("aaa", "[f-1] first");
        let c2 = commit("bbb", "[f-1] second");
        let changes = [
            AttributedChange {
                path: "docs/oops.md",
                commit: &c1,
            },
            AttributedChange {
                path: "docs/oops.md",
                commit: &c2,
            },
        ];
        let findings = path_findings(&touch_set, &changes);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].evidence.contains("aaa"));
    }

    #[test]
    fn out_of_contract_negated_glob_excludes_from_touch_set() {
        let touch_set = vec!["src/**".to_string(), "!src/generated/**".to_string()];
        assert!(touch_set_includes(&touch_set, "src/lib.rs").unwrap());
        assert!(!touch_set_includes(&touch_set, "src/generated/x.rs").unwrap());
    }

    #[test]
    fn out_of_contract_empty_touch_set_is_advisory_off() {
        // touch_set_includes on an empty set always excludes, so callers MUST
        // gate on emptiness rather than calling path_findings with `[]`.
        let touch_set: Vec<String> = vec![];
        assert!(!touch_set_includes(&touch_set, "src/anything.rs").unwrap());
    }

    // -- engine_commit_exempt -------------------------------------------------

    #[test]
    fn engine_commit_exempt_kranz_prefixed_commit_is_meta() {
        assert!(is_meta_commit("[kranz] approved plan for m-abc123"));
        assert!(is_meta_commit("[kranz] mission report"));
        assert!(is_meta_commit("[kranz] mission report for m-abc123"));
        assert!(is_meta_commit("[kranz] revised plan for m-abc123 (rev 2)"));
        assert!(!is_meta_commit("[f-1-2] add sweep"));
        // A bare `[kranz]` prefix does not match a template. (A forged
        // TEMPLATE subject still passes this check — the path-verified
        // is_meta_commit_with_paths is what closes that hole.)
        assert!(!is_meta_commit("[kranz] spoofed worker commit"));
        assert!(!is_meta_commit("[kranz]"));
        // Dirty-tree checkpoints carry worker files — not meta.
        assert!(!is_meta_commit(&checkpoint_commit_message("f-1")));
        assert!(!is_meta_commit(&parallel_checkpoint_commit_message("f-1")));
    }

    #[test]
    fn engine_commit_exempt_requires_mission_record_paths_not_just_subject() {
        let mission_id = "m-abc123";
        // Genuine meta commits: template subject, only mission-record paths
        // (the exact sets the engine's commit sites write).
        assert!(is_meta_commit_with_paths(
            "[kranz] approved plan for m-abc123",
            mission_id,
            &[
                ".kranz/missions/m-abc123/plan.json".to_string(),
                ".kranz/missions/m-abc123/plan.md".to_string(),
                ".kranz/missions/m-abc123/research.md".to_string(),
                ".kranz/missions/index.md".to_string(),
            ],
        ));
        assert!(is_meta_commit_with_paths(
            "[kranz] revised plan for m-abc123 (rev 2)",
            mission_id,
            &[".kranz/missions/m-abc123/revised-plan.md".to_string()],
        ));
        assert!(is_meta_commit_with_paths(
            "[kranz] mission report for m-abc123",
            mission_id,
            &[
                ".kranz/missions/m-abc123/report.md".to_string(),
                ".kranz/missions/index.md".to_string(),
            ],
        ));
        for lesson_path in [".kranz/lessons/m-abc123.md", ".kranz/lessons/index.md"] {
            assert!(!is_meta_commit_with_paths(
                "[kranz] mission report for m-abc123",
                mission_id,
                &[lesson_path.to_string()],
            ));
        }
        // Spoof: a template-matching subject on a commit touching a real
        // file must NOT be meta — it would otherwise dodge the deliverable
        // count and the out-of-contract path sweep.
        assert!(!is_meta_commit_with_paths(
            "[kranz] mission report cleanup",
            mission_id,
            &["src/lib.rs".to_string()],
        ));
        // Even one non-record path among record paths breaks the exemption.
        assert!(!is_meta_commit_with_paths(
            "[kranz] mission report for m-abc123",
            mission_id,
            &[
                ".kranz/missions/m-abc123/report.md".to_string(),
                "docs/oops.md".to_string(),
            ],
        ));
        // ANOTHER mission's record dir is not this mission's metadata.
        assert!(!is_meta_commit_with_paths(
            "[kranz] mission report for m-abc123",
            mission_id,
            &[".kranz/missions/m-other/report.md".to_string()],
        ));
        // A non-template subject is never meta, whatever the paths.
        assert!(!is_meta_commit_with_paths(
            "[f-1-2] add sweep",
            mission_id,
            &[".kranz/missions/m-abc123/report.md".to_string()],
        ));
    }

    #[test]
    fn mission_record_path_matches_engine_commit_sites_only() {
        let mission_id = "m-abc123";
        // The complete exempt set from the engine's commit sites
        // (approve_plan, commit_revised_plan_record, approve_revised_plan,
        // try_write_mission_report) — nothing else.
        for path in [
            ".kranz/missions/index.md",
            ".kranz/missions/m-abc123/plan.json",
            ".kranz/missions/m-abc123/plan.md",
            ".kranz/missions/m-abc123/revised-plan.md",
            ".kranz/missions/m-abc123/research.md",
            ".kranz/missions/m-abc123/report.md",
        ] {
            assert!(is_mission_record_path(mission_id, path), "{path}");
        }
        for path in [
            "src/lib.rs",
            ".kranz/secret-allowlist",
            ".kranz/missions/m-abc123", // the dir itself, not a record
            ".kranz/missions/m-abc1234/plan.json", // id prefix, other mission
            ".kranz/missions/m-other/plan.json",
            "kranz/missions/m-abc123/plan.json", // missing leading .kranz
            // Smuggle shapes: the engine never commits these, so a spoofed
            // meta subject touching them must NOT be sweep-exempt.
            ".kranz/missions/m-abc123/arbitrary.rs",
            ".kranz/missions/m-abc123/nested/plan.json",
            ".kranz/lessons/index.md",
            ".kranz/lessons/m-abc123.md", // even this mission's lesson is spoofable
            ".kranz/lessons/evil.md",     // ingested into future planning prompts
            ".kranz/lessons/m-other.md",  // another mission's lesson file
            ".kranz/lessons/nested/index.md",
        ] {
            assert!(!is_mission_record_path(mission_id, path), "{path}");
        }
    }

    #[test]
    fn engine_commit_exempt_meta_paths_never_flagged() {
        let mission_id = "m-abc123";
        assert!(is_meta_path(
            mission_id,
            ".kranz/missions/m-abc123/plan.json"
        ));
        assert!(is_meta_path(mission_id, ".kranz/missions/m-abc123/plan.md"));
        assert!(is_meta_path(
            mission_id,
            ".kranz/missions/m-abc123/report.md"
        ));
        assert!(is_meta_path(mission_id, ".kranz/missions/index.md"));
        assert!(!is_meta_path(mission_id, "src/lib.rs"));
    }

    #[test]
    fn engine_commit_exempt_kranz_commit_outside_touch_set_produces_no_finding() {
        let touch_set = vec!["src/**".to_string()];
        let meta_commit = commit("abc123", "[kranz] approved plan for m-abc123");
        let worker_commit = commit("def456", "[f-1-2] add sweep");
        let all_paths = [
            (".kranz/missions/m-abc123/plan.json", &meta_commit),
            ("src/sweep.rs", &worker_commit),
            ("docs/oops.md", &worker_commit),
        ];
        let mission_id = "m-abc123";
        let filtered: Vec<AttributedChange> = all_paths
            .iter()
            .filter(|(_, c)| !is_meta_commit(&c.subject))
            .filter(|(p, _)| !is_meta_path(mission_id, p))
            .map(|(p, c)| AttributedChange { path: p, commit: c })
            .collect();
        let findings = path_findings(&touch_set, &filtered);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].subject, "docs/oops.md");
    }

    // -- primary_checkout ------------------------------------------------------

    #[test]
    fn primary_checkout_dirty_yields_critical_finding() {
        let finding = primary_checkout_finding(false, "main", "main").expect("dirty must flag");
        assert_eq!(finding.severity, "critical");
        assert_eq!(finding.subject, "primary-checkout");
        assert_eq!(finding.class, FINDING_CLASS);
    }

    #[test]
    fn primary_checkout_moved_branch_yields_critical_finding() {
        let finding = primary_checkout_finding(true, "kranz/mission-m-abc123", "main")
            .expect("moved branch must flag");
        assert_eq!(finding.severity, "critical");
        assert!(finding.evidence.contains("main"));
    }

    #[test]
    fn primary_checkout_clean_and_unmoved_yields_no_finding() {
        assert!(primary_checkout_finding(true, "main", "main").is_none());
    }

    #[test]
    fn anti_vacuity_detects_cargo_test_without_guard() {
        assert!(!cargo_test_has_anti_vacuity(
            "cargo test --workspace foo 2>&1 | grep -qE 'test result: ok\\.'"
        ));
        assert!(cargo_test_has_anti_vacuity(
            "cargo test --workspace foo 2>&1 | grep -qE 'test result: ok\\. [1-9]'"
        ));
        assert!(cargo_test_has_anti_vacuity("npm run test"));
    }
}
