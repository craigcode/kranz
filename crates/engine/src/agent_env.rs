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
//! - **Agent CLI sessions** (claude/codex/droid/kimi/cursor backends) spawn
//!   with `env_clear` + [`sanitized_child_env`]: PATH, a scratch HOME,
//!   locale vars, and nothing else — plus backend-specific auth injected
//!   explicitly ([`agent_session_env`]), never the ambient set.
//! - **Contract/gate commands** (validation round, final gate, approval-time
//!   contract lint) run with `env_clear` + [`contract_command_env`]: the
//!   sanitized base plus `KRANZ_BASE_SHA`, a cache-only Cargo home, the
//!   non-credential toolchain locations, and at most the operator's
//!   `contractEnvPassthrough` names.
//!
//! The ENGINE process itself keeps its ambient environment — the clearing
//! applies to child processes only. Merge gates keep their own pre-existing
//! `command_exec::sanitized_gate_env` allowlist (it intentionally retains
//! ambient `HOME`/`CI`/temp dirs for the operator's toolchain; not a clean
//! swap for this module's scratch-HOME shape, so both lists stay, each
//! documented at its site) — with ONE exception: the gate env never carries
//! the ambient `CARGO_HOME`, which `run_bounded_gate_command` replaces with
//! a fresh [`cache_only_cargo_home`] exactly like the contract env. Under
//! `worker.sandbox.enforce != off` the merge gate additionally runs WRAPPED
//! in the resolved sandbox profile
//! (`command_exec::run_bounded_gate_command_sandboxed`): the ambient HOME
//! pass-through stays (git identity needs `~/.gitconfig`), and the profile
//! makes it read-only — containment by the sandbox, not by env rewrite.
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

/// Add the non-secret Windows process bootstrap variables to a cleared child
/// environment using one canonical spelling per case-insensitive key. Both
/// agent sessions and engine-run gates need this set: ordinary unsandboxed
/// commands may limp along without all of it, while AppContainer process
/// creation fails with `ERROR_ENVVAR_NOT_FOUND` before the child starts.
#[cfg(windows)]
pub(crate) fn extend_windows_process_env(env: &mut HashMap<String, String>) {
    for key in AMBIENT_WINDOWS_VARS {
        if let Some((_, value)) =
            std::env::vars_os().find(|(k, _)| k.to_string_lossy().eq_ignore_ascii_case(key))
        {
            env.insert((*key).to_string(), value.to_string_lossy().into_owned());
        }
    }
}

/// Redirect the Windows user-profile variables that AppContainer process
/// creation consumes to an already-authorized scratch root. Windows rewrites
/// `LOCALAPPDATA`, `TEMP`, and `TMP` again for the AppContainer profile, but
/// requires the profile tuple to exist in an explicit environment block.
#[cfg(windows)]
pub(crate) fn redirect_windows_profile_env(env: &mut HashMap<String, String>, base_home: &Path) {
    let tmp = base_home.join("tmp");
    let appdata_roaming = base_home.join("AppData").join("Roaming");
    let appdata_local = base_home.join("AppData").join("Local");
    for path in [&tmp, &appdata_roaming, &appdata_local] {
        let _ = std::fs::create_dir_all(path);
    }
    env.insert("USERPROFILE".to_string(), base_home.display().to_string());
    env.insert("TMPDIR".to_string(), tmp.display().to_string());
    env.insert("TEMP".to_string(), tmp.display().to_string());
    env.insert("TMP".to_string(), tmp.display().to_string());
    env.insert("APPDATA".to_string(), appdata_roaming.display().to_string());
    env.insert(
        "LOCALAPPDATA".to_string(),
        appdata_local.display().to_string(),
    );
}

