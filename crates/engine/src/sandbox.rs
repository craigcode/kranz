//! OS sandbox profile/argv generation — Tier 2 filesystem/network containment.
//! See docs/scoping/worker-sandboxing.md tier 2.
//!
//! macOS uses Seatbelt (`sandbox-exec`) for filesystem isolation. Seatbelt
//! cannot express hostname egress allowlists (it accepts only `*`/`localhost`
//! network hosts), so `fs+net` on macOS restricts outbound TCP to loopback
//! and the run routes through the userspace filtering egress proxy
//! (`crate::egress_proxy`), which enforces the per-host allowlist at CONNECT
//! time. Linux uses bubblewrap for filesystem isolation; `fs+net` fails closed
//! with `--unshare-net` because bwrap alone cannot express a hostname egress
//! allowlist (and its netns cannot reach a host proxy — out of scope for v1).
//!
//! Write scope (P1, ticket sandbox-writable-scope): a session may write only
//! its working directory, its per-session private scratch root
//! (`SandboxInputs::tmpdir` — NOT the shared system temp root, which would
//! expose every sibling mission's worktrees and merge scratch), and
//! operator-declared `extraWrite` paths. The mission dir is never
//! worker-writable; its engine-owned metadata (audit log, state snapshot,
//! control inbox, transcripts) is additionally denied/masked so it stays
//! read-only even in checkout mode, where the writable `session_cwd` is an
//! ancestor of the mission dir.

use std::path::{Path, PathBuf};

/// Default egress needed by Claude/Anthropic sessions under `fs+net`.
pub const DEFAULT_EGRESS: &[&str] = &["api.anthropic.com:443", "*.anthropic.com:443"];

/// Inputs used to build a session sandbox.
#[derive(Debug, Clone)]
pub struct SandboxInputs {
    pub enforce: crate::types::SandboxEnforce,
    pub session_cwd: PathBuf,
    pub mission_dir: PathBuf,
    /// The session-PRIVATE scratch root — the only TMPDIR-side path the
    /// session may write (the cleared child env points `HOME`/`TMPDIR`
    /// under it; see `crate::agent_env`). NOT the shared system temp root:
    /// allowing all of `TMPDIR` made every sibling mission's worktree and
    /// merge scratch worker-writable (P1, ticket sandbox-writable-scope).
    /// `build_inputs` defaults it to the mission's gitignored probe scratch;
    /// the runner overrides it per session with
    /// `crate::backend_claude::scratch_home_root(session_id)`.
    pub tmpdir: PathBuf,
    pub extra_write: Vec<PathBuf>,
    pub egress: Vec<String>,
}

/// Concrete OS sandbox backend selected for this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxBackend {
    Seatbelt,
    Bubblewrap,
    /// Tier-3: run the session inside a container (see
    /// [`crate::sandbox_container`]); `ResolvedSandbox::container` is `Some`.
    Container,
}

/// How a role's `enforce` setting maps onto the current platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxDecision {
    /// Enforcement is off; no sandbox is attached.
    Off,
    /// Enforcement is requested and the platform supports it.
    Enforce(SandboxBackend),
    /// Enforcement is requested but the platform can't honor it; run-level
    /// callers must refuse rather than proceed unsandboxed.
    UnsupportedWarn,
}

fn enforce_label(enforce: crate::types::SandboxEnforce) -> &'static str {
    match enforce {
        crate::types::SandboxEnforce::Off => "off",
        crate::types::SandboxEnforce::Fs => "fs",
        crate::types::SandboxEnforce::FsNet => "fs+net",
    }
}

/// Pure decision fn: given a role's `enforce` setting and the target OS
/// (`std::env::consts::OS`-shaped string), decide whether the session gets an
/// enforced sandbox. Parameterized on `target_os` so it is testable
/// cross-platform.
pub fn platform_support(enforce: crate::types::SandboxEnforce, target_os: &str) -> SandboxDecision {
    match enforce {
        crate::types::SandboxEnforce::Off => SandboxDecision::Off,
        crate::types::SandboxEnforce::Fs if target_os == "macos" => {
            SandboxDecision::Enforce(SandboxBackend::Seatbelt)
        }
        crate::types::SandboxEnforce::FsNet if target_os == "macos" => {
            // Seatbelt's loopback-only egress profile plus the egress proxy's
            // per-host allowlist (crate::egress_proxy): the hostname rules the
            // SBPL cannot express live in the proxy, not the profile.
            SandboxDecision::Enforce(SandboxBackend::Seatbelt)
        }
        crate::types::SandboxEnforce::Fs | crate::types::SandboxEnforce::FsNet
            if target_os == "linux" =>
        {
            SandboxDecision::Enforce(SandboxBackend::Bubblewrap)
        }
        crate::types::SandboxEnforce::Fs | crate::types::SandboxEnforce::FsNet => {
            SandboxDecision::UnsupportedWarn
        }
    }
}

/// A resolved, enforced sandbox for one session.
#[derive(Debug, Clone)]
pub struct ResolvedSandbox {
    pub backend: SandboxBackend,
    pub inputs: SandboxInputs,
    /// Container runtime + image; `Some` iff `backend == Container`.
    pub container: Option<crate::sandbox_container::ContainerSpec>,
}

/// Expand a leading `~/` in `raw` using the `HOME` env var; otherwise return
/// `raw` unchanged as a `PathBuf`. `pub(crate)` so the engine-run gate wrap
/// (`crate::command_exec::resolve_gate_sandbox`) builds `extra_write` inputs
/// with the SAME expansion sessions get — never a second hand-rolled rule.
pub(crate) fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Resolve a role's sandbox config into an (optional) enforced sandbox for
/// one session, plus an optional one-time warning string.
///
/// Returns `(Some(ResolvedSandbox), None)` when enforcement is requested and
/// supported, `(None, None)` when enforcement is off, and
/// `(None, Some(warning))` when enforcement is requested but unsupported on
/// this platform.
pub fn resolve_for_session(
    role_sandbox: &crate::types::SandboxConfig,
    session_cwd: &Path,
    mission_dir: &Path,
) -> (Option<ResolvedSandbox>, Option<String>) {
    resolve_for_session_target(
        role_sandbox,
        session_cwd,
        mission_dir,
        std::env::consts::OS,
        command_available("bwrap"),
        crate::sandbox_container::detect(),
    )
}

fn resolve_for_session_target(
    role_sandbox: &crate::types::SandboxConfig,
    session_cwd: &Path,
    mission_dir: &Path,
    target_os: &str,
    bwrap_available: bool,
    container_runtime: Option<crate::sandbox_container::ContainerRuntime>,
) -> (Option<ResolvedSandbox>, Option<String>) {
    if role_sandbox.provider == crate::types::SandboxProvider::Container {
        return resolve_container_target(role_sandbox, session_cwd, mission_dir, container_runtime);
    }
    match platform_support(role_sandbox.enforce, target_os) {
        SandboxDecision::Off => (None, None),
        SandboxDecision::UnsupportedWarn => (
            None,
            Some(format!(
                "sandbox enforce:{} requested but unsupported on target_os={target_os}; refusing to run unsandboxed",
                enforce_label(role_sandbox.enforce)
            )),
        ),
        SandboxDecision::Enforce(SandboxBackend::Bubblewrap) if !bwrap_available => (
            None,
            Some(format!(
                "sandbox enforce:{} requested on linux but `bwrap` was not found; refusing to run unsandboxed",
                enforce_label(role_sandbox.enforce)
            )),
        ),
        SandboxDecision::Enforce(backend) => (
            Some(ResolvedSandbox {
                backend,
                inputs: build_inputs(role_sandbox, session_cwd, mission_dir),
                container: None,
            }),
            None,
        ),
    }
}

