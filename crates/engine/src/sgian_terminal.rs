//! Sgian-backed [`kranz_acp::terminal::TerminalProvider`] consumer.
//!
//! Routes ACP `terminal/*` requests for an explicitly opted-in, uncontained
//! (`SandboxEnforce::Off`) host session into Sgian panes via HMAC-SHA256
//! execution grants (`<repo>/.kranz/serve.grant.key`, `0600`), without adding a
//! crate dependency on Sgian or relaxing Kranz's one-call consent, scope,
//! lifetime, or cleanup-receipt invariants. Contained profiles never admit or
//! fall back to this provider.

use crate::live_permission::{digest, Proposal};
use crate::sgian::{
    canonical_json, ensure_grant_key, hmac_sha256_hex, load_grant_key, project_terminal_pane,
    resolve_bin, run_ctl_detailed, CtlError, BIN_ENV,
};
use kranz_acp::terminal::{
    Budget, CleanupReceipt, Create, Error, ExitStatus, OutputSnapshot, OutputTail, PaneBinding,
    PaneObservation, Scope, TerminalProvider, DEFAULT_OUTPUT_BYTES,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{watch, Notify};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(8);
const WAIT_POLL_SLICE_MS: u64 = 250;
const MAX_TERMINALS: usize = 8;
const MAX_CREATIONS: usize = 32;
const MAX_OUTPUT_READS_PER_SECOND: usize = 10;

/// Environment keys Sgian is permitted to forward from its scrubbed spawn
/// environment into a grant PTY (`docs/design/execution-grants.md` §2).
/// Caller-supplied environment values in `terminal/create` are never accepted.
pub const GRANT_ENV_ALLOW: &[&str] = &[
    "PATH",
    "HOME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "TERM",
    "COLORTERM",
];

/// Canonical capability minted per `terminal/create` and verified by Sgian
/// against `<repo>/.kranz/serve.grant.key`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionGrant {
    pub grant: String,
    pub mission: String,
    pub run: String,
    pub repo: String,
    pub cwd: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub env_allow: Vec<String>,
    pub expires_ms: u64,
    pub issued_ms: u64,
}

impl ExecutionGrant {
    /// Serialize to canonical sorted-key JSON and sign with RFC 2104 HMAC-SHA256.
    pub fn sign(&self, key: &[u8]) -> Result<(String, String), Error> {
        let value = serde_json::to_value(self).map_err(|_| Error::InvalidRequest)?;
        let canonical = canonical_json(&value);
        let signature = hmac_sha256_hex(key, canonical.as_bytes());
        Ok((canonical, signature))
    }
}

/// Lifetime and post-exit retention ceilings for Sgian-backed ACP terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Budgets {
    pub command_ms: u64,
    pub retention_ms: u64,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            command_ms: 600_000,
            retention_ms: 300_000,
        }
    }
}

impl Budgets {
    pub fn new(command_ms: u64, retention_ms: u64) -> Result<Self, String> {
        if !(1..=600_000).contains(&command_ms) {
            return Err(format!(
                "command_ms ({command_ms}) must be within 1..=600_000"
            ));
        }
        if !(1..=300_000).contains(&retention_ms) {
            return Err(format!(
                "retention_ms ({retention_ms}) must be within 1..=300_000"
            ));
        }
        Ok(Self {
            command_ms,
            retention_ms,
        })
    }
}

/// Explicit supervisor-owned admission input for Sgian-backed ACP terminals.
#[derive(Debug, Clone)]
pub struct Config {
    pub sgian_bin: PathBuf,
    pub repo_root: PathBuf,
    pub mission_id: String,
    pub run_id: String,
    pub budgets: Budgets,
}

impl Config {
    pub fn new(
        sgian_bin: impl Into<PathBuf>,
        repo_root: impl Into<PathBuf>,
        mission_id: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            sgian_bin: sgian_bin.into(),
            repo_root: repo_root.into(),
            mission_id: mission_id.into(),
            run_id: run_id.into(),
            budgets: Budgets::default(),
        }
    }

    /// Resolve `KRANZ_SGIAN_BIN` (explicit absolute path only; never `PATH`).
    pub fn from_env(
        repo_root: impl Into<PathBuf>,
        mission_id: impl Into<String>,
        run_id: impl Into<String>,
    ) -> Option<Self> {
        let bin = resolve_bin(std::env::var_os(BIN_ENV), None)?;
        Some(Self::new(bin, repo_root, mission_id, run_id))
    }

    pub fn with_budgets(mut self, budgets: Budgets) -> Self {
        self.budgets = budgets;
        self
    }
}

/// Validated filesystem and credential context bound before spawning an ACP peer.
#[derive(Debug, Clone)]
pub(crate) struct PreparedContext {
    pub(crate) sgian_bin: PathBuf,
    pub(crate) repo_canon: PathBuf,
    pub(crate) session_cwd_canon: PathBuf,
    pub(crate) mission_id: String,
    pub(crate) run_id: String,
    pub(crate) budgets: Budgets,
    pub(crate) alive: Arc<AtomicBool>,
}

impl PreparedContext {
    pub(crate) fn prepare(config: &Config, session_cwd: &Path) -> Result<Self, String> {
        let _ = Budgets::new(config.budgets.command_ms, config.budgets.retention_ms)?;

        let mission_trimmed = config.mission_id.trim();
        if mission_trimmed.is_empty()
            || mission_trimmed != config.mission_id
            || config.mission_id.len() > 128
            || config.mission_id.chars().any(char::is_control)
        {
            return Err("sgian terminal mission_id must be 1..=128 non-control chars".into());
        }
        let run_trimmed = config.run_id.trim();
        if run_trimmed.is_empty()
            || run_trimmed != config.run_id
            || config.run_id.len() > 48
            || !config.run_id.chars().all(|c| c.is_ascii_graphic())
        {
            return Err(
                "sgian terminal run_id must be 1..=48 printable ASCII graphic chars".into(),
            );
        }

        if !config.sgian_bin.is_absolute() {
            return Err("sgian helper path must be absolute".into());
        }
        let repo_canon = config
            .repo_root
            .canonicalize()
            .map_err(|e| format!("sgian repo_root {}: {e}", config.repo_root.display()))?;
        if !repo_canon.is_dir() {
            return Err(format!(
                "sgian repo_root {} is not a directory",
                repo_canon.display()
            ));
        }

        let session_abs = if session_cwd.is_absolute() {
            session_cwd.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| format!("current_dir failed: {e}"))?
                .join(session_cwd)
        };
        let session_cwd_canon = session_abs
            .canonicalize()
            .map_err(|e| format!("sgian session cwd {}: {e}", session_abs.display()))?;
        if !session_cwd_canon.is_dir() {
            return Err(format!(
                "sgian session cwd {} is not a directory",
                session_cwd_canon.display()
            ));
        }
        if !session_cwd_canon.starts_with(&repo_canon) {
            return Err(format!(
                "sgian session cwd {} escapes repo_root {}",
                session_cwd_canon.display(),
                repo_canon.display()
            ));
        }

        let sgian_bin = config
            .sgian_bin
            .canonicalize()
            .map_err(|e| format!("sgian helper {}: {e}", config.sgian_bin.display()))?;
        if !is_executable_file(&sgian_bin) {
            return Err(format!(
                "sgian helper {} is not an executable regular file",
                sgian_bin.display()
            ));
        }
        if sgian_bin.starts_with(&repo_canon) || sgian_bin.starts_with(&session_cwd_canon) {
            return Err(format!(
                "sgian helper {} must reside outside repo_root and session cwd",
                sgian_bin.display()
            ));
        }

        // Verify (or atomically provision) `.kranz/serve.grant.key` (`0600`) up front.
        let _ = ensure_grant_key(&repo_canon)?;

        Ok(Self {
            sgian_bin,
            repo_canon,
            session_cwd_canon,
            mission_id: config.mission_id.clone(),
            run_id: config.run_id.clone(),
            budgets: config.budgets,
            alive: Arc::new(AtomicBool::new(true)),
        })
    }
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Verify `candidate` is an absolute path with no `.` or `..` components, walks
/// cleanly inside `session_cwd_canon` with no symlink components, and resolves
/// inside both `session_cwd_canon` and `repo_canon`.
fn validate_contained_cwd(
    candidate: &Path,
    session_cwd_canon: &Path,
    repo_canon: &Path,
) -> Result<PathBuf, Error> {
    if !candidate.is_absolute() {
        return Err(Error::InvalidRequest);
    }
    for comp in candidate.components() {
        if matches!(comp, Component::CurDir | Component::ParentDir) {
            return Err(Error::InvalidRequest);
        }
    }
    if let Ok(rel) = candidate.strip_prefix(session_cwd_canon) {
        let mut cursor = session_cwd_canon.to_path_buf();
        for comp in rel.components() {
            let Component::Normal(part) = comp else {
                return Err(Error::InvalidRequest);
            };
            cursor.push(part);
            let meta = std::fs::symlink_metadata(&cursor).map_err(|_| Error::InvalidRequest)?;
            if meta.file_type().is_symlink() || !meta.is_dir() {
                return Err(Error::InvalidRequest);
            }
        }
    }
    let canon = candidate
        .canonicalize()
        .map_err(|_| Error::InvalidRequest)?;
    if !canon.is_dir() || !canon.starts_with(session_cwd_canon) || !canon.starts_with(repo_canon) {
        return Err(Error::InvalidRequest);
    }
    // Re-walk relative to `session_cwd_canon` on the canonical path as well in
    // case the caller supplied an uncanonicalized parent prefix (e.g. `/var` vs `/private/var`).
    let rel_canon = canon
        .strip_prefix(session_cwd_canon)
        .map_err(|_| Error::InvalidRequest)?;
    let mut check = session_cwd_canon.to_path_buf();
    for comp in rel_canon.components() {
        let Component::Normal(part) = comp else {
            return Err(Error::InvalidRequest);
        };
        check.push(part);
        let meta = std::fs::symlink_metadata(&check).map_err(|_| Error::InvalidRequest)?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(Error::InvalidRequest);
        }
    }
    Ok(canon)
}

/// Validate `command`: bare program names are resolved by Sgian via `PATH`
/// (skipping `.` and `cwd`); paths with separators must be absolute, point to
/// an existing executable file, and reside OUTSIDE `repo_canon` and `session_cwd_canon`.
fn validate_command_program(
    raw: &str,
    session_cwd_canon: &Path,
    repo_canon: &Path,
) -> Result<String, Error> {
    if raw.is_empty() || raw == "." || raw == ".." || raw.contains('\0') {
        return Err(Error::InvalidRequest);
    }
    let has_sep = raw.contains('/') || (cfg!(windows) && (raw.contains('\\') || raw.contains(':')));
    if !has_sep {
        return Ok(raw.to_owned());
    }
    let path = Path::new(raw);
    if !path.is_absolute() {
        return Err(Error::InvalidRequest);
    }
    for comp in path.components() {
        if matches!(comp, Component::CurDir | Component::ParentDir) {
            return Err(Error::InvalidRequest);
        }
    }
    let canon = path.canonicalize().map_err(|_| Error::InvalidRequest)?;
    if !is_executable_file(&canon) {
        return Err(Error::InvalidRequest);
    }
    if canon.starts_with(repo_canon) || canon.starts_with(session_cwd_canon) {
        return Err(Error::InvalidRequest);
    }
    canon
        .to_str()
        .map(str::to_owned)
        .ok_or(Error::InvalidRequest)
}

/// Parse Sgian's `{"exit_code": i32|null, "signal": String|null}` (or camelCase
/// `exitCode`) while enforcing `kranz_acp::terminal::ExitStatus`'s strict XOR invariant.
fn parse_sgian_exit_status(value: &Value) -> Result<Option<ExitStatus>, Error> {
    if value.is_null() {
        return Ok(None);
    }
    let obj = value.as_object().ok_or(Error::InvalidRequest)?;
    let raw_code = obj
        .get("exit_code")
        .or_else(|| obj.get("exitCode"))
        .unwrap_or(&Value::Null);
    let raw_sig = obj.get("signal").unwrap_or(&Value::Null);

    let code_opt = if raw_code.is_null() {
        None
    } else {
        Some(raw_code.as_i64().ok_or(Error::InvalidRequest)?)
    };
    let sig_opt = if raw_sig.is_null() {
        None
    } else {
        let s = raw_sig.as_str().ok_or(Error::InvalidRequest)?.trim();
        if s.is_empty() {
            return Err(Error::InvalidRequest);
        }
        Some(s.to_owned())
    };

    if code_opt.is_some() == sig_opt.is_some() {
        return Err(Error::InvalidRequest);
    }

    let status = match (code_opt, sig_opt) {
        (Some(code), None) if (0..=u32::MAX as i64).contains(&code) => ExitStatus {
            exit_code: Some(code as u32),
            signal: None,
        },
        (Some(neg), None) => ExitStatus {
            exit_code: None,
            signal: Some(format!("status:{neg}")),
        },
        (None, Some(sig)) => ExitStatus {
            exit_code: None,
            signal: Some(sig),
        },
        _ => return Err(Error::InvalidRequest),
    };
    Ok(Some(status))
}

