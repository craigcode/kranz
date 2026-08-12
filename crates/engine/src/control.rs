//! Cross-process control inbox (design.md "Cross-process control").
//!
//! The single-writer rule (§4.3) means only the engine appends `events.jsonl`.
//! Other processes (CLI `kranz msg/pause/resume`, the server) talk to a
//! running engine by dropping [`ControlCommand`] JSON files into
//! `paths.control_dir()`. File names are `<zero-padded-nanos>-<8-hex>.json`
//! so lexicographic order == chronological order; writes go through a tmp
//! file + rename so a draining engine never observes a partial file. The
//! nanosecond prefix keeps back-to-back enqueues (e.g. `pause` immediately
//! followed by `resume`) in issue order — a millisecond prefix left same-ms
//! enqueues to be ordered by the random suffix.
//!
//! The engine [`drain`]s the inbox between worker runs, and a
//! [`ControlWatcher`] polls [`peek_interrupt`] so an `interrupt` message can
//! abort the active run.

use crate::error::{EngineError, Result};
use crate::paths::MissionPaths;
use crate::types::{ControlCommand, MissionStatus};
use chrono::Utc;
use std::ffi::{OsStr, OsString};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

/// Width of the zero-padded nanosecond prefix — the full `u64` decimal
/// width, so names sort lexicographically for any conceivable timestamp.
const TIMESTAMP_WIDTH: usize = 20;

/// Length of the random hex suffix (a `uuid` v4 `simple()` prefix).
const RAND_LEN: usize = 8;

