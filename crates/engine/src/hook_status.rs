//! Hook-derived status signals: an OPTIONAL, non-authoritative observability
//! lane for CLI backends that expose lifecycle hooks (ticket
//! `.kranz/tickets/agent-hooks-status-signals.md`; the consumer backend is
//! [`crate::backend_cursor`]).
//!
//! NOT [`crate::hooks`] — that module is the D-F webhook triggers the engine
//! EMITS to the operator — and NOT [`crate::hook_gates`], which projects
//! deterministic gates onto Claude Code's in-session `PreToolUse` hooks.
//! This module is the third, deliberately smallest hook lane: the backend
//! CLI's LIFECYCLE hooks (`sessionStart`, `stop`, `sessionEnd`, …) are
//! projected onto a kranz-managed command that reports a coarse signal —
//! "running", "needs input", "interrupted", "turn finished" — so the
//! dashboard and Slack can say SOMETHING honest about a session whose
//! output stream has gone quiet.
//!
//! # Why signals are never mission state
//!
//! The event fold is the only source of mission truth. A hook payload is
//! worker-reachable input (the per-session spec file lives in the
//! session-writable scratch root, so a hostile or confused session can POST
//! anything its token allows), and a lifecycle hook can simply never fire
//! (killed process, pre-hooks CLI, crashed endpoint). Neither property is
//! acceptable for state transitions, so this lane is structurally incapable
//! of touching it: signals land ONLY in an ephemeral derived projection
//! under the gitignored runtime dir (`.kranz/hook-status/`), keyed by
//! `(mission_id, run_id)`, and NO reducer-driving EventKind is added — the
//! module has no `EventKind` reference at all. A durable additive
//! observability event for hook receipts is an explicitly separate decision
//! (ticket's persistence constraint), not this lane.
//!
//! # The verified cursor hook surface (ground truth)
//!
//! Verified 2026-08-06 against the live docs (<https://cursor.com/docs/hooks>
//! and <https://cursor.com/docs/cli/changelog>; the local `agent --help`
//! prints no hook documentation):
//!
//! - Hooks are declared in `hooks.json` files; the USER-level file is
//!   `~/.cursor/hooks.json` (the project-level `<root>/.cursor/hooks.json`
//!   is a tracked-tree file this lane NEVER writes — see install hygiene
//!   below). Shape:
//!   `{ "version": 1, "hooks": { "<event>": [ { "command": "...", "timeout": 10 } ] } }`.
//! - Command hooks are spawned processes receiving the payload JSON on
//!   STDIN (argv delivery was replaced by stdin in the 2026-05-20 CLI
//!   changelog entry) and returning JSON on stdout; exit 0 = ok, exit 2 =
//!   block the action, any other code = hook failed and the action
//!   proceeds (fail-open). There is NO HTTP/URL hook type — delivery to
//!   kranz's endpoint is done by the installed `kranz hook-status` command.
//! - Documented agent lifecycle events include `sessionStart`, `stop`
//!   (`{status, loop_count}`), `sessionEnd` (`{reason: completed|aborted|
//!   error|window_close|user_close, error_message?}`), and
//!   `postToolUseFailure` (`{tool_name, failure_type: timeout|error|
//!   permission_denied, error_message}`). CLI hook support dates from the
//!   January 2026 CLI changelog entry; no docs page names a version floor,
//!   so a pre-hooks CLI is a degradation (hooks never fire — the lane says
//!   nothing), never an error.
//!
//! # The lane, end to end
//!
//! 1. Config opt-in (`hookStatus` in the mission config, off by default)
//!    plus a hook-capable backend ([`crate::types::BackendKind::
//!    supports_hook_status_signals`] — cursor only today) makes the runner
//!    mint a per-run capability token, REGISTER it in the projection store
//!    (the gitignored `.kranz/hook-status/<mission>/<run>.json` file), and
//!    seed the session spec ([`crate::backend::SessionSpec::hook_status`]).
//! 2. The cursor backend installs the session's hook config AT SPAWN: a
//!    `hooks.json` in the SESSION-PRIVATE scratch HOME (`<home>/.cursor/
//!    hooks.json`, the same per-session seeding channel as the account/
//!    config seed) pointing every mapped lifecycle event at
//!    `kranz hook-status --config <scratch>/hook-status/spec.json`, plus
//!    that spec file (endpoint, token, mission/run ids).
//! 3. The cursor CLI fires a lifecycle hook → `kranz hook-status` reads
//!    the payload on stdin (bounded), maps it to a [`HookSignal`]
//!    ([`map_cursor_hook`]), and POSTs `{token, missionId, runId, signal,
//!    detail}` to the configured loopback endpoint. Every failure exits 0:
//!    the lane is observational and must never block or fail a session.
//! 4. The server endpoint (`kranz serve`, loopback + capability-token
//!    gated — NOT the serve mutation token, which never crosses into a
//!    worker-readable file) validates the POST against the registration
//!    (constant-time token-hash compare, safe ids, staleness TTL, bounded
//!    body) and rewrites the run's projection entry. Untrusted-payload
//!    discipline: path traversal, stale ids, oversized bodies are all
//!    rejected, and the endpoint's ONLY write is the projection file.
//! 5. `GET /api/missions/:id/hook-status` re-reads the projection from
//!    disk per request (the server's every-read-is-a-reread idiom); the
//!    dashboard renders it beside the pending-decision chrome and Slack
//!    appends it to `/kranz status`, always labelled hook-derived and
//!    non-authoritative.
//!
//! # Install hygiene (non-negotiable, per the ticket)
//!
//! The lane writes hook config ONLY into the session-private scratch HOME
//! — never the primary checkout's tracked `.cursor/hooks.json`, and never
//! as a side effect of `kranz serve` (the server process writes nothing
//! but projection files under the gitignored runtime dir). A repo-local
//! hook install, if ever offered, must be an explicit operator action
//! producing a reviewable diff — no such action exists today.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Schema version of the per-session spec file ([`HookStatusSpec`]) and of
/// the projection entries ([`RunHookStatus`]) — both bump together.
pub const SPEC_VERSION: u32 = 1;