struct TempGrantFile {
    path: PathBuf,
}

impl TempGrantFile {
    fn write(canonical_grant: &str) -> Result<Self, Error> {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            ".kranz-sgian-grant-{}.json",
            uuid::Uuid::new_v4().as_simple()
        ));
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).map_err(|_| Error::Unavailable)?;
        if file.write_all(canonical_grant.as_bytes()).is_err() || file.flush().is_err() {
            let _ = std::fs::remove_file(&path);
            return Err(Error::Unavailable);
        }
        Ok(Self { path })
    }
}

impl Drop for TempGrantFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One-use execution authority minted only after Kranz records consent for a [`Proposal`].
#[derive(Debug, PartialEq, Eq)]
pub struct Authority {
    nonce: String,
    scope: Scope,
    digest: String,
    deadline: chrono::DateTime<chrono::Utc>,
    expires_at: tokio::time::Instant,
    action_digest: String,
}

struct State {
    output_limit: usize,
    tail: OutputTail,
    ever_truncated: bool,
    started: bool,
    sgian_terminal_id: Option<String>,
    pane_id: Option<String>,
    exit: Option<ExitStatus>,
    exit_receipt_recorded: bool,
    released: bool,
    releasing: bool,
    cleanup_receipt: Option<CleanupReceipt>,
    failed: bool,
    budget: Option<Budget>,
    retention_expired: bool,
    expired_budget: Option<Budget>,
    output_reads: VecDeque<tokio::time::Instant>,
}

impl State {
    fn apply_sgian_output(&mut self, text: &str, sgian_truncated: bool) -> Result<(), Error> {
        let mut next = OutputTail::new(self.output_limit)?;
        next.push(text);
        if sgian_truncated {
            self.ever_truncated = true;
        }
        self.tail = next;
        Ok(())
    }

    fn current_snapshot(&self) -> OutputSnapshot {
        let mut snap = self.tail.snapshot(self.exit.clone());
        if self.ever_truncated {
            snap.truncated = true;
        }
        snap
    }
}

struct Entry {
    binding: PaneBinding,
    deadline: tokio::time::Instant,
    command_deadline: tokio::time::Instant,
    retention_ms: u64,
    state: Mutex<State>,
    changed: watch::Sender<()>,
    stop_monitor: Notify,
    abort: Mutex<Option<tokio::task::AbortHandle>>,
}

#[derive(Default)]
struct Registry {
    entries: HashMap<String, Arc<Entry>>,
    authorities: HashSet<String>,
    reservations: HashSet<String>,
    retired: HashMap<String, (ExitStatus, Option<Budget>, Option<PaneObservation>)>,
    creations: usize,
}

#[derive(Clone, Default)]
struct Receipts {
    queue: Arc<Mutex<VecDeque<Value>>>,
    ready: Arc<Notify>,
}

impl Receipts {
    fn record(&self, value: Value) -> Result<(), Error> {
        let mut queue = self.queue.lock().map_err(|_| Error::Unavailable)?;
        if queue.len() >= 3 * MAX_CREATIONS {
            return Err(Error::Unavailable);
        }
        queue.push_back(value);
        self.ready.notify_one();
        Ok(())
    }
}

/// Sgian-backed [`TerminalProvider`] for explicitly opted-in host ACP sessions.
pub struct Provider {
    context: PreparedContext,
    pub scope: Scope,
    registry: Arc<Mutex<Registry>>,
    session_deadline: tokio::time::Instant,
    failed: Arc<AtomicBool>,
    failure: Arc<Notify>,
    receipts: Receipts,
}

impl Provider {
    /// Construct an admitted Sgian terminal provider after validating workspace,
    /// binary, and `.kranz/serve.grant.key` invariants.
    pub fn new(
        config: &Config,
        session_cwd: &Path,
        engine_session_id: impl Into<String>,
        peer_session_id: impl Into<String>,
    ) -> Result<Self, String> {
        let context = PreparedContext::prepare(config, session_cwd)?;
        Ok(Self::from_prepared(
            context,
            engine_session_id.into(),
            peer_session_id.into(),
        ))
    }

    pub(crate) fn from_prepared(
        context: PreparedContext,
        engine_session: String,
        peer_session: String,
    ) -> Self {
        let run_id = context.run_id.clone();
        Self {
            context,
            scope: Scope {
                run_id,
                engine_session_id: engine_session,
                peer_session_id: peer_session,
                generation: uuid::Uuid::new_v4().to_string(),
            },
            registry: Arc::new(Mutex::new(Registry::default())),
            session_deadline: tokio::time::Instant::now() + Duration::from_secs(600),
            failed: Arc::new(AtomicBool::new(false)),
            failure: Arc::new(Notify::new()),
            receipts: Receipts::default(),
        }
    }

    fn remaining_session_ms(&self) -> u64 {
        self.session_deadline
            .saturating_duration_since(tokio::time::Instant::now())
            .as_millis() as u64
    }

    fn check_scope(&self, scope: &Scope) -> Result<(), Error> {
        if scope != &self.scope {
            return Err(Error::InvalidHandle);
        }
        if !self.context.alive.load(Ordering::Acquire) || self.failed.load(Ordering::Acquire) {
            return Err(Error::Unavailable);
        }
        Ok(())
    }

    pub fn normalize(&self, mut action: Create) -> Result<Create, Error> {
        let limit = action.validate()?;
        if action.session_id != self.scope.peer_session_id || !action.env.is_empty() {
            return Err(Error::InvalidRequest);
        }
        let command = validate_command_program(
            &action.command,
            &self.context.session_cwd_canon,
            &self.context.repo_canon,
        )?;
        let requested_cwd = match &action.cwd {
            Some(raw) => PathBuf::from(raw),
            None => self.context.session_cwd_canon.clone(),
        };
        let cwd_canon = validate_contained_cwd(
            &requested_cwd,
            &self.context.session_cwd_canon,
            &self.context.repo_canon,
        )?;
        let cwd_str = cwd_canon.to_str().ok_or(Error::InvalidRequest)?.to_owned();
        action.command = command;
        action.cwd = Some(cwd_str);
        action.output_byte_limit = Some(limit as u64);
        action.meta = None;
        Ok(action)
    }

    pub fn action(&self, request: &Create) -> Result<Value, Error> {
        let normalized = self.normalize(request.clone())?;
        Ok(json!({
            "kind": "execute",
            "provider": "sgian",
            "rawInput": normalized,
            "repoRoot": self.context.repo_canon,
            "sessionCwd": self.context.session_cwd_canon,
            "envAllow": GRANT_ENV_ALLOW,
            "terminalScope": self.scope,
            "terminalBudgets": self.context.budgets,
            "sessionCeilingMs": 600_000,
        }))
    }

    pub fn reserve(&self, proposal: &str) -> Result<(), Error> {
        self.check_scope(&self.scope)?;
        if self.remaining_session_ms() == 0 {
            return Err(Error::BudgetExceeded(Budget::SessionLifetime));
        }
        let mut registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
        let reserved = registry.reservations.len() + registry.authorities.len();
        if registry.creations + reserved >= MAX_CREATIONS {
            return Err(Error::BudgetExceeded(Budget::Creations));
        }
        if registry.entries.len() + reserved >= MAX_TERMINALS {
            return Err(Error::BudgetExceeded(Budget::Capacity));
        }
        if !registry.reservations.insert(proposal.to_owned()) {
            return Err(Error::NotAuthorized);
        }
        Ok(())
    }

    pub fn cancel_reservation(&self, proposal: &str) -> Result<(), Error> {
        self.registry
            .lock()
            .map_err(|_| Error::Unavailable)?
            .reservations
            .remove(proposal);
        Ok(())
    }

    pub fn exit_status(&self, id: &str) -> Option<ExitStatus> {
        let registry = self.registry.lock().ok()?;
        if let Some((exit, _, _)) = registry.retired.get(id) {
            return Some(exit.clone());
        }
        let exit = registry.entries.get(id)?.state.lock().ok()?.exit.clone();
        exit
    }

    pub fn authorize(&self, action: &Create, proposal: &Proposal) -> Result<Authority, Error> {
        let mut registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
        let reserved = registry.reservations.remove(&proposal.id);
        self.check_scope(&self.scope)?;
        let now = chrono::Utc::now();
        if !reserved
            || proposal.validate().is_err()
            || proposal.prohibition.is_some()
            || proposal.option(true).is_none()
            || proposal.engine_session_id != self.scope.engine_session_id
            || proposal.peer_session_id != self.scope.peer_session_id
            || now >= proposal.deadline
            || proposal.action != self.action(action)?
        {
            return Err(Error::NotAuthorized);
        }
        let max_ttl = (proposal.deadline - proposal.observed_at)
            .to_std()
            .unwrap_or_default();
        let ttl = (proposal.deadline - now)
            .to_std()
            .unwrap_or_default()
            .min(max_ttl);
        if ttl.is_zero() {
            return Err(Error::NotAuthorized);
        }
        let expires_at = tokio::time::Instant::now() + ttl;
        let normalized = self.normalize(action.clone())?;
        let digest = digest(&normalized).map_err(|_| Error::InvalidRequest)?;
        let nonce = uuid::Uuid::new_v4().to_string();
        registry.authorities.insert(nonce.clone());
        Ok(Authority {
            nonce,
            deadline: proposal.deadline,
            expires_at,
            action_digest: proposal.action_digest.clone(),
            scope: self.scope.clone(),
            digest,
        })
    }

    /// Project a verified [`PaneObservation`] over a live, exited, released, or failed handle.
    pub fn observe_pane(&self, scope: &Scope, id: &str) -> Result<PaneObservation, Error> {
        if scope != &self.scope {
            return Err(Error::InvalidHandle);
        }
        let daemon_alive =
            self.context.alive.load(Ordering::Acquire) && !self.failed.load(Ordering::Acquire);
        let registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
        if let Some((_, budget, obs)) = registry.retired.get(id) {
            if let Some(b) = budget {
                return Err(Error::BudgetExceeded(*b));
            }
            if !daemon_alive {
                return Err(Error::Unavailable);
            }
            let retained = obs.clone().ok_or(Error::InvalidHandle)?;
            retained.verify_against(&retained.binding)?;
            return Ok(retained);
        }
        let entry = registry
            .entries
            .get(id)
            .cloned()
            .ok_or(Error::InvalidHandle)?;
        let state = entry.state.lock().map_err(|_| Error::Unavailable)?;
        let cleanup_confirmed = !state.failed;
        project_terminal_pane(
            &entry.binding,
            entry.binding.clone(),
            state.current_snapshot(),
            state.cleanup_receipt.clone(),
            daemon_alive,
            cleanup_confirmed,
        )
    }

    fn entry(&self, scope: &Scope, id: &str) -> Result<Arc<Entry>, Error> {
        self.check_scope(scope)?;
        let registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
        if let Some((_, Some(budget), _)) = registry.retired.get(id) {
            return Err(Error::BudgetExceeded(*budget));
        }
        let entry = registry
            .entries
            .get(id)
            .cloned()
            .ok_or(Error::InvalidHandle)?;
        {
            let state = entry.state.lock().map_err(|_| Error::Unavailable)?;
            if state.retention_expired {
                return Err(Error::BudgetExceeded(
                    state.expired_budget.unwrap_or(Budget::Retention),
                ));
            }
            if state.releasing || state.released {
                return Err(Error::InvalidHandle);
            }
        }
        Ok(entry)
    }

    pub(crate) fn fail(&self) {
        self.failed.store(true, Ordering::Release);
        self.failure.notify_one();
    }

    pub(crate) async fn failure(&self) {
        if !self.failed.load(Ordering::Acquire) {
            self.failure.notified().await;
        }
    }

