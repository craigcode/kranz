//! Review-artifact missions (KRZ-349, design D-G/D-K).
//!
//! `spec-review` and `incident-review` are ordinary mission task classes.
//! This module adds only their artifact contract: one tracked, immutable text
//! input and one non-empty changed text output. Planning, rule resolution,
//! findings, waivers, checker outcomes, replay, and evidence continue through
//! the existing mission types; there is deliberately no review-result schema.

use crate::error::{EngineError, Result};
use crate::git_ops::GitRepo;
use crate::types::Finding;
use std::collections::BTreeMap;
use std::path::{Component, Path};

pub const SPEC_REVIEW: &str = "spec-review";
pub const INCIDENT_REVIEW: &str = "incident-review";
pub const REVIEW_ARTIFACT_HEADING: &str = "## Review artifact\n";
const MAX_ARTIFACT_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewArtifactContract {
    pub task_class: String,
    pub input_path: String,
    pub output_path: String,
}

pub fn is_review_task_class(task_class: Option<&str>) -> bool {
    task_class
        .map(crate::routing::normalize_task_class)
        .is_some_and(|class| matches!(class.as_str(), SPEC_REVIEW | INCIDENT_REVIEW))
}

fn validate_path(slug: &str, field: &str, raw: &str) -> Result<String> {
    let path = raw.trim();
    if path.is_empty()
        || path.starts_with('-')
        || path.contains('\\')
        || path.contains('\0')
        || path.contains("//")
        || path.ends_with('/')
        || path.chars().any(char::is_control)
        || path
            .chars()
            .any(|ch| matches!(ch, '*' | '?' | '[' | ']' | '{' | '}'))
    {
        return Err(EngineError::Config(format!(
            "ticket {slug}: {field} must be one literal repo-relative slash path"
        )));
    }
    let parsed = Path::new(path);
    if parsed.is_absolute()
        || path
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(EngineError::Config(format!(
            "ticket {slug}: {field} must not contain '.', '..', a root, or a platform prefix"
        )));
    }
    let first = path.split('/').next().unwrap_or_default();
    if first.eq_ignore_ascii_case(".git") || first.eq_ignore_ascii_case(".kranz") {
        return Err(EngineError::Config(format!(
            "ticket {slug}: {field} may not target the reserved {first}/ tree"
        )));
    }
    Ok(path.to_string())
}

/// Validate ticket frontmatter and derive the deterministic output path.
/// Non-review tickets remain byte-for-byte unaffected.
pub fn from_ticket_fields(
    slug: &str,
    task_class: Option<&str>,
    input: Option<&str>,
    output: Option<&str>,
) -> Result<Option<ReviewArtifactContract>> {
    if !is_review_task_class(task_class) {
        if input.is_some() || output.is_some() {
            return Err(EngineError::Config(format!(
                "ticket {slug}: review-artifact/review-output require task-class: {SPEC_REVIEW} or {INCIDENT_REVIEW}"
            )));
        }
        return Ok(None);
    }
    let task_class = crate::routing::normalize_task_class(task_class.unwrap_or_default());
    let input = input.ok_or_else(|| {
        EngineError::Config(format!(
            "ticket {slug}: task-class {task_class} requires review-artifact: <tracked-path>"
        ))
    })?;
    let input_path = validate_path(slug, "review-artifact", input)?;
    let default_output = format!("reviews/{slug}.md");
    let output_path = validate_path(
        slug,
        "review-output",
        output.unwrap_or(default_output.as_str()),
    )?;
    if input_path == output_path {
        return Err(EngineError::Config(format!(
            "ticket {slug}: review-output must differ from the immutable review-artifact input"
        )));
    }
    Ok(Some(ReviewArtifactContract {
        task_class,
        input_path,
        output_path,
    }))
}

pub fn render_goal_section(contract: &ReviewArtifactContract) -> String {
    format!(
        "\n{REVIEW_ARTIFACT_HEADING}task-class: {}\ninput: {}\noutput: {}\nsource-policy: read-only; any committed touch is a final-gate failure\ndeliverable-policy: output must be a changed, non-empty regular UTF-8 file\n",
        contract.task_class, contract.input_path, contract.output_path
    )
}

