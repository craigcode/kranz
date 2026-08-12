//! The one-time fold of `.status` sidecars into committed frontmatter
//! `state:` keys (design: ticket-state-frontmatter, rule 4) — `kranz ticket
//! migrate-state`.
//!
//! [`fold_sidecar_states`] walks `.kranz/tickets/` and, for every ticket whose
//! gitignored `.status` sidecar records the terminal pipeline state `done`,
//! folds that verdict into the committed .md as `state: done` (carrying the
//! sidecar note into `state-note:`) via [`Ticket::write_lifecycle`] — the one
//! write path that also refreshes the sidecar cache. Dry-run by default: the
//! same function with `apply: false` reports what it WOULD do without writing
//! a byte, so the operator reviews the fold before applying it.
//!
//! Three classes of ticket are never rewritten:
//! - a ticket whose .md is DIRTY in git (uncommitted modification, or
//!   untracked): an in-flight editor or agent may have the file open, and
//!   rewriting it is the house-rule violation this command exists once to
//!   perform — the skip is reported by name so the operator can fold the
//!   ticket after that work lands. The dirty check is fail-closed: no git,
//!   no fold.
//! - a ticket that already carries a `state:` key — this is what makes a
//!   re-run idempotent (the frontmatter is the source of truth; the fold
//!   only fills ABSENT keys, it never overwrites an operator's verdict);
//! - a ticket with no sidecar, or whose sidecar holds a non-terminal
//!   PIPELINE state (drafting/review/queued/failed/…): the frontmatter
//!   domain has no value for pipeline states — they stay sidecar-owned by
//!   design, and a fresh-clone re-read of an in-flight ticket as NEW is the
//!   same behavior the pipeline always had.

use crate::error::{EngineError, Result};
use crate::git_ops::GitRepo;
use crate::ticket::{Ticket, TicketLifecycle, TicketState};
use std::collections::HashSet;
use std::path::Path;

/// One ticket's fold decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoldAction {
    /// Sidecar `done` → frontmatter `state: done`, carrying the sidecar note
    /// into `state-note:` when present (the note is carried, never parsed —
    /// "Superseded by …" prose stays prose).
    Fold { slug: String, note: Option<String> },
    /// Already carries a frontmatter `state:` key — untouched (this is what
    /// makes a re-run idempotent).
    AlreadyMigrated { slug: String },
    /// The .md is dirty in git: never rewrite a file an in-flight editor or
    /// agent has open. Named loudly in the report; the operator folds it by
    /// re-running once the in-flight work lands.
    SkipDirty { slug: String },
    /// Nothing terminal to fold: no sidecar at all (`None`), or a
    /// non-terminal pipeline sidecar — pipeline states are not operator
    /// lifecycle and remain sidecar-owned by design.
    NoTerminalSidecar {
        slug: String,
        sidecar: Option<TicketState>,
    },
}

impl FoldAction {
    fn slug(&self) -> &str {
        match self {
            FoldAction::Fold { slug, .. }
            | FoldAction::AlreadyMigrated { slug }
            | FoldAction::SkipDirty { slug }
            | FoldAction::NoTerminalSidecar { slug, .. } => slug,
        }
    }
}

/// The fold plan (dry-run) or record (applied) over one repo's tickets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    /// `false` = dry-run: the actions are what WOULD happen; no byte was
    /// written. `true` = the [`FoldAction::Fold`] entries were applied.
    pub applied: bool,
    /// Per-ticket decisions, sorted by slug for a deterministic report.
    pub actions: Vec<FoldAction>,
}

impl MigrationReport {
    /// Tickets the fold rewrote (or would rewrite), counted.
    pub fn folds(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(a, FoldAction::Fold { .. }))
            .count()
    }

    /// Tickets skipped because their .md has uncommitted changes.
    pub fn dirty_skips(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(a, FoldAction::SkipDirty { .. }))
            .count()
    }

    /// Tickets that already carried a frontmatter `state:` key.
    pub fn already_migrated(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(a, FoldAction::AlreadyMigrated { .. }))
            .count()
    }

    /// Tickets with nothing terminal to fold (no sidecar / pipeline sidecar).
    pub fn left_alone(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| matches!(a, FoldAction::NoTerminalSidecar { .. }))
            .count()
    }
}

/// Plan (and with `apply: true`, perform) the fold of terminal `.status`
/// sidecars into frontmatter `state:` keys. See the module docs for the skip
/// rules. Fail-closed on the git dirty-check: when uncommitted edits cannot
/// be detected, NOTHING is planned or written — the check is the only thing
/// standing between the fold and an in-flight edit.
pub fn fold_sidecar_states(repo_root: &Path, apply: bool) -> Result<MigrationReport> {
    let git = GitRepo::open(repo_root).map_err(|e| {
        EngineError::Git(format!(
            "migrate-state needs git to detect uncommitted ticket edits before \
             rewriting them (fail-closed): {e}"
        ))
    })?;
    let dirty: HashSet<String> = git
        .dirty_paths()?
        .iter()
        // Porcelain paths are repo-relative with forward slashes on every
        // platform; normalize anyway so a Windows `\` never misses a match.
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .collect();

    let mut actions = Vec::new();
    for ticket in Ticket::list(repo_root) {
        let slug = ticket.slug;
        if dirty.contains(&format!(".kranz/tickets/{slug}.md")) {
            actions.push(FoldAction::SkipDirty { slug });
            continue;
        }
        if ticket.lifecycle.is_some() {
            actions.push(FoldAction::AlreadyMigrated { slug });
            continue;
        }
        match Ticket::sidecar_record(repo_root, &slug) {
            Some((TicketState::Done, note)) => {
                if apply {
                    Ticket::write_lifecycle(repo_root, &slug, TicketLifecycle::Done, note.clone())?;
                }
                actions.push(FoldAction::Fold { slug, note });
            }
            other => actions.push(FoldAction::NoTerminalSidecar {
                slug,
                sidecar: other.map(|(state, _)| state),
            }),
        }
    }
    actions.sort_by(|a, b| a.slug().cmp(b.slug()));
    Ok(MigrationReport {
        applied: apply,
        actions,
    })
}
