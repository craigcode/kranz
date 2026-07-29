//! Per-repo, priority-ordered execution queue (design: docs/backlog-and-slack.md §3).
//!
//! Approval enqueues a mission rather than starting it. Entries live as one
//! JSON file each under `.kranz/queue/`, named
//! `<priority>-<paddedSeq>-<missionId>.json` so plain lexicographic filename
//! order equals `(priority, insertion order)`. A monotonic counter file
//! (`.seq`) assigns the sequence number.
//!
//! Per-repo serialization is mandatory: missions share the working tree, so at
//! most one may run at a time in a repo. Queue dispatchers acquire a repo-wide
//! busy guard as part of claiming work, and [`is_repo_busy`] reports that guard
//! (falling back to legacy live `events.jsonl.lock` detection).

use crate::error::{EngineError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

/// Width of the zero-padded sequence field in a queue filename. u64 max is 20
/// digits, so this keeps lexicographic order == numeric order for any seq.
const SEQ_WIDTH: usize = 20;

/// One queued mission.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueEntry {
    pub mission_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket_slug: Option<String>,
    pub priority: u8,
    pub seq: u64,
}

impl QueueEntry {
    /// Filename encoding `(priority, seq)` into lexicographic order.
    fn file_name(&self) -> String {
        format!(
            "{:03}-{:0width$}-{}.json",
            self.priority,
            self.seq,
            self.mission_id,
            width = SEQ_WIDTH
        )
    }
}

/// The `.kranz/queue/` directory for a repo.
pub fn queue_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".kranz").join("queue")
}

/// Path of the monotonic sequence counter file.
fn seq_file(repo_root: &Path) -> PathBuf {
    queue_dir(repo_root).join(".seq")
}

/// Same-process serialization of queue mutations (Slack spawns concurrent
/// approval tasks in one process — review P1).
static LOCAL_MUTATION_LOCK: Mutex<()> = Mutex::new(());

/// Cross-process advisory lock: `.mutate.lock` created with `create_new`
/// (exclusive). Held across the read-seq/write-seq/write-entry critical
/// section so `kranz serve` and `kranz work` cannot interleave. A lock file
/// older than [`LOCK_STALE`] is treated as a crash leftover and stolen —
/// contention here is rare and short, so staleness is unambiguous at that
/// age. Dropped = deleted.
const LOCK_STALE: Duration = Duration::from_secs(10);

struct MutationLock {
    path: PathBuf,
    /// Written into the lock file at acquire; Drop deletes the file only if
    /// it still holds OUR token, so a holder whose lock was stale-stolen
    /// cannot delete the stealer's lock and cascade-break mutual exclusion.
    /// (The read-then-remove in Drop is itself a tiny TOCTOU window —
    /// microseconds against a 10s staleness horizon — accepted for an
    /// advisory lock on a low-contention queue.)
    token: String,
}