/// Recover the engine-authored appendix from a folded mission goal. Raw API
/// callers using a review task class must supply the same explicit contract.
pub fn parse_from_goal(goal: &str) -> Result<Option<ReviewArtifactContract>> {
    let task_class = crate::ticket::parse_task_class_from_goal(goal);
    let review_class = is_review_task_class(task_class.as_deref());
    let Some(index) = goal.rfind(REVIEW_ARTIFACT_HEADING) else {
        return if review_class {
            Err(EngineError::Config(format!(
                "task-class {} requires an engine review-artifact contract",
                task_class.unwrap_or_default()
            )))
        } else {
            Ok(None)
        };
    };
    if !review_class {
        return Err(EngineError::Config(
            "a Review artifact appendix requires task-class: spec-review or incident-review"
                .to_string(),
        ));
    }

    let section = &goal[index + REVIEW_ARTIFACT_HEADING.len()..];
    let section = section.split("\n## ").next().unwrap_or(section);
    let mut fields = BTreeMap::new();
    for line in section.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if !matches!(key, "task-class" | "input" | "output") {
            continue;
        }
        if fields.insert(key, value.trim()).is_some() {
            return Err(EngineError::Config(format!(
                "review-artifact contract repeats field {key}"
            )));
        }
    }
    let declared_class = fields.get("task-class").copied().ok_or_else(|| {
        EngineError::Config("review-artifact contract is missing task-class".to_string())
    })?;
    let output = fields.get("output").copied().ok_or_else(|| {
        EngineError::Config("review-artifact contract is missing output".to_string())
    })?;
    if crate::routing::normalize_task_class(declared_class)
        != crate::routing::normalize_task_class(task_class.as_deref().unwrap_or_default())
    {
        return Err(EngineError::Config(
            "review-artifact contract task-class does not match the mission task class".to_string(),
        ));
    }
    from_ticket_fields(
        "mission-goal",
        task_class.as_deref(),
        fields.get("input").copied(),
        Some(output),
    )
}

/// Fail before mission creation unless the source is a bounded, non-empty,
/// regular UTF-8 blob on the base branch. Git-tree reads avoid worktree
/// symlinks and make the reviewed bytes independent of local dirty state.
pub fn validate_source(
    repo: &GitRepo,
    base_ref: &str,
    contract: &ReviewArtifactContract,
) -> Result<()> {
    let base_ref = repo.rev_parse(base_ref)?;
    let entry = repo
        .ls_tree_recursive(&base_ref, &contract.input_path)?
        .into_iter()
        .find(|entry| entry.path == contract.input_path)
        .ok_or_else(|| {
            EngineError::Config(format!(
                "review artifact `{}` is not tracked on base `{base_ref}`",
                contract.input_path
            ))
        })?;
    if entry.kind != "blob" || !matches!(entry.mode.as_str(), "100644" | "100755") {
        return Err(EngineError::Config(format!(
            "review artifact `{}` must be a regular tracked file, not mode {} kind {}",
            contract.input_path, entry.mode, entry.kind
        )));
    }
    let size = entry.size.unwrap_or(u64::MAX);
    if size == 0 || size > MAX_ARTIFACT_BYTES {
        return Err(EngineError::Config(format!(
            "review artifact `{}` is {size} bytes; expected 1..={MAX_ARTIFACT_BYTES}",
            contract.input_path
        )));
    }
    let bytes = repo
        .show_file(&base_ref, &contract.input_path)?
        .ok_or_else(|| EngineError::Config("review artifact disappeared from base".to_string()))?;
    let text = std::str::from_utf8(&bytes).map_err(|_| {
        EngineError::Config(format!(
            "review artifact `{}` must be UTF-8 text",
            contract.input_path
        ))
    })?;
    if text.trim().is_empty() {
        return Err(EngineError::Config(format!(
            "review artifact `{}` is blank; refusing a vacuous review",
            contract.input_path
        )));
    }
    Ok(())
}

fn finding(subject: &str, evidence: String, suggested_fix: &str) -> Finding {
    Finding {
        subject: subject.to_string(),
        severity: "critical".to_string(),
        evidence,
        suggested_fix: suggested_fix.to_string(),
        class: "command-assertion".to_string(),
        rule: None,
    }
}

