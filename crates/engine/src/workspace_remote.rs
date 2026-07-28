//! Thin remote WorkspaceProvider adapter (ticket
//! `.kranz/tickets/workspace-remote-coder-provider.md`, design D-B
//! implementation #3 in `docs/scoping/workspace-contract.md`) — provisions a
//! mission workspace on a **Coder-shaped substrate** (or documented
//! equivalent): create from a pinned template/image, surface
//! preview/takeover URLs, inject secret *names* via the provider. kranz does
//! NOT build a VM scheduler or cloud IDE here — the substrate owns
//! scheduling, images, hibernation economics, and reachability.
//!
//! The substrate boundary is the injectable [`SubstrateClient`] trait (there
//! is no Coder deployment to test against): production drives
//! [`CoderHttpClient`] (reqwest; base URL + token from config/env), tests
//! drive a scripted fake, so all adapter logic is testable offline. The
//! minimal Coder-shaped surface:
//!
//! ```text
//! create_workspace { template, name, env_names, idle_after_hours? } -> { id, urls, takeover }
//! workspace_status(id) -> ready | pending | failed
//! delete_workspace(id)        // TeardownMode::Destroy
//! stop_workspace(id)          // TeardownMode::Hibernate
//! ```
//!
//! Selection + fail-closed config (design D-B): `workspace.provider:
//! "remote"` resolves ONLY when `workspace.remote.baseUrl`,
//! `workspace.remote.template`, and `workspace.remote.tokenEnv` are all
//! configured — a missing key fails closed at resolve (plan approval AND run
//! start) with the key named (owner: operator), never a silent fallback to
//! local. The substrate token is read from the environment variable NAMED by
//! `tokenEnv`, lazily at provision/teardown — never a value in config, logs,
//! or events, and never at resolve time (approval-time pinning stays pure).
//!
//! Secrets (design D-A): v1 passes the contract's `secrets[]` — NAMES only —
//! to `create_workspace` and records which were injected (never values).
//! Where the substrate supports it, **OIDC workload identity is preferred
//! over injected secret values** (docs/reviews/ampcode.md §3): short-lived
//! tokens minted per workspace with mission-scoped claims (repo, mission id,
//! profile), so services trust the issuer and no secret exists to leak. That
//! is a substrate/template capability and is documented here, not built —
//! v1's contract surface stays "names, never values."
//!
//! Previews/takeover (design D-E): the substrate's reported URLs are
//! name-matched onto the contract's `previews[]` placeholders and recorded
//! on `workspace.provisioned` (unmatched placeholders stay UNFILLED — never
//! a fabricated URL). The takeover artifact is the substrate's SSH/web URL
//! from create, recorded in the handle and surfaced by the workspace
//! endpoint's `takeover` field. Whether the substrate reports a preview URL
//! is fronted with auth is RECORDED (`auth`); the adapter never disables
//! auth fronting (docs/reviews/ampcode.md §8: previews authenticated by
//! default).
//!
//! Lifecycle: provision = `create_workspace` + poll `workspace_status` until
//! ready (bounded by [`READY_TIMEOUT`]). Substrate failures (create error,
//! `failed` status, poll timeout) are RECORDED on the handle and surfaced by
//! `readiness` as a provider-OWNED block (`owner: provider` — distinct from
//! `repo-setup` contract failures and `operator` config failures); the block
//! deliberately does NOT carry the workspace-gate prefix, so a later gate
//! pass does not auto-lift it — the operator unblocks once the substrate
//! recovers. Config/credential failures are `Err` at provision (fail closed
//! before spend, owner: operator). Teardown: the engine drives the
//! configured `workspace.teardownMode` at terminal states (ticket
//! `workspace-idle-hibernate`) — hibernate → `stop_workspace`, destroy →
//! `delete_workspace`, keep → no call. The optional
//! `workspace.remote.idleAfterHours` VALUE is passed through at create and
//! recorded in the provisioned detail; the SUBSTRATE owns the idle policy's
//! scheduling/execution (kranz never schedules VMs).
//!
//! v1 honesty notes (deliberate):
//! - **Readiness is substrate-reported only.** Contract bootstrap/readiness
//!   commands do NOT execute on the remote substrate in v1 (the minimal
//!   surface has no exec channel; running them over SSH is a later ticket,
//!   documented in design D-B). The readiness decision line says this out
//!   loud — a remote `workspace.readiness: ready` means "the substrate
//!   reports ready," no more.
//! - **Agent sessions still run in the local mission worktree**
//!   (`handle.cwd = spec.repo_root`): the substrate supplies the runnable
//!   services environment, previews, and takeover; backend-remote session
//!   execution is a later ticket and NOT implied here.
//! - **No public-IP requirement.** The substrate may live behind a VPN or be
//!   reachable only over SSH; reachability is an operator/network concern
//!   (see the `workspace.remote.*` config comments), never something the
//!   adapter probes or opens.
//! - **A contract-less provision starts nothing** (D-H, mirroring the
//!   container provider): no substrate workspace, no credentials required —
//!   never imply a runnable environment that does not exist.
//! - Provision always calls `create_workspace`; resume semantics
//!   (dedupe-by-name, adopt-existing) are the substrate's in v1 — teardown
//!   `Destroy` releases a leftover before re-running.

use crate::error::{EngineError, Result};
use crate::types::{ProvisionedPreview, RemoteWorkspaceConfig};
use crate::workspace_contract::PreviewSpec;
use crate::workspace_provider::{
    PreviewPlaceholder, ProgressSink, ProvisionSpec, ReadinessOutcome, TeardownMode,
    WorkspaceHandle, WorkspaceProvider, WorkspaceProviderKind,
};
use std::sync::Arc;
use std::time::Duration;

/// The adapter version pinned at approval (`workspace.provider.pinned`'s
/// `version` field for the remote kind, design D-B) — the pin records the
/// adapter contract version, not a substrate version (no substrate contact
/// at approval).
pub const ADAPTER_VERSION: &str = "coder-v1";

/// Remote workspace names are mission-owned: `kranz-remote-<sanitized
/// mission id>` — self-describing on a substrate dashboard and never shared
/// between missions.
const WORKSPACE_NAME_PREFIX: &str = "kranz-remote-";

/// One substrate HTTP call (create/status/stop/delete).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// A substrate workspace must report ready within this overall window…
const READY_TIMEOUT: Duration = Duration::from_secs(300);
/// …polled at this interval (VM/image boot takes real seconds).
const READY_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Every provider-owned block reason starts here. DISTINCT from the
/// workspace gate's prefix (`workspace gate:`) on purpose: the gate's pass
/// path auto-lifts only ITS blocks, and a provider block is not lifted by a
/// later passing gate — the operator unblocks after the substrate recovers.
pub(crate) const PROVIDER_REASON_PREFIX: &str = "workspace provider:";

/// The `milestone.blocked` reason for a provider-owned failure (design D-C's
/// owner taxonomy): names the substrate detail and owner `provider` —
/// distinct from `repo-setup` (contract commands) and `operator` (config).
/// Scrubbed: substrate error text must never put a credential into
/// events.jsonl.
pub(crate) fn provider_block_reason(detail: &str) -> String {
    crate::scrub::scrub(&format!(
        "{PROVIDER_REASON_PREFIX} {detail} (owner: provider — fix the substrate, then unblock \
         and re-run)"
    ))
}