    pub(crate) fn stop(&self) {
        self.context.alive.store(false, Ordering::Release);
        if let Ok(registry) = self.registry.lock() {
            for entry in registry.entries.values() {
                entry.stop_monitor.notify_waiters();
                if let Ok(abort) = entry.abort.lock() {
                    if let Some(abort) = &*abort {
                        abort.abort();
                    }
                }
            }
        }
    }

    pub(crate) async fn next_receipt(&self) -> Result<Value, Error> {
        loop {
            if let Some(value) = self
                .receipts
                .queue
                .lock()
                .map_err(|_| Error::Unavailable)?
                .pop_front()
            {
                return Ok(value);
            }
            self.receipts.ready.notified().await;
        }
    }

    pub(crate) fn drain_receipts(&self) -> Result<Vec<Value>, Error> {
        Ok(self
            .receipts
            .queue
            .lock()
            .map_err(|_| Error::Unavailable)?
            .drain(..)
            .collect())
    }

    pub(crate) async fn close(self: &Arc<Self>) -> Result<Vec<CleanupReceipt>, Error> {
        self.check_scope(&self.scope)?;
        let mut ids: Vec<_> = self
            .registry
            .lock()
            .map_err(|_| Error::Unavailable)?
            .entries
            .keys()
            .cloned()
            .collect();
        ids.sort();
        let mut tasks = tokio::task::JoinSet::new();
        for (index, id) in ids.into_iter().enumerate() {
            let provider = Arc::clone(self);
            let scope = self.scope.clone();
            tasks.spawn(async move { (index, provider.release(scope, id).await) });
        }
        let mut indexed = Vec::new();
        while let Some(joined) = tasks.join_next().await {
            let (index, outcome) = joined.map_err(|_| Error::Unavailable)?;
            match outcome {
                Ok(receipt) => indexed.push((index, receipt)),
                Err(Error::BudgetExceeded(Budget::Retention | Budget::SessionLifetime)) => {}
                Err(error) => return Err(error),
            }
        }
        indexed.sort_by_key(|(index, _)| *index);
        Ok(indexed.into_iter().map(|(_, receipt)| receipt).collect())
    }

    /// Best-effort bounded cleanup on abnormal session abort (`kill_child`).
    /// Returns `true` iff every live Sgian terminal handle confirmed `descendants_reaped`
    /// and `output_drained` on `grant release`.
    pub(crate) async fn abort_cleanup(&self) -> bool {
        let sgian_ids: Vec<String> = {
            let Ok(mut registry) = self.registry.lock() else {
                return false;
            };
            let mut ids = Vec::new();
            for (_, entry) in registry.entries.drain() {
                entry.stop_monitor.notify_waiters();
                if let Ok(mut abort) = entry.abort.lock() {
                    if let Some(handle) = abort.take() {
                        handle.abort();
                    }
                }
                if let Ok(mut state) = entry.state.lock() {
                    state.releasing = true;
                    state.released = true;
                    if let Some(sgian_id) = state.sgian_terminal_id.take() {
                        ids.push(sgian_id);
                    }
                }
            }
            ids
        };
        if sgian_ids.is_empty() {
            return true;
        }
        let bin = self.context.sgian_bin.clone();
        let repo = self.context.repo_canon.clone();
        tokio::task::spawn_blocking(move || release_sgian_handles_sync(&bin, &repo, &sgian_ids))
            .await
            .unwrap_or(false)
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.stop();
        let sgian_ids: Vec<String> = self
            .registry
            .lock()
            .map(|mut reg| {
                reg.entries
                    .drain()
                    .filter_map(|(_, entry)| {
                        entry
                            .state
                            .lock()
                            .ok()
                            .and_then(|mut s| s.sgian_terminal_id.take())
                    })
                    .collect()
            })
            .unwrap_or_default();
        if !sgian_ids.is_empty() {
            let bin = self.context.sgian_bin.clone();
            let repo = self.context.repo_canon.clone();
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(move || {
                    let _ = release_sgian_handles_sync(&bin, &repo, &sgian_ids);
                });
            } else {
                let _ = std::thread::Builder::new()
                    .name("kranz-sgian-term-cleanup".into())
                    .spawn(move || {
                        let _ = release_sgian_handles_sync(&bin, &repo, &sgian_ids);
                    });
            }
        }
    }
}

fn release_sgian_handles_sync(bin: &Path, repo: &Path, sgian_ids: &[String]) -> bool {
    let ws = repo.display().to_string();
    let mut all_confirmed = true;
    for sgian_id in sgian_ids {
        let args = [
            "ctl",
            "--workspace",
            &ws,
            "--json",
            "grant",
            "release",
            sgian_id,
        ];
        match run_ctl_detailed(
            bin,
            &args,
            crate::git_ops::process::Limits::for_control(OPERATION_TIMEOUT),
        ) {
            Ok(stdout) => {
                let confirmed = serde_json::from_slice::<Value>(&stdout)
                    .ok()
                    .is_some_and(|v| {
                        v["released"] == true
                            && v["descendants_reaped"] == true
                            && v["output_drained"] == true
                    });
                if !confirmed {
                    all_confirmed = false;
                }
            }
            Err(_) => {
                all_confirmed = false;
            }
        }
    }
    all_confirmed
}

fn ctl_grant_output_sync(
    bin: &Path,
    repo: &Path,
    sgian_id: &str,
) -> Result<(String, bool, Option<ExitStatus>), Error> {
    let ws = repo.display().to_string();
    let args = [
        "ctl",
        "--workspace",
        &ws,
        "--json",
        "grant",
        "output",
        sgian_id,
    ];
    let stdout = run_ctl_detailed(
        bin,
        &args,
        crate::git_ops::process::Limits::for_terminal_output(OPERATION_TIMEOUT),
    )
    .map_err(|_| Error::Unavailable)?;
    let value: Value = serde_json::from_slice(&stdout).map_err(|_| Error::Unavailable)?;
    let text = value["output"]
        .as_str()
        .ok_or(Error::Unavailable)?
        .to_owned();
    let truncated = value["truncated"].as_bool().unwrap_or(false);
    let exit_val = value.get("exit_status").unwrap_or(&Value::Null);
    let exit_status = parse_sgian_exit_status(exit_val).map_err(|_| Error::Unavailable)?;
    Ok((text, truncated, exit_status))
}

enum WaitPollOutcome {
    Running,
    Exited(ExitStatus),
}

fn ctl_grant_wait_slice_sync(
    bin: &Path,
    repo: &Path,
    sgian_id: &str,
    slice_ms: u64,
) -> Result<WaitPollOutcome, Error> {
    let ws = repo.display().to_string();
    let timeout_str = slice_ms.max(1).to_string();
    let args = [
        "ctl",
        "--workspace",
        &ws,
        "--json",
        "grant",
        "wait",
        sgian_id,
        "--timeout",
        &timeout_str,
    ];
    let deadline = Duration::from_millis(slice_ms.max(1)) + OPERATION_TIMEOUT;
    let raw_bytes = match run_ctl_detailed(
        bin,
        &args,
        crate::git_ops::process::Limits::for_control(deadline),
    ) {
        Ok(stdout) => stdout,
        Err(CtlError::Exited { stdout, .. }) if !stdout.is_empty() => stdout,
        Err(_) => return Err(Error::Unavailable),
    };
    let value: Value = serde_json::from_slice(&raw_bytes).map_err(|_| Error::Unavailable)?;
    if value["exited"] == true {
        let exit_val = value.get("exit_status").unwrap_or(&Value::Null);
        let status = parse_sgian_exit_status(exit_val)
            .map_err(|_| Error::Unavailable)?
            .ok_or(Error::Unavailable)?;
        Ok(WaitPollOutcome::Exited(status))
    } else if value["timed_out"] == true {
        Ok(WaitPollOutcome::Running)
    } else {
        Err(Error::Unavailable)
    }
}

fn ctl_grant_kill_sync(bin: &Path, repo: &Path, sgian_id: &str) -> Result<ExitStatus, Error> {
    let ws = repo.display().to_string();
    let args = [
        "ctl",
        "--workspace",
        &ws,
        "--json",
        "grant",
        "kill",
        sgian_id,
    ];
    let stdout = run_ctl_detailed(
        bin,
        &args,
        crate::git_ops::process::Limits::for_control(OPERATION_TIMEOUT),
    )
    .map_err(|_| Error::CleanupUnconfirmed)?;
    let value: Value = serde_json::from_slice(&stdout).map_err(|_| Error::CleanupUnconfirmed)?;
    if value["descendants_reaped"] != true
        || value["output_drained"] != true
        || value["released"] != false
    {
        return Err(Error::CleanupUnconfirmed);
    }
    let exit_val = value.get("exit_status").unwrap_or(&Value::Null);
    parse_sgian_exit_status(exit_val)
        .map_err(|_| Error::CleanupUnconfirmed)?
        .ok_or(Error::CleanupUnconfirmed)
}

fn ctl_grant_release_sync(bin: &Path, repo: &Path, sgian_id: &str) -> Result<ExitStatus, Error> {
    let ws = repo.display().to_string();
    let args = [
        "ctl",
        "--workspace",
        &ws,
        "--json",
        "grant",
        "release",
        sgian_id,
    ];
    let stdout = run_ctl_detailed(
        bin,
        &args,
        crate::git_ops::process::Limits::for_control(OPERATION_TIMEOUT),
    )
    .map_err(|_| Error::CleanupUnconfirmed)?;
    let value: Value = serde_json::from_slice(&stdout).map_err(|_| Error::CleanupUnconfirmed)?;
    if value["released"] != true
        || value["descendants_reaped"] != true
        || value["output_drained"] != true
    {
        return Err(Error::CleanupUnconfirmed);
    }
    let exit_val = value.get("exit_status").unwrap_or(&Value::Null);
    parse_sgian_exit_status(exit_val)
        .map_err(|_| Error::CleanupUnconfirmed)?
        .ok_or(Error::CleanupUnconfirmed)
}

fn record_exit_once(
    entry: &Entry,
    receipts: &Receipts,
    status: ExitStatus,
    budget: Option<Budget>,
) -> Result<(), Error> {
    let mut state = entry.state.lock().map_err(|_| Error::Unavailable)?;
    if state.exit.is_none() {
        state.exit = Some(status.clone());
    }
    if budget.is_some() && state.budget.is_none() {
        state.budget = budget;
    }
    if !state.exit_receipt_recorded {
        let effective_exit = state.exit.clone().unwrap_or(status);
        let effective_budget = state.budget;
        receipts.record(json!({
            "terminalExited": {
                "terminalId": entry.binding.terminal_id,
                "scope": entry.binding.scope,
                "actionDigest": entry.binding.action_digest,
                "status": effective_exit,
                "descendantsReaped": true,
                "outputDrained": true,
                "budget": effective_budget,
            }
        }))?;
        state.exit_receipt_recorded = true;
    }
    Ok(())
}

