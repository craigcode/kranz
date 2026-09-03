//! Mission data layout under the target repo (plan §4):
//!
//! ```text
//! <repo>/.kranz/
//! ├── config.json                 # project config (merged over ~/.kranz/config.json)
//! └── missions/<id>/
//!     ├── plan.json               # approved plan (also committed to mission branch)
//!     ├── events.jsonl            # append-only event log (single writer)
//!     ├── events.jsonl.lock       # exclusive engine lock
//!     ├── state.json              # derived snapshot (cache; rebuildable)
//!     ├── control/                # inbox: CLI/server -> engine ControlCommand files
//!     ├── runs/<runId>.jsonl      # full per-run transcripts
//!     ├── runs/egress-denials.jsonl # fs+net proxy denial records (gitignored runtime)
//!     └── workspace/              # container-provider compose files (gitignored runtime)
//! ```
//!
//! All paths built with std::path so Windows stays first-class (§9).

use crate::error::{EngineError, Result};
use cap_fs_ext::DirExt as _;
use cap_std::ambient_authority;
use cap_std::fs::Dir;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The canonical `.kranz/.gitignore` rules the engine materializes on init
/// (`orchestrator::write_kranz_gitignore`). Single source for the engine's
/// writer and for ready.rs's engine-materialized exception: the file ignores
/// ITSELF (first rule), so it can never be a committed artifact — its rules
/// travel with the tool, not the repo.
pub const KRANZ_GITIGNORE_RULES: &[&str] = &[
    ".gitignore",
    "config.json",
    "missions/*/events.jsonl",
    "missions/*/events.jsonl.lock",
    "missions/*/state.json",
    "missions/*/state.json.tmp",
    "missions/*/estimate.json",
    "missions/*/enqueue-source*.json",
    "missions/*/control/",
    "missions/*/runs/",
    "missions/*/workspace/",
    "slack-threads.json",
    "queue/",
    "hook-status/",
    "tickets/*.status",
    "serve.token",
    "serve.read.token",
];

#[derive(Debug, Clone)]
pub struct MissionPaths {
    pub repo_root: PathBuf,
    pub mission_id: String,
}

impl MissionPaths {
    pub fn new(repo_root: impl Into<PathBuf>, mission_id: impl Into<String>) -> Self {
        Self {
            repo_root: repo_root.into(),
            mission_id: mission_id.into(),
        }
    }

    /// Mission ids that come from untrusted user input are joined into
    /// filesystem paths. Reject separators, `..`, and drive designators before
    /// constructing paths from those ids.
    pub fn is_safe_id(id: &str) -> bool {
        !id.is_empty() && !id.contains(['/', '\\', ':']) && !id.contains("..")
    }

    pub fn kranz_dir(&self) -> PathBuf {
        self.repo_root.join(".kranz")
    }

    pub fn missions_dir(&self) -> PathBuf {
        self.kranz_dir().join("missions")
    }

    pub fn mission_dir(&self) -> PathBuf {
        self.missions_dir().join(&self.mission_id)
    }

    pub fn plan_file(&self) -> PathBuf {
        self.mission_dir().join("plan.json")
    }

    /// Rendered plan markdown (distinct from `plan_file`'s plan.json).
    pub fn plan_md_file(&self) -> PathBuf {
        self.mission_dir().join("plan.md")
    }

    /// Mission report markdown, written on completion.
    pub fn report_file(&self) -> PathBuf {
        self.mission_dir().join("report.md")
    }

    /// Approval-time cost estimate (gitignored runtime bookkeeping, like
    /// `state.json`): persisted at plan approval / revision so the completion
    /// report can compare actual cost against the exact estimate the operator
    /// approved, rather than one recomputed against a later corpus or config.
    pub fn estimate_file(&self) -> PathBuf {
        self.mission_dir().join("estimate.json")
    }

    /// Per-mission research evidence artifact (repo-knowledge-store slice 1),
    /// committed beside `plan.md` on the mission branch when a plan is approved.
    pub fn research_file(&self) -> PathBuf {
        self.mission_dir().join("research.md")
    }

    pub fn events_file(&self) -> PathBuf {
        self.mission_dir().join("events.jsonl")
    }

    pub fn lock_file(&self) -> PathBuf {
        self.mission_dir().join("events.jsonl.lock")
    }

    pub fn state_file(&self) -> PathBuf {
        self.mission_dir().join("state.json")
    }

    pub fn control_dir(&self) -> PathBuf {
        self.mission_dir().join("control")
    }

    pub fn runs_dir(&self) -> PathBuf {
        self.mission_dir().join("runs")
    }

    /// Repo-level (not per-mission) directory of captured cross-mission
    /// lessons; deliberately survives `kranz clean` and mission deletion.
    pub fn lessons_dir(&self) -> PathBuf {
        self.kranz_dir().join("lessons")
    }

    /// Append-only manifest of captured lessons in capture order.
    pub fn lessons_index(&self) -> PathBuf {
        self.lessons_dir().join("index.md")
    }

    pub fn transcript_file(&self, run_id: &str) -> PathBuf {
        self.runs_dir().join(format!("{run_id}.jsonl"))
    }

    /// Mission-shared egress-denial JSONL the per-run egress proxy appends to
    /// (`fs+net` sessions; see `crate::egress_proxy`). Runtime path under the
    /// gitignored `runs/` dir; v1 correlation is mission-level.
    pub fn egress_denials_file(&self) -> PathBuf {
        self.runs_dir().join("egress-denials.jsonl")
    }

    /// Relative transcript path recorded in events/state (stable across hosts).
    pub fn transcript_rel(run_id: &str) -> String {
        format!("runs/{run_id}.jsonl")
    }

