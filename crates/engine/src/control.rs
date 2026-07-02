//! Cross-process control inbox (design.md "Cross-process control").
//!
//! The single-writer rule (§4.3) means only the engine appends `events.jsonl`.
//! Other processes (CLI `kranz msg/pause/resume`, the server) talk to a
//! running engine by dropping [`ControlCommand`] JSON files into
//! `paths.control_dir()`. File names are `<zero-padded-millis>-<8-hex>.json`
//! so lexicographic order == chronological order; writes go through a tmp
//! file + rename so a draining engine never observes a partial file.
//!
//! The engine [`drain`]s the inbox between worker runs, and a
//! [`ControlWatcher`] polls [`peek_interrupt`] so an `interrupt` message can
//! abort the active run.

use crate::error::Result;
use crate::paths::MissionPaths;
use crate::types::ControlCommand;
use chrono::Utc;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// Width of the zero-padded millisecond prefix — the full `u64` decimal
/// width, so names sort lexicographically for any conceivable timestamp.
const MILLIS_WIDTH: usize = 20;

/// Length of the random hex suffix (a `uuid` v4 `simple()` prefix).
const RAND_LEN: usize = 8;

/// Enqueue one command into the mission's control inbox.
///
/// Creates the control directory if needed, writes the JSON to a sibling tmp
/// file, then atomically renames it to `<zero-padded-millis>-<8-hex>.json` —
/// readers never see partial files. Returns the final file path.
pub fn enqueue(paths: &MissionPaths, cmd: &ControlCommand) -> Result<PathBuf> {
    let dir = paths.control_dir();
    std::fs::create_dir_all(&dir)?;

    let millis = Utc::now().timestamp_millis().max(0) as u64;
    let rand = uuid::Uuid::new_v4().simple().to_string();
    let name = format!("{millis:0width$}-{}.json", &rand[..RAND_LEN], width = MILLIS_WIDTH);

    let final_path = dir.join(&name);
    let tmp_path = dir.join(format!("{name}.tmp"));

    let json = serde_json::to_string(cmd)?;
    {
        let mut file = std::fs::File::create(&tmp_path)?;
        file.write_all(json.as_bytes())?;
        file.sync_data()?;
    }
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(final_path)
}

/// Drain the inbox: parse every queued `.json` file, oldest first,
/// NON-destructively.
///
/// Each parsed command is returned together with the file it came from; the
/// caller deletes each file only AFTER the command has been durably applied
/// (e.g. appended to the event log). Deleting up front would lose commands if
/// the process crashed between the drain and the apply — re-processing a file
/// on the next drain is the safe failure mode (duplicates are tolerated
/// downstream).
///
/// A file that fails to parse is renamed to `<name>.bad` (with a warning) and
/// skipped so a corrupt file can never block the queue. Non-`.json` files
/// (tmp files, `.bad` quarantines) are ignored. Returns the commands in
/// filename (== chronological) order.
pub fn drain(paths: &MissionPaths) -> Result<Vec<(PathBuf, ControlCommand)>> {
    let mut commands = Vec::new();
    for path in queued_files(&paths.control_dir())? {
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                // Transient (e.g. racing another drain); skip, never block.
                tracing::warn!(path = %path.display(), error = %e, "unreadable control file, skipping");
                continue;
            }
        };
        match serde_json::from_str::<ControlCommand>(&content) {
            Ok(cmd) => commands.push((path, cmd)),
            Err(e) => quarantine(&path, &e),
        }
    }
    Ok(commands)
}

/// True if any queued file parses to `Msg { interrupt: true }`.
///
/// Non-destructive: nothing is consumed, deleted, or renamed — the engine's
/// run-watcher polls this cheaply while a later [`drain`] still returns the
/// message itself.
pub fn peek_interrupt(paths: &MissionPaths) -> Result<bool> {
    for path in queued_files(&paths.control_dir())? {
        let Ok(content) = std::fs::read_to_string(&path) else { continue };
        if let Ok(ControlCommand::Msg { interrupt: true, .. }) =
            serde_json::from_str::<ControlCommand>(&content)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Queued `.json` command files in filename (== chronological) order.
/// A missing control dir is an empty queue, not an error.
fn queued_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut files = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if is_file && path.extension().and_then(|e| e.to_str()) == Some("json") {
            files.push(path);
        }
    }
    files.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    Ok(files)
}

/// Rename an unparseable command file to `<name>.bad` so it stops blocking
/// the queue but stays on disk for diagnosis.
fn quarantine(path: &Path, err: &serde_json::Error) {
    let file_name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let bad = path.with_file_name(format!("{file_name}.bad"));
    tracing::warn!(
        path = %path.display(),
        error = %err,
        "unparseable control command, quarantining as .bad"
    );
    if let Err(e) = std::fs::rename(path, &bad) {
        tracing::warn!(path = %path.display(), error = %e, "failed to quarantine control file");
    }
}

/// Spawn-able helper that polls [`peek_interrupt`] and fires a
/// [`tokio::sync::Notify`] once an interrupt message is queued. The
/// orchestrator selects on the notify alongside the active worker run.
pub struct ControlWatcher;

impl ControlWatcher {
    /// Loop [`peek_interrupt`] every `poll` interval; when an interrupt is
    /// seen, fire `notify.notify_one()` once and return. Poll errors are
    /// logged and treated as "no interrupt yet" — the watcher never dies on a
    /// transient filesystem hiccup.
    ///
    /// `notify_one` (never `notify_waiters`) is load-bearing: it stores a
    /// permit when nobody is waiting, so a fire while the run loop is between
    /// `notified()` registrations (or still inside `backend.start()`) is
    /// consumed by the NEXT waiter instead of being lost forever. Tokio also
    /// re-stores/passes on the permit when a woken `Notified` future is
    /// dropped unconsumed, so a `select!` race cannot swallow it either.
    pub async fn wait_for_interrupt(
        paths: MissionPaths,
        poll: std::time::Duration,
        notify: std::sync::Arc<tokio::sync::Notify>,
    ) {
        loop {
            match peek_interrupt(&paths) {
                Ok(true) => {
                    notify.notify_one();
                    return;
                }
                Ok(false) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "control watcher poll failed, retrying");
                }
            }
            tokio::time::sleep(poll).await;
        }
    }
}