async fn monitor_entry(
    entry: Arc<Entry>,
    bin: PathBuf,
    repo: PathBuf,
    sgian_id: String,
    session_deadline: tokio::time::Instant,
    receipts: Receipts,
    registry: Arc<Mutex<Registry>>,
) -> Result<(), Error> {
    // Phase 1: supervise running command until exit, explicit release, or command/session deadline.
    loop {
        let already_done = {
            let state = entry.state.lock().map_err(|_| Error::Unavailable)?;
            state.releasing || state.released || state.exit.is_some()
        };
        if already_done {
            break;
        }

        let now = tokio::time::Instant::now();
        let active_limit = entry.command_deadline.min(session_deadline);
        if now >= active_limit {
            let hit_budget = if session_deadline <= entry.command_deadline {
                Budget::SessionLifetime
            } else {
                Budget::CommandLifetime
            };
            let bin_kill = bin.clone();
            let repo_kill = repo.clone();
            let id_kill = sgian_id.clone();
            let (status, out_res) = tokio::task::spawn_blocking(move || {
                let status = ctl_grant_kill_sync(&bin_kill, &repo_kill, &id_kill)?;
                let out = ctl_grant_output_sync(&bin_kill, &repo_kill, &id_kill);
                Ok::<_, Error>((status, out))
            })
            .await
            .map_err(|_| Error::CleanupUnconfirmed)??;

            if let Ok((text, trunc, _)) = out_res {
                if let Ok(mut state) = entry.state.lock() {
                    let _ = state.apply_sgian_output(&text, trunc);
                }
            }
            record_exit_once(&entry, &receipts, status, Some(hit_budget))?;
            entry.changed.send_replace(());
            break;
        }

        let rem_ms = active_limit.saturating_duration_since(now).as_millis() as u64;
        let slice_ms = rem_ms.clamp(1, WAIT_POLL_SLICE_MS);
        let bin_wait = bin.clone();
        let repo_wait = repo.clone();
        let id_wait = sgian_id.clone();
        let wait_fut = tokio::task::spawn_blocking(move || {
            ctl_grant_wait_slice_sync(&bin_wait, &repo_wait, &id_wait, slice_ms)
        });

        let outcome = tokio::select! {
            _ = entry.stop_monitor.notified() => return Ok(()),
            res = wait_fut => res.map_err(|_| Error::Unavailable)?,
        };

        match outcome {
            Ok(WaitPollOutcome::Running) => continue,
            Ok(WaitPollOutcome::Exited(status)) => {
                let releasing = entry
                    .state
                    .lock()
                    .map(|s| s.releasing || s.released)
                    .unwrap_or(true);
                if releasing {
                    return Ok(());
                }
                let bin_out = bin.clone();
                let repo_out = repo.clone();
                let id_out = sgian_id.clone();
                if let Ok(Ok((text, trunc, _))) = tokio::task::spawn_blocking(move || {
                    ctl_grant_output_sync(&bin_out, &repo_out, &id_out)
                })
                .await
                {
                    if let Ok(mut state) = entry.state.lock() {
                        let _ = state.apply_sgian_output(&text, trunc);
                    }
                }
                record_exit_once(&entry, &receipts, status, None)?;
                entry.changed.send_replace(());
                break;
            }
            Err(err) => {
                let releasing = entry
                    .state
                    .lock()
                    .map(|s| s.releasing || s.released)
                    .unwrap_or(true);
                if releasing {
                    return Ok(());
                }
                return Err(err);
            }
        }
    }

    // Phase 2: enforce post-exit retention window and reclaim unreleased Sgian handle.
    let retention_deadline = (tokio::time::Instant::now()
        + Duration::from_millis(entry.retention_ms))
    .min(session_deadline);
    tokio::select! {
        _ = entry.stop_monitor.notified() => return Ok(()),
        _ = tokio::time::sleep_until(retention_deadline) => {}
    }

    let expired_budget = if session_deadline <= retention_deadline {
        Budget::SessionLifetime
    } else {
        Budget::Retention
    };
    let should_reclaim = {
        let mut state = entry.state.lock().map_err(|_| Error::Unavailable)?;
        if state.releasing || state.released {
            false
        } else {
            state.retention_expired = true;
            state.expired_budget = Some(expired_budget);
            state.releasing = true;
            true
        }
    };
    if !should_reclaim {
        return Ok(());
    }

    let bin_rel = bin;
    let repo_rel = repo;
    let id_rel = sgian_id;
    let exit_status =
        tokio::task::spawn_blocking(move || ctl_grant_release_sync(&bin_rel, &repo_rel, &id_rel))
            .await
            .map_err(|_| Error::CleanupUnconfirmed)??;

    {
        let mut state = entry.state.lock().map_err(|_| Error::Unavailable)?;
        state.released = true;
        if state.exit.is_none() {
            state.exit = Some(exit_status.clone());
        }
    }
    if let Ok(mut reg) = registry.lock() {
        let handle_id = entry.binding.terminal_id.clone();
        reg.entries.remove(&handle_id);
        reg.retired
            .insert(handle_id, (exit_status, Some(expired_budget), None));
    }
    entry.changed.send_replace(());
    Ok(())
}

async fn wait_state<T>(entry: &Entry, predicate: impl Fn(&State) -> Option<T>) -> Result<T, Error> {
    let mut changed = entry.changed.subscribe();
    loop {
        {
            let state = entry.state.lock().map_err(|_| Error::Unavailable)?;
            if state.failed {
                return Err(Error::CleanupUnconfirmed);
            }
            if let Some(value) = predicate(&state) {
                return Ok(value);
            }
        }
        changed.changed().await.map_err(|_| Error::Unavailable)?;
    }
}

impl TerminalProvider for Provider {
    type Authority = Authority;

