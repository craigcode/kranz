//! Backend readiness / quota preflight before queue drain.
//!
//! Lightweight probe of the backends a queued mission will use. Park on hard
//! failures; requeue/delay on rate limits; warn+proceed when quota is unknown
//! or the provider exposes no meter (never invent a "0% quota" bar).

use crate::config;
use crate::error::Result;
use crate::paths::MissionPaths;
use crate::types::{BackendKind, MissionConfig, MissionState, Role, SandboxEnforce};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::{Duration, Instant};

/// Probe outcome enum (ticket acceptance surface).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessStatus {
    Ok,
    Missing,
    Unauthenticated,
    RateLimited,
    Unsupported,
    Unknown,
    /// Provider has no quota API — treat like unknown for drain (warn+proceed).
    Meterless,
}

/// One role's readiness row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleReadiness {
    pub role: String,
    pub backend: String,
    pub status: ReadinessStatus,
    pub detail: String,
    /// Operator-facing next action (install binary, login, fix model, …).
    pub next_action: String,
}

/// Aggregate verdict for a mission before claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadinessReport {
    pub mission_id: String,
    pub roles: Vec<RoleReadiness>,
    /// Worst actionable status across roles (drives drain policy).
    pub overall: ReadinessStatus,
    pub warnings: Vec<String>,
}

/// What the drain loop should do with this probe result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainDecision {
    /// Claim and run.
    Proceed { warnings: Vec<String> },
    /// Remove from queue / park ticket with reason; do not claim.
    Park { reason: String },
    /// Leave queued; delay before retrying (rate limit).
    RequeueDelay { reason: String, delay: Duration },
}

impl ReadinessReport {
    pub fn drain_decision(&self) -> DrainDecision {
        match self.overall {
            ReadinessStatus::Ok => DrainDecision::Proceed {
                warnings: self.warnings.clone(),
            },
            ReadinessStatus::Unknown | ReadinessStatus::Meterless => DrainDecision::Proceed {
                warnings: {
                    let mut w = self.warnings.clone();
                    if w.is_empty() {
                        w.push(
                            "backend quota unknown/meterless — proceeding without a fabricated \
                             quota bar"
                                .into(),
                        );
                    }
                    w
                },
            },
            ReadinessStatus::RateLimited => {
                let reason = self
                    .roles
                    .iter()
                    .find(|r| r.status == ReadinessStatus::RateLimited)
                    .map(|r| r.detail.clone())
                    .unwrap_or_else(|| "backend rate limited".into());
                DrainDecision::RequeueDelay {
                    reason,
                    delay: Duration::from_secs(60),
                }
            }
            ReadinessStatus::Missing
            | ReadinessStatus::Unauthenticated
            | ReadinessStatus::Unsupported => {
                let reason = self
                    .roles
                    .iter()
                    .find(|r| {
                        matches!(
                            r.status,
                            ReadinessStatus::Missing
                                | ReadinessStatus::Unauthenticated
                                | ReadinessStatus::Unsupported
                        )
                    })
                    .map(|r| format!("{}: {} — {}", r.role, r.detail, r.next_action))
                    .unwrap_or_else(|| "backend not ready".into());
                DrainDecision::Park { reason }
            }
        }
    }
}

/// Probe readiness for a queued mission using its persisted config (or layered
/// repo config when state is missing).
pub fn probe_mission(repo_root: &Path, mission_id: &str) -> Result<ReadinessReport> {
    let cfg = load_mission_config(repo_root, mission_id)?;
    Ok(probe_config(mission_id, repo_root, &cfg))
}

fn load_mission_config(repo_root: &Path, mission_id: &str) -> Result<MissionConfig> {
    let paths = MissionPaths::new(repo_root, mission_id);
    if paths.state_file().is_file() {
        let text = std::fs::read_to_string(paths.state_file())?;
        if let Ok(state) = serde_json::from_str::<MissionState>(&text) {
            return Ok(state.config);
        }
    }
    config::load(repo_root)
}