/// Bounds a wedged hook invocation (seconds), mirroring the hook-gate
/// lane's discipline: the CLI's default hook timeout is far too generous
/// for a local stdin→HTTP relay.
const HOOK_TIMEOUT_SECS: u32 = 10;

/// Max bytes of hook payload `kranz hook-status` reads from stdin. Real
/// cursor lifecycle payloads are a few KB; a boundless read would let a
/// hostile or broken CLI exhaust memory in the relay.
pub const STDIN_PAYLOAD_MAX_BYTES: usize = 64 * 1024;

/// Max bytes the endpoint accepts for one signal POST (the relay's body is
/// a handful of small fields; the server's route-level body limit is set
/// to this same bound).
pub const SIGNAL_BODY_MAX_BYTES: usize = 16 * 1024;

/// Max chars kept on a recorded signal's detail (the detail is derived
/// from a hook payload — worker-reachable input — so every persisted
/// string is scrubbed AND bounded, same discipline as `worker.message`).
const DETAIL_MAX_CHARS: usize = 200;

/// How long a registration accepts signals. A run outliving this TTL has
/// its late POSTs rejected as stale (`stale ids` per the ticket): the
/// registration file is runtime cruft from a run the engine has long
/// reaped, and an unbounded acceptance window would let a leaked token
/// rewrite history forever. 24h is generous for any real run.
pub const REGISTRATION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Cap on runs returned per mission by [`read_mission_signals`] (most
/// recently registered first) — bounds the read side against a flooded
/// registrations dir.
const READ_CAP: usize = 64;

// ---------------------------------------------------------------------------
// Signal vocabulary (the projection's whole state space)
// ---------------------------------------------------------------------------

/// One coarse lifecycle signal. This is the COMPLETE vocabulary the lane
/// can express — deliberately much smaller than mission status, so no hook
/// payload can ever spell a state transition (`FeatureFailed`, `Blocked`,
/// `Complete`, grant mutations): the mapping is an enum, not a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HookSignal {
    /// The session is alive and working (`sessionStart`).
    Running,
    /// The session appears to want operator attention (cursor: a headless
    /// tool call was refused by the permission posture —
    /// `postToolUseFailure` with `failure_type == "permission_denied"`).
    NeedsInput,
    /// The session ended abnormally (`sessionEnd` with
    /// `aborted`/`error`/`window_close`/`user_close`).
    Interrupted,
    /// The session finished its turn (`stop`, or `sessionEnd` with
    /// `completed`).
    TurnFinished,
}

impl HookSignal {
    /// The wire spelling (kebab-case, matching serde).
    pub fn as_str(self) -> &'static str {
        match self {
            HookSignal::Running => "running",
            HookSignal::NeedsInput => "needs-input",
            HookSignal::Interrupted => "interrupted",
            HookSignal::TurnFinished => "turn-finished",
        }
    }
}