impl MutationLock {
    fn acquire(repo_root: &Path) -> Result<MutationLock> {
        let path = queue_dir(repo_root).join(".mutate.lock");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let token = format!(
                "{}.{}",
                std::process::id(),
                LOCK_TOKEN_SEQ.fetch_add(1, Ordering::Relaxed)
            );
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut f) => {
                    use std::io::Write as _;
                    let _ = f.write_all(token.as_bytes());
                    return Ok(MutationLock { path, token });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > LOCK_STALE);
                    if stale {
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if std::time::Instant::now() > deadline {
                        return Err(crate::error::EngineError::Io(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!("queue mutation lock busy: {}", path.display()),
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl Drop for MutationLock {
    fn drop(&mut self) {
        let ours = std::fs::read_to_string(&self.path)
            .map(|c| c == self.token)
            .unwrap_or(false);
        if ours {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Uniquifies lock tokens within one process (pid alone is shared by all
/// tasks in the process).
static LOCK_TOKEN_SEQ: AtomicU64 = AtomicU64::new(0);

/// Reserve the next sequence number: read the counter file, increment, write
/// it back. Falls back to `max(existing entry seq) + 1` if the counter file is
/// missing or corrupt, so a wiped counter can never hand out a duplicate that
/// would reorder existing entries.
fn next_seq(repo_root: &Path) -> Result<u64> {
    let path = seq_file(repo_root);
    let from_file = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok());

    let next = match from_file {
        Some(current) => current.saturating_add(1),
        None => {
            let max_existing = list(repo_root).iter().map(|e| e.seq).max();
            max_existing.map(|m| m.saturating_add(1)).unwrap_or(0)
        }
    };

    atomic_write(&path, next.to_string().as_bytes())?;
    Ok(next)
}

/// Enqueue a mission. The entry's `seq` is assigned here from the monotonic
/// counter (any incoming `seq` is overwritten). Write is atomic (temp + rename).
/// A no-op if the mission is already queued.
pub fn enqueue(repo_root: &Path, entry: QueueEntry) -> Result<QueueEntry> {
    let dir = queue_dir(repo_root);
    std::fs::create_dir_all(&dir)?;

    // Serialize the dedupe-check + seq-reserve + entry-write critical section
    // against same-process tasks AND sibling processes (review P1).
    let _local = LOCAL_MUTATION_LOCK
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let _cross = MutationLock::acquire(repo_root)?;

    if contains(repo_root, &entry.mission_id) {
        // Already queued: return the existing entry unchanged.
        if let Some(existing) = list(repo_root)
            .into_iter()
            .find(|e| e.mission_id == entry.mission_id)
        {
            return Ok(existing);
        }
    }

    let seq = next_seq(repo_root)?;
    let entry = QueueEntry { seq, ..entry };
    let json = serde_json::to_string_pretty(&entry)?;
    atomic_write(&dir.join(entry.file_name()), json.as_bytes())?;
    Ok(entry)
}

/// All queued entries, sorted by `(priority, seq)` (== filename order).
pub fn list(repo_root: &Path) -> Vec<QueueEntry> {
    let dir = queue_dir(repo_root);
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return out;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<QueueEntry>(&text) {
                Ok(qe) => out.push(qe),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping unparseable queue entry");
                }
            },
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "unreadable queue entry, skipping");
            }
        }
    }
    out.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.seq.cmp(&b.seq)));
    out
}

/// The front of the queue (highest priority, then earliest insertion), if any.
pub fn peek(repo_root: &Path) -> Option<QueueEntry> {
    list(repo_root).into_iter().next()
}

/// Remove the entry for `mission_id`. Returns true if something was removed.
pub fn remove(repo_root: &Path, mission_id: &str) -> bool {
    let dir = queue_dir(repo_root);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return false;
    };
    let mut removed = false;
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let is_match = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<QueueEntry>(&text).ok())
            .is_some_and(|qe| qe.mission_id == mission_id);
        if is_match && std::fs::remove_file(&path).is_ok() {
            removed = true;
        }
    }
    removed
}

/// Whether `mission_id` is currently queued.
pub fn contains(repo_root: &Path, mission_id: &str) -> bool {
    list(repo_root).iter().any(|e| e.mission_id == mission_id)
}

// ---------------------------------------------------------------------------
// Repo busy guard: one RUNNING mission per working tree
// ---------------------------------------------------------------------------

fn repo_busy_lock(repo_root: &Path) -> PathBuf {
    queue_dir(repo_root).join(".repo.busy.lock")
}

fn repo_busy_mission_file(repo_root: &Path) -> PathBuf {
    queue_dir(repo_root).join(".repo.busy.mission")
}

fn repo_busy_mission(repo_root: &Path) -> Option<String> {
    std::fs::read_to_string(repo_busy_mission_file(repo_root))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn lock_held_for_repo(repo_root: &Path) -> EngineError {
    let holder = is_repo_busy(repo_root).unwrap_or_else(|| "unknown".to_string());
    EngineError::LockHeld(format!("repo is busy with mission {holder}"))
}

#[derive(Debug)]
struct RepoBusyGuard {
    lock_path: PathBuf,
    mission_path: PathBuf,
}

impl RepoBusyGuard {
    fn acquire(repo_root: &Path, mission_id: &str) -> Result<Self> {
        Self::acquire_allowing_own_legacy(repo_root, mission_id, false)
    }

    /// Acquire the repo-wide busy lock. When `allow_own_legacy` is true, a
    /// live `events.jsonl.lock` for *this* `mission_id` is ignored (hosted
    /// start already holds the single-writer lock); any other mission's
    /// legacy lock still conflicts. Queue claims keep `allow_own_legacy =
    /// false` so a live engine for the claimed id cannot be double-run.
    fn acquire_allowing_own_legacy(
        repo_root: &Path,
        mission_id: &str,
        allow_own_legacy: bool,
    ) -> Result<Self> {
        std::fs::create_dir_all(queue_dir(repo_root))?;
        let lock_path = repo_busy_lock(repo_root);
        let mission_path = repo_busy_mission_file(repo_root);

        for _ in 0..16 {
            if let Some(holder) = legacy_mission_lock_busy(repo_root) {
                if !(allow_own_legacy && holder == mission_id) {
                    return Err(lock_held_for_repo(repo_root));
                }
            }

            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock_path)
            {
                Ok(mut file) => {
                    use std::io::Write as _;
                    write!(file, "{}", crate::event_log::current_lock_holder_record())?;
                    file.sync_data()?;
                    if let Err(e) = atomic_write(&mission_path, mission_id.as_bytes()) {
                        let _ = std::fs::remove_file(&lock_path);
                        return Err(e);
                    }
                    return Ok(Self {
                        lock_path,
                        mission_path,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_pid_is_alive(&lock_path) {
                        return Err(lock_held_for_repo(repo_root));
                    }
                    let _ = std::fs::remove_file(&mission_path);
                    let _ = std::fs::remove_file(&lock_path);
                }
                Err(e) => return Err(e.into()),
            }
        }

        Err(EngineError::LockHeld(
            "repo busy lock changed too often to acquire safely".to_string(),
        ))
    }
}

impl Drop for RepoBusyGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.mission_path);
        let _ = std::fs::remove_file(&self.lock_path);
    }
}