    /// List mission ids present under a repo (sorted), lenient: an unreadable
    /// missions dir lists as empty, and an erroring directory entry (fd
    /// exhaustion, mid-deletion races, permission flaps) is skipped while the
    /// REST are kept — one bad entry must not collapse the whole listing to
    /// "no missions". Use [`Self::try_list_missions`] when the caller must
    /// distinguish "no missions" from "could not list missions" (e.g. before
    /// pruning per-mission bookkeeping keyed on this listing).
    ///
    /// Symlinks are never followed: a symlinked `.kranz` or `missions` dir
    /// lists as empty (the fallible variant refuses with an error), and a
    /// symlinked mission-dir entry is excluded rather than resolved into
    /// another repository's tree.
    pub fn list_missions(repo_root: &Path) -> Vec<String> {
        let Ok(Some(dir)) = missions_dir_no_follow(repo_root) else {
            return Vec::new();
        };
        let Ok(rd) = std::fs::read_dir(dir) else {
            return Vec::new();
        };
        let mut out: Vec<String> = rd
            .flatten()
            // `file_type` does not follow symlinks: a symlinked mission dir
            // is not a mission — exclude it, never resolve through it.
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
            .filter_map(|entry| entry.file_name().to_str().map(str::to_string))
            .collect();
        out.sort();
        out
    }

    /// List mission ids present under a repo (sorted), distinguishing
    /// filesystem errors from a genuinely empty listing.
    ///
    /// A missing missions dir is `Ok(vec![])` — the repo simply has no
    /// missions yet. Any other `read_dir` failure, or an erroring directory
    /// entry (fd exhaustion, mid-deletion races, permission flaps), is `Err`:
    /// a transient error must not masquerade as "every mission was deleted".
    /// A symlinked `.kranz`/`missions` dir is `Err` too — it must refuse,
    /// never be followed into another repository's tree.
    pub fn try_list_missions(repo_root: &Path) -> std::io::Result<Vec<String>> {
        let Some(dir) = missions_dir_no_follow(repo_root)? else {
            return Ok(Vec::new());
        };
        let rd = std::fs::read_dir(dir)?;
        let mut out = Vec::new();
        for entry in rd {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    out.push(name.to_string());
                }
            }
        }
        out.sort();
        Ok(out)
    }

    // -----------------------------------------------------------------------
    // No-follow mission path resolution (P1 mission-path-no-follow)
    //
    // The same capability-based no-follow idiom as the lessons provenance
    // guard (`crate::lessons`): every component of `.kranz/missions/<id>` is
    // inspected with `symlink_metadata` (the link itself, never its target)
    // and then opened with `open_dir_nofollow`, so a symlinked component is
    // REFUSED with a clear `EngineError::InvalidState` — never followed into
    // another repository's state, transcripts, or control inbox.
    // -----------------------------------------------------------------------

    /// Refuse when any component of `<repo>/.kranz/missions/<id>` is a
    /// symlink (or otherwise not a real directory). An ABSENT component is
    /// not a refusal — callers keep their own missing-mission handling
    /// (404s, "unknown mission" errors); only a symlinked component — one
    /// that would be followed into another tree — must fail here.
    pub fn require_no_follow(&self) -> Result<()> {
        match self.open_mission_dir_nofollow(false) {
            Ok(_) => Ok(()),
            Err(EngineError::Io(e)) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Open `<repo>/.kranz/missions/<id>` as a capability pinned beneath
    /// components that are provably NOT symlinks. With `create`, missing
    /// components are created as plain directories (the no-follow counterpart
    /// of `create_dir_all` for the mission tree); without it, a missing
    /// component is an `io` `NotFound` error. A symlinked component is always
    /// [`unsafe_mission_dir_path`], never a follow.
    pub(crate) fn open_mission_dir_nofollow(&self, create: bool) -> Result<Dir> {
        if !Self::is_safe_id(&self.mission_id) {
            return Err(unsafe_mission_dir_path(&self.mission_dir()));
        }
        let mut dir = Dir::open_ambient_dir(&self.repo_root, ambient_authority())?;
        let mut walked = self.repo_root.clone();
        for segment in [".kranz", "missions", self.mission_id.as_str()] {
            walked.push(segment);
            dir = open_child_dir_nofollow(&dir, segment, &walked, create)?;
        }
        Ok(dir)
    }
}

/// Open `name` for reading RELATIVE to an already-pinned capability dir
/// with `FollowSymlinks::No` — the final step of the pinned-chain reads
/// ([`MissionPaths::open_mission_file_read_nofollow`] /
/// [`MissionPaths::open_ticket_file_read_nofollow`]). `NotFound` passes
/// through as `io` (callers keep missing-file handling); every other
/// failure maps to the mission-refusal error.
#[cfg(unix)]
fn open_file_nofollow_under<P: AsRef<Path>>(
    dir: &Dir,
    name: P,
    display_path: &Path,
) -> Result<std::fs::File> {
    use cap_fs_ext::OpenOptionsFollowExt as _;
    use cap_primitives::fs::FollowSymlinks;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    dir.open_with(name, &options)
        .map(|file| file.into_std())
        .map_err(|e| {
            if e.kind() == ErrorKind::NotFound {
                e.into()
            } else {
                unsafe_mission_dir_path(display_path)
            }
        })
}

/// One [`MissionPaths::open_mission_dir_nofollow`] step: refuse a `name` that
/// exists but is not a real directory (a symlink most of all), create it when
/// permitted and missing, then open it with `open_dir_nofollow` — the open is
/// the authoritative no-follow check, the metadata pass only shapes the error.
fn open_child_dir_nofollow(parent: &Dir, name: &str, walked: &Path, create: bool) -> Result<Dir> {
    match parent.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Ok(_) => return Err(unsafe_mission_dir_path(walked)),
        Err(e) if e.kind() == ErrorKind::NotFound && create => {
            match parent.create_dir(name) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
                Err(e) => return Err(e.into()),
            }
            // Lost a race or an attacker planted the name: re-verify.
            match parent.symlink_metadata(name) {
                Ok(metadata) if metadata.file_type().is_dir() => {}
                Ok(_) => return Err(unsafe_mission_dir_path(walked)),
                Err(e) => return Err(e.into()),
            }
        }
        Err(e) => return Err(e.into()),
    }
    parent
        .open_dir_nofollow(name)
        .map_err(|_| unsafe_mission_dir_path(walked))
}

