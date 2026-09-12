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
use cap_fs_ext::DirExt as _;
use cap_std::{ambient_authority, fs::Dir};
use kranz_engine::error::EngineError;
use kranz_engine::ticket::Ticket;

const MAX_IMPORT_BYTES: u64 = 8 * 1024 * 1024;
const MAX_SPEC_ENTRIES: usize = 1024;
const MAX_SPEC_DEPTH: usize = 32;

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
    read_change_pinned(open_change_root(None, dir)?, dir, slug_override)
}

fn open_change_root(repo: Option<&Path>, dir: &Path) -> Result<Dir> {
    if let Some(repo) = repo {
        let absolute_dir = std::path::absolute(dir)?;
        let canonical_repo = repo.canonicalize()?;
        // Locate the trusted root by identity, not just spelling (/var and
        // /private/var, or a checkout alias, may name the same repository).
        // Canonicalization only locates that anchor; the untrusted suffix is
        // always opened from the pinned repository, never its resolved path.
        if let Some(anchor) = absolute_dir.ancestors().find(|ancestor| {
            ancestor
                .canonicalize()
                .is_ok_and(|path| path == canonical_repo)
        }) {
            let relative = absolute_dir.strip_prefix(anchor)?;
            let mut root = Dir::open_ambient_dir(&canonical_repo, ambient_authority())?;
            for component in relative.components() {
                let std::path::Component::Normal(name) = component else {
                    bail!("import path must stay beneath its repository anchor");
                };
                root = root.open_dir_nofollow(name).with_context(|| {
                    format!(
                        "could not open change directory {} without following links",
                        dir.display()
                    )
                })?;
            }
            return Ok(root);
        }
    }
    // An explicitly selected external change has its own trusted parent.
    // Repository-local imports instead pin every source ancestor above.
    let parent = dir
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = dir
        .file_name()
        .context("change directory must have a name")?;
    Dir::open_ambient_dir(parent, ambient_authority())?
        .open_dir_nofollow(name)
        .with_context(|| {
            format!(
                "could not open change directory {} without following links",
                dir.display()
            )
        })
}