    async fn create(
        &self,
        scope: Scope,
        action: Create,
        authority: Authority,
    ) -> Result<String, Error> {
        let (normalized, id, entry, session_ms, ttl_ms) = {
            let mut registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
            // Always burn the single-use authority first.
            let known = registry.authorities.remove(&authority.nonce);
            self.check_scope(&scope)?;
            if !known {
                return Err(Error::NotAuthorized);
            }
            let normalized = self.normalize(action)?;
            let now_utc = chrono::Utc::now();
            let now_inst = tokio::time::Instant::now();
            if now_utc >= authority.deadline
                || now_inst >= authority.expires_at
                || authority.scope != scope
                || authority.digest != digest(&normalized).map_err(|_| Error::InvalidRequest)?
            {
                return Err(Error::NotAuthorized);
            }
            let session_ms = self.remaining_session_ms();
            if session_ms == 0 {
                return Err(Error::BudgetExceeded(Budget::SessionLifetime));
            }
            let limit = normalized
                .output_byte_limit
                .unwrap_or(DEFAULT_OUTPUT_BYTES as u64) as usize;
            let tail = OutputTail::new(limit)?;
            let id = format!("terminal-{}", uuid::Uuid::new_v4());
            let (changed, _) = watch::channel(());
            let command_budget_ms = session_ms.min(self.context.budgets.command_ms);
            let command_deadline = now_inst + Duration::from_millis(command_budget_ms);
            let deadline = now_inst
                + Duration::from_millis(
                    session_ms.min(
                        self.context
                            .budgets
                            .command_ms
                            .saturating_add(self.context.budgets.retention_ms),
                    ),
                )
                + 3 * OPERATION_TIMEOUT;
            let binding = PaneBinding {
                scope: scope.clone(),
                terminal_id: id.clone(),
                action_digest: authority.action_digest.clone(),
            };
            binding.validate()?;
            let auth_rem_ms = authority
                .expires_at
                .saturating_duration_since(now_inst)
                .as_millis() as u64;
            let ttl_ms = auth_rem_ms
                .min(session_ms)
                .min(OPERATION_TIMEOUT.as_millis() as u64 * 2)
                .max(1_000);
            let entry = Arc::new(Entry {
                binding,
                deadline,
                command_deadline,
                retention_ms: self.context.budgets.retention_ms,
                state: Mutex::new(State {
                    output_limit: limit,
                    tail,
                    ever_truncated: false,
                    started: false,
                    sgian_terminal_id: None,
                    pane_id: None,
                    exit: None,
                    exit_receipt_recorded: false,
                    released: false,
                    releasing: false,
                    cleanup_receipt: None,
                    failed: false,
                    budget: None,
                    retention_expired: false,
                    expired_budget: None,
                    output_reads: VecDeque::with_capacity(MAX_OUTPUT_READS_PER_SECOND),
                }),
                changed,
                stop_monitor: Notify::new(),
                abort: Mutex::new(None),
            });
            registry.creations += 1;
            registry.entries.insert(id.clone(), Arc::clone(&entry));
            (normalized, id, entry, session_ms, ttl_ms)
        };

        let remove_unstarted = || {
            if let Ok(mut reg) = self.registry.lock() {
                reg.entries.remove(&id);
            }
        };

        // Re-verify `cwd` containment immediately before signing to close any
        // symlink replacement window between proposal approval and spawn.
        let approved_cwd = normalized.cwd.clone().ok_or_else(|| {
            remove_unstarted();
            Error::InvalidRequest
        })?;
        let rechecked_cwd = match validate_contained_cwd(
            Path::new(&approved_cwd),
            &self.context.session_cwd_canon,
            &self.context.repo_canon,
        ) {
            Ok(canon) => canon,
            Err(err) => {
                remove_unstarted();
                return Err(err);
            }
        };
        if rechecked_cwd.to_str() != Some(approved_cwd.as_str()) {
            remove_unstarted();
            return Err(Error::InvalidRequest);
        }

        let key = match load_grant_key(&self.context.repo_canon) {
            Ok(k) => k,
            Err(_) => {
                remove_unstarted();
                return Err(Error::Unavailable);
            }
        };

        let issued_ms = now_epoch_ms();
        let mut argv = Vec::with_capacity(1 + normalized.args.len());
        argv.push(normalized.command.clone());
        argv.extend(normalized.args.clone());
        let grant_id = format!("g-{}", uuid::Uuid::new_v4().as_simple());
        let grant = ExecutionGrant {
            grant: grant_id.clone(),
            mission: self.context.mission_id.clone(),
            run: scope.run_id.clone(),
            repo: self.context.repo_canon.to_string_lossy().into_owned(),
            cwd: approved_cwd,
            argv,
            env_allow: GRANT_ENV_ALLOW.iter().map(|s| (*s).to_owned()).collect(),
            expires_ms: issued_ms.saturating_add(ttl_ms),
            issued_ms,
        };
        let (canonical_grant, signature) = match grant.sign(&key) {
            Ok(pair) => pair,
            Err(err) => {
                remove_unstarted();
                return Err(err);
            }
        };

        let bin = self.context.sgian_bin.clone();
        let repo = self.context.repo_canon.clone();
        let exec_outcome = tokio::time::timeout(
            OPERATION_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                let tmp = TempGrantFile::write(&canonical_grant)?;
                let ws = repo.display().to_string();
                let file_arg = tmp.path.display().to_string();
                let args = [
                    "ctl",
                    "--workspace",
                    &ws,
                    "--json",
                    "grant",
                    "exec",
                    "--file",
                    &file_arg,
                    "--signature",
                    &signature,
                ];
                run_ctl_detailed(
                    &bin,
                    &args,
                    crate::git_ops::process::Limits::for_control(OPERATION_TIMEOUT),
                )
                .map_err(|err| match err {
                    CtlError::TimedOut(_) => Error::CleanupUnconfirmed,
                    CtlError::Io(_) | CtlError::Exited { .. } => Error::Unavailable,
                })
            }),
        )
        .await;

        let stdout = match exec_outcome {
            Ok(Ok(Ok(bytes))) => bytes,
            Ok(Ok(Err(Error::Unavailable))) => {
                // Clean refusal before spawn (`grant.refused`); no Sgian handle exists.
                remove_unstarted();
                return Err(Error::Unavailable);
            }
            _ => {
                // Timed out or panicked: a pane might have started without our knowledge.
                remove_unstarted();
                self.fail();
                return Err(Error::CleanupUnconfirmed);
            }
        };

        let reply: Value = match serde_json::from_slice(&stdout) {
            Ok(v) => v,
            Err(_) => {
                remove_unstarted();
                self.fail();
                return Err(Error::CleanupUnconfirmed);
            }
        };
        let Some(sgian_tid) = reply["terminal_id"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
        else {
            remove_unstarted();
            self.fail();
            return Err(Error::CleanupUnconfirmed);
        };
        let Some(pane_id) = reply["pane_id"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_owned)
        else {
            let _ = release_sgian_handles_sync(
                &self.context.sgian_bin,
                &self.context.repo_canon,
                std::slice::from_ref(&sgian_tid),
            );
            remove_unstarted();
            self.fail();
            return Err(Error::CleanupUnconfirmed);
        };

        {
            let mut state = entry.state.lock().map_err(|_| Error::Unavailable)?;
            state.started = true;
            state.sgian_terminal_id = Some(sgian_tid.clone());
            state.pane_id = Some(pane_id.clone());
        }

        if self
            .receipts
            .record(json!({
                "terminalStarted": {
                    "terminalId": id,
                    "sgianTerminalId": sgian_tid,
                    "paneId": pane_id,
                    "grantId": grant_id,
                    "scope": scope,
                    "actionDigest": authority.action_digest,
                    "terminalBudgets": self.context.budgets,
                    "sessionRemainingMs": session_ms,
                }
            }))
            .is_err()
        {
            self.fail();
            return Err(Error::CleanupUnconfirmed);
        }

        let monitor_entry_arc = Arc::clone(&entry);
        let monitor_bin = self.context.sgian_bin.clone();
        let monitor_repo = self.context.repo_canon.clone();
        let monitor_deadline = self.session_deadline;
        let monitor_receipts = self.receipts.clone();
        let monitor_registry = Arc::clone(&self.registry);
        let failed_flag = Arc::clone(&self.failed);
        let failure_notify = Arc::clone(&self.failure);
        let task = tokio::spawn(async move {
            let outcome = tokio::time::timeout_at(
                monitor_entry_arc.deadline,
                monitor_entry(
                    Arc::clone(&monitor_entry_arc),
                    monitor_bin,
                    monitor_repo,
                    sgian_tid,
                    monitor_deadline,
                    monitor_receipts,
                    monitor_registry,
                ),
            )
            .await;
            if !matches!(outcome, Ok(Ok(()))) {
                if let Ok(mut state) = monitor_entry_arc.state.lock() {
                    state.failed = true;
                }
                failed_flag.store(true, Ordering::Release);
                failure_notify.notify_one();
                monitor_entry_arc.changed.send_replace(());
            }
        });
        if let Ok(mut abort) = entry.abort.lock() {
            *abort = Some(task.abort_handle());
        }

        self.check_scope(&scope)?;
        Ok(id)
    }

    async fn output(&self, scope: Scope, id: String) -> Result<OutputSnapshot, Error> {
        let entry = self.entry(&scope, &id)?;
        let (already_exited, sgian_id) = {
            let mut state = entry.state.lock().map_err(|_| Error::Unavailable)?;
            if state.failed {
                return Err(Error::CleanupUnconfirmed);
            }
            if state.retention_expired {
                return Err(Error::BudgetExceeded(
                    state.expired_budget.unwrap_or(Budget::Retention),
                ));
            }
            let now = tokio::time::Instant::now();
            while state
                .output_reads
                .front()
                .is_some_and(|t| now.saturating_duration_since(*t) >= Duration::from_secs(1))
            {
                state.output_reads.pop_front();
            }
            if state.output_reads.len() >= MAX_OUTPUT_READS_PER_SECOND {
                return Err(Error::BudgetExceeded(Budget::OutputRate));
            }
            state.output_reads.push_back(now);
            if state.exit.is_some() {
                return Ok(state.current_snapshot());
            }
            let sgian_id = state
                .sgian_terminal_id
                .clone()
                .ok_or(Error::InvalidHandle)?;
            (false, sgian_id)
        };
        let _ = already_exited;

        let bin = self.context.sgian_bin.clone();
        let repo = self.context.repo_canon.clone();
        let fetched =
            tokio::task::spawn_blocking(move || ctl_grant_output_sync(&bin, &repo, &sgian_id))
                .await
                .map_err(|_| Error::Unavailable)?;
        let (text, sgian_trunc, observed_exit) = match fetched {
            Ok(tuple) => tuple,
            Err(err) => {
                // Re-check whether a concurrent `release` invalidated the handle.
                if let Ok(state) = entry.state.lock() {
                    if state.releasing || state.released {
                        return Err(Error::InvalidHandle);
                    }
                }
                self.fail();
                return Err(err);
            }
        };

        let snapshot = {
            let mut state = entry.state.lock().map_err(|_| Error::Unavailable)?;
            if state.releasing || state.released {
                return Err(Error::InvalidHandle);
            }
            state.apply_sgian_output(&text, sgian_trunc)?;
            state.current_snapshot()
        };

        if let Some(status) = observed_exit {
            let _ = record_exit_once(&entry, &self.receipts, status.clone(), None);
            entry.changed.send_replace(());
            let mut snap = snapshot;
            snap.exit_status = Some(status);
            return Ok(snap);
        }

        Ok(snapshot)
    }

    async fn wait_for_exit(&self, scope: Scope, id: String) -> Result<ExitStatus, Error> {
        let entry = self.entry(&scope, &id)?;
        tokio::time::timeout_at(
            entry.deadline,
            wait_state(&entry, |s| {
                if s.retention_expired {
                    return Some(Err(Error::BudgetExceeded(
                        s.expired_budget.unwrap_or(Budget::Retention),
                    )));
                }
                s.exit.clone().map(|exit| {
                    s.budget
                        .map_or(Ok(exit), |budget| Err(Error::BudgetExceeded(budget)))
                })
            }),
        )
        .await
        .map_err(|_| {
            self.fail();
            Error::CleanupUnconfirmed
        })??
    }

    async fn kill(&self, scope: Scope, id: String) -> Result<CleanupReceipt, Error> {
        let entry = self.entry(&scope, &id)?;
        let sgian_id = {
            let state = entry.state.lock().map_err(|_| Error::Unavailable)?;
            if state.retention_expired {
                return Err(Error::BudgetExceeded(
                    state.expired_budget.unwrap_or(Budget::Retention),
                ));
            }
            if state.releasing || state.released {
                return Err(Error::InvalidHandle);
            }
            state
                .sgian_terminal_id
                .clone()
                .ok_or(Error::InvalidHandle)?
        };

        let bin = self.context.sgian_bin.clone();
        let repo = self.context.repo_canon.clone();
        let kill_res = tokio::time::timeout(
            OPERATION_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                let status = ctl_grant_kill_sync(&bin, &repo, &sgian_id)?;
                let out = ctl_grant_output_sync(&bin, &repo, &sgian_id);
                Ok::<_, Error>((status, out))
            }),
        )
        .await;

        let (exit_status, out_res) = match kill_res {
            Ok(Ok(Ok(pair))) => pair,
            _ => {
                if let Ok(mut state) = entry.state.lock() {
                    state.failed = true;
                }
                self.fail();
                entry.changed.send_replace(());
                return Err(Error::CleanupUnconfirmed);
            }
        };

        if let Ok((text, trunc, _)) = out_res {
            if let Ok(mut state) = entry.state.lock() {
                let _ = state.apply_sgian_output(&text, trunc);
            }
        }
        record_exit_once(&entry, &self.receipts, exit_status.clone(), None)?;
        entry.changed.send_replace(());

        Ok(CleanupReceipt {
            terminal_id: id,
            descendants_reaped: true,
            output_drained: true,
            released: false,
            exit_status,
        })
    }

    async fn release(&self, scope: Scope, id: String) -> Result<CleanupReceipt, Error> {
        self.check_scope(&scope)?;
        let (entry, sgian_id) = {
            let registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
            if let Some((_, Some(budget), _)) = registry.retired.get(&id) {
                return Err(Error::BudgetExceeded(*budget));
            }
            let entry = registry
                .entries
                .get(&id)
                .cloned()
                .ok_or(Error::InvalidHandle)?;
            let sgian_id = {
                let mut state = entry.state.lock().map_err(|_| Error::Unavailable)?;
                if state.retention_expired {
                    return Err(Error::BudgetExceeded(
                        state.expired_budget.unwrap_or(Budget::Retention),
                    ));
                }
                if state.releasing || state.released {
                    return Err(Error::InvalidHandle);
                }
                state.releasing = true;
                state
                    .sgian_terminal_id
                    .clone()
                    .ok_or(Error::InvalidHandle)?
            };
            (entry, sgian_id)
        };

        entry.stop_monitor.notify_waiters();

        let bin = self.context.sgian_bin.clone();
        let repo = self.context.repo_canon.clone();
        let rel_res = tokio::time::timeout(
            OPERATION_TIMEOUT,
            tokio::task::spawn_blocking(move || ctl_grant_release_sync(&bin, &repo, &sgian_id)),
        )
        .await;

        let exit_status = match rel_res {
            Ok(Ok(Ok(status))) => status,
            _ => {
                if let Ok(mut state) = entry.state.lock() {
                    state.failed = true;
                }
                self.fail();
                if let Ok(mut abort) = entry.abort.lock() {
                    if let Some(handle) = abort.take() {
                        handle.abort();
                    }
                }
                entry.changed.send_replace(());
                return Err(Error::CleanupUnconfirmed);
            }
        };

        record_exit_once(&entry, &self.receipts, exit_status.clone(), None)?;
        let receipt = CleanupReceipt {
            terminal_id: id.clone(),
            descendants_reaped: true,
            output_drained: true,
            released: true,
            exit_status: exit_status.clone(),
        };
        let (expired, released_obs) = {
            let mut state = entry.state.lock().map_err(|_| Error::Unavailable)?;
            state.released = true;
            state.sgian_terminal_id = None;
            state.cleanup_receipt = Some(receipt.clone());
            let obs = project_terminal_pane(
                &entry.binding,
                entry.binding.clone(),
                state.current_snapshot(),
                Some(receipt.clone()),
                true,
                true,
            )
            .ok();
            (state.expired_budget, obs)
        };
        if let Ok(mut abort) = entry.abort.lock() {
            if let Some(handle) = abort.take() {
                handle.abort();
            }
        }
        entry.changed.send_replace(());
        {
            let mut registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
            registry.entries.remove(&id);
            registry
                .retired
                .entry(id)
                .or_insert((exit_status, expired, released_obs));
        }
        Ok(receipt)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::backend::{AgentBackend, AgentEvent, PromptMode, SessionExit, SessionSpec};
    use crate::backend_acp::AcpBackend;
    use crate::sgian::{rotate_grant_key, GRANT_KEY_DIR, GRANT_KEY_FILE};
    use kranz_acp::terminal::{EnvironmentVariable, PaneLifecycleState};
    use std::os::unix::fs::PermissionsExt;

    fn write_exec_script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    fn make_proposal(provider: &Provider, id: &str, action: &Create) -> Proposal {
        let value = provider.action(action).unwrap();
        let options = vec![
            json!({"optionId":"execute-once","kind":"allow_once","name":"Execute this exact command"}),
            json!({"optionId":"deny-once","kind":"reject_once","name":"Deny"}),
        ];
        let now = chrono::Utc::now();
        let proposal = Proposal {
            id: id.into(),
            engine_session_id: provider.scope.engine_session_id.clone(),
            peer_session_id: provider.scope.peer_session_id.clone(),
            peer_request_id: json!(1),
            tool_call_id: format!("tc-{id}"),
            action_digest: digest(&value).unwrap(),
            options_digest: digest(&options).unwrap(),
            action: value,
            options,
            observed_at: now,
            deadline: now + chrono::Duration::seconds(60),
            prohibition: None,
        };
        proposal.validate().unwrap();
        proposal
    }

    #[test]
    fn grant_key_and_hmac_canonical_json_hygiene() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let repo_canon = repo.canonicalize().unwrap();

        let key_path = rotate_grant_key(&repo_canon).unwrap();
        let loaded = load_grant_key(&repo_canon).unwrap();
        let loaded_str = std::str::from_utf8(&loaded).unwrap();
        assert!(loaded_str.starts_with("sgk_"));
        assert_eq!(loaded.len(), 68);
        let mode = std::fs::symlink_metadata(&key_path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);

        // Scrubber redacts runtime `sgk_<64-hex>` keys under the `sgian-grant-key` rule.
        let synthetic_key = format!("sgk_{}", "a1".repeat(32));
        let scan = crate::scrub::scrub_with_findings(&format!("leaked {synthetic_key} ok"), "test");
        assert!(!scan.redacted.contains(&synthetic_key), "{}", scan.redacted);
        assert!(scan.redacted.contains("[REDACTED]"), "{}", scan.redacted);
        assert!(scan.findings.iter().any(|f| f.rule_id == "sgian-grant-key"));

        // Reject group/world-readable permissions (`0644`).
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = load_grant_key(&repo_canon).unwrap_err();
        assert!(err.contains("0600"), "{err}");
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();

        // Reject short key (< 16 bytes).
        std::fs::write(&key_path, b"short\n").unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_grant_key(&repo_canon).unwrap_err().contains("shorter"));

        // Reject symlinked `serve.grant.key`.
        std::fs::remove_file(&key_path).unwrap();
        let outside = tmp.path().join("outside.key");
        std::fs::write(&outside, &synthetic_key).unwrap();
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&outside, &key_path).unwrap();
        assert!(load_grant_key(&repo_canon).unwrap_err().contains("symlink"));

        // Reject symlinked `.kranz` directory.
        std::fs::remove_dir_all(repo_canon.join(GRANT_KEY_DIR)).unwrap();
        let fake_kranz = tmp.path().join("fake-kranz");
        std::fs::create_dir_all(&fake_kranz).unwrap();
        let fake_key = fake_kranz.join(GRANT_KEY_FILE);
        std::fs::write(&fake_key, &synthetic_key).unwrap();
        std::fs::set_permissions(&fake_key, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&fake_kranz, repo_canon.join(GRANT_KEY_DIR)).unwrap();
        assert!(load_grant_key(&repo_canon).unwrap_err().contains("symlink"));

        // RFC 4231 test case 2 & >64-byte key vector + canonical sorted JSON.
        assert_eq!(
            hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        assert_eq!(
            hmac_sha256_hex(
                &[0xaa; 131],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
        let canon = canonical_json(&json!({"z": 1, "a": {"d": true, "b": [3, 2]}}));
        assert_eq!(canon, r#"{"a":{"b":[3,2],"d":true},"z":1}"#);
    }

    #[tokio::test]
    async fn normalize_and_create_enforce_containment_and_consent_invariants() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let worktree = repo.join("wt");
        let outside = tmp.path().join("outside");
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();

        let calls_file = tmp.path().join("sgian.calls");
        let sgian_bin = write_exec_script(
            &bin_dir,
            "sgian",
            &format!(
                "printf '%s\\n' \"$*\" >> '{}'\nexit 1",
                calls_file.display()
            ),
        );

        // Relative helper path and repo-interior helper path must fail prepare.
        assert!(Provider::new(
            &Config::new("./sgian", &repo, "m-1", "run-1"),
            &worktree,
            "eng-1",
            "peer-1"
        )
        .is_err());
        let repo_bin = write_exec_script(&repo, "untrusted-sgian", "exit 0");
        assert!(Provider::new(
            &Config::new(&repo_bin, &repo, "m-1", "run-1"),
            &worktree,
            "eng-1",
            "peer-1"
        )
        .is_err());
        // Session cwd outside repo_root must fail prepare.
        assert!(Provider::new(
            &Config::new(&sgian_bin, &repo, "m-1", "run-1"),
            &outside,
            "eng-1",
            "peer-1"
        )
        .is_err());

        let cfg = Config::new(&sgian_bin, &repo, "m-1", "run-1");
        let provider = Provider::new(&cfg, &worktree, "eng-1", "peer-1").unwrap();
        let wt_canon = worktree.canonicalize().unwrap();

        // Non-empty `env` is refused so caller env values never enter Sgian.
        let with_env = Create {
            session_id: "peer-1".into(),
            command: "echo".into(),
            args: vec!["hi".into()],
            env: vec![EnvironmentVariable {
                name: "LD_PRELOAD".into(),
                value: "/tmp/x.so".into(),
            }],
            cwd: None,
            output_byte_limit: None,
            meta: None,
        };
        assert_eq!(provider.normalize(with_env), Err(Error::InvalidRequest));

        // Relative cwd, `..` traversal, and outside cwd are refused.
        for bad_cwd in [
            "sub".to_string(),
            format!("{}/..", wt_canon.display()),
            outside.canonicalize().unwrap().display().to_string(),
        ] {
            let act = Create {
                session_id: "peer-1".into(),
                command: "echo".into(),
                args: vec![],
                env: vec![],
                cwd: Some(bad_cwd),
                output_byte_limit: None,
                meta: None,
            };
            assert_eq!(provider.normalize(act), Err(Error::InvalidRequest));
        }

        // Symlinked directory inside session_cwd pointing outside is refused.
        let link_dir = wt_canon.join("escape-link");
        std::os::unix::fs::symlink(outside.canonicalize().unwrap(), &link_dir).unwrap();
        let sym_cwd = Create {
            session_id: "peer-1".into(),
            command: "echo".into(),
            args: vec![],
            env: vec![],
            cwd: Some(link_dir.display().to_string()),
            output_byte_limit: None,
            meta: None,
        };
        assert_eq!(provider.normalize(sym_cwd), Err(Error::InvalidRequest));

        // Relative `./script` and workspace-interior absolute executables are refused.
        let ws_exec = write_exec_script(&wt_canon, "evil.sh", "echo pwn");
        for bad_cmd in ["./evil.sh".to_string(), ws_exec.display().to_string()] {
            let act = Create {
                session_id: "peer-1".into(),
                command: bad_cmd,
                args: vec![],
                env: vec![],
                cwd: None,
                output_byte_limit: None,
                meta: None,
            };
            assert_eq!(provider.normalize(act), Err(Error::InvalidRequest));
        }

        // TOCTOU symlink swap between consent approval and `create()` is caught before spawning `sgian`.
        let sub = wt_canon.join("legit-sub");
        std::fs::create_dir_all(&sub).unwrap();
        let valid_action = Create {
            session_id: "peer-1".into(),
            command: "echo".into(),
            args: vec!["safe".into()],
            env: vec![],
            cwd: Some(sub.display().to_string()),
            output_byte_limit: None,
            meta: None,
        };
        provider.reserve("prop-toctou").unwrap();
        let proposal = make_proposal(&provider, "prop-toctou", &valid_action);
        let auth = provider.authorize(&valid_action, &proposal).unwrap();

        std::fs::remove_dir(&sub).unwrap();
        std::os::unix::fs::symlink(outside.canonicalize().unwrap(), &sub).unwrap();

        let res = provider
            .create(provider.scope.clone(), valid_action.clone(), auth)
            .await;
        assert_eq!(res, Err(Error::InvalidRequest));
        assert!(
            !calls_file.exists(),
            "sgian must not be invoked after TOCTOU symlink swap"
        );
    }

    #[tokio::test]
    async fn provider_lifecycle_kill_vs_release_utf8_truncation_and_rate_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let bin_dir = tmp.path().join("bin");
        let state_dir = tmp.path().join("fake-state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();
        let repo_canon = repo.canonicalize().unwrap();
        rotate_grant_key(&repo_canon).unwrap();

        // Stateful fake `sgian` that verifies HMAC-SHA256 via openssl on `grant exec`,
        // emits cumulative multi-byte UTF-8 output, and distinguishes `kill` from `release`.
        let sgian_bin = write_exec_script(
            &bin_dir,
            "sgian",
            &format!(
                r#"
STATE='{state}'
shift # ctl
shift # --workspace
WS="$1"; shift
shift # --json
shift # grant
SUB="$1"; shift
case "$SUB" in
  exec)
    shift # --file
    GFILE="$1"; shift
    shift # --signature
    SIG="$1"
    cp "$GFILE" "$STATE/last_grant.json"
    KEY=$(tr -d ' \t\r\n' < "$WS/.kranz/serve.grant.key")
    EXPECTED=$(openssl dgst -sha256 -mac HMAC -macopt "key:$KEY" "$GFILE" | awk '{{print $NF}}')
    if [ "$SIG" != "$EXPECTED" ]; then
      echo "bad signature: got $SIG expected $EXPECTED" >&2
      exit 1
    fi
    echo "running" > "$STATE/status"
    printf '{{"terminal_id":"sgt-1","pane_id":"pane-42"}}\n'
    ;;
  output)
    ST=$(cat "$STATE/status")
    CRABS="🦀🦀🦀🦀🦀🦀🦀🦀🦀🦀"
    if [ "$ST" = "running" ]; then
      printf '{{"terminal_id":"sgt-1","output":"%s","truncated":false,"exit_status":null}}\n' "$CRABS"
    else
      printf '{{"terminal_id":"sgt-1","output":"%s-fin","truncated":false,"exit_status":{{"exit_code":null,"signal":"SIGKILL"}}}}\n' "$CRABS"
    fi
    ;;
  wait)
    ST=$(cat "$STATE/status")
    if [ "$ST" = "running" ]; then
      sleep 0.05
      ST=$(cat "$STATE/status")
    fi
    if [ "$ST" = "running" ]; then
      printf '{{"terminal_id":"sgt-1","exited":false,"timed_out":true,"exit_status":null}}\n'
      exit 1
    else
      printf '{{"terminal_id":"sgt-1","exited":true,"timed_out":false,"exit_status":{{"exit_code":null,"signal":"SIGKILL"}}}}\n'
    fi
    ;;
  kill)
    echo "killed" > "$STATE/status"
    printf '{{"terminal_id":"sgt-1","descendants_reaped":true,"output_drained":true,"released":false,"exit_status":{{"exit_code":null,"signal":"SIGKILL"}}}}\n'
    ;;
  release)
    echo "released" > "$STATE/status"
    printf '{{"terminal_id":"sgt-1","descendants_reaped":true,"output_drained":true,"released":true,"exit_status":{{"exit_code":null,"signal":"SIGKILL"}}}}\n'
    ;;