/// Resolve the tier-3 container provider: `enforce: off` stays unsandboxed;
/// `fs+net` with an empty egress list keeps the `--network none` hard egress
/// boundary; `fs+net` with a non-empty egress list resolves — the run routes
/// the session through the filtering egress proxy (`crate::egress_proxy`) over
/// the runtime bridge. That proxy-routed posture is advisory-only, so
/// `config::validate` refuses it (fail closed) before a mission can reach
/// this point; the resolution remains for the internal-network sidecar
/// follow-up. A requested container with no runtime on PATH is refused.
fn resolve_container_target(
    role_sandbox: &crate::types::SandboxConfig,
    session_cwd: &Path,
    mission_dir: &Path,
    runtime: Option<crate::sandbox_container::ContainerRuntime>,
) -> (Option<ResolvedSandbox>, Option<String>) {
    if role_sandbox.enforce == crate::types::SandboxEnforce::Off {
        return (None, None);
    }
    let Some(runtime) = runtime else {
        return (
            None,
            Some(
                "sandbox provider:container requested but no container runtime (docker/podman/nerdctl/container) found on PATH; refusing to run unsandboxed"
                    .to_string(),
            ),
        );
    };
    (
        Some(ResolvedSandbox {
            backend: SandboxBackend::Container,
            inputs: build_inputs(role_sandbox, session_cwd, mission_dir),
            container: Some(crate::sandbox_container::ContainerSpec {
                runtime,
                image: role_sandbox
                    .image
                    .clone()
                    .unwrap_or_else(|| crate::sandbox_container::DEFAULT_IMAGE.to_string()),
            }),
        }),
        None,
    )
}

fn build_inputs(
    role_sandbox: &crate::types::SandboxConfig,
    session_cwd: &Path,
    mission_dir: &Path,
) -> SandboxInputs {
    let extra_write = role_sandbox
        .extra_write
        .iter()
        .map(|s| expand_tilde(s))
        .collect();
    SandboxInputs {
        enforce: role_sandbox.enforce,
        session_cwd: session_cwd.to_path_buf(),
        mission_dir: mission_dir.to_path_buf(),
        // Default session-private scratch: the mission's gitignored
        // contract/sandbox scratch home — the shape the engine's own probes
        // (preflight contract commands, whose cleared env points HOME/TMPDIR
        // at `runs/contract-home`) execute under. The runner overrides this
        // per session with the session's private scratch root (see the
        // `SandboxInputs::tmpdir` doc); warn-only resolves never execute
        // under the profile, so the default is never their concern.
        tmpdir: mission_dir.join("runs").join("contract-home"),
        extra_write,
        egress: role_sandbox.egress.clone(),
    }
}

pub(crate) fn command_available(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| dir.join(name).is_file())
}

/// Absolutize a path without requiring it to exist: canonicalize if possible,
/// otherwise join it onto the current directory when relative.
pub(crate) fn absolutize(path: &Path) -> PathBuf {
    if let Ok(canon) = path.canonicalize() {
        return canon;
    }
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Make a path absolute without resolving any component. Mount destinations
/// must name the inspected leaf itself; canonicalizing a hostile symlink
/// would instead mask its target and leave the leaf replaceable.
fn lexical_absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Escape a path for embedding in an SBPL string literal.
fn escape_sbpl_literal(path: &Path) -> String {
    escape_sbpl_string(&path.to_string_lossy())
}

fn escape_sbpl_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Escape a path for embedding in an SBPL `#"..."` regex literal: every
/// regex metacharacter is backslash-escaped so the path matches literally
/// (temp-dir names carry no metacharacters in practice, but a repo root
/// might — `.` in a directory name must not become an any-char match).
/// `pub(crate)` so the engine-run gate profile (`crate::command_exec`) can
/// anchor its `xcrun_db` device-cache allow with the same escaping.
pub(crate) fn escape_sbpl_regex(path: &Path) -> String {
    let mut out = String::new();
    for ch in path.to_string_lossy().chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            c if "^.+$*?()[]{}|".contains(c) => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out
}

/// The session's writable roots: its working directory (worktree or, in
/// checkout mode, the repo root), its private scratch root, and each
/// operator-declared `extraWrite` entry. The mission dir and the shared
/// system temp root are deliberately NOT here (ticket
/// sandbox-writable-scope): the engine writes mission metadata from OUTSIDE
/// the sandbox, and whole-`TMPDIR` access made sibling missions' worktrees
/// writable. Mission metadata that would still be reachable through an
/// allowed ancestor (checkout mode: `session_cwd` is the repo root) is
/// carved back out by [`mission_write_denies`].
fn write_allowlist(inputs: &SandboxInputs) -> Vec<PathBuf> {
    let mut write_paths: Vec<PathBuf> =
        vec![absolutize(&inputs.session_cwd), absolutize(&inputs.tmpdir)];
    write_paths.extend(inputs.extra_write.iter().map(|p| absolutize(p)));
    write_paths.sort();
    write_paths.dedup();
    write_paths
}

/// The mission-metadata write-deny set: engine-owned files a sandboxed
/// session must never write even when an allowed ancestor (checkout mode's
/// `session_cwd` = repo root) would otherwise cover them. A worker that
/// could rewrite `events.jsonl` defeats the append-only audit log; one that
/// could drop files into `control/` injects control commands; one that
/// could rewrite `runs/*.jsonl` forges transcripts. Like
/// [`authority_read_deny_paths`], both the raw and canonical mission-dir
/// forms are expanded (Seatbelt matches canonical paths; the child may
/// address either form).
pub(crate) struct MissionWriteDenies {
    /// Engine-written files at the mission-dir root: literal write denies
    /// (Seatbelt) / `/dev/null` masks (bwrap).
    pub files: Vec<PathBuf>,
    /// The `control/` inbox dir: subpath write deny / tmpfs shadow.
    pub control_dirs: Vec<PathBuf>,
    /// The `runs/` transcript dirs: `runs/*.jsonl` regex write deny
    /// (Seatbelt) / per-file `/dev/null` masks for files present at spawn
    /// (bwrap). Subdirectories of `runs/` (the session scratch, the
    /// preflight worktree) stay writable — the regex never crosses `/`.
    pub runs_dirs: Vec<PathBuf>,
}

/// Engine-written files at the mission-dir root a sandboxed session must
/// never write (see [`MissionWriteDenies`]).
pub(crate) const MISSION_METADATA_FILES: &[&str] = &[
    "events.jsonl",
    "events.jsonl.lock",
    "state.json",
    "state.json.tmp",
    "estimate.json",
];

pub(crate) fn mission_write_denies(inputs: &SandboxInputs) -> MissionWriteDenies {
    let mut denies = MissionWriteDenies {
        files: Vec::new(),
        control_dirs: Vec::new(),
        runs_dirs: Vec::new(),
    };
    for mission_dir in [inputs.mission_dir.clone(), absolutize(&inputs.mission_dir)] {
        for name in MISSION_METADATA_FILES {
            denies.files.push(mission_dir.join(name));
        }
        denies.control_dirs.push(mission_dir.join("control"));
        denies.runs_dirs.push(mission_dir.join("runs"));
    }
    denies
}

/// Authority files a sandboxed session must never read, even under the broad
/// read allow: a read of `serve.token` IS mutation authority over `kranz
/// serve` (loopback is reachable from every sandbox tier), `serve.read.token`
/// is its GET-side sibling, and `config.json` carries Slack tokens and
/// remote-workspace credentials. Derived from the mission dir's canonical
/// `<repo>/.kranz/missions/<id>` layout. Both the raw and the canonical
/// mission-dir forms are expanded (the dir exists at spawn time even when the
/// token files do not yet), because Seatbelt matches against canonical paths
/// — the same `/var` ↔ `/private/var` split the write allowlist handles.
///
/// Also denied: `$CARGO_HOME/credentials.toml` AND the legacy extensionless
/// `$CARGO_HOME/credentials` (or `~/.cargo/...` when CARGO_HOME is unset) —
/// CARGO_HOME crosses into child envs for registry-cache locality
/// ([`crate::agent_env`]), but cargo reads BOTH filenames for registry auth
/// tokens (the legacy one is still supported and takes precedence where
/// present), so both are the same credential class as the serve token.
pub(crate) fn authority_read_deny_paths(inputs: &SandboxInputs) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for mission_dir in [inputs.mission_dir.clone(), absolutize(&inputs.mission_dir)] {
        if let Some(kranz_dir) = mission_dir.parent().and_then(Path::parent) {
            for name in ["serve.token", "serve.read.token", "config.json"] {
                paths.push(kranz_dir.join(name));
            }
        }
    }
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")));
    if let Some(cargo_home) = cargo_home {
        for base in [cargo_home.clone(), absolutize(&cargo_home)] {
            paths.push(base.join("credentials.toml"));
            paths.push(base.join("credentials"));
        }
    }
    paths
}