/// Pure-ish probe against an already-loaded config (tests inject stubs via
/// env/PATH; binary discovery is the live side effect).
pub fn probe_config(mission_id: &str, repo_root: &Path, cfg: &MissionConfig) -> ReadinessReport {
    let mut roles = Vec::new();
    let mut warnings = Vec::new();

    // Config validation first — invalid model / sandbox mismatch parks.
    if let Err(e) = config::validate(cfg) {
        let detail = e.to_string();
        let status = ReadinessStatus::Unsupported;
        roles.push(RoleReadiness {
            role: "config".into(),
            backend: "n/a".into(),
            status,
            detail: detail.clone(),
            next_action: "fix .kranz/config.json / mission config and re-queue".into(),
        });
        return ReadinessReport {
            mission_id: mission_id.to_string(),
            roles,
            overall: status,
            warnings,
        };
    }

    for role in [
        Role::Orchestrator,
        Role::Worker,
        Role::ValidatorScrutiny,
        Role::ValidatorFunctional,
    ] {
        roles.push(probe_role(role, cfg));
    }

    // Sandbox resolve: hard unsupported when enforce is on and tooling missing.
    if cfg.worker.sandbox.enforce != SandboxEnforce::Off {
        let mission_dir = MissionPaths::new(repo_root, mission_id).mission_dir();
        let (_resolved, warn) =
            crate::sandbox::resolve_for_session(&cfg.worker.sandbox, repo_root, &mission_dir);
        if let Some(w) = warn {
            let lower = w.to_ascii_lowercase();
            if lower.contains("unsupported") || lower.contains("not available") {
                roles.push(RoleReadiness {
                    role: "worker.sandbox".into(),
                    backend: "sandbox".into(),
                    status: ReadinessStatus::Unsupported,
                    detail: w,
                    next_action: "set worker.sandbox.enforce to \"off\" or install sandbox tooling"
                        .into(),
                });
            } else {
                warnings.push(w);
            }
        }
    }

    // Quota: no provider API wired — always meteless/unknown for honesty.
    warnings.push(
        "provider quota not queried (meterless/unknown) — proceeding does not imply headroom"
            .into(),
    );

    let overall = worst_status(roles.iter().map(|r| r.status));
    // Unknown/meterless from quota alone shouldn't override Ok binaries —
    // we only attached a warning. Overall stays the worst *role* status.
    let overall = match overall {
        ReadinessStatus::Ok => ReadinessStatus::Meterless, // quota unknown → warn+proceed
        other => other,
    };

    ReadinessReport {
        mission_id: mission_id.to_string(),
        roles,
        overall,
        warnings,
    }
}

fn probe_role(role: Role, cfg: &MissionConfig) -> RoleReadiness {
    let role_key = match role {
        Role::Orchestrator => "orchestrator",
        Role::Worker => "worker",
        Role::ValidatorScrutiny => "validatorScrutiny",
        Role::ValidatorFunctional => "validatorFunctional",
    };
    let kind = cfg.backend_kind(role);
    let backend = kind.as_str().to_string();

    // The local HTTP backend has no CLI binary to discover; its readiness
    // probe is a best-effort, short-timeout reachability check of the role's
    // baseUrl instead. Never panics and never blocks indefinitely: a bad URL
    // or an unreachable endpoint downgrades to a not-ready/uncertain status
    // rather than an error.
    if kind == BackendKind::Local {
        let base_url = cfg.role(role).base_url.as_deref();
        let (status, detail, next_action) = probe_local_reachability(base_url);
        return RoleReadiness {
            role: role_key.into(),
            backend,
            status,
            detail,
            next_action,
        };
    }

    // The ACP backend has no binary-discovery or login-status convention:
    // its readiness probe only confirms the configured command exists (the
    // initialize handshake at session start is the real probe). Never
    // panics; a missing/relative command downgrades to an honest Unknown.
    if kind == BackendKind::Acp {
        let configured = cfg.role(role).acp_command.clone();
        let (status, detail, next_action) = match configured.as_deref() {
            Some(command) if !command.trim().is_empty() => (
                ReadinessStatus::Unknown,
                format!(
                    "acp agent {command:?} configured; no cheap probe — the initialize \
                     handshake at session start is the real probe"
                ),
                "none".into(),
            ),
            _ => (
                ReadinessStatus::Unknown,
                "acp backend has no acpCommand configured".into(),
                "set the role's acpCommand and re-queue".into(),
            ),
        };
        return RoleReadiness {
            role: role_key.into(),
            backend,
            status,
            detail,
            next_action,
        };
    }

    let discover = match kind {
        BackendKind::Claude => crate::backend_claude::discover_claude_binary(None),
        BackendKind::Codex => crate::backend_codex::discover_codex_binary(None),
        BackendKind::Droid => crate::backend_droid::discover_droid_binary(None),
        BackendKind::Kimi => crate::backend_kimi::discover_kimi_binary(None),
        BackendKind::Cursor => crate::backend_cursor::discover_cursor_binary(None),
        BackendKind::Local | BackendKind::Acp => unreachable!("handled above"),
    };

    match discover {
        Ok(binary) => match probe_cli_login(&binary, kind) {
            AuthProbe::Ok => RoleReadiness {
                role: role_key.into(),
                backend,
                status: ReadinessStatus::Ok,
                detail: "binary found; login probe ok/unknown".into(),
                next_action: "none".into(),
            },
            AuthProbe::Unauthenticated(detail) => RoleReadiness {
                role: role_key.into(),
                backend: backend.clone(),
                status: ReadinessStatus::Unauthenticated,
                detail,
                next_action: format!("authenticate the {backend} CLI and re-queue"),
            },
            AuthProbe::RateLimited(detail) => RoleReadiness {
                role: role_key.into(),
                backend,
                status: ReadinessStatus::RateLimited,
                detail,
                next_action: "wait for rate-limit reset, then drain again".into(),
            },
            AuthProbe::Unknown(detail) => RoleReadiness {
                role: role_key.into(),
                backend,
                status: ReadinessStatus::Ok,
                detail: format!("binary found; auth probe inconclusive ({detail})"),
                next_action: "none".into(),
            },
        },
        Err(e) => RoleReadiness {
            role: role_key.into(),
            backend: backend.clone(),
            status: ReadinessStatus::Missing,
            detail: e.to_string(),
            next_action: format!("install the {backend} CLI on PATH and re-queue"),
        },
    }
}