/// Map one cursor lifecycle-hook payload (the JSON the CLI pipes to the
/// hook command's stdin) to a signal and an optional human-readable
/// detail. `None` = the event carries no status meaning for this lane and
/// is IGNORED (malformed or unmapped payloads are always ignorable — the
/// relay treats them as success and never retries).
///
/// The mapping is deliberately honest about what a headless
/// `--print` session can produce:
///
/// - `sessionStart` → [`HookSignal::Running`].
/// - `stop` → [`HookSignal::TurnFinished`]. In an INTERACTIVE cursor
///   session a stop means "idle at the prompt"; kranz's cursor backend is
///   always headless single-shot (`agent --print`, see
///   [`crate::backend_cursor`] module docs), where the agent stopping IS
///   the turn finishing.
/// - `sessionEnd` → `completed` maps to [`HookSignal::TurnFinished`];
///   `aborted` / `error` / `window_close` / `user_close` map to
///   [`HookSignal::Interrupted`] with the reason (and any `error_message`)
///   as detail. An absent reason is unmapped — never guessed at.
/// - `postToolUseFailure` with `failure_type == "permission_denied"` →
///   [`HookSignal::NeedsInput`]: a headless session wanted a capability
///   its posture refused, which is exactly the "a human should look"
///   signal this lane exists to surface. Other failure types
///   (`timeout`/`error`) are ordinary tool noise, not status.
pub fn map_cursor_hook(payload: &Value) -> Option<(HookSignal, Option<String>)> {
    let event = payload.get("hook_event_name")?.as_str()?;
    match event {
        "sessionStart" => Some((HookSignal::Running, None)),
        "stop" => Some((HookSignal::TurnFinished, None)),
        "sessionEnd" => {
            let reason = payload.get("reason").and_then(Value::as_str)?;
            let detail = match payload.get("error_message").and_then(Value::as_str) {
                Some(message) if !message.is_empty() => {
                    Some(format!("session ended: {reason} ({message})"))
                }
                _ => Some(format!("session ended: {reason}")),
            };
            match reason {
                "completed" => Some((HookSignal::TurnFinished, detail)),
                "aborted" | "error" | "window_close" | "user_close" => {
                    Some((HookSignal::Interrupted, detail))
                }
                _ => None,
            }
        }
        "postToolUseFailure" => {
            let failure_type = payload.get("failure_type").and_then(Value::as_str)?;
            if failure_type != "permission_denied" {
                return None;
            }
            let tool = payload
                .get("tool_name")
                .and_then(Value::as_str)
                .unwrap_or("tool");
            Some((
                HookSignal::NeedsInput,
                Some(format!(
                    "{tool} was refused by the session's permission posture"
                )),
            ))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Per-session spec file (engine-written, the `kranz hook-status` relay reads)
// ---------------------------------------------------------------------------

/// Everything `kranz hook-status` needs to report one signal, written by
/// the backend at spawn time into the session-private scratch root. The
/// hook command line carries ONLY this file's path (mirroring the
/// hook-gate lane: no new env or credential channel — the session's
/// already-cleared env is the whole channel).
///
/// The `token` is a per-RUN capability that authorizes exactly one thing —
/// writing this run's projection entry — never the serve mutation token
/// (`serve.token` is mutation authority and must stay unreadable inside
/// sandboxes; a worker-readable file can only ever carry a token whose
/// forgery ceiling is lying about its own run's status).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookStatusSpec {
    /// [`SPEC_VERSION`] at write time.
    pub version: u32,
    /// The full signal POST URL (e.g. `http://127.0.0.1:4560/api/hook-status`).
    pub endpoint: String,
    /// The per-run capability token (registered server-side as a hash).
    pub token: String,
    pub mission_id: String,
    pub run_id: String,
}

impl HookStatusSpec {
    /// Load the spec the hook command was pointed at. Any IO/parse failure
    /// is the relay's ignore-and-exit-0 branch.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// The runner-side seed carried on the session spec: what the backend
/// needs to install the lane. Backend-neutral (the runner mints it without
/// knowing which backend impl will consume it); backends without a
/// lifecycle-hook surface ignore it exactly like `settings_json`.
#[derive(Debug, Clone)]
pub struct HookStatusSeed {
    /// The full signal POST URL (from config `hookStatus.endpoint`).
    pub endpoint: String,
    /// The freshly minted per-run capability token.
    pub token: String,
    pub mission_id: String,
    pub run_id: String,
}

/// The per-session hook-status dir under the session-private scratch root
/// (the one tree every sandbox tier keeps worker-writable — see
/// [`crate::hook_gates`] module docs).
fn hook_status_session_dir(session_id: &str) -> PathBuf {
    crate::backend_claude::scratch_home_root(session_id).join("hook-status")
}

/// The backend-written spec file the hook command is pointed at.
pub fn spec_file(session_id: &str) -> PathBuf {
    hook_status_session_dir(session_id).join("spec.json")
}

// ---------------------------------------------------------------------------
// Install (backend side, at spawn)
// ---------------------------------------------------------------------------

/// The `~/.cursor/hooks.json` content wiring the mapped lifecycle events
/// to `command`. Pure so the exact wire shape is unit-testable without
/// spawning anything. Uses only the long-stable documented subset —
/// `version: 1`, per-event `[{command, timeout}]` handler lists (see
/// module docs for the verified surface). The mapped events are exactly
/// the ones [`map_cursor_hook`] consumes; installing MORE would only add
/// hook-spawn overhead for payloads the relay ignores.
pub fn cursor_hooks_json(command: &str) -> Value {
    let handler = json!({ "command": command, "timeout": HOOK_TIMEOUT_SECS });
    json!({
        "version": 1,
        "hooks": {
            "sessionStart": [handler],
            "stop": [handler],
            "sessionEnd": [handler],
            "postToolUseFailure": [handler],
        }
    })
}

/// Install the lane into one cursor session: write the per-session spec
/// file plus `<session_home>/.cursor/hooks.json`. `session_home` MUST be
/// the session-private HOME the child will actually receive (the backend
/// resolves it from the same seeding logic as its env channel) — this is
/// the install-hygiene invariant: hook config only ever lands in the
/// throwaway per-session HOME, never the primary checkout's tracked tree.
///
/// A pre-existing `hooks.json` in that HOME is overwritten only when it is
/// EXACTLY the file a previous install for this session wrote (same
/// session-private path, never operator content — the account/config seed
/// deliberately never copies `hooks.json`, so the only way the file exists
/// is this lane).
pub fn install_cursor_hook_status(
    session_home: &Path,
    seed: &HookStatusSeed,
    session_id: &str,
) -> std::io::Result<()> {
    let spec = HookStatusSpec {
        version: SPEC_VERSION,
        endpoint: seed.endpoint.clone(),
        token: seed.token.clone(),
        mission_id: seed.mission_id.clone(),
        run_id: seed.run_id.clone(),
    };
    let spec_path = spec_file(session_id);
    if let Some(parent) = spec_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let spec_text = serde_json::to_string_pretty(&spec).map_err(std::io::Error::other)?;
    std::fs::write(&spec_path, spec_text)?;

    let exe = std::env::current_exe()?;
    let command = format!(
        "{} hook-status --config {}",
        shell_quote(&exe),
        shell_quote(&spec_path)
    );
    let cursor_dir = session_home.join(".cursor");
    std::fs::create_dir_all(&cursor_dir)?;
    let hooks_text = serde_json::to_string_pretty(&cursor_hooks_json(&command))
        .map_err(std::io::Error::other)?;
    std::fs::write(cursor_dir.join("hooks.json"), hooks_text)
}

/// Single-quote a path for the shell-form hook command line (`sh -c`
/// semantics) — identical discipline to the hook-gate lane: the only safe
/// interpolation is none at all.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

// ---------------------------------------------------------------------------
// Projection store (gitignored runtime: .kranz/hook-status/)
// ---------------------------------------------------------------------------

/// The repo-level projection dir (gitignored runtime — see
/// [`crate::paths::KRANZ_GITIGNORE_RULES`], which carries `hook-status/`).
pub fn hook_status_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".kranz").join("hook-status")
}

/// One mission's projection dir (one file per run).
fn mission_dir(repo_root: &Path, mission_id: &str) -> PathBuf {
    hook_status_dir(repo_root).join(mission_id)
}

/// The run's projection file. Caller must have validated both ids with
/// [`crate::paths::MissionPaths::is_safe_id`] — they are joined into
/// filesystem paths.
fn run_file(repo_root: &Path, mission_id: &str, run_id: &str) -> PathBuf {
    mission_dir(repo_root, mission_id).join(format!("{run_id}.json"))
}

/// One run's projection entry: the registration (who may write) plus the
/// latest signal (what was last heard). The token lives here ONLY as a
/// SHA-256 hash — the cleartext token exists in exactly two places (the
/// runner's memory and the session's spec file), so a read of the
/// projection dir yields nothing replayable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunHookStatus {
    /// [`SPEC_VERSION`] at write time.
    pub version: u32,
    /// SHA-256 hex of the per-run capability token.
    pub token_hash: String,
    pub registered_at: DateTime<Utc>,
    /// The latest accepted signal; `None` = registered but never heard
    /// from (hooks not fired, relay down, or a pre-hooks CLI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<SignalRecord>,
}

