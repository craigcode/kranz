//! Append-only repo-level signal ledger and scrubbed raw payload store
//! (`docs/scoping/signal-intake.md` §D-A, ticket `signal-intake-2-ledger`).
//!
//! Layout under `<repo>/.kranz/signals/` (gitignored runtime, mode `0700`):
//! - `ledger.jsonl` (mode `0600`): hash-chained, authority-key-MACed JSONL
//!   records with domain separation prefix `kranz.signal-ledger.v1\n`.
//! - `ledger.lock` (mode `0600`): cross-process + in-process writer lock.
//! - `raw/<sha256>` (dir mode `0700`, files mode `0600`): scrubbed raw payload
//!   bodies, written once and verified against their SHA-256 on ledger read.

use crate::error::{EngineError, Result};
use cap_fs_ext::OpenOptionsFollowExt as _;
use cap_primitives::fs::FollowSymlinks;
use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq as _;

/// Hard cap on one incoming signal HTTP body before buffering or persistence.
pub const MAX_RAW_SIGNAL_BYTES: usize = 256 * 1024;

/// Maximum number of raw payload files retained under `.kranz/signals/raw/`.
pub const MAX_RAW_FILES: usize = 1_000;

/// Maximum aggregate bytes across `.kranz/signals/raw/`.
pub const MAX_RAW_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

/// Default per-source intake rate limit within a 60-second rolling window.
pub const DEFAULT_SOURCE_RATE_LIMIT_PER_MIN: usize = 120;

const LEDGER_DOMAIN_PREFIX: &str = "kranz.signal-ledger.v1\n";
const LEDGER_SEAL_ID: &str = "__signal_ledger__";
const LOCK_STALE: Duration = Duration::from_secs(15);
const LOCK_WAIT: Duration = Duration::from_secs(10);

static IN_PROCESS_LEDGER_LOCK: Mutex<()> = Mutex::new(());

/// High-level triage outcome recorded in the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    Proposed,
    Parked,
    NotACodeChange,
}

impl DecisionOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            DecisionOutcome::Proposed => "proposed",
            DecisionOutcome::Parked => "parked",
            DecisionOutcome::NotACodeChange => "not_a_code_change",
        }
    }
}

/// Every lifecycle record kind persisted in `.kranz/signals/ledger.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalRecordKind {
    /// Verified signal payload accepted and stored under `.kranz/signals/raw/<raw_sha256>`.
    Received {
        signal_id: String,
        source: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery_id: Option<String>,
        raw_sha256: String,
        raw_len: usize,
    },
    /// Unverified, replayed, or malformed request dropped before buffering/triage.
    /// Carries neither secret nor payload body.
    Dropped {
        source: String,
        reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delivery_id: Option<String>,
    },
    /// Deterministic triage result for a verified signal.
    Triaged {
        signal_id: String,
        source: String,
        fingerprint: String,
        outcome: DecisionOutcome,
        rule: String,
        dedupe_count: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        candidate_missions: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        severity: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        environment: Option<String>,
    },
    /// Undrafted proposal ticket created or updated in `.kranz/tickets/`.
    Proposed {
        signal_id: String,
        ticket_slug: String,
        fingerprint: String,
        dedupe_count: u64,
    },
    /// Explicit human consent recorded for a signal proposal.
    Consented {
        signal_id: String,
        ticket_slug: String,
        actor: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mission_id: Option<String>,
    },
    /// Human-confirmed `traced-from-mission` defect linkage recorded at draft/consent time.
    Linked {
        signal_id: String,
        ticket_slug: String,
        traced_from_mission: String,
        actor: String,
    },
    /// Write-back initiated (recorded before outbound HTTP request for kill -9 idempotency).
    WriteBackPending {
        signal_id: String,
        ticket_slug: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mission_id: Option<String>,
        status: String,
        idempotency_key: String,
        target_url: String,
        external_id: String,
    },
    /// Outbound status write-back confirmed by the vendor endpoint.
    WriteBackConfirmed {
        signal_id: String,
        ticket_slug: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mission_id: Option<String>,
        status: String,
        idempotency_key: String,
        external_id: String,
    },
    /// Outbound write-back attempt failed (mission is never blocked; retryable on resume).
    WriteBackFailed {
        signal_id: String,
        ticket_slug: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mission_id: Option<String>,
        status: String,
        idempotency_key: String,
        external_id: String,
        error: String,
    },
}