/// Bound applied to the local backend's TCP reachability check — this is a
/// preflight hint, not a guarantee, so it must never stall the drain loop.
const LOCAL_REACHABILITY_TIMEOUT: Duration = Duration::from_millis(1500);

/// Best-effort, short-timeout reachability probe for a local role's
/// `base_url`. Never panics and never blocks past
/// [`LOCAL_REACHABILITY_TIMEOUT`]: a missing/invalid `base_url` or a
/// connection failure both downgrade to an indeterminate status rather than
/// propagating an error.
fn probe_local_reachability(base_url: Option<&str>) -> (ReadinessStatus, String, String) {
    let Some(base_url) = base_url else {
        return (
            ReadinessStatus::Unknown,
            "local backend has no baseUrl configured".into(),
            "set the role's baseUrl and re-queue".into(),
        );
    };

    let url = match reqwest::Url::parse(base_url) {
        Ok(url) => url,
        Err(e) => {
            return (
                ReadinessStatus::Unknown,
                format!("baseUrl {base_url:?} is not a valid URL: {e}"),
                "fix the role's baseUrl and re-queue".into(),
            );
        }
    };

    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return (
            ReadinessStatus::Unknown,
            format!("baseUrl {base_url:?} has no resolvable host/port"),
            "fix the role's baseUrl and re-queue".into(),
        );
    };

    let addr = match (host, port).to_socket_addrs() {
        Ok(mut addrs) => addrs.next(),
        Err(_) => None,
    };
    let Some(addr) = addr else {
        return (
            ReadinessStatus::Missing,
            format!("baseUrl host {host:?} did not resolve to an address"),
            "verify the local endpoint is running and reachable, then re-queue".into(),
        );
    };

    match TcpStream::connect_timeout(&addr, LOCAL_REACHABILITY_TIMEOUT) {
        Ok(_) => (
            ReadinessStatus::Ok,
            format!("local endpoint {base_url} reachable"),
            "none".into(),
        ),
        Err(e) => (
            ReadinessStatus::Missing,
            format!("local endpoint {base_url} unreachable: {e}"),
            "start the local endpoint / verify baseUrl, then re-queue".into(),
        ),
    }
}

enum AuthProbe {
    Ok,
    Unauthenticated(String),
    RateLimited(String),
    Unknown(String),
}

