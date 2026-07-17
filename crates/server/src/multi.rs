//! Operator-owned repository catalog and the routing plane around per-repo
//! [`MissionHost`](crate::MissionHost) instances.

use crate::MissionHost;
use anyhow::{anyhow, Context, Result};
use kranz_engine::git_ops::GitRepo;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Semaphore;

fn default_max_concurrent_repos() -> usize {
    1
}

/// The `host` object in the operator's global `~/.kranz/config.json`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HostConfig {
    pub default_repo: Option<String>,
    pub max_concurrent_repos: usize,
    pub repos: Vec<RepoConfig>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            default_repo: None,
            max_concurrent_repos: default_max_concurrent_repos(),
            repos: Vec::new(),
        }
    }
}

/// One operator-owned repository catalog row.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoConfig {
    pub id: String,
    pub root: PathBuf,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub slack: RepoSlackConfig,
}

/// Per-repository Slack routing and authorization owned by global config.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RepoSlackConfig {
    pub channels: Vec<SlackChannelRoute>,
    pub allow_users: Vec<String>,
}

/// Exact Slack workspace/channel route.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct SlackChannelRoute {
    pub team: String,
    pub channel: String,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct GlobalConfig {
    host: HostConfig,
}

/// Load only the operator-owned `host` block from a global config file.
/// Missing files are equivalent to an empty catalog; malformed files fail
/// startup instead of silently selecting another repository.
pub fn load_host_config(path: &Path) -> Result<HostConfig> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(HostConfig::default())
        }
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
    };
    let global: GlobalConfig = serde_json::from_str(&text)
        .with_context(|| format!("invalid JSON in {}", path.display()))?;
    Ok(global.host)
}

/// A resolved catalog entry. The root is fixed at startup and requests only
/// ever resolve this entry by its validated id.
#[derive(Clone)]
pub struct RepoContext {
    config: RepoConfig,
    host: Option<Arc<MissionHost>>,
    unavailable_reason: Option<String>,
}

impl RepoContext {
    pub fn id(&self) -> &str {
        &self.config.id
    }

    pub fn root(&self) -> &Path {
        &self.config.root
    }

    pub fn config(&self) -> &RepoConfig {
        &self.config
    }

    pub fn host(&self) -> Option<&Arc<MissionHost>> {
        self.host.as_ref()
    }

    pub fn is_healthy(&self) -> bool {
        self.host.is_some()
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        self.unavailable_reason.as_deref()
    }

    fn summary(&self, is_default: bool) -> RepoSummary {
        RepoSummary {
            id: self.config.id.clone(),
            root: self.config.root.to_string_lossy().into_owned(),
            display_name: self
                .config
                .display_name
                .clone()
                .unwrap_or_else(|| self.config.id.clone()),
            group: self.config.group.clone(),
            pinned: self.config.pinned,
            is_default,
            status: if self.is_healthy() {
                "healthy".to_string()
            } else {
                "unavailable".to_string()
            },
            error: self.unavailable_reason.clone(),
        }
    }
}

/// Public `GET /api/repos` row.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoSummary {
    pub id: String,
    pub root: String,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    pub pinned: bool,
    pub is_default: bool,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Static process-lifetime catalog plus one existing single-repo host per