/// Public RAII hold on the repo-wide busy lock — same underlying guard the
/// queue claim path uses. Drop (or end of scope) releases the lock so a
/// sibling dispatcher or hosted start can proceed.
#[derive(Debug)]
pub struct RepoBusyHold {
    _inner: RepoBusyGuard,
}

/// Acquire the repo-wide busy lock for `mission_id`. Returns
/// [`EngineError::LockHeld`] when another live holder already owns it.
///
/// Unlike the queue claim path, this allows the caller to already hold
/// `events.jsonl.lock` for `mission_id` (hosted start: the planning engine
/// is about to become the run).
pub fn acquire_repo_busy(repo_root: &Path, mission_id: &str) -> Result<RepoBusyHold> {
    Ok(RepoBusyHold {
        _inner: RepoBusyGuard::acquire_allowing_own_legacy(repo_root, mission_id, true)?,
    })
}

// ---------------------------------------------------------------------------
// Claims: crash-safe hand-off from queue to dispatcher (review P1)
// ---------------------------------------------------------------------------

/// A claimed queue entry: the entry file was atomically RENAMED to
/// `<name>.json.claimed.<pid>`, so no sibling dispatcher can double-run it,
/// and a crash before completion leaves a recoverable file instead of
/// dropped work. Call [`finish_claim`] when the mission reached a terminal
/// state (any outcome), or [`release_claim`] to put the entry back.
#[derive(Debug)]
pub struct Claim {
    pub entry: QueueEntry,
    claimed_path: PathBuf,
    original_path: PathBuf,
    _repo_guard: Option<RepoBusyGuard>,
}

/// Result of atomically taking work with the repo-wide busy guard.
#[derive(Debug)]
pub enum ClaimFront {
    Empty,
    LostRace,
    Busy { mission_id: String },
    Claimed(Claim),
}

/// Atomically claim the front entry, if any. A lost rename race (a sibling
/// claimed first) retries with the next front.
pub fn claim_front(repo_root: &Path) -> Option<Claim> {
    // Bounded: a lost race retries, but a PERSISTENT rename failure
    // (read-only fs, permissions) must not spin forever.
    for _ in 0..16 {
        let entry = peek(repo_root)?;
        let original = queue_dir(repo_root).join(entry.file_name());
        // The claim name carries the claimant's process IDENTITY TOKEN (the
        // event-log lock idiom) when the platform can compute one: after a
        // crash, a recycled pid then proves itself different from the
        // recorded claimant and the age backstop can fire (4th-pass review —
        // "alive pid stands the claim" alone stranded claims forever behind
        // unrelated long-lived processes). No token available: the legacy
        // pid-only name, which forgoes reuse detection exactly like the
        // legacy lock-file formats.
        let claimed = queue_dir(repo_root).join(format!(
            "{}.claimed.{}{}",
            entry.file_name(),
            std::process::id(),
            claim_identity_suffix()
        ));
        match std::fs::rename(&original, &claimed) {
            Ok(()) => {
                return Some(Claim {
                    entry,
                    claimed_path: claimed,
                    original_path: original,
                    _repo_guard: None,
                })
            }
            Err(_) => {
                // Raced: the front changed under us. Re-peek; a missing queue
                // means nothing left to claim.
                peek(repo_root)?;
            }
        }
    }
    tracing::warn!("claim_front: 16 consecutive claim failures; treating queue as unclaimable");
    None
}