/// Create `name` beneath `dir` (a capability already opened no-follow) when
/// missing, and verify the result is a REAL directory — a symlinked entry
/// (`control/`, `runs/` planted inside a genuine mission dir) is refused,
/// never followed.
pub(crate) fn create_real_subdir(dir: &Dir, name: &str, full_path: &Path) -> Result<()> {
    match dir.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_dir() => return Ok(()),
        Ok(_) => return Err(unsafe_mission_dir_path(full_path)),
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    match dir.create_dir(name) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => match dir.symlink_metadata(name) {
            Ok(metadata) if metadata.file_type().is_dir() => Ok(()),
            Ok(_) => Err(unsafe_mission_dir_path(full_path)),
            Err(e) => Err(e.into()),
        },
        Err(e) => Err(e.into()),
    }
}

/// Open a real child directory beneath an already-pinned capability,
/// optionally creating it. The returned capability is the one callers must
/// retain for subsequent reads, writes, removals, and renames; going back to
/// the absolute display path would reintroduce a parent-swap window.
pub(crate) fn open_real_subdir(
    dir: &Dir,
    name: &str,
    full_path: &Path,
    create: bool,
) -> Result<Dir> {
    open_child_dir_nofollow(dir, name, full_path, create)
}

/// Locate the LAST `.kranz` component whose suffix has a recognized runtime
/// layout. Using the last matching component matters when a perfectly valid
/// repository itself lives below an unrelated ancestor named `.kranz`.
fn mission_layout_anchor(components: &[std::path::Component<'_>]) -> Option<usize> {
    components
        .iter()
        .enumerate()
        .rev()
        .find_map(|(idx, component)| {
            if !matches!(component, std::path::Component::Normal(os) if *os == ".kranz") {
                return None;
            }
            let after = &components[idx + 1..];
            match after.first() {
                Some(std::path::Component::Normal(os)) if *os == "missions" => {
                    if let Some(std::path::Component::Normal(id)) = after.get(1) {
                        MissionPaths::is_safe_id(&id.to_string_lossy()).then_some(idx)
                    } else {
                        None
                    }
                }
                Some(std::path::Component::Normal(os)) if *os == "tickets" => Some(idx),
                _ => None,
            }
        })
}

/// Pin the parent of `path` and return its leaf name. Mission-layout paths
/// are resolved from the trusted repository prefix and every component from
/// `.kranz` downward is opened no-follow. This is the write-side counterpart
/// of [`open_read_nofollow`]: callers perform the eventual open/rename/remove
/// relative to the returned capability, never through the absolute path that
/// was checked.
///
/// Paths outside the mission/ticket layout are used by unit-test scratch
/// fixtures and retain the weaker canonical-parent tier described by
/// [`open_read_nofollow`].
pub(crate) fn open_parent_nofollow(path: &Path) -> Result<(Dir, OsString)> {
    let name = path.file_name().map(OsString::from).ok_or_else(|| {
        EngineError::InvalidState(format!("path {} has no file name", path.display()))
    })?;
    let components: Vec<_> = path.components().collect();
    let anchor_info = mission_layout_anchor(&components);

    if let Some(idx) = anchor_info {
        let anchor: PathBuf = components[..idx].iter().collect();
        let anchor = if anchor.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            anchor
        };
        let mut dir = Dir::open_ambient_dir(&anchor, ambient_authority())?;
        let parent_end = components.len().saturating_sub(1);
        let mut walked = anchor;
        for component in &components[idx..parent_end] {
            let std::path::Component::Normal(component) = component else {
                return Err(unsafe_mission_dir_path(path));
            };
            let Some(component) = component.to_str() else {
                return Err(unsafe_mission_dir_path(path));
            };
            walked.push(component);
            dir = open_child_dir_nofollow(&dir, component, &walked, false)?;
        }
        return Ok((dir, name));
    }

    let parent = path.parent().ok_or_else(|| {
        EngineError::InvalidState(format!("path {} has no parent", path.display()))
    })?;
    let parent = parent.canonicalize()?;
    Ok((Dir::open_ambient_dir(parent, ambient_authority())?, name))
}

/// Open `path` for reading with the mission-tree chain pinned
/// (7th-pass review): when the path carries the mission layout
/// (`<root>/.kranz/missions/<id>/...` or `<root>/.kranz/tickets/...`), the
/// prefix BEFORE `.kranz` is taken as the trusted anchor (ambient
/// authority — the same trust basis [`MissionPaths`] uses for its repo
/// root), and every component from `.kranz` down is opened per-component
/// no-follow, the final file with `FollowSymlinks::No`. No
/// canonicalization of the untrusted region anywhere: the earlier
/// canonicalize-then-walk shape resolved a hostile parent symlink BEFORE
/// the no-follow discipline began.
///
/// Paths OUTSIDE the mission layout (test scratch dirs under macOS
/// `/var`, which is itself a system symlink) keep the weaker
/// canonicalize-then-walk tier: those regions are outside the mission
/// threat model, and canonicalizing them is the only way macOS tempdirs
/// resolve at all. Off-unix there is no `O_NOFOLLOW`; fall back to
/// check-then-open (Windows symlink creation needs privileges).
pub fn open_read_nofollow(path: &Path) -> Result<std::fs::File> {
    #[cfg(unix)]
    {
        use cap_fs_ext::DirExt as _;

        let components: Vec<_> = path.components().collect();
        // The mission layout yields a trusted prefix + an untrusted suffix
        // to pin; anything else takes the weaker tier.
        let anchor_info = mission_layout_anchor(&components);

        if let Some(idx) = anchor_info {
            // Trusted anchor: everything before `.kranz` ("/" when relative).
            let anchor: PathBuf = components[..idx].iter().collect();
            let anchor = if anchor.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                anchor
            };
            let mut dir = Dir::open_ambient_dir(&anchor, ambient_authority())?;
            let mut components_iter = components[idx..].iter().peekable();
            while let Some(component) = components_iter.next() {
                let std::path::Component::Normal(name) = component else {
                    return Err(unsafe_mission_dir_path(path));
                };
                if components_iter.peek().is_some() {
                    dir = dir.open_dir_nofollow(name).map_err(|error| {
                        if error.kind() == ErrorKind::NotFound {
                            EngineError::Io(error)
                        } else {
                            unsafe_mission_dir_path(path)
                        }
                    })?;
                } else {
                    return open_file_nofollow_under(&dir, name, path);
                }
            }
            return Err(unsafe_mission_dir_path(path));
        }

        // Weaker tier (out-of-model paths): canonicalize the parent, pin
        // the canonical ancestors, open the final no-follow.
        open_read_nofollow_weaker_tier(path)
    }
    #[cfg(not(unix))]
    {
        ensure_absent_or_regular_file(path)?;
        Ok(std::fs::File::open(path)?)
    }
}

