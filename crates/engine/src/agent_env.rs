//! Cleared-environment construction for every prompt-injectable child the
//! engine spawns (ticket `agent-env-clear`, P1 of the 2026-07-28
//! hostile-workload review).
//!
//! Before this module, agent CLI sessions spawned with `.envs(&spec.env)`
//! overlaid on the FULL ambient environment (backend_claude.rs), and contract
//! `command` assertions ran with `clear_env = false` (command_exec.rs) — so
//! ambient server secrets (Slack tokens, GH_TOKEN, cloud credentials,
//! remote-workspace tokens) reached every prompt-injectable child. Now:
//!
//! - **Agent CLI sessions** (claude/codex/droid/kimi backends) spawn with
//!   `env_clear` + [`sanitized_child_env`]: PATH, a scratch HOME, locale
//!   vars, and nothing else — plus backend-specific auth injected explicitly
//!   ([`agent_session_env`]), never the ambient set.
//! - **Contract/gate commands** (validation round, final gate, approval-time
//!   contract lint) run with `env_clear` + [`contract_command_env`]: the
//!   sanitized base plus `KRANZ_BASE_SHA`, toolchain caches, and at most the
//!   operator's `contractEnvPassthrough` names.
//!
//! The ENGINE process itself keeps its ambient environment — the clearing
//! applies to child processes only. Merge gates keep their own pre-existing
//! `command_exec::sanitized_gate_env` allowlist (it intentionally retains
//! ambient `HOME`/`CI`/temp dirs for the operator's toolchain; not a clean
//! swap for this module's scratch-HOME shape, so both lists stay, each
//! documented at its site).
//!
//! Secret hygiene: only variable NAMES are ever logged here (the injected
//! auth key's name, the passthrough names applied/skipped) — never values.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Locale/terminal variables passed through from ambient when present. None
/// of them carry credentials; a missing one is simply omitted (CI runners
/// routinely have no `TERM`). `USER` rides along as account identity, not a
/// credential: the `claude` CLI's keychain-backed OAuth resolution FAILS
/// without it ("Not logged in", probed 2026-07-29 — `USER` alone is
/// sufficient, `LOGNAME` is not consulted), and a username is already
/// visible in every absolute path the child sees.
const AMBIENT_LOCALE_VARS: &[&str] = &["TERM", "LANG", "LC_ALL", "TZ", "USER"];

/// Windows process requirements passed through from ambient: without
/// `SystemRoot`/`ComSpec`/`PATHEXT` `cmd` and process creation break; the
/// remaining names are machine-descriptive (not credentials) that `cmd`,
/// PowerShell, and the .NET CLR consult on startup — a child missing them
/// hangs or misbehaves in opaque ways (Windows CI, 89f05a1). Names are
/// matched CASE-INSENSITIVELY (`SystemRoot` vs `SYSTEMROOT`) and emitted
/// under the canonical casing below so the child env block never carries
/// duplicate-case entries (Windows env lookup is case-insensitive; a block
/// with both casings is undefined which wins). `USERPROFILE`/`APPDATA`/
/// `LOCALAPPDATA`/`TEMP`/`TMP` are NOT passed through: like `HOME` they
/// are redirected to the scratch dir, never the operator's real profile.
#[cfg(windows)]
const AMBIENT_WINDOWS_VARS: &[&str] = &[
    "SystemRoot",
    "ComSpec",
    "PATHEXT",
    "SystemDrive",
    "windir",
    "OS",
    "PROCESSOR_ARCHITECTURE",
    "PSModulePath",
];

/// Toolchain cache locations contract commands may inherit from ambient
/// (design decision 3 of the ticket): they speed up `cargo`/`npm` contract
/// gates — dropping CARGO_HOME (3rd-pass review) forces a cold registry
/// cache per mission, which fails outright under an egress-restricted
/// sandbox. CARGO_HOME also carries `credentials.toml` (registry auth
/// tokens); that is handled where the boundary actually is: the sandbox
/// profiles READ-DENY it ([`crate::sandbox::authority_read_deny_paths`]),
/// and contract command text is operator-approved at draft time. An
/// UNSANDBOXED contract command can still read it — the documented residual
/// for running with `sandbox.enforce=off`.
const CONTRACT_TOOLCHAIN_VARS: &[&str] = &["CARGO_HOME", "RUSTUP_HOME", "NPM_CONFIG_CACHE"];

