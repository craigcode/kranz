//! Integration tests for the committed ticket lifecycle state (design:
//! `.kranz/tickets/ticket-state-frontmatter.md`): the additive `state:` /
//! `state-note:` frontmatter keys, frontmatter precedence over the `.status`
//! sidecar cache, the one lifecycle write path that keeps both in step, and
//! the one-time `migrate-state` fold of terminal sidecars into frontmatter.
//!
//! Git fixtures mirror `merged_test.rs`: throwaway temp repos with git's
//! global/system config masked, skipping cleanly when git is missing (only
//! the fold tests need git — the dirty-skip is defined in porcelain terms).

use kranz_engine::deps;
use kranz_engine::git_ops::GitRepo;
use kranz_engine::migrate_state::{fold_sidecar_states, FoldAction};
use kranz_engine::ticket::{Ticket, TicketLifecycle, TicketState};
use std::path::Path;
use std::process::Command;
use std::sync::Once;
use tempfile::TempDir;

fn write_ticket(repo: &Path, slug: &str, body: &str) {
    let dir = Ticket::tickets_dir(repo);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{slug}.md")), body).unwrap();
}

fn read_ticket_md(repo: &Path, slug: &str) -> String {
    std::fs::read_to_string(Ticket::tickets_dir(repo).join(format!("{slug}.md"))).unwrap()
}

const OPEN_TICKET: &str = "\
---
title: Plain open ticket
priority: 2
---

## Goal
Do the thing.
";

// ---------------------------------------------------------------------------
// Parsing + backcompat (absent key = open = sidecar governs, as before)
// ---------------------------------------------------------------------------

#[test]
fn ticket_state_frontmatter_absent_key_defaults_open_and_sidecar_governs() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_ticket(repo, "plain", OPEN_TICKET);

    // No `state:` key anywhere: the parsed lifecycle is absent, and every
    // read is exactly the pre-schema sidecar behavior.
    let ticket = Ticket::load(&Ticket::tickets_dir(repo).join("plain.md")).unwrap();
    assert_eq!(ticket.lifecycle, None);
    assert_eq!(ticket.state_note, None);
    assert_eq!(Ticket::read_state(repo, "plain"), TicketState::New);

    Ticket::write_state(repo, "plain", TicketState::Review, None).unwrap();
    assert_eq!(Ticket::read_state(repo, "plain"), TicketState::Review);
    let resolved = Ticket::resolve_state(repo, "plain");
    assert_eq!(resolved.state, TicketState::Review);
    assert_eq!(resolved.divergence, None);
}

#[test]
fn ticket_state_frontmatter_parses_state_and_state_note() {
    let md = "\
---
title: Closed elsewhere
state: superseded
state-note: superseded by the flight-surgeon console
---

## Goal
Dead end.
";
    let t = Ticket::parse("closed", md).unwrap();
    assert_eq!(t.lifecycle, Some(TicketLifecycle::Superseded));
    assert_eq!(
        t.state_note.as_deref(),
        Some("superseded by the flight-surgeon console")
    );

    // An empty value reads as absent (same rule as the other scalar keys).
    let t = Ticket::parse("closed", &md.replace("state: superseded", "state:")).unwrap();
    assert_eq!(t.lifecycle, None);
}

#[test]
fn ticket_state_frontmatter_invalid_state_is_a_hard_parse_error() {
    let md = "\
---
title: Typoed
state: supersed
---

## Goal
A mistyped terminal state must not silently re-open.
";
    // The parser fails closed (the defer-until rule): a silently-defaulted
    // terminal state would re-queue work its author explicitly closed.
    let err = Ticket::parse("typoed", md).unwrap_err();
    assert!(
        err.to_string().contains("invalid state 'supersed'"),
        "unexpected error: {err}"
    );

    // The read path stays total (listings already dropped the ticket,
    // loudly): it warns and lets the sidecar govern rather than inventing
    // a state.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_ticket(repo, "typoed", md);
    Ticket::write_state(repo, "typoed", TicketState::Review, None).unwrap();
    assert_eq!(Ticket::read_state(repo, "typoed"), TicketState::Review);
}

// ---------------------------------------------------------------------------
// Frontmatter precedence: terminal states win, survive a cold cache, and a
// diverging sidecar is reported (never silently followed)
// ---------------------------------------------------------------------------

#[test]
fn ticket_state_frontmatter_terminal_states_win_over_a_cold_cache() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    for (slug, key, expected) in [
        ("done-t", "done", TicketState::Done),
        ("sup-t", "superseded", TicketState::Superseded),
        ("wont-t", "wontfix", TicketState::Wontfix),
    ] {
        write_ticket(
            repo,
            slug,
            &format!("---\ntitle: {slug}\nstate: {key}\n---\n## Goal\nx\n"),
        );
        // NO sidecar — the fresh-clone case: the committed frontmatter alone
        // keeps the ticket terminal, and a cold cache is not a divergence.
        let resolved = Ticket::resolve_state(repo, slug);
        assert_eq!(resolved.state, expected, "slug {slug}");
        assert_eq!(resolved.divergence, None, "slug {slug}");
        assert_eq!(Ticket::read_state(repo, slug), expected, "slug {slug}");
    }
}