/// Claim the front queue entry only if this repo is not already running a
/// mission. The queue claim and repo busy guard travel together in [`Claim`],
/// so the guard stays held until [`finish_claim`] or [`release_claim`] consumes
/// it after the injected mission runner returns.
pub fn claim_front_when_repo_free(repo_root: &Path) -> Result<ClaimFront> {
    let had_front = peek(repo_root).is_some();
    let Some(mut claim) = claim_front(repo_root) else {
        return Ok(if had_front {
            ClaimFront::LostRace
        } else {
            ClaimFront::Empty
        });
    };

    match RepoBusyGuard::acquire(repo_root, &claim.entry.mission_id) {
        Ok(guard) => {
            claim._repo_guard = Some(guard);
            Ok(ClaimFront::Claimed(claim))
        }
        Err(EngineError::LockHeld(_)) => {
            let mission_id = is_repo_busy(repo_root).unwrap_or_else(|| "unknown".to_string());
            release_claim(claim);
            Ok(ClaimFront::Busy { mission_id })
        }
        Err(e) => {
            release_claim(claim);
            Err(e)
        }
    }
}

/// The mission ran to a terminal state (any outcome): retire the claim.
pub fn finish_claim(mut claim: Claim) {
    let _ = std::fs::remove_file(&claim.claimed_path);
    claim.disarm();
}

/// The mission could NOT be run (start failure, lock held, config error):
/// put the entry back so the work is not lost.
pub fn release_claim(mut claim: Claim) {
    if std::fs::rename(&claim.claimed_path, &claim.original_path).is_err() {
        tracing::warn!(
            path = %claim.claimed_path.display(),
            "failed to release queue claim; entry remains claimed on disk"
        );
    }
    claim.disarm();
}

impl Claim {
    /// Mark the claim as consumed so [`Drop`] does not re-release it.
    /// The embedded [`RepoBusyGuard`] still drops normally.
    fn disarm(&mut self) {
        self.claimed_path = PathBuf::new();
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        // RAII safety net: an early `?` in drain_queue used to leak the
        // `.claimed.<pid>` rename + RepoBusyGuard. If the claimed file is
        // still present when Claim goes out of scope, put the entry back.
        if self.claimed_path.as_os_str().is_empty() {
            return;
        }
        if self.claimed_path.exists()
            && std::fs::rename(&self.claimed_path, &self.original_path).is_err()
        {
            tracing::warn!(
                path = %self.claimed_path.display(),
                "Claim::drop failed to release queue claim"
            );
        }
    }
}

/// Liveness verdict for the pid recorded in a claim filename.
///
/// Same INVARIANT as the event-log lock probe: anything uncertain must never
/// report [`ClaimPidLiveness::Dead`] — a false "dead" requeues a mission a
/// live dispatcher is still running, executing it twice (review P2).
// Alive/Dead are constructed only by the unix probe arm; silence dead_code
// off-unix without masking it on unix (windows-latest clippy gates -D warnings).
#[cfg_attr(not(unix), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClaimPidLiveness {
    /// `kill(pid, 0)` succeeded: the claiming process exists.
    Alive,
    /// `kill(pid, 0)` failed with ESRCH: no such process — positive proof.
    Dead,
    /// The probe cannot settle it on this platform/errno: the caller's age
    /// backstop breaks the tie.
    Unknown,
}