/// Env names [`contract_command_env`] manages itself; a `contractEnvPassthrough`
/// entry naming one of these is refused (loudly, name only) so the escape
/// hatch cannot silently saw off the isolation it sits on — e.g. passing
/// `HOME` through would hand the operator's real home to the contract.
fn managed_contract_keys() -> &'static [&'static str] {
    &[
        "PATH",
        "HOME",
        "USERPROFILE",
        "TMPDIR",
        "TEMP",
        "TMP",
        "APPDATA",
        "LOCALAPPDATA",
        "SystemRoot",
        "SYSTEMROOT",
        "ComSpec",
        "COMSPEC",
        "PATHEXT",
        "TERM",
        "LANG",
        "LC_ALL",
        "TZ",
        "USER",
        "KRANZ_BASE_SHA",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "NPM_CONFIG_CACHE",
    ]
}

/// Build a cleared child environment from scratch: EXACTLY `PATH` (from
/// ambient — binaries must resolve), `HOME = base_home` (the scratch dir the
/// session/command already gets, never the operator's real home),
/// `TMPDIR = base_home/tmp`, the ambient locale vars when present, and on
/// Windows the process-required passthroughs ([`AMBIENT_WINDOWS_VARS`])
/// plus `USERPROFILE = base_home`, `TEMP`/`TMP = base_home/tmp`, and
/// `APPDATA`/`LOCALAPPDATA = base_home/AppData/{Roaming,Local}`. Then
/// `extra` is applied verbatim, in order —
/// that is where `KRANZ_BASE_SHA`, proxy wiring, git identity, and
/// backend-specific auth go. NOTHING else crosses from ambient.
///
/// Creates `base_home`, `base_home/tmp` (and on Windows the AppData dirs)
/// best-effort (a child pointing at a nonexistent HOME/TMPDIR fails in
/// opaque ways); a creation failure is not fatal to env construction — the
/// child surfaces it on its own.
pub fn sanitized_child_env(
    base_home: &Path,
    extra: &[(String, String)],
) -> HashMap<String, String> {
    let _ = std::fs::create_dir_all(base_home.join("tmp"));

    let mut env = HashMap::new();
    if let Some(path) = std::env::var_os("PATH") {
        env.insert("PATH".to_string(), path.to_string_lossy().into_owned());
    }
    env.insert("HOME".to_string(), base_home.display().to_string());
    env.insert(
        "TMPDIR".to_string(),
        base_home.join("tmp").display().to_string(),
    );
    for key in AMBIENT_LOCALE_VARS {
        if let Some(value) = std::env::var_os(key) {
            env.insert((*key).to_string(), value.to_string_lossy().into_owned());
        }
    }
    #[cfg(windows)]
    {
        for key in AMBIENT_WINDOWS_VARS {
            // Case-insensitive ambient lookup, canonical-cased emission:
            // Windows env names are case-insensitive, but the child block is
            // a Rust HashMap keyed case-SENSITIVELY — without this, ambient
            // `SYSTEMROOT` + canonical `SystemRoot` produce duplicate-case
            // entries and which one the child sees is undefined.
            if let Some((_, value)) =
                std::env::vars_os().find(|(k, _)| k.to_string_lossy().eq_ignore_ascii_case(key))
            {
                env.insert((*key).to_string(), value.to_string_lossy().into_owned());
            }
        }
        // Profile/temp locations redirect to scratch (like HOME), never the
        // operator's real profile. `cmd` stages pipe temp files in %TEMP%
        // and PowerShell/CLR consult APPDATA/LOCALAPPDATA on startup —
        // leaving them unset hangs children in opaque ways (89f05a1 CI).
        let tmp = base_home.join("tmp");
        let appdata_roaming = base_home.join("AppData").join("Roaming");
        let appdata_local = base_home.join("AppData").join("Local");
        let _ = std::fs::create_dir_all(&appdata_roaming);
        let _ = std::fs::create_dir_all(&appdata_local);
        env.insert("USERPROFILE".to_string(), base_home.display().to_string());
        env.insert("TEMP".to_string(), tmp.display().to_string());
        env.insert("TMP".to_string(), tmp.display().to_string());
        env.insert("APPDATA".to_string(), appdata_roaming.display().to_string());
        env.insert(
            "LOCALAPPDATA".to_string(),
            appdata_local.display().to_string(),
        );
    }
    for (key, value) in extra {
        env.insert(key.clone(), value.clone());
    }
    env
}