/// One accepted signal occurrence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalRecord {
    pub signal: HookSignal,
    /// Scrubbed + bounded human-readable detail (worker-reachable input).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub received_at: DateTime<Utc>,
}

/// Mint a per-run capability token (uuid v4 simple hex, same idiom as the
/// serve tokens).
pub fn mint_token() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// SHA-256 hex of a cleartext token — the only form the projection stores.
fn token_hash(token: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(token.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Register one run's lane: create the projection entry carrying the
/// token hash. Called by the runner at spec time. Ids are validated before
/// any path is joined; a failure here degrades to NO lane (the caller
/// logs and leaves the spec seed unset) — never to a spawn error.
pub fn register(
    repo_root: &Path,
    mission_id: &str,
    run_id: &str,
    token: &str,
    now: DateTime<Utc>,
) -> std::io::Result<PathBuf> {
    if !crate::paths::MissionPaths::is_safe_id(mission_id)
        || !crate::paths::MissionPaths::is_safe_id(run_id)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "hook-status ids must be safe path components",
        ));
    }
    let entry = RunHookStatus {
        version: SPEC_VERSION,
        token_hash: token_hash(token),
        registered_at: now,
        signal: None,
    };
    let path = run_file(repo_root, mission_id, run_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(&entry).map_err(std::io::Error::other)?;
    std::fs::write(&path, text)?;
    Ok(path)
}