/// healthy root.
pub struct MultiRepoHost {
    repos: BTreeMap<String, Arc<RepoContext>>,
    default_repo: Option<String>,
    max_concurrent_repos: usize,
    operator_catalog: bool,
    global_run_permits: Arc<Semaphore>,
    auto_work_cursor: Mutex<usize>,
    auto_work: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl MultiRepoHost {
    /// Wrap an already-constructed host (including test hosts with injected
    /// backends) in a one-repository catalog.
    pub fn with_host(host: Arc<MissionHost>) -> Self {
        let root = host.repo_root().clone();
        let id = fallback_repo_id(&root);
        let context = Arc::new(RepoContext {
            config: RepoConfig {
                id: id.clone(),
                root,
                display_name: None,
                group: None,
                pinned: false,
                slack: RepoSlackConfig::default(),
            },
            host: Some(host),
            unavailable_reason: None,
        });
        Self {
            repos: BTreeMap::from([(id.clone(), context)]),
            default_repo: Some(id),
            max_concurrent_repos: 1,
            operator_catalog: false,
            global_run_permits: Arc::new(Semaphore::new(1)),
            auto_work_cursor: Mutex::new(0),
            auto_work: Mutex::new(None),
        }
    }

    /// Preserve the historical one-repository serve path when no global host
    /// catalog is configured.
    pub fn single(repo_root: PathBuf) -> Result<Self> {
        let canonical = canonical_or_lexical(&repo_root);
        let id = fallback_repo_id(&canonical);
        Self::from_config_with_mode(
            HostConfig {
                default_repo: Some(id.clone()),
                max_concurrent_repos: 1,
                repos: vec![RepoConfig {
                    id,
                    root: canonical,
                    display_name: None,
                    group: None,
                    pinned: false,
                    slack: RepoSlackConfig::default(),
                }],
            },
            false,
        )
    }

    /// Load the global catalog, falling back to the historical current repo
    /// only when the `host.repos` list is absent or empty.
    pub fn from_global_config(global_path: Option<&Path>, fallback_root: PathBuf) -> Result<Self> {
        let config = match global_path {
            Some(path) => load_host_config(path)?,
            None => HostConfig::default(),
        };
        if config.repos.is_empty() {
            Self::single(fallback_root)
        } else {
            Self::from_config(config)
        }
    }

    pub fn from_config(config: HostConfig) -> Result<Self> {
        Self::from_config_with_mode(config, true)
    }

    fn from_config_with_mode(mut config: HostConfig, operator_catalog: bool) -> Result<Self> {
        if config.repos.is_empty() {
            return Err(anyhow!("host.repos must contain at least one repository"));
        }
        if config.max_concurrent_repos == 0 {
            return Err(anyhow!("host.maxConcurrentRepos must be at least 1"));
        }

        let mut ids = HashSet::new();
        let mut roots = HashSet::new();
        let mut repos = BTreeMap::new();
        let global_run_permits = Arc::new(Semaphore::new(config.max_concurrent_repos));

        for repo in &mut config.repos {
            validate_repo_id(&repo.id)?;
            if !ids.insert(repo.id.clone()) {
                return Err(anyhow!("duplicate host repository id '{}'", repo.id));
            }
            if !repo.root.is_absolute() {
                return Err(anyhow!(
                    "host repository '{}' root must be absolute: {}",
                    repo.id,
                    repo.root.display()
                ));
            }
            repo.root = canonical_or_lexical(&repo.root);
            if !roots.insert(repo.root.clone()) {
                return Err(anyhow!(
                    "duplicate host repository root: {}",
                    repo.root.display()
                ));
            }

            let unavailable_reason = repository_unavailable_reason(&repo.root);
            let host = unavailable_reason.is_none().then(|| {
                Arc::new(MissionHost::new_with_global_run_permits(
                    repo.root.clone(),
                    Arc::clone(&global_run_permits),
                ))
            });
            repos.insert(
                repo.id.clone(),
                Arc::new(RepoContext {
                    config: repo.clone(),
                    host,
                    unavailable_reason,
                }),
            );
        }

        if let Some(default) = config.default_repo.as_deref() {
            validate_repo_id(default)?;
            if !repos.contains_key(default) {
                return Err(anyhow!(
                    "host.defaultRepo '{}' does not name a configured repository",
                    default
                ));
            }
        }

        Ok(Self {
            repos,
            default_repo: config.default_repo,
            max_concurrent_repos: config.max_concurrent_repos,
            operator_catalog,
            global_run_permits,
            auto_work_cursor: Mutex::new(0),
            auto_work: Mutex::new(None),
        })
    }

    pub fn contexts(&self) -> impl Iterator<Item = Arc<RepoContext>> + '_ {
        self.repos.values().cloned()
    }