/// Default Anthropic egress plus mission-configured additions, trimmed and
/// de-duplicated in stable order.
pub fn effective_egress(configured: &[String]) -> Vec<String> {
    let mut out: Vec<String> = DEFAULT_EGRESS.iter().map(|s| (*s).to_string()).collect();
    for item in configured {
        let item = item.trim();
        if !item.is_empty() && !out.iter().any(|existing| existing == item) {
            out.push(item.to_string());
        }
    }
    out
}

/// Generate an SBPL profile: deny-by-default, broad read (Seatbelt cannot
/// usefully scope toolchain/dyld reads without breaking `/bin/sh`) with the
/// [`authority_read_deny_paths`] carve-out, write limited to subpaths of
/// `session_cwd`, the session-private scratch `tmpdir`, and each
/// `extra_write` entry — with the mission metadata of
/// [`mission_write_denies`] carved back OUT by explicit write denies, so the
/// audit log / state snapshot / control inbox / transcripts stay read-only
/// to the session even in checkout mode (where `session_cwd` is the repo
/// root and the mission dir sits under it). `fs` allows network — the profile wraps the agent
/// binary itself, so denying egress bricks Anthropic/API sessions; write
/// containment is the fs-tier promise. `fs+net` restricts outbound TCP to
/// loopback: Seatbelt rejects hostname egress rules (`host must be * or
/// localhost`), so the per-host allowlist is enforced by the run's egress
/// proxy (`crate::egress_proxy`) — the only reachable way out.
pub fn generate_profile(inputs: &SandboxInputs) -> String {
    let write_paths = write_allowlist(inputs);

    let mut profile = String::new();
    profile.push_str("(version 1)\n");
    profile.push_str("(deny default)\n");
    profile.push('\n');
    profile.push_str("(allow process*)\n");
    profile.push_str("(allow signal (target self))\n");
    profile.push_str("(allow sysctl-read)\n");
    profile.push_str("(allow mach-lookup)\n");
    profile.push_str("(allow mach-register)\n");
    profile.push_str("(allow iokit-open)\n");
    profile.push('\n');
    // Reads stay broad: Seatbelt cannot usefully express "toolchain + dyld +
    // locale" without a long allowlist that still breaks `/bin/sh` redirects.
    // Secrecy is not the fs-tier promise — write containment is — with ONE
    // carve-out: the authority material below.
    profile.push_str("(allow file-read*)\n");
    profile.push('\n');
    // Serve tokens and the repo config must stay unreadable even under the
    // broad read allow (see authority_read_deny_paths). SBPL denies take
    // precedence over allows regardless of clause order (verified with
    // sandbox-exec), so placing the deny after the allow is documentary.
    let mut deny_literals = std::collections::BTreeSet::new();
    for path in authority_read_deny_paths(inputs) {
        deny_literals.insert(escape_sbpl_literal(&path));
    }
    if !deny_literals.is_empty() {
        profile.push_str("(deny file-read*\n");
        for lit in &deny_literals {
            profile.push_str(&format!("  (literal \"{lit}\")\n"));
        }
        profile.push_str(")\n");
        profile.push('\n');
    }
    match inputs.enforce {
        crate::types::SandboxEnforce::FsNet => {
            // Loopback-only egress: the session's proxy hops (CONNECT to
            // 127.0.0.1) are legal, and every non-localhost destination is
            // denied here at the kernel boundary — the egress proxy is the
            // only way out and applies the hostname allowlist.
            profile.push_str("(allow network-outbound (remote tcp \"localhost:*\"))\n");
        }
        // `fs` (and Off) must allow network: this profile wraps the agent
        // binary, so `deny network*` bricks API egress. Egress restriction
        // is an `fs+net` concern.
        crate::types::SandboxEnforce::Fs | crate::types::SandboxEnforce::Off => {
            profile.push_str("(allow network*)\n");
        }
    }
    profile.push('\n');
    // Write allowlist: include both the canonical path and the path as given
    // (macOS `/var` ↔ `/private/var`) so shell redirects using either form match.
    profile.push_str("(allow file-write*\n");
    let mut write_literals = std::collections::BTreeSet::new();
    for p in &write_paths {
        write_literals.insert(escape_sbpl_literal(p));
    }
    for raw in [&inputs.session_cwd, &inputs.tmpdir]
        .into_iter()
        .chain(inputs.extra_write.iter())
    {
        write_literals.insert(escape_sbpl_literal(raw));
        write_literals.insert(escape_sbpl_literal(&absolutize(raw)));
    }
    for lit in &write_literals {
        profile.push_str(&format!("  (subpath \"{lit}\")\n"));
    }
    profile.push_str(")\n");
    profile.push('\n');
    // Mission metadata write deny: the allowlist no longer names the mission
    // dir, but in checkout mode `session_cwd` IS the repo root and the
    // mission dir sits under it — without these denies the audit log, state
    // snapshot, control inbox, and transcripts would be writable through the
    // session-cwd subpath allow. SBPL denies take precedence over allows
    // regardless of clause order (the same guarantee the read deny above
    // relies on), so placement after the allow is documentary.
    let write_denies = mission_write_denies(inputs);
    profile.push_str("(deny file-write*\n");
    let mut deny_literals = std::collections::BTreeSet::new();
    for p in &write_denies.files {
        deny_literals.insert(escape_sbpl_literal(p));
    }
    for lit in &deny_literals {
        profile.push_str(&format!("  (literal \"{lit}\")\n"));
    }
    let mut deny_subpaths = std::collections::BTreeSet::new();
    for p in &write_denies.control_dirs {
        deny_subpaths.insert(escape_sbpl_literal(p));
    }
    for lit in &deny_subpaths {
        profile.push_str(&format!("  (subpath \"{lit}\")\n"));
    }
    // Transcripts are `runs/*.jsonl` files directly under the runs dir; the
    // `[^/]*` keeps `runs/` SUBDIRECTORIES (session scratch, preflight
    // worktree) writable.
    let mut deny_regexes = std::collections::BTreeSet::new();
    for p in &write_denies.runs_dirs {
        deny_regexes.insert(escape_sbpl_regex(p));
    }
    for lit in &deny_regexes {
        profile.push_str(&format!("  (regex #\"^{lit}/[^/]*\\.jsonl$\")\n"));
    }
    profile.push_str(")\n");

    profile
}

