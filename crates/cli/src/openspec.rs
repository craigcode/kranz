//! Import an OpenSpec change folder as a kranz ticket.
//!
//! OpenSpec (github.com/Fission-AI/OpenSpec) authors each change as
//! `openspec/changes/<name>/` holding `proposal.md` (rationale and scope),
//! `specs/` (requirements as SHALL-style scenarios), `design.md` (technical
//! approach), and `tasks.md` (an implementation checklist). Its README states
//! that it deliberately avoids phase gates and does not enforce a workflow.
//!
//! That non-goal is kranz's goal, so the two compose: OpenSpec produces
//! intent, kranz makes intent binding. This importer is the seam, and it runs
//! ONE WAY. `openspec/changes/` explains why work exists; the approved plan is
//! what the validator judges. Syncing back would leave two sources of truth to
//! drift the moment someone edits a spec mid-mission.
//!
//! Two mappings carry the whole risk, and both are deliberate omissions.
//!
//! `tasks.md` is not imported. It is the assistant's own decomposition,
//! self-reported and ungraded, and importing it would slip an unvalidated plan
//! past the orchestrator — the one step that should be doing that thinking.
//!
//! Spec scenarios never become acceptance hints. `docs/tickets.md` requires
//! hints to be concrete and testable with a passed-count guard, because a bare
//! test-name filter exits 0 on zero matches. "The app SHALL default to the
//! system preference" cannot fail, so copying scenarios into hints would ship
//! a vacuous assertion with every imported mission. They arrive as intent
//! instead, and the orchestrator still has to propose commands that can fail.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use kranz_engine::ticket::Ticket;

/// The parts of an OpenSpec change that survive the crossing.
#[derive(Debug, PartialEq, Eq)]
pub struct OpenSpecChange {
    /// Ticket slug, from the change directory's name unless overridden.
    pub slug: String,
    /// Ticket title, from the proposal's first heading, else the directory.
    pub title: String,
    /// `## Goal` body: the proposal, minus its own title heading.
    pub goal: String,
    /// `## Context` body: design notes, requirements as intent, provenance.
    pub context: String,
}

/// What the generated `## Acceptance hints` says instead of scenarios.
///
/// An imported ticket must not look finished when its criteria are still
/// prose. This states the gap in the artifact itself, where the orchestrator
/// and a human reviewer both see it.
pub const ACCEPTANCE_PLACEHOLDER: &str = "\
The requirements imported from this change are INTENT, not criteria: a SHALL \
sentence cannot fail, so none of them were copied here. Replace this with \
commands that can fail, each guarded by a passed count \
(`grep -qE 'result: ok\\. [1-9][0-9]* passed'`) so a zero-match filter cannot \
report success.";

/// Read an OpenSpec change directory.
///
/// `proposal.md` is required. A change folder without one is either not an
/// OpenSpec change or is half-written, and guessing which would import an
/// empty goal.
pub fn read_change(dir: &Path, slug_override: Option<&str>) -> Result<OpenSpecChange> {
    let proposal_path = dir.join("proposal.md");
    let proposal = std::fs::read_to_string(&proposal_path).with_context(|| {
        format!(
            "no OpenSpec proposal at {}; an OpenSpec change directory holds proposal.md \
             (plus optional design.md, specs/, tasks.md)",
            proposal_path.display()
        )
    })?;

    let dir_name = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if dir_name.is_empty() {
        bail!("could not read a change name from {}", dir.display());
    }
    let slug = slug_override
        .map(str::to_string)
        .unwrap_or_else(|| slugify(&dir_name));

    let (heading, body) = split_leading_heading(&proposal);
    let title = heading.unwrap_or_else(|| dir_name.clone());

    let mut context = String::new();
    context.push_str(&format!(
        "Imported from the OpenSpec change `{}`. The proposal and requirements below are \
         the authored intent; this ticket is what kranz executes, and the approved plan is \
         what the validator judges. `tasks.md` was deliberately not imported: it is the \
         authoring assistant's own decomposition, and the orchestrator re-plans this work \
         rather than inheriting an ungraded checklist.\n",
        dir.display()
    ));

    if let Some(design) = read_optional(&dir.join("design.md"))? {
        context.push_str(&format!("\n### Design notes (design.md)\n\n{design}\n"));
    }

    for (path, spec) in read_specs(&dir.join("specs"))? {
        context.push_str(&format!(
            "\n### Requirements as intent ({})\n\n{spec}\n",
            path.display()
        ));
    }

    Ok(OpenSpecChange {
        slug,
        title,
        goal: body.trim().to_string(),
        context: context.trim_end().to_string(),
    })
}