/// One sealed entry in `.kranz/signals/ledger.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalLedgerEntry {
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    #[serde(flatten)]
    pub record: SignalRecordKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SealedLedgerLine {
    #[serde(flatten)]
    entry: SignalLedgerEntry,
    v: u64,
    h: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    m: Option<String>,
}

/// Scrub sensitive headers, cookies, DSNs, and secret tokens from raw payload
/// bytes before persistence under `.kranz/signals/raw/<sha256>`.
pub fn scrub_raw_signal_payload(raw: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(raw);
    let scrubbed = crate::scrub::scrub(&text);
    let mut out = String::with_capacity(scrubbed.len());
    for (idx, line) in scrubbed.split('\n').enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        let cleaned = scrub_sensitive_line(&scrub_dsn_credentials(line));
        out.push_str(&cleaned);
    }
    out.into_bytes()
}

fn scrub_sensitive_line(line: &str) -> String {
    let lower = line.trim_start().to_ascii_lowercase();
    for header in [
        "authorization:",
        "cookie:",
        "set-cookie:",
        "x-sentry-auth:",
        "x-api-key:",
        "api-key:",
    ] {
        if lower.starts_with(header) {
            if let Some((prefix, _)) = line.split_once(':') {
                return format!("{prefix}: [REDACTED]");
            }
        }
    }
    // Also scrub JSON fields naming cookies or auth headers.
    let mut current = line.to_string();
    for key in [
        "\"cookie\"",
        "\"cookies\"",
        "\"authorization\"",
        "\"x-sentry-auth\"",
        "\"dsn\"",
    ] {
        let mut search_from = 0;
        while let Some(rel_pos) = current[search_from..].to_ascii_lowercase().find(key) {
            let pos = search_from + rel_pos;
            let after_key = &current[pos + key.len()..];
            let mut next_search = pos + key.len();
            if let Some(colon) = after_key.find(':') {
                let value_part = &after_key[colon + 1..];
                let trimmed = value_part.trim_start();
                if let Some(rest) = trimmed.strip_prefix('"') {
                    if let Some(end_quote) = rest.find('"') {
                        let start_idx =
                            pos + key.len() + colon + 1 + (value_part.len() - trimmed.len()) + 1;
                        let end_idx = start_idx + end_quote;
                        current.replace_range(start_idx..end_idx, "[REDACTED]");
                        next_search = start_idx + "[REDACTED]".len() + 1;
                    }
                }
            }
            search_from = next_search.min(current.len());
        }
    }
    current
}

fn scrub_dsn_credentials(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(scheme_idx) = rest.find("https://").or_else(|| rest.find("http://")) {
        let scheme_len = if rest[scheme_idx..].starts_with("https://") {
            8
        } else {
            7
        };
        out.push_str(&rest[..scheme_idx + scheme_len]);
        let after_scheme = &rest[scheme_idx + scheme_len..];
        let authority_end = after_scheme
            .find(['/', '"', '\'', ' ', '\t', '\r', '\n', ')', '>'])
            .unwrap_or(after_scheme.len());
        let authority = &after_scheme[..authority_end];
        if let Some((_userinfo, host)) = authority.split_once('@') {
            out.push_str("[REDACTED]@");
            out.push_str(host);
        } else {
            out.push_str(authority);
        }
        rest = &after_scheme[authority_end..];
    }
    out.push_str(rest);
    out
}