/// Probe the pid from a claim filename. unix: `kill(pid, 0)` == 0 → Alive;
/// ESRCH → Dead; EPERM → Unknown (a process exists but is owned by another
/// user — it cannot be our same-user dispatcher, yet its presence also means
/// the pid was recycled, so let the age backstop decide); any other errno →
/// Unknown. Non-positive pids → Unknown (never probe a process GROUP, and
/// `kill(0, 0)` would match our own). Non-unix: no probe is wired up — the
/// same posture as [`crate::event_log::lock_holder_is_alive`] — so every pid
/// is Unknown and recovery keeps the conservative age-only rule there.
fn probe_claim_pid(pid: i32) -> ClaimPidLiveness {
    if pid <= 0 {
        return ClaimPidLiveness::Unknown;
    }
    #[cfg(unix)]
    {
        if unsafe { libc::kill(pid, 0) } == 0 {
            return ClaimPidLiveness::Alive;
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => ClaimPidLiveness::Dead,
            _ => ClaimPidLiveness::Unknown,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        ClaimPidLiveness::Unknown
    }
}

/// The `.TOKENHASH` claim-name suffix for the current process (empty when
/// the platform cannot compute an identity token — the legacy pid-only
/// claim name, which forgoes reuse detection like the legacy lock formats).
fn claim_identity_suffix() -> String {
    crate::event_log::process_identity_token(std::process::id() as i32)
        .map(|token| format!(".{:016x}", identity_token_hash(&token)))
        .unwrap_or_default()
}

/// DefaultHasher over the identity token: equality is all that matters
/// (matching the lock idiom's raw-equality compare), and the hex hash keeps
/// spaces and punctuation out of the claim filename.
fn identity_token_hash(token: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    token.hash(&mut hasher);
    hasher.finish()
}

/// Recover claims left by dead dispatchers. A `*.claimed.<pid>[.<token>]`
/// file is renamed back to its entry name when its claimant is provably
/// gone — or, when liveness cannot be determined, when the file is over an
/// hour old (the pid-REUSE backstop).
///
/// A pid probed ALIVE keeps its claim only while it is provably the SAME
/// process that claimed: the claim name carries the claimant's identity
/// token (the event-log lock idiom), so a recycled pid — alive but a
/// different token — is NOT the claimant and the age backstop decides
/// (4th-pass review: "alive stands the claim" alone stranded claims forever
/// behind unrelated long-lived processes). Legacy tokenless claims keep the
/// pre-token rule (alive stands at any age): a false stand delays work, a
/// false requeue runs a mission twice.
pub fn recover_dead_claims(repo_root: &Path) -> usize {
    let dir = queue_dir(repo_root);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return 0;
    };
    let mut recovered = 0;
    for f in rd.flatten() {
        let path = f.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some((entry_name, claim_suffix)) = name.split_once(".claimed.") else {
            continue;
        };
        // Suffix shape: `<pid>` (legacy) or `<pid>.<token-hash>`.
        let (pid_str, recorded_token) = match claim_suffix.split_once('.') {
            Some((pid, token)) => (pid, Some(token.to_string())),
            None => (claim_suffix, None),
        };
        let aged_out = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > Duration::from_secs(3600));
        let dead = match pid_str.parse::<i32>() {
            Ok(pid) => match probe_claim_pid(pid) {
                ClaimPidLiveness::Alive => match recorded_token {
                    // Alive AND provably the claimant: stands regardless of
                    // age (the reorder: age must never requeue a mission a
                    // live dispatcher is still running).
                    None => false,
                    Some(recorded) => match crate::event_log::process_identity_token(pid) {
                        // Alive but a DIFFERENT process than the claimant:
                        // the pid was recycled after a crash — the age
                        // backstop decides, or the claim strands forever
                        // behind an unrelated long-lived process.
                        Some(current)
                            if format!("{:016x}", identity_token_hash(&current)) != recorded =>
                        {
                            aged_out
                        }
                        // Token matches (provably the claimant), or the
                        // token is unprobeable right now: alive stands.
                        _ => false,
                    },
                },
                ClaimPidLiveness::Dead => true,
                // Ambiguous (unprobeable platform, EPERM, bad errno): age
                // breaks the tie, as the pid-reuse backstop.
                ClaimPidLiveness::Unknown => aged_out,
            },
            // A pid suffix that can't be parsed belongs to no probe-able
            // dispatcher: recover immediately rather than strand the entry
            // behind an unanswerable probe (existing rule, unchanged).
            Err(_) => true,
        };
        if dead && std::fs::rename(&path, dir.join(entry_name)).is_ok() {
            recovered += 1;
        }
    }
    recovered
}

/// The mission id currently RUNNING in this repo, if any: detected by any
/// repo-wide busy guard, falling back to any legacy
/// `.kranz/missions/*/events.jsonl.lock` whose recorded pid is still alive.
pub fn is_repo_busy(repo_root: &Path) -> Option<String> {
    let repo_lock = repo_busy_lock(repo_root);
    if repo_lock.exists() {
        if lock_pid_is_alive(&repo_lock) {
            return repo_busy_mission(repo_root).or_else(|| Some("unknown".to_string()));
        }
        let _ = std::fs::remove_file(repo_busy_mission_file(repo_root));
        let _ = std::fs::remove_file(repo_lock);
    }
    legacy_mission_lock_busy(repo_root)
}