/// Toolchain locations children may inherit. `CARGO_HOME` is the exception:
/// [`sanitized_child_env`] always replaces it with a per-invocation
/// cache-only home (see [`cache_only_cargo_home`]), so neither agent sessions
/// nor engine-run contract code receives the ambient credential/config root.
/// `RUSTUP_HOME` must remain visible so a standard rustup shim can locate the
/// installed toolchain.
///
/// Resolution rule for each var: the ambient value when set, ELSE the
/// default under the OPERATOR's real home (`<real home>/.rustup` etc.) when
/// that dir exists. The fallback matters: standard rustup/cargo installs
/// export NEITHER var and derive both from HOME — and the child's HOME is
/// mission scratch, so without the explicit derivation `cargo --version`
/// fails "no default is configured" (7th-pass review, reproduced on the
/// review host and this one).
const CONTRACT_TOOLCHAIN_VARS: &[(&str, &str)] = &[
    ("CARGO_HOME", ".cargo"),
    ("RUSTUP_HOME", ".rustup"),
    ("NPM_CONFIG_CACHE", ".npm"),
];

/// Above this size seeding one shared cache directory as a per-env COPY —
/// even an accelerated clonefile/reflink one — costs more wall clock and
/// disk per generated child env than the cache reuse saves: this builder
/// runs for EVERY agent session and EVERY contract command, and the copy
/// cost scales with the cache's entry count even when its bytes would
/// clone instantly. Two measurements set the ceiling. Local (2026-08-03):
/// a 1.34 GiB / ~55k-entry APFS registry takes ~7s to clonefile per env —
/// all syscall time — and a mission builds dozens of these envs. CI
/// (same day, run 30842947196): a 512 MiB ceiling put every runner's
/// registry UNDER the copy threshold, so the workspace suite copied
/// hundreds of MB per env-build until all three OS legs filled their
/// disks (windows-latest died "No space left"). Above the ceiling the
/// cache is therefore LINKED instead — the residual trade documented at
/// [`cache_only_cargo_home`]: a poisoned write can then still reach the
/// operator's shared cache. That trade stands for real-world registries
/// (which are never this small) until `engine-gates-sandbox-wrapped`
/// (pri 1) lands: under the enforced sandbox the link target is outside
/// the writable roots and read-only in practice, which is the finding's
/// true fix. The ceiling still protects the small-cache rigs where the
/// copy is genuinely cheap.
const CACHE_COPY_MAX_BYTES: u64 = 64 * 1024 * 1024;

/// File names that must NEVER reach a contract Cargo home: credentials and
/// credential-provider configuration. Only `registry/` and `git/` are ever
/// seeded, so these names cannot legitimately appear inside them — the copy
/// skips them EXPLICITLY anyway (loudly), so a planted
/// `registry/credentials.toml` cannot ride the seed into the child's home.
const CARGO_CACHE_NEVER_SEED: &[&str] =
    &["credentials.toml", "credentials", "config.toml", "config"];

/// Build a fresh Cargo home containing only the two cache directories Cargo
/// uses for registry and git dependencies. Root-level Cargo configuration,
/// `credentials.toml`, and the legacy `credentials` file are deliberately
/// never copied or linked. This matters even though contract command text is
/// operator-approved: `cargo test` executes worker-authored build scripts and
/// test binaries outside the agent sandbox.
///
/// A fresh, unpredictable directory is used for every generated child env so
/// worker code cannot pre-plant `config.toml` or a credential-provider in a
/// stable scratch location. Only `registry/` and `git/` are seeded into it,
/// preserving cache locality without making the operator's Cargo root
/// reachable.
///
/// The seed is a per-env COPY, not a link (12th-pass review, P1): the
/// operator's real caches were previously SYMLINKED in, so worker-authored
/// contract code writing through its Cargo cache could poison the shared
/// cache for later missions and engine builds. Now each cache is seeded
/// through the same tier order as the validator snapshot's `target/` warm
/// ([`crate::validator_snapshot`]): APFS clonefile, else Linux reflink —
/// both copy-on-write, so a write through the seeded cache never reaches the
/// operator's bytes — else a plain byte copy. But only at or below
/// [`CACHE_COPY_MAX_BYTES`]: above that ceiling even an accelerated copy
/// costs more per child env than the reuse saves, so the cache is still
/// LINKED (with the trade named in a warning): a poisoned write can then
/// reach the shared cache, but only one the operator let grow past the
/// ceiling. A failed seed simply leaves that cache absent and lets Cargo
/// populate the isolated home (unchanged).
///
/// Used by BOTH child-env builders here and by
/// [`crate::command_exec::run_bounded_gate_command`], whose merge-gate env
/// substitutes this for the ambient `CARGO_HOME` over a self-cleaning temp
/// scratch.
pub(crate) fn cache_only_cargo_home(base_home: &Path) -> PathBuf {
    let destination = base_home.join(format!(
        ".cargo-cache-only-{}",
        uuid::Uuid::new_v4().simple()
    ));
    if let Err(error) = std::fs::create_dir_all(&destination) {
        tracing::warn!(
            path = %destination.display(),
            error = %error,
            "could not create cache-only Cargo home; Cargo will surface the failure"
        );
        return destination;
    }

    let Some(source) = toolchain_var_value("CARGO_HOME", ".cargo").map(PathBuf::from) else {
        return destination;
    };
    for name in ["registry", "git"] {
        let from = source.join(name);
        let to = destination.join(name);
        if !from.is_dir() {
            continue;
        }
        seed_cargo_cache(name, &from, &to);
    }
    destination
}