/// Enqueue one command into the mission's control inbox.
///
/// Creates the mission tree and control directory if needed — no-follow (P1
/// mission-path-no-follow): a symlinked `.kranz`/`missions`/mission dir or
/// `control/` entry is refused, never followed, since an enqueue through a
/// symlink would route control commands into another repository's mission.
/// The JSON goes to a sibling tmp file, then atomically renames to
/// `<zero-padded-nanos>-<8-hex>.json` — readers never see partial files.
/// Returns the final file path.
pub fn enqueue(paths: &MissionPaths, cmd: &ControlCommand) -> Result<PathBuf> {
    let mission_dir = paths.open_mission_dir_nofollow(true)?;
    let dir = paths.control_dir();
    let control_dir = crate::paths::open_real_subdir(&mission_dir, "control", &dir, true)?;

    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or(0).max(0) as u64;
    let rand = uuid::Uuid::new_v4().simple().to_string();
    let name = format!(
        "{nanos:0width$}-{}.json",
        &rand[..RAND_LEN],
        width = TIMESTAMP_WIDTH
    );

    let final_path = dir.join(&name);
    let tmp_name = format!("{name}.tmp");

    let json = serde_json::to_string(cmd)?;
    {
        use cap_fs_ext::OpenOptionsFollowExt as _;
        use cap_primitives::fs::FollowSymlinks;
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .follow(FollowSymlinks::No);
        let mut file = control_dir.open_with(&tmp_name, &options)?.into_std();
        file.write_all(json.as_bytes())?;
        file.sync_data()?;
    }
    control_dir.rename(&tmp_name, &control_dir, &name)?;
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
    let Some(control_dir) = control_dir(paths, false)? else {
        return Ok(commands);
    };
    for name in queued_files(&control_dir)? {
        let path = paths.control_dir().join(&name);
        let content = match read_control_file(&control_dir, &name) {
            Ok(c) => c,
            Err(e) => {
                // Transient (e.g. racing another drain); skip, never block.
                tracing::warn!(path = %path.display(), error = %e, "unreadable control file, skipping");
                continue;
            }
        };
        match serde_json::from_str::<ControlCommand>(&content) {
            Ok(cmd) => commands.push((path, cmd)),
            Err(e) => quarantine(&control_dir, &name, &path, &e),
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
    let Some(control_dir) = control_dir(paths, false)? else {
        return Ok(false);
    };
    for name in queued_files(&control_dir)? {
        let Ok(content) = read_control_file(&control_dir, &name) else {
            continue;
        };
        if let Ok(ControlCommand::Msg {
            interrupt: true, ..
        }) = serde_json::from_str::<ControlCommand>(&content)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Remove a command returned by [`drain`] after its event has been durably
/// applied. The delete stays relative to the same no-follow mission/control
/// chain as enqueue and drain, so a swapped parent symlink cannot redirect
/// acknowledgement outside the mission.
pub fn acknowledge(paths: &MissionPaths, path: &Path) -> Result<()> {
    if path.parent() != Some(paths.control_dir().as_path()) {
        return Err(EngineError::InvalidState(format!(
            "refusing control acknowledgement outside {}: {}",
            paths.control_dir().display(),
            path.display()
        )));
    }
    let name = path.file_name().ok_or_else(|| {
        EngineError::InvalidState(format!("control path {} has no file name", path.display()))
    })?;
    let Some(control_dir) = control_dir(paths, false)? else {
        return Err(std::io::Error::from(ErrorKind::NotFound).into());
    };
    control_dir.remove_file(name)?;
    Ok(())
}

/// Fold one mission's status; `None` when its log is unreadable or absent.
fn mission_status(repo_root: &Path, id: &str) -> Option<MissionStatus> {
    let paths = MissionPaths::new(repo_root, id);
    let events = crate::event_log::EventLog::read_events(&paths.events_file()).ok()?;
    Some(crate::reducer::fold(&events).ok()?.mission.status)
}

/// Resolve the target mission for a mid-mission control command (config
/// change, pause, resume): an ACTIVE (non-terminal) mission, chosen
/// unambiguously. Shared by the Slack bridge (`/kranz config|pause|resume`)
/// and the CLI (`kranz config role`), so both surfaces refuse the same
/// hazardous targets:
///
/// - explicit id: must exist and be active. A terminal mission's control
///   inbox is never drained (run() refuses terminal missions), so enqueuing
///   there would be a silent no-op reported as success — reject it instead.
/// - no id: exactly one active mission → use it; none → error; several →
///   error listing the candidates and asking for an explicit id (an
///   mtime-based guess could hijack the wrong running mission).
pub fn resolve_active_mission(repo_root: &Path, explicit: Option<&str>) -> Result<String> {
    let is_terminal = crate::mission_catalog::is_terminal_status;
    if let Some(id) = explicit {
        if !MissionPaths::is_safe_id(id) {
            return Err(EngineError::Other(format!("unknown mission `{id}`")));
        }
        // A symlinked mission dir is refused (P1 mission-path-no-follow),
        // never followed into another repository's mission.
        if MissionPaths::new(repo_root, id)
            .require_no_follow()
            .is_err()
        {
            return Err(EngineError::Other(format!("unknown mission `{id}`")));
        }
        match mission_status(repo_root, id) {
            None => Err(EngineError::Other(format!("unknown mission `{id}`"))),
            Some(s) if is_terminal(s) => Err(EngineError::Other(format!(
                "mission `{id}` is {s:?}; this change applies only to active missions"
            ))),
            Some(_) => Ok(id.to_string()),
        }
    } else {
        let active: Vec<String> = MissionPaths::list_missions(repo_root)
            .into_iter()
            .filter(|id| mission_status(repo_root, id).is_some_and(|s| !is_terminal(s)))
            .collect();
        match active.len() {
            0 => Err(EngineError::Other(
                "no active mission — create one first".into(),
            )),
            1 => Ok(active.into_iter().next().expect("len == 1")),
            _ => Err(EngineError::Other(format!(
                "several active missions ({}); name one explicitly",
                active.join(", ")
            ))),
        }
    }
}

/// Open the control directory through the pinned mission capability. A
/// missing mission/control directory is an empty inbox when `create` is
/// false.
fn control_dir(paths: &MissionPaths, create: bool) -> Result<Option<cap_std::fs::Dir>> {
    let mission_dir = match paths.open_mission_dir_nofollow(create) {
        Ok(dir) => dir,
        Err(EngineError::Io(error)) if error.kind() == ErrorKind::NotFound && !create => {
            return Ok(None)
        }
        Err(error) => return Err(error),
    };
    match crate::paths::open_real_subdir(&mission_dir, "control", &paths.control_dir(), create) {
        Ok(dir) => Ok(Some(dir)),
        Err(EngineError::Io(error)) if error.kind() == ErrorKind::NotFound && !create => Ok(None),
        Err(error) => Err(error),
    }
}

/// Queued `.json` command names in filename (== chronological) order.
fn queued_files(dir: &cap_std::fs::Dir) -> Result<Vec<OsString>> {
    let entries = dir.entries()?;
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let is_file = entry.file_type().map(|t| t.is_file()).unwrap_or(false);
        if is_file && Path::new(&name).extension().and_then(|e| e.to_str()) == Some("json") {
            files.push(name);
        }
    }
    files.sort();
    Ok(files)
}

fn read_control_file(dir: &cap_std::fs::Dir, name: &OsStr) -> Result<String> {
    use cap_fs_ext::OpenOptionsFollowExt as _;
    use cap_primitives::fs::FollowSymlinks;
    use std::io::Read;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let mut file = dir.open_with(name, &options)?.into_std();
    let mut content = String::new();
    file.read_to_string(&mut content)?;
    Ok(content)
}

/// Rename an unparseable command file to `<name>.bad` so it stops blocking
/// the queue but stays on disk for diagnosis.
fn quarantine(dir: &cap_std::fs::Dir, name: &OsStr, path: &Path, err: &serde_json::Error) {
    let bad_name = format!("{}.bad", name.to_string_lossy());
    tracing::warn!(
        path = %path.display(),
        error = %err,
        "unparseable control command, quarantining as .bad"
    );
    if let Err(e) = dir.rename(name, dir, &bad_name) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{Event, EventKind};
    use crate::types::MissionConfig;
    use tempfile::TempDir;

    /// Serialized `events.jsonl` lines for a `mission.created` event (and
    /// optionally a `mission.completed`) that fold like a real log.
    fn events_lines(mission_id: &str, completed: bool) -> String {
        let mut lines = String::new();
        let created = Event {
            seq: 1,
            ts: Utc::now(),
            mission_id: mission_id.to_string(),
            kind: EventKind::MissionCreated {
                goal: "goal".into(),
                base_branch: "main".into(),
                mission_branch: format!("kranz/mission-{mission_id}"),
                config: MissionConfig::default(),
            },
        };
        lines.push_str(&serde_json::to_string(&created).unwrap());
        lines.push('\n');
        if completed {
            let done = Event {
                seq: 2,
                ts: Utc::now(),
                mission_id: mission_id.to_string(),
                kind: EventKind::MissionCompleted {},
            };
            lines.push_str(&serde_json::to_string(&done).unwrap());
            lines.push('\n');
        }
        lines
    }

    /// Seed a mission's `events.jsonl` under the repo's missions dir.
    fn seed_mission(repo_root: &Path, mission_id: &str, completed: bool) {
        let paths = MissionPaths::new(repo_root, mission_id);
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        std::fs::write(paths.events_file(), events_lines(mission_id, completed)).unwrap();
    }

    #[test]
    fn resolve_explicit_active_mission_is_accepted() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-a", false);
        assert_eq!(
            resolve_active_mission(tmp.path(), Some("m-a")).unwrap(),
            "m-a"
        );
    }

    #[test]
    fn resolve_unknown_mission_is_an_error() {
        let tmp = TempDir::new().unwrap();
        let err = resolve_active_mission(tmp.path(), Some("m-nope"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("m-nope"),
            "error names the unknown mission: {err}"
        );
    }

    #[test]
    fn resolve_explicit_mission_rejects_path_traversal_before_reading() {
        let tmp = TempDir::new().unwrap();
        let repo_root = tmp.path().join("nested").join("repo");
        std::fs::create_dir_all(repo_root.join(".kranz").join("missions")).unwrap();
        // Seed a real, foldable ACTIVE mission log at the traversal TARGET:
        // from `<repo>/.kranz/missions/<id>`, the `../../..` id below lands
        // in `<tmp>/nested/outside-target/`. An empty repo would mask a
        // reverted guard — every traversal id would fail as "unknown mission"
        // for the wrong reason.
        let outside = tmp.path().join("nested").join("outside-target");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("events.jsonl"),
            events_lines("outside-target", false),
        )
        .unwrap();
        let traversal_id = "../../../outside-target";
        // Fixture liveness: the traversal id really folds to an active
        // mission, so a reverted `is_safe_id` guard would ACCEPT it.
        assert!(
            mission_status(&repo_root, traversal_id)
                .is_some_and(|status| !crate::mission_catalog::is_terminal_status(status)),
            "fixture: traversal target must fold as an active mission"
        );

        for id in [traversal_id, "a/b", r"a\b", "C:escape"] {
            let error = resolve_active_mission(&repo_root, Some(id))
                .unwrap_err()
                .to_string();
            assert!(error.contains("unknown mission"), "{id}: {error}");
        }
    }

    #[test]
    fn resolve_terminal_mission_is_an_error() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-done", true);
        let err = resolve_active_mission(tmp.path(), Some("m-done"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("active missions"),
            "honest error, not false success: {err}"
        );
    }

    #[test]
    fn resolve_bare_uses_the_single_active_mission() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-only", false);
        // A terminal sibling does not make the bare form ambiguous.
        seed_mission(tmp.path(), "m-done", true);
        assert_eq!(resolve_active_mission(tmp.path(), None).unwrap(), "m-only");
    }

    #[test]
    fn resolve_bare_with_no_active_mission_is_an_error() {
        let tmp = TempDir::new().unwrap();
        assert!(resolve_active_mission(tmp.path(), None).is_err());
    }

    #[test]
    fn resolve_bare_with_several_active_missions_refuses_and_lists_them() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-a", false);
        seed_mission(tmp.path(), "m-b", false);
        let err = resolve_active_mission(tmp.path(), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("several active missions"), "{err}");
        assert!(
            err.contains("m-a") && err.contains("m-b"),
            "candidates listed: {err}"
        );
    }

    // Symlink-creating tests are unix-only, exactly like the lessons guard's
    // tests; Windows needs privileges to create symlinks.

    #[cfg(unix)]
    #[test]
    fn resolve_explicit_mission_refuses_a_symlinked_mission_dir() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let elsewhere = TempDir::new().unwrap();
        // The symlink target holds a foldable ACTIVE mission, so a reverted
        // guard would ACCEPT the id instead of refusing it.
        seed_mission(elsewhere.path(), "m-evil", false);
        let missions = tmp.path().join(".kranz").join("missions");
        std::fs::create_dir_all(&missions).unwrap();
        symlink(
            elsewhere
                .path()
                .join(".kranz")
                .join("missions")
                .join("m-evil"),
            missions.join("m-evil"),
        )
        .unwrap();
        let err = resolve_active_mission(tmp.path(), Some("m-evil"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("unknown mission"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn enqueue_refuses_a_symlinked_mission_dir_without_touching_the_target() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        let elsewhere = TempDir::new().unwrap();
        let missions = tmp.path().join(".kranz").join("missions");
        std::fs::create_dir_all(&missions).unwrap();
        symlink(elsewhere.path(), missions.join("m-evil")).unwrap();
        let paths = MissionPaths::new(tmp.path(), "m-evil");
        let err = enqueue(&paths, &ControlCommand::Pause).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
        // Nothing was routed into the target tree.
        assert!(!elsewhere.path().join("control").exists());
    }

    #[cfg(unix)]
    #[test]
    fn enqueue_refuses_a_symlinked_control_dir_without_touching_the_target() {
        use std::os::unix::fs::symlink;
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-1", false);
        let paths = MissionPaths::new(tmp.path(), "m-1");
        let elsewhere = TempDir::new().unwrap();
        symlink(elsewhere.path(), paths.control_dir()).unwrap();
        let err = enqueue(&paths, &ControlCommand::Pause).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
        // Nothing was written into the target dir.
        assert!(std::fs::read_dir(elsewhere.path())
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn rapid_back_to_back_enqueues_drain_in_issue_order() {
        // Failure-mode fixture for the 2026-07-23 CI flake: with a
        // millisecond filename prefix, two commands enqueued in the same
        // millisecond were drained in random-suffix order (resume sorted
        // before pause and red the run). The nanosecond prefix must keep
        // rapid back-to-back enqueues in issue order.
        let tmp = TempDir::new().unwrap();
        let paths = MissionPaths::new(tmp.path(), "m-1");
        for cmd in [
            ControlCommand::Pause,
            ControlCommand::Resume,
            ControlCommand::Pause,
            ControlCommand::Resume,
            ControlCommand::Pause,
        ] {
            enqueue(&paths, &cmd).unwrap();
        }
        let order: Vec<bool> = drain(&paths)
            .unwrap()
            .iter()
            .map(|(_, cmd)| matches!(cmd, ControlCommand::Pause))
            .collect();
        assert_eq!(
            order,
            vec![true, false, true, false, true],
            "rapid enqueues must drain in issue order, never random-suffix order"
        );
    }
}