/// What one provision asks the substrate to create. `env_names` are secret
/// NAMES (the contract's validated `secrets[]`) — values never cross this
/// boundary.
#[derive(Debug, Clone, PartialEq)]
pub struct SubstrateWorkspaceSpec {
    /// The pinned template/image id (from `workspace.remote.template`).
    pub template: String,
    /// The mission-owned workspace name (`kranz-remote-<mission id>`).
    pub name: String,
    /// Secret NAMES the substrate injects from its own secret store.
    pub env_names: Vec<String>,
    /// Substrate-side idle policy VALUE (`workspace.remote.idleAfterHours`,
    /// ticket `workspace-idle-hibernate`): hours of inactivity after which
    /// the SUBSTRATE hibernates the workspace. Passed through verbatim when
    /// the substrate accepts an idle policy — the substrate owns
    /// scheduling/execution; kranz never schedules. `None` = no idle
    /// policy requested.
    pub idle_after_hours: Option<f64>,
}

/// A URL the substrate reported for one workspace endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstrateUrl {
    pub name: String,
    pub url: String,
    /// Whether the substrate reports the URL is fronted with auth (design
    /// D-E — recorded, never disabled). `None` = the substrate did not say.
    pub auth: Option<bool>,
}

/// What the substrate created: the workspace id, its reported endpoint URLs,
/// and the human takeover URL (SSH/web) when it reported one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstrateWorkspace {
    pub id: String,
    pub urls: Vec<SubstrateUrl>,
    pub takeover: Option<String>,
}

/// The substrate-reported workspace lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubstrateStatus {
    Ready,
    Pending,
    Failed { reason: String },
}

/// The substrate boundary, injectable for tests — the minimal Coder-shaped
/// surface the adapter drives (see the module docs for the wire mapping
/// [`CoderHttpClient`] uses). No VM scheduling semantics cross this trait:
/// the substrate owns images, placement, and hibernation economics.
#[async_trait::async_trait]
pub trait SubstrateClient: Send + Sync {
    /// Create (and start) one workspace from a pinned template, injecting
    /// the named secrets — NAMES only, never values.
    async fn create_workspace(&self, spec: &SubstrateWorkspaceSpec) -> Result<SubstrateWorkspace>;
    /// The workspace's current lifecycle state.
    async fn workspace_status(&self, id: &str) -> Result<SubstrateStatus>;
    /// Release the workspace entirely ([`TeardownMode::Destroy`]).
    async fn delete_workspace(&self, id: &str) -> Result<()>;
    /// Provider-owned idle suspension ([`TeardownMode::Hibernate`]).
    async fn stop_workspace(&self, id: &str) -> Result<()>;
}

/// The validated, complete remote config — every field required. Produced
/// ONLY by [`RemoteConfig::require`], so a `RemoteWorkspaceProvider` can
/// never exist with partial remote config.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteConfig {
    pub base_url: String,
    pub template: String,
    /// NAME of the env var holding the substrate token — never the value.
    pub token_env: String,
    /// Substrate-side idle policy VALUE (`workspace.remote.idleAfterHours`,
    /// ticket `workspace-idle-hibernate`) — passed through to the substrate
    /// at provision when set; the SUBSTRATE owns scheduling/execution of the
    /// policy (kranz never schedules VMs).
    pub idle_after_hours: Option<f64>,
}

impl RemoteConfig {
    /// Validate the additive mission config into a complete remote config,
    /// failing CLOSED with the first missing key named (owner: operator) —
    /// never a silent fallback to local. Pure: no env, no network.
    pub fn require(config: Option<&RemoteWorkspaceConfig>) -> Result<RemoteConfig> {
        fn missing(key: &str) -> EngineError {
            EngineError::Config(format!(
                "workspace.provider \"remote\" needs {key} configured (owner: operator — set the \
                 mission config key); refusing rather than silently falling back to local"
            ))
        }
        let Some(config) = config else {
            return Err(missing("workspace.remote.baseUrl"));
        };
        Ok(RemoteConfig {
            base_url: config
                .base_url
                .clone()
                .ok_or_else(|| missing("workspace.remote.baseUrl"))?,
            template: config
                .template
                .clone()
                .ok_or_else(|| missing("workspace.remote.template"))?,
            token_env: config
                .token_env
                .clone()
                .ok_or_else(|| missing("workspace.remote.tokenEnv"))?,
            // Optional — the only non-required remote key (absent = no idle
            // policy passed to the substrate).
            idle_after_hours: config.idle_after_hours,
        })
    }
}

/// How the provider gets a substrate client at provision/teardown time.
/// Production reads the token from the env var NAMED by `tokenEnv` and
/// builds the [`CoderHttpClient`]; tests inject a scripted fake.
type ClientHook = Arc<dyn Fn(&RemoteConfig) -> Result<Arc<dyn SubstrateClient>> + Send + Sync>;

/// The production client hook: read the substrate token from the env var
/// NAMED by `workspace.remote.tokenEnv` — failing CLOSED with the var NAME
/// (never a value) when unset/empty — and build the Coder-shaped HTTP
/// client. Lazy on purpose: resolve/pin stay pure, and the token value
/// touches only this process's memory.
fn coder_client_from_env(config: &RemoteConfig) -> Result<Arc<dyn SubstrateClient>> {
    let token = std::env::var(&config.token_env)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            EngineError::Config(format!(
                "workspace.provider \"remote\": the substrate token env var {} (from \
                 workspace.remote.tokenEnv) is not set (owner: operator — export it before \
                 `kranz work`); refusing rather than silently falling back to local",
                config.token_env
            ))
        })?;
    Ok(Arc::new(CoderHttpClient::new(&config.base_url, &token)?))
}

/// What the provision-time poll concluded — recorded on the handle so
/// `readiness` surfaces a provider-owned block instead of re-polling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PollOutcome {
    Ready,
    Failed { reason: String },
    TimedOut { waited_secs: u64 },
}

/// The remote provider's handle state — everything `readiness` and
/// `teardown` need across the seam's calls (the provider itself stays
/// stateless).
#[derive(Debug, Clone)]
pub struct RemoteWorkspace {
    /// The mission-owned workspace name (always known).
    pub name: String,
    /// The substrate workspace id — EMPTY when create failed (teardown then
    /// no-ops: nothing exists to release).
    pub id: String,
    /// The substrate's takeover URL (SSH/web) as reported at create.
    pub takeover: Option<String>,
    /// Substrate URLs name-matched onto the contract's `previews[]` —
    /// recorded on `workspace.provisioned`.
    pub previews: Vec<ProvisionedPreview>,
    /// The secret NAMES the substrate was asked to inject (never values).
    pub injected_env_names: Vec<String>,
    /// What the provision-time readiness poll concluded.
    pub poll: PollOutcome,
}

/// The remote provider: thin adapter over a [`SubstrateClient`]. See the
/// module docs for the config gate, owner taxonomy, and v1 honesty notes.
pub struct RemoteWorkspaceProvider {
    config: RemoteConfig,
    client_hook: ClientHook,
    ready_timeout: Duration,
    poll_interval: Duration,
}

impl RemoteWorkspaceProvider {
    /// From mission config — fails CLOSED (owner: operator) with the first
    /// missing `workspace.remote.*` key named. The production client hook
    /// reads the token env var lazily, at provision/teardown.
    pub(crate) fn from_config(config: Option<&RemoteWorkspaceConfig>) -> Result<Self> {
        Ok(Self {
            config: RemoteConfig::require(config)?,
            client_hook: Arc::new(coder_client_from_env),
            ready_timeout: READY_TIMEOUT,
            poll_interval: READY_POLL_INTERVAL,
        })
    }