/// The pre-7th-pass behavior for paths outside the mission layout:
/// canonicalize the parent (resolves macOS `/var`-style system symlinks),
/// pin the canonical ancestors per-component no-follow, open the final
/// file with `FollowSymlinks::No`. Used only for out-of-model paths (test
/// scratch), where canonicalization is required for tempdirs to resolve at
/// all; the mission-tree paths never reach here.
#[cfg(unix)]
fn open_read_nofollow_weaker_tier(path: &Path) -> Result<std::fs::File> {
    use cap_fs_ext::DirExt as _;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| unsafe_mission_dir_path(path))?;
    let canonical_parent = std::fs::canonicalize(parent)?;
    let relative = canonical_parent
        .strip_prefix("/")
        .map_err(|_| unsafe_mission_dir_path(path))?;
    let mut dir = Dir::open_ambient_dir("/", ambient_authority())?;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(unsafe_mission_dir_path(path));
        };
        dir = dir
            .open_dir_nofollow(name)
            .map_err(|_| unsafe_mission_dir_path(path))?;
    }
    open_file_nofollow_under(&dir, file_name, path)
}

/// Refuse `path` when it exists and is anything but a regular file — a
/// symlink most of all. `symlink_metadata` (never `metadata`) inspects the
/// link itself, so a symlinked runtime file (events.jsonl, state.json, the
/// lock file) is rejected at open time instead of being read or written
/// through into another tree. An absent path is `Ok`: the caller's own
/// open/read produces its usual `NotFound`.
///
/// This is a CHECK ONLY: a caller that opens the file afterwards has a
/// check-then-open window a concurrent writer could swap a symlink into.
/// Readers should use [`open_read_nofollow`] instead, which closes that
/// window on unix.
#[cfg(any(not(unix), test))]
pub(crate) fn ensure_absent_or_regular_file(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(EngineError::InvalidState(format!(
            "refusing mission runtime file that is not a regular file: {}",
            path.display()
        ))),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// The `<repo>/.kranz/missions` dir for LISTING: `Ok(None)` when a component
/// is absent (no missions yet), `Err` when a present component is not a real
/// directory — a symlinked `.kranz`/`missions` must refuse, never be followed
/// into another repository's tree.
fn missions_dir_no_follow(repo_root: &Path) -> std::io::Result<Option<PathBuf>> {
    let kranz = repo_root.join(".kranz");
    let missions = kranz.join("missions");
    for path in [&kranz, &missions] {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_dir() => {}
            Ok(_) => {
                return Err(std::io::Error::other(format!(
                    "refusing to list missions through a symlinked or non-directory path: {}",
                    path.display()
                )));
            }
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        }
    }
    Ok(Some(missions))
}

/// The refusal surfaced when a mission path component is a symlink (or
/// otherwise not a real directory) — the same refusal family as the lessons
/// provenance guard: a clear `EngineError::InvalidState`, never a panic and
/// never a silent follow.
fn unsafe_mission_dir_path(path: &Path) -> EngineError {
    EngineError::InvalidState(format!(
        "refusing mission path with a symlinked or non-directory component: {}",
        path.display()
    ))
}

/// Project config file path.
pub fn project_config(repo_root: &Path) -> PathBuf {
    repo_root.join(".kranz").join("config.json")
}

/// Global config file path (`<global kranz dir>/config.json`), None if no
/// home dir.
pub fn global_config() -> Option<PathBuf> {
    global_kranz_dir().map(|dir| dir.join("config.json"))
}

/// The operator's global kranz directory: `$KRANZ_HOME` when set and
/// non-empty, else `~/.kranz` (`%USERPROFILE%\.kranz` on Windows), `None`
/// when neither resolves.
///
/// `KRANZ_HOME` exists so a test harness or a CI runner can point every
/// global store (config, authority keys, seal floors) at a scratch directory
/// instead of the operator's real home. It is read from the ENGINE process's
/// own environment; agent sessions spawn from a cleared environment
/// (`agent_env`), so a session cannot redirect the key directory it is
/// denied from reading.
pub fn global_kranz_dir() -> Option<PathBuf> {
    // Resolved once per process. The authority key, the seal floors, and
    // the control marks all hang off this directory, and a signer and a
    // verifier in the same process must agree on it even if something
    // rewrites `HOME` in between (a test does; a wrapper script could).
    static GLOBAL: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    GLOBAL
        .get_or_init(|| {
            if let Some(dir) = std::env::var_os("KRANZ_HOME") {
                if !dir.is_empty() {
                    return Some(PathBuf::from(dir));
                }
            }
            std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(|h| PathBuf::from(h).join(".kranz"))
        })
        .clone()
}