/// Why a signal POST was rejected. The endpoint maps these to statuses
/// that oracle nothing about neighboring runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordRejection {
    /// Mission/run id failed the safe-id check (path traversal).
    UnsafeId,
    /// No registration exists for this run (unknown, or cleaned up).
    UnknownRun,
    /// A registration exists but is unreadable/corrupt — treated as
    /// unknown (the run's lane is broken, not the request's problem).
    RegistrationUnreadable,
    /// The presented token does not match the registration's hash.
    TokenMismatch,
    /// The registration is older than [`REGISTRATION_TTL`].
    Stale,
}

/// Record one signal against its registration — the endpoint's ONLY write.
/// Every untrusted-input check lives here so the route handler stays a
/// thin shell: safe ids, registration present and readable, constant-time
/// token-hash compare, staleness TTL. The detail is scrubbed and bounded
/// before it persists. The rewrite is atomic (tmp + rename) so a reader
/// never observes a torn entry.
pub fn record_signal(
    repo_root: &Path,
    mission_id: &str,
    run_id: &str,
    presented_token: &str,
    signal: HookSignal,
    detail: Option<&str>,
    now: DateTime<Utc>,
) -> std::result::Result<RunHookStatus, RecordRejection> {
    if !crate::paths::MissionPaths::is_safe_id(mission_id)
        || !crate::paths::MissionPaths::is_safe_id(run_id)
    {
        return Err(RecordRejection::UnsafeId);
    }
    let path = run_file(repo_root, mission_id, run_id);
    let text = std::fs::read_to_string(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => RecordRejection::UnknownRun,
        _ => RecordRejection::RegistrationUnreadable,
    })?;
    let mut entry: RunHookStatus =
        serde_json::from_str(&text).map_err(|_| RecordRejection::RegistrationUnreadable)?;
    use subtle::ConstantTimeEq as _;
    let presented_hash = token_hash(presented_token);
    if !bool::from(presented_hash.as_bytes().ct_eq(entry.token_hash.as_bytes())) {
        return Err(RecordRejection::TokenMismatch);
    }
    let age = now
        .signed_duration_since(entry.registered_at)
        .to_std()
        .unwrap_or(Duration::ZERO);
    if age > REGISTRATION_TTL {
        return Err(RecordRejection::Stale);
    }
    entry.signal = Some(SignalRecord {
        signal,
        detail: detail
            .filter(|d| !d.trim().is_empty())
            .map(|d| crate::scrub::scrub_and_truncate(d, DETAIL_MAX_CHARS)),
        received_at: now,
    });
    let tmp = path.with_extension("json.tmp");
    let out = serde_json::to_string_pretty(&entry)
        .map_err(|_| RecordRejection::RegistrationUnreadable)?;
    std::fs::write(&tmp, out).map_err(|_| RecordRejection::RegistrationUnreadable)?;
    std::fs::rename(&tmp, &path).map_err(|_| RecordRejection::RegistrationUnreadable)?;
    Ok(entry)
}

/// The public (tokenless) view of one run's entry, served by
/// `GET /api/missions/:id/hook-status` and rendered by dashboard/Slack.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunHookStatusView {
    pub run_id: String,
    pub registered_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<SignalRecord>,
}

/// Read one mission's projection (most recently registered first, capped
/// at [`READ_CAP`]). Best-effort per entry: an unreadable/corrupt run file
/// is skipped rather than failing the whole read — the projection is
/// runtime state, and a partial honest answer beats none. Returns an empty
/// vec when the lane was never used for this mission.
pub fn read_mission_signals(repo_root: &Path, mission_id: &str) -> Vec<RunHookStatusView> {
    if !crate::paths::MissionPaths::is_safe_id(mission_id) {
        return Vec::new();
    }
    let dir = mission_dir(repo_root, mission_id);
    let read_dir = match std::fs::read_dir(&dir) {
        Ok(read_dir) => read_dir,
        Err(_) => return Vec::new(),
    };
    let mut entries: Vec<RunHookStatusView> = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(run_id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let parsed = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<RunHookStatus>(&text).ok());
        if let Some(status) = parsed {
            entries.push(RunHookStatusView {
                run_id: run_id.to_string(),
                registered_at: status.registered_at,
                signal: status.signal,
            });
        }
    }
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.registered_at));
    entries.truncate(READ_CAP);
    entries
}

/// The JSON body `kranz hook-status` POSTs (and the endpoint consumes).
/// Kept in the engine so the relay and the server share one wire shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalPost {
    pub token: String,
    pub mission_id: String,
    pub run_id: String,
    pub signal: HookSignal,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Build the POST body for one mapped payload (relay side). `None` when
