//! Mission catalog + mission hygiene — extracted from `orchestrator.rs` in
//! the monolith split (pure code motion, no behavior change). The catalog
//! helpers edit `.kranz/missions/index.md` lines (prune, list ids, attach a
//! report link); the hygiene family (roadmap M2) retires missions
//! ([`abandon_mission`]) and classifies mission directories for `kranz clean`
//! ([`cleanable_class`]) — lifecycle helpers kept deliberately OUTSIDE the
//! run loop, touching neither orchestrator sessions nor worker turns.

use crate::error::{EngineError, Result};
use crate::event_log::{EventLog, LockForce};
use crate::events::EventKind;
use crate::orchestrator::canonical_root;
use crate::paths::MissionPaths;
use crate::reducer;
use crate::types::MissionStatus;
use std::path::PathBuf;
use std::time::Duration;

/// Remove a single mission's line from the missions catalog (deletion
/// counterpart to [`crate::orchestrator::upsert_mission_index`]), matched by
/// the same `[<id>](` marker. Every other line and the header stay
/// byte-for-byte; pruning an id with no line is a no-op (modulo
/// trailing-newline normalization, same as [`mark_mission_index_report`]).
pub fn prune_mission_index(existing: &str, mission_id: &str) -> String {
    if existing.trim().is_empty() {
        return existing.to_string();
    }
    let marker = format!("[{mission_id}](");
    let mut out = String::new();
    for l in existing.lines() {
        if l.contains(&marker) {
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    out
}

/// Every mission id appearing as `[<id>](` in the catalog body, in file
/// order, de-duplicated. Tolerant of the trailing ` · [report](<id>/report.md)`
/// link: the FIRST bracket on a line (the plan.md link) is taken as the id.
pub fn mission_index_ids(existing: &str) -> Vec<String> {
    let mut ids = Vec::new();
    for l in existing.lines() {
        if !l.contains("](") {
            continue;
        }
        let Some(start) = l.find('[') else {
            continue;
        };
        let rest = &l[start + 1..];
        let Some(end) = rest.find("](") else {
            continue;
        };
        let id = &rest[..end];
        if !ids.iter().any(|existing_id: &String| existing_id == id) {
            ids.push(id.to_string());
        }
    }
    ids
}

/// Prune one mission's line from `<repo>/.kranz/missions/index.md` and
/// write the result back. A missing index file is a no-op — it is never
/// created here.
pub fn prune_mission_index_file(repo_root: &std::path::Path, mission_id: &str) {
    let index = MissionPaths::new(repo_root, "_")
        .missions_dir()
        .join("index.md");
    let Ok(existing) = std::fs::read_to_string(&index) else {
        return;
    };
    let updated = prune_mission_index(&existing, mission_id);
    let _ = std::fs::write(&index, updated);
}

/// Add a completion-report link to one mission's line in the missions
/// catalog, turning
/// `- <date> · [<id>](<id>/plan.md) — <goal>` into
/// `- <date> · [<id>](<id>/plan.md) — <goal> · [report](<id>/report.md)`.
///
/// Idempotent; every other line — and the line format itself — stays
/// untouched. When the mission has no line, the index comes back unchanged.
pub fn mark_mission_index_report(existing: &str, mission_id: &str) -> String {
    let marker = format!("[{mission_id}](");
    let link = format!("[report]({mission_id}/report.md)");
    let mut out = String::new();
    for l in existing.lines() {
        out.push_str(l);
        if l.contains(&marker) && !l.contains(&link) {
            out.push_str(" · ");
            out.push_str(&link);
        }
        out.push('\n');
    }
    out
}

// ---------------------------------------------------------------------------
// Mission hygiene (roadmap M2): abandon + clean classification
//
// These are lifecycle helpers kept deliberately OUTSIDE the run loop — they
// never touch the orchestrator session and only ever append the terminal
// `mission.abandoned` event or classify a directory for removal.
// ---------------------------------------------------------------------------

/// Retire a mission as [`MissionStatus::Abandoned`] — a terminal, operator-
/// initiated end-of-life that is *not* a failure (§ roadmap M2 "mission
/// hygiene"; the contract already defines the event + reducer mapping).
///
/// Acquires the single-writer lock via [`EventLog::acquire`], so a live engine
/// holding it surfaces as [`EngineError::LockHeld`] (the CLI then tells the
/// operator to stop the running mission; `--force-lock` steals only a lock
/// whose holder is not provably alive, `--dangerously-steal-live-lock` steals
/// even a live one). A mission that
/// is already terminal (Complete/Failed/Abandoned) is rejected with
/// [`EngineError::InvalidState`] — abandoning is only meaningful for live work.
/// On success one `mission.abandoned` event is appended, the state snapshot is
/// refreshed, and the lock released on drop.
///
/// A ticket that points at this mission is left untouched: the reverse mapping
/// (`.kranz/tickets/<slug>.status` → mission id) is not cheaply invertible, and
/// abandoning the mission is the operator's intent regardless.
pub fn abandon_mission(
    repo_root: impl Into<PathBuf>,
    mission_id: &str,
    reason: &str,
    force: LockForce,
) -> Result<()> {
    let repo_root = canonical_root(repo_root.into());
    let paths = MissionPaths::new(&repo_root, mission_id);

    // Fold the existing log first so we can reject an already-terminal mission
    // before writing anything.
    let events = EventLog::read_events(&paths.events_file())?;
    let state = reducer::fold(&events)?;
    if is_terminal_status(state.mission.status) {
        return Err(EngineError::InvalidState(format!(
            "mission '{mission_id}' is already terminal ({:?}); nothing to abandon",
            state.mission.status
        )));
    }

    // Acquire the lock (LockHeld ⇒ a live engine owns this mission).
    let mut log = EventLog::acquire(
        &paths,
        mission_id,
        Duration::from_millis(state.config.event_stream_throttle_ms),
        force,
    )?;
    let (event, audits) = log.append_with_redaction_audits(EventKind::MissionAbandoned {
        reason: reason.to_string(),
    })?;
    // Fold the new event plus any redaction audits on top of the state we
    // already have and snapshot, so state.json matches the log without a full
    // re-fold.
    let mut state = state;
    reducer::apply(&mut state, &event)?;
    for audit in &audits {
        reducer::apply(&mut state, audit)?;
    }
    reducer::write_snapshot(&state, &paths.state_file())?;
    // `log` drops here: buffer flushed, lock released.
    Ok(())
}

/// Terminal mission statuses (no further work will ever run against them).
pub fn is_terminal_status(status: MissionStatus) -> bool {
    matches!(
        status,
        MissionStatus::Complete | MissionStatus::Failed | MissionStatus::Abandoned
    )
}

/// How a mission directory classifies for `kranz clean`. The decision is a
/// pure function of the folded status and whether a `plan.json` exists, so it
/// is trivially testable in isolation from the filesystem walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanClass {
    /// Retire by default (`kranz clean`): the mission failed, was abandoned, or
    /// is an abandoned-in-planning husk (still Planning with no plan.json).
    Stale,
    /// Only removed with `--all`: a Complete mission whose branch/report may
    /// still be under review.
    CompleteKeepByDefault,
    /// Never cleaned: the mission is live (Planning-with-plan, Running, Paused,
    /// Blocked, Validating).
    Keep,
}

impl CleanClass {
    /// Whether this class is removed given the `--all` opt-in.
    pub fn is_cleaned(self, all: bool) -> bool {
        match self {
            CleanClass::Stale => true,
            CleanClass::CompleteKeepByDefault => all,
            CleanClass::Keep => false,
        }
    }
}

/// Classify a mission for cleaning from its folded `status` and whether a
/// `plan.json` is present. Liveness (a held lock) is handled separately by the
/// caller — a running mission is *never* cleaned regardless of this class.
pub fn cleanable_class(status: MissionStatus, has_plan: bool) -> CleanClass {
    match status {
        MissionStatus::Failed | MissionStatus::Abandoned => CleanClass::Stale,
        // An abandoned-in-planning husk: never approved a plan, so nothing on a
        // branch to lose.
        MissionStatus::Planning if !has_plan => CleanClass::Stale,
        MissionStatus::Complete => CleanClass::CompleteKeepByDefault,
        // Planning-with-plan, Running, Paused, Blocked, Validating: live work.
        _ => CleanClass::Keep,
    }
}

/// True when a mission's lock file records a holder that is still alive (a
/// running engine). Delegates to the event-log module's canonical probe
/// ([`crate::event_log::lock_holder_is_alive`]) — ONE source of truth for
/// lock-file format and liveness semantics. A missing lock is not live; a
/// present-but-unreadable/unparseable one is treated as live (conservative —
/// a false "alive" only spares a directory from cleaning, while a false
/// "dead" could delete a mission out from under a running engine); a
/// provably-dead holder (ESRCH, or a token-proven pid reuse) is not live.
/// Non-unix platforms cannot probe, so an existing lock reads as live.
pub fn mission_lock_is_live(paths: &MissionPaths) -> bool {
    crate::event_log::lock_holder_is_alive(&paths.lock_file())
}
