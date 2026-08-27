//! Cursor agent backend: drives the Cursor CLI headless
//! (`agent --print --output-format stream-json`).
//!
//! Ground truth is the decided route in `docs/scoping/cursor-cli-backend.md`
//! (decision revised 2026-07-09: `direct-parser`) and the committed wire
//! fixture `docs/scoping/cursor-probe-evidence/fixture-stream-json.jsonl`,
//! captured from a real write-capable run. The parser is built against THAT
//! shape first; the event mapping table in the scoping doc's implementation
//! brief is the authority for every arm below.
//!
//! This module is single-shot only: `--resume` exists on the CLI but was
//! never exercised by the probe, and there is no streaming-input mode, so
//! [`CursorSession::send_user_message`] and a `resume`d [`SessionSpec`] are
//! both rejected at the seam rather than translated into flags.
//!
//! Several `SessionSpec` fields are claude-isms with no cursor equivalent and
//! are deliberately ignored when building argv: `json_schema`,
//! `max_budget_usd`, `resume`, `permission_mode`, `allowed_tools` /
//! `disallowed_tools`, `tools`, `settings_json`, and `effort` — the CLI has
//! no `--effort` flag, and the `--model` bracket-override syntax
//! (`'model[effort=high]'`) is documented only for parameterized models and
//! was never probed, so effort is NOT munged into the model id.
//!
//! Permission posture (probe item 6, observed): read-only sessions map to
//! `--mode ask` (turn-level read-only, the validator role), writable sessions
//! to default mode + `--force` (writes/shell proceed unprompted, the worker
//! role; `--yolo` is only a documented alias of `--force`, never separately
//! live-tested, so `--force` is the emitted spelling). `--trust` rides every
//! session: headless `--print` otherwise prompts for workspace trust. The
//! CLI's own `--sandbox` flag is NEVER emitted — the probe observed it make
//! no difference to outbound network access or writes outside `--workspace`,
//! so it is not a kranz isolation boundary. The no-push/no-publish/
//! no-main-write invariants therefore hold exactly the way the scoping doc
//! prescribes: turn-level read-only modes for validators, throwaway
//! `--workspace` directories, scoped credentials, and (when requested) the
//! engine's external process sandbox — never this flag. Because the resolved
//! OS sandbox is not applied by this backend,
//! `BackendKind::Cursor::supports_sandbox_enforcement` is `false` and
//! `config::validate` fails closed on enforced-sandbox pairings.
//!
//! Auth posture (probe, verified): the CLI's login state does not survive a
//! relocated `$HOME` (`HOME=/tmp/x agent status` reports "Not logged in"),
//! and on macOS the credential itself is Keychain-backed (the sandboxed probe
//! crashed with `SecItemCopyMatching failed -50`) — no credential FILE exists
//! under `~/.cursor` to copy. The scratch-HOME seed therefore carries only
//! the small account-identity/CLI-config files ([`CURSOR_SEED_ENTRIES`]),
//! never transcripts or caches, and the one ambient var a cursor session may
//! authenticate with — `CURSOR_API_KEY`, the scoping doc's sanctioned
//! headless channel — is injected explicitly, never the ambient set. A
//! session whose seed+key is insufficient fails auth loudly
//! ("Authentication required", pre-billing), which the stream watcher turns
//! into an honest configuration-style failure rather than a retryable one
//! (probe item 5). One macOS addendum found by the first live mission
//! (m-a5a8fd, 2026-08-08): the CLI consults the login keychain at startup
//! EVEN with `CURSOR_API_KEY` set, and the keychain domain resolves through
//! `HOME`, so a relocated HOME without `Library/Keychains/login.keychain-db`
//! dies pre-auth with `security` exit 154. Both spawn branches therefore
//! seed an EMPTY login keychain (`ensure_session_login_keychain`) — never
//! a link to the operator's real keychain.
//!
//! Hook-status lane (ticket `agent-hooks-status-signals`,
//! [`crate::hook_status`]): when the runner seeds
//! [`SessionSpec::hook_status`] (mission config `hookStatus.enabled` AND
//! this hook-capable backend), [`CursorBackend::start`] installs the lane
//! into the session-private HOME BEFORE spawning: `<home>/.cursor/
//! hooks.json` (the CLI's documented user-level hook file — verified
//! 2026-08-06 against <https://cursor.com/docs/hooks>: `version: 1` with
//! per-event `[{command, timeout}]` handlers, payloads delivered on stdin,
//! exit 0 = ok / 2 = block / other = fail-open; there is NO HTTP hook
//! type, so delivery to kranz's endpoint is the installed
//! `kranz hook-status` relay) plus the per-session spec file the relay
//! reads. The install NEVER touches the workspace's tracked
//! `.cursor/hooks.json` — the project-level file is the operator's own,
//! and mutating it as a side effect of spawning would be exactly the
//! silent tracked-tree write the ticket forbids. Every install failure
//! degrades to NO lane (loud warning, ordinary session): hooks are
//! non-authoritative observability, and the lane disabled is the
//! byte-identical default. `SessionSpec::hook_status` is the one field
//! this backend consumes beyond argv/env; the claude-ism fields stay
//! ignored as documented above.

use crate::backend::{
    AgentBackend, AgentEvent, AgentSession, PromptMode, SessionExit, SessionSpec,
};
#[cfg(unix)]
use crate::backend_claude::kill_group;
#[cfg(windows)]
use crate::backend_claude::win_job;
use crate::cost;
use crate::error::{EngineError, Result};
use crate::stream_bounds::{drain_to_tail, BoundedLines, STDERR_TAIL_CAP};
use crate::types::TokenUsage;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::process::{Child, ChildStdout};
use tokio::task::JoinHandle;

/// Max characters kept in tool-use / tool-result summaries.
const SUMMARY_MAX_CHARS: usize = 200;
/// Max characters of captured stderr included in failure messages.
const STDERR_TAIL_CHARS: usize = 500;

/// The ambient var a cursor session may authenticate with (injected
/// explicitly, never via ambient inheritance). Confirmed by `agent --help`:
/// `--api-key <key>` "(can also use CURSOR_API_KEY env var)".
const CURSOR_AUTH_ENV: &str = "CURSOR_API_KEY";

/// The minimal `~/.cursor` state seeded into a session's scratch HOME:
/// `cli-config.json` carries the account identity (`authInfo`) and CLI
/// config, `agent-cli-state.json` the CLI's small state file. Both are a few
/// KB. The interactive-login CREDENTIAL is not here — on macOS it is
/// Keychain-backed (see module docs) — so this seed preserves account/config
/// context but cannot guarantee auth; `CURSOR_API_KEY` is the reliable
/// headless channel. Deliberately excluded: `chats/` (per-session
/// transcripts, hundreds of MB), `ai-tracking/`, `projects/`, `plugins/`,
/// `extensions/`, `prompt_history.json`, `statsig-cache.json` (unbounded or
/// per-session state).
const CURSOR_SEED_ENTRIES: &[&str] = &["cli-config.json", "agent-cli-state.json"];

/// Plain-text (non-JSON) stdout/stderr phrases the CLI emits when it rejects
/// a session BEFORE any billed turn starts (probe item 5): an invalid or
/// unentitled `--model` id exits 1 with `Cannot use this model: <id>.
/// Available models: ...`, and an unauthenticated `--print` fails with
/// `Authentication required`. Both are deterministic, user-readable,
/// pre-billing rejections — the session watcher surfaces them as
/// configuration-style failures (fix the model id / authenticate), never as
/// retryable transport errors.
const PRE_BILLING_FAILURE_PHRASES: &[&str] = &["cannot use this model", "authentication required"];

/// macOS: `agent` consults the login keychain at startup even when
/// `CURSOR_API_KEY` is set, and the keychain domain resolves through `HOME`
/// — so a relocated scratch HOME with no `Library/Keychains/login.keychain-db`
/// dies before auth with `Security command failed: ... code: 154` (verified
/// live 2026-08-08: scratch HOME + API key fails 154; the same plus an empty
/// keychain succeeds). Seed an EMPTY keychain so the startup probe has a
/// valid, secret-free domain. Never link or copy the operator's real login
/// keychain — that would hand the session every credential reachable in it,
/// defeating the scratch-HOME posture.
///
/// Two further live findings shape the seed (m-eee81f): an EMPTY-password
/// keychain cannot be unlocked programmatically, and a fresh keychain
/// defaults to a 300-second inactivity relock — long builds then relock it
/// mid-session and every credential write pops a desktop dialog the
/// operator cancels only to see again. So the seed creates the keychain
/// with a real passphrase, sets a session-SCALE auto-lock (never the 300s
/// default, never no-timeout), and unlocks at every spawn (unlock state
/// lives in securityd, so the engine-side unlock covers the subsequently
/// spawned CLI).
///
/// Non-interactivity invariant (14th-pass review, cargo-test hang): some
/// `security` subcommands fall back to INTERACTIVE auth — a GUI password
/// dialog at the operator — when they touch a LOCKED db without a
/// passphrase (`set-keychain-settings`, `show-keychain-info`); others never
/// prompt (`create-keychain -p`, `unlock-keychain -p`, `lock-keychain`).
/// Every call below is therefore either passphrase-carrying or ordered so
/// it only ever runs against a db this code path just unlocked.
///
/// Auto-lock restored on the seeded keychain: session-scale (the fresh-db
/// 300s default relocked mid-build into desktop prompts — m-eee81f), never
/// no-timeout; [`lock_session_login_keychain`] relocks at session end.
#[cfg(target_os = "macos")]
const SESSION_KEYCHAIN_LOCK_SECS: u32 = 8 * 60 * 60;

