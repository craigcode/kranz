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

/// Commit message prefix used for engine/meta commits (approved-plan,
/// mission-report). Never attributed to a worker, never flagged.
const ENGINE_COMMIT_PREFIX: &str = "[kranz]";

/// Whether `subject` is an engine/meta commit message (`"[kranz] ..."`).
pub fn is_meta_commit(subject: &str) -> bool {
    subject.starts_with(ENGINE_COMMIT_PREFIX)
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
/// [`is_meta_commit`]) against the mission's declared touch-set.
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
                evidence: format!("touch-set glob compile error while checking {}: {e}", change.path),
                suggested_fix: "fix the mission's touchSet glob syntax".to_string(),
                class: FINDING_CLASS.to_string(),
            }),
        }
    }
    findings
}

/// Build the critical `primary-checkout` finding when the primary checkout
/// is dirty and/or has moved off the branch recorded at mission start.
/// Returns `None` when the primary is clean and unmoved.
pub fn primary_checkout_finding(
    is_clean: bool,
    current_branch: &str,
    branch_at_start: &str,
) -> Option<Finding> {
    let mut issues = Vec::new();
    if !is_clean {
        issues.push(
            "primary checkout has uncommitted changes (git status --porcelain is non-empty)"
                .to_string(),
        );
    }
    if current_branch != branch_at_start {
        issues.push(format!(
            "primary checkout branch moved from '{branch_at_start}' (recorded at mission start) to '{current_branch}'"
        ));
    }
    if issues.is_empty() {
        return None;
    }
    Some(Finding {
        subject: "primary-checkout".to_string(),
        severity: "critical".to_string(),
        evidence: issues.join("; "),
        suggested_fix:
            "worker/validator sessions must run in the mission's integration worktree, never the primary checkout"
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
        let c = commit("abc123", "[f-1] add widget");
        let changes = vec![AttributedChange {
            path: "docs/random.md",
            commit: &c,
        }];
        let findings = path_findings(&touch_set, &changes);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].class, FINDING_CLASS);
        assert_eq!(findings[0].subject, "docs/random.md");
        assert_eq!(findings[0].severity, "major");
    }

    #[test]
    fn out_of_contract_path_inside_touch_set_produces_no_finding() {
        let touch_set = vec!["src/**".to_string()];
        let c = commit("abc123", "[f-1] add widget");
        let changes = vec![AttributedChange {
            path: "src/widget.rs",
            commit: &c,
        }];
        let findings = path_findings(&touch_set, &changes);
        assert!(findings.is_empty());
    }

    #[test]
    fn out_of_contract_negated_glob_excludes_from_touch_set() {
        let touch_set = vec!["src/**".to_string(), "!src/generated/**".to_string()];
        let c = commit("abc123", "[f-1] add widget");
        let changes = vec![AttributedChange {
            path: "src/generated/schema.rs",
            commit: &c,
        }];
        let findings = path_findings(&touch_set, &changes);
        assert_eq!(
            findings.len(),
            1,
            "negated path must be flagged as out-of-contract"
        );
    }

    #[test]
    fn out_of_contract_duplicate_path_produces_exactly_one_finding() {
        let touch_set = vec!["src/**".to_string()];
        let c1 = commit("abc123", "[f-1] add widget");
        let c2 = commit("def456", "[f-1] tweak widget");
        let changes = vec![
            AttributedChange {
                path: "docs/random.md",
                commit: &c1,
            },
            AttributedChange {
                path: "docs/random.md",
                commit: &c2,
            },
        ];
        let findings = path_findings(&touch_set, &changes);
        assert_eq!(findings.len(), 1);
    }

    #[test]
    fn out_of_contract_empty_touch_set_is_advisory_off() {
        // Caller-level contract: an empty touch_set means the path sweep is
        // skipped entirely (no findings emitted), checked by the orchestrator
        // before calling path_findings. Encode the "would-be" match here too:
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
        assert!(!is_meta_commit("[f-1-2] add sweep"));
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
        // The orchestrator filters meta commits/paths BEFORE calling
        // path_findings; this test proves that filtering, end to end.
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
}