/// Seed one shared cache directory (`registry/` or `git/`) into the isolated
/// contract home. At or below [`CACHE_COPY_MAX_BYTES`] the seed is a per-env
/// COPY through the same tier order as the validator snapshot's `target/`
/// warm — clonefile, else reflink, else plain copy — so a write through the
/// child's cache can never reach the operator's bytes. Above the ceiling
/// (measured by [`crate::validator_snapshot::dir_size_exceeds`], which stops
/// its walk the moment the answer is known) the cache is LINKED, with the
/// trade named — the pre-12th-pass behavior, kept for exactly the case a
/// copy is prohibitively expensive. Credential-shaped top-level entries are
/// excluded from every copy tier explicitly ([`CARGO_CACHE_NEVER_SEED`]). A
/// failed seed leaves the cache absent and lets Cargo populate the isolated
/// home.
fn seed_cargo_cache(name: &str, from: &Path, to: &Path) {
    if crate::validator_snapshot::dir_size_exceeds(from, CACHE_COPY_MAX_BYTES) {
        // The documented residual trade: the cache exceeds the copy ceiling,
        // so even an accelerated copy would cost more per child env than the
        // reuse saves. Linking keeps the cache available, but a poisoned
        // write through the child's Cargo cache reaches the operator's
        // shared cache — accepted only for a cache the operator let grow
        // past the ceiling.
        tracing::warn!(
            cache = name,
            source = %from.display(),
            "shared Cargo cache exceeds the copy ceiling; LINKING it into the contract home — \
             cache writes from worker-authored contract code will reach the shared cache"
        );
    } else if copy_cargo_cache_entries(from, to, crate::validator_snapshot::copy_dir_clonefile)
        || copy_cargo_cache_entries(from, to, crate::validator_snapshot::copy_dir_reflink)
        || copy_cargo_cache_entries(from, to, copy_entry_plain)
    {
        return;
    } else {
        tracing::warn!(
            cache = name,
            source = %from.display(),
            "every copy tier failed for the shared Cargo cache; falling back to linking it"
        );
    }
    link_cargo_cache(name, from, to);
}

/// Copy each top-level entry of `from` into `to` with `copy_entry` (which
/// handles files and dirs uniformly), skipping [`CARGO_CACHE_NEVER_SEED`]
/// names explicitly. `false` on the first entry that fails — the partial
/// copy is swept before returning, mirroring `run_cp`'s discipline in
/// [`crate::validator_snapshot`], so the caller's next tier starts clean.
fn copy_cargo_cache_entries(from: &Path, to: &Path, copy_entry: fn(&Path, &Path) -> bool) -> bool {
    let Ok(entries) = std::fs::read_dir(from) else {
        return false;
    };
    if std::fs::create_dir_all(to).is_err() {
        return false;
    }
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        if CARGO_CACHE_NEVER_SEED.contains(&file_name.to_string_lossy().as_ref()) {
            tracing::warn!(
                cache = %from.display(),
                entry = %file_name.to_string_lossy(),
                "skipping credential-shaped entry while seeding the contract Cargo cache"
            );
            continue;
        }
        if !copy_entry(&entry.path(), &to.join(&file_name)) {
            let _ = std::fs::remove_dir_all(to);
            return false;
        }
    }
    true
}