/// `securityd` is shared by every session owned by the OS account. Even
/// keychains at distinct explicit paths can intermittently reject overlapping
/// create/unlock/settings requests (observed on hosted macOS while the
/// keychain tests ran in parallel). Keep each setup or teardown transaction
/// contiguous; this does not serialize the agent sessions themselves.
#[cfg(target_os = "macos")]
static SESSION_KEYCHAIN_OPERATION_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The per-session keychain passphrase file: a dotfile beside the db it
/// guards, inside the session-private scratch HOME.
#[cfg(target_os = "macos")]
fn session_keychain_secret_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("Keychains")
        .join(".login.keychain-passphrase")
}

/// Run an argv `security` invocation pinned to the session HOME, returning
/// whether it exited 0. Only for subcommands that can never fall back to
/// interactive auth (see the invariant above [`SESSION_KEYCHAIN_LOCK_SECS`])
/// — currently just `lock-keychain` at session teardown; passphrase-carrying
/// work goes through [`security_script_in_session_home`].
///
/// Bounded by [`SECURITY_TIMEOUT`]: a locked keychain makes `security` park
/// on a GUI approval forever (observed live 2026-08-10, a 20-minute gate
/// hang), so every spawn goes through the one bounded helper and a timeout
/// surfaces as `Err(TimedOut)` — unavailable-not-authorized, never a hang.
#[cfg(target_os = "macos")]
fn security_in_session_home(home: &Path, args: &[&std::ffi::OsStr]) -> std::io::Result<bool> {
    let output = security_bounded(home, args, None)?;
    Ok(output.status.success())
}

/// Hard ceiling on any `security` invocation: a healthy subcommand answers in
/// well under a second; anything past the bound is a locked keychain waiting
/// on a GUI approval that will never come in a headless session.
#[cfg(target_os = "macos")]
const SECURITY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The one bounded `security` spawn (ticket security-cli-invocation-timeout):
/// every `security` call in the engine goes through here so a locked keychain
/// can never park a gate or a spawn. Polls `try_wait` against
/// [`SECURITY_TIMEOUT`], kills the child on expiry, and reports the timeout
/// as `Err(TimedOut)` naming the bound. `stdin_script`, when present, is fed
/// to the child on a pipe (the `security -i` batch form).
#[cfg(target_os = "macos")]
fn security_bounded(
    home: &Path,
    args: &[&std::ffi::OsStr],
    stdin_script: Option<&str>,
) -> std::io::Result<std::process::Output> {
    security_bounded_with_timeout(
        Path::new("security"),
        home,
        args,
        stdin_script,
        SECURITY_TIMEOUT,
    )
}

/// [`security_bounded`] with an explicit binary path and timeout — the unit
/// under test for the locked-keychain hang regression (a stub `security`
/// that sleeps forever must fail fast, never hang the caller). The production
/// wrapper pins the binary to `security` resolved through the pinned
/// `/usr/bin:/bin` PATH; only tests substitute a stub path.
#[cfg(target_os = "macos")]
fn security_bounded_with_timeout(
    binary: &Path,
    home: &Path,
    args: &[&std::ffi::OsStr],
    stdin_script: Option<&str>,
    timeout: std::time::Duration,
) -> std::io::Result<std::process::Output> {
    use std::io::Read as _;
    use std::io::Write as _;
    let mut cmd = std::process::Command::new(binary);
    cmd.args(args)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if stdin_script.is_some() {
        cmd.stdin(std::process::Stdio::piped());
    } else {
        cmd.stdin(std::process::Stdio::null());
    }
    if let Ok(user) = std::env::var("USER") {
        cmd.env("USER", user);
    }
    // HOME is pinned to the session home so any preference side effect
    // lands in the scratch tree, never in the operator's real keychain
    // search list.
    let mut child = cmd.spawn()?;
    if let Some(script) = stdin_script {
        if let Some(mut stdin) = child.stdin.take() {
            // A broken pipe means the process died before reading; the wait
            // below surfaces the real status.
            let _ = stdin.write_all(script.as_bytes());
        }
    }
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "security did not exit within {}s (killed; locked keychain?)",
                        timeout.as_secs()
                    ),
                ));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        }
    };
    // The process has exited, so both pipes are at EOF and drain immediately.
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_end(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_end(&mut stderr);
    }
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Run `security -i` with `script` fed on stdin: the one-shot commands have
/// no passphrase-from-stdin form, so interactive mode is the only way to
/// keep secrets out of argv (see the seed's doc above). stdout is dropped
/// (interactive mode may echo prompts); stderr is captured for the
/// failure warning — callers must redact any secret before logging it.
/// Bounded by [`SECURITY_TIMEOUT`] like every `security` spawn.
#[cfg(target_os = "macos")]
fn security_script_in_session_home(
    home: &Path,
    script: &str,
) -> std::io::Result<std::process::Output> {
    security_bounded(home, &[std::ffi::OsStr::new("-i")], Some(script))
}

/// Write the per-session keychain passphrase 0600 (mode forced even when
/// the file pre-exists — `mode()` applies only at creation), refusing a
/// planted symlink like the repo's other secret-adjacent writes.
#[cfg(target_os = "macos")]
fn write_session_keychain_secret(path: &Path, secret: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::os::unix::fs::PermissionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(secret.as_bytes())?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Seed/unlock the session's login keychain (see the block doc above
/// [`SESSION_KEYCHAIN_LOCK_SECS`] for the full rationale). Secret hygiene
/// (ticket keychain-passphrase-predictable-permanent-unlock — the seeded
/// store is NOT empty forever; Cursor writes credentials into it, so the v2
/// shortcuts stopped being free):
///
/// - The passphrase is a RANDOM per-session secret (uuid v4, the repo's
///   randomness idiom) — never derived from the session id, which appears
///   in paths and logs. It is persisted 0600 next to the db
///   ([`session_keychain_secret_path`]) so later spawns into the same HOME
///   re-unlock with the same secret, and it is never logged (the one
///   captured `security` stderr is redacted before it can reach a warning).
/// - argv hygiene: `security unlock-keychain` has no stdin/flag passphrase
///   alternative (without `-p` it prompts via getpass(3) on the controlling
///   tty, which a headless spawn does not have), so an argv `-p` would
///   expose the secret to any same-user `ps`. The whole
///   create/settings/unlock sequence is instead fed to `security -i`
///   (interactive mode) on a PIPE ([`security_script_in_session_home`]):
///   argv carries only `-i`, and the pipe contents are not visible to other
///   processes. Verified live 2026-08-09: a stdin-fed batch behaves
///   identically to the argv form (including quoted paths with spaces), a
///   failed command does not abort the batch, and the process exit status
///   is the LAST command's — so with `unlock-keychain` last, a non-zero
///   exit means the unlock failed. Residual exposure: the secret lives in
///   securityd's memory and in the 0600 file inside the session-private
///   HOME — both reachable only to the same user, which the scratch-HOME
///   threat model already accepts (the session itself runs with that HOME).
/// - The store must not stay open forever: the seed restores a bounded
///   auto-lock (`set-keychain-settings -lut
///   [`SESSION_KEYCHAIN_LOCK_SECS`]`) and [`lock_session_login_keychain`]
///   relocks it when the session ends.
/// - GUI-prompt hygiene (the non-interactivity invariant above
///   [`SESSION_KEYCHAIN_LOCK_SECS`]): `set-keychain-settings` on a LOCKED
///   db falls back to interactive auth — a desktop password dialog — so it
///   runs ONLY in batch B, after batch A's `unlock-keychain` reported
///   success and the db is known-unlocked. A failed unlock skips the
///   settings entirely: no call here can ever pop a prompt at the
///   operator, even on a respawn into a scratch HOME this module's own
///   teardown just relocked.
///
/// Returns true iff the seed left the session db UNLOCKED (batch A's
/// unlock exited 0). This is the ONLY trustworthy non-interactive witness
/// of lock state: for a `login.keychain-db` that securityd has previously
/// unlocked this session, `unlock-keychain -p <wrong>` can exit 0 anyway
/// (securityd credential caching — verified live 2026-08-09, and the probe
/// attempt itself RE-UNLOCKS the db), while `show-keychain-info` /
/// `set-keychain-settings` on a locked db either error 152 or HANG on a GUI
/// dialog, nondeterministically. Callers/tests must therefore never probe
/// lock state through `security`; they consume this return value instead.
#[cfg(target_os = "macos")]
fn ensure_session_login_keychain(home: &Path, session_id: &str) -> bool {
    let _operation = SESSION_KEYCHAIN_OPERATION_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let keychains = home.join("Library").join("Keychains");
    let db = keychains.join("login.keychain-db");
    let db_exists = db.exists();
    if !db_exists {
        if let Err(e) = std::fs::create_dir_all(&keychains) {
            tracing::warn!(
                error = %e,
                "cursor session keychain seed: cannot create Library/Keychains; the CLI may \
                 fail startup with a security error under the relocated HOME"
            );
            return false;
        }
    }
    let secret_path = session_keychain_secret_path(home);
    let stored = std::fs::read_to_string(&secret_path)
        .ok()
        .filter(|s| !s.is_empty());
    let passphrase = match (stored, db_exists) {
        // The stored secret wins: later spawns into the same HOME re-unlock
        // with the passphrase the db was created with.
        (Some(secret), _) => secret,
        // Pre-hardening seeds (acdc77b) derived the passphrase from the
        // session id and left no secret file; keep unlocking those homes so
        // a scratch HOME written before the upgrade never wedges.
        (None, true) => format!("kranz-scratch-{session_id}"),
        (None, false) => {
            let fresh = uuid::Uuid::new_v4().simple().to_string();
            if let Err(e) = write_session_keychain_secret(&secret_path, &fresh) {
                tracing::warn!(
                    error = %e,
                    "cursor session keychain seed: cannot persist the passphrase; the CLI may \
                     fail startup with a security error under the relocated HOME"
                );
                return false;
            }
            fresh
        }
    };
    // Batch A carries the passphrase (stdin script, never argv): create
    // (fresh db only), then unlock LAST — the batch's exit status is the
    // last command's, so success means the db is now unlocked. Paths are
    // quoted — verified handled by interactive mode.
    let mut script = String::new();
    if !db_exists {
        script.push_str(&format!(
            "create-keychain -p {passphrase} \"{}\"\n",
            db.display()
        ));
    }
    script.push_str(&format!(
        "unlock-keychain -p {passphrase} \"{}\"\n",
        db.display()
    ));
    match security_script_in_session_home(home, &script) {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            // The passphrase is never logged: redact it from the captured
            // stderr before it can reach a warning (a future `security`
            // build that echoes its input would otherwise leak it).
            let stderr = String::from_utf8_lossy(&output.stderr)
                .replace(&passphrase, "<redacted>")
                .trim()
                .to_string();
            tracing::warn!(
                status = %output.status,
                stderr = %stderr,
                "cursor session keychain seed: unlock failed; the CLI may fail startup with \
                 a security error under the relocated HOME"
            );
            // The db may still be LOCKED — applying settings now would fall
            // back to an interactive GUI prompt (the failure this ordering
            // exists to prevent). Skip batch B.
            return false;
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "cursor session keychain seed: security failed to spawn; the CLI may fail \
                 startup with a security error under the relocated HOME"
            );
            return false;
        }
    }
    // Batch B: the db is known-unlocked (batch A just succeeded), so
    // bounding the auto-lock cannot prompt.
    let settings = format!(
        "set-keychain-settings -lut {SESSION_KEYCHAIN_LOCK_SECS} \"{}\"\n",
        db.display()
    );
    match security_script_in_session_home(home, &settings) {
        Ok(output) if output.status.success() => {}
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr)
                .replace(&passphrase, "<redacted>")
                .trim()
                .to_string();
            tracing::warn!(
                status = %output.status,
                stderr = %stderr,
                "cursor session keychain seed: could not bound the auto-lock; the store keeps \
                 its current lock settings"
            );
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "cursor session keychain seed: could not bound the auto-lock; the store keeps \
                 its current lock settings"
            );
        }
    }
    // Batch A's unlock reported success: the db is left unlocked.
    true
}