esac
"#,
                state = state_dir.display()
            ),
        );

        let cfg = Config::new(&sgian_bin, &repo_canon, "mission-alpha", "run-alpha");
        let provider = Arc::new(Provider::new(&cfg, &repo_canon, "eng-1", "peer-1").unwrap());

        // Limit to 25 bytes (6 crabs = 24 bytes; 25 splits a 4-byte emoji and must round cleanly).
        let create_req = Create {
            session_id: "peer-1".into(),
            command: "echo".into(),
            args: vec!["hello".into()],
            env: vec![],
            cwd: None,
            output_byte_limit: Some(25),
            meta: None,
        };
        provider.reserve("p1").unwrap();
        let prop = make_proposal(&provider, "p1", &create_req);
        let auth = provider.authorize(&create_req, &prop).unwrap();
        let tid = provider
            .create(provider.scope.clone(), create_req, auth)
            .await
            .unwrap();

        // Inspect minted ExecutionGrant JSON written to `--file`.
        let grant_raw = std::fs::read_to_string(state_dir.join("last_grant.json")).unwrap();
        let grant_parsed: ExecutionGrant = serde_json::from_str(&grant_raw).unwrap();
        assert_eq!(grant_parsed.mission, "mission-alpha");
        assert_eq!(grant_parsed.run, "run-alpha");
        assert_eq!(grant_parsed.repo, repo_canon.display().to_string());
        assert_eq!(grant_parsed.cwd, repo_canon.display().to_string());
        assert_eq!(grant_parsed.argv, vec!["echo", "hello"]);
        assert_eq!(
            grant_parsed.env_allow,
            GRANT_ENV_ALLOW
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>()
        );
        assert!(grant_parsed.expires_ms > grant_parsed.issued_ms);

        // Active pane observation.
        let active_obs = provider.observe_pane(&provider.scope, &tid).unwrap();
        assert_eq!(active_obs.state, PaneLifecycleState::Active);

        // Wrong scope / generation is rejected with InvalidHandle.
        let mut wrong_scope = provider.scope.clone();
        wrong_scope.generation = "stale-gen".into();
        assert_eq!(
            provider.output(wrong_scope, tid.clone()).await,
            Err(Error::InvalidHandle)
        );

        // Cumulative UTF-8 output truncates cleanly on character boundary without duplicating on re-poll.
        let snap1 = provider
            .output(provider.scope.clone(), tid.clone())
            .await
            .unwrap();
        assert!(snap1.truncated);
        assert!(snap1.output.len() <= 25);
        assert_eq!(snap1.output, "🦀🦀🦀🦀🦀🦀");
        let snap2 = provider
            .output(provider.scope.clone(), tid.clone())
            .await
            .unwrap();
        assert_eq!(snap2.output, snap1.output);

        // 8 more rapid reads (total 10 in 1s) succeed; 11th hits OutputRate.
        for _ in 0..8 {
            provider
                .output(provider.scope.clone(), tid.clone())
                .await
                .unwrap();
        }
        assert_eq!(
            provider.output(provider.scope.clone(), tid.clone()).await,
            Err(Error::BudgetExceeded(Budget::OutputRate))
        );

        // `kill` terminates process (`released: false`) while keeping handle readable & waitable.
        let kill_receipt = provider
            .kill(provider.scope.clone(), tid.clone())
            .await
            .unwrap();
        assert!(!kill_receipt.released);
        assert!(kill_receipt.descendants_reaped);
        assert!(kill_receipt.output_drained);
        assert_eq!(kill_receipt.exit_status.signal.as_deref(), Some("SIGKILL"));

        let wait_status = provider
            .wait_for_exit(provider.scope.clone(), tid.clone())
            .await
            .unwrap();
        assert_eq!(wait_status.signal.as_deref(), Some("SIGKILL"));

        let exited_obs = provider.observe_pane(&provider.scope, &tid).unwrap();
        assert_eq!(exited_obs.state, PaneLifecycleState::Exited);
        assert!(exited_obs.snapshot.output.ends_with("-fin"));

        // `release` reclaims the handle (`released: true`); subsequent operations return InvalidHandle.
        let rel_receipt = provider
            .release(provider.scope.clone(), tid.clone())
            .await
            .unwrap();
        assert!(rel_receipt.released);
        assert_eq!(
            provider.output(provider.scope.clone(), tid.clone()).await,
            Err(Error::InvalidHandle)
        );
        assert_eq!(
            provider
                .wait_for_exit(provider.scope.clone(), tid.clone())
                .await,
            Err(Error::InvalidHandle)
        );
        assert_eq!(
            provider.release(provider.scope.clone(), tid.clone()).await,
            Err(Error::InvalidHandle)
        );
        let rel_obs = provider.observe_pane(&provider.scope, &tid).unwrap();
        assert_eq!(rel_obs.state, PaneLifecycleState::Released);
    }

    #[tokio::test]
    async fn provider_enforces_command_lifetime_retention_and_fail_closed_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let bin_dir = tmp.path().join("bin");
        let state_dir = tmp.path().join("budget-state");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&state_dir).unwrap();
        let repo_canon = repo.canonicalize().unwrap();
        rotate_grant_key(&repo_canon).unwrap();

        // 1. CommandLifetime + Retention auto-reclaim.
        let sgian_budget_bin = write_exec_script(
            &bin_dir,
            "sgian-budget",
            &format!(
                r#"
STATE='{state}'
shift; shift; shift; shift; shift
SUB="$1"; shift
case "$SUB" in
  exec)
    echo "running" > "$STATE/st"
    printf '{{"terminal_id":"sgt-b","pane_id":"pane-b"}}\n'
    ;;
  wait)
    if [ "$(cat "$STATE/st")" = "running" ]; then
      sleep 0.04
      printf '{{"terminal_id":"sgt-b","exited":false,"timed_out":true,"exit_status":null}}\n'
      exit 1
    fi
    printf '{{"terminal_id":"sgt-b","exited":true,"timed_out":false,"exit_status":{{"exit_code":null,"signal":"SIGKILL"}}}}\n'
    ;;
  output)
    printf '{{"terminal_id":"sgt-b","output":"tick\n","truncated":false,"exit_status":null}}\n'
    ;;
  kill)
    echo "killed" > "$STATE/st"
    printf '{{"terminal_id":"sgt-b","descendants_reaped":true,"output_drained":true,"released":false,"exit_status":{{"exit_code":null,"signal":"SIGKILL"}}}}\n'
    ;;
  release)
    echo "released" > "$STATE/released"
    printf '{{"terminal_id":"sgt-b","descendants_reaped":true,"output_drained":true,"released":true,"exit_status":{{"exit_code":null,"signal":"SIGKILL"}}}}\n'
    ;;