// ---------------------------------------------------------------------------
// Per-repository authority key (audit 2026-09-01 C1 / H6)
// ---------------------------------------------------------------------------

/// Length of a freshly minted authority key.
pub const AUTHORITY_KEY_LEN: usize = 32;

/// Stable per-repository id for authority material: sha256 of the CANONICAL
/// repo root path, so `/repo`, `/repo/../repo`, and macOS `/var` vs
/// `/private/var` all resolve to one key instead of minting a second one that
/// would make previously signed control files and log lines unverifiable.
fn repo_fingerprint(repo_root: &Path) -> String {
    let canonical = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    crate::standards_waiver::sha256_hex(canonical.as_os_str().as_encoded_bytes())
}

/// Where this repository's authority key lives:
/// `<global kranz dir>/keys/<repo fingerprint>.key`.
///
/// OUTSIDE the repository on purpose. The key authenticates the control inbox
/// (`control::enqueue`/`control::drain`) and MACs `events.jsonl`, so an agent
/// process that can read it can forge operator consent. Keeping it in the
/// operator's home puts it inside the sandbox's authority-read-deny set
/// (`sandbox::authority_read_deny_paths`) and, when the sandbox is off,
/// behind the agent CLI's own `Read(~/.kranz/**)` deny rule
/// (`permissions::AUTHORITY_DENY`).
pub fn authority_key_path(repo_root: &Path) -> Option<PathBuf> {
    global_kranz_dir().map(|dir| {
        dir.join("keys")
            .join(format!("{}.key", repo_fingerprint(repo_root)))
    })
}

/// Where a mission's SEAL FLOOR lives: the first `events.jsonl` seq that must
/// carry integrity fields, recorded outside the repository beside the key.
///
/// This is the anchor that makes the event log's integrity non-optional. The
/// log itself cannot carry the requirement: an attacker who rewrites every
/// line, stripping `h` and `m` from all of them, produces something
/// indistinguishable from a log written before integrity existed, and a
/// reader with only the file in front of it has to accept it. A floor the
/// attacker cannot write says "from seq N on, unsealed is forged", and it
/// grandfathers the lines below N that legitimately predate the key.
pub fn seal_floor_path(repo_root: &Path, mission_id: &str) -> Option<PathBuf> {
    if !MissionPaths::is_safe_id(mission_id) {
        return None;
    }
    global_kranz_dir().map(|dir| {
        dir.join("seals")
            .join(repo_fingerprint(repo_root))
            .join(mission_id)
    })
}

/// The recorded seal floor for one mission, `None` when none was ever
/// recorded (so every line is grandfathered).
pub fn read_seal_floor(repo_root: &Path, mission_id: &str) -> Option<u64> {
    let path = seal_floor_path(repo_root, mission_id)?;
    std::fs::read_to_string(&path).ok()?.trim().parse().ok()
}

/// Where a mission's HIGH-WATER MARK lives: the highest seq the engine has
/// durably written, recorded beside the seal floor and, like it, outside the
/// repository.
///
/// The seal floor and the chain make a forged or edited line detectable; the
/// high-water mark makes a TRUNCATED log detectable. Cutting `events.jsonl`
/// at a line boundary leaves a valid chain and a valid seq run, so nothing in
/// the file itself can say lines are missing. `state.json` carries a
/// `last_seq`, but it sits in the repository next to the log, so whoever can
/// truncate one can rewrite the other. A mark the same writer cannot reach
/// is the only witness that survives.
pub fn high_water_path(repo_root: &Path, mission_id: &str) -> Option<PathBuf> {
    // Own subdirectory, never a suffix on the floor's name: mission ids may
    // contain dots, so `with_extension` would fold `release-1.2` and
    // `release-1.3` onto one mark and make mission `foo`'s mark mission
    // `foo.hwm`'s floor (follow-up review F-5).
    seal_floor_path(repo_root, mission_id).map(|p| {
        let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
        dir.join("hwm").join(mission_id)
    })
}

/// Where a mission's CONTROL MARK lives: the name of the last control file
/// the engine acknowledged, recorded beside the high-water mark. Control
/// file names are `<zero-padded-nanos>-<8-hex>.json`, so they order
/// lexicographically by creation time and the mark never moves backwards.
///
/// The mark is what makes a signed control file single-use. The signature
/// binds the file to its name, and the engine refuses any name at or below
/// the mark, so a captured `approve-grant` re-dropped after the drain
/// deleted it authenticates but is refused as a replay (follow-up review
/// F-1).
pub fn control_mark_path(repo_root: &Path, mission_id: &str) -> Option<PathBuf> {
    seal_floor_path(repo_root, mission_id).map(|p| {
        let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
        dir.join("ctl").join(mission_id)
    })
}

/// The recorded control mark, `None` when none was ever recorded.
pub fn read_control_mark(repo_root: &Path, mission_id: &str) -> Option<String> {
    let path = control_mark_path(repo_root, mission_id)?;
    let text = std::fs::read_to_string(&path).ok()?;
    let name = text.trim();
    (!name.is_empty()).then(|| name.to_string())
}

/// Record `name` as the last acknowledged control file. Never lowers.
pub fn record_control_mark(repo_root: &Path, mission_id: &str, name: &str) -> Result<()> {
    let Some(path) = control_mark_path(repo_root, mission_id) else {
        return Ok(());
    };
    if read_control_mark(repo_root, mission_id).is_some_and(|current| current.as_str() >= name) {
        return Ok(());
    }
    write_mark(&path, name)
}

/// The recorded high-water mark, `None` when none was ever recorded.
pub fn read_high_water(repo_root: &Path, mission_id: &str) -> Option<u64> {
    let path = high_water_path(repo_root, mission_id)?;
    std::fs::read_to_string(&path).ok()?.trim().parse().ok()
}