/// the payload maps to no signal — the relay then has nothing to send.
pub fn signal_post_for(spec: &HookStatusSpec, payload: &Value) -> Option<SignalPost> {
    let (signal, detail) = map_cursor_hook(payload)?;
    Some(SignalPost {
        token: spec.token.clone(),
        mission_id: spec.mission_id.clone(),
        run_id: spec.run_id.clone(),
        signal,
        detail,
    })
}

/// The full signal POST path for a configured base endpoint: callers
/// configure the bare endpoint URL (e.g. `http://127.0.0.1:4560/api/
/// hook-status`) verbatim — no path munging here; config validation owns
/// its shape.
pub fn endpoint_is_loopback_http(endpoint: &str) -> bool {
    let Some(rest) = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
    else {
        return false;
    };
    let authority = rest.split('/').next().unwrap_or("");
    // Bracketed v6 literals carry their own ':'s — split them off first.
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        match bracketed.split_once(']') {
            Some((host, _)) => host,
            None => return false,
        }
    } else {
        authority.split(':').next().unwrap_or("")
    };
    host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// The engine-side unit of the lane's config gate: everything the runner
/// needs from `hookStatus` resolved to "install or not, and where to".
/// Defined here (not in `types.rs`) so `MissionConfig` stays POD — see
/// [`crate::types::HookStatusConfig`].
pub fn resolved_endpoint(config: &crate::types::HookStatusConfig) -> Option<&str> {
    if config.enabled && !config.endpoint.trim().is_empty() {
        Some(config.endpoint.trim())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed() -> HookStatusSeed {
        HookStatusSeed {
            endpoint: "http://127.0.0.1:4560/api/hook-status".to_string(),
            token: "tok-1".to_string(),
            mission_id: "m-1".to_string(),
            run_id: "r-1".to_string(),
        }
    }

    /// The mapping covers the four ticket-named signals and ignores
    /// everything else (malformed or unmapped payloads are never a signal).
    #[test]
    fn hook_status_signal_cursor_mapping_covers_the_vocabulary() {
        let session_start = json!({ "hook_event_name": "sessionStart" });
        assert_eq!(
            map_cursor_hook(&session_start),
            Some((HookSignal::Running, None))
        );

        let stop = json!({ "hook_event_name": "stop", "status": "completed", "loop_count": 0 });
        assert_eq!(
            map_cursor_hook(&stop),
            Some((HookSignal::TurnFinished, None))
        );

        let ended_ok = json!({ "hook_event_name": "sessionEnd", "reason": "completed" });
        assert_eq!(
            map_cursor_hook(&ended_ok),
            Some((
                HookSignal::TurnFinished,
                Some("session ended: completed".into())
            ))
        );
        let ended_err = json!({
            "hook_event_name": "sessionEnd",
            "reason": "error",
            "error_message": "model exploded",
        });
        assert_eq!(
            map_cursor_hook(&ended_err),
            Some((
                HookSignal::Interrupted,
                Some("session ended: error (model exploded)".into())
            ))
        );
        for reason in ["aborted", "window_close", "user_close"] {
            let ended = json!({ "hook_event_name": "sessionEnd", "reason": reason });
            assert!(
                matches!(map_cursor_hook(&ended), Some((HookSignal::Interrupted, _))),
                "{reason} must map to interrupted"
            );
        }

        let denied = json!({
            "hook_event_name": "postToolUseFailure",
            "tool_name": "Shell",
            "failure_type": "permission_denied",
        });
        assert_eq!(
            map_cursor_hook(&denied),
            Some((
                HookSignal::NeedsInput,
                Some("Shell was refused by the session's permission posture".into())
            ))
        );
    }

    /// Malformed payloads are ignored: no event name, an unmapped event, a
    /// reason-less sessionEnd, a non-denial tool failure, and a non-object
    /// body all map to None (the relay exits 0 without POSTing).
    #[test]
    fn hook_status_signal_malformed_payloads_map_to_nothing() {
        for payload in [
            json!({}),
            json!({ "hook_event_name": "beforeSubmitPrompt" }),
            json!({ "hook_event_name": "sessionEnd" }),
            json!({ "hook_event_name": "sessionEnd", "reason": "melted" }),
            json!({ "hook_event_name": "postToolUseFailure", "failure_type": "timeout" }),
            json!({ "hook_event_name": "postToolUseFailure", "failure_type": "error" }),
            json!("not an object"),
            json!(null),
        ] {
            assert_eq!(map_cursor_hook(&payload), None, "{payload}");
        }
    }

    /// The generated hooks.json has exactly the documented shape: version 1,
    /// one command handler per mapped lifecycle event, bounded timeout.
    #[test]
    fn hook_status_signal_hooks_json_matches_the_cursor_schema() {
        let hooks =
            cursor_hooks_json("'/usr/local/bin/kranz' hook-status --config '/tmp/s/spec.json'");
        assert_eq!(hooks["version"], 1);
        for event in ["sessionStart", "stop", "sessionEnd", "postToolUseFailure"] {
            let handlers = hooks["hooks"][event].as_array().expect(event);
            assert_eq!(handlers.len(), 1, "{event}");
            assert_eq!(
                handlers[0]["command"],
                "'/usr/local/bin/kranz' hook-status --config '/tmp/s/spec.json'"
            );
            assert!(handlers[0]["timeout"].as_u64().unwrap() <= 30, "{event}");
        }
        // Nothing beyond the mapped set is installed.
        let object = hooks["hooks"].as_object().unwrap();
        assert_eq!(object.len(), 4, "{object:?}");
    }

    /// Install writes the spec file + the session-HOME hooks.json — and
    /// nothing anywhere else (the install-hygiene invariant).
    #[test]
    fn hook_status_signal_install_writes_only_the_session_home() {
        let home = tempfile::tempdir().unwrap();
        let primary_checkout = tempfile::tempdir().unwrap();
        let session_id = format!("hook-status-install-{}", uuid::Uuid::new_v4());

        install_cursor_hook_status(home.path(), &seed(), &session_id).unwrap();

        let hooks_text =
            std::fs::read_to_string(home.path().join(".cursor").join("hooks.json")).unwrap();
        let hooks: Value = serde_json::from_str(&hooks_text).unwrap();
        let command = hooks["hooks"]["sessionStart"][0]["command"]
            .as_str()
            .unwrap();
        assert!(command.contains("hook-status"), "{command}");
        assert!(command.contains("--config"), "{command}");

        let spec = HookStatusSpec::load(&spec_file(&session_id)).unwrap();
        assert_eq!(spec.version, SPEC_VERSION);
        assert_eq!(spec.endpoint, "http://127.0.0.1:4560/api/hook-status");
        assert_eq!(spec.token, "tok-1");
        assert_eq!(spec.mission_id, "m-1");
        assert_eq!(spec.run_id, "r-1");

        // The primary checkout is byte-untouched — no `.cursor` dir, no
        // hooks.json, nothing (the ticket's non-negotiable install rule).
        assert_eq!(
            std::fs::read_dir(primary_checkout.path()).unwrap().count(),
            0,
            "install must never write into the primary tracked tree"
        );

        let _ = std::fs::remove_dir_all(crate::backend_claude::scratch_home_root(&session_id));
    }

    /// Register → record → read round-trip: the projection keeps only the
    /// token HASH, the latest signal wins, and the public view carries no
    /// hash.
    #[test]
    fn hook_status_signal_projection_round_trip() {
        let repo = tempfile::tempdir().unwrap();
        let now = Utc::now();
        register(repo.path(), "m-1", "r-1", "tok-1", now).unwrap();

        // The stored file carries no cleartext token.
        let raw = std::fs::read_to_string(run_file(repo.path(), "m-1", "r-1")).unwrap();
        assert!(!raw.contains("tok-1"), "{raw}");

        let entry = record_signal(
            repo.path(),
            "m-1",
            "r-1",
            "tok-1",
            HookSignal::Running,
            None,
            now,
        )
        .unwrap();
        assert!(entry.signal.is_some());
        let entry = record_signal(
            repo.path(),
            "m-1",
            "r-1",
            "tok-1",
            HookSignal::NeedsInput,
            Some("Shell was refused"),
            now,
        )
        .unwrap();
        assert_eq!(
            entry.signal.as_ref().map(|s| s.signal),
            Some(HookSignal::NeedsInput),
            "the latest signal wins"
        );

        let views = read_mission_signals(repo.path(), "m-1");
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].run_id, "r-1");
        assert_eq!(
            views[0].signal.as_ref().map(|s| s.signal),
            Some(HookSignal::NeedsInput)
        );
        // The public view serializes without the token hash.
        let public = serde_json::to_string(&views[0]).unwrap();
        assert!(!public.contains("tokenHash"), "{public}");
    }

    /// Untrusted-payload discipline at the store boundary: path traversal,
    /// unknown runs, wrong tokens, and stale registrations are all
    /// rejected, and none of them write anything.
    #[test]
    fn hook_status_signal_record_rejects_traversal_stale_and_wrong_token() {
        let repo = tempfile::tempdir().unwrap();
        let now = Utc::now();
        register(repo.path(), "m-1", "r-1", "tok-1", now).unwrap();

        // Path traversal in either id is refused before any path is joined.
        for (mission, run) in [("../m-1", "r-1"), ("m-1", "../r-1"), ("m/1", "r-1")] {
            assert_eq!(
                record_signal(
                    repo.path(),
                    mission,
                    run,
                    "tok-1",
                    HookSignal::Running,
                    None,
                    now
                ),
                Err(RecordRejection::UnsafeId)
            );
            assert_eq!(
                register(repo.path(), mission, run, "tok-1", now)
                    .unwrap_err()
                    .kind(),
                std::io::ErrorKind::InvalidInput
            );
        }

        // Unknown run.
        assert_eq!(
            record_signal(
                repo.path(),
                "m-1",
                "r-9",
                "tok-1",
                HookSignal::Running,
                None,
                now
            ),
            Err(RecordRejection::UnknownRun)
        );

        // Wrong token (constant-time compare; only the hash is stored).
        assert_eq!(
            record_signal(
                repo.path(),
                "m-1",
                "r-1",
                "tok-2",
                HookSignal::Running,
                None,
                now
            ),
            Err(RecordRejection::TokenMismatch)
        );

        // Stale registration: older than the TTL rejects new signals.
        let old = now - chrono::Duration::seconds(REGISTRATION_TTL.as_secs() as i64 + 60);
        register(repo.path(), "m-1", "r-old", "tok-1", old).unwrap();
        assert_eq!(
            record_signal(
                repo.path(),
                "m-1",
                "r-old",
                "tok-1",
                HookSignal::Running,
                None,
                now
            ),
            Err(RecordRejection::Stale)
        );

        // None of the rejections changed the honest entry.
        let views = read_mission_signals(repo.path(), "m-1");
        assert!(views.iter().all(|v| v.signal.is_none()), "{views:?}");
    }

    /// The detail is worker-reachable input: scrubbed and bounded before it
    /// persists.
    #[test]
    fn hook_status_signal_detail_is_scrubbed_and_bounded() {
        let repo = tempfile::tempdir().unwrap();
        let now = Utc::now();
        register(repo.path(), "m-1", "r-1", "tok-1", now).unwrap();
        let long = "x".repeat(DETAIL_MAX_CHARS * 3);
        let entry = record_signal(
            repo.path(),
            "m-1",
            "r-1",
            "tok-1",
            HookSignal::Interrupted,
            Some(&long),
            now,
        )
        .unwrap();
        let detail = entry.signal.unwrap().detail.unwrap();
        // House truncation semantics (`scrub::truncate_chars`): at most
        // DETAIL_MAX_CHARS of content plus the truncation marker.
        assert!(
            detail.chars().count() <= DETAIL_MAX_CHARS + "… [truncated]".chars().count(),
            "{detail}"
        );
        assert!(detail.ends_with(" [truncated]"), "{detail}");
    }

    /// `signal_post_for` shares one wire shape between the relay and the
    /// endpoint, and maps nothing for ignored payloads.
    #[test]
    fn hook_status_signal_post_body_shares_the_wire_shape() {
        let spec = HookStatusSpec {
            version: SPEC_VERSION,
            endpoint: "http://127.0.0.1:9/api/hook-status".to_string(),
            token: "tok-1".to_string(),
            mission_id: "m-1".to_string(),
            run_id: "r-1".to_string(),
        };
        let payload = json!({ "hook_event_name": "sessionStart" });
        let post = signal_post_for(&spec, &payload).unwrap();
        assert_eq!(post.token, "tok-1");
        assert_eq!(post.mission_id, "m-1");
        assert_eq!(post.run_id, "r-1");
        assert_eq!(post.signal, HookSignal::Running);
        let wire = serde_json::to_value(&post).unwrap();
        assert_eq!(wire["signal"], "running");
        assert!(wire.get("detail").is_none());

        let ignored = json!({ "hook_event_name": "preCompact" });
        assert!(signal_post_for(&spec, &ignored).is_none());
    }

    /// The endpoint gate: loopback http(s) only — the capability token
    /// rides this URL, so it must never point at a remote host.
    #[test]
    fn hook_status_signal_endpoint_gate_accepts_loopback_only() {
        for ok in [
            "http://127.0.0.1:4560/api/hook-status",
            "http://localhost:4560/api/hook-status",
            "http://[::1]:4560/api/hook-status",
            "https://127.0.0.1/api/hook-status",
        ] {
            assert!(endpoint_is_loopback_http(ok), "{ok}");
        }
        for bad in [
            "http://example.com/api/hook-status",
            "http://192.168.1.5/api/hook-status",
            "ftp://127.0.0.1/x",
            "127.0.0.1:4560/api/hook-status",
            "",
        ] {
            assert!(!endpoint_is_loopback_http(bad), "{bad}");
        }
    }

    /// The config resolution: disabled or endpoint-less means NO lane.
    #[test]
    fn hook_status_signal_config_resolution_is_off_by_default() {
        let off = crate::types::HookStatusConfig::default();
        assert!(resolved_endpoint(&off).is_none());
        let disabled_with_endpoint = crate::types::HookStatusConfig {
            enabled: false,
            endpoint: "http://127.0.0.1:4560/api/hook-status".to_string(),
        };
        assert!(resolved_endpoint(&disabled_with_endpoint).is_none());
        let on = crate::types::HookStatusConfig {
            enabled: true,
            endpoint: " http://127.0.0.1:4560/api/hook-status ".to_string(),
        };
        assert_eq!(
            resolved_endpoint(&on),
            Some("http://127.0.0.1:4560/api/hook-status")
        );
    }
}