    /// A provider driving a scripted substrate boundary: the full
    /// provision/readiness/teardown logic runs without a substrate, with
    /// millisecond poll bounds so the timeout path stays fast.
    #[cfg(test)]
    pub(crate) fn with_client(config: RemoteConfig, client: Arc<dyn SubstrateClient>) -> Self {
        Self {
            config,
            client_hook: Arc::new(move |_config| Ok(Arc::clone(&client))),
            ready_timeout: Duration::from_millis(120),
            poll_interval: Duration::from_millis(5),
        }
    }

    /// Poll `workspace_status` until ready, failed, or out of time. Never
    /// `Err`: every substrate outcome is a recorded [`PollOutcome`] so
    /// `readiness` can block with the provider owner (a status-call error is
    /// a substrate failure too, not a config error).
    async fn poll_until_ready(&self, client: &dyn SubstrateClient, id: &str) -> PollOutcome {
        let deadline = std::time::Instant::now() + self.ready_timeout;
        loop {
            match client.workspace_status(id).await {
                Ok(SubstrateStatus::Ready) => return PollOutcome::Ready,
                Ok(SubstrateStatus::Failed { reason }) => return PollOutcome::Failed { reason },
                Ok(SubstrateStatus::Pending) => {}
                Err(e) => {
                    return PollOutcome::Failed {
                        reason: format!("status poll failed: {e}"),
                    };
                }
            }
            if std::time::Instant::now() >= deadline {
                return PollOutcome::TimedOut {
                    waited_secs: self.ready_timeout.as_secs(),
                };
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

/// `kranz-remote-<sanitized-mission-id>` — Coder-shaped names are lowercase
/// alnum plus `-`; the prefix guarantees a valid leading character even when
/// the mission id sanitizes to nothing. (Same sanitization idiom as the
/// container provider's compose project names.)
fn workspace_name(mission_id: &str) -> String {
    let sanitized: String = mission_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else if c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() {
        format!("{WORKSPACE_NAME_PREFIX}mission")
    } else {
        format!("{WORKSPACE_NAME_PREFIX}{sanitized}")
    }
}

/// The handle's preview placeholders (the seam's existing field): a
/// substrate URL whose name matches a contract preview FILLS the template;
/// unmatched placeholders keep the contract's URL template UNFILLED (design
/// D-E — never a fabricated URL).
fn preview_placeholders(
    previews: &[PreviewSpec],
    urls: &[SubstrateUrl],
) -> Vec<PreviewPlaceholder> {
    previews
        .iter()
        .map(|preview| {
            let url_template = urls
                .iter()
                .find(|url| url.name == preview.name)
                .map(|url| url.url.clone())
                .unwrap_or_else(|| preview.url_template.clone());
            PreviewPlaceholder {
                name: preview.name.clone(),
                url_template,
            }
        })
        .collect()
}

/// The event-recorded previews: ONLY substrate-reported URLs, name-matched
/// onto the contract's `previews[]` and carrying the substrate's auth report.
/// Unmatched contract previews are absent — the substrate never reported a
/// URL for them, so nothing is recorded (never fabricated).
fn map_previews(previews: &[PreviewSpec], urls: &[SubstrateUrl]) -> Vec<ProvisionedPreview> {
    previews
        .iter()
        .filter_map(|preview| {
            urls.iter()
                .find(|url| url.name == preview.name)
                .map(|url| ProvisionedPreview {
                    name: preview.name.clone(),
                    url: url.url.clone(),
                    auth: url.auth,
                })
        })
        .collect()
}

/// The `workspace.provisioned` detail string: names the substrate workspace
/// and WHICH secret names were injected (never values), plus the configured
/// substrate-owned idle policy when one was passed through (ticket
/// `workspace-idle-hibernate` — recorded, never scheduled by kranz). Empty
/// id = create failed before the substrate assigned one.
fn provision_detail(
    name: &str,
    id: &str,
    env_names: &[String],
    idle_after_hours: Option<f64>,
) -> String {
    let injected = if env_names.is_empty() {
        "none".to_string()
    } else {
        env_names.join(",")
    };
    let idle = match idle_after_hours {
        Some(hours) => format!("; idle policy: hibernate after {hours}h (substrate-owned)"),
        None => String::new(),
    };
    if id.is_empty() {
        format!("substrate workspace {name}: create failed (injected env names: {injected}){idle}")
    } else {
        format!("substrate workspace {name} (id {id}); injected env names: {injected}{idle}")
    }
}

#[async_trait::async_trait]
impl WorkspaceProvider for RemoteWorkspaceProvider {
    fn kind(&self) -> WorkspaceProviderKind {
        WorkspaceProviderKind::Remote
    }

    async fn provision(&self, spec: &ProvisionSpec) -> Result<WorkspaceHandle> {
        if !spec.repo_root.is_dir() {
            return Err(EngineError::InvalidState(format!(
                "remote provision: execution cwd {} does not exist",
                spec.repo_root.display()
            )));
        }
        let env = crate::runner::contract_env(spec.base_sha.as_deref());
        let Some(contract) = &spec.contract else {
            // No contract ⇒ nothing runnable to provision remotely (D-H,
            // mirroring the container provider): no substrate workspace, no
            // credentials required — never imply a runnable environment that
            // does not exist.
            return Ok(WorkspaceHandle {
                cwd: spec.repo_root.clone(),
                env,
                previews: Vec::new(),
                contract: None,
                detail: None,
                container: None,
                remote: None,
            });
        };

        // Credentials fail closed HERE, at provision (run start, before
        // spend), with the env var NAME named — never at resolve/pin.
        let client = (self.client_hook)(&self.config)?;
        let name = workspace_name(&spec.mission_id);
        let env_names = contract.secrets.clone();

        // Substrate failures (create error, failed status, poll timeout) are
        // RECORDED on the handle and surfaced by `readiness` as a
        // provider-owned block — never an error that loses the audit trail.
        let (id, urls, takeover, poll) = match client
            .create_workspace(&SubstrateWorkspaceSpec {
                template: self.config.template.clone(),
                name: name.clone(),
                env_names: env_names.clone(),
                idle_after_hours: self.config.idle_after_hours,
            })
            .await
        {
            Ok(created) => {
                let poll = self.poll_until_ready(&*client, &created.id).await;
                (created.id, created.urls, created.takeover, poll)
            }
            Err(e) => (
                String::new(),
                Vec::new(),
                None,
                PollOutcome::Failed {
                    reason: format!("create_workspace failed: {e}"),
                },
            ),
        };

        Ok(WorkspaceHandle {
            cwd: spec.repo_root.clone(),
            env,
            previews: preview_placeholders(&contract.previews, &urls),
            contract: Some(contract.clone()),
            detail: Some(provision_detail(
                &name,
                &id,
                &env_names,
                self.config.idle_after_hours,
            )),
            container: None,
            remote: Some(RemoteWorkspace {
                name,
                id,
                takeover,
                previews: map_previews(&contract.previews, &urls),
                injected_env_names: env_names,
                poll,
            }),
        })
    }

    async fn readiness(
        &self,
        handle: &WorkspaceHandle,
        progress: &mut ProgressSink<'_>,
    ) -> Result<ReadinessOutcome> {
        let Some(remote) = &handle.remote else {
            // Contract-less provision: nothing was created — trivially ready,
            // no progress lines (byte-identical to the local kinds).
            return Ok(ReadinessOutcome::Ready);
        };
        match &remote.poll {
            PollOutcome::Ready => {
                // Substrate-reported readiness ONLY (v1): say out loud that
                // the contract's bootstrap/readiness commands did not execute
                // on the remote substrate — the decision line's prefix is
                // deliberately NOT the gate's, so no false gate outcome is
                // derived from it.
                progress(
                    "workspace remote: substrate reports ready (substrate-reported readiness \
                     only — contract bootstrap/readiness commands do not execute on the remote \
                     substrate in v1)",
                    None,
                )?;
                Ok(ReadinessOutcome::Ready)
            }
            PollOutcome::Failed { reason } => Ok(ReadinessOutcome::ProviderFailed {
                detail: format!(
                    "substrate workspace {} failed to provision: {reason}",
                    remote.name
                ),
            }),
            PollOutcome::TimedOut { waited_secs } => Ok(ReadinessOutcome::ProviderFailed {
                detail: format!(
                    "substrate workspace {} did not become ready within {waited_secs}s",
                    remote.name
                ),
            }),
        }
    }

    async fn teardown(&self, handle: WorkspaceHandle, mode: TeardownMode) -> Result<()> {
        let Some(remote) = &handle.remote else {
            return Ok(()); // contract-less provision: nothing was created
        };
        if remote.id.is_empty() {
            return Ok(()); // create never succeeded: nothing exists to release
        }
        match mode {
            // Leave the workspace running/present (resume, inspection,
            // takeover — documented: previews and the takeover URL stay live).
            TeardownMode::Keep => Ok(()),
            TeardownMode::Hibernate => {
                let client = (self.client_hook)(&self.config)?;
                client.stop_workspace(&remote.id).await.map_err(|e| {
                    EngineError::InvalidState(format!(
                        "remote teardown (hibernate): stop_workspace failed (owner: provider): {}",
                        crate::scrub::scrub(&e.to_string())
                    ))
                })
            }
            TeardownMode::Destroy => {
                let client = (self.client_hook)(&self.config)?;
                client.delete_workspace(&remote.id).await.map_err(|e| {
                    EngineError::InvalidState(format!(
                        "remote teardown (destroy): delete_workspace failed (owner: provider): {}",
                        crate::scrub::scrub(&e.to_string())
                    ))
                })
            }
        }
    }
}

/// The Coder-shaped HTTP client (production [`SubstrateClient`]): base URL +
/// session token, Coder's `Coder-Session-Token` auth header, one bounded
/// timeout per call. Wire mapping (the documented v1 substrate shape —
/// validated against a loopback mock only, no live Coder deployment):
///
/// - `create_workspace` → `POST {base}/api/v2/users/me/workspaces` with body
///   `{"template_id", "name", "env_names"}` (names only — the substrate
///   injects the named secrets from its own store), plus `"idle_after_hours"`
///   when `workspace.remote.idleAfterHours` is configured (the substrate
///   owns the idle policy; a substrate without support ignores the key).
///   Response: `{"id",
///   "urls"?, "takeover"?}` — `urls`/`takeover` optional so a stock Coder
///   create response (which carries the id but not flattened app URLs)
///   degrades honestly to "no previews reported."
/// - `workspace_status` → `GET {base}/api/v2/workspaces/{id}`; reads
///   top-level `status` or `latest_build.status`: `"running"` → ready,
///   `"failed"` → failed, anything else (starting/stopping/unknown) →
///   pending (keep polling, bounded).
/// - `delete_workspace` / `stop_workspace` →
///   `POST {base}/api/v2/workspaces/{id}/builds` with `{"transition":
///   "delete" | "stop"}` (Coder models both as workspace builds).
pub struct CoderHttpClient {
    base_url: String,
    token: String,
    client: reqwest::Client,
}

impl CoderHttpClient {
    /// Build the client, validating the base URL eagerly (fail closed at
    /// provision with the config key named, not mid-request).
    pub fn new(base_url: &str, token: &str) -> Result<Self> {
        let parsed = reqwest::Url::parse(base_url).map_err(|e| {
            EngineError::Config(format!(
                "workspace.remote.baseUrl {base_url:?} is not a valid URL: {e} (owner: operator)"
            ))
        })?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(EngineError::Config(format!(
                "workspace.remote.baseUrl {base_url:?} needs an http(s) URL with a host (owner: \
                 operator)"
            )));
        }
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| {
                EngineError::Config(format!("could not build the substrate HTTP client: {e}"))
            })?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
            client,
        })
    }