/// Bounded login probe (≤3s). Prefer an explicit auth-status subcommand when
/// the CLI supports it; never treat a missing subcommand as unauthenticated.
fn probe_cli_login(binary: &Path, kind: BackendKind) -> AuthProbe {
    let args: &[&str] =
        match kind {
            BackendKind::Claude => &["auth", "status"],
            BackendKind::Codex => &["login", "status"],
            BackendKind::Droid => return AuthProbe::Unknown("no auth-status subcommand".into()),
            // No scriptable auth-status subcommand; `provider list` is the
            // documented free read-only probe (docs/scoping/kimi-cli-backend.md
            // §2) that shows the OAuth-managed provider when authenticated.
            BackendKind::Kimi => &["provider", "list"],
            // `agent models` is the free read-only probe that distinguishes
            // BOTH failure modes the scoping doc's preflight requires
            // (docs/scoping/cursor-cli-backend.md): unauthenticated ("Not
            // logged in") vs. authenticated-but-zero-entitled-models ("No
            // models available for this account") — the honest
            // model-availability diagnostic of probe item 5.
            BackendKind::Cursor => &["models"],
            BackendKind::Local => {
                return AuthProbe::Unknown("local backend has no CLI to probe".into())
            }
            BackendKind::Acp => return AuthProbe::Unknown(
                "acp backend has no auth-status convention; the initialize handshake is the probe"
                    .into(),
            ),
        };
    match run_bounded(binary, args, Duration::from_secs(3), auth_env_for(kind)) {
        Ok((code, out)) => {
            let lower = out.to_ascii_lowercase();
            if kind == BackendKind::Kimi && lower.contains("no providers configured") {
                return AuthProbe::Unauthenticated(out);
            }
            // An authenticated cursor account with zero entitled models fails
            // every session deterministically (probe item 5); park with the
            // real cause named rather than letting a mission discover it.
            if kind == BackendKind::Cursor && lower.contains("no models available") {
                return AuthProbe::Unauthenticated(format!(
                    "cursor account has no entitled models ({out}); provision at least one \
                     model for the account (docs/scoping/cursor-cli-backend.md item 5)"
                ));
            }
            if lower.contains("not logged")
                || lower.contains("not authenticated")
                || lower.contains("unauthenticated")
                || (lower.contains("please run") && lower.contains("login"))
            {
                return AuthProbe::Unauthenticated(out);
            }
            if (lower.contains("rate") && lower.contains("limit"))
                || lower.contains("429")
                || (lower.contains("quota") && lower.contains("exceed"))
            {
                return AuthProbe::RateLimited(out);
            }
            if code == 0 {
                AuthProbe::Ok
            } else if lower.contains("unknown")
                || lower.contains("unrecognized")
                || lower.contains("invalid command")
                || lower.contains("no such command")
            {
                AuthProbe::Unknown(format!("auth status unsupported: {out}"))
            } else {
                AuthProbe::Unknown(format!("exit {code}: {out}"))
            }
        }
        Err(e) => AuthProbe::Unknown(e),
    }
}

/// The ONE ambient var each backend's CLI may authenticate with — the same
/// per-backend injection `agent_env::agent_session_env` performs for a
/// session. A login probe that could not see it would report a
/// key-authenticated operator as unauthenticated, so it is the single
/// credential the cleared probe env carries ([`run_bounded`]).
fn auth_env_for(kind: BackendKind) -> Option<&'static str> {
    match kind {
        BackendKind::Claude => Some("ANTHROPIC_API_KEY"),
        BackendKind::Codex => Some("OPENAI_API_KEY"),
        BackendKind::Cursor => Some("CURSOR_API_KEY"),
        BackendKind::Kimi => Some("KIMI_API_KEY"),
        BackendKind::Droid | BackendKind::Local | BackendKind::Acp => None,
    }
}

