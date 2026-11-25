//! Milestone tag outcome: moves `kranz/<mission>/<milestone>` to the commit
//! that LAST passed validation, instead of failing the mission when the tag
//! already exists from an earlier completion (ticket: re-completed
//! milestones move their tag).

use crate::git_ops::GitRepo;

/// Result of attempting to name a milestone's commit with its tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MilestoneTagOutcome {
    /// The tag did not exist; it now names `commit`.
    Created { tag: String, commit: String },
    /// The tag existed at a different commit; it now names `commit`,
    /// having previously named `previous`.
    Moved {
        tag: String,
        commit: String,
        previous: String,
    },
    /// The tag already named `commit` (resume-after-crash, or a reopen with
    /// no new commit landing before re-completion).
    AlreadyCurrent { tag: String, commit: String },
    /// A real git failure; the milestone completes untagged. `stale_tag`
    /// names a tag that may still exist (pointing at an earlier
    /// completion) when one was found on re-probe.
    Failed {
        error: String,
        stale_tag: Option<String>,
    },
}

impl MilestoneTagOutcome {
    /// The four `milestone.completed` event fields this outcome produces:
    /// `(tag, tag_commit, retagged_from, stale_tag)`.
    pub fn event_fields(
        &self,
    ) -> (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) {
        match self {
            MilestoneTagOutcome::Created { tag, commit } => {
                (Some(tag.clone()), Some(commit.clone()), None, None)
            }
            MilestoneTagOutcome::Moved {
                tag,
                commit,
                previous,
            } => (
                Some(tag.clone()),
                Some(commit.clone()),
                Some(previous.clone()),
                None,
            ),
            MilestoneTagOutcome::AlreadyCurrent { tag, commit } => {
                (Some(tag.clone()), Some(commit.clone()), None, None)
            }
            MilestoneTagOutcome::Failed { stale_tag, .. } => (None, None, None, stale_tag.clone()),
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, MilestoneTagOutcome::Failed { .. })
    }
}

/// Tag `name` at `HEAD`, moving a pre-existing tag of that name forward
/// rather than failing when the tag already exists. Never panics; any git
/// failure degrades to [`MilestoneTagOutcome::Failed`] so the mission always
/// completes.
pub fn tag_milestone(repo: &GitRepo, name: &str) -> MilestoneTagOutcome {
    let head = match repo.head_sha() {
        Ok(head) => head,
        Err(e) => {
            let error = e.to_string();
            tracing::warn!(tag = %name, error = %error, "milestone tag failed; completing untagged");
            let stale_tag = probe_stale_tag(repo, name, None);
            return MilestoneTagOutcome::Failed { error, stale_tag };
        }
    };

    let existing = match repo.tag_commit(name) {
        Ok(existing) => existing,
        Err(e) => {
            let error = e.to_string();
            tracing::warn!(tag = %name, error = %error, "milestone tag failed; completing untagged");
            let stale_tag = probe_stale_tag(repo, name, Some(&head));
            return MilestoneTagOutcome::Failed { error, stale_tag };
        }
    };

    match existing {
        None => match repo.tag(name, "kranz milestone complete") {
            Ok(()) => MilestoneTagOutcome::Created {
                tag: name.to_string(),
                commit: head,
            },
            Err(e) => {
                let error = e.to_string();
                tracing::warn!(tag = %name, error = %error, "milestone tag failed; completing untagged");
                let stale_tag = probe_stale_tag(repo, name, Some(&head));
                MilestoneTagOutcome::Failed { error, stale_tag }
            }
        },
        Some(existing) if existing == head => {
            tracing::info!(
                tag = %name,
                commit = %head,
                "milestone completed again; tag already names this commit"
            );
            MilestoneTagOutcome::AlreadyCurrent {
                tag: name.to_string(),
                commit: head,
            }
        }
        Some(existing) => {
            let message =
                format!("kranz milestone complete (completed again; previously {existing})");
            match repo.retag(name, &message) {
                Ok(()) => {
                    tracing::info!(
                        tag = %name,
                        from = %existing,
                        to = %head,
                        "milestone completed again; moved tag to the newly validated commit"
                    );
                    MilestoneTagOutcome::Moved {
                        tag: name.to_string(),
                        commit: head,
                        previous: existing,
                    }
                }
                Err(e) => {
                    let error = e.to_string();
                    tracing::warn!(tag = %name, error = %error, "milestone tag failed; completing untagged");
                    let stale_tag = probe_stale_tag(repo, name, Some(&head));
                    MilestoneTagOutcome::Failed { error, stale_tag }
                }
            }
        }
    }
}