    /// One authenticated JSON request; non-2xx maps to an error naming the
    /// op, the status, and a scrubbed bounded body tail (never the token).
    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        op: &'static str,
    ) -> Result<reqwest::Response> {
        let response = request
            .header("Coder-Session-Token", &self.token)
            .send()
            .await
            .map_err(|e| {
                EngineError::InvalidState(format!(
                    "coder substrate {op}: request failed: {}",
                    crate::scrub::scrub(&e.to_string())
                ))
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            let tail = crate::command_exec::last_chars_local(&body, 500);
            return Err(EngineError::InvalidState(format!(
                "coder substrate {op}: HTTP {status}: {}",
                crate::scrub::scrub(tail.trim())
            )));
        }
        Ok(response)
    }

    /// Parse one response body as JSON.
    async fn json(response: reqwest::Response, op: &'static str) -> Result<serde_json::Value> {
        response.json().await.map_err(|e| {
            EngineError::InvalidState(format!("coder substrate {op}: response was not JSON: {e}"))
        })
    }

    /// A workspace build transition (delete/stop share the shape).
    async fn transition(&self, id: &str, transition: &str, op: &'static str) -> Result<()> {
        let url = format!("{}/api/v2/workspaces/{id}/builds", self.base_url);
        self.send(
            self.client
                .post(&url)
                .json(&serde_json::json!({ "transition": transition })),
            op,
        )
        .await?;
        Ok(())
    }
}

/// Parse one substrate-reported endpoint URL (`{"name", "url", "auth"?}`).
fn parse_substrate_url(value: &serde_json::Value) -> Option<SubstrateUrl> {
    Some(SubstrateUrl {
        name: value.get("name")?.as_str()?.to_string(),
        url: value.get("url")?.as_str()?.to_string(),
        auth: value.get("auth").and_then(serde_json::Value::as_bool),
    })
}

/// Map a status response body onto the lifecycle state: top-level `status`
/// wins, else `latest_build.status` (stock Coder nests it). Unknown/absent
/// states are PENDING — keep polling (bounded), never guess ready.
fn parse_status(value: &serde_json::Value) -> SubstrateStatus {
    let status = value
        .get("status")
        .and_then(|v| v.as_str())
        .or_else(|| {
            value
                .get("latest_build")
                .and_then(|build| build.get("status"))
                .and_then(|v| v.as_str())
        })
        .unwrap_or("");
    match status {
        "running" => SubstrateStatus::Ready,
        "failed" => SubstrateStatus::Failed {
            reason: "substrate reported workspace status \"failed\"".to_string(),
        },
        _ => SubstrateStatus::Pending,
    }
}