/// Run one bounded readiness probe with a CLEARED environment (2026-09-01
/// adversarial audit, H5): the discovery and readiness probes were the only
/// children the engine spawned with the operator's whole environment, so a
/// repo-named or PATH-shadowed CLI collected `GH_TOKEN`, `SLACK_*`, `AWS_*`
/// and every API key on its first invocation. The probe env is
/// `agent_env::probe_child_env` — the session allowlist WITHOUT the scratch
/// relocation, because a login probe has to read the operator's real config
/// to answer the question it is asked — plus `auth_env`, the one ambient
/// credential this backend's CLI may authenticate with.
fn run_bounded(
    binary: &Path,
    args: &[&str],
    timeout: Duration,
    auth_env: Option<&str>,
) -> std::result::Result<(i32, String), String> {
    use std::process::{Command, Stdio};
    let extra: Vec<(String, String)> = auth_env
        .and_then(|name| {
            std::env::var_os(name)
                .filter(|value| !value.is_empty())
                .map(|value| (name.to_string(), value.to_string_lossy().into_owned()))
        })
        .into_iter()
        .collect();
    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_clear()
        .envs(crate::agent_env::probe_child_env(&extra));
    let mut child = command.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut out) = child.stdout.take() {
                    let _ = out.read_to_string(&mut stdout);
                }
                if let Some(mut err) = child.stderr.take() {
                    let _ = err.read_to_string(&mut stderr);
                }
                let combined = format!("{stdout}{stderr}").trim().to_string();
                return Ok((status.code().unwrap_or(-1), combined));
            }
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => {
                let _ = child.kill();
                return Err(format!("wait failed: {e}"));
            }
        }
    }
}