/// Final-gate artifact contract. `changed_paths` is the union over every
/// non-meta deliverable commit, so edit→restore of the source still fails.
///
/// Source immutability is decided on BYTES, not only on the path list
/// (audit H8): git's rename detection reports `git mv docs/spec.md
/// reviews/api.md` as the destination alone, so the source path is absent
/// from `changed_paths` while the source itself is gone at head. Comparing
/// the blob at base with the blob at head catches the rename, the delete,
/// and a mode change; the path-list check stays because it also catches a
/// path git chose not to pair.
pub fn deliverable_findings(
    repo: &GitRepo,
    base_ref: &str,
    head_ref: &str,
    changed_paths: &[String],
    contract: &ReviewArtifactContract,
) -> Result<Vec<Finding>> {
    let base_ref = repo.rev_parse(base_ref)?;
    let head_ref = repo.rev_parse(head_ref)?;
    let mut findings = Vec::new();
    let source_in_changed_paths = changed_paths
        .iter()
        .any(|path| path == &contract.input_path);
    let base_source = repo.show_file(&base_ref, &contract.input_path)?;
    let head_source = repo.show_file(&head_ref, &contract.input_path)?;
    let source_bytes_differ = base_source != head_source;
    if source_in_changed_paths || source_bytes_differ {
        let detail = if head_source.is_none() {
            "is gone at mission HEAD (deleted or renamed away)"
        } else if source_bytes_differ {
            "differs from its bytes on the approved base"
        } else {
            "was touched by a deliverable commit"
        };
        findings.push(finding(
            "review-artifact:source",
            format!("immutable review source `{}` {detail}", contract.input_path),
            "restore the source artifact exactly and keep the review in the declared output",
        ));
    }

    let mut output_errors = Vec::new();
    if !changed_paths
        .iter()
        .any(|path| path == &contract.output_path)
    {
        output_errors.push("no deliverable commit touched the declared output".to_string());
    }
    let entry = repo
        .ls_tree_recursive(&head_ref, &contract.output_path)?
        .into_iter()
        .find(|entry| entry.path == contract.output_path);
    match entry {
        Some(entry)
            if entry.kind == "blob" && matches!(entry.mode.as_str(), "100644" | "100755") =>
        {
            let size = entry.size.unwrap_or(u64::MAX);
            if size == 0 || size > MAX_ARTIFACT_BYTES {
                output_errors.push(format!(
                    "output size is {size} bytes; expected 1..={MAX_ARTIFACT_BYTES}"
                ));
            }
            match repo.show_file(&head_ref, &contract.output_path)? {
                Some(bytes) => {
                    match std::str::from_utf8(&bytes) {
                        Ok(text) if text.trim().is_empty() => {
                            output_errors.push("output is blank".to_string())
                        }
                        Err(_) => output_errors.push("output is not UTF-8 text".to_string()),
                        Ok(_) => {}
                    }
                    if repo.show_file(&base_ref, &contract.output_path)?.as_deref() == Some(&bytes)
                    {
                        output_errors
                            .push("output bytes are unchanged from the approved base".to_string());
                    }
                }
                None => output_errors.push("output disappeared from mission HEAD".to_string()),
            }
        }
        Some(entry) => output_errors.push(format!(
            "output is mode {} kind {}, not a regular file",
            entry.mode, entry.kind
        )),
        None => output_errors.push("output does not exist at mission HEAD".to_string()),
    }
    if !output_errors.is_empty() {
        findings.push(finding(
            "review-artifact:output",
            format!(
                "review output `{}` is not a valid deliverable: {}",
                contract.output_path,
                output_errors.join("; ")
            ),
            "write a substantive review to the declared output path and commit it",
        ));
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(root: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .env("GIT_AUTHOR_NAME", "kranz-test")
            .env("GIT_AUTHOR_EMAIL", "kranz-test@localhost")
            .env("GIT_COMMITTER_NAME", "kranz-test")
            .env("GIT_COMMITTER_EMAIL", "kranz-test@localhost")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn flight_rules_review_class_contract_round_trips_without_parallel_schema() {
        let contract =
            from_ticket_fields("api-spec", Some(" Spec-Review "), Some("docs/api.md"), None)
                .unwrap()
                .unwrap();
        assert_eq!(contract.output_path, "reviews/api-spec.md");
        let goal = format!(
            "Review the API.{}\n## Task class\nspec-review\n",
            render_goal_section(&contract)
        );
        assert_eq!(parse_from_goal(&goal).unwrap(), Some(contract));
    }

    #[test]
    fn flight_rules_review_class_rejects_missing_or_aliased_artifacts() {
        assert!(from_ticket_fields("x", Some(SPEC_REVIEW), None, None).is_err());
        assert!(
            from_ticket_fields("x", Some(INCIDENT_REVIEW), Some("../incident.md"), None).is_err()
        );
        assert!(from_ticket_fields(
            "x",
            Some(SPEC_REVIEW),
            Some("docs/spec.md"),
            Some("docs/spec.md")
        )
        .is_err());
        assert!(from_ticket_fields("x", Some(SPEC_REVIEW), Some(".GIT/config"), None).is_err());
        assert!(parse_from_goal(
            "review\n\n## Review artifact\ntask-class: spec-review\ninput: docs/spec.md\n\n## Task class\nspec-review\n"
        )
        .is_err());
    }

    #[test]
    fn flight_rules_review_class_deliverable_is_nonempty_and_source_is_immutable() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(root.join("docs")).unwrap();
        std::fs::write(root.join("docs/spec.md"), "# API spec\n").unwrap();
        git(root, &["add", "docs/spec.md"]);
        git(root, &["commit", "-q", "-m", "base"]);

        let repo = GitRepo::open(root).unwrap();
        let contract = from_ticket_fields(
            "api",
            Some(SPEC_REVIEW),
            Some("docs/spec.md"),
            Some("reviews/api.md"),
        )
        .unwrap()
        .unwrap();
        validate_source(&repo, "main", &contract).unwrap();
        let base = repo.head_sha().unwrap();

        std::fs::create_dir_all(root.join("reviews")).unwrap();
        std::fs::write(
            root.join("reviews/api.md"),
            "# Review\n\n- Finding cites ZZ-SPEC-001 r1.\n",
        )
        .unwrap();
        git(root, &["add", "reviews/api.md"]);
        git(root, &["commit", "-q", "-m", "review"]);
        assert!(deliverable_findings(
            &repo,
            &base,
            "HEAD",
            &["reviews/api.md".to_string()],
            &contract,
        )
        .unwrap()
        .is_empty());

        let findings = deliverable_findings(
            &repo,
            &base,
            "HEAD",
            &["docs/spec.md".to_string(), "reviews/api.md".to_string()],
            &contract,
        )
        .unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].subject, "review-artifact:source");

        let missing = ReviewArtifactContract {
            output_path: "reviews/missing.md".to_string(),
            ..contract
        };
        let findings = deliverable_findings(&repo, &base, "HEAD", &[], &missing).unwrap();
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].subject, "review-artifact:output");
    }

    /// Audit H8: `git mv <source> <output>` makes the immutable source
    /// DISAPPEAR at head while git's rename detection reports only the
    /// destination, so the path-string match sees a clean deliverable. The
    /// contract is about bytes, so the check compares them.
    #[test]
    fn flight_rules_review_class_source_rename_is_a_source_touch() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::create_dir_all(root.join("docs")).unwrap();
        // Big enough that appending a few lines keeps similarity above
        // git's rename threshold, which is the case the path-list check
        // misses.
        let mut spec = String::from("# API spec\n\n");
        for n in 1..=40 {
            spec.push_str(&format!("r{n}: requirement number {n} holds.\n"));
        }
        std::fs::write(root.join("docs/spec.md"), &spec).unwrap();
        git(root, &["add", "docs/spec.md"]);
        git(root, &["commit", "-q", "-m", "base"]);

        let repo = GitRepo::open(root).unwrap();
        let contract = from_ticket_fields(
            "api",
            Some(SPEC_REVIEW),
            Some("docs/spec.md"),
            Some("reviews/api.md"),
        )
        .unwrap()
        .unwrap();
        let base = repo.head_sha().unwrap();

        // The worker renames the source onto the review output and appends,
        // so git pairs the commit as a rename.
        std::fs::create_dir_all(root.join("reviews")).unwrap();
        git(root, &["mv", "docs/spec.md", "reviews/api.md"]);
        let mut moved = std::fs::read_to_string(root.join("reviews/api.md")).unwrap();
        moved.push_str("\n## Review\n\n- Finding cites ZZ-SPEC-001 r1.\n");
        std::fs::write(root.join("reviews/api.md"), moved).unwrap();
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "review"]);

        // What `git diff --name-only` reports for that commit: the
        // destination only. The fixture asserts it, so the test cannot pass
        // for the wrong reason if git's rename behaviour changes.
        let changed = repo.changed_paths(&base, "HEAD").unwrap();
        assert_eq!(changed, vec!["reviews/api.md".to_string()]);

        let findings = deliverable_findings(&repo, &base, "HEAD", &changed, &contract).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.subject == "review-artifact:source"),
            "a renamed-away source must fail the immutability check: {findings:?}"
        );
    }
}
