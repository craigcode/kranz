//! Append-only JSONL event log (plan §4.3) — the single source of truth.
//!
//! One engine process owns `events.jsonl` at a time, guarded by a lock file
//! (`events.jsonl.lock`) rather than POSIX advisory locks so Windows stays
//! first-class. Lifecycle events are flushed + fsynced per append; stream
//! deltas (`worker.message`) are buffered in memory and drained by age
//! (throttle) — checked on the next append, or on demand via
//! [`EventLog::flush_if_due`] — by the next lifecycle append, by explicit
//! [`EventLog::flush`], or on drop. Losing buffered deltas on a crash is
//! recoverable; losing a lifecycle event is not, hence the asymmetry.
//!
//! # Line integrity (audit 2026-09-01 H6)
//!
//! The log is not only an audit record: three gate decisions read it back
//! mid-run, so a well-formed forged append or a rollback by truncation is a
//! live consent bypass, not a post-hoc bookkeeping problem. Every line the
//! writer produces therefore carries three extra fields, kept to one character
//! because they ride every event:
//!
//! - `v` — canonicalization version, currently 2. Absent means the legacy
//!   float parser. Version 2 preserves floating-point bits and prefixes the
//!   hash input with `kranz.event-log.v2\n`, so stripping `v` cannot downgrade it.
//! - `h` — the CHAIN. Hex sha256 over the version prefix, the previous line's
//!   `h` (empty for the first chained line), and this line's
//!   canonical event bytes, which are the event re-serialized WITHOUT `v`, `h`,
//!   and `m`. Legacy seals have no prefix and retain their original numeric
//!   interpretation. Version 2 sorts all object keys, independently of Cargo's
//!   `serde_json/preserve_order` feature. Both sides start from an [`Event`].
//!   The chain makes an edit anywhere in the file loud instead of local.
//! - `m` — the MAC. Hex HMAC-SHA256 of `h` under the repository authority key
//!   ([`crate::paths::authority_key_path`]).
//!
//! Be clear about which does what. The chain ALONE only catches accidental
//! corruption: an attacker who rewrites a line can recompute every following
//! `h` themselves. `m` is what defeats a same-uid forger, because the key
//! lives outside the repository, in the sandbox's authority-read-deny set and
//! behind the agent CLI's `Read(~/.kranz/**)` deny rule, so they cannot
//! compute it.
//!
//! Compatibility, and why it is not a hole. A log written before this existed
//! has no `h` on any line, and refusing it would strand every in-flight
//! mission, so an unsealed line is not refused on its own. Two rules stop
//! that from becoming a free downgrade:
//!
//! 1. No downgrade WITHIN a log. Once a line carries `h`, every later line
//!    must; once a line carries `m`, every later line must. An attacker
//!    cannot strip integrity off just the tail they want to rewrite.
//! 2. A SEAL FLOOR outside the repository. The first time a writer with the
//!    key opens a mission's log it records, under `~/.kranz/seals/`, the seq
//!    it will start sealing from ([`crate::paths::record_seal_floor`]). Every
//!    line at or above that seq must carry a valid `h` and `m`. That is what
//!    stops the whole-log rewrite: stripping every line makes the file look
//!    legacy, but the floor is not in the file and the attacker cannot lower
//!    it. It is also why the `m` rule is not simply "the key exists, so every
//!    line needs `m`" — the key can be minted mid-mission, and the lines
//!    written before that legitimately have none.

use crate::error::{EngineError, Result};
use crate::events::{Event, EventKind};
use crate::paths::MissionPaths;
use crate::scrub::SecretFinding;
use cap_std::fs::Dir;
use chrono::Utc;
#[cfg(unix)]
use std::ffi::OsString;
use std::fs::File;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq as _;

/// JSON key holding a line's chain hash.
const CHAIN_FIELD: &str = "h";

/// JSON key holding a line's MAC over the chain hash.
const MAC_FIELD: &str = "m";
const VERSION_FIELD: &str = "v";
const EXACT_FLOAT_VERSION: u64 = 2;

/// Canonical bytes for one event, excluding the `v`/`h`/`m` envelope.
fn canonical_event_bytes(event: &Event, version: u64) -> Result<String> {
    if version == EXACT_FLOAT_VERSION {
        let mut value = serde_json::to_value(event)?;
        value.sort_all_objects();
        Ok(serde_json::to_string(&value)?)
    } else {
        Ok(serde_json::to_string(event)?)
    }
}

/// Next link in the chain: sha256 over the previous link and this event's
/// canonical bytes.
fn chain_hash(prev: &str, body: &str, version: u64) -> String {
    let prefix = if version == EXACT_FLOAT_VERSION {
        "kranz.event-log.v2\n"
    } else {
        ""
    };
    let mut input = String::with_capacity(prefix.len() + prev.len() + body.len());
    input.push_str(prefix);
    input.push_str(prev);
    input.push_str(body);
    crate::standards_waiver::sha256_hex(input.as_bytes())
}

/// Serialize one event as a sealed log line (no trailing newline), returning
/// the line and the chain hash the NEXT line must build on.
fn seal_line(event: &Event, prev_hash: &str, key: Option<&[u8]>) -> Result<(String, String)> {
    let body = canonical_event_bytes(event, EXACT_FLOAT_VERSION)?;
    let hash = chain_hash(prev_hash, &body, EXACT_FLOAT_VERSION);
    let mut value = serde_json::to_value(event)?;
    let object = value.as_object_mut().ok_or_else(|| {
        EngineError::InvalidState("event did not serialize as a JSON object".to_string())
    })?;
    object.insert(VERSION_FIELD.to_string(), EXACT_FLOAT_VERSION.into());
    object.insert(
        CHAIN_FIELD.to_string(),
        serde_json::Value::String(hash.clone()),
    );
    if let Some(key) = key {
        object.insert(
            MAC_FIELD.to_string(),
            serde_json::Value::String(crate::hooks::hmac_sha256_hex(key, hash.as_bytes())),
        );
    }
    Ok((serde_json::to_string(&value)?, hash))
}