/// Build the bubblewrap argv tail for running `binary args` under the resolved
/// sandbox. The caller uses program `bwrap` and passes this vector as args.
///
/// Write scope mirrors the Seatbelt profile: the whole filesystem is bound
/// read-only, then `session_cwd`, the session-private scratch `tmpdir`, and
/// each `extra_write` entry are bound writable — the mission dir and the
/// shared system temp root are NOT writable (ticket sandbox-writable-scope).
/// Mission metadata that an rw ancestor bind would otherwise cover (checkout
/// mode) is masked back out, the bwrap analogue of the profile's write deny.
pub fn bubblewrap_args(
    inputs: &SandboxInputs,
    binary: &Path,
    args: &[String],
) -> crate::error::Result<Vec<String>> {
    let mut out = vec![
        "--die-with-parent".to_string(),
        "--ro-bind".to_string(),
        "/".to_string(),
        "/".to_string(),
        "--dev".to_string(),
        "/dev".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
    ];
    if inputs.enforce == crate::types::SandboxEnforce::FsNet {
        out.push("--unshare-net".to_string());
    }
    for path in write_allowlist(inputs) {
        let path = path.display().to_string();
        out.push("--bind".to_string());
        out.push(path.clone());
        out.push(path);
    }
    // bwrap has no per-path read deny, so mask the authority material the
    // whole-fs ro-bind would otherwise expose: a /dev/null bind stacked over
    // each file that exists at spawn time (later binds win; the destination
    // must exist, hence the filter). A token file created AFTER spawn stays
    // a residual gap the Seatbelt profile does not have.
    let masks: std::collections::BTreeSet<String> = authority_read_deny_paths(inputs)
        .iter()
        .filter(|path| path.exists())
        .map(|path| absolutize(path).display().to_string())
        .collect();
    for mask in masks {
        out.push("--ro-bind".to_string());
        out.push("/dev/null".to_string());
        out.push(mask);
    }
    // Mission metadata must stay unwritable inside the sandbox: in checkout
    // mode the rw `session_cwd` bind covers the mission dir, so the audit
    // log, state snapshot, control inbox, and transcripts need the same
    // spawn-time masking the authority material gets (bwrap has no per-path
    // write deny to stack over an rw bind). Files are masked with /dev/null
    // binds; the control dir gets an empty tmpfs shadow (writes evaporate,
    // the host dir is untouched). Same residual gap as the authority masks:
    // a metadata file created AFTER spawn is unmasked until the next
    // session — the engine creates all of these before the first session,
    // EXCEPT `state.json.tmp`, which is transient (recreated on every atomic
    // state write and routinely absent at spawn). Create just that one so
    // the mask binds — an empty placeholder is inert: the engine truncates
    // it on use and nothing ever reads it. (The other metadata files are NOT
    // pre-created here: an empty `state.json` would turn a clean NotFound
    // into a parse error for first-run flows.) A creation failure is NOT
    // best-effort: proceeding would silently leave the metadata writable —
    // fail the spawn instead (3rd-pass review).
    let write_denies = mission_write_denies(inputs);
    for path in &write_denies.files {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_file() => {}
            Ok(_) => {
                return Err(crate::error::EngineError::InvalidState(format!(
                    "bwrap mask prep: {} exists and is not a regular file",
                    path.display()
                )))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    for path in write_denies
        .files
        .iter()
        .filter(|p| p.ends_with("state.json.tmp"))
    {
        let metadata = std::fs::symlink_metadata(path);
        match metadata {
            // A regular file already there: mask binds over it as-is.
            Ok(m) if m.file_type().is_file() => continue,
            // Anything else present (symlink, dir, fifo, device): refuse.
            // In particular, accepting a symlink and later canonicalizing it
            // masks the target rather than the state.json.tmp mount point.
            Ok(_) => {
                return Err(crate::error::EngineError::InvalidState(format!(
                    "bwrap mask prep: {} exists and is not a regular file",
                    path.display()
                )));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        // Pin the mission parent from the trusted repository anchor, then
        // create the leaf relative to that capability with no-follow. This
        // closes both the hostile-parent canonicalization gap and the leaf
        // swap between metadata and open.
        let (parent, name) = crate::paths::open_parent_nofollow(path)?;
        use cap_fs_ext::OpenOptionsFollowExt as _;
        use cap_primitives::fs::FollowSymlinks;
        let mut options = cap_std::fs::OpenOptions::new();
        options
            .create(true)
            .write(true)
            .truncate(false)
            .follow(FollowSymlinks::No);
        let open_result = parent.open_with(name, &options);
        open_result.map_err(|e| {
            crate::error::EngineError::Io(std::io::Error::new(
                e.kind(),
                format!("bwrap mask prep: create {}: {e}", path.display()),
            ))
        })?;
    }
    let mut write_masks: std::collections::BTreeSet<String> = write_denies
        .files
        .iter()
        .filter(|path| {
            std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_file())
        })
        .map(|path| lexical_absolute(path).display().to_string())
        .collect();
    // Transcripts: bwrap cannot express the Seatbelt `runs/*.jsonl` regex,
    // so mask each transcript file present at spawn individually. The sweep
    // is shallow — `runs/` subdirectories (session scratch, preflight
    // worktree) are not transcripts and stay as their binds left them.
    for runs_dir in &write_denies.runs_dirs {
        if let Ok(entries) = std::fs::read_dir(runs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if entry.file_type().is_ok_and(|kind| kind.is_file())
                    && path.extension().is_some_and(|ext| ext == "jsonl")
                {
                    write_masks.insert(lexical_absolute(&path).display().to_string());
                }
            }
        }
    }
    for mask in write_masks {
        out.push("--ro-bind".to_string());
        out.push("/dev/null".to_string());
        out.push(mask);
    }
    let control_shadows: std::collections::BTreeSet<String> = write_denies
        .control_dirs
        .iter()
        .filter(|path| path.is_dir())
        .map(|path| absolutize(path).display().to_string())
        .collect();
    for shadow in control_shadows {
        out.push("--tmpfs".to_string());
        out.push(shadow);
    }
    out.push("--chdir".to_string());
    out.push(absolutize(&inputs.session_cwd).display().to_string());
    out.push("--".to_string());
    out.push(binary.display().to_string());
    out.extend(args.iter().cloned());
    Ok(out)
}

/// Write the profile to a uniquely-named file under `dir`, returning its path.
pub fn write_profile_file(dir: &Path, profile: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("kranz-sandbox-{}.sb", uuid::Uuid::new_v4()));
    std::fs::write(&path, profile)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    use std::sync::Mutex;

    #[cfg(target_os = "macos")]
    static SANDBOX_EXEC_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[cfg(target_os = "macos")]
    fn sandbox_exec_can_apply() -> bool {
        let found = std::process::Command::new("which")
            .arg("sandbox-exec")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !found {
            eprintln!("sandbox-exec not found on this host; skipping");
            return false;
        }

        let smoke = std::process::Command::new("sandbox-exec")
            .arg("-p")
            .arg("(version 1)\n(allow default)\n")
            .arg("/usr/bin/true")
            .output();
        match smoke {
            Ok(output) if output.status.success() => true,
            Ok(output) => {
                eprintln!(
                    "sandbox-exec cannot apply a smoke profile on this host; skipping: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                false
            }
            Err(e) => {
                eprintln!("sandbox-exec smoke probe failed; skipping: {e}");
                false
            }
        }
    }

    #[cfg(target_os = "linux")]
    fn bwrap_can_apply() -> bool {
        if !command_available("bwrap") {
            eprintln!("bwrap not found on this host; skipping");
            return false;
        }

        let smoke = std::process::Command::new("bwrap")
            .args([
                "--die-with-parent",
                "--ro-bind",
                "/",
                "/",
                "--dev",
                "/dev",
                "--proc",
                "/proc",
                "--",
                "/bin/true",
            ])
            .output();
        match smoke {
            Ok(output) if output.status.success() => true,
            Ok(output) => {
                eprintln!(
                    "bwrap cannot apply a smoke sandbox on this host; skipping: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                false
            }
            Err(e) => {
                eprintln!("bwrap smoke probe failed; skipping: {e}");
                false
            }
        }
    }

    fn inputs(
        session_cwd: &Path,
        mission_dir: &Path,
        tmpdir: &Path,
        extra: Vec<PathBuf>,
    ) -> SandboxInputs {
        SandboxInputs {
            enforce: crate::types::SandboxEnforce::Fs,
            session_cwd: session_cwd.to_path_buf(),
            mission_dir: mission_dir.to_path_buf(),
            tmpdir: tmpdir.to_path_buf(),
            extra_write: extra,
            egress: Vec::new(),
        }
    }

    #[test]
    fn sandbox_profile_contains_required_clauses() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(
            session.path(),
            mission.path(),
            tmp.path(),
            vec![extra.path().to_path_buf()],
        ));

        assert!(profile.contains("(version 1)"));
        assert!(profile.contains("(deny default)"));
        assert!(profile.contains("(allow file-read*)"));
        // `fs` must allow network so the sandboxed agent can reach its API.
        assert!(profile.contains("(allow network*)"));
        assert!(!profile.contains("(deny network*)"));

        let session_abs = absolutize(session.path());
        let tmp_abs = absolutize(tmp.path());
        let extra_abs = absolutize(extra.path());

        // The writable set: session cwd, the session-private scratch, and
        // each extraWrite entry.
        for p in [&session_abs, &tmp_abs, &extra_abs] {
            let expected = format!("(subpath \"{}\")", escape_sbpl_literal(p));
            assert!(
                profile.contains(&expected),
                "profile missing subpath rule for {:?}:\n{}",
                p,
                profile
            );
        }

        // The mission dir is NOT writable (its engine-owned metadata carries
        // explicit write denies instead — see
        // sandbox_profile_denies_mission_metadata_writes).
        let mission_abs = absolutize(mission.path());
        let mission_rule = format!("(subpath \"{}\")", escape_sbpl_literal(&mission_abs));
        assert!(
            !profile.contains(&mission_rule),
            "profile must not allow writes to the whole mission dir:\n{profile}"
        );
    }

    #[test]
    fn sandbox_profile_denies_authority_material_reads() {
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let tmp = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(repo.path(), &mission, tmp.path(), vec![]));

        // Broad reads stay, with the authority material carved out by explicit
        // denies (SBPL denies take precedence over the allow).
        assert!(profile.contains("(allow file-read*)"));
        assert!(profile.contains("(deny file-read*"));
        let kranz_dir = repo.path().join(".kranz");
        for name in ["serve.token", "serve.read.token", "config.json"] {
            for base in [kranz_dir.clone(), absolutize(&kranz_dir)] {
                let expected = format!("(literal \"{}\")", escape_sbpl_literal(&base.join(name)));
                assert!(
                    profile.contains(&expected),
                    "profile missing read deny for {}:\n{profile}",
                    base.join(name).display()
                );
            }
        }
        // The cargo registry token file is denied too (CARGO_HOME crosses
        // into child envs for cache locality; its credentials must not
        // ride along).
        assert!(
            profile.contains("credentials.toml"),
            "profile missing read deny for cargo credentials:\n{profile}"
        );
    }

    #[test]
    fn bubblewrap_args_mask_authority_material_with_dev_null() {
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let serve_token = repo.path().join(".kranz").join("serve.token");
        std::fs::write(&serve_token, "secret").unwrap();

        let args = bubblewrap_args(
            &inputs(repo.path(), &mission, tmp.path(), vec![]),
            Path::new("/usr/bin/claude"),
            &[],
        )
        .unwrap();
        let joined = args.join(" ");

        let expected = format!("--ro-bind /dev/null {}", absolutize(&serve_token).display());
        assert!(
            joined.contains(&expected),
            "missing /dev/null mask for serve.token: {args:?}"
        );
        // Absent files are not masked — bwrap requires the destination to exist.
        assert!(
            !joined.contains("serve.read.token"),
            "absent authority files must not be masked: {args:?}"
        );
    }

    #[test]
    fn sandbox_profile_denies_mission_metadata_writes() {
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let scratch = tempfile::tempdir().unwrap();

        // Checkout-mode shape: the session cwd is the repo root, an ANCESTOR
        // of the mission dir — without explicit write denies the audit log,
        // state snapshot, control inbox, and transcripts would be writable
        // through the session-cwd subpath allow.
        let profile = generate_profile(&inputs(repo.path(), &mission, scratch.path(), vec![]));

        assert!(
            profile.contains("(deny file-write*"),
            "missing write deny block:\n{profile}"
        );
        for name in MISSION_METADATA_FILES {
            for base in [mission.clone(), absolutize(&mission)] {
                let expected = format!("(literal \"{}\")", escape_sbpl_literal(&base.join(name)));
                assert!(
                    profile.contains(&expected),
                    "profile missing write deny for {}:\n{profile}",
                    base.join(name).display()
                );
            }
        }
        for base in [mission.clone(), absolutize(&mission)] {
            let control = format!(
                "(subpath \"{}\")",
                escape_sbpl_literal(&base.join("control"))
            );
            assert!(
                profile.contains(&control),
                "profile missing control/ write deny:\n{profile}"
            );
            let runs = format!(
                "(regex #\"^{}/[^/]*\\.jsonl$\")",
                escape_sbpl_regex(&base.join("runs"))
            );
            assert!(
                profile.contains(&runs),
                "profile missing transcript write deny:\n{profile}"
            );
        }
        // The mission dir root itself is not in the allow set.
        let mission_rule = format!(
            "(subpath \"{}\")",
            escape_sbpl_literal(&absolutize(&mission))
        );
        assert!(
            !profile.contains(&mission_rule),
            "the whole mission dir must not be writable:\n{profile}"
        );
    }

    #[test]
    fn sandbox_profile_keeps_sibling_temp_neighbors_unwritable() {
        // The worktree-mode layout the finding named: integration/feature
        // worktrees for ALL missions sit side by side under the shared temp
        // root. The session's own worktree + private scratch must be
        // writable; the sibling mission's worktree, the sibling's scratch,
        // and the shared temp root itself must not.
        let root = tempfile::tempdir().unwrap();
        let session = root.path().join("kranz-wt-aaa-m1-f-1-1");
        let scratch = root.path().join("kranz-worker-home-sess-1");
        let sibling = root.path().join("kranz-wt-bbb-m2-_integration");
        let sibling_scratch = root.path().join("kranz-worker-home-sess-2");
        for d in [&session, &scratch, &sibling, &sibling_scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        let mission = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(&session, mission.path(), &scratch, vec![]));

        for allowed in [&session, &scratch] {
            let expected = format!(
                "(subpath \"{}\")",
                escape_sbpl_literal(&absolutize(allowed))
            );
            assert!(
                profile.contains(&expected),
                "profile missing allow for {}:\n{profile}",
                allowed.display()
            );
        }
        for denied in [&sibling, &sibling_scratch, &root.path().to_path_buf()] {
            let rule = format!("(subpath \"{}\")", escape_sbpl_literal(&absolutize(denied)));
            assert!(
                !profile.contains(&rule),
                "{} must not be writable:\n{profile}",
                denied.display()
            );
        }
    }

    #[test]
    fn bubblewrap_args_mask_mission_metadata() {
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        let runs = mission.join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        // Engine-owned metadata present at spawn.
        let events = mission.join("events.jsonl");
        let state = mission.join("state.json");
        let transcript = runs.join("run-1.jsonl");
        let denials = runs.join("egress-denials.jsonl");
        for f in [&events, &state, &transcript, &denials] {
            std::fs::write(f, "engine").unwrap();
        }
        let control = mission.join("control");
        std::fs::create_dir_all(&control).unwrap();
        // A runs/ SUBDIRECTORY of session scratch: its jsonl files are not
        // transcripts and must NOT be masked.
        let contract_home = runs.join("contract-home");
        std::fs::create_dir_all(&contract_home).unwrap();
        let scratch_jsonl = contract_home.join("notes.jsonl");
        std::fs::write(&scratch_jsonl, "session").unwrap();

        let args = bubblewrap_args(
            &inputs(repo.path(), &mission, scratch.path(), vec![]),
            Path::new("/usr/bin/claude"),
            &[],
        )
        .unwrap();
        let joined = args.join(" ");

        for f in [&events, &state, &transcript, &denials] {
            let expected = format!("--ro-bind /dev/null {}", absolutize(f).display());
            assert!(
                joined.contains(&expected),
                "missing /dev/null mask for {}: {args:?}",
                f.display()
            );
        }
        let control_abs = absolutize(&control).display().to_string();
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--tmpfs" && w[1] == control_abs),
            "missing tmpfs shadow for control/: {args:?}"
        );
        // Absent metadata files are not masked (bwrap needs the destination
        // to exist).
        assert!(
            !joined.contains("estimate.json"),
            "absent metadata files must not be masked: {args:?}"
        );
        // No rw bind of the mission dir, and runs/-subdir scratch files stay
        // unmasked.
        let mission_abs = absolutize(&mission);
        assert!(
            !joined.contains(&format!("--bind {0} {0}", mission_abs.display())),
            "mission dir must not be rw-bound: {args:?}"
        );
        assert!(
            !joined.contains(&scratch_jsonl.display().to_string()),
            "runs/ subdir scratch files must not be masked: {args:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn bubblewrap_mask_prep_rejects_preexisting_state_tmp_symlink() {
        use std::os::unix::fs::symlink;
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(mission.join("runs")).unwrap();
        let target_dir = tempfile::tempdir().unwrap();
        let target = target_dir.path().join("outside");
        std::fs::write(&target, "unchanged").unwrap();
        symlink(&target, mission.join("state.json.tmp")).unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let error = bubblewrap_args(
            &inputs(repo.path(), &mission, scratch.path(), vec![]),
            Path::new("/usr/bin/claude"),
            &[],
        )
        .expect_err("a symlink cannot become a bwrap mask mount point");

        assert!(error.to_string().contains("not a regular file"), "{error}");
        assert_eq!(std::fs::read_to_string(target).unwrap(), "unchanged");
        assert!(
            std::fs::symlink_metadata(mission.join("state.json.tmp"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "mask preparation must not replace or follow the hostile leaf"
        );
    }

    #[test]
    fn sandbox_profile_excludes_paths_outside_allowlist() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let outsider = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(session.path(), mission.path(), tmp.path(), vec![]));

        let outsider_abs = absolutize(outsider.path());
        let forbidden = format!("(subpath \"{}\")", escape_sbpl_literal(&outsider_abs));
        assert!(
            !profile.contains(&forbidden),
            "profile unexpectedly allows write to path outside the allowlist"
        );
    }

    #[test]
    fn sandbox_profile_fs_net_restricts_egress_to_loopback() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut inputs = inputs(session.path(), mission.path(), tmp.path(), vec![]);
        inputs.enforce = crate::types::SandboxEnforce::FsNet;
        inputs.egress = vec!["crates.io:443".into(), "api.anthropic.com:443".into()];

        let profile = generate_profile(&inputs);

        // Seatbelt rejects hostname egress rules, so the profile cuts outbound
        // TCP to loopback only; the per-host allowlist (including the
        // configured entries above) is the egress proxy's job, not the SBPL's.
        assert!(!profile.contains("(allow network*)"));
        assert!(profile.contains("(allow network-outbound (remote tcp \"localhost:*\"))"));
        assert!(
            !profile.contains("crates.io") && !profile.contains("anthropic.com"),
            "no per-host egress rules in the profile:\n{profile}"
        );
    }

    #[test]
    fn bubblewrap_args_bind_write_roots_and_unshare_network_for_fs_net() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let mut inputs = inputs(
            session.path(),
            mission.path(),
            tmp.path(),
            vec![extra.path().to_path_buf()],
        );
        inputs.enforce = crate::types::SandboxEnforce::FsNet;

        let args =
            bubblewrap_args(&inputs, Path::new("/usr/bin/claude"), &["--print".into()]).unwrap();
        let joined = args.join(" ");

        assert!(args.contains(&"--unshare-net".to_string()));
        for path in [
            absolutize(session.path()),
            absolutize(tmp.path()),
            absolutize(extra.path()),
        ] {
            assert!(
                joined.contains(&format!("--bind {0} {0}", path.display())),
                "bubblewrap args missing bind for {}: {args:?}",
                path.display()
            );
        }
        // The mission dir is bound read-only via the whole-fs ro-bind only —
        // never re-bound writable.
        let mission_abs = absolutize(mission.path());
        assert!(
            !joined.contains(&format!("--bind {0} {0}", mission_abs.display())),
            "bubblewrap args must not rw-bind the mission dir: {args:?}"
        );
        assert!(joined.contains("--ro-bind / /"));
        assert!(joined.ends_with("/usr/bin/claude --print"));
    }

    #[test]
    fn sandbox_profile_write_profile_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let profile = "(version 1)\n(deny default)\n";
        let path = write_profile_file(dir.path(), profile).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), profile);
        assert!(path.starts_with(dir.path()));
    }

    #[test]
    fn sandbox_platform_support_matrix() {
        use crate::types::SandboxEnforce;

        assert_eq!(
            platform_support(SandboxEnforce::Off, "macos"),
            SandboxDecision::Off
        );
        assert_eq!(
            platform_support(SandboxEnforce::Fs, "macos"),
            SandboxDecision::Enforce(SandboxBackend::Seatbelt)
        );
        assert_eq!(
            platform_support(SandboxEnforce::Fs, "linux"),
            SandboxDecision::Enforce(SandboxBackend::Bubblewrap)
        );
        assert_eq!(
            platform_support(SandboxEnforce::FsNet, "macos"),
            SandboxDecision::Enforce(SandboxBackend::Seatbelt)
        );
        assert_eq!(
            platform_support(SandboxEnforce::FsNet, "linux"),
            SandboxDecision::Enforce(SandboxBackend::Bubblewrap)
        );
        assert_eq!(
            platform_support(SandboxEnforce::Off, "linux"),
            SandboxDecision::Off
        );
    }

    #[test]
    fn sandbox_resolve_off_yields_none() {
        let cfg = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::Off,
            provider: crate::types::SandboxProvider::Process,
            image: None,
            extra_write: vec![],
            egress: vec![],
        };
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let (resolved, warn) = resolve_for_session(&cfg, session.path(), mission.path());
        assert!(resolved.is_none());
        assert!(warn.is_none());
    }

    #[test]
    fn sandbox_resolve_linux_requires_bwrap() {
        let cfg = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::Fs,
            provider: crate::types::SandboxProvider::Process,
            image: None,
            extra_write: vec![],
            egress: vec![],
        };
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let (resolved, warn) =
            resolve_for_session_target(&cfg, session.path(), mission.path(), "linux", false, None);
        assert!(resolved.is_none());
        assert!(
            warn.unwrap().contains("bwrap"),
            "missing-bwrap warning should name bwrap"
        );

        let (resolved, warn) =
            resolve_for_session_target(&cfg, session.path(), mission.path(), "linux", true, None);
        assert!(warn.is_none());
        assert_eq!(
            resolved.expect("bwrap present").backend,
            SandboxBackend::Bubblewrap
        );
    }

    fn container_cfg(
        enforce: crate::types::SandboxEnforce,
        egress: Vec<String>,
    ) -> crate::types::SandboxConfig {
        crate::types::SandboxConfig {
            enforce,
            provider: crate::types::SandboxProvider::Container,
            image: None,
            extra_write: vec![],
            egress,
        }
    }

    #[test]
    fn container_provider_off_stays_unsandboxed() {
        let cfg = container_cfg(crate::types::SandboxEnforce::Off, vec![]);
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let (resolved, warn) =
            resolve_for_session_target(&cfg, session.path(), mission.path(), "macos", false, None);
        assert!(resolved.is_none());
        assert!(warn.is_none());
    }

    #[test]
    fn container_provider_without_runtime_fails_closed() {
        let cfg = container_cfg(crate::types::SandboxEnforce::Fs, vec![]);
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let (resolved, warn) =
            resolve_for_session_target(&cfg, session.path(), mission.path(), "linux", false, None);
        assert!(resolved.is_none());
        let warn = warn.expect("missing runtime must produce a warning");
        assert!(warn.contains("provider:container"), "{warn}");
        assert!(warn.contains("docker/podman/nerdctl/container"), "{warn}");
        assert!(warn.contains("refusing to run unsandboxed"), "{warn}");
    }

    #[test]
    fn container_provider_fs_net_with_egress_list_resolves_for_the_proxy() {
        let cfg = container_cfg(
            crate::types::SandboxEnforce::FsNet,
            vec!["crates.io:443".to_string()],
        );
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        // fs+net with a per-host egress list still RESOLVES — the run routes
        // the session through the filtering egress proxy over the runtime
        // bridge (crate::egress_proxy) — but that posture is advisory-only,
        // so `config::validate` refuses it (fail closed) before a mission can
        // reach this point. The resolution remains for the internal-network
        // sidecar follow-up.
        let (resolved, warn) = resolve_for_session_target(
            &cfg,
            session.path(),
            mission.path(),
            "linux",
            false,
            Some(crate::sandbox_container::ContainerRuntime::Docker),
        );
        assert!(warn.is_none(), "{warn:?}");
        let resolved = resolved.expect("container fs+net with egress must resolve");
        assert_eq!(resolved.backend, SandboxBackend::Container);
        assert_eq!(resolved.inputs.egress, vec!["crates.io:443".to_string()]);
    }

    #[test]
    fn container_provider_resolves_runtime_and_image() {
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        // Default image when config names none.
        let cfg = container_cfg(crate::types::SandboxEnforce::FsNet, vec![]);
        let (resolved, warn) = resolve_for_session_target(
            &cfg,
            session.path(),
            mission.path(),
            "macos",
            false,
            Some(crate::sandbox_container::ContainerRuntime::Podman),
        );
        assert!(warn.is_none());
        let resolved = resolved.expect("runtime present and policy supportable");
        assert_eq!(resolved.backend, SandboxBackend::Container);
        let container = resolved.container.expect("container spec must be set");
        assert_eq!(
            container.runtime,
            crate::sandbox_container::ContainerRuntime::Podman
        );
        assert_eq!(container.image, crate::sandbox_container::DEFAULT_IMAGE);

        // Configured image overrides the default.
        let mut cfg = container_cfg(crate::types::SandboxEnforce::Fs, vec![]);
        cfg.image = Some("ghcr.io/example/kranz-worker:1".to_string());
        let (resolved, warn) = resolve_for_session_target(
            &cfg,
            session.path(),
            mission.path(),
            "macos",
            false,
            Some(crate::sandbox_container::ContainerRuntime::Docker),
        );
        assert!(warn.is_none());
        assert_eq!(
            resolved
                .expect("runtime present")
                .container
                .expect("container spec")
                .image,
            "ghcr.io/example/kranz-worker:1"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_resolve_fs_on_macos_yields_resolved_sandbox() {
        let cfg = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::Fs,
            provider: crate::types::SandboxProvider::Process,
            image: None,
            extra_write: vec![],
            egress: vec![],
        };
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let (resolved, warn) = resolve_for_session(&cfg, session.path(), mission.path());
        assert!(warn.is_none());
        let resolved = resolved.expect("expected an enforced sandbox on macos");
        assert_eq!(resolved.backend, SandboxBackend::Seatbelt);
        assert_eq!(resolved.inputs.session_cwd, session.path());
        assert_eq!(resolved.inputs.mission_dir, mission.path());
        assert!(!resolved.inputs.tmpdir.as_os_str().is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_resolve_expands_tilde_extra_write_via_home() {
        let cfg = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::Fs,
            provider: crate::types::SandboxProvider::Process,
            image: None,
            extra_write: vec!["~/.cargo".to_string()],
            egress: vec![],
        };
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let home = std::env::var("HOME").expect("HOME must be set to run this test");

        let (resolved, _warn) = resolve_for_session(&cfg, session.path(), mission.path());
        let resolved = resolved.expect("expected an enforced sandbox on macos");
        assert_eq!(
            resolved.inputs.extra_write,
            vec![PathBuf::from(home).join(".cargo")]
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_resolve_fs_net_on_macos_yields_seatbelt_with_loopback_profile() {
        let cfg = crate::types::SandboxConfig {
            enforce: crate::types::SandboxEnforce::FsNet,
            provider: crate::types::SandboxProvider::Process,
            image: None,
            extra_write: vec![],
            egress: vec![],
        };
        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();

        let (resolved, warn) = resolve_for_session(&cfg, session.path(), mission.path());

        assert!(warn.is_none(), "fs+net on macOS resolves: {warn:?}");
        let resolved = resolved.expect("fs+net on macOS resolves to Seatbelt");
        assert_eq!(resolved.backend, SandboxBackend::Seatbelt);
        let profile = generate_profile(&resolved.inputs);
        assert!(
            profile.contains("(allow network-outbound (remote tcp \"localhost:*\"))"),
            "fs+net profile must restrict egress to loopback so only the egress proxy is reachable:\n{profile}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_enforcement_macos_allows_inside_denies_outside() {
        use std::process::Command;

        let _guard = SANDBOX_EXEC_TEST_LOCK.lock().unwrap();

        if !sandbox_exec_can_apply() {
            return;
        }

        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(session.path(), mission.path(), tmp.path(), vec![]));
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        let inside_file = session.path().join("inside.txt");
        let inside_status = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!("echo hi > {}", inside_file.display()))
            .status()
            .expect("failed to run sandbox-exec");
        assert!(
            inside_status.success(),
            "expected write inside session_cwd to succeed"
        );
        assert!(inside_file.exists(), "expected inside file to be created");

        let outside_file = outside.path().join(format!(
            "kranz_sandbox_should_fail_{}",
            uuid::Uuid::new_v4()
        ));
        let outside_status = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/sh")
            .arg("-c")
            .arg(format!("echo hi > {}", outside_file.display()))
            .status()
            .expect("failed to run sandbox-exec");
        assert!(
            !outside_status.success(),
            "expected write outside allowlist to be denied"
        );
        assert!(
            !outside_file.exists(),
            "denied write must not have created the file"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_enforcement_macos_denies_authority_material_reads() {
        use std::process::Command;

        let _guard = SANDBOX_EXEC_TEST_LOCK.lock().unwrap();

        if !sandbox_exec_can_apply() {
            return;
        }

        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let kranz_dir = repo.path().join(".kranz");
        for name in ["serve.token", "serve.read.token", "config.json"] {
            std::fs::write(kranz_dir.join(name), "secret").unwrap();
        }
        let public = repo.path().join("public.txt");
        std::fs::write(&public, "public").unwrap();

        let profile = generate_profile(&inputs(repo.path(), &mission, tmp.path(), vec![]));
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        for name in ["serve.token", "serve.read.token", "config.json"] {
            let status = Command::new("sandbox-exec")
                .arg("-f")
                .arg(&profile_path)
                .arg("/bin/cat")
                .arg(kranz_dir.join(name))
                .status()
                .expect("failed to run sandbox-exec");
            assert!(
                !status.success(),
                "sandboxed read of .kranz/{name} must be denied"
            );
        }

        // Ordinary repo reads keep working under the same profile.
        let output = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/bin/cat")
            .arg(&public)
            .output()
            .expect("failed to run sandbox-exec");
        assert!(
            output.status.success(),
            "ordinary repo reads must keep working: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&output.stdout), "public");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_enforcement_macos_denies_mission_metadata_writes() {
        use std::process::Command;

        let _guard = SANDBOX_EXEC_TEST_LOCK.lock().unwrap();

        if !sandbox_exec_can_apply() {
            return;
        }

        // Checkout-mode shape: session_cwd is the repo root, an ANCESTOR of
        // the mission dir — the hostile case the write denies exist for.
        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        let runs = mission.join("runs");
        std::fs::create_dir_all(&runs).unwrap();
        let control = mission.join("control");
        std::fs::create_dir_all(&control).unwrap();
        let contract_home = runs.join("contract-home");
        std::fs::create_dir_all(&contract_home).unwrap();
        let events = mission.join("events.jsonl");
        let state = mission.join("state.json");
        let old_transcript = runs.join("run-old.jsonl");
        std::fs::write(&events, "{\"seq\":1}\n").unwrap();
        std::fs::write(&state, "{}").unwrap();
        std::fs::write(&old_transcript, "original\n").unwrap();
        let scratch = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(repo.path(), &mission, scratch.path(), vec![]));
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        // Engine-owned paths refuse writes — including a NEW runs/*.jsonl
        // (the transcript regex denies creation, not just modification).
        let denied_writes = [
            format!("echo tampered >> {}", events.display()),
            format!("echo tampered > {}", state.display()),
            format!("echo x > {}", control.join("approve.json").display()),
            format!("echo forged >> {}", old_transcript.display()),
            format!("echo forged > {}", runs.join("run-new.jsonl").display()),
        ];
        for write in denied_writes {
            let status = Command::new("sandbox-exec")
                .arg("-f")
                .arg(&profile_path)
                .arg("/bin/sh")
                .arg("-c")
                .arg(&write)
                .status()
                .expect("failed to run sandbox-exec");
            assert!(!status.success(), "write must be denied: {write}");
        }
        assert_eq!(std::fs::read_to_string(&events).unwrap(), "{\"seq\":1}\n");
        assert_eq!(std::fs::read_to_string(&state).unwrap(), "{}");
        assert_eq!(
            std::fs::read_to_string(&old_transcript).unwrap(),
            "original\n"
        );
        assert!(!runs.join("run-new.jsonl").exists());
        assert!(std::fs::read_dir(&control).unwrap().next().is_none());

        // The session's own work continues under the same profile: the repo
        // tree, the private scratch, and runs/ SUBDIRECTORIES stay writable.
        let allowed_writes = [
            repo.path().join("src.txt"),
            scratch.path().join("notes.txt"),
            contract_home.join("out.txt"),
        ];
        for target in allowed_writes {
            let status = Command::new("sandbox-exec")
                .arg("-f")
                .arg(&profile_path)
                .arg("/bin/sh")
                .arg("-c")
                .arg(format!("echo ok > {}", target.display()))
                .status()
                .expect("failed to run sandbox-exec");
            assert!(
                status.success(),
                "write must be allowed: {}",
                target.display()
            );
            assert!(target.exists());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_enforcement_macos_denies_sibling_temp_neighbors() {
        use std::process::Command;

        let _guard = SANDBOX_EXEC_TEST_LOCK.lock().unwrap();

        if !sandbox_exec_can_apply() {
            return;
        }

        // The finding's layout: every mission's integration/feature
        // worktrees and scratch homes sit side by side under the shared
        // temp root. A session must write its own worktree + scratch and
        // nothing beside them.
        let root = tempfile::tempdir().unwrap();
        let session = root.path().join("kranz-wt-aaa-m1-f-1-1");
        let scratch = root.path().join("kranz-worker-home-sess-1");
        let scratch_home = scratch.join("home");
        let sibling = root.path().join("kranz-wt-bbb-m2-_integration");
        let sibling_scratch = root.path().join("kranz-worker-home-sess-2");
        for d in [&session, &scratch_home, &sibling, &sibling_scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        let mission = tempfile::tempdir().unwrap();

        let profile = generate_profile(&inputs(&session, mission.path(), &scratch, vec![]));
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        for allowed in [session.join("code.rs"), scratch_home.join("notes.txt")] {
            let status = Command::new("sandbox-exec")
                .arg("-f")
                .arg(&profile_path)
                .arg("/bin/sh")
                .arg("-c")
                .arg(format!("echo ok > {}", allowed.display()))
                .status()
                .expect("failed to run sandbox-exec");
            assert!(
                status.success(),
                "write inside the session's own roots must be allowed: {}",
                allowed.display()
            );
            assert!(allowed.exists());
        }

        for denied in [
            sibling.join("evil.txt"),
            sibling_scratch.join("evil.txt"),
            root.path().join("evil.txt"),
        ] {
            let status = Command::new("sandbox-exec")
                .arg("-f")
                .arg(&profile_path)
                .arg("/bin/sh")
                .arg("-c")
                .arg(format!("echo evil > {}", denied.display()))
                .status()
                .expect("failed to run sandbox-exec");
            assert!(
                !status.success(),
                "write to a temp neighbor must be denied: {}",
                denied.display()
            );
            assert!(!denied.exists());
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sandbox_enforcement_macos_fs_net_loopback_profile_applies() {
        use std::process::Command;

        let _guard = SANDBOX_EXEC_TEST_LOCK.lock().unwrap();

        if !sandbox_exec_can_apply() {
            return;
        }

        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut inputs = inputs(session.path(), mission.path(), tmp.path(), vec![]);
        inputs.enforce = crate::types::SandboxEnforce::FsNet;

        // The fs+net profile (loopback-only egress) must be ACCEPTED by
        // sandbox-exec — unlike the hostname-rule shape Seatbelt rejects with
        // "host must be * or localhost" — or fs+net sessions could not run.
        let profile = generate_profile(&inputs);
        let profile_dir = tempfile::tempdir().unwrap();
        let profile_path = write_profile_file(profile_dir.path(), &profile).unwrap();

        let applied = Command::new("sandbox-exec")
            .arg("-f")
            .arg(&profile_path)
            .arg("/usr/bin/true")
            .output()
            .expect("failed to run sandbox-exec");
        assert!(
            applied.status.success(),
            "loopback-only fs+net profile must apply cleanly on macOS: {}",
            String::from_utf8_lossy(&applied.stderr)
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sandbox_enforcement_linux_bwrap_allows_inside_denies_outside() {
        use std::process::Command;

        if !bwrap_can_apply() {
            return;
        }

        let session = tempfile::tempdir().unwrap();
        let mission = tempfile::tempdir().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inputs = inputs(session.path(), mission.path(), tmp.path(), vec![]);

        let inside_file = session.path().join("inside.txt");
        let inside_args = bubblewrap_args(
            &inputs,
            Path::new("/bin/sh"),
            &["-c".into(), format!("echo hi > {}", inside_file.display())],
        )
        .unwrap();
        let inside_status = Command::new("bwrap")
            .args(inside_args)
            .status()
            .expect("failed to run bwrap");
        assert!(
            inside_status.success(),
            "expected write inside session_cwd to succeed"
        );
        assert!(inside_file.exists(), "expected inside file to be created");

        let outside_file = outside
            .path()
            .join(format!("kranz_bwrap_should_fail_{}", uuid::Uuid::new_v4()));
        let outside_args = bubblewrap_args(
            &inputs,
            Path::new("/bin/sh"),
            &["-c".into(), format!("echo hi > {}", outside_file.display())],
        )
        .unwrap();
        let outside_status = Command::new("bwrap")
            .args(outside_args)
            .status()
            .expect("failed to run bwrap");
        assert!(
            !outside_status.success(),
            "expected write outside allowlist to be denied"
        );
        assert!(
            !outside_file.exists(),
            "denied write must not have created the file"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn sandbox_enforcement_linux_bwrap_masks_authority_material() {
        use std::process::Command;

        if !bwrap_can_apply() {
            return;
        }

        let repo = tempfile::tempdir().unwrap();
        let mission = repo.path().join(".kranz").join("missions").join("m-x");
        std::fs::create_dir_all(&mission).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let serve_token = repo.path().join(".kranz").join("serve.token");
        std::fs::write(&serve_token, "secret").unwrap();
        let public = repo.path().join("public.txt");
        std::fs::write(&public, "public").unwrap();
        let inputs = inputs(repo.path(), &mission, tmp.path(), vec![]);

        // The /dev/null mask hides the content rather than failing the open:
        // cat "succeeds" with empty output.
        let masked = Command::new("bwrap")
            .args(
                bubblewrap_args(
                    &inputs,
                    Path::new("/bin/cat"),
                    &[serve_token.display().to_string()],
                )
                .unwrap(),
            )
            .output()
            .expect("failed to run bwrap");
        assert!(
            masked.status.success(),
            "reading the masked path must not fail the session: {}",
            String::from_utf8_lossy(&masked.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&masked.stdout).contains("secret"),
            "serve.token content must be masked inside the sandbox"
        );

        let control = Command::new("bwrap")
            .args(
                bubblewrap_args(
                    &inputs,
                    Path::new("/bin/cat"),
                    &[public.display().to_string()],
                )
                .unwrap(),
            )
            .output()
            .expect("failed to run bwrap");
        assert_eq!(String::from_utf8_lossy(&control.stdout), "public");
    }
}