/// Re-probe `tag_commit(name)` after a failure to describe whether a stale
/// tag of that name remains. `Ok(None)` is reported as no stale tag; a
/// probe error, or a tag that still exists and does not name `head`, is
/// conservatively reported as stale.
fn probe_stale_tag(repo: &GitRepo, name: &str, head: Option<&str>) -> Option<String> {
    match repo.tag_commit(name) {
        Ok(None) => None,
        Ok(Some(commit)) => {
            if head.is_some_and(|head| commit == head) {
                None
            } else {
                Some(name.to_string())
            }
        }
        Err(_) => Some(name.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo() -> (tempfile::TempDir, GitRepo) {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-b", "main"]);
        git(dir.path(), &["config", "user.name", "kranz-test"]);
        git(dir.path(), &["config", "user.email", "test@kranz.local"]);
        git(
            dir.path(),
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "initial",
            ],
        );
        let repo = GitRepo::open(dir.path()).unwrap();
        (dir, repo)
    }

    fn commit(dir: &Path, message: &str) {
        git(
            dir,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                message,
            ],
        );
    }

    #[test]
    fn created_on_fresh_repo() {
        let (_dir, repo) = init_repo();
        let head = repo.head_sha().unwrap();
        let outcome = tag_milestone(&repo, "kranz/m-1/ms-1");
        assert_eq!(
            outcome,
            MilestoneTagOutcome::Created {
                tag: "kranz/m-1/ms-1".to_string(),
                commit: head,
            }
        );
        assert!(!outcome.is_failure());
    }

    #[test]
    fn already_current_when_called_twice_at_same_head() {
        let (_dir, repo) = init_repo();
        let head = repo.head_sha().unwrap();
        let _ = tag_milestone(&repo, "kranz/m-1/ms-1");
        let outcome = tag_milestone(&repo, "kranz/m-1/ms-1");
        assert_eq!(
            outcome,
            MilestoneTagOutcome::AlreadyCurrent {
                tag: "kranz/m-1/ms-1".to_string(),
                commit: head,
            }
        );
        assert!(!outcome.is_failure());
    }

    #[test]
    fn moved_after_new_commit_names_old_commit_as_previous() {
        let (dir, repo) = init_repo();
        let first_head = repo.head_sha().unwrap();
        let first = tag_milestone(&repo, "kranz/m-1/ms-1");
        assert!(matches!(first, MilestoneTagOutcome::Created { .. }));

        commit(dir.path(), "fix");
        let second_head = repo.head_sha().unwrap();
        assert_ne!(first_head, second_head);

        let outcome = tag_milestone(&repo, "kranz/m-1/ms-1");
        assert_eq!(
            outcome,
            MilestoneTagOutcome::Moved {
                tag: "kranz/m-1/ms-1".to_string(),
                commit: second_head.clone(),
                previous: first_head,
            }
        );
        assert!(!outcome.is_failure());
        assert_eq!(
            repo.tag_commit("kranz/m-1/ms-1").unwrap(),
            Some(second_head)
        );
    }

    #[test]
    fn failed_with_stale_tag_none_on_ref_conflict() {
        let (_dir, repo) = init_repo();
        // Pre-create a tag nested under the name we want, so git cannot
        // create `refs/tags/kranz/m-1/ms-1` (a directory now sits there).
        git(repo.root(), &["tag", "kranz/m-1/ms-1/blocker"]);

        let outcome = tag_milestone(&repo, "kranz/m-1/ms-1");
        match outcome {
            MilestoneTagOutcome::Failed { stale_tag, .. } => assert_eq!(stale_tag, None),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(tag_milestone(&repo, "kranz/m-1/ms-1").is_failure());
    }

    #[test]
    fn failed_with_stale_tag_some_when_move_is_blocked() {
        let (dir, repo) = init_repo();
        let created = tag_milestone(&repo, "kranz/m-1/ms-1");
        assert!(matches!(created, MilestoneTagOutcome::Created { .. }));

        commit(dir.path(), "fix");

        // Lock the tag ref so `git tag -f` fails while the old tag still
        // exists.
        let lock = dir
            .path()
            .join(".git/refs/tags/kranz/m-1/ms-1.lock");
        std::fs::create_dir_all(lock.parent().unwrap()).unwrap();
        std::fs::write(&lock, "").unwrap();

        let outcome = tag_milestone(&repo, "kranz/m-1/ms-1");
        assert!(outcome.is_failure());
        match outcome {
            MilestoneTagOutcome::Failed { stale_tag, .. } => {
                assert_eq!(stale_tag, Some("kranz/m-1/ms-1".to_string()));
            }
            other => panic!("expected Failed, got {other:?}"),
        }

        std::fs::remove_file(&lock).unwrap();
    }

    #[test]
    fn is_failure_only_true_for_failed() {
        assert!(!MilestoneTagOutcome::Created {
            tag: "t".into(),
            commit: "c".into(),
        }
        .is_failure());
        assert!(!MilestoneTagOutcome::Moved {
            tag: "t".into(),
            commit: "c".into(),
            previous: "p".into(),
        }
        .is_failure());
        assert!(!MilestoneTagOutcome::AlreadyCurrent {
            tag: "t".into(),
            commit: "c".into(),
        }
        .is_failure());
        assert!(MilestoneTagOutcome::Failed {
            error: "e".into(),
            stale_tag: None,
        }
        .is_failure());
    }
}