    pub fn healthy_contexts(&self) -> impl Iterator<Item = Arc<RepoContext>> + '_ {
        self.contexts().filter(|context| context.is_healthy())
    }

    pub fn resolve(&self, id: &str) -> Option<Arc<RepoContext>> {
        self.repos.get(id).cloned()
    }

    /// One fair auto-work scheduling pass (test + serve watcher entrypoint).
    pub async fn auto_work_tick(&self) -> usize {
        self.auto_work_tick_inner().await
    }

    /// Existing unscoped routes are available for an explicit default, or
    /// when exactly one healthy root makes the choice unambiguous.
    pub fn compatibility_context(&self) -> Option<Arc<RepoContext>> {
        if let Some(default) = self.default_repo.as_deref() {
            return self.resolve(default);
        }
        let mut healthy = self.healthy_contexts();
        let only = healthy.next()?;
        healthy.next().is_none().then_some(only)
    }

    pub fn summaries(&self) -> Vec<RepoSummary> {
        self.repos
            .values()
            .map(|context| context.summary(self.default_repo.as_deref() == Some(context.id())))
            .collect()
    }

    pub fn max_concurrent_repos(&self) -> usize {
        self.max_concurrent_repos
    }

    pub fn uses_operator_catalog(&self) -> bool {
        self.operator_catalog
    }

    /// One fair auto-work scheduling pass. At most one pass over the static
    /// catalog is made; each successful start advances the next pass beyond
    /// that repository, while the shared semaphore enforces the configured
    /// concurrency bound for the complete drain lifetime.
    async fn auto_work_tick_inner(&self) -> usize {
        let contexts: Vec<_> = self.healthy_contexts().collect();
        if contexts.is_empty() {
            return 0;
        }
        let start = *self.auto_work_cursor.lock().expect("auto-work cursor lock") % contexts.len();
        let mut started = 0;
        let mut last_started = None;
        // Do not pre-check `available_permits()` — that races HTTP start/drain
        // and can skip a fair rotation pass. Each host's try_acquire inside
        // start/drain is the authoritative saturation gate (returns false).
        for offset in 0..contexts.len() {
            let index = (start + offset) % contexts.len();
            if let Some(host) = contexts[index].host() {
                if host.auto_work_tick().await {
                    started += 1;
                    last_started = Some(index);
                }
            }
        }
        if let Some(index) = last_started {
            *self.auto_work_cursor.lock().expect("auto-work cursor lock") =
                (index + 1) % contexts.len();
        }
        started
    }

    /// Spawn the single process-wide fair auto-work watcher at most once.
    pub fn ensure_auto_work_started(self: &Arc<Self>) {
        let mut guard = self.auto_work.lock().expect("multi auto-work lock");
        if guard.is_some() {
            return;
        }
        let host = Arc::clone(self);
        *guard = Some(tokio::spawn(async move {
            const AUTO_WORK_INTERVAL: Duration = Duration::from_secs(10);
            loop {
                tokio::time::sleep(AUTO_WORK_INTERVAL).await;
                let _ = host.auto_work_tick().await;
            }
        }));
    }

    /// Remaining process-wide run slots under `host.maxConcurrentRepos`.
    pub fn available_global_run_permits(&self) -> usize {
        self.global_run_permits.available_permits()
    }

    #[cfg(test)]
    fn auto_work_cursor(&self) -> usize {
        *self.auto_work_cursor.lock().expect("auto-work cursor lock")
    }

    /// Test-only catalog that reuses already-constructed hosts (injected
    /// backends + a shared semaphore) without rediscovering Claude.
    #[cfg(test)]
    fn from_injected_hosts(
        entries: Vec<(String, Arc<MissionHost>)>,
        max_concurrent_repos: usize,
        global_run_permits: Arc<Semaphore>,
    ) -> Self {
        let mut repos = BTreeMap::new();
        for (id, host) in entries {
            let root = host.repo_root().clone();
            repos.insert(
                id.clone(),
                Arc::new(RepoContext {
                    config: RepoConfig {
                        id: id.clone(),
                        root,
                        display_name: None,
                        group: None,
                        pinned: false,
                        slack: RepoSlackConfig::default(),
                    },
                    host: Some(host),
                    unavailable_reason: None,
                }),
            );
        }
        Self {
            repos,
            default_repo: None,
            max_concurrent_repos,
            operator_catalog: true,
            global_run_permits,
            auto_work_cursor: Mutex::new(0),
            auto_work: Mutex::new(None),
        }
    }
}

fn validate_repo_id(id: &str) -> Result<()> {
    let mut chars = id.chars();
    let valid_first = chars.next().is_some_and(|ch| ch.is_ascii_alphanumeric());
    let valid_rest = chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'));
    if valid_first && valid_rest {
        Ok(())
    } else {
        Err(anyhow!(
            "invalid host repository id '{id}': use ASCII letters, digits, '-' or '_', starting with a letter or digit"
        ))
    }
}

fn canonical_or_lexical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| lexical_normalize(path))
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn repository_unavailable_reason(root: &Path) -> Option<String> {
    if !root.exists() {
        return Some(format!(
            "repository root does not exist: {}",
            root.display()
        ));
    }
    if !root.is_dir() {
        return Some(format!(
            "repository root is not a directory: {}",
            root.display()
        ));
    }
    if !root.join(".git").exists() {
        return Some(format!(
            "repository root is not a Git worktree: {}",
            root.display()
        ));
    }
    GitRepo::open(root).err().map(|error| {
        format!(
            "repository root is not a usable Git worktree: {} ({error})",
            root.display()
        )
    })
}