/// The per-session scratch `HOME` used when a session spec carries no
/// relocated `HOME` of its own: the `home` dir under the same per-session
/// scratch root worker relocation uses
/// ([`crate::backend_claude::scratch_home_root`]), so sandboxed sessions get
/// a HOME inside their writable TMPDIR allowlist either way.
pub fn session_scratch_home(session_id: &str) -> PathBuf {
    crate::backend_claude::scratch_home_root(session_id).join("home")
}

/// The cleared env for one agent CLI session, uniform across the spawning
/// backends (claude/codex/droid/kimi).
///
/// - `base_home` is the session's relocated scratch `HOME` when `spec_env`
///   carries one (worker relocation, the auth probe's candidate env), else a
///   fresh per-session scratch home.
/// - Every `spec_env` entry crosses (it is engine-built: `KRANZ_BASE_SHA`,
///   `CLAUDE_CONFIG_DIR`, git identity, egress-proxy vars).
/// - `auth_env_name` is the ONE ambient var this backend may need to
///   authenticate (`ANTHROPIC_API_KEY` for claude, `OPENAI_API_KEY` for
///   codex, …): injected only when the operator actually has it set, and
///   recorded name-only. Ambient `GH_TOKEN`/`SLACK_*`/`AWS_*`/`GOOGLE_*`
///   never cross, regardless.
pub fn agent_session_env(
    spec_env: &HashMap<String, String>,
    session_id: &str,
    auth_env_name: Option<&str>,
) -> HashMap<String, String> {
    let base_home = spec_env
        .get("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| session_scratch_home(session_id));
    session_env_with_home(spec_env, session_id, auth_env_name, &base_home)
}

/// [`agent_session_env`] with an explicit `base_home` — the claude backend
/// uses this after seeding a fresh scratch home (OAuth credentials copy) for
/// a spec that carried no relocated HOME, so the seeded dir is the HOME the
/// child actually gets.
pub fn session_env_with_home(
    spec_env: &HashMap<String, String>,
    session_id: &str,
    auth_env_name: Option<&str>,
    base_home: &Path,
) -> HashMap<String, String> {
    let mut extra: Vec<(String, String)> = spec_env
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    if let Some(name) = auth_env_name {
        if let Some(value) = std::env::var_os(name).filter(|v| !v.is_empty()) {
            // Name only in the log; the value is copied, never recorded.
            tracing::info!(
                session_id = %session_id,
                key = name,
                "backend auth env var injected from ambient into cleared session env"
            );
            extra.push((name.to_string(), value.to_string_lossy().into_owned()));
        }
    }
    sanitized_child_env(base_home, &extra)
}

/// The cleared env for one contract/gate command execution (validation
/// round, final gate, approval-time lint — design decision 3 of the
/// ticket): [`sanitized_child_env`] over the per-mission writable
/// `mission_scratch` home, plus
///
/// - `KRANZ_BASE_SHA` via the shared [`crate::runner::contract_env`] idiom,
/// - the [`CONTRACT_TOOLCHAIN_VARS`] caches from ambient when present,
/// - exactly the ambient vars NAMED in `passthrough` (the mission config's
///   `contractEnvPassthrough` escape hatch — the sanctioned way to give a
///   contract one credential). Names only are logged, never values; a
///   passthrough name colliding with a managed key (PATH/HOME/…) is refused
///   with a warning so the hatch cannot reopen the boundary it sits on.
pub fn contract_command_env(
    mission_scratch: &Path,
    base_sha: Option<&str>,
    passthrough: &[String],
) -> HashMap<String, String> {
    let mut extra: Vec<(String, String)> =
        crate::runner::contract_env(base_sha).into_iter().collect();
    for key in CONTRACT_TOOLCHAIN_VARS {
        if let Some(value) = std::env::var_os(key) {
            extra.push(((*key).to_string(), value.to_string_lossy().into_owned()));
        }
    }
    let managed = managed_contract_keys();
    for name in passthrough {
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        // Case-INSENSITIVE refusal: Windows env names are case-insensitive,
        // so a `path`/`Temp` passthrough would otherwise slip the check and
        // emit a duplicate-case entry — undefined which value the child
        // sees, silently overriding a scratch redirect. Refusing every
        // casing everywhere keeps one rule for all platforms.
        if managed.iter().any(|m| m.eq_ignore_ascii_case(name)) {
            tracing::warn!(
                key = name,
                "contractEnvPassthrough entry refused: name is managed by the contract env itself"
            );
            continue;
        }
        match std::env::var_os(name) {
            Some(value) => {
                extra.push((name.to_string(), value.to_string_lossy().into_owned()));
            }
            None => {
                tracing::warn!(
                    key = name,
                    "contractEnvPassthrough entry named a var that is not set in the ambient env"
                );
            }
        }
    }
    sanitized_child_env(mission_scratch, &extra)
}

// ---------------------------------------------------------------------------

/// Test-only shared lock + env guard for the exfiltration tests across
/// `agent_env` / `backend_claude` / `command_exec`: they poison ambient
/// secret vars, and assertions that depend on an ambient VALUE (e.g. an
/// injected API key) must serialize against each other so a parallel test
/// cannot restore a var mid-assertion.
#[cfg(test)]
pub(crate) static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// RAII guard: set each `(name, value)` pair on engage, restore the prior
/// state (set/unset) on drop, all while holding [`ENV_TEST_LOCK`].
#[cfg(test)]
pub(crate) struct EnvTestGuard {
    vars: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl EnvTestGuard {
    pub(crate) fn engage(settings: &[(&'static str, &str)]) -> Self {
        let lock = ENV_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let vars = settings
            .iter()
            .map(|(name, value)| {
                let prev = std::env::var_os(name);
                std::env::set_var(name, value);
                (*name, prev)
            })
            .collect();
        EnvTestGuard { vars, _lock: lock }
    }

    /// Engage with some vars set and others REMOVED (e.g. prove a key is
    /// absent unless this backend injects it).
    pub(crate) fn engage_unsetting(
        settings: &[(&'static str, &str)],
        unset: &[&'static str],
    ) -> Self {
        let lock = ENV_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut vars: Vec<(&'static str, Option<std::ffi::OsString>)> = settings
            .iter()
            .map(|(name, value)| {
                let prev = std::env::var_os(name);
                std::env::set_var(name, value);
                (*name, prev)
            })
            .collect();
        for name in unset {
            let prev = std::env::var_os(name);
            std::env::remove_var(name);
            vars.push((name, prev));
        }
        EnvTestGuard { vars, _lock: lock }
    }
}

#[cfg(test)]
impl Drop for EnvTestGuard {
    fn drop(&mut self) {
        for (name, prev) in &self.vars {
            match prev {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn extra(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The locked allowlist (design decision 1): poisoned ambient secrets
    /// never cross; PATH/scratch-HOME/locale/TMPDIR do; extra applies
    /// verbatim; and the scratch tmp dir is actually created.
    #[test]
    fn sanitized_child_env_starts_empty_and_never_inherits_secrets() {
        let _poison = EnvTestGuard::engage(&[
            ("GH_TOKEN", "hunter2"),
            ("SLACK_BOT_TOKEN", "xoxb-poison"),
            ("AWS_SECRET_ACCESS_KEY", "aws-poison"),
        ]);
        let home = tempfile::tempdir().unwrap();

        let env = sanitized_child_env(home.path(), &extra(&[("KRANZ_BASE_SHA", "deadbeef")]));

        for secret in [
            "GH_TOKEN",
            "SLACK_BOT_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "SSH_AUTH_SOCK",
            "GOOGLE_APPLICATION_CREDENTIALS",
        ] {
            assert!(!env.contains_key(secret), "child env leaked {secret}");
        }
        assert_eq!(
            env.get("HOME").map(String::as_str),
            Some(home.path().to_string_lossy().as_ref()),
            "HOME must be the scratch dir, never the operator's real home"
        );
        assert_eq!(
            env.get("TMPDIR").map(String::as_str),
            Some(home.path().join("tmp").to_string_lossy().as_ref()),
            "TMPDIR must be <scratch>/tmp"
        );
        assert!(
            home.path().join("tmp").is_dir(),
            "the scratch tmp dir must be created for the child"
        );
        assert_eq!(
            env.get("KRANZ_BASE_SHA").map(String::as_str),
            Some("deadbeef"),
            "extra must apply verbatim"
        );
        if std::env::var_os("PATH").is_some() {
            assert!(env.contains_key("PATH"), "PATH must cross from ambient");
        }
        // Nothing beyond the allowlist + extra crosses.
        let allowed = [
            "PATH",
            "HOME",
            "TMPDIR",
            "TERM",
            "LANG",
            "LC_ALL",
            "TZ",
            "USER",
            "KRANZ_BASE_SHA",
        ];
        for key in env.keys() {
            assert!(
                allowed.contains(&key.as_str()) || cfg!(windows),
                "unexpected key in child env: {key}"
            );
        }
    }

    /// Windows shape: temp/profile dirs redirect into scratch (never the
    /// operator's), machine passthroughs cross case-deduped, and ambient
    /// APPDATA/LOCALAPPDATA/TEMP/TMP do NOT pass through.
    #[cfg(windows)]
    #[test]
    fn sanitized_child_env_windows_redirects_profile_and_temp_to_scratch() {
        // Create the scratch home BEFORE poisoning TEMP/TMP — tempfile
        // resolves its parent from ambient TMP/TEMP, so engaging the guard
        // first would break tempdir creation itself (CI, b4beb75).
        let home = tempfile::tempdir().unwrap();
        let _poison = EnvTestGuard::engage(&[
            ("TEMP", r"C:\operator-temp"),
            ("TMP", r"C:\operator-tmp"),
            ("APPDATA", r"C:\operator-roaming"),
            ("LOCALAPPDATA", r"C:\operator-local"),
        ]);

        let env = sanitized_child_env(home.path(), &extra(&[]));

        let tmp = home.path().join("tmp").display().to_string();
        assert_eq!(env.get("TEMP").map(String::as_str), Some(tmp.as_str()));
        assert_eq!(env.get("TMP").map(String::as_str), Some(tmp.as_str()));
        assert_eq!(
            env.get("USERPROFILE").map(String::as_str),
            Some(home.path().to_string_lossy().as_ref())
        );
        assert!(
            env.get("APPDATA")
                .is_some_and(|v| v.starts_with(&home.path().display().to_string())),
            "APPDATA must redirect under scratch, not the operator profile"
        );
        assert!(
            env.get("LOCALAPPDATA")
                .is_some_and(|v| v.starts_with(&home.path().display().to_string())),
            "LOCALAPPDATA must redirect under scratch"
        );
        // Machine passthroughs cross under canonical casing only (the
        // duplicate-case check below is the strict property).
        if env.keys().any(|k| k.eq_ignore_ascii_case("systemroot")) {
            assert!(
                env.contains_key("SystemRoot"),
                "SystemRoot must be emitted under canonical casing"
            );
        }
        // No duplicate-case keys in the emitted block.
        let mut lowered: Vec<String> = env.keys().map(|k| k.to_ascii_lowercase()).collect();
        lowered.sort();
        lowered.dedup();
        assert_eq!(
            lowered.len(),
            env.len(),
            "child env block carries duplicate-case entries: {:?}",
            env.keys().collect::<Vec<_>>()
        );
    }

    /// Backend auth (design decision 2): exactly the one named key the
    /// backend needs is injected from ambient — a different backend's key
    /// (and every non-auth secret) stays out.
    #[test]
    fn agent_session_env_injects_only_the_backends_own_auth_key() {
        let _poison = EnvTestGuard::engage_unsetting(
            &[
                ("ANTHROPIC_API_KEY", "sk-ant-poison"),
                ("GH_TOKEN", "hunter2"),
            ],
            &["OPENAI_API_KEY"],
        );

        // Claude-shaped spawn: its own key crosses, nothing else does.
        let env = agent_session_env(&HashMap::new(), "sess-claude", Some("ANTHROPIC_API_KEY"));
        assert_eq!(
            env.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-ant-poison"),
            "the backend's own auth key must be injected when set"
        );
        assert!(!env.contains_key("GH_TOKEN"), "GH_TOKEN never crosses");
        assert_eq!(
            env.get("HOME").map(String::as_str),
            Some(
                session_scratch_home("sess-claude")
                    .to_string_lossy()
                    .as_ref()
            ),
            "a HOME-less spec gets the per-session scratch home"
        );

        // Codex-shaped spawn on the same ambient env: the claude key must
        // NOT cross — auth is injected only for the backend that needs it.
        let env = agent_session_env(&HashMap::new(), "sess-codex", Some("OPENAI_API_KEY"));
        assert!(
            !env.contains_key("ANTHROPIC_API_KEY"),
            "another backend's auth key must never be injected"
        );
        assert!(!env.contains_key("OPENAI_API_KEY"), "not set in ambient");
    }

    /// A spec carrying a relocated scratch HOME keeps exactly that HOME —
    /// the worker-relocation / auth-probe candidate path the auth verdict
    /// proved out.
    #[test]
    fn agent_session_env_honors_the_specs_relocated_home() {
        let home = tempfile::tempdir().unwrap();
        let mut spec_env = HashMap::new();
        spec_env.insert("HOME".to_string(), home.path().display().to_string());
        spec_env.insert(
            "CLAUDE_CONFIG_DIR".to_string(),
            home.path().join(".claude").display().to_string(),
        );

        let env = agent_session_env(&spec_env, "sess-worker", None);

        assert_eq!(
            env.get("HOME").map(String::as_str),
            Some(home.path().to_string_lossy().as_ref())
        );
        assert_eq!(
            env.get("CLAUDE_CONFIG_DIR").map(String::as_str),
            Some(home.path().join(".claude").to_string_lossy().as_ref()),
            "the seeded config dir must survive env clearing (auth probe shape)"
        );
    }

    /// Contract env (design decision 3): base-sha + toolchain caches +
    /// passthrough names cross; ambient secrets do not; a passthrough entry
    /// naming a managed key — in ANY letter casing — is refused. CARGO_HOME
    /// crosses (the registry cache makes contract gates viable); its
    /// credentials.toml is denied at the sandbox layer instead.
    #[test]
    fn contract_command_env_shapes_the_gate_boundary() {
        let _guard = EnvTestGuard::engage(&[
            ("RUSTUP_HOME", "/poisoned/rustup-home"),
            ("CARGO_HOME", "/poisoned/cargo-home"),
            ("KRANZ_AGENT_ENV_TEST_CRED", "cred-value"),
            ("GH_TOKEN", "hunter2"),
        ]);
        let scratch = tempfile::tempdir().unwrap();

        // No passthrough configured: exactly base + toolchain caches.
        let env = contract_command_env(scratch.path(), Some("deadbeef"), &[]);
        assert_eq!(
            env.get("KRANZ_BASE_SHA").map(String::as_str),
            Some("deadbeef")
        );
        assert_eq!(
            env.get("RUSTUP_HOME").map(String::as_str),
            Some("/poisoned/rustup-home"),
            "toolchain caches cross from ambient"
        );
        assert_eq!(
            env.get("CARGO_HOME").map(String::as_str),
            Some("/poisoned/cargo-home"),
            "CARGO_HOME crosses (cache locality); credentials.toml is denied at the sandbox"
        );
        assert!(!env.contains_key("GH_TOKEN"));
        assert!(
            !env.contains_key("KRANZ_AGENT_ENV_TEST_CRED"),
            "a credential crosses ONLY when named in contractEnvPassthrough"
        );
        assert_eq!(
            env.get("HOME").map(String::as_str),
            Some(scratch.path().to_string_lossy().as_ref())
        );

        // Passthrough configured: the named var crosses; a managed name is
        // refused in any letter casing (HOME stays the scratch).
        let env = contract_command_env(
            scratch.path(),
            None,
            &[
                "KRANZ_AGENT_ENV_TEST_CRED".to_string(),
                "home".to_string(),
                "KRANZ_AGENT_ENV_TEST_UNSET".to_string(),
            ],
        );
        assert_eq!(
            env.get("KRANZ_AGENT_ENV_TEST_CRED").map(String::as_str),
            Some("cred-value"),
            "the passthrough-named var crosses"
        );
        assert_eq!(
            env.get("HOME").map(String::as_str),
            Some(scratch.path().to_string_lossy().as_ref()),
            "a passthrough entry naming `home` (any casing) must be refused"
        );
        assert!(
            !env.contains_key("KRANZ_BASE_SHA"),
            "no base sha pinned => no KRANZ_BASE_SHA key"
        );
    }
}