fn legacy_mission_lock_busy(repo_root: &Path) -> Option<String> {
    let missions = repo_root.join(".kranz").join("missions");
    let rd = std::fs::read_dir(&missions).ok()?;
    for entry in rd.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let lock = dir.join("events.jsonl.lock");
        if !lock.exists() {
            continue;
        }
        if lock_pid_is_alive(&lock) {
            if let Some(id) = dir.file_name().and_then(|n| n.to_str()) {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// True when a lock file records a holder that is still alive.
///
/// Delegates to the event-log module's canonical probe
/// ([`crate::event_log::lock_holder_is_alive`]) — ONE source of truth for
/// lock-file format and liveness semantics, so the queue can never diverge
/// from what `EventLog::acquire` itself would decide. (A divergent local
/// parser once read the entire file as a single integer, so any multi-line
/// lock was "unparseable ⇒ busy" — a SIGKILL'd engine livelocked `kranz
/// work` forever.) Genuinely unparseable-but-present locks still read as
/// busy, conservatively: a false "busy" only delays a queued mission, while
/// a false "free" could run two missions on one working tree.
fn lock_pid_is_alive(lock_path: &Path) -> bool {
    crate::event_log::lock_holder_is_alive(lock_path)
}

/// Atomic write via a sibling temp file + rename.
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("entry");
    // Pid + process-wide counter: concurrent tasks in ONE process must not
    // share a temp file (review P1 — Slack spawns parallel approvals).
    static TMP_SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = dir.join(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        TMP_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(_) if cfg!(windows) => {
            let _ = std::fs::remove_file(path);
            std::fs::rename(&tmp, path)?;
            Ok(())
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

    fn entry(mission_id: &str) -> QueueEntry {
        QueueEntry {
            mission_id: mission_id.to_string(),
            ticket_slug: None,
            priority: 2,
            seq: 0,
        }
    }

    #[test]
    fn claim_front_when_repo_free_holds_repo_busy_until_claim_finishes() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        enqueue(repo, entry("m-1")).unwrap();
        enqueue(repo, entry("m-2")).unwrap();

        let first = match claim_front_when_repo_free(repo).unwrap() {
            ClaimFront::Claimed(claim) => claim,
            other => panic!("expected first claim, got {other:?}"),
        };
        assert_eq!(first.entry.mission_id, "m-1");
        assert_eq!(is_repo_busy(repo).as_deref(), Some("m-1"));

        match claim_front_when_repo_free(repo).unwrap() {
            ClaimFront::Busy { mission_id } => assert_eq!(mission_id, "m-1"),
            other => panic!("expected repo-busy result, got {other:?}"),
        }
        assert!(
            contains(repo, "m-2"),
            "busy loser releases the queue claim instead of dropping work"
        );

        finish_claim(first);
        assert_eq!(is_repo_busy(repo), None);

        let second = match claim_front_when_repo_free(repo).unwrap() {
            ClaimFront::Claimed(claim) => claim,
            other => panic!("expected second claim after guard drop, got {other:?}"),
        };
        assert_eq!(second.entry.mission_id, "m-2");
        finish_claim(second);
        assert!(list(repo).is_empty());
        assert_eq!(is_repo_busy(repo), None);
    }

    #[test]
    fn acquire_repo_busy_holds_until_drop_and_conflicts_with_second() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();

        let hold = acquire_repo_busy(repo, "m-hosted").expect("first acquire");
        assert_eq!(is_repo_busy(repo).as_deref(), Some("m-hosted"));

        let err = acquire_repo_busy(repo, "m-other").expect_err("second must conflict");
        assert!(
            matches!(err, EngineError::LockHeld(_)),
            "expected LockHeld, got {err:?}"
        );

        drop(hold);
        assert_eq!(is_repo_busy(repo), None);
        let again = acquire_repo_busy(repo, "m-hosted").expect("re-acquire after drop");
        drop(again);
        assert_eq!(is_repo_busy(repo), None);
    }

    #[test]
    fn acquire_repo_busy_ignores_own_mission_events_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let paths = crate::paths::MissionPaths::new(repo, "m-self");
        // Hold the single-writer lock the way a hosted planning engine would.
        let _log = crate::event_log::EventLog::acquire(
            &paths,
            "m-self",
            Duration::ZERO,
            crate::event_log::LockForce::No,
        )
        .expect("mission lock");

        let hold = acquire_repo_busy(repo, "m-self").expect("self legacy lock must not block");
        assert_eq!(is_repo_busy(repo).as_deref(), Some("m-self"));

        let err = acquire_repo_busy(repo, "m-other").expect_err("other must still conflict");
        assert!(matches!(err, EngineError::LockHeld(_)));
        drop(hold);
    }
}