/// Cross-process + in-process lock over `.kranz/signals/ledger.lock`.
struct LedgerLock {
    lock_path: PathBuf,
    token: String,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl LedgerLock {
    fn acquire(repo_root: &Path, signals_dir: &Dir) -> Result<Self> {
        let guard = IN_PROCESS_LEDGER_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let lock_path = SignalLedger::signals_dir(repo_root).join("ledger.lock");
        let token = format!(
            "{}:{}",
            std::process::id(),
            uuid::Uuid::new_v4().as_simple()
        );
        let deadline = Instant::now() + LOCK_WAIT;
        loop {
            let mut opts = OpenOptions::new();
            opts.write(true).create_new(true).follow(FollowSymlinks::No);
            #[cfg(unix)]
            {
                use cap_fs_ext::OpenOptionsExt as _;
                opts.mode(0o600);
            }
            match signals_dir.open_with("ledger.lock", &opts) {
                Ok(mut file) => {
                    let _ = file.write_all(token.as_bytes());
                    let _ = file.sync_data();
                    return Ok(Self {
                        lock_path,
                        token,
                        _guard: guard,
                    });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Ok(meta) = signals_dir.symlink_metadata("ledger.lock") {
                        if let Ok(modified) = meta.modified() {
                            if modified.into_std().elapsed().unwrap_or_default() > LOCK_STALE {
                                let _ = signals_dir.remove_file("ledger.lock");
                                continue;
                            }
                        }
                    }
                    if Instant::now() >= deadline {
                        return Err(EngineError::LockHeld(format!(
                            "signal ledger lock held at {}",
                            lock_path.display()
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
}

impl Drop for LedgerLock {
    fn drop(&mut self) {
        if let Ok(current) = std::fs::read_to_string(&self.lock_path) {
            if current == self.token {
                let _ = std::fs::remove_file(&self.lock_path);
            }
        }
    }
}

/// Repo-level signal ledger handle (`<repo>/.kranz/signals/`).
pub struct SignalLedger;

impl SignalLedger {
    pub fn signals_dir(repo_root: &Path) -> PathBuf {
        repo_root.join(".kranz").join("signals")
    }

    pub fn ledger_path(repo_root: &Path) -> PathBuf {
        Self::signals_dir(repo_root).join("ledger.jsonl")
    }

    pub fn raw_dir(repo_root: &Path) -> PathBuf {
        Self::signals_dir(repo_root).join("raw")
    }

    pub fn raw_path(repo_root: &Path, sha256: &str) -> PathBuf {
        Self::raw_dir(repo_root).join(sha256)
    }

    fn open_signals_dirs(repo_root: &Path, create: bool) -> Result<(Dir, Dir)> {
        let root_dir = Dir::open_ambient_dir(repo_root, ambient_authority())?;
        let kranz_path = repo_root.join(".kranz");
        let kranz_dir = crate::paths::open_real_subdir(&root_dir, ".kranz", &kranz_path, create)?;
        let signals_path = kranz_path.join("signals");
        let signals_dir =
            crate::paths::open_real_subdir(&kranz_dir, "signals", &signals_path, create)?;
        let raw_path = signals_path.join("raw");
        let raw_dir = crate::paths::open_real_subdir(&signals_dir, "raw", &raw_path, create)?;
        #[cfg(unix)]
        if create {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&signals_path, std::fs::Permissions::from_mode(0o700));
            let _ = std::fs::set_permissions(&raw_path, std::fs::Permissions::from_mode(0o700));
        }
        Ok((signals_dir, raw_dir))
    }

    /// Record a verified raw signal payload and append a `Received` ledger
    /// entry atomically under `.kranz/signals/ledger.lock`.
    pub fn record_received(
        repo_root: &Path,
        source: &str,
        delivery_id: Option<&str>,
        raw_payload: &[u8],
        rate_limit_per_min: Option<usize>,
    ) -> Result<(SignalLedgerEntry, String)> {
        if raw_payload.len() > MAX_RAW_SIGNAL_BYTES {
            return Err(EngineError::InvalidState(format!(
                "signal payload from '{source}' exceeds 256 KiB cap ({} > {})",
                raw_payload.len(),
                MAX_RAW_SIGNAL_BYTES
            )));
        }
        let (signals_dir, raw_dir) = Self::open_signals_dirs(repo_root, true)?;
        let _lock = LedgerLock::acquire(repo_root, &signals_dir)?;

        let existing = Self::read_entries_unlocked(repo_root, &signals_dir, &raw_dir)?;
        let now = Utc::now();
        let limit = rate_limit_per_min.unwrap_or(DEFAULT_SOURCE_RATE_LIMIT_PER_MIN);
        let recent_from_source = existing
            .iter()
            .filter(|e| {
                (now - e.timestamp).num_seconds() < 60
                    && matches!(
                        &e.record,
                        SignalRecordKind::Received { source: s, .. } if s == source
                    )
            })
            .count();
        if recent_from_source >= limit {
            return Err(EngineError::InvalidState(format!(
                "signal source '{source}' rate limit exceeded ({recent_from_source}/{limit} per minute)"
            )));
        }

        let scrubbed = scrub_raw_signal_payload(raw_payload);
        let raw_sha256 = crate::standards_waiver::sha256_hex(&scrubbed);

        Self::store_raw_unlocked(repo_root, &raw_dir, &raw_sha256, &scrubbed)?;

        let signal_id = format!("sig-{}", &raw_sha256[..12]);
        let kind = SignalRecordKind::Received {
            signal_id,
            source: source.to_string(),
            delivery_id: delivery_id.map(str::to_string),
            raw_sha256: raw_sha256.clone(),
            raw_len: scrubbed.len(),
        };
        let entry = Self::append_unlocked(repo_root, &signals_dir, existing.as_slice(), kind)?;
        Ok((entry, raw_sha256))
    }

    /// Record a dropped (unverified/replayed/malformed) request with no secret
    /// and no payload body.
    pub fn record_dropped(
        repo_root: &Path,
        source: &str,
        reason: &str,
        delivery_id: Option<&str>,
    ) -> Result<SignalLedgerEntry> {
        let clean_reason = crate::scrub::scrub(reason)
            .lines()
            .next()
            .unwrap_or("dropped")
            .chars()
            .take(200)
            .collect::<String>();
        Self::append(
            repo_root,
            SignalRecordKind::Dropped {
                source: source.to_string(),
                reason: clean_reason,
                delivery_id: delivery_id.map(str::to_string),
            },
        )
    }

    /// Append any [`SignalRecordKind`] to `.kranz/signals/ledger.jsonl` under
    /// `.kranz/signals/ledger.lock`.
    pub fn append(repo_root: &Path, record: SignalRecordKind) -> Result<SignalLedgerEntry> {
        let (signals_dir, raw_dir) = Self::open_signals_dirs(repo_root, true)?;
        let _lock = LedgerLock::acquire(repo_root, &signals_dir)?;
        let existing = Self::read_entries_unlocked(repo_root, &signals_dir, &raw_dir)?;
        Self::append_unlocked(repo_root, &signals_dir, existing.as_slice(), record)
    }

    /// Read and cryptographically verify all ledger entries and their raw payloads.
    pub fn read_entries(repo_root: &Path) -> Result<Vec<SignalLedgerEntry>> {
        let (signals_dir, raw_dir) = match Self::open_signals_dirs(repo_root, false) {
            Ok(dirs) => dirs,
            Err(EngineError::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(hwm) = crate::paths::read_high_water(repo_root, LEDGER_SEAL_ID) {
                    if hwm > 0 {
                        return Err(EngineError::LogCorruption(format!(
                            "signal ledger missing but high-water mark is {hwm}"
                        )));
                    }
                }
                return Ok(Vec::new());
            }
            Err(e) => return Err(e),
        };
        let _lock = LedgerLock::acquire(repo_root, &signals_dir)?;
        Self::read_entries_unlocked(repo_root, &signals_dir, &raw_dir)
    }

    /// Read a verified raw payload by its SHA-256 hex digest.
    pub fn read_raw(repo_root: &Path, sha256: &str) -> Result<Vec<u8>> {
        if !is_valid_sha256_hex(sha256) {
            return Err(EngineError::Config(format!(
                "invalid raw signal sha256 '{sha256}'"
            )));
        }
        let (_signals_dir, raw_dir) = Self::open_signals_dirs(repo_root, false)?;
        Self::read_and_verify_raw_unlocked(&raw_dir, sha256)
    }

    fn store_raw_unlocked(
        repo_root: &Path,
        raw_dir: &Dir,
        sha256: &str,
        bytes: &[u8],
    ) -> Result<()> {
        if raw_dir.symlink_metadata(sha256).is_ok() {
            let existing = Self::read_and_verify_raw_unlocked(raw_dir, sha256)?;
            if existing == bytes {
                return Ok(());
            }
            return Err(EngineError::LogCorruption(format!(
                "raw signal payload collision/tamper for {sha256}"
            )));
        }

        // Enforce raw store retention quota before writing a new file.
        let mut count = 0usize;
        let mut total_bytes = 0u64;
        for entry in raw_dir.entries()? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_file() {
                count += 1;
                total_bytes = total_bytes.saturating_add(meta.len());
            }
        }
        if count >= MAX_RAW_FILES
            || total_bytes.saturating_add(bytes.len() as u64) > MAX_RAW_TOTAL_BYTES
        {
            return Err(EngineError::InvalidState(format!(
                "signal raw store quota exceeded in {} (files: {count}/{MAX_RAW_FILES}, bytes: {total_bytes}/{MAX_RAW_TOTAL_BYTES})",
                Self::raw_dir(repo_root).display()
            )));
        }

        let mut opts = OpenOptions::new();
        opts.write(true).create_new(true).follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_fs_ext::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut file = raw_dir.open_with(sha256, &opts)?;
        file.write_all(bytes)?;
        file.sync_data()?;
        Ok(())
    }

    fn read_and_verify_raw_unlocked(raw_dir: &Dir, sha256: &str) -> Result<Vec<u8>> {
        let meta = raw_dir.symlink_metadata(sha256).map_err(|e| {
            EngineError::LogCorruption(format!("missing raw signal payload {sha256}: {e}"))
        })?;
        if meta.is_symlink() || !meta.is_file() {
            return Err(EngineError::LogCorruption(format!(
                "raw signal payload {sha256} is not a regular file"
            )));
        }
        if meta.len() > MAX_RAW_SIGNAL_BYTES as u64 {
            return Err(EngineError::LogCorruption(format!(
                "raw signal payload {sha256} exceeds max size ({} bytes)",
                meta.len()
            )));
        }
        let mut opts = OpenOptions::new();
        opts.read(true).follow(FollowSymlinks::No);
        let mut file = raw_dir.open_with(sha256, &opts)?;
        let mut buf = Vec::with_capacity(meta.len() as usize);
        file.read_to_end(&mut buf)?;
        let actual = crate::standards_waiver::sha256_hex(&buf);
        if actual != sha256 {
            return Err(EngineError::LogCorruption(format!(
                "raw signal payload {sha256} hash mismatch (got {actual})"
            )));
        }
        Ok(buf)
    }

    fn append_unlocked(
        repo_root: &Path,
        signals_dir: &Dir,
        existing: &[SignalLedgerEntry],
        record: SignalRecordKind,
    ) -> Result<SignalLedgerEntry> {
        let key = crate::paths::load_or_create_authority_key(repo_root)?;
        let next_seq = existing.last().map_or(1, |last| last.seq + 1);
        let prev_hash = Self::last_chain_hash(signals_dir)?;

        let entry = SignalLedgerEntry {
            seq: next_seq,
            timestamp: Utc::now(),
            record,
        };
        let (line, _hash) = seal_ledger_entry(&entry, &prev_hash, Some(key.as_slice()))?;

        if next_seq == 1 {
            crate::paths::record_seal_floor(repo_root, LEDGER_SEAL_ID, 1)?;
        }

        let mut opts = OpenOptions::new();
        opts.create(true)
            .append(true)
            .write(true)
            .follow(FollowSymlinks::No);
        #[cfg(unix)]
        {
            use cap_fs_ext::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut file = signals_dir.open_with("ledger.jsonl", &opts)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                Self::ledger_path(repo_root),
                std::fs::Permissions::from_mode(0o600),
            );
        }
        let mut payload = line.into_bytes();
        payload.push(b'\n');
        file.write_all(&payload)?;
        file.sync_data()?;

        crate::paths::record_high_water(repo_root, LEDGER_SEAL_ID, next_seq)?;
        Ok(entry)
    }

    fn last_chain_hash(signals_dir: &Dir) -> Result<String> {
        let mut opts = OpenOptions::new();
        opts.read(true).follow(FollowSymlinks::No);
        let mut file = match signals_dir.open_with("ledger.jsonl", &opts) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
            Err(e) => return Err(e.into()),
        };
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        let Some(last_line) = text.lines().rev().find(|l| !l.trim().is_empty()) else {
            return Ok(String::new());
        };
        let val: serde_json::Value = serde_json::from_str(last_line)
            .map_err(|e| EngineError::LogCorruption(format!("corrupt signal ledger tail: {e}")))?;
        Ok(val
            .get("h")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string())
    }

    fn read_entries_unlocked(
        repo_root: &Path,
        signals_dir: &Dir,
        raw_dir: &Dir,
    ) -> Result<Vec<SignalLedgerEntry>> {
        let mut opts = OpenOptions::new();
        opts.read(true).follow(FollowSymlinks::No);
        let mut file = match signals_dir.open_with("ledger.jsonl", &opts) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if let Some(hwm) = crate::paths::read_high_water(repo_root, LEDGER_SEAL_ID) {
                    if hwm > 0 {
                        return Err(EngineError::LogCorruption(format!(
                            "signal ledger missing but high-water mark is {hwm}"
                        )));
                    }
                }
                return Ok(Vec::new());
            }
            Err(e) => return Err(e.into()),
        };
        let mut text = String::new();
        file.read_to_string(&mut text)?;

        let key = crate::paths::load_authority_key(repo_root);
        let floor = crate::paths::read_seal_floor(repo_root, LEDGER_SEAL_ID);
        let hwm = crate::paths::read_high_water(repo_root, LEDGER_SEAL_ID);

        let mut prev_hash = String::new();
        let mut out = Vec::new();

        for (idx, line) in text.lines().enumerate() {
            if line.trim().is_empty() {
                return Err(EngineError::LogCorruption(format!(
                    "blank line in signal ledger at line {}",
                    idx + 1
                )));
            }
            let sealed: SealedLedgerLine = serde_json::from_str(line).map_err(|e| {
                EngineError::LogCorruption(format!(
                    "malformed signal ledger JSON at line {}: {e}",
                    idx + 1
                ))
            })?;
            let expected_seq = (idx as u64) + 1;
            if sealed.entry.seq != expected_seq {
                return Err(EngineError::LogCorruption(format!(
                    "signal ledger seq gap at line {}: expected {expected_seq}, got {}",
                    idx + 1,
                    sealed.entry.seq
                )));
            }
            if sealed.v != 1 {
                return Err(EngineError::LogCorruption(format!(
                    "unsupported signal ledger version {} at line {}",
                    sealed.v,
                    idx + 1
                )));
            }
            let body = canonical_entry_bytes(&sealed.entry)?;
            let expected_hash = compute_chain_hash(&prev_hash, &body);
            if sealed.h != expected_hash {
                return Err(EngineError::LogCorruption(format!(
                    "signal ledger hash chain mismatch at seq {}",
                    sealed.entry.seq
                )));
            }
            if floor.is_some_and(|f| sealed.entry.seq >= f) && sealed.m.is_none() {
                return Err(EngineError::LogCorruption(format!(
                    "signal ledger missing MAC at seq {} (seal floor {:?})",
                    sealed.entry.seq, floor
                )));
            }
            if let Some(mac) = &sealed.m {
                let Some(key_bytes) = key.as_deref() else {
                    return Err(EngineError::LogCorruption(format!(
                        "signal ledger carries MAC at seq {} but repository authority key is missing",
                        sealed.entry.seq
                    )));
                };
                let expected_mac = compute_entry_mac(key_bytes, &sealed.h);
                if mac.as_bytes().ct_eq(expected_mac.as_bytes()).unwrap_u8() != 1 {
                    return Err(EngineError::LogCorruption(format!(
                        "signal ledger MAC verification failed at seq {}",
                        sealed.entry.seq
                    )));
                }
            }
            if let SignalRecordKind::Received { raw_sha256, .. } = &sealed.entry.record {
                Self::read_and_verify_raw_unlocked(raw_dir, raw_sha256)?;
            }
            prev_hash = sealed.h;
            out.push(sealed.entry);
        }

        if let Some(hwm) = hwm {
            let last_seq = out.last().map_or(0, |e| e.seq);
            if last_seq < hwm {
                return Err(EngineError::LogCorruption(format!(
                    "signal ledger truncated: last seq {last_seq} < high-water mark {hwm}"
                )));
            }
        }

        Ok(out)
    }
}

fn is_valid_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn canonical_entry_bytes(entry: &SignalLedgerEntry) -> Result<String> {
    let mut val = serde_json::to_value(entry)?;
    sort_json_objects(&mut val);
    Ok(serde_json::to_string(&val)?)
}

fn sort_json_objects(val: &mut serde_json::Value) {
    match val {
        serde_json::Value::Array(items) => {
            for item in items {
                sort_json_objects(item);
            }
        }
        serde_json::Value::Object(map) => {
            let mut sorted = BTreeMap::new();
            for (k, mut v) in std::mem::take(map) {
                sort_json_objects(&mut v);
                sorted.insert(k, v);
            }
            for (k, v) in sorted {
                map.insert(k, v);
            }
        }
        _ => {}
    }
}

fn compute_chain_hash(prev_hash: &str, canonical_body: &str) -> String {
    let mut input =
        String::with_capacity(LEDGER_DOMAIN_PREFIX.len() + prev_hash.len() + canonical_body.len());
    input.push_str(LEDGER_DOMAIN_PREFIX);
    input.push_str(prev_hash);
    input.push_str(canonical_body);
    crate::standards_waiver::sha256_hex(input.as_bytes())
}

fn compute_entry_mac(key: &[u8], hash: &str) -> String {
    let mut msg = String::with_capacity(LEDGER_DOMAIN_PREFIX.len() + hash.len());
    msg.push_str(LEDGER_DOMAIN_PREFIX);
    msg.push_str(hash);
    crate::hooks::hmac_sha256_hex(key, msg.as_bytes())
}

fn seal_ledger_entry(
    entry: &SignalLedgerEntry,
    prev_hash: &str,
    key: Option<&[u8]>,
) -> Result<(String, String)> {
    let body = canonical_entry_bytes(entry)?;
    let hash = compute_chain_hash(prev_hash, &body);
    let mac = key.map(|k| compute_entry_mac(k, &hash));
    let sealed = SealedLedgerLine {
        entry: entry.clone(),
        v: 1,
        h: hash.clone(),
        m: mac,
    };
    Ok((serde_json::to_string(&sealed)?, hash))
}