/// Plain-copy one cache entry: [`crate::validator_snapshot::copy_dir_plain`]
/// for directories (Cargo cache top-levels like `registry/cache/`), a plain
/// `std::fs::copy` for files (`registry/CACHEDIR.TAG`, lockfiles). Symlinks
/// are followed either way — the copy owns real bytes, never a link into
/// the operator's cache.
fn copy_entry_plain(src: &Path, dst: &Path) -> bool {
    if src.is_dir() {
        crate::validator_snapshot::copy_dir_plain(src, dst).is_ok()
    } else {
        std::fs::copy(src, dst).is_ok()
    }
}

/// Link the operator's cache dir into the contract home — the pre-12th-pass
/// behavior, now ONLY the last resort when the cache is over the copy
/// ceiling or every copy tier failed. A failed link leaves the cache absent
/// and lets Cargo populate the isolated home (unchanged).
fn link_cargo_cache(name: &str, from: &Path, to: &Path) {
    #[cfg(unix)]
    if let Err(error) = std::os::unix::fs::symlink(from, to) {
        tracing::warn!(
            cache = name,
            source = %from.display(),
            error = %error,
            "could not seed contract Cargo cache; using an empty isolated cache"
        );
    }
    #[cfg(windows)]
    if let Err(error) = std::os::windows::fs::symlink_dir(from, to) {
        tracing::warn!(
            cache = name,
            source = %from.display(),
            error = %error,
            "could not seed contract Cargo cache; using an empty isolated cache"
        );
    }
}

/// The operator's home directory from the OS account record (`getpwuid_r`),
/// NOT the ambient `HOME` env var (ticket contract-toolchain-home-os-account).
/// In env_clear'd / sandboxed gate contexts `HOME` is absent or points at a
/// relocated scratch dir, so deriving CARGO_HOME/RUSTUP_HOME from it silently
/// degrades (the m-eee81f workers each misread this as an in-scope bug). The
/// passwd entry is the operator's real home regardless of the process env.
/// `HOME` is consulted only as a fallback when the account record is
/// unavailable, and the toolchain env vars themselves remain the explicit
/// override (handled in [`toolchain_var_value`]).
#[cfg(unix)]
fn os_account_home() -> Option<PathBuf> {
    // getpwuid_r (the reentrant form): the engine is a multi-threaded tokio
    // process, so the static-buffer getpwuid is not sound here. pw_dir points
    // into `buf`; copy it to an owned PathBuf before returning.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0_u8; 4096];
    let mut entry_ptr = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr() as *mut libc::c_char,
            buf.len(),
            &mut entry_ptr,
        )
    };
    if rc != 0 || entry_ptr.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    let home = unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) }
        .to_string_lossy()
        .into_owned();
    (!home.is_empty()).then(|| PathBuf::from(home))
}