/// Relock the seeded keychain at session end so the store is not left open
/// past the session's life — the auto-lock timeout is only the backstop.
/// `lock-keychain` takes only the db path and LOCKING never requires
/// authorization, so this cannot fall back to an interactive prompt even
/// on an already-locked db (it just exits non-zero); fire-and-forget at
/// teardown, the status is returned only so tests can assert the command
/// ran. Best-effort like the seed: a failure leaves the timeout.
#[cfg(target_os = "macos")]
fn lock_session_login_keychain(home: &Path) -> std::io::Result<bool> {
    let _operation = SESSION_KEYCHAIN_OPERATION_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let db = home
        .join("Library")
        .join("Keychains")
        .join("login.keychain-db");
    if !db.exists() {
        return Ok(false);
    }
    security_in_session_home(
        home,
        &[std::ffi::OsStr::new("lock-keychain"), db.as_os_str()],
    )
}

/// The cleared environment one `agent` session spawns with, mirroring
/// [`crate::backend_kimi`]'s seeding contract: a spec carrying a relocated
/// scratch `HOME` (worker relocation) is used verbatim; otherwise a fresh
/// per-session scratch HOME is seeded with [`CURSOR_SEED_ENTRIES`] so the
/// CLI's account-identity/config context survives. Seeding failure degrades
/// to an empty scratch home — the session then fails auth loudly rather than
/// silently inheriting the operator's real HOME. `CURSOR_API_KEY` is injected
/// explicitly when set (logged name-only).
fn cursor_child_env(spec: &SessionSpec) -> std::collections::HashMap<String, String> {
    if spec.env.contains_key("HOME") {
        // The verbatim branch carries no .cursor seed (the runner owns the
        // relocation), but the macOS keychain domain must still exist.
        #[cfg(target_os = "macos")]
        if let Some(home) = spec.env.get("HOME") {
            let _ = ensure_session_login_keychain(Path::new(home), &spec.session_id);
        }
        return crate::agent_env::agent_session_env(
            &spec.env,
            &spec.session_id,
            Some(CURSOR_AUTH_ENV),
        );
    }
    let real_home = std::env::var_os("HOME").map(PathBuf::from);
    let scratch_root = crate::backend_claude::scratch_home_root(&spec.session_id);
    match seed_cursor_scratch_home(&scratch_root, real_home.as_deref()) {
        Ok(home) => {
            #[cfg(target_os = "macos")]
            let _ = ensure_session_login_keychain(&home, &spec.session_id);
            tracing::info!(
                session_id = %spec.session_id,
                decision = "scratch-seeded",
                "session spec carried no relocated HOME; spawning into a seeded scratch \
                 HOME (.cursor minimal account/config set)"
            );
            crate::agent_env::session_env_with_home(
                &spec.env,
                &spec.session_id,
                Some(CURSOR_AUTH_ENV),
                &home,
            )
        }
        Err(e) => {
            tracing::warn!(
                session_id = %spec.session_id,
                error = %e,
                "cursor scratch HOME seeding failed; session spawns into an empty scratch \
                 HOME and will fail auth loudly if CURSOR_API_KEY is not injected"
            );
            crate::agent_env::agent_session_env(&spec.env, &spec.session_id, Some(CURSOR_AUTH_ENV))
        }
    }
}

/// Seed `<scratch_root>/home/.cursor` with [`CURSOR_SEED_ENTRIES`], copied
/// opaquely (bytes only, no parsing/logging of contents) from the real
/// home's `.cursor` when present; a missing source yields an
/// empty-but-present `.cursor`. Returns the home dir the child should get as
/// `HOME`.
fn seed_cursor_scratch_home(
    scratch_root: &Path,
    real_home: Option<&Path>,
) -> std::io::Result<PathBuf> {
    let home = scratch_root.join("home");
    let cursor_dir = home.join(".cursor");
    std::fs::create_dir_all(&cursor_dir)?;
    if let Some(real_home) = real_home {
        let source = real_home.join(".cursor");
        for entry in CURSOR_SEED_ENTRIES {
            let src = source.join(entry);
            let dst = cursor_dir.join(entry);
            if src.is_file() {
                std::fs::copy(&src, &dst)?;
            } else if src.is_dir() {
                copy_dir_recursive(&src, &dst)?;
            }
        }
    }
    Ok(home)
}

/// Opaque recursive copy (files only; symlinks and other special entries
/// are skipped rather than followed).
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &target)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// The HOME one session's child will actually receive, mirroring
/// [`cursor_child_env`]'s resolution exactly (both branches of
/// [`crate::agent_env::agent_session_env`] land on one of these two):
/// a spec-carried relocated HOME is used verbatim; otherwise the
/// per-session scratch home `<scratch_home_root>/home` — seeded by
/// [`seed_cursor_scratch_home`], or empty-but-present on seed failure.
/// The hook-status install resolves the same path so its
/// `<home>/.cursor/hooks.json` is always in the tree the child sees
/// (and never the primary checkout).
fn cursor_session_home(spec: &SessionSpec) -> PathBuf {
    if let Some(home) = spec.env.get("HOME") {
        return PathBuf::from(home);
    }
    crate::backend_claude::scratch_home_root(&spec.session_id).join("home")
}

/// Serializes the tests in this module that mutate the process-global env
/// vars consulted by [`discover_cursor_binary`] (`KRANZ_CURSOR_BIN`, `PATH`,
/// `HOME`), since `cargo test` runs tests in parallel threads within one
/// process (mirrors `KIMI_ENV_LOCK`; private because — unlike kimi — cursor's
/// env-mutating tests live in this one source file).
#[cfg(test)]
static CURSOR_ENV_LOCK: Mutex<()> = Mutex::new(());

// ---------------------------------------------------------------------------
// Binary discovery
// ---------------------------------------------------------------------------

/// Locate a working cursor `agent` binary.
///
/// Order: `KRANZ_CURSOR_BIN` env var → `configured` → `agent` on PATH →
/// well-known install locations, ending with the Cursor-specific
/// `~/.local/bin/agent` (per docs/scoping/cursor-cli-backend.md, this is
/// where the real install lived on the probe host). Each candidate is
/// validated by running it with `--version`; the first one that succeeds
/// wins. Errors list every attempt so the user can see what was tried.
///
/// `KRANZ_CURSOR_BIN`, when set and non-empty, is an *exclusive* override:
/// only that path is probed, and a failure is returned immediately rather
/// than falling through to PATH or the well-known fallback locations. Naming
/// the binary explicitly and having it not work is an error, not a reason to
/// search elsewhere.
pub fn discover_cursor_binary(configured: Option<&str>) -> Result<PathBuf> {
    if let Some(env_bin) = std::env::var_os("KRANZ_CURSOR_BIN") {
        if !env_bin.is_empty() {
            let candidate = PathBuf::from(env_bin);
            return match probe_version(&candidate) {
                Ok(_version) => Ok(candidate),
                Err(why) => Err(EngineError::Config(format!(
                    "KRANZ_CURSOR_BIN points at {} which did not work: {why}",
                    candidate.display()
                ))),
            };
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(configured) = configured {
        candidates.push(PathBuf::from(configured));
    }
    // Bare names resolve through PATH (std::process handles .cmd/.exe lookup
    // rules per-platform). The Cursor CLI's binary is `agent`, not `cursor`
    // (`cursor` is the desktop wrapper).
    candidates.push(PathBuf::from("agent"));
    #[cfg(windows)]
    {
        candidates.push(PathBuf::from("agent.cmd"));
        candidates.push(PathBuf::from("agent.exe"));
    }
    candidates.extend(fallback_candidates());

    // Dedupe, preserving priority order.
    let mut deduped: Vec<PathBuf> = Vec::new();
    for candidate in candidates {
        if !deduped.contains(&candidate) {
            deduped.push(candidate);
        }
    }

    let mut attempts: Vec<String> = Vec::new();
    for candidate in deduped {
        match probe_version(&candidate) {
            Ok(_version) => return Ok(candidate),
            Err(why) => attempts.push(format!("{} ({why})", candidate.display())),
        }
    }
    Err(EngineError::Config(format!(
        "no working cursor agent binary found; tried: {}. Install the Cursor \
         CLI or point kranz at it via the KRANZ_CURSOR_BIN environment variable.",
        attempts.join(", ")
    )))
}

/// Well-known install locations checked after PATH, ending with the
/// Cursor-specific install dir (per docs/scoping/cursor-cli-backend.md).
#[cfg(not(windows))]
fn fallback_candidates() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut out = Vec::new();
    if let Some(home) = &home {
        out.push(home.join(".npm-global").join("bin").join("agent"));
    }
    out.push(PathBuf::from("/opt/homebrew/bin/agent"));
    out.push(PathBuf::from("/usr/local/bin/agent"));
    if let Some(home) = &home {
        out.push(home.join(".local").join("bin").join("agent"));
    }
    out
}

/// Well-known install locations checked after PATH (Windows).
#[cfg(windows)]
fn fallback_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(profile) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        for dir in [
            profile.join("AppData").join("Roaming").join("npm"),
            profile.join(".npm-global").join("bin"),
            profile.join(".local").join("bin"),
        ] {
            for name in ["agent.cmd", "agent.exe", "agent"] {
                out.push(dir.join(name));
            }
        }
    }
    out
}