esac
"#,
                state = state_dir.display()
            ),
        );

        let cfg = Config::new(&sgian_budget_bin, &repo_canon, "m-b", "run-b")
            .with_budgets(Budgets::new(120, 150).unwrap());
        let provider = Arc::new(Provider::new(&cfg, &repo_canon, "eng-b", "peer-b").unwrap());
        let req = Create {
            session_id: "peer-b".into(),
            command: "sleep".into(),
            args: vec!["10".into()],
            env: vec![],
            cwd: None,
            output_byte_limit: None,
            meta: None,
        };
        provider.reserve("pb").unwrap();
        let prop = make_proposal(&provider, "pb", &req);
        let auth = provider.authorize(&req, &prop).unwrap();
        let tid = provider
            .create(provider.scope.clone(), req.clone(), auth)
            .await
            .unwrap();

        // Wait hits CommandLifetime budget automatically.
        let wait_err = provider
            .wait_for_exit(provider.scope.clone(), tid.clone())
            .await;
        assert_eq!(
            wait_err,
            Err(Error::BudgetExceeded(Budget::CommandLifetime))
        );

        // After retention_ms (150ms), monitor automatically calls `grant release`.
        tokio::time::sleep(Duration::from_millis(260)).await;
        assert!(state_dir.join("released").exists());
        assert_eq!(
            provider.output(provider.scope.clone(), tid).await,
            Err(Error::BudgetExceeded(Budget::Retention))
        );

        // 2. Fail-closed `CleanupUnconfirmed` when `descendants_reaped: false`.
        let sgian_unreaped_bin = write_exec_script(
            &bin_dir,
            "sgian-unreaped",
            r#"
shift; shift; shift; shift; shift
SUB="$1"
case "$SUB" in
  exec) printf '{"terminal_id":"sgt-u","pane_id":"pane-u"}\n' ;;
  wait) sleep 0.2; printf '{"terminal_id":"sgt-u","exited":false,"timed_out":true}\n'; exit 1 ;;
  release) printf '{"terminal_id":"sgt-u","descendants_reaped":false,"output_drained":true,"released":true,"exit_status":{"exit_code":0,"signal":null}}\n' ;;
  *) exit 1 ;;
esac
"#,
        );
        let cfg_u = Config::new(&sgian_unreaped_bin, &repo_canon, "m-u", "run-u");
        let provider_u = Provider::new(&cfg_u, &repo_canon, "eng-u", "peer-b").unwrap();
        provider_u.reserve("pu").unwrap();
        let prop_u = make_proposal(&provider_u, "pu", &req);
        let auth_u = provider_u.authorize(&req, &prop_u).unwrap();
        let tid_u = provider_u
            .create(provider_u.scope.clone(), req.clone(), auth_u)
            .await
            .unwrap();
        assert_eq!(
            provider_u.release(provider_u.scope.clone(), tid_u).await,
            Err(Error::CleanupUnconfirmed)
        );
        assert!(provider_u.failed.load(Ordering::Acquire));

        // 3. Clean daemon refusal on `grant exec` maps to `Unavailable` (not `CleanupUnconfirmed`).
        let sgian_refuse_bin =
            write_exec_script(&bin_dir, "sgian-refuse", "echo 'grant refused' >&2; exit 1");
        let cfg_r = Config::new(&sgian_refuse_bin, &repo_canon, "m-r", "run-r");
        let provider_r = Provider::new(&cfg_r, &repo_canon, "eng-r", "peer-b").unwrap();
        provider_r.reserve("pr").unwrap();
        let prop_r = make_proposal(&provider_r, "pr", &req);
        let auth_r = provider_r.authorize(&req, &prop_r).unwrap();
        assert_eq!(
            provider_r
                .create(provider_r.scope.clone(), req, auth_r)
                .await,
            Err(Error::Unavailable)
        );
        assert!(!provider_r.failed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn acp_backend_sgian_terminals_e2e_and_admission_guards() {
        use crate::sandbox::{ResolvedSandbox, SandboxBackend, SandboxInputs};
        use crate::types::SandboxEnforce;

        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let bin_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();
        let repo_canon = repo.canonicalize().unwrap();

        let sgian_bin = write_exec_script(
            &bin_dir,
            "sgian",
            r#"
shift; shift; shift; shift; shift
SUB="$1"
case "$SUB" in
  exec) printf '{"terminal_id":"sgt-e2e","pane_id":"pane-e2e"}\n' ;;
  output) printf '{"terminal_id":"sgt-e2e","output":"e2e-ok\\n","truncated":false,"exit_status":{"exit_code":0,"signal":null}}\n' ;;
  wait) printf '{"terminal_id":"sgt-e2e","exited":true,"timed_out":false,"exit_status":{"exit_code":0,"signal":null}}\n' ;;
  release) printf '{"terminal_id":"sgt-e2e","descendants_reaped":true,"output_drained":true,"released":true,"exit_status":{"exit_code":0,"signal":null}}\n' ;;
  *) exit 1 ;;
esac
"#,
        );

        let base_spec = |cwd: &Path, sid: &str| SessionSpec {
            cwd: cwd.to_path_buf(),
            prompt: PromptMode::SingleShot("run check".into()),
            append_system_prompt: None,
            model: "test-model".into(),
            effort: "high".into(),
            session_id: sid.into(),
            resume: None,
            permission_mode: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
            tools: vec![],
            writable: true,
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: HashMap::new(),
            sandbox: None,
            hook_status: None,
        };

        // Guard 1: `with_sgian_terminals` refuses any supplied `spec.sandbox` before spawning peer.
        let never_peer = write_exec_script(&bin_dir, "never-peer.sh", "touch spawned; exit 1");
        let mut sandboxed_spec = base_spec(&repo_canon, "s-sandboxed");
        sandboxed_spec.sandbox = Some(ResolvedSandbox {
            backend: SandboxBackend::Seatbelt,
            inputs: SandboxInputs {
                enforce: SandboxEnforce::FsNet,
                session_cwd: repo_canon.clone(),
                mission_dir: repo_canon.join(".kranz/missions/m-1"),
                tmpdir: tmp.path().join("scratch"),
                extra_write: vec![],
                egress: vec![],
                validator_read_deny_roots: vec![],
            },
            container: None,
        });
        let sgian_cfg = Config::new(&sgian_bin, &repo_canon, "m-e2e", "run-e2e");
        let refused = AcpBackend::new(&never_peer, vec![])
            .with_sgian_terminals(sgian_cfg.clone())
            .start(sandboxed_spec)
            .await;
        assert!(refused.is_err());
        assert!(!repo_canon.join("spawned").exists());

        // Guard 2: Without `with_sgian_terminals`, `terminal: true` is NOT advertised and `terminal/create` gets -32601.
        let unconfigured_peer = write_exec_script(
            &bin_dir,
            "unconfigured-peer.sh",
            r#"
IFS= read -r init_line
case "$init_line" in
  *'"terminal":true'*) exit 91 ;;
esac
id=$(printf '%s' "$init_line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{}}}\n' "$id"
IFS= read -r new_line
id=$(printf '%s' "$new_line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"peer-unconf"}}\n' "$id"
IFS= read -r prompt_line
pid=$(printf '%s' "$prompt_line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":500,"method":"terminal/create","params":{"sessionId":"peer-unconf","command":"echo"}}\n'
IFS= read -r resp_line
case "$resp_line" in
  *'-32601'*) ;;
  *) exit 92 ;;
esac
printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"peer-unconf","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"refused-as-expected"},"messageId":"m1"}}}\n'
printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$pid"
exit 0
"#,
        );
        let mut plain_session = AcpBackend::new(&unconfigured_peer, vec![])
            .start(base_spec(&repo_canon, "s-plain"))
            .await
            .unwrap();
        while plain_session.next_event().await.unwrap().is_some() {}
        assert_eq!(plain_session.exit_status(), Some(SessionExit::Completed));

        // 3. Full E2E session with `with_sgian_terminals`: initialize -> terminal/create (consent) -> output -> wait_for_exit -> release -> end_turn.
        let e2e_peer = write_exec_script(
            &bin_dir,
            "e2e-peer.sh",
            r#"
IFS= read -r init_line
case "$init_line" in
  *'"terminal":true'*) ;;
  *) echo "missing terminal capability: $init_line" >&2; exit 81 ;;
esac
id=$(printf '%s' "$init_line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{}}}\n' "$id"
IFS= read -r new_line
id=$(printf '%s' "$new_line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"peer-sgian-1"}}\n' "$id"
IFS= read -r prompt_line
pid=$(printf '%s' "$prompt_line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')

printf '{"jsonrpc":"2.0","id":10,"method":"terminal/create","params":{"sessionId":"peer-sgian-1","command":"echo","args":["e2e-ok"]}}\n'
IFS= read -r create_resp
tid=$(printf '%s' "$create_resp" | sed -n 's/.*"terminalId":"\([^"]*\)".*/\1/p')
if [ -z "$tid" ]; then
  echo "missing terminalId: $create_resp" >&2
  exit 82
fi

printf '{"jsonrpc":"2.0","id":11,"method":"terminal/output","params":{"sessionId":"peer-sgian-1","terminalId":"%s"}}\n' "$tid"
IFS= read -r out_resp
case "$out_resp" in
  *'e2e-ok'*) ;;
  *) echo "unexpected output: $out_resp" >&2; exit 83 ;;
esac

printf '{"jsonrpc":"2.0","id":12,"method":"terminal/wait_for_exit","params":{"sessionId":"peer-sgian-1","terminalId":"%s"}}\n' "$tid"
IFS= read -r wait_resp
case "$wait_resp" in
  *'"exitCode":0'*) ;;
  *) echo "unexpected wait: $wait_resp" >&2; exit 84 ;;
esac

printf '{"jsonrpc":"2.0","id":13,"method":"terminal/release","params":{"sessionId":"peer-sgian-1","terminalId":"%s"}}\n' "$tid"
IFS= read -r rel_resp

printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"peer-sgian-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"all-done"},"messageId":"m1"}}}\n'
printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$pid"
exit 0
"#,
        );

        let mut session = AcpBackend::new(&e2e_peer, vec![])
            .with_sgian_terminals(sgian_cfg)
            .start(base_spec(&repo_canon, "s-e2e"))
            .await
            .unwrap();

        let mut saw_consent = false;
        let mut saw_started = false;
        let mut saw_session_cleanup = false;
        while let Some(ev) = session.next_event().await.unwrap() {
            match &ev {
                AgentEvent::PermissionRequested { proposal, .. } => {
                    assert_eq!(proposal.action["provider"], "sgian");
                    saw_consent = true;
                    session
                        .permission_responder()
                        .unwrap()
                        .respond(proposal, true)
                        .unwrap();
                }
                AgentEvent::Other { raw } => {
                    if raw.get("terminalStarted").is_some() {
                        saw_started = true;
                    }
                    if raw["terminalSessionCleanup"]["confirmed"] == true {
                        saw_session_cleanup = true;
                    }
                }
                _ => {}
            }
        }
        assert_eq!(session.exit_status(), Some(SessionExit::Completed));
        assert!(saw_consent, "terminal/create must require one-call consent");
        assert!(saw_started, "terminalStarted receipt must be recorded");
        assert!(
            saw_session_cleanup,
            "terminalSessionCleanup receipt must be confirmed"
        );

        // 4. Abnormal `abort()` fails closed if Sgian `grant release` cannot confirm descendant reap.
        let bad_release_sgian = write_exec_script(
            &bin_dir,
            "sgian-bad-abort",
            r#"
shift; shift; shift; shift; shift
SUB="$1"
case "$SUB" in
  exec) printf '{"terminal_id":"sgt-ab","pane_id":"pane-ab"}\n' ;;
  wait) sleep 0.2; printf '{"terminal_id":"sgt-ab","exited":false,"timed_out":true}\n'; exit 1 ;;
  release) printf '{"terminal_id":"sgt-ab","descendants_reaped":false,"output_drained":true,"released":true,"exit_status":{"exit_code":0,"signal":null}}\n' ;;
  *) exit 1 ;;
esac
"#,
        );
        let hanging_peer = write_exec_script(
            &bin_dir,
            "hanging-peer.sh",
            r#"
IFS= read -r init_line
id=$(printf '%s' "$init_line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{}}}\n' "$id"
IFS= read -r new_line
id=$(printf '%s' "$new_line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"peer-hang"}}\n' "$id"
IFS= read -r _prompt_line
printf '{"jsonrpc":"2.0","id":20,"method":"terminal/create","params":{"sessionId":"peer-hang","command":"sleep","args":["30"]}}\n'
IFS= read -r _create_resp
printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"peer-hang","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"created"},"messageId":"m1"}}}\n'
sleep 30
"#,
        );
        let mut abort_session = AcpBackend::new(&hanging_peer, vec![])
            .with_sgian_terminals(Config::new(
                &bad_release_sgian,
                &repo_canon,
                "m-ab",
                "run-ab",
            ))
            .start(base_spec(&repo_canon, "s-abort"))
            .await
            .unwrap();
        while let Some(ev) = abort_session.next_event().await.unwrap() {
            if let AgentEvent::PermissionRequested { proposal, .. } = &ev {
                abort_session
                    .permission_responder()
                    .unwrap()
                    .respond(proposal, true)
                    .unwrap();
            }
            if matches!(&ev, AgentEvent::Text { text, .. } if text == "created") {
                break;
            }
        }
        let abort_err = abort_session.abort().await.unwrap_err();
        assert!(
            abort_err
                .to_string()
                .contains("sgian terminal cleanup unconfirmed"),
            "{abort_err}"
        );
    }

    /// Live cross-repo verification against a real Sgian binary (`SGIAN_LIVE_BIN`).
    /// Spawns an isolated Sgian daemon under a temporary `HOME`/`XDG_*` directory,
    /// drives `AcpBackend::with_sgian_terminals` end-to-end, verifies `grant.exec`
    /// and `lease.taken` (`holder: "kranz:<run>"`) in `sgian ctl ledger --verify`,
    /// and terminates the isolated daemon before returning.
    #[tokio::test]
    #[ignore = "requires SGIAN_LIVE_BIN pointing to compiled sgian binary"]
    async fn live_sgian_daemon_round_trip_verifies_grant_ledger_and_cleanup() {
        let sgian_bin = PathBuf::from(
            std::env::var("SGIAN_LIVE_BIN").expect("set SGIAN_LIVE_BIN to compiled sgian binary"),
        );
        // Use `/tmp` so macOS `Library/Application Support/Sgian/workspaces/<id>/daemon.sock`
        // stays well below macOS's 104-byte `SUN_LEN` Unix domain socket limit.
        let tmp = tempfile::Builder::new()
            .prefix("sg-")
            .tempdir_in("/tmp")
            .unwrap();
        let fake_home = tmp.path().join("h");
        let xdg_data = tmp.path().join("d");
        let xdg_runtime = tmp.path().join("r");
        let repo = tmp.path().join("repo");
        let peer_dir = tmp.path().join("bin");
        std::fs::create_dir_all(&fake_home).unwrap();
        std::fs::create_dir_all(&xdg_data).unwrap();
        std::fs::create_dir_all(&xdg_runtime).unwrap();
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&peer_dir).unwrap();
        std::fs::set_permissions(&xdg_runtime, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert!(std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap()
            .success());
        let repo_canon = repo.canonicalize().unwrap();

        // Point HOME / XDG_* at the isolated temp directory so Sgian starts a fresh
        // private daemon and never touches the operator's real Sgian data directory.
        std::env::set_var("HOME", &fake_home);
        std::env::set_var("XDG_DATA_HOME", &xdg_data);
        std::env::set_var("XDG_RUNTIME_DIR", &xdg_runtime);

        struct DaemonCleanup {
            home: PathBuf,
            xdg_data: PathBuf,
        }
        impl Drop for DaemonCleanup {
            fn drop(&mut self) {
                let bases = [
                    self.home.join("Library/Application Support/Sgian"),
                    self.xdg_data.join("Sgian"),
                    self.xdg_data.join("sgian"),
                ];
                for base in bases {
                    let direct = base.join("daemon.pid");
                    if let Ok(raw) = std::fs::read_to_string(&direct) {
                        if let Ok(pid) = raw.trim().parse::<i32>() {
                            if pid > 1 {
                                unsafe {
                                    libc::kill(pid, libc::SIGTERM);
                                }
                            }
                        }
                    }
                    if let Ok(entries) = std::fs::read_dir(base.join("workspaces")) {
                        for entry in entries.flatten() {
                            if let Ok(raw) =
                                std::fs::read_to_string(entry.path().join("daemon.pid"))
                            {
                                if let Ok(pid) = raw.trim().parse::<i32>() {
                                    if pid > 1 {
                                        unsafe {
                                            libc::kill(pid, libc::SIGTERM);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        let _cleanup = DaemonCleanup {
            home: fake_home.clone(),
            xdg_data: xdg_data.clone(),
        };

        // Start the isolated Sgian daemon and register `repo_canon` via a mutating `ctl new` call.
        let boot_status = std::process::Command::new(&sgian_bin)
            .args([
                "ctl",
                "--workspace",
                &repo_canon.display().to_string(),
                "--json",
                "new",
            ])
            .status()
            .unwrap();
        assert!(
            boot_status.success(),
            "sgian ctl new failed to bootstrap isolated daemon"
        );

        let live_peer = write_exec_script(
            &peer_dir,
            "live-peer.sh",
            r#"
IFS= read -r init_line
id=$(printf '%s' "$init_line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":1,"agentCapabilities":{}}}\n' "$id"
IFS= read -r new_line
id=$(printf '%s' "$new_line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"peer-live-1"}}\n' "$id"
IFS= read -r prompt_line
pid=$(printf '%s' "$prompt_line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')

printf '{"jsonrpc":"2.0","id":30,"method":"terminal/create","params":{"sessionId":"peer-live-1","command":"printf","args":["SGIAN_LIVE_ROUNDTRIP_OK\\n"]}}\n'
IFS= read -r create_resp
tid=$(printf '%s' "$create_resp" | sed -n 's/.*"terminalId":"\([^"]*\)".*/\1/p')
if [ -z "$tid" ]; then
  echo "create failed: $create_resp" >&2
  exit 71
fi

printf '{"jsonrpc":"2.0","id":31,"method":"terminal/wait_for_exit","params":{"sessionId":"peer-live-1","terminalId":"%s"}}\n' "$tid"
IFS= read -r wait_resp
case "$wait_resp" in
  *'"exitCode":0'*) ;;
  *) echo "wait failed: $wait_resp" >&2; exit 72 ;;
esac

printf '{"jsonrpc":"2.0","id":32,"method":"terminal/output","params":{"sessionId":"peer-live-1","terminalId":"%s"}}\n' "$tid"
IFS= read -r out_resp
case "$out_resp" in
  *'SGIAN_LIVE_ROUNDTRIP_OK'*) ;;
  *) echo "output missing marker: $out_resp" >&2; exit 73 ;;
esac

printf '{"jsonrpc":"2.0","id":33,"method":"terminal/release","params":{"sessionId":"peer-live-1","terminalId":"%s"}}\n' "$tid"
IFS= read -r _rel_resp

printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"peer-live-1","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"live-verified"},"messageId":"m1"}}}\n'
printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$pid"
exit 0
"#,
        );

        let spec = SessionSpec {
            cwd: repo_canon.clone(),
            prompt: PromptMode::SingleShot("verify live sgian grant".into()),
            append_system_prompt: None,
            model: "live-test".into(),
            effort: "high".into(),
            session_id: "eng-live-1".into(),
            resume: None,
            permission_mode: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
            tools: vec![],
            writable: true,
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: HashMap::new(),
            sandbox: None,
            hook_status: None,
        };
        let cfg = Config::new(&sgian_bin, &repo_canon, "m-live-42", "run-live-42");
        let mut session = AcpBackend::new(&live_peer, vec![])
            .with_sgian_terminals(cfg)
            .start(spec)
            .await
            .unwrap();

        let mut pane_id: Option<String> = None;
        while let Some(ev) = session.next_event().await.unwrap() {
            match &ev {
                AgentEvent::PermissionRequested { proposal, .. } => {
                    session
                        .permission_responder()
                        .unwrap()
                        .respond(proposal, true)
                        .unwrap();
                }
                AgentEvent::Other { raw } => {
                    if let Some(started) = raw.get("terminalStarted") {
                        pane_id = started["paneId"].as_str().map(str::to_owned);
                    }
                }
                _ => {}
            }
        }
        assert_eq!(session.exit_status(), Some(SessionExit::Completed));
        let pane_id = pane_id.expect("terminalStarted receipt must record Sgian paneId");

        // Verify Sgian's tamper-evident ledger chain (`--verify`) AND record payloads (`grant.exec`, `lease.taken`).
        let verify_out = std::process::Command::new(&sgian_bin)
            .args([
                "ctl",
                "--workspace",
                &repo_canon.display().to_string(),
                "--json",
                "ledger",
                &pane_id,
                "--verify",
            ])
            .output()
            .unwrap();
        assert!(
            verify_out.status.success(),
            "ledger verify failed: {}",
            String::from_utf8_lossy(&verify_out.stderr)
        );
        let verified: Value = serde_json::from_slice(&verify_out.stdout).unwrap();
        assert_eq!(verified["ok"], true, "{verified}");

        let ledger_out = std::process::Command::new(&sgian_bin)
            .args([
                "ctl",
                "--workspace",
                &repo_canon.display().to_string(),
                "--json",
                "ledger",
                &pane_id,
            ])
            .output()
            .unwrap();
        assert!(
            ledger_out.status.success(),
            "ledger read failed: {}",
            String::from_utf8_lossy(&ledger_out.stderr)
        );
        let records: Vec<Value> = serde_json::from_slice(&ledger_out.stdout).unwrap();
        let has_grant_exec = records.iter().any(|r| {
            r["type"] == "grant.exec"
                && r["payload"]["mission"] == "m-live-42"
                && r["payload"]["run"] == "run-live-42"
        });
        let has_lease_taken = records
            .iter()
            .any(|r| r["type"] == "lease.taken" && r["payload"]["holder"] == "kranz:run-live-42");
        assert!(has_grant_exec, "expected grant.exec in ledger: {records:?}");
        assert!(
            has_lease_taken,
            "expected lease.taken for kranz:run-live-42 in ledger: {records:?}"
        );
    }
}