// The old writer parsed its serialized body once before writing, and readers
// parsed it again. Some valid seals depend on that parser's rounded arithmetic.
// Reproduce it only for legacy lines, using the canonical float token emitted
// by serde_json (at most 17 significant digits). Integers and strings stay intact.
fn restore_legacy_float_parsing(value: &mut serde_json::Value) -> Result<()> {
    match value {
        serde_json::Value::Number(number) if number.is_f64() => {
            *number = legacy_float_number(number).ok_or_else(|| {
                EngineError::LogCorruption("legacy event has an out-of-range float".into())
            })?;
        }
        serde_json::Value::Array(values) => {
            for value in values {
                restore_legacy_float_parsing(value)?;
            }
        }
        serde_json::Value::Object(values) => {
            for value in values.values_mut() {
                restore_legacy_float_parsing(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn legacy_float_number(number: &serde_json::Number) -> Option<serde_json::Number> {
    let token = number.to_string();
    let negative = token.starts_with('-');
    let unsigned = token.strip_prefix('-').unwrap_or(&token);
    let (mantissa, exponent) = unsigned.split_once('e').unwrap_or((unsigned, "0"));
    let fraction_digits = mantissa
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let mut exponent = exponent.parse::<i32>().ok()? - i32::try_from(fraction_digits).ok()?;
    let coefficient = mantissa.replace('.', "").parse::<u64>().ok()?;
    let mut parsed = coefficient as f64;
    if exponent < -308 {
        parsed /= 1e308;
        exponent += 308;
    }
    if exponent.unsigned_abs() > 308 {
        return None;
    }
    // Decimal parsing gives the same rounded powers as the old parser's
    // literal table. powi() can round differently and cannot substitute here.
    let power = format!("1e{}", exponent.unsigned_abs())
        .parse::<f64>()
        .ok()?;
    parsed = if exponent < 0 {
        parsed / power
    } else {
        parsed * power
    };
    serde_json::Number::from_f64(if negative { -parsed } else { parsed })
}

/// Seal a whole event sequence into `events.jsonl` bytes exactly as the
/// writer would. Public so tests and tooling can build a log that satisfies
/// [`EventLog::read_events`] without driving a live [`EventLog`].
pub fn seal_events(events: &[Event], key: Option<&[u8]>) -> Result<String> {
    let mut out = String::new();
    let mut prev = String::new();
    for event in events {
        let (line, hash) = seal_line(event, &prev, key)?;
        out.push_str(&line);
        out.push('\n');
        prev = hash;
    }
    Ok(out)
}

/// Refuse to carry on when the log is SHORTER than the last snapshot says it
/// was (audit 2026-09-01 H6, rollback by truncation).
///
/// `state.json` is a derived cache, but it holds an independent copy of the
/// high-water mark (`MissionState::last_seq`), and nothing compared the two.
/// A log cut at a line boundary stays internally valid, so the only signal
/// that events were erased is that the snapshot remembers more of them.
///
/// A snapshot that is BEHIND the log is normal: the log is the source of
/// truth and the snapshot is rewritten after the fold, so a crash between the
/// two leaves it stale. Only the shorter-log direction is refused, and the
/// error names both numbers so an operator can see the size of the gap.
///
/// A missing or unreadable snapshot is not an error: a mission resumed on a
/// machine that never wrote one has nothing to compare against.
pub fn check_no_rollback(paths: &MissionPaths, events: &[Event]) -> Result<()> {
    // Two witnesses, and the higher one wins. The snapshot lives in the
    // repository beside the log, so it only catches a crash or a careless
    // edit; the high-water mark lives outside the repository beside the
    // authority key, so it also catches a writer who trimmed both.
    let snapshot_seq = crate::reducer::read_snapshot(&paths.state_file())
        .map(|snapshot| snapshot.last_seq)
        .unwrap_or(0);
    let mark_seq = crate::paths::read_high_water(&paths.repo_root, &paths.mission_id).unwrap_or(0);
    let (witness_seq, witness) = if mark_seq >= snapshot_seq {
        (mark_seq, "the out-of-repo high-water mark")
    } else {
        (snapshot_seq, "the last snapshot")
    };
    let log_last_seq = events.last().map(|e| e.seq).unwrap_or(0);
    if witness_seq > log_last_seq {
        return Err(EngineError::LogCorruption(format!(
            "refusing to resume mission '{}': {} ends at seq {log_last_seq} but {witness} \
             recorded seq {witness_seq}. The log has lost {} event(s) since it was written; \
             resuming would overwrite the snapshot with the rolled-back state and erase the \
             evidence. Restore the log from the mission branch or abandon the mission.",
            paths.mission_id,
            paths.events_file().display(),
            witness_seq - log_last_seq
        )));
    }
    Ok(())
}

/// The repository root and mission id a log sits under, recovered from its
/// path (`<repo>/.kranz/missions/<id>/events.jsonl`). Used to locate the
/// authority key and the seal floor from the static reader entry points,
/// which take only a path.
fn log_identity(path: &Path) -> Option<(&Path, &str)> {
    let mission_dir = path.parent()?;
    let mission_id = mission_dir.file_name()?.to_str()?;
    let missions_dir = mission_dir.parent()?;
    if missions_dir.file_name()? != "missions" {
        return None;
    }
    let kranz_dir = missions_dir.parent()?;
    if kranz_dir.file_name()? != ".kranz" {
        return None;
    }
    Some((kranz_dir.parent()?, mission_id))
}

/// How aggressively [`EventLog::acquire`] may steal an existing lock.
///
/// A holder that is provably DEAD is always stolen (a stale lock from a
/// crashed engine), regardless of tier. The tiers only govern holders that
/// are alive or of indeterminate liveness. Automatic Dead detection is
/// unix-only (`kill(pid, 0)` returning `ESRCH`, plus the own-pid
/// token-reuse screen on any platform): on non-unix targets only the
/// own-pid token-reuse screen can ever yield Dead, so recovering the lock
/// from a foreign crashed holder there always requires an explicit force
/// tier (`--force-lock` / `--dangerously-steal-live-lock`).
///
/// | holder liveness | `No`       | `IfNotLive` | `EvenIfLive` |
/// |-----------------|------------|-------------|--------------|
/// | Dead            | steal      | steal       | steal        |
/// | Unknown         | `LockHeld` | steal       | steal        |
/// | Alive           | `LockHeld` | `LockHeld`  | steal (loud) |
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockForce {
    /// Honor any lock whose holder is not provably dead.
    No,
    /// `--force-lock`: steal unless the holder is provably ALIVE. This is the
    /// historical force behavior on platforms/lockfiles where liveness cannot
    /// be probed (Unknown), but it refuses to rip the lock from a running
    /// engine.
    IfNotLive,
    /// `--dangerously-steal-live-lock`: steal even from a live holder. Only
    /// correct when the operator has verified the holder is a zombie or an
    /// unrelated (pid-reused) process — stealing from a live kranz engine
    /// means two engines write one log.
    EvenIfLive,
}

/// A serialized delta line waiting in the write buffer.
#[derive(Debug)]
struct BufferedLine {
    buffered_at: Instant,
    line: String,
}

/// Result of validating a log file, including how many leading bytes hold
/// successfully parsed lines (so `acquire` can truncate a torn tail).
#[derive(Debug)]
struct ParsedLog {
    events: Vec<Event>,
    /// Byte length of the valid prefix: the end of the last successfully
    /// parsed line, including its trailing newline when present.
    valid_len: u64,
    /// False only when the last parsed line lacked a trailing newline (a torn
    /// write that cut exactly at the terminator).
    terminated: bool,
    /// Chain hash of the last parsed line, `None` when the log is empty or
    /// its tail is still unchained (a legacy log). The next append builds on
    /// this, so a legacy log starts a fresh chain from the empty string.
    last_hash: Option<String>,
    /// True once any parsed line carried a MAC, so the writer keeps MACing
    /// even if the key becomes unreadable rather than silently downgrading a
    /// log every reader would then refuse.
    saw_mac: bool,
}

/// Single-writer, append-only handle on a mission's `events.jsonl`.
///
/// Constructed via [`EventLog::acquire`]; the lock file is released on drop.
#[derive(Debug)]
pub struct EventLog {
    mission_id: String,
    events_path: PathBuf,
    lock_path: PathBuf,
    /// Generation written into the lock file at acquire. Re-checked on every
    /// append so a stolen-from process fails closed instead of dual-writing.
    lock_generation: u64,
    /// Identity token written into the lock file at acquire (when the
    /// platform can produce one). Drop deletes the lock file only when the
    /// on-disk generation (and token, when present) still match — so a
    /// stolen-from teardown cannot wipe the stealer's lock.
    lock_token: Option<String>,
    /// Pinned mission directory capability retained from acquisition through
    /// every lock read/removal. Absolute paths below are display-only.
    mission_dir: Dir,
    file: File,
    /// Seq to assign to the next appended event.
    next_seq: u64,
    throttle: Duration,
    buffer: Vec<BufferedLine>,
    /// Repository root, for the out-of-repo high-water mark recorded after
    /// every durable append (see [`crate::paths::record_high_water`]).
    repo_root: PathBuf,
    /// Chain hash of the last line written (or loaded at acquire); the empty
    /// string for a fresh or still-unchained log.
    prev_hash: String,
    /// Repository authority key, when one is readable. `None` writes the
    /// chain without a MAC, which is what a reader with no key can verify
    /// anyway.
    authority_key: Option<Vec<u8>>,
}

/// Write buffered lines to `sink` from the front, removing each line from
/// `buffer` only after it is successfully written. On the first write error
/// the failing line and everything after it stay in `buffer`, in original
/// order, so a later retry can pick up exactly where this call left off —
/// `Vec::drain` cannot do this since its guard discards not-yet-yielded
/// items if the iteration is cut short by `?`.
fn drain_lines<W: std::io::Write>(
    sink: &mut W,
    buffer: &mut Vec<BufferedLine>,
) -> std::io::Result<()> {
    while !buffer.is_empty() {
        sink.write_all(buffer[0].line.as_bytes())?;
        buffer.remove(0);
    }
    Ok(())
}

fn ensure_absent_or_regular_at(dir: &Dir, name: &str, display: &Path) -> Result<()> {
    match dir.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(()),
        Ok(_) => Err(EngineError::InvalidState(format!(
            "refusing non-regular mission runtime path {}",
            display.display()
        ))),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn open_create_new_at(dir: &Dir, name: &str) -> std::io::Result<File> {
    use cap_fs_ext::OpenOptionsFollowExt as _;
    use cap_primitives::fs::FollowSymlinks;
    let mut options = cap_std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    dir.open_with(name, &options).map(|file| file.into_std())
}

fn open_write_at(dir: &Dir, name: &str, create: bool) -> std::io::Result<File> {
    use cap_fs_ext::OpenOptionsFollowExt as _;
    use cap_primitives::fs::FollowSymlinks;
    let mut options = cap_std::fs::OpenOptions::new();
    options
        .write(true)
        .create(create)
        .follow(FollowSymlinks::No);
    dir.open_with(name, &options).map(|file| file.into_std())
}

fn open_append_at(dir: &Dir, name: &str, create: bool) -> std::io::Result<File> {
    use cap_fs_ext::OpenOptionsFollowExt as _;
    use cap_primitives::fs::FollowSymlinks;
    let mut options = cap_std::fs::OpenOptions::new();
    options
        .append(true)
        .create(create)
        .follow(FollowSymlinks::No);
    dir.open_with(name, &options).map(|file| file.into_std())
}

fn open_read_at(dir: &Dir, name: &str) -> std::io::Result<File> {
    use cap_fs_ext::OpenOptionsFollowExt as _;
    use cap_primitives::fs::FollowSymlinks;
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    #[cfg(unix)]
    {
        use cap_fs_ext::OpenOptionsExt as _;
        options.custom_flags(libc::O_NONBLOCK);
    }
    dir.open_with(name, &options).map(|file| file.into_std())
}

impl EventLog {
    /// Acquire the single-writer lock for a mission and open its event log.
    ///
    /// Creates the mission directory tree (mission dir, `runs/`, `control/`)
    /// if missing — no-follow: a symlinked `.kranz`/`missions`/mission dir or
    /// runtime file is refused (P1 mission-path-no-follow), never followed
    /// into another repository's tree. If the lock file already exists, the
    /// holder's liveness decides against the [`LockForce`] tier (see its
    /// matrix): a provably
    /// dead holder is always stolen; a live or indeterminate one fails with
    /// [`EngineError::LockHeld`] naming the holder's pid unless the tier
    /// permits the steal. Existing events are loaded to resume the seq
    /// counter and to verify `mission_id` matches the log. Any torn final
    /// line left by a crash is repaired (truncated, or newline-terminated if
    /// the line itself is intact) before the append handle opens, so new
    /// events never glue onto a partial line.
    ///
    /// The lock file records up to three lines — `<pid>`, the acquire time as
    /// unix epoch seconds (diagnostics only), and the holder's own process
    /// identity token — so a later acquire can detect pid reuse: a holder
    /// whose CURRENT token differs from the recorded one is not the process
    /// that wrote the lock. Tokens are compared for raw equality (see
    /// [`process_identity_token`]) — never via clock arithmetic, which
    /// wall-clock steps would poison. The legacy one- and two-line formats
    /// are still accepted; they just forgo reuse detection.
    pub fn acquire(
        paths: &MissionPaths,
        mission_id: &str,
        throttle: Duration,
        force: LockForce,
    ) -> Result<EventLog> {
        // Resolve the mission tree no-follow (P1 mission-path-no-follow): a
        // symlinked component or runtime file is refused before any create or
        // open — an append through a symlink would write another repo's tree.
        let mission_dir = paths.open_mission_dir_nofollow(true)?;
        crate::paths::create_real_subdir(&mission_dir, "runs", &paths.runs_dir())?;
        crate::paths::create_real_subdir(&mission_dir, "control", &paths.control_dir())?;
        ensure_absent_or_regular_at(&mission_dir, "events.jsonl.lock", &paths.lock_file())?;
        ensure_absent_or_regular_at(&mission_dir, "events.jsonl", &paths.events_file())?;

        let lock_path = paths.lock_file();
        let (mut lock_file, lock_generation) =
            match open_create_new_at(&mission_dir, "events.jsonl.lock") {
                Ok(f) => (f, 0u64),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    let (f, prev_gen) =
                        steal_lock(&mission_dir, "events.jsonl.lock", &lock_path, force)?;
                    (f, prev_gen.saturating_add(1))
                }
                Err(e) => return Err(e.into()),
            };

        // From here on we hold the lock; release it if the rest of the
        // acquisition fails so a failed open doesn't strand the mission.
        let mut open = || -> Result<EventLog> {
            // Line 1: pid. Line 2: acquire time (diagnostics only — reuse
            // detection is the token's job). Line 3: our own identity token,
            // where this platform can produce one; a probe that finds it
            // missing degrades to plain pid liveness, never to Dead.
            // Line 4: generation — increments on every steal so a stolen-from
            // process fails closed on its next append.
            lock_file.write_all(
                current_lock_holder_record_with_generation(lock_generation).as_bytes(),
            )?;
            lock_file.flush()?;

            let events_path = paths.events_file();
            // Chain state carried forward from whatever is already on disk.
            let mut prev_hash = String::new();
            let mut tail_had_mac = false;
            let last_seq = if mission_dir
                .symlink_metadata("events.jsonl")
                .is_ok_and(|metadata| metadata.file_type().is_file())
            {
                let parsed = Self::parse_log_file(
                    open_read_at(&mission_dir, "events.jsonl")?,
                    &events_path,
                )?;
                if let Some(first) = parsed.events.first() {
                    if first.mission_id != mission_id {
                        return Err(EngineError::InvalidState(format!(
                            "event log {} belongs to mission '{}', not '{}'",
                            events_path.display(),
                            first.mission_id,
                            mission_id
                        )));
                    }
                }
                // Repair torn writes from a crashed predecessor BEFORE opening
                // the append handle. A torn final line is tolerated on read,
                // but if left in place the next append glues onto it; once a
                // further event lands the spliced garbage is no longer final
                // and every read fails with LogCorruption forever.
                let file_len = mission_dir.metadata("events.jsonl")?.len();
                if parsed.valid_len < file_len {
                    // Unparseable garbage past the last good line: cut it off.
                    let repair = open_write_at(&mission_dir, "events.jsonl", false)?;
                    repair.set_len(parsed.valid_len)?;
                    repair.sync_data()?;
                } else if !parsed.terminated {
                    // The final line parsed but the tear ate its trailing
                    // newline; terminate it so the next append starts fresh.
                    let mut repair = open_append_at(&mission_dir, "events.jsonl", false)?;
                    repair.write_all(b"\n")?;
                    repair.sync_data()?;
                }
                prev_hash = parsed.last_hash.clone().unwrap_or_default();
                tail_had_mac = parsed.saw_mac;
                parsed.events.last().map(|e| e.seq).unwrap_or(0)
            } else {
                0
            };

            let file = open_append_at(&mission_dir, "events.jsonl", true)?;
            // Acquiring the writer IS the operator action that starts or
            // resumes a mission, so it mints the repository key on first
            // use: a mission that never saw a control command must still
            // be sealed, or the MAC and the high-water mark protect nothing
            // until the first `kranz msg`. Minting failure (no resolvable
            // home, an unwritable one) degrades to an unsealed log with a
            // warning rather than refusing every mission on such a host;
            // the readers treat an unsealed log exactly as before.
            // Refuse, never degrade: a writer that carried on unsealed
            // because the key file was unreadable would hand a same-uid
            // attacker exactly the downgrade the seal exists to prevent
            // (zero the key, delete the floor, rewrite the log keyless;
            // follow-up review F-2). No key, no mission.
            let authority_key = Some(
                crate::paths::load_or_create_authority_key(&paths.repo_root).map_err(|error| {
                    EngineError::InvalidState(format!(
                        "cannot mint or read the repository authority key for mission '{mission_id}' \
                         ({}): {error}. The event log is not written unsealed; restore the key \
                         directory before running this mission",
                        crate::paths::authority_key_path(&paths.repo_root)
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "<global kranz dir>/keys/<repo>.key".to_string())
                    ))
                })?,
            );
            if authority_key.is_none() && tail_had_mac {
                // The tail is MACed and we cannot MAC any more: appending
                // would write a downgrade every reader then refuses. Refuse
                // now, naming the key, instead of corrupting the log.
                return Err(EngineError::InvalidState(format!(
                    "event log {} is MAC-protected but the repository authority key is unreadable; \
                     restore {} before running this mission",
                    events_path.display(),
                    crate::paths::authority_key_path(&paths.repo_root)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "~/.kranz/keys/<repo>.key".to_string())
                )));
            }
            if authority_key.is_some() {
                // Record, outside the repo and exactly once, the first seq
                // this writer will seal. From here on an unsealed line at or
                // above that seq is a forgery, not a legacy line, and the
                // lines below it stay grandfathered.
                crate::paths::record_seal_floor(
                    &paths.repo_root,
                    mission_id,
                    last_seq.saturating_add(1),
                )?;
            }
            Ok(EventLog {
                mission_id: mission_id.to_string(),
                events_path,
                lock_path: lock_path.clone(),
                lock_generation,
                lock_token: process_identity_token(std::process::id() as i32),
                mission_dir: mission_dir.try_clone()?,
                file,
                next_seq: last_seq + 1,
                throttle,
                buffer: Vec::new(),
                repo_root: paths.repo_root.clone(),
                prev_hash,
                authority_key,
            })
        };

        match open() {
            Ok(log) => Ok(log),
            Err(e) => {
                let _ = mission_dir.remove_file("events.jsonl.lock");
                Err(e)
            }
        }
    }

    /// Mission id this log was acquired for.
    pub fn mission_id(&self) -> &str {
        &self.mission_id
    }

    /// Seq of the last appended (or loaded) event; 0 for a fresh log.
    pub fn last_seq(&self) -> u64 {
        self.next_seq - 1
    }

    /// Path of the underlying `events.jsonl`.
    pub fn events_path(&self) -> &Path {
        &self.events_path
    }

    /// Append one event: assigns the next seq and the current timestamp,
    /// serializes to a single JSON line, and returns a clone of the stored
    /// event so the caller can broadcast it. If this boundary redacts any
    /// string payload, `secret.redacted` audit events are appended immediately
    /// after the sanitized event.
    ///
    /// Durability: lifecycle events drain any buffered deltas first (file
    /// order == append order), then write + flush + fsync. Stream deltas are
    /// buffered and drained once the oldest buffered delta exceeds the
    /// throttle age — checked here on each append, or on demand (without
    /// waiting for another append) via [`EventLog::flush_if_due`].
    pub fn append(&mut self, kind: EventKind) -> Result<Event> {
        Ok(self.append_with_redaction_audits(kind)?.0)
    }

    /// Append one event and any required `secret.redacted` audit events.
    /// Returns the sanitized primary event plus the audit events that followed
    /// it, so callers that maintain snapshots can fold the same sequence.
    pub fn append_with_redaction_audits(&mut self, kind: EventKind) -> Result<(Event, Vec<Event>)> {
        let (event, redactions) = self.append_redacting(kind)?;
        let mut audits = Vec::new();
        for finding in redactions {
            let (audit, _) = self.append_redacting(EventKind::SecretRedacted {
                rule_id: finding.rule_id,
                fingerprint: finding.fingerprint,
                location: finding.location,
            })?;
            audits.push(audit);
        }
        Ok((event, audits))
    }

    /// Append one event after scanning/redacting string payloads. Returns the
    /// sanitized event plus secret findings (fingerprints only, never values).
    pub fn append_redacting(&mut self, kind: EventKind) -> Result<(Event, Vec<SecretFinding>)> {
        // Fail closed if another process stole the lock out from under us —
        // otherwise two engines dual-write one log (seq gaps / corruption).
        let current_gen = read_lock_info_at(&self.mission_dir, "events.jsonl.lock")
            .generation
            .unwrap_or(0);
        if current_gen != self.lock_generation {
            return Err(EngineError::LockHeld(format!(
                "event log lock for '{}' was stolen (generation {} → {}); refusing append",
                self.mission_id, self.lock_generation, current_gen
            )));
        }

        let event = Event {
            seq: self.next_seq,
            ts: Utc::now(),
            mission_id: self.mission_id.clone(),
            kind,
        };
        let mut value = serde_json::to_value(&event)?;
        let findings = crate::scrub::scrub_json_value(&mut value, "event");
        let event: Event = serde_json::from_value(value)?;
        // Seal AFTER scrubbing, so the chain covers the bytes that actually
        // land on disk rather than the pre-redaction event.
        let (mut line, hash) = seal_line(&event, &self.prev_hash, self.authority_key.as_deref())?;
        self.prev_hash = hash;
        line.push('\n');

        if event.kind.is_stream_delta() {
            self.buffer.push(BufferedLine {
                buffered_at: Instant::now(),
                line,
            });
            let oldest = self.buffer.first().expect("just pushed").buffered_at;
            if oldest.elapsed() >= self.throttle {
                self.drain_buffer()?;
            }
        } else {
            self.drain_buffer()?;
            self.file.write_all(line.as_bytes())?;
            self.file.flush()?;
            self.file.sync_data()?;
            // The line is durable; move the out-of-repo witness up to it.
            // Only sealed missions carry one, the same missions whose lines
            // a forger cannot rewrite, so the mark and the MAC cover the
            // same set.
            if self.authority_key.is_some() {
                crate::paths::record_high_water(&self.repo_root, &self.mission_id, event.seq)?;
            }
        }

        self.next_seq += 1;
        Ok((event, findings))
    }

    /// Write any buffered deltas out to the file (no fsync — deltas are
    /// recoverable).
    pub fn flush(&mut self) -> Result<()> {
        self.drain_buffer()?;
        self.file.flush()?;
        Ok(())
    }

    /// Elapsed time since the OLDEST buffered delta, or `None` when the
    /// buffer is empty.
    pub fn buffer_age(&self) -> Option<Duration> {
        self.buffer.first().map(|b| b.buffered_at.elapsed())
    }

    /// Drain the buffer to the file, WITHOUT waiting for another [`append`]
    /// call, if it is non-empty and has aged past `throttle`. Gives idle
    /// missions (waiting on an approval gate, worker stopped) a wall-clock-
    /// driven flush instead of leaving deltas buffered indefinitely.
    ///
    /// [`append`]: EventLog::append
    pub fn flush_if_due(&mut self) -> Result<bool> {
        match self.buffer_age() {
            Some(age) if age >= self.throttle => {
                self.drain_buffer()?;
                self.file.flush()?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn drain_buffer(&mut self) -> Result<()> {
        drain_lines(&mut self.file, &mut self.buffer)?;
        Ok(())
    }

    // -- readers (no lock required; used by the server/CLI to tail) ---------

    /// Read and validate the full event log at `path`.
    ///
    /// Seq must start at 1 and increase by exactly 1; any gap or duplicate is
    /// [`EngineError::LogCorruption`]. An unparseable FINAL line is a torn
    /// write from a crash and is dropped with a warning; an unparseable line
    /// anywhere else is corruption.
    pub fn read_events(path: &Path) -> Result<Vec<Event>> {
        Ok(Self::parse_log(path)?.events)
    }

    /// Read the log at `path` ONCE and return the validated events together
    /// with the exact byte prefix they were parsed from (12th-pass review):
    /// the evidence bundle must ship `events.jsonl` bytes that reproduce the
    /// chain/cost/escalations it derived, so parsing one snapshot and then
    /// rereading the file for the raw copy is not allowed — a concurrent
    /// append between the two opens would ship bytes the folds never saw.
    ///
    /// Torn-tail rule (the honest one): an unparseable FINAL line is
    /// dropped from the events AND excluded from the returned bytes — the
    /// shipped prefix is exactly what parsed, so the bundle's log always
    /// re-folds to the bundle's derived files. bytes-shipped == bytes-parsed.
    pub fn read_events_and_log_bytes(path: &Path) -> Result<(Vec<Event>, Vec<u8>)> {
        use std::io::Read;
        // Same no-follow refusal as `parse_log`: never read through a
        // symlink, `O_NOFOLLOW` on unix so there is no check-then-open window.
        let mut bytes = Vec::new();
        crate::paths::open_read_nofollow(path)?.read_to_end(&mut bytes)?;
        let parsed = Self::parse_log_bytes(&bytes, path)?;
        bytes.truncate(parsed.valid_len as usize);
        Ok((parsed.events, bytes))
    }

    /// Parse and validate the log at `path`, tracking how many leading bytes
    /// form the valid prefix so [`EventLog::acquire`] can truncate torn tails.
    ///
    /// Works on raw bytes (decoding each line lossily) because a torn write
    /// can split a multi-byte UTF-8 character, which must not render the
    /// whole log unreadable.
    fn parse_log(path: &Path) -> Result<ParsedLog> {
        // A symlinked log file is refused (never read through into another
        // tree); an absent one errors NotFound from the read below, as
        // before. `O_NOFOLLOW` on unix — no check-then-open window.
        Self::parse_log_file(crate::paths::open_read_nofollow(path)?, path)
    }

    /// Parse from an already-open file. Acquisition uses this form so log
    /// recovery reads through the same retained mission capability later
    /// used for truncation, append, and lock removal.
    fn parse_log_file(mut file: File, path: &Path) -> Result<ParsedLog> {
        use std::io::Read;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Self::parse_log_bytes(&bytes, path)
    }

    /// Parse one in-memory buffer — the single entry point every reader
    /// funnels into, so the validation rules (seq contiguity, one mission
    /// id, torn-final-line drop, and the `h`/`m` integrity checks described
    /// in the module docs) can never drift between the file-reading forms and
    /// the single-snapshot form.
    ///
    /// Check order is load-bearing: seq and mission id are verified BEFORE
    /// the chain, so a plain seq gap still reports as a seq discontinuity
    /// rather than as the broken chain it also is.
    fn parse_log_bytes(bytes: &[u8], path: &Path) -> Result<ParsedLog> {
        let identity = log_identity(path);
        if identity.is_none() {
            // A log read from outside the `<repo>/.kranz/missions/<id>/`
            // layout (a bundle, an archive, a copied file) gets the chain
            // check only: no key, no floor, no mark. Say so, because
            // chain-only is no defence against a forger (follow-up review
            // F-6).
            tracing::warn!(
                path = %path.display(),
                "event log read from a non-mission path: integrity verified by chain only"
            );
        }
        let key = identity.and_then(|(root, _)| crate::paths::load_authority_key(root));
        // The floor lives outside the repository, so an attacker who rewrites
        // every line cannot lower it back to "this log was never sealed".
        let seal_floor = identity
            .and_then(|(root, mission)| crate::paths::read_seal_floor(root, mission))
            .unwrap_or(u64::MAX);
        let mut events = Vec::new();
        let mut valid_len: usize = 0;
        let mut terminated = true;
        let mut offset: usize = 0;
        let mut line_no: usize = 0;
        let mut prev_hash = String::new();
        let mut last_hash: Option<String> = None;
        let mut saw_mac = false;
        while offset < bytes.len() {
            line_no += 1;
            let rest = &bytes[offset..];
            let (line_end, step) = match rest.iter().position(|&b| b == b'\n') {
                Some(nl) => (nl, nl + 1),
                None => (rest.len(), rest.len()),
            };
            let is_final = offset + step == bytes.len();
            let line = String::from_utf8_lossy(&rest[..line_end]);
            let mut value: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(err) => {
                    if is_final {
                        tracing::warn!(
                            path = %path.display(),
                            line = line_no,
                            error = %err,
                            "dropping unparseable final event line (torn write)"
                        );
                        break;
                    }
                    return Err(EngineError::LogCorruption(format!(
                        "unparseable event at {}:{}: {err}",
                        path.display(),
                        line_no
                    )));
                }
            };
            // Lift the envelope OUT before deserializing: the canonical bytes the
            // chain covers are the event without them, and removing the keys
            // here means the `Event` type never has to tolerate extras.
            let (presented_hash, presented_mac, version) = match value.as_object_mut() {
                Some(object) => (
                    object
                        .remove(CHAIN_FIELD)
                        .and_then(|v| v.as_str().map(str::to_string)),
                    object
                        .remove(MAC_FIELD)
                        .and_then(|v| v.as_str().map(str::to_string)),
                    object.remove(VERSION_FIELD),
                ),
                None => (None, None, None),
            };
            let version = match version {
                None => 1,
                Some(value) if value.as_u64() == Some(EXACT_FLOAT_VERSION) => EXACT_FLOAT_VERSION,
                Some(_) => {
                    return Err(EngineError::LogCorruption(format!(
                        "unsupported integrity version at {}:{}",
                        path.display(),
                        line_no
                    )));
                }
            };
            if version == 1 {
                restore_legacy_float_parsing(&mut value)?;
            }
            let event: Event = match serde_json::from_value(value) {
                Ok(e) => e,
                Err(err) => {
                    if is_final {
                        tracing::warn!(
                            path = %path.display(),
                            line = line_no,
                            error = %err,
                            "dropping unparseable final event line (torn write)"
                        );
                        break;
                    }
                    return Err(EngineError::LogCorruption(format!(
                        "unparseable event at {}:{}: {err}",
                        path.display(),
                        line_no
                    )));
                }
            };
            let expected = events.len() as u64 + 1;
            if event.seq != expected {
                return Err(EngineError::LogCorruption(format!(
                    "seq discontinuity at {}:{}: expected {expected}, found {}",
                    path.display(),
                    line_no,
                    event.seq
                )));
            }
            if let Some(first_mission_id) = events.first().map(|e: &Event| &e.mission_id) {
                if event.mission_id != *first_mission_id {
                    return Err(EngineError::LogCorruption(format!(
                        "mission_id mismatch at {}:{}: expected '{}' (from first event), found '{}'",
                        path.display(),
                        line_no,
                        first_mission_id,
                        event.mission_id
                    )));
                }
            }
            match &presented_hash {
                Some(hash) => {
                    let body = canonical_event_bytes(&event, version)?;
                    let expected_hash = chain_hash(&prev_hash, &body, version);
                    if expected_hash != *hash {
                        return Err(EngineError::LogCorruption(format!(
                            "integrity chain broken at {}:{}: the line does not hash to its recorded `h`",
                            path.display(),
                            line_no
                        )));
                    }
                    match (&key, &presented_mac) {
                        (Some(key), Some(mac)) => {
                            let expected_mac = crate::hooks::hmac_sha256_hex(key, hash.as_bytes());
                            if !bool::from(expected_mac.as_bytes().ct_eq(mac.as_bytes())) {
                                return Err(EngineError::LogCorruption(format!(
                                    "integrity mac does not verify at {}:{}",
                                    path.display(),
                                    line_no
                                )));
                            }
                        }
                        // A MAC we cannot check is not a MAC we reject: a
                        // reader with no key still gets the chain, which is
                        // the whole point of chaining separately.
                        (None, Some(_)) => {}
                        (_, None) if saw_mac || event.seq >= seal_floor => {
                            return Err(EngineError::LogCorruption(format!(
                                "integrity mac missing at {}:{}: this mission is sealed from seq {}, and earlier lines carry `m`",
                                path.display(),
                                line_no,
                                seal_floor
                            )));
                        }
                        (_, None) => {}
                    }
                    saw_mac |= presented_mac.is_some();
                    prev_hash = hash.clone();
                    last_hash = Some(hash.clone());
                }
                None if last_hash.is_some() => {
                    return Err(EngineError::LogCorruption(format!(
                        "integrity chain dropped at {}:{}: earlier lines carry `h`, so a line without one is a downgrade",
                        path.display(),
                        line_no
                    )));
                }
                None if event.seq >= seal_floor => {
                    return Err(EngineError::LogCorruption(format!(
                        "integrity chain missing at {}:{}: this mission is sealed from seq {seal_floor} on",
                        path.display(),
                        line_no
                    )));
                }
                None if version == EXACT_FLOAT_VERSION => {
                    return Err(EngineError::LogCorruption(format!(
                        "integrity chain missing at {}:{}: versioned lines must be sealed",
                        path.display(),
                        line_no
                    )));
                }
                // Unchained legacy prefix: tolerated, and the chain starts
                // fresh from the empty string at the first line that has one.
                None => {}
            }
            events.push(event);
            offset += step;
            valid_len = offset;
            terminated = step > line_end;
        }
        Ok(ParsedLog {
            events,
            valid_len: valid_len as u64,
            terminated,
            last_hash,
            saw_mac,
        })
    }

    /// Read events with `seq > after_seq` (WS reconnect / tailing). The whole
    /// log is still validated — a corrupt prefix must not go unnoticed.
    pub fn read_events_after(path: &Path, after_seq: u64) -> Result<Vec<Event>> {
        let mut events = Self::read_events(path)?;
        events.retain(|e| e.seq > after_seq);
        Ok(events)
    }

    /// Read the events on the last `max_bytes` of the log WITHOUT reading or
    /// validating the full file — O(tail) I/O for hot callers that only need
    /// trailing facts (e.g. "has a terminal lifecycle event been appended?").
    ///
    /// The window is aligned to the first complete line inside it, and
    /// unparseable lines (a torn final write) are skipped rather than treated
    /// as corruption — callers that need validation use
    /// [`EventLog::read_events`]. Returns the whole log when the file fits
    /// inside the window.
    pub fn read_tail_events(path: &Path, max_bytes: u64) -> Result<Vec<Event>> {
        use std::io::{Read, Seek, SeekFrom};
        // Same no-follow refusal as `parse_log`: never tail through a
        // symlink, `O_NOFOLLOW` on unix so there is no check-then-open window.
        let mut file = crate::paths::open_read_nofollow(path)?;
        let len = file.metadata()?.len();
        let window_start = len.saturating_sub(max_bytes);
        // Read from one byte BEFORE the window: when the window happens to
        // start exactly on a line boundary, that extra byte is the previous
        // line's '\n', so the drop-through-first-'\n' below discards zero
        // content bytes instead of eating one complete in-window line.
        let start = window_start.saturating_sub(1);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = Vec::with_capacity((len - start) as usize);
        file.read_to_end(&mut bytes)?;
        let mut slice = bytes.as_slice();
        if window_start > 0 {
            // Drop the line the window cut into; its head is outside.
            match slice.iter().position(|&b| b == b'\n') {
                Some(nl) => slice = &slice[nl + 1..],
                None => return Ok(Vec::new()),
            }
        }
        Ok(slice
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .filter_map(|line| {
                let mut value: serde_json::Value =
                    serde_json::from_str(&String::from_utf8_lossy(line)).ok()?;
                let object = value.as_object_mut()?;
                let version = object.remove(VERSION_FIELD);
                object.remove(CHAIN_FIELD);
                object.remove(MAC_FIELD);
                match version {
                    None => restore_legacy_float_parsing(&mut value).ok()?,
                    Some(version) if version.as_u64() == Some(EXACT_FLOAT_VERSION) => {}
                    Some(_) => return None,
                }
                serde_json::from_value(value).ok()
            })
            .collect())
    }
}

impl Drop for EventLog {
    fn drop(&mut self) {
        if let Err(e) = self.flush() {
            tracing::warn!(
                error = %e,
                retained = self.buffer.len(),
                "failed to flush event buffer on drop; buffered deltas retained for a future drain"
            );
        }
        // Only remove the lock if we still own it. After a --force-lock steal
        // the stolen-from process's teardown would otherwise delete the
        // stealer's lock file; the stealer would then see generation 0 and
        // fail closed on its next append (MutationLock in queue.rs uses the
        // same still-ours check).
        let info = read_lock_info_at(&self.mission_dir, "events.jsonl.lock");
        let generation_matches = info.generation.unwrap_or(0) == self.lock_generation;
        let token_matches = match (&self.lock_token, &info.token) {
            (Some(ours), Some(theirs)) => ours == theirs,
            // Platforms/legacy files without a token: generation alone is
            // the ownership fence.
            _ => true,
        };
        if generation_matches && token_matches {
            if let Err(e) = self.mission_dir.remove_file("events.jsonl.lock") {
                if e.kind() != ErrorKind::NotFound {
                    tracing::warn!(
                        path = %self.lock_path.display(),
                        error = %e,
                        "failed to remove lock file on drop"
                    );
                }
            }
        }
    }
}

/// Contended-path acquire: decide whether the existing lock at `lock_path`
/// may be stolen under `force` and, if so, atomically replace it with a fresh
/// lock file owned by this process.
///
/// unix: the whole probe→remove→create sequence is serialized under an
/// exclusive `flock` on a sibling guard file (`<lock>.steal`). Unserialized,
/// two concurrent acquires can both judge the same stale holder Dead; the
/// slower one's `remove_file` then deletes the FASTER one's freshly created
/// lock and both end up holding. After winning the flock the lock is
/// re-attempted and re-probed from scratch — it may have been released, or
/// stolen and rewritten, while we waited.
///
/// non-unix: no guard, and that is acceptable — liveness is never probed
/// there (every verdict is Unknown, see [`probe_liveness`]), so the Dead
/// auto-steal path cannot trigger; steals happen only under explicit operator
/// force flags, which are deliberate one-off actions rather than the
/// concurrent-by-accident crash-recovery restarts the guard defends against.
fn steal_lock(
    mission_dir: &Dir,
    lock_name: &str,
    lock_path: &Path,
    force: LockForce,
) -> Result<(File, u64)> {
    #[cfg(unix)]
    let _guard = StealGuard::acquire(mission_dir, lock_name)?;

    loop {
        // The lock may have been RELEASED while we waited for the guard:
        // retry the clean create before probing anything.
        match open_create_new_at(mission_dir, lock_name) {
            Ok(f) => return Ok((f, 0)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }

        let info = read_lock_info_at(mission_dir, lock_name);
        authorize_steal(lock_path, &info, force)?;
        let prev_gen = info.generation.unwrap_or(0);

        // Guarded steals never interleave here, but a rival acquire's FIRST
        // (unguarded) create attempt can still slip into the remove→create
        // window and win the freed slot. If it does, loop back and judge
        // THAT holder like any other — never surface the raw io collision.
        match mission_dir.remove_file(lock_name) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        }
        match open_create_new_at(mission_dir, lock_name) {
            Ok(f) => return Ok((f, prev_gen)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

/// Probe the CURRENT holder recorded at `lock_path` and decide, against the
/// [`LockForce`] matrix, whether stealing is permitted: `Ok(())` authorizes
/// the steal, `Err(LockHeld)` refuses with operator guidance.
fn authorize_steal(lock_path: &Path, info: &LockInfo, force: LockForce) -> Result<()> {
    match (probe_liveness(info), force) {
        // A provably-dead holder is stale (e.g. the engine was Ctrl-C'd —
        // SIGINT skips destructors — or its pid was provably reused): steal
        // it at every tier without demanding --force-lock.
        (LockLiveness::Dead, _) => {
            tracing::warn!(
                lock = %lock_path.display(),
                holder = %info.holder,
                "stale engine lock (holder is dead); taking over"
            );
            Ok(())
        }
        // Not provably dead and no force: refuse. Same message whether the
        // holder is alive or indeterminate — without force the distinction
        // changes nothing for the operator.
        (LockLiveness::Unknown | LockLiveness::Alive, LockForce::No) => {
            Err(EngineError::LockHeld(format!(
                "lock file {} exists (held by pid {}); if that process \
                 is truly gone, re-run with --force-lock",
                lock_path.display(),
                info.holder
            )))
        }
        // Indeterminate liveness (unparseable pid, non-unix platform):
        // --force-lock keeps its historical meaning and steals.
        (LockLiveness::Unknown, LockForce::IfNotLive | LockForce::EvenIfLive) => {
            tracing::warn!(
                lock = %lock_path.display(),
                holder = %info.holder,
                "forced takeover of a lock whose holder's liveness \
                 cannot be determined"
            );
            Ok(())
        }
        // A provably ALIVE holder survives --force-lock: this is exactly how
        // an operator who believes a long run is "stuck" would otherwise
        // corrupt it.
        (LockLiveness::Alive, LockForce::IfNotLive) => Err(EngineError::LockHeld(format!(
            "lock file {} is held by pid {}, and that process is \
             ALIVE — refusing --force-lock. Identify it with \
             `ps -p {}`; pass --dangerously-steal-live-lock ONLY \
             if you are certain it is a zombie or foreign process \
             and not a running kranz engine (two engines on one \
             mission corrupt its event log)",
            lock_path.display(),
            info.holder,
            info.holder
        ))),
        (LockLiveness::Alive, LockForce::EvenIfLive) => {
            tracing::warn!(
                lock = %lock_path.display(),
                holder = %info.holder,
                "DANGEROUS: stealing the mission lock from a LIVE \
                 process at operator request \
                 (--dangerously-steal-live-lock); if that process is \
                 a kranz engine, two engines now write one event log"
            );
            Ok(())
        }
    }
}

/// RAII serialization of the lock-steal sequence (unix): an exclusive
/// `flock` on a sibling guard file (`<lock>.steal`).
///
/// The guard file is best-effort removed on drop, so acquisition must defend
/// against the unlink race: a waiter can win the flock on an inode that was
/// unlinked (and possibly recreated) while it slept, which would serialize
/// nothing. After each flock win the held fd's identity is compared to
/// whatever the path names NOW; a mismatch retries on the fresh file.
///
/// `flock` contends across separate fds within one process too, so the guard
/// serializes racing threads exactly like racing processes.
#[cfg(unix)]
struct StealGuard {
    /// Held only for the flock; dropping (closing) it releases the lock.
    _file: File,
    dir: Dir,
    name: OsString,
}

#[cfg(unix)]
impl StealGuard {
    fn acquire(dir: &Dir, lock_name: &str) -> Result<StealGuard> {
        use std::os::unix::io::AsRawFd;
        let mut name = OsString::from(lock_name);
        name.push(".steal");
        loop {
            // Contents are irrelevant (the file exists only to be flock'd),
            // but be explicit that nothing is truncated.
            use cap_fs_ext::OpenOptionsExt as _;
            use cap_fs_ext::OpenOptionsFollowExt as _;
            use cap_primitives::fs::FollowSymlinks;
            let mut options = cap_std::fs::OpenOptions::new();
            options
                .write(true)
                .truncate(false)
                .follow(FollowSymlinks::No);
            options.custom_flags(libc::O_NONBLOCK);
            let file = match dir.open_with(&name, &options) {
                Ok(file) => file.into_std(),
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    let mut create = cap_std::fs::OpenOptions::new();
                    create
                        .write(true)
                        .create_new(true)
                        .follow(FollowSymlinks::No);
                    create.custom_flags(libc::O_NONBLOCK);
                    match dir.open_with(&name, &create) {
                        Ok(file) => file.into_std(),
                        Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(error) => return Err(error.into()),
            };
            if !file.metadata()?.is_file() {
                return Err(EngineError::InvalidState(format!(
                    "event-log steal guard {} is not a regular file",
                    name.to_string_lossy()
                )));
            }
            loop {
                if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                    break;
                }
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::EINTR) {
                    return Err(err.into());
                }
            }
            let held = file.metadata()?;
            match dir.symlink_metadata(&name) {
                Ok(m)
                    if cap_fs_ext::MetadataExt::dev(&m)
                        == std::os::unix::fs::MetadataExt::dev(&held)
                        && cap_fs_ext::MetadataExt::ino(&m)
                            == std::os::unix::fs::MetadataExt::ino(&held) =>
                {
                    return Ok(StealGuard {
                        _file: file,
                        dir: dir.try_clone()?,
                        name,
                    });
                }
                // The guard file was unlinked (and possibly recreated) while
                // we waited: this flock guards a dead inode and serializes
                // nothing. Retry on whatever the path names now.
                _ => continue,
            }
        }
    }
}

#[cfg(unix)]
impl Drop for StealGuard {
    fn drop(&mut self) {
        // Best-effort cleanup; the identity re-check in acquire() keeps this
        // safe against waiters still blocked on the removed inode.
        let _ = self.dir.remove_file(&self.name);
    }
}

/// Whether the holder recorded in the lock file at `lock_path` is still
/// alive — the ONE liveness query shared by every subsystem that asks "is
/// this mission's engine running?" (the event-log acquire path itself, queue
/// busy checks, hygiene sweeps), so no caller can diverge from the canonical
/// lock-file format.
///
/// Missing lock file → `false` (nothing holds it). A provably-dead holder
/// (ESRCH, or a token-proven pid reuse) → `false`. Everything else — alive
/// holder, unparseable/garbage lock file, unprobeable platform — → `true`:
/// conservative, because a false "alive" merely delays a queued mission or
/// spares a directory from cleaning, while a false "dead" runs two engines
/// on one working tree.
pub fn lock_holder_is_alive(lock_path: &Path) -> bool {
    if !std::fs::symlink_metadata(lock_path).is_ok_and(|m| m.file_type().is_file()) {
        return false;
    }
    let info = read_lock_info(lock_path);
    match probe_liveness(&info) {
        LockLiveness::Dead => false,
        LockLiveness::Alive | LockLiveness::Unknown => true,
    }
}

/// Lock-file contents for a lock held by the current process, in the same
/// format parsed by [`lock_holder_is_alive`].
///
/// Format (four lines):
/// `<pid>\n<acquired_unix_secs>\n<identity_token>\n<generation>\n`
///
/// `generation` increments on every steal so a stolen-from process can detect
/// that its append handle is no longer authoritative.
pub fn current_lock_holder_record() -> String {
    current_lock_holder_record_with_generation(0)
}

fn current_lock_holder_record_with_generation(generation: u64) -> String {
    let acquired_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut contents = format!("{}\n{}\n", std::process::id(), acquired_secs);
    if let Some(token) = process_identity_token(std::process::id() as i32) {
        contents.push_str(&token);
        contents.push('\n');
    } else {
        contents.push('\n');
    }
    contents.push_str(&format!("{generation}\n"));
    contents
}

/// Liveness verdict for the process recorded in a lock file.
///
/// INVARIANT: anything uncertain must NEVER report `Dead` — a false `Dead`
/// lets two engines write one log. `Dead` requires positive proof: ESRCH
/// from `kill(pid, 0)`, or a detected pid reuse (the holder process started
/// after the lock was acquired).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockLiveness {
    /// The holder is provably gone (or the pid provably belongs to a
    /// different, younger process): the lock is stale.
    Dead,
    /// The holder pid maps to a running process.
    Alive,
    /// Cannot tell (unparseable pid, non-unix platform, unexpected probe
    /// errno): honor the lock unless forced.
    Unknown,
}

/// What a lock file records about its holder.
#[derive(Debug)]
struct LockInfo {
    /// First line of the lock file, for error messages (`"unknown"` when the
    /// file is empty or unreadable).
    holder: String,
    /// The holder pid, when the first line parses as a positive i32.
    pid: Option<i32>,
    /// Unix epoch seconds at acquire time (second line): diagnostics only.
    /// `None` for the legacy one-line pid-only format.
    acquired_secs: Option<u64>,
    /// Process identity token the holder recorded for ITSELF at acquire
    /// (third line, see [`process_identity_token`]). `None` for the older
    /// one-/two-line formats and on platforms that cannot produce one —
    /// reuse detection is then impossible and an alive pid is simply Alive.
    token: Option<String>,
    /// Steal generation (fourth line). `None` for legacy lock files — treated
    /// as generation 0 by append ownership checks.
    generation: Option<u64>,
}

/// Best-effort parse of a lock file:
/// `<pid>\n<acquired_unix_epoch_secs>\n<identity_token>\n<generation>`,
/// tolerating the legacy one-/two-/three-line formats and arbitrary garbage.
fn read_lock_info(lock_path: &Path) -> LockInfo {
    use std::io::Read;
    let mut contents = String::new();
    if let Ok(file) = crate::paths::open_read_nofollow(lock_path) {
        let _ = file.take(8 * 1024).read_to_string(&mut contents);
    }
    parse_lock_info(&contents)
}

/// Capability-relative lock read used after [`EventLog::acquire`] pins the
/// mission directory. The small bound prevents a hostile stale lock from
/// turning liveness checks into unbounded allocation.
fn read_lock_info_at(dir: &Dir, name: &str) -> LockInfo {
    use std::io::Read;
    let mut contents = String::new();
    if let Ok(file) = open_read_at(dir, name) {
        let _ = file.take(8 * 1024).read_to_string(&mut contents);
    }
    parse_lock_info(&contents)
}

fn parse_lock_info(contents: &str) -> LockInfo {
    let mut lines = contents.lines();
    let first = lines.next().unwrap_or("").trim();
    let holder = if first.is_empty() {
        "unknown".to_string()
    } else {
        first.to_string()
    };
    let pid = first.parse::<i32>().ok().filter(|p| *p > 0);
    let acquired_secs = lines.next().and_then(|l| l.trim().parse::<u64>().ok());
    let token = lines
        .next()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(String::from);
    let generation = lines.next().and_then(|l| l.trim().parse::<u64>().ok());
    LockInfo {
        holder,
        pid,
        acquired_secs,
        token,
        generation,
    }
}

/// Probe the liveness of a lock file's recorded holder.
///
/// unix: `kill(pid, 0)` == 0 → Alive; EPERM → Alive (exists, not ours);
/// ESRCH → Dead; any other errno → Unknown. Unparseable or non-positive pid →
/// Unknown. Our OWN pid → Alive (we hold it). Non-unix → Unknown. An Alive
/// verdict is then screened for pid reuse (see [`alive_or_reused`]).
fn probe_liveness(info: &LockInfo) -> LockLiveness {
    let Some(pid) = info.pid else {
        return LockLiveness::Unknown;
    };
    if pid as u32 == std::process::id() {
        // We recorded this pid ourselves (double acquire) — or a dead engine
        // did and the OS recycled its pid onto us, which the reuse screen
        // can prove from the timestamps.
        return alive_or_reused(pid, info);
    }
    #[cfg(unix)]
    {
        // kill(pid, 0): 0 = alive; EPERM = alive but not ours; ESRCH = dead.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return alive_or_reused(pid, info);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM) => alive_or_reused(pid, info),
            Some(libc::ESRCH) => LockLiveness::Dead,
            _ => LockLiveness::Unknown,
        }
    }
    #[cfg(not(unix))]
    {
        // No non-unix equivalent of `kill(pid, 0)` is wired up here, and
        // `process_identity_token`'s non-unix stub always returns `None`, so
        // `alive_or_reused` can never reach `Dead` for a foreign pid on this
        // platform. Liveness is therefore unprovable for a foreign holder:
        // report Unknown rather than guessing, and never Dead.
        tracing::debug!(
            pid,
            "liveness cannot be proven for a foreign pid on this platform; \
             reporting Unknown (Dead is unreachable here)"
        );
        non_unix_liveness_fallback()
    }
}

/// The verdict `probe_liveness` reports for a foreign pid on non-unix
/// targets, where liveness cannot be proven. Factored out (and compiled on
/// every platform) so a cross-platform test can pin that it is `Unknown` and
/// never `Dead` — uncertainty must never demote to Dead.
#[cfg_attr(unix, allow(dead_code))]
fn non_unix_liveness_fallback() -> LockLiveness {
    LockLiveness::Unknown
}

/// Screen an Alive pid for reuse by comparing process identity tokens: the
/// token the holder recorded for ITSELF at acquire (lock line 3) against the
/// token of whatever occupies that pid NOW. Tokens are compared for raw
/// equality — no clocks, no slack — so wall-clock steps (NTP corrections,
/// manual resets) can never misclassify a live holder as Dead the way the
/// old start-time-vs-acquire-time arithmetic could. Different tokens prove
/// the pid was recycled within this boot, or the machine rebooted; the
/// process that wrote the lock is dead either way, so the verdict is `Dead`.
/// Any inability to obtain a token (legacy one-/two-line lock file,
/// unsupported platform, probe error) keeps the verdict `Alive` — uncertainty
/// must never demote Alive to Dead.
fn alive_or_reused(pid: i32, info: &LockInfo) -> LockLiveness {
    let Some(recorded) = info.token.as_deref() else {
        return LockLiveness::Alive;
    };
    let Some(current) = process_identity_token(pid) else {
        return LockLiveness::Alive;
    };
    if current == recorded {
        LockLiveness::Alive
    } else {
        tracing::warn!(
            pid,
            recorded_token = recorded,
            current_token = %current,
            lock_acquired_epoch_secs = ?info.acquired_secs,
            "lock holder pid was REUSED: the process now at this pid is not \
             the one that recorded the lock, so the engine that wrote the \
             lock is dead"
        );
        LockLiveness::Dead
    }
}

/// Platform-opaque identity token for process `pid`, or `None` when this
/// platform cannot produce one. Two readings of a LIVE process's token are
/// byte-identical because both derive from the same immutable kernel state
/// (boot + spawn identity), never from wall-clock arithmetic — so tokens are
/// compared for raw equality only.
///
/// linux: `"<boot_id>:<starttime_ticks>"`. `/proc/sys/kernel/random/boot_id`
/// is fixed per boot; `/proc/<pid>/stat` field 22 (starttime) is a
/// CLOCK_MONOTONIC-based tick count fixed for the life of the process. comm
/// (field 2) is parenthesized and may itself contain spaces or ')', so
/// fields 3.. are indexed from after the LAST ')'.
#[cfg(target_os = "linux")]
pub(crate) fn process_identity_token(pid: i32) -> Option<String> {
    let boot_id = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    let boot_id = boot_id.trim();
    if boot_id.is_empty() {
        return None;
    }
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.get(stat.rfind(')')? + 1..)?;
    let start_ticks = rest.split_whitespace().nth(19)?;
    start_ticks.parse::<u64>().ok()?; // reject garbage rather than record it
    Some(format!("{boot_id}:{start_ticks}"))
}

/// macOS: `ps -p <pid> -o lstart=` prints an absolute spawn timestamp
/// rendered from the kernel's stored `p_starttime`. Locale and timezone are
/// pinned so the acquire-time and probe-time renderings of the SAME stored
/// value are byte-identical; the strings are compared for equality, never
/// parsed back into clock arithmetic.
///
/// This is the actual `ps` spawn — the seam [`process_identity_token`] caches
/// in front of. Kept as a separate function (rather than inlining the
/// `Command` call) so a test can observe how many times it actually ran, via
/// [`PS_SPAWN_COUNT`].
#[cfg(target_os = "macos")]
fn ps_identity_token(pid: i32) -> Option<String> {
    #[cfg(test)]
    {
        *PS_SPAWN_COUNTS
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
            .lock()
            .unwrap()
            .entry(pid)
            .or_insert(0) += 1;
    }
    let out = std::process::Command::new("ps")
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// Counts real `ps` spawns from [`ps_identity_token`], keyed by pid, so a
/// test can prove the cache in [`process_identity_token`] collapses repeated
/// checks for one pid into a single spawn — without being confused by other
/// tests in this file concurrently spawning `ps` for a DIFFERENT pid.
/// Test-only: it exists purely to observe the seam.
#[cfg(all(test, target_os = "macos"))]
static PS_SPAWN_COUNTS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<i32, usize>>,
> = std::sync::OnceLock::new();

/// How long a cached macOS identity token may be served before a fresh `ps`
/// spawn is required. This window only needs to be long enough to collapse
/// the handful of `alive_or_reused` calls a single steal decision or hygiene
/// sweep makes for the SAME pid (microseconds to low milliseconds apart in
/// practice); it must stay far shorter than any realistic pid-reuse
/// turnaround (the OS has to fully tear down the old process and allocate a
/// new one, which takes at least tens of milliseconds, typically much more).
/// A cached token therefore can never span an actual reuse: by the time a
/// pid is recycled, the cache entry for it has long since expired and the
/// next probe spawns fresh `ps`.
#[cfg(target_os = "macos")]
const IDENTITY_TOKEN_CACHE_TTL: Duration = Duration::from_millis(50);

/// Per-pid cache of [`ps_identity_token`] results, so a burst of liveness
/// checks against the same pid (queue-busy checks, hygiene sweeps, a single
/// steal decision) spawns at most one `ps`. Keyed by pid so a lookup for one
/// pid can never return another pid's token. Guarded by a `Mutex` for safe
/// concurrent access.
#[cfg(target_os = "macos")]
type IdentityTokenCache =
    std::sync::Mutex<std::collections::HashMap<i32, (Option<String>, Instant)>>;

#[cfg(target_os = "macos")]
static IDENTITY_TOKEN_CACHE: std::sync::OnceLock<IdentityTokenCache> = std::sync::OnceLock::new();

/// macOS identity token WITHOUT the `ps` spawn: `proc_pidinfo(
/// PROC_PIDTBSDINFO)` reads the kernel's stored `p_starttime` directly — the
/// same immutable value `ps -o lstart=` renders — and this renders it
/// byte-identically (ctime shape, UTC: probed 2026-08-05 against
/// `LC_ALL=C TZ=UTC ps -p <pid> -o lstart=`, e.g. `Wed Aug  5 00:34:16 2026`
/// from both paths for the same process). Byte-identity is load-bearing:
/// tokens are compared for raw equality against lock-file recordings that
/// may predate this path (recorded via `ps`), so the rendering must not
/// drift.
///
/// Why this path exists (ticket gate-sandbox-supervision-dogfood): `/bin/ps`
/// is setuid root, and setuid exec is kernel-denied inside ANY Seatbelt
/// sandbox — probed: EPERM even under `(allow default)`, not expressible in
/// SBPL, and a copied binary is AMFI-killed on exec. A process inside the
/// gate sandbox wrap (a wrapped `cargo test --workspace` dogfooding this
/// repo, or any wrapped contract command that probes a kranz lock) could
/// therefore NEVER obtain a token via `ps`. `proc_pidinfo` is not
/// sandbox-gated for same-uid targets (probed under the session-profile
/// posture: self, children, and unrelated same-uid host processes all
/// answer) and needs no spawn at all.
///
/// The limit: OTHER-UID pids. Unprivileged `proc_pidinfo` on pid 1 is EPERM
/// (probed, unsandboxed included) — which is exactly why `/bin/ps` carries
/// the setuid bit. Those pids fall back to the [`ps_identity_token`] spawn
/// seam, which keeps answering them wherever setuid exec is permitted.
#[cfg(target_os = "macos")]
fn proc_pidinfo_identity_token(pid: i32) -> Option<String> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut libc::proc_bsdinfo as *mut libc::c_void,
            std::mem::size_of::<libc::proc_bsdinfo>() as i32,
        )
    };
    if rc <= 0 {
        return None;
    }
    let secs = i64::try_from(info.pbi_start_tvsec).ok()?;
    let rendered = chrono::DateTime::from_timestamp(secs, 0)?
        .format("%a %b %e %H:%M:%S %Y")
        .to_string();
    if rendered.is_empty() {
        None
    } else {
        Some(rendered)
    }
}

/// The uncached token lookup [`process_identity_token`] memoizes:
/// [`proc_pidinfo_identity_token`] first (no spawn, works inside the gate
/// sandbox wrap), the `ps` spawn seam only for the pids the unprivileged
/// syscall cannot read (other-uid — see its doc). [`PS_SPAWN_COUNTS`] still
/// counts REAL spawns only, so the cache test's pid-1 probe stays the sole
/// contributor to its own count.
#[cfg(target_os = "macos")]
fn uncached_identity_token(pid: i32) -> Option<String> {
    if let Some(token) = proc_pidinfo_identity_token(pid) {
        return Some(token);
    }
    ps_identity_token(pid)
}

/// Cache layer in front of [`uncached_identity_token`]: the raw token lookup
/// (proc_pidinfo first, the real `ps` spawn seam behind it).
/// The VERDICT (Alive/Dead) is never cached or short-circuited here — only
/// the raw token lookup is memoized; [`alive_or_reused`] still compares
/// `recorded == current` on every call, using whatever token this returns.
#[cfg(target_os = "macos")]
pub(crate) fn process_identity_token(pid: i32) -> Option<String> {
    let cache = IDENTITY_TOKEN_CACHE
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
    let now = Instant::now();
    if let Some((token, captured)) = cache.lock().unwrap().get(&pid) {
        if now.duration_since(*captured) < IDENTITY_TOKEN_CACHE_TTL {
            return token.clone();
        }
    }
    let identity = uncached_identity_token(pid);
    cache.lock().unwrap().insert(pid, (identity.clone(), now));
    identity
}

/// Everywhere else (windows, exotic unix): no identity token, so pid reuse
/// cannot be proven and an alive holder stays Alive.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn process_identity_token(_pid: i32) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_float_parser_matches_independent_reference() {
        let reference: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/legacy-json-floats.json"))
                .unwrap();
        let cases = reference["cases"].as_array().unwrap();
        assert_eq!(cases.len(), 96);
        for case in cases {
            let number: serde_json::Number =
                serde_json::from_str(case["json"].as_str().unwrap()).unwrap();
            let actual = legacy_float_number(&number)
                .and_then(|n| n.as_f64())
                .map(f64::to_bits);
            let expected = case["expected_bits"]
                .as_str()
                .map(|bits| bits.parse::<u64>().unwrap());
            assert_eq!(actual, expected, "reference: {case}");
        }
    }

    #[test]
    fn versioned_canonical_bytes_sort_nested_objects() {
        let event = Event {
            seq: 1,
            ts: "2026-09-05T00:00:00Z".parse().unwrap(),
            mission_id: "m-canonical".into(),
            kind: EventKind::ConfigChanged {
                patch: serde_json::json!({"z": 1, "a": [{"d": 2, "b": 3}]}),
            },
        };
        assert_eq!(
            canonical_event_bytes(&event, EXACT_FLOAT_VERSION).unwrap(),
            r#"{"missionId":"m-canonical","payload":{"patch":{"a":[{"b":3,"d":2}],"z":1}},"seq":1,"ts":"2026-09-05T00:00:00Z","type":"config.changed"}"#
        );
    }

    // `probe_liveness`'s non-unix arm (the actual code path this pins) only
    // compiles under `#[cfg(not(unix))]`, and our CI runs macOS/Linux, so it
    // cannot be exercised directly here. `non_unix_liveness_fallback` is
    // factored out and compiled on ALL platforms so this cross-platform test
    // can still pin its invariant: uncertainty must never demote to Dead.
    #[test]
    fn non_unix_liveness_fallback_is_never_dead() {
        assert_eq!(non_unix_liveness_fallback(), LockLiveness::Unknown);
        assert_ne!(non_unix_liveness_fallback(), LockLiveness::Dead);
    }

    /// A valid one-event log body for a not-yet-acquired mission.
    // Only the unix symlink tests use this; silence dead_code off-unix
    // without masking it on unix (windows-latest clippy gates -D warnings).
    #[cfg_attr(not(unix), allow(dead_code))]
    fn one_event_line() -> String {
        let event = Event {
            seq: 1,
            ts: Utc::now(),
            mission_id: "m-1".to_string(),
            kind: EventKind::MissionCreated {
                goal: "goal".into(),
                base_branch: "main".into(),
                mission_branch: "kranz/mission-m-1".into(),
                config: crate::types::MissionConfig::default(),
            },
        };
        let mut line = serde_json::to_string(&event).unwrap();
        line.push('\n');
        line
    }

    // Symlink-creating tests are unix-only, exactly like the lessons guard's
    // tests; Windows needs privileges to create symlinks.

    #[cfg(unix)]
    #[test]
    fn read_events_refuses_a_symlinked_log() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        // A valid log at the symlink TARGET: the read must refuse, not
        // return the target's events.
        let target = dir.path().join("target.jsonl");
        std::fs::write(&target, one_event_line()).unwrap();
        let link = dir.path().join("events.jsonl");
        symlink(&target, &link).unwrap();
        let err = EventLog::read_events(&link).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn acquire_refuses_symlinked_runtime_files_without_writing_through() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        let paths = MissionPaths::new(dir.path(), "m-1");
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let target = elsewhere.path().join("elsewhere.jsonl");
        std::fs::write(&target, b"").unwrap();

        // A symlinked events.jsonl is refused before the lock is taken.
        symlink(&target, paths.events_file()).unwrap();
        let err = EventLog::acquire(&paths, "m-1", Duration::ZERO, LockForce::No).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
        assert_eq!(std::fs::read(&target).unwrap(), b"");
        assert!(!paths.lock_file().exists(), "no lock taken on refusal");

        // A symlinked lock file is refused before any steal logic.
        std::fs::remove_file(paths.events_file()).unwrap();
        symlink(&target, paths.lock_file()).unwrap();
        let err = EventLog::acquire(&paths, "m-1", Duration::ZERO, LockForce::No).unwrap_err();
        assert!(err.to_string().contains("refusing"), "{err}");
        assert_eq!(std::fs::read(&target).unwrap(), b"");
    }

    #[cfg(unix)]
    #[test]
    fn acquired_log_retains_mission_capability_across_parent_swap() {
        use std::os::unix::fs::symlink;
        let repo = tempfile::tempdir().unwrap();
        let paths = MissionPaths::new(repo.path(), "m-1");
        let mut log = EventLog::acquire(&paths, "m-1", Duration::ZERO, LockForce::No).unwrap();
        let original = paths.missions_dir().join("m-original");
        std::fs::rename(paths.mission_dir(), &original).unwrap();

        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("events.jsonl.lock"), "outside-lock").unwrap();
        std::fs::write(outside.path().join("events.jsonl"), "outside-events").unwrap();
        symlink(outside.path(), paths.mission_dir()).unwrap();

        log.append(EventKind::MissionPaused {}).unwrap();
        drop(log);

        assert!(
            std::fs::read_to_string(original.join("events.jsonl"))
                .unwrap()
                .contains("mission.paused"),
            "the retained append handle must stay on the originally pinned mission"
        );
        assert!(
            !original.join("events.jsonl.lock").exists(),
            "drop must remove the lock relative to the retained capability"
        );
        assert_eq!(
            std::fs::read_to_string(outside.path().join("events.jsonl")).unwrap(),
            "outside-events"
        );
        assert_eq!(
            std::fs::read_to_string(outside.path().join("events.jsonl.lock")).unwrap(),
            "outside-lock"
        );
    }

    #[test]
    fn lock_info_parses_all_formats() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("l");

        // Current three-line format: pid, acquire time, identity token.
        std::fs::write(&lock, "1234\n1700000000\nabcd-boot-id:5678\n").unwrap();
        let info = read_lock_info(&lock);
        assert_eq!(info.holder, "1234");
        assert_eq!(info.pid, Some(1234));
        assert_eq!(info.acquired_secs, Some(1_700_000_000));
        assert_eq!(info.token.as_deref(), Some("abcd-boot-id:5678"));

        // Two-line format: token unknown, reuse screen impossible.
        std::fs::write(&lock, "1234\n1700000000\n").unwrap();
        let info = read_lock_info(&lock);
        assert_eq!(info.pid, Some(1234));
        assert_eq!(info.acquired_secs, Some(1_700_000_000));
        assert_eq!(info.token, None);

        // An EMPTY third line (a platform with no token) is the same as none.
        std::fs::write(&lock, "1234\n1700000000\n\n").unwrap();
        assert_eq!(read_lock_info(&lock).token, None);

        // Legacy one-line format: pid known, everything else unknown.
        std::fs::write(&lock, "1234").unwrap();
        let info = read_lock_info(&lock);
        assert_eq!(info.pid, Some(1234));
        assert_eq!(info.acquired_secs, None);
        assert_eq!(info.token, None);

        // Garbage: nothing parseable, holder preserved for the message.
        std::fs::write(&lock, "not-a-pid\nnot-a-time").unwrap();
        let info = read_lock_info(&lock);
        assert_eq!(info.holder, "not-a-pid");
        assert_eq!(info.pid, None);
        assert_eq!(info.acquired_secs, None);
        assert_eq!(info.token, None);

        // Non-positive pids are never probeable.
        std::fs::write(&lock, "-4\n1700000000").unwrap();
        assert_eq!(read_lock_info(&lock).pid, None);
    }

    /// Two readings of a live process's identity token must be byte-identical
    /// — the entire reuse screen rests on this. (The old design reconstructed
    /// a wall-clock start time on every probe, which clock steps shifted.)
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn identity_token_is_stable_for_a_live_process() {
        let pid = std::process::id() as i32;
        let a = process_identity_token(pid).expect("own token must be obtainable");
        let b = process_identity_token(pid).expect("own token must be obtainable");
        assert_eq!(a, b, "token readings of the same live process must match");
        assert!(
            !a.is_empty() && !a.contains('\n'),
            "token must be a single non-empty line: {a:?}"
        );
    }

    /// A pid that provably has no process yields no token (dead pids have no
    /// /proc entry on linux and no ps row on macOS) — never a fabricated one.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn identity_token_of_a_dead_pid_is_none() {
        assert_eq!(process_identity_token(i32::MAX), None);
    }

    /// Token mismatch on a lock that records OUR pid: the recorder was a
    /// different process whose pid the OS recycled onto us — provably Dead.
    /// Matching token: an ordinary double acquire — Alive.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn own_pid_reuse_is_decided_by_token_equality() {
        let pid = std::process::id() as i32;
        let own = process_identity_token(pid).expect("own token must be obtainable");

        let info = LockInfo {
            holder: pid.to_string(),
            pid: Some(pid),
            acquired_secs: Some(0),
            token: Some(own.clone()),
            generation: None,
        };
        assert_eq!(probe_liveness(&info), LockLiveness::Alive);

        let info = LockInfo {
            token: Some(format!("{own}-not")),
            ..info
        };
        assert_eq!(probe_liveness(&info), LockLiveness::Dead);
    }

    /// Two `process_identity_token` calls for the SAME pid in quick
    /// succession must spawn `ps` only once — the cache should serve the
    /// second call from memory, and both returned tokens must still match.
    ///
    /// Uses pid 1 (launchd — always alive on macOS) rather than our own pid,
    /// so this test's spawn-count delta is not polluted by other tests in
    /// this file that concurrently probe `process_identity_token` for the
    /// test process's own pid.
    ///
    /// Premise-gated (ticket gate-sandbox-supervision-dogfood): reading
    /// launchd's token needs the setuid `/bin/ps` (unprivileged
    /// `proc_pidinfo` on pid 1 is EPERM — see
    /// [`proc_pidinfo_identity_token`]), and setuid exec is kernel-denied
    /// inside the gate sandbox wrap. Under a wrapped `cargo test` the raw
    /// `ps` seam cannot answer for pid 1, so the test skips with a
    /// detectable marker rather than failing on the sandbox's presence.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_identity_token_caches_one_ps_per_pid() {
        let pid = 1;
        if ps_identity_token(pid).is_none() {
            eprintln!(
                "SKIP-UNDER-WRAP (gate-sandbox-supervision-dogfood): \
                 macos_identity_token_caches_one_ps_per_pid — the setuid /bin/ps cannot \
                 execute inside the gate sandbox wrap, so pid 1's token is unreadable here; \
                 skipping"
            );
            return;
        }
        let count_for_pid = |p: i32| {
            *PS_SPAWN_COUNTS
                .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
                .lock()
                .unwrap()
                .get(&p)
                .unwrap_or(&0)
        };
        let before = count_for_pid(pid);

        let a = process_identity_token(pid).expect("own token must be obtainable");
        let b = process_identity_token(pid).expect("own token must be obtainable");

        let after = count_for_pid(pid);
        assert_eq!(
            after - before,
            1,
            "second call within the cache window must not spawn ps again"
        );
        assert_eq!(a, b, "cached token must match the freshly spawned one");
    }

    /// The cache must never mask pid reuse: it only ever memoizes the
    /// CURRENT token lookup, never the recorded-vs-current comparison. Even
    /// though `current` is served from cache here, a differing `recorded`
    /// token must still yield Dead.
    #[cfg(target_os = "macos")]
    #[test]
    fn macos_cache_never_masks_pid_reuse() {
        let pid = std::process::id() as i32;
        // Prime the cache for this pid.
        let own = process_identity_token(pid).expect("own token must be obtainable");

        let info = LockInfo {
            holder: pid.to_string(),
            pid: Some(pid),
            acquired_secs: Some(0),
            token: Some(format!("{own}-not")),
            generation: None,
        };
        // `current` comes from the cache primed above, but the differing
        // `recorded` token must still be judged Dead — the comparison is
        // never skipped just because `current` was cached.
        assert_eq!(probe_liveness(&info), LockLiveness::Dead);
    }

    /// A writer that succeeds for the first `fail_at` writes and then always
    /// errors, recording every line it actually wrote.
    struct FlakyWriter {
        fail_at: usize,
        writes: Vec<String>,
    }

    impl std::io::Write for FlakyWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.writes.len() >= self.fail_at {
                return Err(std::io::Error::other("simulated write failure"));
            }
            self.writes.push(String::from_utf8_lossy(buf).into_owned());
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn buffered(n: usize) -> Vec<BufferedLine> {
        (0..n)
            .map(|i| BufferedLine {
                buffered_at: Instant::now(),
                line: format!("line-{i}\n"),
            })
            .collect()
    }

    #[test]
    fn drain_retains_unwritten_deltas_on_write_failure() {
        let k = 3;
        let n = 7;
        let mut writer = FlakyWriter {
            fail_at: k,
            writes: Vec::new(),
        };
        let mut buffer = buffered(n);

        let result = drain_lines(&mut writer, &mut buffer);

        assert!(result.is_err(), "drain must surface the write error");
        assert_eq!(
            writer.writes,
            (0..k).map(|i| format!("line-{i}\n")).collect::<Vec<_>>(),
            "exactly the first k lines must have been written, in order"
        );
        assert_eq!(
            buffer.iter().map(|b| b.line.clone()).collect::<Vec<_>>(),
            (k..n).map(|i| format!("line-{i}\n")).collect::<Vec<_>>(),
            "the remaining lines, including the one that failed, must stay buffered in order"
        );

        // A subsequent drain with a working writer must recover the retained
        // lines successfully — nothing is permanently lost.
        let mut retry_writer = FlakyWriter {
            fail_at: usize::MAX,
            writes: Vec::new(),
        };
        let retry_result = drain_lines(&mut retry_writer, &mut buffer);
        assert!(retry_result.is_ok());
        assert!(buffer.is_empty());
        assert_eq!(
            retry_writer.writes,
            (k..n).map(|i| format!("line-{i}\n")).collect::<Vec<_>>()
        );
    }

    /// Build a raw JSONL line for a `mission.paused` event with the given
    /// `seq`/`mission_id` — enough to exercise seq continuity and mission-id
    /// consistency without pulling in the full Event field set.
    fn event_line(seq: u64, mission_id: &str) -> String {
        let event = Event {
            seq,
            ts: Utc::now(),
            mission_id: mission_id.to_string(),
            kind: EventKind::MissionPaused {},
        };
        let mut line = serde_json::to_string(&event).unwrap();
        line.push('\n');
        line
    }

    #[test]
    fn acquire_rejects_foreign_mission_id_in_later_event() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MissionPaths::new(dir.path(), "m-a");
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        let events_path = paths.events_file();

        // Only defect: seq 2's mission_id differs from seq 1's.
        std::fs::write(
            &events_path,
            format!("{}{}", event_line(1, "m-a"), event_line(2, "m-b")),
        )
        .unwrap();

        let err = EventLog::acquire(&paths, "m-a", Duration::from_secs(1), LockForce::No)
            .expect_err("mixed mission_id log must be rejected");
        assert!(
            matches!(err, EngineError::LogCorruption(_)),
            "expected LogCorruption, got {err:?}"
        );

        // Control: identical seqs, single consistent mission_id, acquires cleanly.
        let dir2 = tempfile::tempdir().unwrap();
        let paths2 = MissionPaths::new(dir2.path(), "m-a");
        std::fs::create_dir_all(paths2.mission_dir()).unwrap();
        std::fs::write(
            paths2.events_file(),
            format!("{}{}", event_line(1, "m-a"), event_line(2, "m-a")),
        )
        .unwrap();
        let log = EventLog::acquire(&paths2, "m-a", Duration::from_secs(1), LockForce::No)
            .expect("consistent-mission log must acquire cleanly");
        assert_eq!(log.last_seq(), 2);
    }

    #[test]
    fn parse_log_rejects_mixed_mission_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        std::fs::write(
            &path,
            format!("{}{}", event_line(1, "m-a"), event_line(2, "m-b")),
        )
        .unwrap();

        let err = EventLog::read_events(&path).expect_err("mixed mission_id must be rejected");
        assert!(
            matches!(err, EngineError::LogCorruption(_)),
            "expected LogCorruption, got {err:?}"
        );
    }

    #[test]
    fn acquire_adopts_empty_preexisting_log_as_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let paths = MissionPaths::new(dir.path(), "m-a");
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        std::fs::write(paths.events_file(), "").unwrap();

        let log = EventLog::acquire(&paths, "m-a", Duration::from_secs(1), LockForce::No)
            .expect("empty pre-existing log must be adopted as fresh");
        assert_eq!(log.last_seq(), 0);
        let appended = {
            let mut log = log;
            log.append(EventKind::MissionPaused {}).unwrap()
        };
        assert_eq!(appended.seq, 1);
    }
}