/// Record `seq` as the highest durably written event. Never lowers an
/// existing mark. Written through a tmp file and rename so a crash between
/// the log write and this one leaves the OLD mark, which is at most one
/// event behind and therefore never accuses a healthy log.
pub fn record_high_water(repo_root: &Path, mission_id: &str, seq: u64) -> Result<()> {
    let Some(path) = high_water_path(repo_root, mission_id) else {
        return Ok(());
    };
    if read_high_water(repo_root, mission_id).is_some_and(|current| current >= seq) {
        return Ok(());
    }
    write_mark(&path, &seq.to_string())
}

/// Write a small witness file under the seals tree: parent dirs `0700`,
/// file `0600`, tmp file plus rename so a crash leaves the previous value.
fn write_mark(path: &Path, value: &str) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| EngineError::InvalidState("mark path has no parent".to_string()))?;
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut cursor = Some(dir);
        // Lock every directory we may have just created up to the seals
        // root: the marks are authority material like the key.
        for _ in 0..3 {
            let Some(d) = cursor else { break };
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
            if d.file_name().is_some_and(|n| n == "seals") {
                break;
            }
            cursor = d.parent();
        }
    }
    let tmp = dir.join(format!(".mark.tmp-{}", uuid::Uuid::new_v4().as_simple()));
    let write = || -> std::io::Result<()> {
        use std::io::Write as _;
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(value.as_bytes())?;
        file.sync_data()?;
        Ok(())
    };
    if let Err(error) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(error.into());
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error.into());
    }
    Ok(())
}

/// Record `seq` as this mission's seal floor, once. An existing floor is
/// never moved: raising it would grandfather away lines that were sealed, and
/// lowering it would condemn lines that legitimately were not.
pub fn record_seal_floor(repo_root: &Path, mission_id: &str, seq: u64) -> Result<()> {
    let Some(path) = seal_floor_path(repo_root, mission_id) else {
        return Ok(());
    };
    let dir = path
        .parent()
        .ok_or_else(|| EngineError::InvalidState("seal floor path has no parent".to_string()))?;
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Lock both the per-repo dir and the `seals` root above it: the
        // floor is what makes a stripped log detectable, so its directory
        // is authority material like the key's.
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        if let Some(seals_root) = dir.parent() {
            let _ = std::fs::set_permissions(seals_root, std::fs::Permissions::from_mode(0o700));
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            use std::io::Write as _;
            file.write_all(seq.to_string().as_bytes())?;
            file.sync_data()?;
            Ok(())
        }
        // Already recorded: whoever got there first is authoritative.
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Sidecar beside each key naming the repository it was minted for, so the
/// pruner below can tell an orphaned scratch-repo key from a live one. The
/// key file itself is named by a one-way fingerprint, which is right for the
/// deny sets but leaves nothing to stat.
fn key_owner_path(key_path: &Path) -> PathBuf {
    key_path.with_extension("path")
}

fn record_key_owner(key_path: &Path, repo_root: &Path) {
    let canonical = std::fs::canonicalize(repo_root).unwrap_or_else(|_| repo_root.to_path_buf());
    let _ = std::fs::write(
        key_owner_path(key_path),
        canonical.as_os_str().as_encoded_bytes(),
    );
}

/// Remove keys (and their seal floors) minted for repositories under the
/// system temp root that no longer exist.
///
/// Every mission a test creates in a `tempfile` repo mints a key, and the
/// repo is gone the moment the test ends, so without this the operator's key
/// directory grows by one file per test forever. Scoped to the temp root on
/// purpose: a key for a real repository is never pruned, even when its path
/// is momentarily absent (an unmounted volume), because losing the key
/// strands every log line and control file it signed. Best effort, never an
/// error: pruning is housekeeping, minting is the job.
fn prune_orphan_temp_keys(keys_dir: &Path) {
    let temp_root = std::env::temp_dir();
    let temp_root = std::fs::canonicalize(&temp_root).unwrap_or(temp_root);
    let Ok(entries) = std::fs::read_dir(keys_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let owner_path = entry.path();
        if owner_path.extension().and_then(|e| e.to_str()) != Some("path") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&owner_path) else {
            continue;
        };
        // The sidecar was written as encoded bytes of an OsStr; `from_utf8`
        // is only a lossy view for the containment check below, never an
        // identity we act on.
        let repo = PathBuf::from(String::from_utf8_lossy(&bytes).into_owned());
        if !repo.starts_with(&temp_root) || repo.exists() {
            continue;
        }
        // The sidecar names the key it belongs to only through the
        // fingerprint. A sidecar whose recorded path does not hash to its
        // own filename was not written by this code, so it deletes nothing
        // (follow-up review F-4: a planted `<real fp>.path` naming a dead
        // temp path must not take a real repository's key with it).
        let stem = owner_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string();
        let expected = crate::standards_waiver::sha256_hex(repo.as_os_str().as_encoded_bytes());
        if stem != expected {
            continue;
        }
        let key_path = owner_path.with_extension("key");
        let _ = std::fs::remove_file(&key_path);
        let _ = std::fs::remove_file(&owner_path);
        // The seal directory is left alone on purpose: a stale one costs
        // bytes, a deleted one costs a witness.
    }
}

/// Read this repository's authority key, or `None` when there is none to
/// read (no home dir, no key minted yet, unreadable file).
///
/// The READER form: a verifier that cannot load the key degrades to the
/// weaker check it can still perform and says so, but it never mints key
/// material as a side effect of reading.
pub fn load_authority_key(repo_root: &Path) -> Option<Vec<u8>> {
    let path = authority_key_path(repo_root)?;
    let bytes = std::fs::read(&path).ok()?;
    (bytes.len() >= AUTHORITY_KEY_LEN).then_some(bytes)
}