/// Deadline for a `--version` probe. Generous for a healthy CLI, but bounds
/// a hung shim on PATH so binary discovery (`kranz ready`, session spawn)
/// can never block forever on a candidate.
const VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Validate a candidate by running `<candidate> --version`, draining both
/// output pipes concurrently while enforcing [`VERSION_PROBE_TIMEOUT`].
fn probe_version(binary: &Path) -> std::result::Result<String, String> {
    crate::backend_probe::probe_version(binary, VERSION_PROBE_TIMEOUT)
}

// ---------------------------------------------------------------------------
// Argument construction
// ---------------------------------------------------------------------------

/// The prompt text `agent` actually receives: `append_system_prompt` (if any)
/// concatenated ahead of the prompt text — the CLI has no
/// `--append-system-prompt` flag, so the engine folds it into the single
/// positional prompt argument instead.
fn effective_prompt(spec: &SessionSpec) -> String {
    let prompt_text = match &spec.prompt {
        PromptMode::SingleShot(text) => text.as_str(),
        PromptMode::Streaming(text) => text.as_str(),
    };
    match &spec.append_system_prompt {
        Some(system) if !system.is_empty() => format!("{system}\n\n{prompt_text}"),
        _ => prompt_text.to_string(),
    }
}

/// Build the argv (excluding the binary itself) for one session.
///
/// Public so tests can assert the exact CLI wire format without spawning.
/// Deliberately ignores every claude-only `SessionSpec` field: `json_schema`,
/// `max_budget_usd`, `resume`, `permission_mode`, `allowed_tools` /
/// `disallowed_tools`, `tools`, `settings_json`, `effort` (see module docs).
/// `--workspace` pins the CLI's workspace to the session cwd (probe item 4,
/// exercised live); `--worktree` is deliberately never emitted (its behavior
/// is unobserved by design — kranz does its own worktree isolation), and
/// neither is `--sandbox` (observed to be no isolation boundary) or
/// `--stream-partial-output` (complete assistant events are what the parser
/// consumes; the fixture was captured without it).
pub fn build_args(spec: &SessionSpec) -> Vec<String> {
    let mut args = vec![
        "--print".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--trust".into(),
        "--workspace".into(),
        spec.cwd.display().to_string(),
        "--model".into(),
        spec.model.clone(),
    ];
    // The permission mapping (probe item 6): read-only sessions get the
    // turn-level read-only `--mode ask`; writable sessions get default mode +
    // `--force` (the observed unprompted-writes spelling; `--yolo` is only an
    // inferred alias).
    if spec.writable {
        args.push("--force".into());
    } else {
        args.push("--mode".into());
        args.push("ask".into());
    }
    args.push(effective_prompt(spec));
    args
}

// ---------------------------------------------------------------------------
// `agent --print --output-format stream-json` line parsing
// ---------------------------------------------------------------------------

/// Parse one stdout line into zero or more [`AgentEvent`]s. `model` is the
/// configured model id, used as the `Init` fallback (the wire's init event
/// carries a model DISPLAY string, e.g. "GPT-5.6 Luna 272K Low", which is
/// preferred when present) and as the pricing key for the terminal event's
/// client-side cost computation.
///
/// Unparseable lines become [`AgentEvent::Other`] with
/// `raw = {"unparsed": <line>}` so nothing is ever dropped from transcripts
/// — this is also what the pre-billing plain-text rejections
/// (`Cannot use this model`, `Authentication required`) arrive as.
pub fn parse_cursor_line(line: &str, model: &str) -> Vec<AgentEvent> {
    match serde_json::from_str::<Value>(line) {
        Ok(value) => parse_cursor_value(value, model),
        Err(_) => vec![AgentEvent::Other {
            raw: json!({ "unparsed": line }),
        }],
    }
}

/// Map one parsed `stream-json` value to events (see module docs /
/// docs/scoping/cursor-cli-backend.md's event-to-`AgentEvent` table).
/// Unrecognized `type`/`subtype` combinations route to [`AgentEvent::Other`]
/// rather than being guessed at.
pub fn parse_cursor_value(value: Value, model: &str) -> Vec<AgentEvent> {
    let line_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    match line_type {
        "system" if str_field(&value, "subtype") == "init" => vec![AgentEvent::Init {
            session_id: str_field(&value, "session_id"),
            model: value
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or(model)
                .to_string(),
            raw: value,
        }],
        // The `user` event echoes the prompt back; any other `system`
        // subtype is unobserved. Both are transcript-only.
        "user" | "system" => vec![AgentEvent::Other { raw: value }],
        "assistant" => {
            let text = assistant_text(&value);
            if text.is_empty() {
                vec![AgentEvent::Other { raw: value }]
            } else {
                vec![AgentEvent::Text { text, raw: value }]
            }
        }
        "tool_call" => match str_field(&value, "subtype").as_str() {
            "started" => vec![parse_tool_use(value)],
            "completed" => parse_tool_result(value),
            _ => vec![AgentEvent::Other { raw: value }],
        },
        "result" => vec![parse_terminal(value, model)],
        _ => vec![AgentEvent::Other { raw: value }],
    }
}

/// The joined text blocks of an `assistant` event's `message.content`
/// (the fixture carries exactly one `{"type":"text","text":...}` block;
/// multiple blocks concatenate so none are dropped).
fn assistant_text(value: &Value) -> String {
    let mut out = String::new();
    if let Some(blocks) = value.pointer("/message/content").and_then(Value::as_array) {
        for block in blocks {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                }
            }
        }
    }
    out
}

/// The tool kind key under `/tool_call` (`shellToolCall`, `editToolCall`,
/// `readToolCall` in the fixture; the discriminated union's member is the
/// one key ending in `ToolCall` — the object also carries bookkeeping keys
/// like `toolCallId`/`hookAdditionalContexts` that must not be mistaken for
/// it). Falls back to `"tool"` when the shape is unobserved.
fn tool_kind(value: &Value) -> String {
    value
        .get("tool_call")
        .and_then(Value::as_object)
        .and_then(|obj| obj.keys().find(|k| k.ends_with("ToolCall")).cloned())
        .unwrap_or_else(|| "tool".to_string())
}