#[test]
fn ticket_state_frontmatter_explicit_open_defers_to_sidecar() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_ticket(
        repo,
        "open",
        "---\ntitle: Open\nstate: open\n---\n## Goal\nx\n",
    );
    Ticket::write_state(repo, "open", TicketState::Review, None).unwrap();
    // `open` is no terminal claim: the pipeline sidecar governs, and there
    // is nothing to diverge.
    let resolved = Ticket::resolve_state(repo, "open");
    assert_eq!(resolved.state, TicketState::Review);
    assert_eq!(resolved.divergence, None);
}

#[test]
fn ticket_state_frontmatter_conflict_resolves_to_frontmatter_with_logged_note() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_ticket(
        repo,
        "conflict",
        "---\ntitle: Conflict\nstate: superseded\nstate-note: moved\n---\n## Goal\nx\n",
    );
    // The cache says one thing, the committed truth another: the frontmatter
    // wins and the discard is surfaced for the warn log.
    Ticket::write_state(repo, "conflict", TicketState::Review, None).unwrap();
    let resolved = Ticket::resolve_state(repo, "conflict");
    assert_eq!(resolved.state, TicketState::Superseded);
    let divergence = resolved.divergence.expect("a diverging cache is reported");
    assert_eq!(divergence.frontmatter, TicketState::Superseded);
    assert_eq!(divergence.sidecar, TicketState::Review);
    assert_eq!(
        Ticket::read_state(repo, "conflict"),
        TicketState::Superseded
    );

    // Re-syncing the cache through the one write path clears the divergence.
    Ticket::write_lifecycle(
        repo,
        "conflict",
        TicketLifecycle::Superseded,
        Some("moved".to_string()),
    )
    .unwrap();
    let resolved = Ticket::resolve_state(repo, "conflict");
    assert_eq!(resolved.state, TicketState::Superseded);
    assert_eq!(resolved.divergence, None);
}

#[test]
fn ticket_state_frontmatter_terminal_frontmatter_excludes_from_queue() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_ticket(
        repo,
        "closable",
        "---\ntitle: Closable\nstate: wontfix\n---\n## Goal\nx\n",
    );
    // A stale Review cache must not make the ticket queueable: the approve
    // gate resolves the frontmatter and refuses like any terminal state.
    Ticket::write_state(repo, "closable", TicketState::Review, None).unwrap();
    let err = deps::approve_ticket(repo, "closable", None, false).unwrap_err();
    assert!(
        err.to_string().contains("only a REVIEW or PARKED ticket"),
        "unexpected error: {err}"
    );
    assert!(
        err.to_string().contains("WONTFIX"),
        "unexpected error: {err}"
    );
}

// ---------------------------------------------------------------------------
// The one lifecycle write path: frontmatter + sidecar cache in step
// ---------------------------------------------------------------------------

#[test]
fn ticket_state_frontmatter_write_lifecycle_writes_both_sides() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    write_ticket(repo, "both", OPEN_TICKET);
    Ticket::record_mission(repo, "both", "m-both1").unwrap();

    Ticket::write_lifecycle(
        repo,
        "both",
        TicketLifecycle::Superseded,
        Some("superseded by other-work".to_string()),
    )
    .unwrap();

    // The committed .md carries the keys (inside the frontmatter block, the
    // rest of the file preserved)…
    let md = read_ticket_md(repo, "both");
    assert!(md.contains("state: superseded\n"), "md:\n{md}");
    assert!(
        md.contains("state-note: superseded by other-work\n"),
        "md:\n{md}"
    );
    assert!(md.contains("title: Plain open ticket\n"), "md:\n{md}");
    assert!(md.contains("## Goal\nDo the thing.\n"), "md:\n{md}");
    let ticket = Ticket::load(&Ticket::tickets_dir(repo).join("both.md")).unwrap();
    assert_eq!(ticket.lifecycle, Some(TicketLifecycle::Superseded));
    assert_eq!(
        ticket.state_note.as_deref(),
        Some("superseded by other-work")
    );

    // …and the sidecar cache mirrors the terminal projection, keeping every
    // pre-schema reader working — including the mission link.
    assert_eq!(Ticket::read_state(repo, "both"), TicketState::Superseded);
    assert_eq!(
        Ticket::mission_for(repo, "both").as_deref(),
        Some("m-both1")
    );
    let sidecar: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(Ticket::tickets_dir(repo).join("both.status")).unwrap(),
    )
    .unwrap();
    assert_eq!(sidecar["state"], "superseded");

    // Re-closing without a note REPLACES the state and REMOVES the stale
    // note line — a note must not outlive the state it described.
    Ticket::write_lifecycle(repo, "both", TicketLifecycle::Done, None).unwrap();
    let md = read_ticket_md(repo, "both");
    assert!(md.contains("state: done\n"), "md:\n{md}");
    assert!(!md.contains("state-note"), "md:\n{md}");
    assert_eq!(Ticket::read_state(repo, "both"), TicketState::Done);

    // `open` is the absence of a claim — there is nothing to cache, so the
    // write path refuses it rather than inventing a sidecar value.
    assert!(Ticket::write_lifecycle(repo, "both", TicketLifecycle::Open, None).is_err());
}