/// Read this repository's authority key, minting it on first use.
///
/// The WRITER/SIGNER form. Directory `0700`, file `0600`, written through a
/// tmp file + rename so a crash never leaves a truncated key at the stable
/// path (the same shape `kranz serve` uses for `serve.token`). A racing
/// creator wins harmlessly: `create_new` fails `AlreadyExists` and we re-read
/// whatever landed, so two processes never disagree about the key.
///
/// The 32 bytes come from two `uuid` v4 values. `uuid` is already a workspace
/// dependency drawing on the OS CSPRNG, so this adds no crate to the set
/// `deny.toml` audits.
pub fn load_or_create_authority_key(repo_root: &Path) -> Result<Vec<u8>> {
    let path = authority_key_path(repo_root).ok_or_else(|| {
        EngineError::InvalidState(
            "cannot resolve the operator kranz directory for the authority key".to_string(),
        )
    })?;
    if let Some(existing) = load_authority_key(repo_root) {
        return Ok(existing);
    }
    let dir = path
        .parent()
        .ok_or_else(|| EngineError::InvalidState("authority key path has no parent".to_string()))?;
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Best effort: an operator who deliberately widened ~/.kranz is not
        // overridden loudly, but a directory we just created is ours to lock.
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }

    let key = {
        let mut key = Vec::with_capacity(AUTHORITY_KEY_LEN);
        key.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        key.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        key
    };
    let tmp = dir.join(format!(".key.tmp-{}", uuid::Uuid::new_v4().as_simple()));
    let write_tmp = || -> std::io::Result<()> {
        use std::io::Write as _;
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(&key)?;
        file.sync_data()?;
        Ok(())
    };
    if let Err(error) = write_tmp() {
        let _ = std::fs::remove_file(&tmp);
        return Err(error.into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    // `rename` would clobber a key a racing process just minted, which would
    // strand every file it had already signed. Link-then-unlink refuses
    // instead, and we adopt the winner's key.
    match std::fs::hard_link(&tmp, &path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            record_key_owner(&path, repo_root);
            prune_orphan_temp_keys(dir);
            Ok(key)
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&tmp);
            load_authority_key(repo_root).ok_or_else(|| {
                EngineError::InvalidState(format!(
                    "authority key {} exists but could not be read",
                    path.display()
                ))
            })
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lessons_paths_are_repo_level_not_per_mission() {
        let paths = MissionPaths::new("/repo", "m-abc123");
        assert_eq!(paths.lessons_dir(), PathBuf::from("/repo/.kranz/lessons"));
        assert_eq!(
            paths.lessons_index(),
            PathBuf::from("/repo/.kranz/lessons/index.md")
        );
    }

    #[test]
    fn plan_md_and_report_paths_are_per_mission() {
        let paths = MissionPaths::new("/repo", "m-abc123");
        assert_eq!(
            paths.plan_md_file(),
            PathBuf::from("/repo/.kranz/missions/m-abc123/plan.md")
        );
        assert_eq!(
            paths.report_file(),
            PathBuf::from("/repo/.kranz/missions/m-abc123/report.md")
        );
    }

    #[test]
    fn try_list_missions_distinguishes_missing_dir_from_real_listing() {
        let tmp = tempfile::TempDir::new().unwrap();
        // No .kranz/missions dir at all: genuinely empty, not an error.
        assert_eq!(
            MissionPaths::try_list_missions(tmp.path()).unwrap(),
            Vec::<String>::new()
        );
        // With mission subdirs (plus a stray file that must be skipped):
        // listed and sorted.
        let missions = tmp.path().join(".kranz").join("missions");
        std::fs::create_dir_all(missions.join("m-bbb222")).unwrap();
        std::fs::create_dir_all(missions.join("m-aaa111")).unwrap();
        std::fs::write(missions.join("stray.txt"), b"x").unwrap();
        let listed = MissionPaths::try_list_missions(tmp.path()).unwrap();
        assert_eq!(listed, vec!["m-aaa111".to_string(), "m-bbb222".to_string()]);
        // The lenient wrapper agrees on the happy path.
        assert_eq!(MissionPaths::list_missions(tmp.path()), listed);
    }

    #[cfg(unix)]
    #[test]
    fn try_list_missions_reports_errors_instead_of_swallowing_them() {
        // A missions *file* (not dir) makes read_dir fail with a non-NotFound
        // error; the fallible listing must surface it, not return "empty".
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".kranz")).unwrap();
        std::fs::write(tmp.path().join(".kranz").join("missions"), b"not a dir").unwrap();
        assert!(MissionPaths::try_list_missions(tmp.path()).is_err());
        // The lenient listing stays lenient on the same failure: empty, not a
        // panic or an error. (A single erroring dir ENTRY — skipped while the
        // rest are kept — is not portably constructible in a test; the walk
        // uses `.flatten()` to encode that contract.)
        assert_eq!(
            MissionPaths::list_missions(tmp.path()),
            Vec::<String>::new()
        );
    }

    #[test]
    fn safe_id_rejects_path_traversal_shapes() {
        for id in ["", "../m-x", "m-x/../../y", "m-x\\..\\y", "c:m-x", "m-.."] {
            assert!(!MissionPaths::is_safe_id(id), "{id:?} should be unsafe");
        }
        for id in ["m-abc123", "m-2026-07-08", "m_ticket.linked"] {
            assert!(MissionPaths::is_safe_id(id), "{id:?} should be safe");
        }
    }

    // Symlink-creating tests are unix-only, exactly like the lessons guard's
    // tests (`std::os::unix::fs::symlink`); Windows needs privileges to
    // create symlinks, so CI coverage there comes from the no-symlink cases.

    #[cfg(unix)]
    #[test]
    fn list_missions_excludes_symlinked_mission_dirs() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().unwrap();
        let missions = tmp.path().join(".kranz").join("missions");
        std::fs::create_dir_all(missions.join("m-real")).unwrap();
        // A mission dir that is a symlink into another tree is not a mission:
        // excluded, never followed.
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        symlink(&elsewhere, missions.join("m-evil")).unwrap();
        assert_eq!(
            MissionPaths::list_missions(tmp.path()),
            vec!["m-real".to_string()]
        );
        assert_eq!(
            MissionPaths::try_list_missions(tmp.path()).unwrap(),
            vec!["m-real".to_string()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn list_missions_refuses_a_symlinked_missions_dir() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join(".kranz")).unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("m-evil")).unwrap();
        symlink(&elsewhere, tmp.path().join(".kranz").join("missions")).unwrap();
        // The lenient listing stays lenient: empty, not another repo's ids.
        assert_eq!(
            MissionPaths::list_missions(tmp.path()),
            Vec::<String>::new()
        );
        // The fallible listing refuses with a clear error.
        let err = MissionPaths::try_list_missions(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn require_no_follow_refuses_symlinked_components() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().unwrap();
        let missions = tmp.path().join(".kranz").join("missions");
        std::fs::create_dir_all(missions.join("m-real")).unwrap();
        // A real mission dir passes; an absent one is not a refusal (the
        // caller's own missing-mission handling decides).
        assert!(MissionPaths::new(tmp.path(), "m-real")
            .require_no_follow()
            .is_ok());
        assert!(MissionPaths::new(tmp.path(), "m-absent")
            .require_no_follow()
            .is_ok());
        // A symlinked mission dir is refused with a clear error.
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        symlink(&elsewhere, missions.join("m-evil")).unwrap();
        let err = MissionPaths::new(tmp.path(), "m-evil")
            .require_no_follow()
            .unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
        // Unsafe ids never reach the filesystem.
        assert!(MissionPaths::new(tmp.path(), "../x")
            .require_no_follow()
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn require_no_follow_refuses_a_symlinked_kranz_dir() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("missions").join("m-evil")).unwrap();
        symlink(&elsewhere, tmp.path().join(".kranz")).unwrap();
        assert!(MissionPaths::new(tmp.path(), "m-evil")
            .require_no_follow()
            .is_err());
        assert_eq!(
            MissionPaths::list_missions(tmp.path()),
            Vec::<String>::new()
        );
    }

    #[cfg(unix)]
    #[test]
    fn open_mission_dir_nofollow_creates_missing_dirs_but_refuses_symlinks() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().unwrap();
        // Fresh repo: the whole chain is created, mirroring create_dir_all.
        let paths = MissionPaths::new(tmp.path(), "m-new");
        paths.open_mission_dir_nofollow(true).unwrap();
        assert!(paths.mission_dir().is_dir());
        // A planted symlink in place of the mission dir is refused.
        std::fs::remove_dir(paths.mission_dir()).unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        symlink(&elsewhere, paths.mission_dir()).unwrap();
        assert!(paths.open_mission_dir_nofollow(true).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_absent_or_regular_file_refuses_symlinks() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::TempDir::new().unwrap();
        let target = tmp.path().join("target.jsonl");
        std::fs::write(&target, b"secret").unwrap();
        let link = tmp.path().join("link.jsonl");
        symlink(&target, &link).unwrap();
        let err = ensure_absent_or_regular_file(&link).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
        assert!(ensure_absent_or_regular_file(&target).is_ok());
        assert!(ensure_absent_or_regular_file(&tmp.path().join("missing.jsonl")).is_ok());
    }

    // -- authority key (audit 2026-09-01 C1 / H6) --------------------------

    #[test]
    fn the_authority_key_lives_outside_the_repository() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = authority_key_path(tmp.path()).expect("a home dir in tests");
        assert!(
            !path.starts_with(tmp.path()),
            "the key must never sit inside the repo it authenticates: {}",
            path.display()
        );
        assert!(path.parent().unwrap().ends_with("keys"));
    }

    #[test]
    fn the_authority_key_is_stable_per_repo_and_distinct_between_repos() {
        let a = tempfile::TempDir::new().unwrap();
        let b = tempfile::TempDir::new().unwrap();
        let first = load_or_create_authority_key(a.path()).unwrap();
        assert_eq!(first.len(), AUTHORITY_KEY_LEN);
        assert_eq!(
            load_or_create_authority_key(a.path()).unwrap(),
            first,
            "a second call adopts the existing key rather than rotating it"
        );
        assert_ne!(
            load_or_create_authority_key(b.path()).unwrap(),
            first,
            "one repo's key must not authenticate another repo's inbox"
        );
        // A path spelled differently but naming the same tree resolves to the
        // same key, or every file signed under the other spelling would stop
        // verifying.
        let indirect = a
            .path()
            .join(".")
            .join("..")
            .join(a.path().file_name().unwrap());
        assert_eq!(load_or_create_authority_key(&indirect).unwrap(), first);
    }

    #[cfg(unix)]
    #[test]
    fn the_authority_key_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::TempDir::new().unwrap();
        load_or_create_authority_key(tmp.path()).unwrap();
        let path = authority_key_path(tmp.path()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key mode {mode:o}");
        let dir_mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "keys dir mode {dir_mode:o}");
    }

    #[test]
    fn the_seal_floor_is_recorded_once_and_never_moved() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert_eq!(read_seal_floor(tmp.path(), "m-1"), None);
        record_seal_floor(tmp.path(), "m-1", 7).unwrap();
        assert_eq!(read_seal_floor(tmp.path(), "m-1"), Some(7));
        // Raising it would grandfather away lines that were sealed; lowering
        // it would condemn lines that legitimately were not.
        record_seal_floor(tmp.path(), "m-1", 1).unwrap();
        record_seal_floor(tmp.path(), "m-1", 99).unwrap();
        assert_eq!(read_seal_floor(tmp.path(), "m-1"), Some(7));
        assert_eq!(read_seal_floor(tmp.path(), "m-2"), None);
    }

    #[test]
    fn an_unsafe_mission_id_gets_no_seal_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        assert!(seal_floor_path(tmp.path(), "../../escape").is_none());
        assert!(seal_floor_path(tmp.path(), "a/b").is_none());
    }
}