#[async_trait::async_trait]
impl SubstrateClient for CoderHttpClient {
    async fn create_workspace(&self, spec: &SubstrateWorkspaceSpec) -> Result<SubstrateWorkspace> {
        let url = format!("{}/api/v2/users/me/workspaces", self.base_url);
        let mut body = serde_json::json!({
            "template_id": spec.template,
            "name": spec.name,
            // Secret NAMES only — the substrate injects values from
            // its own store; values never cross this wire.
            "env_names": spec.env_names,
        });
        if let Some(hours) = spec.idle_after_hours {
            // The substrate-side idle policy VALUE (ticket
            // workspace-idle-hibernate): passed through verbatim — the
            // substrate owns scheduling/execution of the policy (a
            // substrate without idle-policy support ignores the key).
            body["idle_after_hours"] = serde_json::json!(hours);
        }
        let response = self
            .send(self.client.post(&url).json(&body), "create_workspace")
            .await?;
        let value = Self::json(response, "create_workspace").await?;
        let id = value
            .get("id")
            .and_then(|v| v.as_str())
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                EngineError::InvalidState(
                    "coder substrate create_workspace: response carried no workspace id"
                        .to_string(),
                )
            })?;
        let urls = value
            .get("urls")
            .and_then(|v| v.as_array())
            .map(|entries| entries.iter().filter_map(parse_substrate_url).collect())
            .unwrap_or_default();
        let takeover = value
            .get("takeover")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        Ok(SubstrateWorkspace {
            id: id.to_string(),
            urls,
            takeover,
        })
    }

    async fn workspace_status(&self, id: &str) -> Result<SubstrateStatus> {
        let url = format!("{}/api/v2/workspaces/{id}", self.base_url);
        let response = self.send(self.client.get(&url), "workspace_status").await?;
        let value = Self::json(response, "workspace_status").await?;
        Ok(parse_status(&value))
    }

    async fn delete_workspace(&self, id: &str) -> Result<()> {
        self.transition(id, "delete", "delete_workspace").await
    }

    async fn stop_workspace(&self, id: &str) -> Result<()> {
        self.transition(id, "stop", "stop_workspace").await
    }
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_contract::{parse_workspace_contract, WorkspaceContract};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    fn config() -> RemoteConfig {
        RemoteConfig {
            base_url: "https://coder.internal.example.com".to_string(),
            template: "tmpl-baked-ami".to_string(),
            token_env: "CODER_SESSION_TOKEN".to_string(),
            idle_after_hours: None,
        }
    }

    fn spec(root: &std::path::Path, contract: Option<WorkspaceContract>) -> ProvisionSpec {
        ProvisionSpec {
            mission_id: "m-test".to_string(),
            repo_root: root.to_path_buf(),
            runtime_dir: root.join(".kranz").join("missions").join("m-test"),
            base_sha: Some("deadbeefcafe".to_string()),
            contract,
        }
    }

    fn contract(json: &[u8]) -> WorkspaceContract {
        parse_workspace_contract(json).expect("valid contract")
    }

    /// Contract with two secret names, two previews (only `app` gets a
    /// substrate URL back — `db` stays an unfilled placeholder).
    fn remote_contract() -> WorkspaceContract {
        contract(
            br#"{
                "schemaVersion": 1,
                "bootstrap": ["echo never-run-remotely"],
                "readiness": ["true"],
                "previews": [
                    { "name": "app", "urlTemplate": "http://localhost:{port}/" },
                    { "name": "db", "urlTemplate": "postgres://localhost:{port}/" }
                ],
                "secrets": ["DATABASE_URL", "STRIPE_API_KEY"]
            }"#,
        )
    }

    #[derive(Default)]
    struct Progress(Vec<String>);

    impl Progress {
        fn sink(&mut self) -> impl FnMut(&str, Option<String>) -> Result<()> + Send + use<'_> {
            |summary, _detail| {
                self.0.push(summary.to_string());
                Ok(())
            }
        }
    }

    /// The scripted substrate boundary: records every call and answers from
    /// its script. `statuses` repeats the LAST entry forever once drained, so
    /// `[Pending]` is "never ready" and `[Pending, Ready]` is "ready on the
    /// second poll."
    struct FakeSubstrateClient {
        created: Mutex<Vec<SubstrateWorkspaceSpec>>,
        ops: Mutex<Vec<String>>,
        statuses: Mutex<VecDeque<SubstrateStatus>>,
        fail_create: Mutex<Option<String>>,
        workspace: SubstrateWorkspace,
    }

    impl FakeSubstrateClient {
        fn new(statuses: Vec<SubstrateStatus>) -> Arc<Self> {
            Arc::new(Self {
                created: Mutex::new(Vec::new()),
                ops: Mutex::new(Vec::new()),
                statuses: Mutex::new(statuses.into()),
                fail_create: Mutex::new(None),
                workspace: SubstrateWorkspace {
                    id: "ws-abc123".to_string(),
                    urls: vec![SubstrateUrl {
                        name: "app".to_string(),
                        url: "https://app--m-test.coder.internal.example.com".to_string(),
                        auth: Some(true),
                    }],
                    takeover: Some(
                        "ssh://coder.internal.example.com/kranz-remote-m-test".to_string(),
                    ),
                },
            })
        }

        fn failing_create(message: &str) -> Arc<Self> {
            let client = Self::new(vec![]);
            client
                .fail_create
                .lock()
                .unwrap()
                .replace(message.to_string());
            client
        }

        fn ops(&self) -> Vec<String> {
            self.ops.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl SubstrateClient for FakeSubstrateClient {
        async fn create_workspace(
            &self,
            spec: &SubstrateWorkspaceSpec,
        ) -> Result<SubstrateWorkspace> {
            self.created.lock().unwrap().push(spec.clone());
            if let Some(message) = self.fail_create.lock().unwrap().as_ref() {
                return Err(EngineError::InvalidState(message.clone()));
            }
            Ok(self.workspace.clone())
        }

        async fn workspace_status(&self, id: &str) -> Result<SubstrateStatus> {
            self.ops.lock().unwrap().push(format!("status:{id}"));
            let mut statuses = self.statuses.lock().unwrap();
            if statuses.len() > 1 {
                Ok(statuses.pop_front().expect("len > 1"))
            } else {
                Ok(statuses.front().cloned().unwrap_or(SubstrateStatus::Ready))
            }
        }

        async fn delete_workspace(&self, id: &str) -> Result<()> {
            self.ops.lock().unwrap().push(format!("delete:{id}"));
            Ok(())
        }

        async fn stop_workspace(&self, id: &str) -> Result<()> {
            self.ops.lock().unwrap().push(format!("stop:{id}"));
            Ok(())
        }
    }

    fn provider_with(client: Arc<FakeSubstrateClient>) -> RemoteWorkspaceProvider {
        RemoteWorkspaceProvider::with_client(config(), client)
    }

    async fn provisioned(
        provider: &RemoteWorkspaceProvider,
        root: &std::path::Path,
    ) -> WorkspaceHandle {
        provider
            .provision(&spec(root, Some(remote_contract())))
            .await
            .expect("provision")
    }

    /// Provision asks the substrate to create from the PINNED template with
    /// the mission-owned name and the contract's secret NAMES — never values
    /// (the spec type carries no value field at all).
    #[tokio::test]
    async fn remote_workspace_provision_creates_with_template_name_and_secret_names() {
        let dir = tempfile::tempdir().expect("tempdir");
        let client = FakeSubstrateClient::new(vec![SubstrateStatus::Ready]);
        let provider = provider_with(Arc::clone(&client));
        let handle = provisioned(&provider, dir.path()).await;

        let created = client.created.lock().unwrap().clone();
        assert_eq!(created.len(), 1, "exactly one create_workspace call");
        assert_eq!(created[0].template, "tmpl-baked-ami");
        assert_eq!(created[0].name, "kranz-remote-m-test");
        assert_eq!(
            created[0].env_names,
            vec!["DATABASE_URL".to_string(), "STRIPE_API_KEY".to_string()],
            "the contract's secret NAMES — and only names — cross to the substrate"
        );

        assert_eq!(
            handle.cwd,
            dir.path(),
            "sessions stay in the local worktree in v1"
        );
        assert_eq!(
            handle.env.get("KRANZ_BASE_SHA").map(String::as_str),
            Some("deadbeefcafe")
        );
        let remote = handle.remote.as_ref().expect("remote state");
        assert_eq!(remote.id, "ws-abc123");
        assert_eq!(remote.injected_env_names, created[0].env_names);
        assert!(
            handle.detail.as_deref().unwrap().contains("ws-abc123")
                && handle
                    .detail
                    .as_deref()
                    .unwrap()
                    .contains("DATABASE_URL,STRIPE_API_KEY"),
            "the provisioned detail records the workspace and injected names: {:?}",
            handle.detail
        );
    }

    /// A ready poll lands the substrate's URLs in the handle: `app` filled
    /// with the substrate URL (+ auth report), the unmatched `db` placeholder
    /// UNFILLED, and the takeover URL carried for the endpoint.
    #[tokio::test]
    async fn remote_workspace_ready_poll_lands_previews_and_takeover() {
        let dir = tempfile::tempdir().expect("tempdir");
        let client =
            FakeSubstrateClient::new(vec![SubstrateStatus::Pending, SubstrateStatus::Ready]);
        let provider = provider_with(Arc::clone(&client));
        let handle = provisioned(&provider, dir.path()).await;

        assert!(
            client
                .ops()
                .iter()
                .filter(|op| op.starts_with("status:"))
                .count()
                >= 2,
            "the poll looped past the pending status: {:?}",
            client.ops()
        );
        let remote = handle.remote.as_ref().expect("remote state");
        assert_eq!(remote.poll, PollOutcome::Ready);
        assert_eq!(
            remote.takeover.as_deref(),
            Some("ssh://coder.internal.example.com/kranz-remote-m-test")
        );
        assert_eq!(
            remote.previews,
            vec![ProvisionedPreview {
                name: "app".to_string(),
                url: "https://app--m-test.coder.internal.example.com".to_string(),
                auth: Some(true),
            }],
            "only the substrate-reported URL is recorded — db stays unfabricated"
        );
        // The seam's placeholder field: app filled, db's template UNFILLED.
        assert_eq!(
            handle.previews,
            vec![
                PreviewPlaceholder {
                    name: "app".to_string(),
                    url_template: "https://app--m-test.coder.internal.example.com".to_string(),
                },
                PreviewPlaceholder {
                    name: "db".to_string(),
                    url_template: "postgres://localhost:{port}/".to_string(),
                },
            ]
        );

        // Readiness is substrate-reported ONLY, and says so out loud.
        let mut progress = Progress::default();
        let outcome = provider
            .readiness(&handle, &mut progress.sink())
            .await
            .expect("readiness");
        assert!(matches!(outcome, ReadinessOutcome::Ready), "{outcome:?}");
        assert_eq!(progress.0.len(), 1);
        assert!(
            progress.0[0].starts_with("workspace remote: substrate reports ready")
                && progress.0[0].contains("substrate-reported readiness only"),
            "honest readiness wording, no gate-outcome prefix: {}",
            progress.0[0]
        );
    }

    /// A substrate `failed` status maps to the provider-OWNED block shape —
    /// owner `provider`, never `repo-setup`, and not the gate's prefix (no
    /// auto-lift on a later passing gate).
    #[tokio::test]
    async fn remote_workspace_failed_status_is_a_provider_owned_block() {
        let dir = tempfile::tempdir().expect("tempdir");
        let client = FakeSubstrateClient::new(vec![SubstrateStatus::Failed {
            reason: "template build exited 1".to_string(),
        }]);
        let provider = provider_with(Arc::clone(&client));
        let handle = provisioned(&provider, dir.path()).await;

        let mut progress = Progress::default();
        let outcome = provider
            .readiness(&handle, &mut progress.sink())
            .await
            .expect("readiness");
        let ReadinessOutcome::ProviderFailed { detail } = outcome else {
            panic!("a failed substrate status must be ProviderFailed, got {outcome:?}");
        };
        assert!(detail.contains("kranz-remote-m-test"), "{detail}");
        assert!(detail.contains("template build exited 1"), "{detail}");

        let reason = provider_block_reason(&detail);
        assert!(reason.starts_with("workspace provider:"), "{reason}");
        assert!(reason.contains("owner: provider"), "{reason}");
        assert!(!reason.contains("repo-setup"), "{reason}");
        assert!(
            !reason.starts_with(crate::workspace_gate::GATE_REASON_PREFIX),
            "not gate-owned ⇒ the pass path never auto-lifts it: {reason}"
        );
        assert!(
            progress.0.is_empty(),
            "a failed workspace reports no ready line"
        );
    }

    /// A workspace that never becomes ready inside the bound is the same
    /// provider-owned failure (bounded — the poll cannot hang the run).
    #[tokio::test]
    async fn remote_workspace_poll_timeout_is_provider_owned_and_bounded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let client = FakeSubstrateClient::new(vec![SubstrateStatus::Pending]);
        let provider = provider_with(Arc::clone(&client));
        let handle =
            tokio::time::timeout(Duration::from_secs(10), provisioned(&provider, dir.path()))
                .await
                .expect("the provision poll must terminate inside its bound");

        assert!(matches!(
            handle.remote.as_ref().expect("remote").poll,
            PollOutcome::TimedOut { .. }
        ));
        let mut progress = Progress::default();
        let outcome = provider
            .readiness(&handle, &mut progress.sink())
            .await
            .expect("readiness");
        let ReadinessOutcome::ProviderFailed { detail } = outcome else {
            panic!("a poll timeout must be ProviderFailed, got {outcome:?}");
        };
        assert!(detail.contains("did not become ready within"), "{detail}");
    }

    /// A create failure records a provider failure too (nothing exists: the
    /// id is empty and teardown no-ops) — substrate failures never lose the
    /// audit trail to a bare run error.
    #[tokio::test]
    async fn remote_workspace_create_failure_is_recorded_and_teardown_noops() {
        let dir = tempfile::tempdir().expect("tempdir");
        let client = FakeSubstrateClient::failing_create("HTTP 401: bad token");
        let provider = provider_with(Arc::clone(&client));
        let handle = provisioned(&provider, dir.path()).await;

        let remote = handle.remote.as_ref().expect("remote state");
        assert!(remote.id.is_empty(), "no id when create failed");
        assert!(
            matches!(&remote.poll, PollOutcome::Failed { reason } if reason.contains("HTTP 401")),
            "{:?}",
            remote.poll
        );
        let mut progress = Progress::default();
        let outcome = provider
            .readiness(&handle, &mut progress.sink())
            .await
            .expect("readiness");
        assert!(
            matches!(outcome, ReadinessOutcome::ProviderFailed { .. }),
            "{outcome:?}"
        );

        for mode in [
            TeardownMode::Keep,
            TeardownMode::Hibernate,
            TeardownMode::Destroy,
        ] {
            provider
                .teardown(handle.clone(), mode)
                .await
                .expect("teardown with no workspace no-ops");
        }
        assert!(
            !client
                .ops()
                .iter()
                .any(|op| op.starts_with("stop:") || op.starts_with("delete:")),
            "nothing exists ⇒ no substrate teardown calls: {:?}",
            client.ops()
        );
    }

    /// Teardown modes map to the substrate lifecycle: Destroy=delete,
    /// Hibernate=stop, Keep=leave running (no call).
    #[tokio::test]
    async fn remote_workspace_teardown_modes_map_to_delete_stop_keep() {
        let dir = tempfile::tempdir().expect("tempdir");

        let client = FakeSubstrateClient::new(vec![SubstrateStatus::Ready]);
        let provider = provider_with(Arc::clone(&client));
        let handle = provisioned(&provider, dir.path()).await;
        provider
            .teardown(handle, TeardownMode::Keep)
            .await
            .expect("keep");
        assert!(
            !client
                .ops()
                .iter()
                .any(|op| op.starts_with("stop:") || op.starts_with("delete:")),
            "Keep leaves the workspace running — no teardown call: {:?}",
            client.ops()
        );

        let client = FakeSubstrateClient::new(vec![SubstrateStatus::Ready]);
        let provider = provider_with(Arc::clone(&client));
        let handle = provisioned(&provider, dir.path()).await;
        provider
            .teardown(handle, TeardownMode::Hibernate)
            .await
            .expect("hibernate");
        assert!(
            client.ops().contains(&"stop:ws-abc123".to_string())
                && !client.ops().iter().any(|op| op.starts_with("delete:")),
            "Hibernate is stop_workspace: {:?}",
            client.ops()
        );

        let client = FakeSubstrateClient::new(vec![SubstrateStatus::Ready]);
        let provider = provider_with(Arc::clone(&client));
        let handle = provisioned(&provider, dir.path()).await;
        provider
            .teardown(handle, TeardownMode::Destroy)
            .await
            .expect("destroy");
        assert!(
            client.ops().contains(&"delete:ws-abc123".to_string())
                && !client.ops().iter().any(|op| op.starts_with("stop:")),
            "Destroy is delete_workspace: {:?}",
            client.ops()
        );
    }

    /// `workspace.remote.idleAfterHours` (ticket `workspace-idle-hibernate`):
    /// the VALUE passes through require → provision → `create_workspace`
    /// verbatim and is recorded in the provisioned detail as substrate-owned
    /// policy — kranz records and passes through, never schedules.
    #[tokio::test]
    async fn idle_after_hours_passes_through_to_create_and_the_provisioned_detail() {
        // require() carries the optional value through (the only
        // non-required remote key); absent config leaves it None.
        let validated = RemoteConfig::require(Some(&RemoteWorkspaceConfig {
            base_url: Some("https://coder.internal.example.com".to_string()),
            template: Some("tmpl-baked-ami".to_string()),
            token_env: Some("CODER_SESSION_TOKEN".to_string()),
            idle_after_hours: Some(24.0),
        }))
        .expect("complete remote config validates with an idle policy");
        assert_eq!(validated.idle_after_hours, Some(24.0));
        let without = RemoteConfig::require(Some(&RemoteWorkspaceConfig {
            base_url: Some("https://coder.internal.example.com".to_string()),
            template: Some("tmpl-baked-ami".to_string()),
            token_env: Some("CODER_SESSION_TOKEN".to_string()),
            idle_after_hours: None,
        }))
        .expect("complete remote config validates without an idle policy");
        assert_eq!(without.idle_after_hours, None);

        let dir = tempfile::tempdir().expect("tempdir");
        let client = FakeSubstrateClient::new(vec![SubstrateStatus::Ready]);
        let provider = RemoteWorkspaceProvider::with_client(
            validated,
            Arc::clone(&client) as Arc<dyn SubstrateClient>,
        );
        let handle = provisioned(&provider, dir.path()).await;

        let created = client.created.lock().unwrap().clone();
        assert_eq!(created.len(), 1);
        assert_eq!(
            created[0].idle_after_hours,
            Some(24.0),
            "the idle policy VALUE crosses to the substrate verbatim"
        );
        assert!(
            handle
                .detail
                .as_deref()
                .unwrap()
                .contains("idle policy: hibernate after 24h (substrate-owned)"),
            "the provisioned event records the substrate-owned policy: {:?}",
            handle.detail
        );
    }

    /// No contract ⇒ nothing provisioned remotely (D-H): no substrate call,
    /// no credentials consulted, and readiness is trivially ready + silent.
    #[tokio::test]
    async fn remote_workspace_contract_less_provision_never_touches_the_substrate() {
        let dir = tempfile::tempdir().expect("tempdir");
        let client = FakeSubstrateClient::new(vec![SubstrateStatus::Ready]);
        let provider = provider_with(Arc::clone(&client));
        let handle = provider
            .provision(&spec(dir.path(), None))
            .await
            .expect("contract-less provision");
        assert!(handle.remote.is_none() && handle.contract.is_none());
        assert!(
            client.created.lock().unwrap().is_empty() && client.ops().is_empty(),
            "no contract ⇒ no substrate workspace (D-H)"
        );
        let mut progress = Progress::default();
        let outcome = provider
            .readiness(&handle, &mut progress.sink())
            .await
            .expect("readiness");
        assert!(matches!(outcome, ReadinessOutcome::Ready));
        assert!(progress.0.is_empty(), "contract-less ⇒ silent");
    }

    /// The production hook fails CLOSED when the token env var is unset —
    /// naming the var NAME (never a value) and the config key, owner
    /// operator.
    #[tokio::test]
    async fn remote_workspace_missing_creds_fail_closed_naming_the_env_var() {
        let dir = tempfile::tempdir().expect("tempdir");
        let var = format!("KRANZ_TEST_REMOTE_TOKEN_UNSET_{}", std::process::id());
        std::env::remove_var(&var); // defensive: prove unset
        let mut cfg = config();
        cfg.token_env = var.clone();
        let provider = RemoteWorkspaceProvider {
            config: cfg,
            client_hook: Arc::new(coder_client_from_env),
            ready_timeout: Duration::from_millis(50),
            poll_interval: Duration::from_millis(5),
        };
        let err = provider
            .provision(&spec(dir.path(), Some(remote_contract())))
            .await
            .expect_err("missing creds must fail closed at provision");
        let msg = err.to_string();
        assert!(msg.contains(&var), "names the env var NAME: {msg}");
        assert!(msg.contains("workspace.remote.tokenEnv"), "{msg}");
        assert!(msg.contains("owner: operator"), "{msg}");
        assert!(
            msg.contains("refusing rather than silently falling back"),
            "{msg}"
        );
    }

    #[test]
    fn remote_workspace_name_is_mission_owned_and_coder_shaped() {
        assert_eq!(workspace_name("m-test"), "kranz-remote-m-test");
        assert_eq!(workspace_name("M.Test X"), "kranz-remote-m-test-x");
        assert_eq!(
            workspace_name("..."),
            "kranz-remote----",
            "every non-alnum sanitizes to '-'"
        );
        assert_eq!(
            workspace_name(""),
            "kranz-remote-mission",
            "the prefix guarantees a valid leading character"
        );
    }

    #[test]
    fn coder_http_client_rejects_a_bad_base_url() {
        let err = CoderHttpClient::new("not a url", "tok")
            .err()
            .expect("invalid URL fails closed");
        assert!(
            err.to_string().contains("workspace.remote.baseUrl"),
            "{err}"
        );
        let err = CoderHttpClient::new("file:///etc/passwd", "tok")
            .err()
            .expect("non-http(s) schemes fail closed");
        assert!(err.to_string().contains("http(s)"), "{err}");
    }

    #[test]
    fn coder_http_status_mapping_is_conservative() {
        assert_eq!(
            parse_status(&serde_json::json!({"status": "running"})),
            SubstrateStatus::Ready
        );
        assert_eq!(
            parse_status(&serde_json::json!({"latest_build": {"status": "running"}})),
            SubstrateStatus::Ready,
            "stock Coder nests the state under latest_build"
        );
        assert!(matches!(
            parse_status(&serde_json::json!({"latest_build": {"status": "failed"}})),
            SubstrateStatus::Failed { .. }
        ));
        for unknown in [
            serde_json::json!({"latest_build": {"status": "starting"}}),
            serde_json::json!({"status": "stopping"}),
            serde_json::json!({}),
        ] {
            assert_eq!(
                parse_status(&unknown),
                SubstrateStatus::Pending,
                "unknown/absent states keep polling, never guess ready: {unknown}"
            );
        }
    }

    /// Find a subslice (the header/body separator) in a byte buffer.
    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    /// A one-request canned-response HTTP server on 127.0.0.1 — the ONLY
    /// network the tests touch. Reads one request (headers + content-length
    /// body), records it, and responds with `body` as JSON. Driven with
    /// `tokio::join!` against the client call, so no spawned task outlives
    /// the exchange.
    async fn serve_once(
        listener: tokio::net::TcpListener,
        body: &str,
        recorded: Arc<Mutex<Vec<String>>>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = socket.read(&mut chunk).await.expect("read request");
            assert!(n > 0, "connection closed before the full request arrived");
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&buf[..pos]).to_string();
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if buf.len() >= pos + 4 + content_length {
                    break;
                }
            }
        }
        recorded
            .lock()
            .unwrap()
            .push(String::from_utf8_lossy(&buf).to_string());
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        socket
            .write_all(response.as_bytes())
            .await
            .expect("write response");
    }

    /// The Coder-shaped wire mapping, proven against a loopback mock: auth
    /// header, create body (template + name + secret NAMES, no values), and
    /// response parsing (id/urls/auth/takeover).
    #[tokio::test]
    async fn coder_http_client_maps_the_coder_shaped_wire_over_loopback() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let recorded = Arc::new(Mutex::new(Vec::new()));
        let create_body = serde_json::json!({
            "id": "ws-loop1",
            "urls": [{"name": "app", "url": "https://app.example.com", "auth": true}],
            "takeover": "https://coder.example.com/@me/ws-loop1",
        })
        .to_string();

        let client = CoderHttpClient::new(&base_url, "test-session-token").expect("client");
        let create_spec = SubstrateWorkspaceSpec {
            template: "tmpl-1".to_string(),
            name: "kranz-remote-m-1".to_string(),
            env_names: vec!["DATABASE_URL".to_string()],
            idle_after_hours: None,
        };
        let ((), workspace) = tokio::join!(
            serve_once(listener, &create_body, Arc::clone(&recorded)),
            tokio::time::timeout(
                Duration::from_secs(10),
                client.create_workspace(&create_spec)
            )
        );
        let workspace = workspace.expect("bounded").expect("create_workspace");

        assert_eq!(workspace.id, "ws-loop1");
        assert_eq!(
            workspace.urls,
            vec![SubstrateUrl {
                name: "app".to_string(),
                url: "https://app.example.com".to_string(),
                auth: Some(true),
            }]
        );
        assert_eq!(
            workspace.takeover.as_deref(),
            Some("https://coder.example.com/@me/ws-loop1")
        );

        let requests = recorded.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert!(
            request.starts_with("POST /api/v2/users/me/workspaces "),
            "the create verb+path: {}",
            request.lines().next().unwrap_or("")
        );
        assert!(
            request.contains("coder-session-token: test-session-token"),
            "Coder's auth header carries the token (and the token appears NOWHERE else): {request}"
        );
        let body = request.split("\r\n\r\n").nth(1).expect("a JSON body");
        let body: serde_json::Value = serde_json::from_str(body).expect("body is JSON");
        assert_eq!(body["template_id"], "tmpl-1");
        assert_eq!(body["name"], "kranz-remote-m-1");
        assert_eq!(
            body["env_names"],
            serde_json::json!(["DATABASE_URL"]),
            "secret NAMES on the wire — never values"
        );
    }

    /// The create body carries `idle_after_hours` only when configured
    /// (ticket `workspace-idle-hibernate`) — additive on the wire, so a
    /// substrate without idle-policy support is never sent a key it must
    /// understand. The substrate owns the policy's execution.
    #[tokio::test]
    async fn coder_http_create_body_carries_idle_after_hours_only_when_configured() {
        for (idle_after_hours, expected) in
            [(Some(24.0), Some(serde_json::json!(24.0))), (None, None)]
        {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind loopback");
            let base_url = format!("http://{}", listener.local_addr().expect("addr"));
            let recorded = Arc::new(Mutex::new(Vec::new()));
            let client = CoderHttpClient::new(&base_url, "tok").expect("client");
            let spec = SubstrateWorkspaceSpec {
                template: "tmpl-1".to_string(),
                name: "kranz-remote-m-1".to_string(),
                env_names: vec![],
                idle_after_hours,
            };
            let ((), created) = tokio::join!(
                serve_once(listener, r#"{"id":"ws-1"}"#, Arc::clone(&recorded)),
                tokio::time::timeout(Duration::from_secs(10), client.create_workspace(&spec))
            );
            created.expect("bounded").expect("create_workspace");
            let request = recorded.lock().unwrap()[0].clone();
            let body = request.split("\r\n\r\n").nth(1).expect("a JSON body");
            let body: serde_json::Value = serde_json::from_str(body).expect("body is JSON");
            assert_eq!(
                body.get("idle_after_hours").cloned(),
                expected,
                "idle_after_hours rides the wire only when configured: {body}"
            );
        }
    }

    /// Status + transitions over the same loopback: nested `latest_build`
    /// parsing and the delete/stop transition bodies.
    #[tokio::test]
    async fn coder_http_client_status_and_transitions_over_loopback() {
        let recorded = Arc::new(Mutex::new(Vec::new()));

        // Status: stock Coder nests the state under latest_build.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let client = CoderHttpClient::new(&base_url, "tok").expect("client");
        let ((), status) = tokio::join!(
            serve_once(
                listener,
                r#"{"latest_build":{"status":"running"}}"#,
                Arc::clone(&recorded)
            ),
            tokio::time::timeout(Duration::from_secs(10), client.workspace_status("ws-9"))
        );
        let status = status.expect("bounded").expect("status");
        assert_eq!(status, SubstrateStatus::Ready);
        assert!(
            recorded.lock().unwrap()[0].starts_with("GET /api/v2/workspaces/ws-9 "),
            "status path: {:?}",
            recorded.lock().unwrap()[0].lines().next()
        );

        // stop + delete are workspace-build transitions.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let client = CoderHttpClient::new(&base_url, "tok").expect("client");
        let ((), stopped) = tokio::join!(
            serve_once(listener, "{}", Arc::clone(&recorded)),
            tokio::time::timeout(Duration::from_secs(10), client.stop_workspace("ws-9"))
        );
        stopped.expect("bounded").expect("stop");
        let stop_request = recorded.lock().unwrap().last().unwrap().clone();
        assert!(
            stop_request.starts_with("POST /api/v2/workspaces/ws-9/builds "),
            "{stop_request}"
        );
        assert!(
            stop_request.contains(r#""transition":"stop""#),
            "the stop transition body: {stop_request}"
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback");
        let base_url = format!("http://{}", listener.local_addr().expect("addr"));
        let client = CoderHttpClient::new(&base_url, "tok").expect("client");
        let ((), deleted) = tokio::join!(
            serve_once(listener, "{}", Arc::clone(&recorded)),
            tokio::time::timeout(Duration::from_secs(10), client.delete_workspace("ws-9"))
        );
        deleted.expect("bounded").expect("delete");
        let delete_request = recorded.lock().unwrap().last().unwrap().clone();
        assert!(
            delete_request.contains(r#""transition":"delete""#),
            "the delete transition body: {delete_request}"
        );
    }
}