/// A `tool_call/started` event maps to [`AgentEvent::ToolUse`]; the summary
/// is the shell command, the edited/read path, or the call's description,
/// whichever the tool's args carry first.
fn parse_tool_use(value: Value) -> AgentEvent {
    let kind = tool_kind(&value);
    let args = value.pointer(&format!("/tool_call/{kind}/args"));
    let summary = args
        .and_then(|args| {
            args.get("command")
                .or_else(|| args.get("path"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .pointer(&format!("/tool_call/{kind}/description"))
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    AgentEvent::ToolUse {
        tool: kind,
        summary: truncate_chars(summary, SUMMARY_MAX_CHARS),
        raw: value,
    }
}

/// A `tool_call/completed` event maps to [`AgentEvent::ToolResult`]. The
/// result union discriminates on `success`/`failure`; a `failure` carrying a
/// real `exitCode` is a normal failed command, NOT a kranz guardrail denial
/// — no in-band permission-denial frame was observed on this wire (kranz's
/// no-push invariant for cursor is enforced externally: read-only turn modes
/// and scoped credentials), so `denied` is always `false` here rather than
/// guessed from text. A completed event with no recognizable result member
/// routes to [`AgentEvent::Other`] (unobserved shape).
fn parse_tool_result(value: Value) -> Vec<AgentEvent> {
    let kind = tool_kind(&value);
    let result = value.pointer(&format!("/tool_call/{kind}/result"));
    let Some(result) = result else {
        return vec![AgentEvent::Other { raw: value }];
    };
    let summary = if let Some(success) = result.get("success") {
        success
            .get("stdout")
            .or_else(|| success.get("message"))
            .or_else(|| success.get("content"))
            .or_else(|| success.get("diffString"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| success.to_string())
    } else if let Some(failure) = result.get("failure") {
        failure
            .get("stderr")
            .or_else(|| failure.get("stdout"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| {
                failure
                    .get("exitCode")
                    .and_then(Value::as_i64)
                    .map(|code| format!("exit code {code}"))
            })
            .unwrap_or_else(|| failure.to_string())
    } else {
        return vec![AgentEvent::Other { raw: value }];
    };
    vec![AgentEvent::ToolResult {
        tool: Some(kind),
        denied: false,
        summary: truncate_chars(&summary, SUMMARY_MAX_CHARS),
        raw: value,
    }]
}

/// The terminal `result` event: full result text in `.result` (acceptance
/// item 1 — no cross-line stitching is needed on this wire; the final
/// `assistant` event repeats the same text), usage from the `.usage` object
/// (`inputTokens`/`outputTokens`/`cacheReadTokens`/`cacheWriteTokens`).
///
/// Absent stays absent, never fabricated: a result with NO `usage` object
/// records the zero default and `cost_usd: None` — the wire carries no
/// dollar cost (probe item 3), so cost is only ever computed client-side
/// from REAL usage tokens via [`cost::usage_cost_usd`].
fn parse_terminal(value: Value, model: &str) -> AgentEvent {
    let usage_present = value.get("usage").is_some();
    let usage_field = |key: &str| {
        value
            .pointer(&format!("/usage/{key}"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let usage = TokenUsage {
        input: usage_field("inputTokens"),
        output: usage_field("outputTokens"),
        cache_read: usage_field("cacheReadTokens"),
        cache_write: usage_field("cacheWriteTokens"),
    };
    let cost_usd = usage_present.then(|| cost::usage_cost_usd(&usage, model));
    let is_error = value
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || str_field(&value, "subtype") == "error";
    AgentEvent::Result {
        text: str_field(&value, "result"),
        is_error,
        usage,
        cost_usd,
        num_turns: Some(1),
        raw: value,
    }
}

/// Whether ONE line of CLI output names a known pre-billing rejection
/// ([`PRE_BILLING_FAILURE_PHRASES`], probe item 5) — deterministic,
/// user-readable, and never retried because no turn was billed.
///
/// The phrase must LEAD the (trimmed) line (14th-pass review — the match
/// was a loose substring over the whole text): the observed rejections are
/// the CLI's own plain-text lines (`Cannot use this model: <id>. Available
/// models: ...`, `Authentication required`), and anchoring keeps text that
/// merely QUOTES a rejection from tripping the detector — a torn
/// stream-json fragment riding the transcript as `Other { "unparsed": ... }`
/// (torn-line tolerance can land half of an assistant event there) or a
/// tool's mid-turn stderr line relaying a remote's "Authentication
/// required". A quoted phrase would relabel a failed turn as the
/// billing-free config error it wasn't.
fn names_pre_billing_failure(line: &str) -> bool {
    let lower = line.trim_start().to_ascii_lowercase();
    PRE_BILLING_FAILURE_PHRASES
        .iter()
        .any(|phrase| lower.starts_with(phrase))
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Keep at most `max` characters (not bytes — never splits a code point).
fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect()
    }
}

/// Last `max` characters of `text` (for stderr tails in error messages).
fn last_chars(text: &str, max: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = chars.len().saturating_sub(max);
    chars[start..].iter().collect()
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// The [`AgentBackend`] for `agent --print --output-format stream-json`:
/// single-shot with the `--mode ask` / `--force` permission posture selected
/// from the session role.
#[derive(Debug, Clone)]
pub struct CursorBackend {
    binary: PathBuf,
}

impl CursorBackend {
    /// Use an explicit binary path (no validation performed).
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        CursorBackend {
            binary: binary.into(),
        }
    }

    /// Discover the binary via [`discover_cursor_binary`].
    pub fn discover(configured: Option<&str>) -> Result<Self> {
        Ok(CursorBackend {
            binary: discover_cursor_binary(configured)?,
        })
    }

    /// The binary this backend spawns.
    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

#[async_trait::async_trait]
impl AgentBackend for CursorBackend {
    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>> {
        if spec.resume.is_some() {
            return Err(EngineError::Backend(
                "cursor backend is single-shot only; resume is unsupported".to_string(),
            ));
        }
        let model = spec.model.clone();
        let args = build_args(&spec);

        // agent-env-clear: CLEARED env from the minimal allowlist; the
        // scratch HOME is SEEDED with the minimal .cursor account/config
        // set (login state does not survive a relocated HOME), and the
        // one ambient var a cursor session may authenticate with is
        // injected explicitly, never the whole ambient set.
        let child_env = cursor_child_env(&spec);

        // Ticket agent-hooks-status-signals: install the OPTIONAL hook
        // lane into the session-private HOME (the seeded `.cursor` now
        // exists, and the tracked project `.cursor/hooks.json` is never
        // touched — module docs). The seed/env are unaffected; a failure
        // degrades to NO lane with a loud warning, never a spawn error.
        if let Some(seed) = &spec.hook_status {
            if let Err(e) = crate::hook_status::install_cursor_hook_status(
                &cursor_session_home(&spec),
                seed,
                &spec.session_id,
            ) {
                tracing::warn!(
                    session_id = %spec.session_id,
                    error = %e,
                    "hook-status install failed; the session spawns without the lane \
                     (mission state is unaffected — the lane is observational)"
                );
            }
        }

        let mut command = tokio::process::Command::new(&self.binary);
        command
            .args(&args)
            .current_dir(&spec.cwd)
            .env_clear()
            .envs(child_env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        // Unix: make the child the leader of a fresh process group so aborts
        // can kill the whole tree, mirroring `backend_claude::ClaudeBackend`.
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn().map_err(|e| {
            EngineError::Backend(format!("failed to spawn {}: {e}", self.binary.display()))
        })?;

        // Windows: kill-on-close Job Object, mirroring `backend_claude`.
        #[cfg(windows)]
        let job = match child.raw_handle() {
            Some(handle) => match win_job::JobHandle::create_and_assign(handle) {
                Ok(job) => Some(job),
                Err(e) => {
                    tracing::warn!(error = %e, "failed to create Job Object for cursor child; \
                        tree-kill on abort will be unavailable");
                    None
                }
            },
            None => None,
        };

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| EngineError::Backend("cursor child has no stdout pipe".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| EngineError::Backend("cursor child has no stderr pipe".to_string()))?;

        // Capture stderr concurrently so a chatty child never blocks on a
        // full pipe and failure messages can include the tail. The stream is
        // drained to EOF but only a bounded tail is retained — a noisy or
        // malicious CLI must not exhaust host memory (stream_bounds).
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_task = {
            let buf = Arc::clone(&stderr_buf);
            tokio::spawn(async move {
                let tail = drain_to_tail(stderr, STDERR_TAIL_CAP).await;
                *buf.lock().expect("stderr buffer lock") = tail;
            })
        };

        Ok(Box::new(CursorSession {
            session_id: spec.session_id.clone(),
            model,
            #[cfg(target_os = "macos")]
            session_home: cursor_session_home(&spec),
            child,
            #[cfg(windows)]
            job,
            lines: BoundedLines::new(stdout),
            stderr_buf,
            stderr_task: Some(stderr_task),
            queue: VecDeque::new(),
            saw_result: false,
            saw_success_result: false,
            pre_billing_failure: None,
            exit: None,
        }))
    }
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// A live `agent --print --output-format stream-json` session (the
/// [`AgentSession`] impl).
///
/// Single-shot only: [`send_user_message`](AgentSession::send_user_message)
/// always errors, and there is no streaming stdin to hold open.
pub struct CursorSession {
    session_id: String,
    model: String,
    /// macOS: the session-private HOME, remembered so the seeded login
    /// keychain under it can be relocked when the session ends
    /// ([`lock_session_login_keychain`]).
    #[cfg(target_os = "macos")]
    session_home: PathBuf,
    child: Child,
    #[cfg(windows)]
    job: Option<win_job::JobHandle>,
    lines: BoundedLines<ChildStdout>,
    stderr_buf: Arc<Mutex<String>>,
    stderr_task: Option<JoinHandle<()>>,
    /// Multi-block lines queue several events; popped one per `next_event`.
    queue: VecDeque<AgentEvent>,
    saw_result: bool,
    saw_success_result: bool,
    /// The first unparsed stdout line naming a known pre-billing rejection
    /// (`Cannot use this model`, `Authentication required` — probe item 5):
    /// recorded so EOF can word the failure as the configuration error it is
    /// rather than a retryable transport failure.
    pre_billing_failure: Option<String>,
    exit: Option<SessionExit>,
}

impl CursorSession {
    fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Init { session_id, .. } => {
                self.session_id = session_id.clone();
            }
            AgentEvent::Result { is_error, .. } => {
                self.saw_result = true;
                if !is_error {
                    self.saw_success_result = true;
                }
            }
            AgentEvent::Other { raw } if self.pre_billing_failure.is_none() => {
                if let Some(line) = raw.get("unparsed").and_then(Value::as_str) {
                    if names_pre_billing_failure(line) {
                        self.pre_billing_failure = Some(truncate_chars(line, STDERR_TAIL_CHARS));
                    }
                }
            }
            _ => {}
        }
    }

    /// Kill the child and reap it, best-effort; also joins the stderr capture
    /// task. Mirrors `backend_claude::ClaudeSession::kill_child` exactly:
    /// unix process-group SIGKILL (with a post-reap sweep for stragglers that
    /// raced a mid-fork), windows kill-on-close Job Object.
    async fn kill_child(&mut self) {
        #[cfg(unix)]
        {
            let pgid = self
                .child
                .id()
                .and_then(|pid| i32::try_from(pid).ok())
                .filter(|pid| *pid > 0);
            let group_killed = matches!(pgid, Some(pgid) if kill_group(pgid));
            if !group_killed {
                let _ = self.child.start_kill();
            }
            let _ = self.child.wait().await;
            if group_killed {
                if let Some(pgid) = pgid {
                    let _ = kill_group(pgid);
                }
            }
        }
        #[cfg(windows)]
        {
            match &self.job {
                Some(job) => job.kill(),
                None => {
                    let _ = self.child.start_kill();
                }
            }
            let _ = self.child.wait().await;
        }
        #[cfg(all(not(unix), not(windows)))]
        {
            let _ = self.child.start_kill();
            let _ = self.child.wait().await;
        }
        if let Some(task) = self.stderr_task.take() {
            let _ = task.await;
        }
        // macOS: the session is over — relock the seeded keychain so the
        // store is not left open past the session's life (the auto-lock
        // timeout is only the backstop).
        #[cfg(target_os = "macos")]
        let _ = lock_session_login_keychain(&self.session_home);
    }

    async fn finish_at_eof(&mut self) {
        let status = self.child.wait().await;
        if let Some(task) = self.stderr_task.take() {
            let _ = task.await;
        }
        // macOS: same relock as kill_child — EOF means the session ended.
        #[cfg(target_os = "macos")]
        let _ = lock_session_login_keychain(&self.session_home);
        // A known pre-billing rejection (probe item 5) is reported as the
        // configuration error it is — the caller fixes the model id or
        // authenticates; nothing was billed and there is nothing to retry.
        // The stdout capture wins; the stderr tail is the fallback for a CLI
        // that prints the rejection there instead (matched per line — the
        // tail is multi-line and the match is line-anchored). The guard
        // matters: a COMPLETED turn (exit 0 with a success result) is never
        // re-labeled — a tool's own stderr can legitimately contain one of
        // the phrases mid-turn (e.g. a remote's "Authentication required").
        let completed = matches!(status, Ok(ref s) if s.success()) && self.saw_result;
        let pre_billing = if completed {
            None
        } else {
            self.pre_billing_failure.clone().or_else(|| {
                let tail = self.stderr_tail();
                tail.lines().any(names_pre_billing_failure).then_some(tail)
            })
        };
        let exit = match (status, pre_billing) {
            (Ok(status), Some(detail)) => SessionExit::Failed(format!(
                "cursor rejected the session before any billed turn (exit {status}): {detail} — \
                 fix the configured model id or authenticate the cursor CLI; this is not a \
                 retryable failure"
            )),
            (Ok(status), None) if status.success() && self.saw_result => SessionExit::Completed,
            (Ok(status), None) => SessionExit::Failed(format!(
                "cursor exited with {status}{}; stderr tail: {}",
                if self.saw_result {
                    ""
                } else {
                    " without emitting a terminal event"
                },
                self.stderr_tail(),
            )),
            (Err(e), _) => SessionExit::Failed(format!(
                "failed to reap cursor process: {e}; stderr tail: {}",
                self.stderr_tail(),
            )),
        };
        self.exit = Some(exit);
    }

    fn stderr_tail(&self) -> String {
        let captured = self
            .stderr_buf
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        last_chars(captured.trim_end(), STDERR_TAIL_CHARS)
    }
}

#[async_trait::async_trait]
impl AgentSession for CursorSession {
    fn session_id(&self) -> String {
        self.session_id.clone()
    }

    async fn next_event(&mut self) -> Result<Option<AgentEvent>> {
        loop {
            if let Some(event) = self.queue.pop_front() {
                return Ok(Some(event));
            }
            if self.exit.is_some() {
                return Ok(None);
            }
            let line = match self.lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => {
                    self.finish_at_eof().await;
                    return Ok(None);
                }
                Err(e) => {
                    self.kill_child().await;
                    self.exit = Some(SessionExit::Failed(format!(
                        "error reading cursor stdout: {e}; stderr tail: {}",
                        self.stderr_tail(),
                    )));
                    return Ok(None);
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            let events = parse_cursor_line(&line, &self.model);
            for event in &events {
                self.observe(event);
            }
            self.queue.extend(events);
        }
    }

    async fn send_user_message(&mut self, _text: &str) -> Result<()> {
        Err(EngineError::Backend(
            "cursor backend is single-shot only; send_user_message is unsupported".to_string(),
        ))
    }

    async fn abort(&mut self) -> Result<()> {
        let already_exited = matches!(self.child.try_wait(), Ok(Some(_)));
        self.kill_child().await;
        if self.saw_success_result && already_exited {
            self.exit = Some(SessionExit::Completed);
        } else {
            self.exit = Some(SessionExit::Aborted);
        }
        Ok(())
    }

    fn exit_status(&self) -> Option<SessionExit> {
        self.exit.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_MODEL: &str = "gpt-5";

    fn fixture_lines() -> Vec<String> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("docs")
            .join("scoping")
            .join("cursor-probe-evidence")
            .join("fixture-stream-json.jsonl");
        std::fs::read_to_string(path)
            .expect("read fixture")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| line.to_string())
            .collect()
    }

    fn spec(cwd: &Path, writable: bool) -> SessionSpec {
        SessionSpec {
            cwd: cwd.to_path_buf(),
            prompt: PromptMode::SingleShot("do the thing".to_string()),
            append_system_prompt: None,
            model: TEST_MODEL.to_string(),
            effort: "high".to_string(),
            session_id: "sess-1".to_string(),
            resume: None,
            permission_mode: None,
            allowed_tools: vec![],
            disallowed_tools: vec![],
            tools: vec![],
            writable,
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: Default::default(),
            sandbox: None,
            hook_status: None,
        }
    }

    /// The scratch seed carries the minimal `.cursor` account/config set
    /// (login state does not survive a relocated HOME) and never the
    /// unbounded transcripts/caches.
    #[test]
    fn seed_cursor_scratch_home_copies_the_minimal_state_set() {
        let real_home = tempfile::tempdir().unwrap();
        let cursor = real_home.path().join(".cursor");
        std::fs::create_dir_all(cursor.join("chats")).unwrap();
        std::fs::write(cursor.join("cli-config.json"), "{}").unwrap();
        std::fs::write(cursor.join("agent-cli-state.json"), "{}").unwrap();
        std::fs::write(cursor.join("chats").join("big.jsonl"), "transcript").unwrap();
        std::fs::write(cursor.join("prompt_history.json"), "[]").unwrap();

        let scratch = tempfile::tempdir().unwrap();
        let home = seed_cursor_scratch_home(scratch.path(), Some(real_home.path())).unwrap();

        let seeded = home.join(".cursor");
        assert!(seeded.join("cli-config.json").is_file());
        assert!(seeded.join("agent-cli-state.json").is_file());
        assert!(
            !seeded.join("chats").exists(),
            "per-session transcripts are never seeded"
        );
        assert!(
            !seeded.join("prompt_history.json").exists(),
            "unbounded history is never seeded"
        );
    }

    /// A missing real `.cursor` yields an empty-but-present seed (the session
    /// then fails auth loudly rather than inheriting).
    #[test]
    fn seed_cursor_scratch_home_without_a_source_yields_an_empty_seed() {
        let real_home = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let home = seed_cursor_scratch_home(scratch.path(), Some(real_home.path())).unwrap();

        let seeded = home.join(".cursor");
        assert!(seeded.is_dir());
        assert_eq!(std::fs::read_dir(&seeded).unwrap().count(), 0);
    }

    /// A spec carrying no relocated HOME (the validator/orchestrator shape)
    /// spawns into a freshly seeded scratch HOME: the `.cursor` minimal set
    /// crosses, and the child env's HOME points at it.
    #[test]
    fn cursor_child_env_without_relocated_home_seeds_cursor_config() {
        let real_home = tempfile::tempdir().unwrap();
        let cursor = real_home.path().join(".cursor");
        std::fs::create_dir_all(&cursor).unwrap();
        std::fs::write(cursor.join("cli-config.json"), "{}").unwrap();
        std::fs::write(cursor.join("agent-cli-state.json"), "{}").unwrap();

        let _home_guard =
            crate::agent_env::EnvTestGuard::engage(&[("HOME", real_home.path().to_str().unwrap())]);
        let session_spec = spec(Path::new("."), false);

        let env = cursor_child_env(&session_spec);

        let home = env.get("HOME").expect("child env carries HOME");
        let seeded = Path::new(home).join(".cursor");
        assert!(
            seeded.join("cli-config.json").is_file(),
            "validator-path HOME must carry the seeded cli-config.json"
        );
        assert!(
            seeded.join("agent-cli-state.json").is_file(),
            "validator-path HOME must carry the seeded agent-cli-state.json"
        );
    }

    /// A `security` invocation against a locked keychain parks on a GUI
    /// approval forever (the 2026-08-10 gate hang). The bounded helper must
    /// kill the child at the deadline and fail with `TimedOut` naming the
    /// bound — never hang. Regression: a stub `security` that sleeps 30s is
    /// killed at the 1s test bound.
    #[cfg(target_os = "macos")]
    #[test]
    fn security_bounded_kills_a_locked_keychain_hang_at_the_deadline() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("hung-security");
        std::fs::write(&stub, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
        let home = tempfile::tempdir().unwrap();

        let start = std::time::Instant::now();
        let result = security_bounded_with_timeout(
            &stub,
            home.path(),
            &[std::ffi::OsStr::new("find-generic-password")],
            None,
            std::time::Duration::from_secs(1),
        );

        let error = result.expect_err("a hung security must be reported as timed out");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut, "{error}");
        assert!(
            error.to_string().contains("did not exit within 1s"),
            "the error names the bound: {error}"
        );
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "killed at the deadline, not after the stub's 30s sleep"
        );
    }

    /// Run `security` pinned to a session HOME, capturing status+output.
    /// Passing the passphrase via argv is fine in tests — the secret is a
    /// throwaway and the point under test is its value, not the transport.
    /// Callers must pass only subcommands that cannot fall back to
    /// interactive auth (see the non-interactivity invariant above
    /// [`SESSION_KEYCHAIN_LOCK_SECS`]): `show-keychain-info` only ever
    /// against a db the seed JUST reported unlocked.
    #[cfg(target_os = "macos")]
    fn security_output(home: &Path, args: &[&str]) -> std::process::Output {
        std::process::Command::new("security")
            .args(args)
            .env_clear()
            .env("HOME", home)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap()
    }

    /// macOS: a relocated HOME gets an EMPTY login keychain (the CLI consults
    /// the keychain domain at startup even with CURSOR_API_KEY set and dies
    /// with security exit 154 when none resolves through HOME), created with
    /// a random per-session passphrase stored 0600 beside the db and left
    /// unlocked for the session (auto-lock bounded — never the 300s default
    /// that relocked mid-build, never no-timeout).
    #[cfg(target_os = "macos")]
    #[test]
    fn cursor_keychain_seeded_empty_when_absent() {
        let home = tempfile::tempdir().unwrap();

        // The seed's own unlock witness (batch A's exit 0) is the only
        // trustworthy non-interactive evidence the store is left unlocked —
        // this db path was never unlocked before, so its credential cache
        // is empty and the witnessed unlock genuinely consumed the stored
        // secret (see the ensure doc for why probing lock state through
        // `security` is unsound).
        assert!(ensure_session_login_keychain(home.path(), "test-session"));

        let db = home
            .path()
            .join("Library")
            .join("Keychains")
            .join("login.keychain-db");
        let meta = std::fs::symlink_metadata(&db).unwrap();
        assert!(meta.is_file(), "the seed is a real file, never a link");
        assert!(meta.len() > 0, "security create-keychain writes a real db");
        // The stored secret is the db's real passphrase by construction —
        // one string is both written 0600 and fed to create-keychain — and
        // the witnessed first unlock above consumed exactly it.
        let unlock_material =
            std::fs::read_to_string(session_keychain_secret_path(home.path())).unwrap();
        assert!(!unlock_material.is_empty());
    }

    /// macOS: an existing keychain path is never replaced — the seed must
    /// not disturb anything already present in the session HOME.
    #[cfg(target_os = "macos")]
    #[test]
    fn cursor_keychain_never_replaces_an_existing_db() {
        let home = tempfile::tempdir().unwrap();
        let keychains = home.path().join("Library").join("Keychains");
        std::fs::create_dir_all(&keychains).unwrap();
        let db = keychains.join("login.keychain-db");
        std::fs::write(&db, b"sentinel").unwrap();

        let _ = ensure_session_login_keychain(home.path(), "test-session");

        assert_eq!(std::fs::read(&db).unwrap(), b"sentinel");
    }

    /// macOS (ticket keychain-passphrase-predictable-permanent-unlock): the
    /// seed's passphrase is a random per-session secret — two sessions never
    /// share one, it is never derived from the session id, it persists 0600
    /// under the session scratch HOME, and a respawn into the same HOME
    /// reuses it (the db keeps the passphrase it was created with).
    #[cfg(target_os = "macos")]
    #[test]
    fn cursor_keychain_hardened_secret_is_random_per_session_and_stored_0600() {
        use std::os::unix::fs::PermissionsExt as _;
        let home_a = tempfile::tempdir().unwrap();
        let home_b = tempfile::tempdir().unwrap();

        // Hosted macOS exposed securityd's account-global mutation race when
        // this test overlapped the other keychain tests. Start both independent
        // homes together and prove the production transaction lock makes both
        // seeds reliable.
        let start = std::sync::Barrier::new(3);
        let (seeded_a, seeded_b) = std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                start.wait();
                ensure_session_login_keychain(home_a.path(), "test-session")
            });
            let b = scope.spawn(|| {
                start.wait();
                ensure_session_login_keychain(home_b.path(), "test-session")
            });
            start.wait();
            (a.join().unwrap(), b.join().unwrap())
        });
        assert!(seeded_a);
        assert!(seeded_b);

        let path_a = session_keychain_secret_path(home_a.path());
        let secret_a = std::fs::read_to_string(&path_a).unwrap();
        let secret_b =
            std::fs::read_to_string(session_keychain_secret_path(home_b.path())).unwrap();
        assert_ne!(
            secret_a, secret_b,
            "each session gets its own random secret"
        );
        // Both homes were seeded with the SAME session id — the secret must
        // not derive from it (the v2 hole was kranz-scratch-{session_id}).
        assert!(!secret_a.contains("test-session"));
        assert_eq!(secret_a.len(), 32, "a uuid v4 simple secret is 128 bits");
        assert!(secret_a.chars().all(|c| c.is_ascii_hexdigit()));
        let mode = std::fs::metadata(&path_a).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the secret file must be owner-only, got {mode:o}"
        );

        assert!(ensure_session_login_keychain(home_a.path(), "test-session"));
        assert_eq!(
            std::fs::read_to_string(&path_a).unwrap(),
            secret_a,
            "a respawn into the same HOME reuses the stored secret"
        );
    }

    /// macOS: the seed restores a bounded auto-lock (never the cleared
    /// no-timeout of v2) and leaves the store unlocked for the session.
    #[cfg(target_os = "macos")]
    #[test]
    fn cursor_keychain_hardened_lock_timeout_is_bounded_and_unlocked() {
        let home = tempfile::tempdir().unwrap();

        assert!(
            ensure_session_login_keychain(home.path(), "test-session"),
            "the seed's own unlock witness: batch A exited 0, so the store \
             is known-unlocked and show-keychain-info below cannot prompt"
        );

        let db = home
            .path()
            .join("Library")
            .join("Keychains")
            .join("login.keychain-db");
        // show-keychain-info on a LOCKED db pops a GUI auth dialog (and hung
        // the gate suite); it is only called here because the witness above
        // proved the db unlocked.
        let info = security_output(home.path(), &["show-keychain-info", db.to_str().unwrap()]);
        assert!(
            info.status.success(),
            "show-keychain-info on the known-unlocked db: {}",
            String::from_utf8_lossy(&info.stderr)
        );
        // show-keychain-info prints the settings line on STDERR.
        let info_text = format!(
            "{}{}",
            String::from_utf8_lossy(&info.stdout),
            String::from_utf8_lossy(&info.stderr)
        );
        assert!(
            info_text.contains(&format!("timeout={SESSION_KEYCHAIN_LOCK_SECS}s")),
            "the auto-lock is bounded, never no-timeout: {info_text}"
        );
    }

    /// macOS: session teardown relocks the seeded store. The locked state
    /// itself is NOT asserted: it cannot be probed non-interactively (the
    /// wrong-pass probe lies and re-unlocks via securityd's credential
    /// cache for a previously-unlocked login-named db; interrogating a
    /// locked db can hang on a GUI dialog — see the ensure doc). What is
    /// asserted: the teardown hook ran `lock-keychain` on the session db to
    /// exit 0 — verified live 2026-08-09 to genuinely lock (a post-lock
    /// `set-keychain-settings` fails 152) — idempotently, and the bounded
    /// auto-lock is the independent backstop.
    #[cfg(target_os = "macos")]
    #[test]
    fn cursor_keychain_hardened_teardown_relocks_the_store() {
        let home = tempfile::tempdir().unwrap();
        assert!(ensure_session_login_keychain(home.path(), "test-session"));

        let ran = lock_session_login_keychain(home.path()).unwrap();
        assert!(ran, "the teardown hook ran lock-keychain on the session db");

        let again = lock_session_login_keychain(home.path()).unwrap();
        assert!(
            again,
            "relocking an already-locked db neither prompts nor errors"
        );

        // The respawn path stays intact: the stored secret re-unlocks
        // (prompt-free with -p supplied; also the exact command the next
        // spawn's batch A runs).
        let db = home
            .path()
            .join("Library")
            .join("Keychains")
            .join("login.keychain-db");
        let unlock_material =
            std::fs::read_to_string(session_keychain_secret_path(home.path())).unwrap();
        assert!(
            security_output(
                home.path(),
                &[
                    "unlock-keychain",
                    "-p",
                    &unlock_material,
                    db.to_str().unwrap(),
                ]
            )
            .status
            .success(),
            "the stored secret re-unlocks after teardown"
        );
    }

    /// macOS: a pre-hardening scratch HOME (db created with the
    /// session-derived passphrase, no secret file) still unlocks — the
    /// legacy fallback keeps in-flight homes from wedging across the
    /// upgrade. The returned witness is honest here: this db path was never
    /// successfully unlocked before ensure ran, so no securityd credential
    /// cache can mask a wrong passphrase — had the fallback been wrong,
    /// batch A would have exited non-zero.
    #[cfg(target_os = "macos")]
    #[test]
    fn cursor_keychain_hardened_legacy_seed_still_unlocks() {
        let home = tempfile::tempdir().unwrap();
        let keychains = home.path().join("Library").join("Keychains");
        std::fs::create_dir_all(&keychains).unwrap();
        let db = keychains.join("login.keychain-db");
        // Recreate the v2 shape: derived passphrase, no secret file, locked.
        let created = security_output(
            home.path(),
            &[
                "create-keychain",
                "-p",
                "kranz-scratch-test-session",
                db.to_str().unwrap(),
            ],
        );
        assert!(created.status.success());
        let locked = security_output(home.path(), &["lock-keychain", db.to_str().unwrap()]);
        assert!(locked.status.success());

        assert!(
            ensure_session_login_keychain(home.path(), "test-session"),
            "the legacy derived passphrase still unlocks the pre-hardening db"
        );
    }

    /// The one sanctioned auth var crosses when set; ambient secrets never do.
    #[test]
    fn cursor_child_env_injects_the_sanctioned_api_key_and_never_ambient_secrets() {
        let _poison = crate::agent_env::EnvTestGuard::engage(&[
            ("CURSOR_API_KEY", "hunter2"),
            ("GH_TOKEN", "ghp-poison"),
            ("SLACK_BOT_TOKEN", "xoxb-poison"),
        ]);
        let session_spec = spec(Path::new("."), false);

        let env = cursor_child_env(&session_spec);

        assert_eq!(
            env.get("CURSOR_API_KEY").map(String::as_str),
            Some("hunter2"),
            "the sanctioned auth var must be injected explicitly"
        );
        for secret in ["GH_TOKEN", "SLACK_BOT_TOKEN", "ANTHROPIC_API_KEY"] {
            assert!(!env.contains_key(secret), "child env leaked {secret}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn cursor_probe_version_kills_a_hung_binary_within_the_deadline() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("hung-agent");
        std::fs::write(&stub, "#!/bin/sh\nsleep 30\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let start = std::time::Instant::now();
        let result = probe_version(&stub);

        let error = result.expect_err("a hung probe must be reported as broken");
        assert!(error.contains("did not exit"), "{error}");
        assert!(
            start.elapsed() < std::time::Duration::from_secs(10),
            "probe returned within the deadline, not after the stub's sleep"
        );
    }

    #[test]
    fn cursor_discovery_honors_env_override_exclusively() {
        // ENV_TEST_LOCK first: tempfile resolves its parent from ambient
        // TMP/TEMP, and env-poisoning tests elsewhere in this binary hold the
        // same lock (see `KIMI_ENV_LOCK`'s note in backend_kimi.rs).
        let _env_lock = crate::agent_env::ENV_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _guard = CURSOR_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let working = dir.path().join("working-agent");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(&working, "#!/bin/sh\necho 2026.07.08-test\n").unwrap();
            std::fs::set_permissions(&working, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let bogus = dir.path().join("does-not-exist-agent");

        std::env::set_var("KRANZ_CURSOR_BIN", &bogus);
        let result = discover_cursor_binary(Some(working.to_str().unwrap()));
        std::env::remove_var("KRANZ_CURSOR_BIN");

        let error = result.expect_err("a broken KRANZ_CURSOR_BIN must fail immediately");
        assert!(
            error.to_string().contains("KRANZ_CURSOR_BIN"),
            "expected the error to name the exclusive override, got: {error}"
        );
        assert!(
            !error.to_string().contains("working-agent"),
            "the exclusive override must not fall through to `configured`, got: {error}"
        );
    }

    #[test]
    fn backend_cursor_parse_fixture() {
        let mut events: Vec<AgentEvent> = Vec::new();
        for line in fixture_lines() {
            events.extend(parse_cursor_line(&line, TEST_MODEL));
        }

        assert!(
            events.iter().any(
                |e| matches!(e, AgentEvent::Init { session_id, model, .. }
                    if !session_id.is_empty() && model == "GPT-5.6 Luna 272K Low")
            ),
            "expected an Init event with a non-empty session id and the wire's model display string"
        );
        for kind in ["shellToolCall", "editToolCall", "readToolCall"] {
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, AgentEvent::ToolUse { tool, .. } if tool == kind)),
                "expected a ToolUse event with tool == {kind:?}"
            );
            assert!(
                events
                    .iter()
                    .any(|e| matches!(e, AgentEvent::ToolResult { tool, denied, .. }
                        if tool.as_deref() == Some(kind) && !denied)),
                "expected a non-denied ToolResult event with tool == {kind:?}"
            );
        }
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::Text { text, .. } if !text.is_empty())),
            "expected at least one Text event"
        );

        let terminal = events
            .iter()
            .find_map(|e| match e {
                AgentEvent::Result {
                    text,
                    is_error,
                    usage,
                    cost_usd,
                    num_turns,
                    ..
                } => Some((text, is_error, usage, cost_usd, num_turns)),
                _ => None,
            })
            .expect("expected a terminal Result event");
        let (text, is_error, usage, cost_usd, num_turns) = terminal;
        assert!(
            !text.is_empty(),
            "the terminal result event carries the full result text (no stitching needed)"
        );
        assert!(!is_error);
        assert_eq!(
            *usage,
            TokenUsage {
                input: 32473,
                output: 305,
                cache_read: 96675,
                cache_write: 0,
            },
            "the fixture's usage object must map verbatim onto TokenUsage"
        );
        assert!(
            cost_usd.is_some(),
            "usage is on the wire, so a client-side computed cost must be present"
        );
        assert_eq!(*num_turns, Some(1));
    }

    #[test]
    fn build_args_maps_read_only_to_mode_ask_and_writable_to_force() {
        let read_only = build_args(&spec(Path::new("/tmp/ws"), false));
        assert_eq!(
            read_only,
            vec![
                "--print",
                "--output-format",
                "stream-json",
                "--trust",
                "--workspace",
                "/tmp/ws",
                "--model",
                TEST_MODEL,
                "--mode",
                "ask",
                "do the thing",
            ]
        );
        let writable = build_args(&spec(Path::new("/tmp/ws"), true));
        assert_eq!(
            writable,
            vec![
                "--print",
                "--output-format",
                "stream-json",
                "--trust",
                "--workspace",
                "/tmp/ws",
                "--model",
                TEST_MODEL,
                "--force",
                "do the thing",
            ]
        );
    }

    #[test]
    fn build_args_ignores_claude_only_fields_and_folds_the_system_prompt() {
        let mut session_spec = spec(Path::new("."), false);
        session_spec.append_system_prompt = Some("be terse".to_string());
        session_spec.permission_mode = Some("acceptEdits".to_string());
        session_spec.allowed_tools = vec!["Bash(npm test*)".to_string()];
        session_spec.disallowed_tools = vec!["Bash(git push*)".to_string()];
        session_spec.tools = vec!["Bash".to_string()];
        session_spec.settings_json = Some(json!({"hooks": {}}));
        session_spec.json_schema = Some(json!({"type": "object"}));
        session_spec.max_budget_usd = Some(5.0);

        let args = build_args(&session_spec);
        assert_eq!(
            args.last().map(String::as_str),
            Some("be terse\n\ndo the thing")
        );
        for forbidden in [
            "--effort",
            "--permission-mode",
            "--allowedTools",
            "--disallowedTools",
            "--tools",
            "--settings",
            "--json-schema",
            "--max-budget-usd",
            "--sandbox",
            "--worktree",
            "--yolo",
        ] {
            assert!(
                !args.iter().any(|a| a == forbidden),
                "argv must not contain {forbidden}: {args:?}"
            );
        }
    }

    /// Probe item 2 / the brief's table: a `result.failure` with a real
    /// exitCode is a normal failed command, never a guardrail denial.
    #[test]
    fn tool_result_failure_is_a_normal_failure_not_a_denial() {
        let completed = json!({
            "type": "tool_call",
            "subtype": "completed",
            "call_id": "c1",
            "tool_call": {
                "shellToolCall": {
                    "args": {"command": "git push origin main"},
                    "result": {"failure": {
                        "command": "git push origin main",
                        "exitCode": 1,
                        "signal": "",
                        "stdout": "",
                        "stderr": "denied by policy",
                        "aborted": false
                    }}
                }
            }
        });
        let events = parse_cursor_value(completed, TEST_MODEL);
        match &events[0] {
            AgentEvent::ToolResult {
                tool,
                denied,
                summary,
                ..
            } => {
                assert_eq!(tool.as_deref(), Some("shellToolCall"));
                assert!(
                    !denied,
                    "a failed command with a real exit code is not a denial"
                );
                assert_eq!(summary, "denied by policy");
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    /// Absent stays absent: a terminal result with no `usage` object records
    /// the zero default and `cost_usd: None` — cost is computed client-side
    /// only from REAL usage tokens, never fabricated from nothing.
    #[test]
    fn result_without_usage_keeps_usage_and_cost_absent() {
        let result = json!({
            "type": "result",
            "subtype": "success",
            "duration_ms": 10,
            "is_error": false,
            "result": "done",
        });
        let events = parse_cursor_value(result, TEST_MODEL);
        match &events[0] {
            AgentEvent::Result {
                usage, cost_usd, ..
            } => {
                assert_eq!(*usage, TokenUsage::default(), "usage is never fabricated");
                assert_eq!(*cost_usd, None, "unreported usage means no cost either");
            }
            other => panic!("expected Result, got {other:?}"),
        }
    }

    /// A torn/unparseable line is never a parse failure: it rides the
    /// transcript as `Other { raw: {"unparsed": ... } }`.
    #[test]
    fn unparseable_lines_become_other_transcript_entries() {
        let events = parse_cursor_line("{\"type\":\"resu", TEST_MODEL);
        assert_eq!(events.len(), 1);
        match &events[0] {
            AgentEvent::Other { raw } => {
                assert_eq!(raw["unparsed"], "{\"type\":\"resu");
            }
            other => panic!("expected Other, got {other:?}"),
        }
    }

    /// Probe item 5: the pre-billing rejection phrases are detected
    /// case-insensitively; ordinary output never trips the detector.
    #[test]
    fn pre_billing_failure_detection_names_only_known_rejections() {
        assert!(names_pre_billing_failure(
            "Cannot use this model: bogus-id. Available models: gpt-5"
        ));
        assert!(names_pre_billing_failure("Authentication required"));
        assert!(!names_pre_billing_failure("README.md"));
        assert!(!names_pre_billing_failure(""));
    }

    /// 14th-pass review: the match is line-anchored, not a loose substring —
    /// text that merely QUOTES a rejection (a torn stream-json fragment of
    /// an assistant event, a tool relaying a remote's error mid-line) is not
    /// a pre-billing failure.
    #[test]
    fn pre_billing_match_ignores_quoted_phrases_and_torn_json() {
        // A torn stream-json line whose assistant text quotes the phrase.
        assert!(!names_pre_billing_failure(
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"text\":\"the remote said Authentication required\""
        ));
        // Mid-line mentions (a tool's stderr relaying a remote's rejection).
        assert!(!names_pre_billing_failure(
            "remote: Authentication required"
        ));
        assert!(!names_pre_billing_failure(
            "exit 1 upstream: Cannot use this model: gpt-5"
        ));
        // The CLI's own rejection lines still match: phrase-led,
        // case-insensitive, leading whitespace tolerated.
        assert!(names_pre_billing_failure(
            "Cannot use this model: bogus-id. Available models: gpt-5"
        ));
        assert!(names_pre_billing_failure("  authentication required"));
    }
}