/// Import a change directory as `.kranz/tickets/<slug>.md`.
///
/// Reuses [`Ticket::scaffold`], so slug validation and the refusal to
/// overwrite an existing ticket stay in one place: an operator who has
/// already edited an imported ticket does not lose that work to a re-import.
pub fn import_change(repo: &Path, dir: &Path, slug_override: Option<&str>) -> Result<PathBuf> {
    let change = read_change(dir, slug_override)?;
    let path = Ticket::scaffold(
        repo,
        &change.slug,
        &change.title,
        Some(&change.goal),
        Some(&change.context),
    )?;
    let scaffolded = std::fs::read_to_string(&path)?;
    let with_hints = scaffolded.replace(
        "## Acceptance hints\n",
        &format!("## Acceptance hints\n\n{ACCEPTANCE_PLACEHOLDER}\n"),
    );
    std::fs::write(&path, with_hints)?;
    Ok(path)
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => Ok(Some(text.trim().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(anyhow::Error::from(error))
            .with_context(|| format!("could not read {}", path.display())),
    }
}

/// Every `.md` under `specs/`, sorted, so an import is reproducible rather
/// than ordered by whatever the filesystem returns.
fn read_specs(specs_dir: &Path) -> Result<Vec<(PathBuf, String)>> {
    let mut found = Vec::new();
    collect_markdown(specs_dir, &mut found)?;
    found.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(found)
}

fn collect_markdown(dir: &Path, out: &mut Vec<(PathBuf, String)>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(anyhow::Error::from(error))
                .with_context(|| format!("could not read {}", dir.display()));
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
            if let Some(text) = read_optional(&path)? {
                out.push((path, text));
            }
        }
    }
    Ok(())
}

/// Split a leading `# Heading` from the body it titles.
fn split_leading_heading(markdown: &str) -> (Option<String>, String) {
    let trimmed = markdown.trim_start();
    let Some(rest) = trimmed.strip_prefix("# ") else {
        return (None, markdown.to_string());
    };
    let (heading, body) = rest.split_once('\n').unwrap_or((rest, ""));
    (Some(heading.trim().to_string()), body.to_string())
}

/// Ticket slugs allow ascii alphanumerics, `-`, `_`, and `.`; a change name
/// can carry anything a directory can.
fn slugify(name: &str) -> String {
    let slug: String = name
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    slug.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_change(root: &Path) -> PathBuf {
        let dir = root.join("openspec").join("changes").join("dark mode");
        std::fs::create_dir_all(dir.join("specs")).unwrap();
        std::fs::write(
            dir.join("proposal.md"),
            "# Add a dark theme\n\nUsers on night shift ask for it weekly.\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("design.md"),
            "A CSS custom property per surface colour.\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("specs").join("theme.md"),
            "The app SHALL default to the system preference.\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("tasks.md"),
            "- [x] Add the toggle\n- [ ] Ship it\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn import_carries_proposal_and_specs_but_never_the_task_checklist() {
        let repo = tempfile::tempdir().unwrap();
        let dir = write_change(repo.path());

        let path = import_change(repo.path(), &dir, None).unwrap();
        let ticket = std::fs::read_to_string(&path).unwrap();

        assert!(path.ends_with(".kranz/tickets/dark-mode.md"), "{path:?}");
        assert!(ticket.contains("title: Add a dark theme"), "{ticket}");
        assert!(ticket.contains("night shift"), "{ticket}");
        assert!(ticket.contains("CSS custom property"), "{ticket}");
        assert!(
            ticket.contains("SHALL default to the system preference"),
            "{ticket}"
        );

        // The checklist is the authoring assistant's own decomposition. If it
        // arrived here the orchestrator would inherit an ungraded plan.
        assert!(!ticket.contains("Add the toggle"), "{ticket}");
        assert!(!ticket.contains("Ship it"), "{ticket}");
    }

    #[test]
    fn acceptance_hints_refuse_to_pass_off_scenarios_as_criteria() {
        let repo = tempfile::tempdir().unwrap();
        let dir = write_change(repo.path());

        let path = import_change(repo.path(), &dir, None).unwrap();
        let ticket = std::fs::read_to_string(&path).unwrap();
        let hints = ticket
            .split_once("## Acceptance hints")
            .expect("the template carries an acceptance section")
            .1;

        // The scenario is present as intent, but not as a criterion: a SHALL
        // sentence cannot fail, and a hint that cannot fail is a vacuous pass.
        // (The placeholder itself says the word "SHALL" while explaining why,
        // so assert on the imported sentence rather than the word.)
        assert!(
            !hints.contains("default to the system preference"),
            "{hints}"
        );
        assert!(hints.contains("cannot fail"), "{hints}");
        assert!(hints.contains("[1-9][0-9]* passed"), "{hints}");
    }

    #[test]
    fn a_directory_without_a_proposal_fails_closed_naming_the_path() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().join("openspec").join("changes").join("empty");
        std::fs::create_dir_all(&dir).unwrap();

        let error = import_change(repo.path(), &dir, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("proposal.md"), "{error}");
        assert!(error.contains(&dir.display().to_string()), "{error}");
    }

    #[test]
    fn re_importing_refuses_rather_than_overwriting_operator_edits() {
        let repo = tempfile::tempdir().unwrap();
        let dir = write_change(repo.path());
        let path = import_change(repo.path(), &dir, None).unwrap();
        std::fs::write(&path, "operator rewrote this ticket").unwrap();

        let error = import_change(repo.path(), &dir, None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "operator rewrote this ticket"
        );
    }
}