/// The operator's toolchain home: the OS account record on Unix and the
/// original `USERPROFILE` on Windows, falling back to the ambient `HOME`
/// only when the platform-native source is unavailable. The generated child
/// environment redirects both HOME and USERPROFILE later; this lookup happens
/// first against the engine's operator environment. See [`os_account_home`].
fn operator_home() -> Option<PathBuf> {
    #[cfg(unix)]
    if let Some(home) = os_account_home() {
        return Some(home);
    }
    #[cfg(windows)]
    if let Some(home) = std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(home));
    }
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The value a toolchain var resolves to for a child env: ambient when set,
/// else `<real home>/<default_subdir>` when that directory exists.
fn toolchain_var_value(var: &str, default_subdir: &str) -> Option<String> {
    if let Some(value) = std::env::var_os(var) {
        return Some(value.to_string_lossy().into_owned());
    }
    let real_home = operator_home()?;
    let candidate = real_home.join(default_subdir);
    candidate.is_dir().then(|| candidate.display().to_string())
}

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
/// `TMPDIR = base_home/tmp`, the ambient locale vars when present, the
/// non-credential toolchain locations plus the cache-only Cargo home
/// ([`CONTRACT_TOOLCHAIN_VARS`] / [`cache_only_cargo_home`]; without cache
/// seeding every agent session re-downloads the registry into scratch, which
/// filled the disk and killed mission m-533143), and on
/// Windows the process-required passthroughs (`AMBIENT_WINDOWS_VARS`)
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
    // Non-credential toolchain locations ride for BOTH sessions and contract
    // commands. CARGO_HOME is always replaced with an isolated cache-only
    // root; no prompt-injectable child receives operator Cargo config/tokens.
    for (var, default_subdir) in CONTRACT_TOOLCHAIN_VARS {
        if *var == "CARGO_HOME" {
            continue;
        }
        if let Some(value) = toolchain_var_value(var, default_subdir) {
            env.insert((*var).to_string(), value);
        }
    }
    env.insert(
        "CARGO_HOME".to_string(),
        cache_only_cargo_home(base_home).display().to_string(),
    );
    #[cfg(windows)]
    {
        // Case-insensitive ambient lookup, canonical-cased emission: Windows
        // env names are case-insensitive, but this map is not. Duplicate-case
        // entries make the resulting child block ambiguous.
        extend_windows_process_env(&mut env);
        // Profile/temp locations redirect to scratch (like HOME), never the
        // operator's real profile. `cmd` stages pipe temp files in %TEMP%
        // and PowerShell/CLR consult APPDATA/LOCALAPPDATA on startup —
        // leaving them unset hangs children in opaque ways (89f05a1 CI).
        redirect_windows_profile_env(&mut env, base_home);
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
/// backends (claude/codex/droid/kimi/cursor).
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
/// - a cache-only `CARGO_HOME` plus the non-credential toolchain locations,
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
    for (var, default_subdir) in CONTRACT_TOOLCHAIN_VARS {
        if *var == "CARGO_HOME" {
            continue;
        }
        if let Some(value) = toolchain_var_value(var, default_subdir) {
            extra.push(((*var).to_string(), value));
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
            "CARGO_HOME",
            "RUSTUP_HOME",
            "NPM_CONFIG_CACHE",
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
        // Create both roots BEFORE poisoning TEMP/TMP. The poison paths must
        // exist: Rust tests share one process, so another concurrently
        // running test may legitimately call tempfile while this guard is
        // engaged. A nonexistent C:\operator-tmp made those unrelated tests
        // fail nondeterministically on windows-latest.
        let home = tempfile::tempdir().unwrap();
        let operator = tempfile::tempdir().unwrap();
        let operator_temp = operator.path().join("operator-temp");
        let operator_tmp = operator.path().join("operator-tmp");
        let operator_roaming = operator.path().join("operator-roaming");
        let operator_local = operator.path().join("operator-local");
        for dir in [
            &operator_temp,
            &operator_tmp,
            &operator_roaming,
            &operator_local,
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let operator_temp = operator_temp.display().to_string();
        let operator_tmp = operator_tmp.display().to_string();
        let operator_roaming = operator_roaming.display().to_string();
        let operator_local = operator_local.display().to_string();
        let _poison = EnvTestGuard::engage(&[
            ("TEMP", &operator_temp),
            ("TMP", &operator_tmp),
            ("APPDATA", &operator_roaming),
            ("LOCALAPPDATA", &operator_local),
        ]);
        let _parallel_temp =
            tempfile::tempdir().expect("ambient poison paths must remain usable by parallel tests");

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

    /// 7th-pass review: a standard rustup install exports NEITHER
    /// RUSTUP_HOME nor CARGO_HOME. RUSTUP_HOME must derive from the
    /// OPERATOR's real home or the shim fails "no default is configured";
    /// CARGO_HOME must instead be isolated under scratch. Proven by actually
    /// executing Cargo under the generated env.
    #[cfg(unix)]
    /// Ticket contract-toolchain-home-os-account: with `HOME` UNSET in the
    /// engine's own env (the env_clear'd / sandboxed gate shape), the
    /// toolchain derivation must fall to the OS account record, not silently
    /// degrade to None. On a normal host the account record equals `$HOME`.
    #[cfg(unix)]
    #[test]
    fn toolchain_home_os_account_resolves_when_home_is_unset() {
        let real_home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        let _guard = EnvTestGuard::engage_unsetting(&[], &["HOME", "CARGO_HOME", "RUSTUP_HOME"]);

        // The account record is the source now — HOME is gone, yet the
        // resolved operator home is still the operator's real home.
        let account_home = os_account_home().expect("this host has a passwd entry");
        assert_eq!(account_home, real_home, "account record == $HOME here");
        assert_eq!(operator_home().as_deref(), Some(real_home.as_path()));

        // And the derivation still resolves the operator's real toolchain
        // dirs (only asserted when present, so the test is host-independent).
        if real_home.join(".rustup").is_dir() {
            assert_eq!(
                toolchain_var_value("RUSTUP_HOME", ".rustup"),
                Some(real_home.join(".rustup").display().to_string())
            );
        }
    }

    /// The toolchain env var remains an explicit override: it wins even when
    /// the OS account record disagrees.
    #[cfg(unix)]
    #[test]
    fn toolchain_home_os_account_env_var_is_still_an_explicit_override() {
        let _guard = EnvTestGuard::engage(&[("RUSTUP_HOME", "/explicit/override")]);
        assert_eq!(
            toolchain_var_value("RUSTUP_HOME", ".rustup"),
            Some("/explicit/override".to_string()),
            "an explicit toolchain env var always wins"
        );
    }

    /// The agent-session env shape is byte-identical (ticket's "do not weaken
    /// env_clear + scratch HOME" invariant): with HOME set normally, the
    /// toolchain derivation lands on the same operator home it always did.
    #[cfg(unix)]
    #[test]
    fn toolchain_home_os_account_keeps_session_env_shape_unchanged() {
        let _guard = EnvTestGuard::engage_unsetting(&[], &["CARGO_HOME", "RUSTUP_HOME"]);
        let real_home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let env = contract_command_env(scratch.path(), None, &[]);

        if real_home.join(".rustup").is_dir() {
            assert_eq!(
                env.get("RUSTUP_HOME").map(String::as_str),
                Some(real_home.join(".rustup").display().to_string().as_str()),
                "RUSTUP_HOME still derives from the operator's real home"
            );
        }
    }

    #[test]
    fn contract_env_derives_toolchain_homes_from_the_real_home_and_cargo_runs() {
        let _guard = EnvTestGuard::engage_unsetting(&[], &["RUSTUP_HOME", "CARGO_HOME"]);
        let scratch = tempfile::tempdir().unwrap();
        let real_home = operator_home().expect("operator home");

        let env = contract_command_env(scratch.path(), None, &[]);

        // The operator's rustup toolchain remains discoverable, while Cargo's
        // config/credential home is a fresh cache-only directory.
        let rustup_home = real_home.join(".rustup");
        if rustup_home.is_dir() {
            assert_eq!(
                env.get("RUSTUP_HOME").map(String::as_str),
                Some(rustup_home.display().to_string().as_str()),
                "RUSTUP_HOME derives from the operator's real home"
            );
        }
        let cargo_home = PathBuf::from(env.get("CARGO_HOME").expect("CARGO_HOME"));
        assert!(
            cargo_home.starts_with(scratch.path()),
            "CARGO_HOME must be isolated under mission scratch: {}",
            cargo_home.display()
        );
        assert_ne!(
            cargo_home,
            real_home.join(".cargo"),
            "the operator's real Cargo home must never reach contract code"
        );

        // And cargo actually executes under the generated env: not a PATH
        // probe, a real run with HOME=scratch and the derived homes.
        let mut cmd = std::process::Command::new("cargo");
        cmd.arg("--version")
            .env_clear()
            .envs(&env)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let out = cmd.output().expect("spawn cargo --version");
        assert!(
            out.status.success(),
            "cargo --version must succeed under the generated env: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let version = String::from_utf8_lossy(&out.stdout);
        assert!(
            version.starts_with("cargo "),
            "expected a cargo version string: {version}"
        );
    }

    /// Contract env: base-sha + non-credential toolchain caches + passthrough
    /// names cross; ambient secrets do not; a passthrough entry naming a
    /// managed key — in ANY letter casing — is refused. CARGO_HOME always
    /// points at a fresh cache-only directory under mission scratch.
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
        let cargo_home = PathBuf::from(env.get("CARGO_HOME").expect("CARGO_HOME"));
        assert!(
            cargo_home.starts_with(scratch.path()),
            "CARGO_HOME must be cache-only mission scratch: {}",
            cargo_home.display()
        );
        assert_ne!(cargo_home, PathBuf::from("/poisoned/cargo-home"));
        for forbidden in ["credentials.toml", "credentials", "config.toml", "config"] {
            assert!(
                !cargo_home.join(forbidden).exists(),
                "cache-only Cargo home copied forbidden root file {forbidden}"
            );
        }
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

    /// The cache seed admits only registry/git. Root Cargo credentials and
    /// credential-provider configuration stay outside the child namespace,
    /// while cache contents remain available for offline/egress-restricted
    /// contract gates.
    #[cfg(unix)]
    #[test]
    fn contract_cargo_home_contains_caches_but_no_credentials_or_config() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("registry")).unwrap();
        std::fs::create_dir_all(source.path().join("git")).unwrap();
        std::fs::write(source.path().join("registry/cache-marker"), "registry").unwrap();
        std::fs::write(source.path().join("git/cache-marker"), "git").unwrap();
        for name in ["credentials.toml", "credentials", "config.toml", "config"] {
            std::fs::write(source.path().join(name), "operator-secret").unwrap();
        }
        let _guard = EnvTestGuard::engage(&[(
            "CARGO_HOME",
            source.path().to_str().expect("utf-8 temp path"),
        )]);
        let scratch = tempfile::tempdir().unwrap();

        let env = contract_command_env(scratch.path(), None, &[]);
        let cargo_home = PathBuf::from(env.get("CARGO_HOME").expect("CARGO_HOME"));

        for cache in ["registry", "git"] {
            assert_eq!(
                std::fs::read_to_string(cargo_home.join(cache).join("cache-marker")).unwrap(),
                cache
            );
        }
        for forbidden in ["credentials.toml", "credentials", "config.toml", "config"] {
            assert!(
                std::fs::symlink_metadata(cargo_home.join(forbidden)).is_err(),
                "cache-only Cargo home exposed {forbidden}"
            );
        }
    }

    /// 12th-pass review (P1): below the plain-copy ceiling the seeded caches
    /// are per-env COPIES — real files, never symlinks into the operator's
    /// Cargo home — so a write through the child's cache (worker-authored
    /// contract code) cannot poison the operator's shared cache for later
    /// missions and engine builds. Credential-shaped entries are excluded
    /// explicitly, even ones PLANTED inside a cache dir.
    #[cfg(unix)]
    #[test]
    fn contract_cache_cow_seeds_real_copies_and_isolates_writes() {
        let source = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(source.path().join("registry/cache")).unwrap();
        std::fs::create_dir_all(source.path().join("git/db")).unwrap();
        std::fs::write(source.path().join("registry/cache/crate-a.crate"), "aaaa").unwrap();
        std::fs::write(source.path().join("git/db/HEAD"), "ref: refs/heads/main").unwrap();
        // Credential-shaped files at the Cargo root AND planted inside the
        // cache dir itself — the copy must exclude both shapes explicitly.
        for name in ["credentials.toml", "credentials", "config.toml", "config"] {
            std::fs::write(source.path().join(name), "operator-secret").unwrap();
            std::fs::write(source.path().join("registry").join(name), "planted-secret").unwrap();
        }
        let _guard = EnvTestGuard::engage(&[(
            "CARGO_HOME",
            source.path().to_str().expect("utf-8 temp path"),
        )]);
        let scratch = tempfile::tempdir().unwrap();

        let env = contract_command_env(scratch.path(), None, &[]);
        let cargo_home = PathBuf::from(env.get("CARGO_HOME").expect("CARGO_HOME"));

        // Real copies, never links: the seeded cache dirs and their files
        // are owned by the child's home.
        for cache in ["registry", "git"] {
            let seeded = cargo_home.join(cache);
            assert!(
                !std::fs::symlink_metadata(&seeded)
                    .unwrap()
                    .file_type()
                    .is_symlink(),
                "{cache} must be seeded as a real copy, not a symlink into the operator's cache"
            );
        }
        assert_eq!(
            std::fs::read_to_string(cargo_home.join("registry/cache/crate-a.crate")).unwrap(),
            "aaaa",
            "cache contents survive the seed"
        );
        assert_eq!(
            std::fs::read_to_string(cargo_home.join("git/db/HEAD")).unwrap(),
            "ref: refs/heads/main"
        );

        // A write through the seeded cache — a new file AND an in-place
        // overwrite — never reaches the operator's source dirs (copy-on-write
        // tiers break the clone on write; the plain tier owns its bytes).
        std::fs::write(cargo_home.join("registry/cache/poisoned.crate"), "x").unwrap();
        std::fs::write(cargo_home.join("registry/cache/crate-a.crate"), "POISON").unwrap();
        assert!(
            !source.path().join("registry/cache/poisoned.crate").exists(),
            "a new file written through the seeded cache must not reach the operator's cache"
        );
        assert_eq!(
            std::fs::read_to_string(source.path().join("registry/cache/crate-a.crate")).unwrap(),
            "aaaa",
            "an overwrite through the seeded cache must not reach the operator's cache"
        );

        // Credential-shaped files never appear — neither the operator's
        // root-level ones nor the ones planted inside the cache dir.
        for forbidden in ["credentials.toml", "credentials", "config.toml", "config"] {
            assert!(
                std::fs::symlink_metadata(cargo_home.join(forbidden)).is_err(),
                "cache-only Cargo home exposed {forbidden}"
            );
            assert!(
                std::fs::symlink_metadata(cargo_home.join("registry").join(forbidden)).is_err(),
                "the copy tier smuggled a planted {forbidden} out of the cache dir"
            );
        }
    }

    /// Above the copy ceiling the seed links (the documented residual
    /// trade); at or below it the cache is always copied. The boundary is
    /// exercised through the early-exit size probe itself, so no giant
    /// fixture is needed (mirrors the validator snapshot's
    /// `pick_plain_or_fresh` split).
    #[test]
    fn contract_cache_cow_links_only_above_the_copy_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.bin"), vec![0u8; 8]).unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("nested/b.bin"), vec![0u8; 8]).unwrap();
        let probe = crate::validator_snapshot::dir_size_exceeds;
        assert!(!probe(dir.path(), 16), "exactly at the limit: copies");
        assert!(probe(dir.path(), 15), "one byte over: links");
        assert!(probe(dir.path(), 0));
        assert!(
            !probe(dir.path(), CACHE_COPY_MAX_BYTES),
            "a small cache is always copied"
        );
        // The configured ceiling is the documented per-env-cadence one
        // (64 MiB — see the constant's CI/local measurement notes).
        assert_eq!(CACHE_COPY_MAX_BYTES, 64 * 1024 * 1024);
    }
}