fn read_change_pinned(
    root: Dir,
    dir: &Path,
    slug_override: Option<&str>,
) -> Result<OpenSpecChange> {
    let mut remaining = MAX_IMPORT_BYTES;
    let proposal_path = dir.join("proposal.md");
    let proposal =
        read_text(&root, Path::new("proposal.md"), &mut remaining).with_context(|| {
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

    if let Some(design) = read_optional(&root, Path::new("design.md"), &mut remaining)? {
        context.push_str(&format!("\n### Design notes (design.md)\n\n{design}\n"));
    }

    for (path, spec) in read_specs(&root, &dir.join("specs"), &mut remaining)? {
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
/// Reuses [`Ticket::create_markdown`], so slug validation and the refusal to
/// overwrite an existing ticket stay in one place: an operator who has
/// already edited an imported ticket does not lose that work to a re-import.
pub fn import_change(repo: &Path, dir: &Path, slug_override: Option<&str>) -> Result<PathBuf> {
    let change = read_change_pinned(open_change_root(Some(repo), dir)?, dir, slug_override)?;
    let mut body =
        Ticket::ticket_template(&change.title, Some(&change.goal), Some(&change.context));
    body.push_str(&format!("\n{ACCEPTANCE_PLACEHOLDER}\n"));
    Ok(Ticket::create_markdown(repo, &change.slug, &body)?)
}

fn read_text(dir: &Dir, name: &Path, remaining: &mut u64) -> kranz_engine::error::Result<String> {
    let text = kranz_engine::paths::read_regular_file_under(dir, name, *remaining)?;
    *remaining -= text.len() as u64;
    Ok(text)
}

fn read_optional(dir: &Dir, name: &Path, remaining: &mut u64) -> Result<Option<String>> {
    match read_text(dir, name, remaining) {
        Ok(text) if text.trim().is_empty() => Ok(None),
        Ok(text) => Ok(Some(text.trim().to_string())),
        Err(EngineError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(anyhow::Error::from(error))
            .with_context(|| format!("could not read {}", name.display())),
    }
}

/// Every `.md` under `specs/`, sorted, so an import is reproducible rather
/// than ordered by whatever the filesystem returns.
fn read_specs(
    root: &Dir,
    specs_path: &Path,
    remaining: &mut u64,
) -> Result<Vec<(PathBuf, String)>> {
    let mut found = Vec::new();
    let specs = match root.open_dir_nofollow("specs") {
        Ok(dir) => dir,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(error) => return Err(error).context("could not open specs without following links"),
    };
    let mut remaining_entries = MAX_SPEC_ENTRIES;
    collect_markdown(
        &specs,
        specs_path,
        &mut found,
        remaining,
        &mut remaining_entries,
        0,
    )?;
    found.sort_by(|(left, _), (right, _)| left.cmp(right));
    Ok(found)
}

fn collect_markdown(
    dir: &Dir,
    display_path: &Path,
    out: &mut Vec<(PathBuf, String)>,
    remaining_bytes: &mut u64,
    remaining_entries: &mut usize,
    depth: usize,
) -> Result<()> {
    if depth > MAX_SPEC_DEPTH {
        bail!("OpenSpec specs exceed the directory depth limit ({MAX_SPEC_DEPTH})");
    }
    for entry in dir.entries()? {
        let entry = entry?;
        if *remaining_entries == 0 {
            bail!("OpenSpec specs exceed the entry limit ({MAX_SPEC_ENTRIES})");
        }
        *remaining_entries -= 1;
        let name = PathBuf::from(entry.file_name());
        let path = display_path.join(&name);
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            bail!("refusing linked OpenSpec input {}", path.display());
        }
        if kind.is_dir() {
            let child = dir.open_dir_nofollow(&name).with_context(|| {
                format!("could not open {} without following links", path.display())
            })?;
            collect_markdown(
                &child,
                &path,
                out,
                remaining_bytes,
                remaining_entries,
                depth + 1,
            )?;
        } else if name.extension().is_some_and(|ext| ext == "md") {
            // A removed or unreadable entry is an incomplete import, not an
            // optional file: propagate the error instead of silently omitting it.
            let text = read_text(dir, &name, remaining_bytes)?;
            if !text.trim().is_empty() {
                let text = text.trim().to_owned();
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

    #[cfg(unix)]
    #[test]
    fn imports_refuse_links_at_every_untrusted_component() {
        use std::os::unix::fs::symlink;
        for component in [
            "root",
            "proposal.md",
            "design.md",
            "specs",
            "specs/theme.md",
            "specs/nested",
        ] {
            let repo = tempfile::tempdir().unwrap();
            let dir = write_change(repo.path());
            let outside = tempfile::tempdir().unwrap();
            let sentinel = outside.path().join("sentinel.md");
            std::fs::write(&sentinel, "synthetic outside content").unwrap();
            let selected = if component == "root" {
                let linked = repo.path().join("linked-change");
                symlink(&dir, &linked).unwrap();
                linked
            } else {
                let path = dir.join(component);
                if path.is_dir() {
                    std::fs::remove_dir_all(&path).unwrap();
                } else if path.exists() {
                    std::fs::remove_file(&path).unwrap();
                }
                let target = if component == "specs" || component == "specs/nested" {
                    outside.path()
                } else {
                    &sentinel
                };
                symlink(target, path).unwrap();
                dir
            };
            assert!(
                import_change(repo.path(), &selected, Some("example")).is_err(),
                "{component}"
            );
            assert!(
                !repo.path().join(".kranz/tickets/example.md").exists(),
                "{component}"
            );
            assert_eq!(
                std::fs::read_to_string(&sentinel).unwrap(),
                "synthetic outside content"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn repository_source_ancestors_cannot_redirect_imports() {
        let repo = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let change = write_change(external.path());
        std::os::unix::fs::symlink(
            external.path().join("openspec"),
            repo.path().join("openspec"),
        )
        .unwrap();
        let indirect = repo.path().join("openspec/changes/dark mode");
        assert!(import_change(repo.path(), &indirect, Some("indirect")).is_err());
        assert!(!repo.path().join(".kranz/tickets/indirect.md").exists());
        // Explicitly naming an external change remains supported.
        assert!(import_change(repo.path(), &change, Some("explicit")).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn checkout_alias_does_not_turn_repository_inputs_into_external_authority() {
        let repo = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let aliases = tempfile::tempdir().unwrap();
        write_change(external.path());
        let alias = aliases.path().join("checkout");
        std::os::unix::fs::symlink(repo.path(), &alias).unwrap();
        write_change(repo.path());
        let source = alias.join("openspec/changes/dark mode");
        assert!(import_change(repo.path(), &source, Some("local")).is_ok());
        std::fs::remove_dir_all(repo.path().join("openspec")).unwrap();
        std::os::unix::fs::symlink(
            external.path().join("openspec"),
            repo.path().join("openspec"),
        )
        .unwrap();
        assert!(import_change(repo.path(), &source, Some("indirect")).is_err());
        assert!(!repo.path().join(".kranz/tickets/indirect.md").exists());
    }

    #[test]
    fn imports_bound_aggregate_bytes_and_directory_depth() {
        let repo = tempfile::tempdir().unwrap();
        let dir = write_change(repo.path());
        let file = std::fs::File::create(dir.join("design.md")).unwrap();
        file.set_len(MAX_IMPORT_BYTES).unwrap();
        assert!(
            read_change(&dir, None).is_err(),
            "proposal and design together exceed the budget"
        );
        std::fs::remove_file(dir.join("design.md")).unwrap();
        let mut nested = dir.join("specs");
        for _ in 0..=MAX_SPEC_DEPTH {
            nested.push("nested");
        }
        std::fs::create_dir_all(nested).unwrap();
        assert!(read_change(&dir, None)
            .unwrap_err()
            .to_string()
            .contains("depth limit"));
    }

    #[test]
    fn imports_bound_entry_count_even_for_empty_directories() {
        let repo = tempfile::tempdir().unwrap();
        let dir = write_change(repo.path());
        for index in 0..MAX_SPEC_ENTRIES {
            std::fs::create_dir(dir.join("specs").join(index.to_string())).unwrap();
        }
        assert!(read_change(&dir, None)
            .unwrap_err()
            .to_string()
            .contains("entry limit"));
    }

    #[test]
    fn nested_regular_specs_are_imported_in_path_order() {
        let repo = tempfile::tempdir().unwrap();
        let dir = write_change(repo.path());
        std::fs::create_dir(dir.join("specs/aaa")).unwrap();
        std::fs::write(dir.join("specs/aaa/first.md"), "First nested requirement").unwrap();
        let change = read_change(&dir, None).unwrap();
        assert!(
            change.context.find("First nested requirement").unwrap()
                < change.context.find("SHALL").unwrap()
        );
    }

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