// ---------------------------------------------------------------------------
// migrate-state: the one-time fold (git fixtures — the dirty-skip is defined
// in porcelain terms)
// ---------------------------------------------------------------------------

static ENV_ISOLATION: Once = Once::new();

fn isolate_git_env() {
    ENV_ISOLATION.call_once(|| {
        let missing = std::env::temp_dir().join(format!(
            "kranz-ticket-state-test-no-config-{}",
            std::process::id()
        ));
        std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
        std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
        if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
            std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
        }
    });
}

fn git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `(TempDir, canonicalized repo root)` on branch `main` with git identity
/// configured, or `None` when git is unavailable (test skips).
fn init_repo() -> Option<(TempDir, std::path::PathBuf)> {
    isolate_git_env();
    if !git_available() {
        kranz_engine::test_capability::skip(
            kranz_engine::test_capability::capability::GIT,
            "git is not on PATH",
        );
        return None;
    }
    let dir = tempfile::tempdir().unwrap();
    let init = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(dir.path())
        .output()
        .expect("spawn git init");
    if !init.status.success() {
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git init (fallback)");
        Command::new("git")
            .args(["symbolic-ref", "HEAD", "refs/heads/main"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git symbolic-ref");
    }
    let repo = GitRepo::open(dir.path()).expect("open freshly-initialized repo");
    repo.ensure_identity().expect("ensure_identity");
    let root = std::fs::canonicalize(dir.path()).expect("canonicalize repo root");
    Some((dir, root))
}

fn commit_all(repo: &Path, message: &str) {
    let git = GitRepo::open(repo).expect("open repo");
    git.add_all_and_commit(message).expect("commit fixture");
}

/// A fold fixture: committed tickets covering every report bucket.
fn seed_fold_fixture(repo: &Path) {
    write_ticket(repo, "fold-note", OPEN_TICKET);
    Ticket::write_state(
        repo,
        "fold-note",
        TicketState::Done,
        Some("shipped in m-fold1".to_string()),
    )
    .unwrap();
    Ticket::record_mission(repo, "fold-note", "m-fold1").unwrap();

    write_ticket(repo, "fold-plain", OPEN_TICKET);
    Ticket::write_state(repo, "fold-plain", TicketState::Done, None).unwrap();

    write_ticket(repo, "pipeline", OPEN_TICKET);
    Ticket::write_state(repo, "pipeline", TicketState::NeedsContext, None).unwrap();

    write_ticket(repo, "no-sidecar", OPEN_TICKET);

    write_ticket(
        repo,
        "already",
        "---\ntitle: Migrated\nstate: done\nstate-note: by hand\n---\n## Goal\nx\n",
    );
    Ticket::write_state(repo, "already", TicketState::Done, None).unwrap();

    commit_all(repo, "ticket fixtures");
}

#[test]
fn ticket_state_frontmatter_migration_folds_terminal_sidecars() {
    let Some((_dir, repo)) = init_repo() else {
        return;
    };
    seed_fold_fixture(&repo);

    // Dry-run: every bucket reported, not a byte written.
    let plan = fold_sidecar_states(&repo, false).unwrap();
    assert!(!plan.applied);
    assert_eq!(plan.folds(), 2);
    assert_eq!(plan.already_migrated(), 1);
    assert_eq!(plan.left_alone(), 2);
    assert_eq!(plan.dirty_skips(), 0);
    assert!(plan.actions.contains(&FoldAction::Fold {
        slug: "fold-note".to_string(),
        note: Some("shipped in m-fold1".to_string()),
    }));
    assert!(plan.actions.contains(&FoldAction::Fold {
        slug: "fold-plain".to_string(),
        note: None,
    }));
    assert!(plan.actions.contains(&FoldAction::NoTerminalSidecar {
        slug: "pipeline".to_string(),
        sidecar: Some(TicketState::NeedsContext),
    }));
    assert!(plan.actions.contains(&FoldAction::NoTerminalSidecar {
        slug: "no-sidecar".to_string(),
        sidecar: None,
    }));
    assert!(plan.actions.contains(&FoldAction::AlreadyMigrated {
        slug: "already".to_string(),
    }));
    assert!(!read_ticket_md(&repo, "fold-note").contains("state:"));

    // Apply: the fold lands in the frontmatter (note carried, mission link
    // preserved), and untouched buckets stay untouched.
    let applied = fold_sidecar_states(&repo, true).unwrap();
    assert!(applied.applied);
    assert_eq!(applied.folds(), 2);

    let md = read_ticket_md(&repo, "fold-note");
    assert!(md.contains("state: done\n"), "md:\n{md}");
    assert!(md.contains("state-note: shipped in m-fold1\n"), "md:\n{md}");
    assert!(md.contains("## Goal\nDo the thing.\n"), "md:\n{md}");
    assert_eq!(Ticket::read_state(&repo, "fold-note"), TicketState::Done);
    assert_eq!(
        Ticket::mission_for(&repo, "fold-note").as_deref(),
        Some("m-fold1")
    );

    let md = read_ticket_md(&repo, "fold-plain");
    assert!(md.contains("state: done\n"), "md:\n{md}");
    assert!(!md.contains("state-note"), "md:\n{md}");

    assert!(!read_ticket_md(&repo, "pipeline").contains("state:"));
    assert_eq!(
        read_ticket_md(&repo, "already")
            .matches("state: done")
            .count(),
        1,
        "an operator-set state: key is never rewritten"
    );

    // Idempotency has two halves. Immediately after the fold, the rewritten
    // tickets are uncommitted — porcelain-dirty — so a re-run protects them
    // with the same skip as any in-flight edit (and folds nothing twice)…
    let rerun_dirty = fold_sidecar_states(&repo, true).unwrap();
    assert_eq!(rerun_dirty.folds(), 0);
    assert_eq!(rerun_dirty.dirty_skips(), 2);
    // …and once the operator commits the fold, a re-run reports every folded
    // ticket as already migrated and changes no bytes.
    commit_all(&repo, "fold sidecar states into frontmatter");
    let before = read_ticket_md(&repo, "fold-note");
    let rerun = fold_sidecar_states(&repo, true).unwrap();
    assert_eq!(rerun.folds(), 0);
    assert_eq!(rerun.already_migrated(), 3);
    assert_eq!(read_ticket_md(&repo, "fold-note"), before);
}

#[test]
fn ticket_state_frontmatter_migration_skips_dirty_tickets_by_name() {
    let Some((_dir, repo)) = init_repo() else {
        return;
    };
    write_ticket(&repo, "clean", OPEN_TICKET);
    Ticket::write_state(&repo, "clean", TicketState::Done, None).unwrap();
    write_ticket(&repo, "dirty", OPEN_TICKET);
    Ticket::write_state(&repo, "dirty", TicketState::Done, None).unwrap();
    commit_all(&repo, "ticket fixtures");

    // An in-flight edit (uncommitted modification) and a never-committed new
    // ticket: both are porcelain-dirty, both must be skipped by name — never
    // rewrite a file an in-flight editor or agent has open.
    std::fs::write(
        Ticket::tickets_dir(&repo).join("dirty.md"),
        format!("{OPEN_TICKET}\nin-flight edit\n"),
    )
    .unwrap();
    write_ticket(&repo, "untracked", OPEN_TICKET);
    Ticket::write_state(&repo, "untracked", TicketState::Done, None).unwrap();

    let plan = fold_sidecar_states(&repo, false).unwrap();
    assert_eq!(plan.folds(), 1);
    assert_eq!(plan.dirty_skips(), 2);
    assert!(plan.actions.contains(&FoldAction::SkipDirty {
        slug: "dirty".to_string(),
    }));
    assert!(plan.actions.contains(&FoldAction::SkipDirty {
        slug: "untracked".to_string(),
    }));

    let applied = fold_sidecar_states(&repo, true).unwrap();
    assert_eq!(applied.dirty_skips(), 2);
    assert!(read_ticket_md(&repo, "clean").contains("state: done\n"));
    let dirty_md = read_ticket_md(&repo, "dirty");
    assert!(!dirty_md.contains("state:"), "md:\n{dirty_md}");
    assert!(dirty_md.contains("in-flight edit"), "md:\n{dirty_md}");
    assert!(!read_ticket_md(&repo, "untracked").contains("state:"));
    // The dirty tickets' sidecars still govern via the backcompat path —
    // their terminal state is not lost, just not yet folded.
    assert_eq!(Ticket::read_state(&repo, "dirty"), TicketState::Done);
    assert_eq!(Ticket::read_state(&repo, "untracked"), TicketState::Done);
}