fn fallback_repo_id(root: &Path) -> String {
    let raw = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repo");
    let mut id = String::new();
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            id.push(ch);
        } else if !id.ends_with('-') {
            id.push('-');
        }
    }
    let id = id.trim_matches('-');
    if id.is_empty() {
        "repo".to_string()
    } else {
        id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_git(root: &Path) {
        std::fs::create_dir_all(root).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(root)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn repo_config(id: &str, root: PathBuf) -> RepoConfig {
        RepoConfig {
            id: id.to_string(),
            root,
            display_name: None,
            group: None,
            pinned: false,
            slack: RepoSlackConfig::default(),
        }
    }

    #[test]
    fn catalog_rejects_duplicate_ids_and_canonical_roots() {
        let temp = tempfile::tempdir().unwrap();
        init_git(temp.path());
        let root = std::fs::canonicalize(temp.path()).unwrap();

        let duplicate_id = HostConfig {
            repos: vec![
                repo_config("one", root.clone()),
                repo_config("one", root.join("other")),
            ],
            ..HostConfig::default()
        };
        assert!(MultiRepoHost::from_config(duplicate_id)
            .err()
            .unwrap()
            .to_string()
            .contains("duplicate host repository id"));

        let duplicate_root = HostConfig {
            repos: vec![repo_config("one", root.clone()), repo_config("two", root)],
            ..HostConfig::default()
        };
        assert!(MultiRepoHost::from_config(duplicate_root)
            .err()
            .unwrap()
            .to_string()
            .contains("duplicate host repository root"));
    }

    #[test]
    fn missing_root_stays_visible_but_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        let catalog = MultiRepoHost::from_config(HostConfig {
            repos: vec![repo_config("missing", missing)],
            ..HostConfig::default()
        })
        .unwrap();
        let summary = &catalog.summaries()[0];
        assert_eq!(summary.status, "unavailable");
        assert!(summary.error.as_deref().unwrap().contains("does not exist"));
    }

    #[test]
    fn fake_dot_git_directory_is_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("fake");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        let catalog = MultiRepoHost::from_config(HostConfig {
            repos: vec![repo_config("fake", root)],
            ..HostConfig::default()
        })
        .unwrap();
        let summary = &catalog.summaries()[0];
        assert_eq!(summary.status, "unavailable");
        assert!(summary.error.as_deref().unwrap().contains("usable Git"));
    }

    #[test]
    fn unscoped_alias_requires_default_or_one_healthy_repo() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for root in [&a, &b] {
            init_git(root);
        }
        let catalog = MultiRepoHost::from_config(HostConfig {
            repos: vec![repo_config("a", a), repo_config("b", b)],
            ..HostConfig::default()
        })
        .unwrap();
        assert!(catalog.compatibility_context().is_none());
    }

    #[test]
    fn configured_concurrency_limit_is_shared_across_repository_hosts() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for root in [&a, &b] {
            init_git(root);
        }
        let catalog = MultiRepoHost::from_config(HostConfig {
            max_concurrent_repos: 1,
            repos: vec![repo_config("a", a), repo_config("b", b)],
            ..HostConfig::default()
        })
        .unwrap();
        let a = catalog.resolve("a").unwrap();
        let b = catalog.resolve("b").unwrap();
        let permit = a.host().unwrap().try_global_run_permit().unwrap().unwrap();
        assert_eq!(catalog.available_global_run_permits(), 0);
        let error = b.host().unwrap().try_global_run_permit().unwrap_err();
        assert_eq!(error.status, axum::http::StatusCode::CONFLICT);
        drop(permit);
        assert_eq!(catalog.available_global_run_permits(), 1);
        assert!(b.host().unwrap().try_global_run_permit().unwrap().is_some());
    }

    fn write_auto_work_config(root: &Path, enabled: bool) {
        let dir = root.join(".kranz");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.json"),
            serde_json::json!({ "autoWork": enabled }).to_string(),
        )
        .unwrap();
    }

    fn enqueue_placeholder(root: &Path, mission_id: &str) {
        kranz_engine::queue::enqueue(
            root,
            kranz_engine::queue::QueueEntry {
                mission_id: mission_id.to_string(),
                ticket_slug: None,
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();
    }

    async fn wait_for_idle_drain(host: &MissionHost) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let state = host.queue_state();
            if state["drain"]["live"] == false {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "drain never settled idle: {state}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn auto_work_tick_is_noop_when_global_permits_saturated() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for root in [&a, &b] {
            init_git(root);
            write_auto_work_config(root, true);
            enqueue_placeholder(root, "m-queued");
        }
        let permits = Arc::new(Semaphore::new(1));
        let backend: Arc<dyn kranz_engine::backend::AgentBackend> =
            Arc::new(kranz_engine::backend_mock::MockBackend::new());
        let host_a = Arc::new(MissionHost::with_backend_and_global_run_permits(
            a,
            Arc::clone(&backend),
            Arc::clone(&permits),
        ));
        let host_b = Arc::new(MissionHost::with_backend_and_global_run_permits(
            b,
            backend,
            Arc::clone(&permits),
        ));
        let catalog = MultiRepoHost::from_injected_hosts(
            vec![("a".into(), host_a), ("b".into(), host_b)],
            1,
            Arc::clone(&permits),
        );
        let _hold = Arc::clone(&permits).try_acquire_owned().unwrap();

        assert_eq!(catalog.auto_work_tick().await, 0);
        assert_eq!(catalog.auto_work_cursor(), 0);
    }

    #[tokio::test]
    async fn auto_work_tick_skips_busy_repo_and_starts_next_ready_repo() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for root in [&a, &b] {
            init_git(root);
            write_auto_work_config(root, true);
            enqueue_placeholder(root, "m-queued");
        }
        let busy = kranz_engine::queue::acquire_repo_busy(&a, "m-external").unwrap();
        let permits = Arc::new(Semaphore::new(1));
        let backend: Arc<dyn kranz_engine::backend::AgentBackend> =
            Arc::new(kranz_engine::backend_mock::MockBackend::new());
        let host_a = Arc::new(MissionHost::with_backend_and_global_run_permits(
            a,
            Arc::clone(&backend),
            Arc::clone(&permits),
        ));
        let host_b = Arc::new(MissionHost::with_backend_and_global_run_permits(
            b,
            backend,
            Arc::clone(&permits),
        ));
        let catalog = MultiRepoHost::from_injected_hosts(
            vec![
                ("a".into(), Arc::clone(&host_a)),
                ("b".into(), Arc::clone(&host_b)),
            ],
            1,
            permits,
        );

        assert_eq!(catalog.auto_work_tick().await, 1);
        assert!(!host_a.drain_is_live());
        assert!(host_b.drain_is_live());
        assert_eq!(catalog.auto_work_cursor(), 0);

        wait_for_idle_drain(&host_b).await;
        drop(busy);
    }

    #[tokio::test]
    async fn auto_work_tick_rotates_fairly_under_max_concurrent_one() {
        let temp = tempfile::tempdir().unwrap();
        let a = temp.path().join("a");
        let b = temp.path().join("b");
        for root in [&a, &b] {
            init_git(root);
            write_auto_work_config(root, true);
            enqueue_placeholder(root, "m-queued");
        }
        let permits = Arc::new(Semaphore::new(1));
        let backend: Arc<dyn kranz_engine::backend::AgentBackend> =
            Arc::new(kranz_engine::backend_mock::MockBackend::new());
        let host_a = Arc::new(MissionHost::with_backend_and_global_run_permits(
            a,
            Arc::clone(&backend),
            Arc::clone(&permits),
        ));
        let host_b = Arc::new(MissionHost::with_backend_and_global_run_permits(
            b,
            backend,
            Arc::clone(&permits),
        ));
        let catalog = MultiRepoHost::from_injected_hosts(
            vec![
                ("a".into(), Arc::clone(&host_a)),
                ("b".into(), Arc::clone(&host_b)),
            ],
            1,
            permits,
        );

        assert_eq!(catalog.auto_work_tick().await, 1);
        assert!(host_a.drain_is_live() || host_b.drain_is_live());
        // BTreeMap order is a then b; cursor starts at 0 so a starts first.
        assert!(host_a.drain_is_live());
        assert!(!host_b.drain_is_live());
        assert_eq!(catalog.auto_work_cursor(), 1);

        wait_for_idle_drain(&host_a).await;
        assert_eq!(catalog.auto_work_tick().await, 1);
        assert!(host_b.drain_is_live());
        assert_eq!(catalog.auto_work_cursor(), 0);
        wait_for_idle_drain(&host_b).await;
    }
}