fn worst_status(statuses: impl Iterator<Item = ReadinessStatus>) -> ReadinessStatus {
    // Priority: missing/unauth/unsupported > rate_limited > ok > unknown/meterless
    let mut worst = ReadinessStatus::Ok;
    for s in statuses {
        worst = match (worst, s) {
            (_, ReadinessStatus::Missing) => ReadinessStatus::Missing,
            (ReadinessStatus::Missing, _) => ReadinessStatus::Missing,
            (_, ReadinessStatus::Unauthenticated) => ReadinessStatus::Unauthenticated,
            (ReadinessStatus::Unauthenticated, _) => ReadinessStatus::Unauthenticated,
            (_, ReadinessStatus::Unsupported) => ReadinessStatus::Unsupported,
            (ReadinessStatus::Unsupported, _) => ReadinessStatus::Unsupported,
            (_, ReadinessStatus::RateLimited) => ReadinessStatus::RateLimited,
            (ReadinessStatus::RateLimited, _) => ReadinessStatus::RateLimited,
            (ReadinessStatus::Ok, other) => other,
            (a, _) => a,
        };
    }
    worst
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MissionConfig;

    #[test]
    fn unknown_quota_proceeds_with_warning() {
        // Default config on a machine with claude may be Ok or Missing —
        // force overall Meterless path via drain_decision.
        let report = ReadinessReport {
            mission_id: "m-1".into(),
            roles: vec![RoleReadiness {
                role: "worker".into(),
                backend: "claude".into(),
                status: ReadinessStatus::Ok,
                detail: "ok".into(),
                next_action: "none".into(),
            }],
            overall: ReadinessStatus::Meterless,
            warnings: vec![],
        };
        match report.drain_decision() {
            DrainDecision::Proceed { warnings } => {
                assert!(!warnings.is_empty());
            }
            other => panic!("expected Proceed, got {other:?}"),
        }
    }

    #[test]
    fn missing_binary_parks() {
        let report = ReadinessReport {
            mission_id: "m-1".into(),
            roles: vec![RoleReadiness {
                role: "worker".into(),
                backend: "codex".into(),
                status: ReadinessStatus::Missing,
                detail: "no codex binary".into(),
                next_action: "install codex".into(),
            }],
            overall: ReadinessStatus::Missing,
            warnings: vec![],
        };
        assert!(matches!(
            report.drain_decision(),
            DrainDecision::Park { .. }
        ));
    }

    #[test]
    fn missing_kimi_binary_parks() {
        let report = ReadinessReport {
            mission_id: "m-1".into(),
            roles: vec![RoleReadiness {
                role: "worker".into(),
                backend: "kimi".into(),
                status: ReadinessStatus::Missing,
                detail: "no kimi binary".into(),
                next_action: "install kimi".into(),
            }],
            overall: ReadinessStatus::Missing,
            warnings: vec![],
        };
        assert!(matches!(
            report.drain_decision(),
            DrainDecision::Park { .. }
        ));
    }

    #[test]
    fn probe_role_classifies_kimi_backend() {
        let mut cfg = MissionConfig::default();
        cfg.worker.backend = Some("kimi".into());
        let role = probe_role(crate::types::Role::Worker, &cfg);
        assert_eq!(role.backend, "kimi");
        // Discovery either finds the binary (Ok/Unauthenticated/RateLimited)
        // or reports Missing — never panics, always a real classification.
        assert!(matches!(
            role.status,
            ReadinessStatus::Ok
                | ReadinessStatus::Missing
                | ReadinessStatus::Unauthenticated
                | ReadinessStatus::RateLimited
                | ReadinessStatus::Meterless
        ));
    }

    #[test]
    fn rate_limited_requeues() {
        let report = ReadinessReport {
            mission_id: "m-1".into(),
            roles: vec![RoleReadiness {
                role: "orchestrator".into(),
                backend: "claude".into(),
                status: ReadinessStatus::RateLimited,
                detail: "429".into(),
                next_action: "wait".into(),
            }],
            overall: ReadinessStatus::RateLimited,
            warnings: vec![],
        };
        match report.drain_decision() {
            DrainDecision::RequeueDelay { delay, .. } => {
                assert!(delay.as_secs() >= 1);
            }
            other => panic!("expected RequeueDelay, got {other:?}"),
        }
    }

    #[test]
    fn invalid_model_parks_via_validate() {
        let mut cfg = MissionConfig::default();
        cfg.orchestrator.model = "not-a-real-model-xyz".into();
        let report = probe_config("m-x", Path::new("/tmp"), &cfg);
        assert_eq!(report.overall, ReadinessStatus::Unsupported);
        assert!(matches!(
            report.drain_decision(),
            DrainDecision::Park { .. }
        ));
    }

    #[test]
    fn enforced_sandbox_on_non_claude_backend_parks_via_validate() {
        // The fail-closed sandbox/backend check in config::validate flows
        // through the readiness probe's config-validation-first block, so a
        // queued mission with the bad pair parks before it ever runs.
        let mut cfg = MissionConfig::default();
        cfg.worker.backend = Some("codex".into());
        cfg.worker.sandbox.enforce = SandboxEnforce::FsNet;
        let report = probe_config("m-sandbox", Path::new("/tmp"), &cfg);
        assert_eq!(report.overall, ReadinessStatus::Unsupported);
        let config_row = report
            .roles
            .iter()
            .find(|r| r.role == "config")
            .expect("config validation row");
        assert!(
            config_row.detail.contains("codex"),
            "detail must name the backend: {}",
            config_row.detail
        );
        assert!(
            config_row.detail.contains("fs+net"),
            "detail must name the enforce mode: {}",
            config_row.detail
        );
        match report.drain_decision() {
            DrainDecision::Park { reason } => {
                assert!(reason.contains("codex"), "{reason}");
            }
            other => panic!("expected Park, got {other:?}"),
        }
    }

    #[test]
    fn enforced_sandbox_on_claude_backend_does_not_park_on_config() {
        let mut cfg = MissionConfig::default();
        cfg.worker.sandbox.enforce = SandboxEnforce::FsNet;
        let report = probe_config("m-sandbox-ok", Path::new("/tmp"), &cfg);
        assert!(
            !report
                .roles
                .iter()
                .any(|r| r.role == "config" && r.status == ReadinessStatus::Unsupported),
            "claude + enforced sandbox must not produce a config rejection: {:?}",
            report.roles
        );
    }

    #[test]
    fn passing_default_config_does_not_park_on_quota() {
        let cfg = MissionConfig::default();
        // May be Missing if claude absent in CI — either Proceed or Park(missing),
        // but never Park for meteless alone when binaries are Ok.
        let report = probe_config("m-ok", Path::new("/tmp"), &cfg);
        match report.drain_decision() {
            DrainDecision::Proceed { .. } => {}
            DrainDecision::Park { reason } => {
                assert!(
                    reason.contains("Missing")
                        || reason.to_ascii_lowercase().contains("install")
                        || reason.to_ascii_lowercase().contains("binary")
                        || reason.to_ascii_lowercase().contains("not found")
                        || reason.to_ascii_lowercase().contains("could not"),
                    "unexpected park reason: {reason}"
                );
            }
            DrainDecision::RequeueDelay { .. } => panic!("default config should not rate-limit"),
        }
    }

    #[test]
    fn local_role_with_missing_base_url_is_unknown_not_panic() {
        let (status, detail, _next_action) = probe_local_reachability(None);
        assert_eq!(status, ReadinessStatus::Unknown);
        assert!(detail.to_ascii_lowercase().contains("baseurl"));
    }

    #[test]
    fn local_role_with_unreachable_base_url_is_not_ready_not_panic() {
        // Port 1 is a reserved/unassigned TCP port — nothing should be
        // listening there, so this must fail fast (bounded by
        // LOCAL_REACHABILITY_TIMEOUT) rather than hang or panic.
        let (status, detail, next_action) = probe_local_reachability(Some("http://127.0.0.1:1/v1"));
        assert!(
            matches!(status, ReadinessStatus::Missing | ReadinessStatus::Unknown),
            "expected a not-ready/uncertain status, got {status:?}"
        );
        assert!(!detail.is_empty());
        assert_ne!(next_action, "none");
    }

    #[test]
    fn local_role_with_invalid_base_url_is_unknown_not_panic() {
        let (status, _detail, _next_action) = probe_local_reachability(Some("not-a-url"));
        assert_eq!(status, ReadinessStatus::Unknown);
    }

    /// H5 (2026-09-01 adversarial audit), the readiness half: `run_bounded`
    /// spawned the login probe with the engine's whole ambient environment,
    /// so a PATH-shadowed `claude`/`codex`/`kimi`/`cursor` collected every
    /// operator credential on its first invocation. Only the allowlist and
    /// this backend's ONE auth var cross now.
    #[cfg(unix)]
    #[test]
    fn login_probe_spawns_with_a_cleared_env_carrying_only_its_auth_var() {
        use std::os::unix::fs::PermissionsExt as _;

        let _guard = crate::agent_env::EnvTestGuard::engage(&[
            ("KRANZ_SECRET_TEST", "leaked-to-the-probe"),
            ("GH_TOKEN", "ghp_poison"),
            ("ANTHROPIC_API_KEY", "sk-ant-allowed"),
        ]);

        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("env-dumping-cli");
        std::fs::write(&stub, "#!/bin/sh\nenv\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (code, dumped) = run_bounded(
            &stub,
            &["auth", "status"],
            Duration::from_secs(5),
            auth_env_for(BackendKind::Claude),
        )
        .unwrap();

        assert_eq!(code, 0, "{dumped}");
        for secret in ["KRANZ_SECRET_TEST", "GH_TOKEN"] {
            assert!(
                !dumped.contains(secret),
                "{secret} reached the login probe:\n{dumped}"
            );
        }
        // The one credential the CLI may authenticate with DOES cross, or a
        // key-authenticated operator would be reported unauthenticated.
        assert!(
            dumped.contains(&format!("ANTHROPIC_API_KEY={}", "sk-ant-allowed")),
            "the backend's own auth var must reach its login probe:\n{dumped}"
        );
        // And HOME stays real: `claude auth status` reads the operator's own
        // config to answer the question it is asked.
        assert!(dumped.contains("HOME="), "{dumped}");
    }

    /// A backend with no auth-var convention carries none — the probe env is
    /// then the bare allowlist.
    #[cfg(unix)]
    #[test]
    fn login_probe_without_an_auth_var_carries_no_credential() {
        use std::os::unix::fs::PermissionsExt as _;

        let _guard =
            crate::agent_env::EnvTestGuard::engage(&[("ANTHROPIC_API_KEY", "sk-ant-poison")]);
        let dir = tempfile::tempdir().unwrap();
        let stub = dir.path().join("env-dumping-cli");
        std::fs::write(&stub, "#!/bin/sh\nenv\n").unwrap();
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

        let (_code, dumped) = run_bounded(
            &stub,
            &["--help"],
            Duration::from_secs(5),
            auth_env_for(BackendKind::Droid),
        )
        .unwrap();

        assert!(!dumped.contains("ANTHROPIC_API_KEY"), "{dumped}");
    }

    #[test]
    fn local_probe_role_never_panics_and_reports_local_backend() {
        let mut cfg = MissionConfig::default();
        cfg.worker.backend = Some("local".into());
        cfg.worker.base_url = Some("http://127.0.0.1:1/v1".into());
        cfg.worker.model = "my-local-model".into();
        let role = probe_role(crate::types::Role::Worker, &cfg);
        assert_eq!(role.backend, "local");
        assert!(matches!(
            role.status,
            ReadinessStatus::Missing | ReadinessStatus::Unknown
        ));
    }
}
