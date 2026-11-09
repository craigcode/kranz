//! Engine-owned terminal fixture provider. It can only address the exact
//! namespace admitted by OwnedContainer; no host command fallback exists.
use crate::gate_evaluation::subprocess::DockerEvaluator;
use crate::live_permission::{digest, Proposal};
use kranz_acp::terminal::{
    Budget, CleanupReceipt, Create, Error, ExitStatus, OutputSnapshot, OutputTail, Scope,
    TerminalProvider,
};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::{watch, Notify};

pub(crate) const FIXTURE_IMAGE: &str =
    "python@sha256:540c7d91f98ff6880174c40e99067bf5941eb54d818a7a5e094d188b196a934d";
const OPERATION_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_TERMINALS: usize = 8;
const MAX_CREATIONS: usize = 32;
const MAX_OUTPUT_READS_PER_SECOND: usize = 10;
pub(crate) const MAX_LAUNCH_BYTES: usize = kranz_acp::terminal::MAX_REQUEST_BYTES + 4096;

/// Code-owned fixture policy, never deserialized from peer or repository input.
/// The selected resource profile still has a 600-second session ceiling.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Budgets {
    command_ms: u64,
    retention_ms: u64,
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
    #[cfg(test)]
    pub(crate) fn fixture(command_ms: u64, retention_ms: u64) -> Self {
        assert!((1..=600_000).contains(&command_ms));
        assert!((1..=300_000).contains(&retention_ms));
        Self {
            command_ms,
            retention_ms,
        }
    }
}

pub(crate) struct Context {
    pub(crate) client: DockerEvaluator,
    pub(crate) container_id: String,
    pub(crate) image: String,
    pub(crate) workspace: PathBuf,
    pub(crate) scratch: PathBuf,
    pub(crate) alive: Arc<AtomicBool>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Authority {
    nonce: String,
    scope: Scope,
    digest: String,
    deadline: chrono::DateTime<chrono::Utc>,
    expires_at: tokio::time::Instant,
    action_digest: String,
}

struct State {
    tail: OutputTail,
    started: bool,
    exit: Option<ExitStatus>,
    released: bool,
    releasing: bool,
    driver_ok: bool,
    failed: bool,
    budget: Option<Budget>,
    retention_expired: bool,
    expired_budget: Option<Budget>,
    output_reads: VecDeque<tokio::time::Instant>,
}

struct Entry {
    deadline: tokio::time::Instant,
    state: Mutex<State>,
    stdin: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    changed: watch::Sender<()>,
    abort: Mutex<Option<tokio::task::AbortHandle>>,
}

#[derive(Default)]
struct Registry {
    entries: HashMap<String, Arc<Entry>>,
    authorities: HashSet<String>,
    reservations: HashSet<String>,
    retired: HashMap<String, (ExitStatus, Option<Budget>)>,
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

pub(crate) struct Provider {
    context: Context,
    pub(crate) scope: Scope,
    registry: Arc<Mutex<Registry>>,
    budgets: Budgets,
    session_deadline: tokio::time::Instant,
    failed: Arc<AtomicBool>,
    failure: Arc<Notify>,
    receipts: Receipts,
}

impl Provider {
    pub(crate) fn new(
        context: Context,
        run: String,
        engine_session: String,
        peer_session: String,
        budgets: Budgets,
    ) -> Self {
        Self {
            context,
            scope: Scope {
                run_id: run,
                engine_session_id: engine_session,
                peer_session_id: peer_session,
                generation: uuid::Uuid::new_v4().to_string(),
            },
            registry: Arc::new(Mutex::new(Registry::default())),
            budgets,
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

    pub(crate) fn encode_launch(&self, action: &Create, session_ms: u64) -> Result<Vec<u8>, Error> {
        let mut launch = serde_json::to_vec(&json!({
            "workspace": self.context.workspace,
            "scratch": self.context.scratch,
            "action": action,
            "budgets": self.budgets,
            "sessionRemainingMs": session_ms,
        }))
        .map_err(|_| Error::InvalidRequest)?;
        launch.push(b'\n');
        if launch.len() > MAX_LAUNCH_BYTES {
            return Err(Error::InvalidRequest);
        }
        Ok(launch)
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

    /// The first fixture admits immutable image executables and the bound mount
    /// root only. Nested directories and arbitrary environments fail closed.
    pub(crate) fn normalize(&self, mut action: Create) -> Result<Create, Error> {
        let limit = action.validate()?;
        if action.session_id != self.scope.peer_session_id
            || !matches!(
                action.command.as_str(),
                "/bin/sh" | "/usr/local/bin/python3"
            )
            || action.env.iter().any(|v| {
                !matches!(
                    v.name.as_str(),
                    "LANG" | "LC_ALL" | "TERM" | "NO_COLOR" | "CI"
                )
            })
        {
            return Err(Error::InvalidRequest);
        }
        let root = self
            .context
            .workspace
            .to_str()
            .ok_or(Error::InvalidRequest)?;
        if action.cwd.as_deref().is_some_and(|cwd| cwd != root) {
            return Err(Error::InvalidRequest);
        }
        action.cwd = Some(root.to_owned());
        action.output_byte_limit = Some(limit as u64);
        action.meta = None;
        Ok(action)
    }

    pub(crate) fn action(&self, request: &Create) -> Result<Value, Error> {
        let request = self.normalize(request.clone())?;
        let mut environment = json!({"PATH":"/usr/local/bin:/usr/bin:/bin","LANG":"C.UTF-8",
            "HOME":self.context.scratch,"TMPDIR":self.context.scratch});
        for variable in &request.env {
            environment[&variable.name] = json!(variable.value);
        }
        Ok(
            json!({"kind":"execute", "rawInput": request, "effectiveEnvironment":environment,
            "terminalScope":self.scope, "namespaceImage":self.context.image,
            "namespaceId":self.context.container_id,"terminalBudgets":self.budgets,
            "sessionCeilingMs":600_000,"oomScoreAdjRequested":500}),
        )
    }

    /// Reserve before human consent. Reservations and issued authorities both
    /// occupy capacity until denial or the atomic transition to a live entry.
    pub(crate) fn reserve(&self, proposal: &str) -> Result<(), Error> {
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

    pub(crate) fn cancel_reservation(&self, proposal: &str) -> Result<(), Error> {
        self.registry
            .lock()
            .map_err(|_| Error::Unavailable)?
            .reservations
            .remove(proposal);
        Ok(())
    }

    pub(crate) fn exit_status(&self, id: &str) -> Option<ExitStatus> {
        let registry = self.registry.lock().ok()?;
        if let Some((exit, _)) = registry.retired.get(id) {
            return Some(exit.clone());
        }
        let exit = registry.entries.get(id)?.state.lock().ok()?.exit.clone();
        exit
    }

    /// Called only after the backend validates the engine's durable response.
    /// The authority is never deserialized from ACP or reconstructed on resume.
    pub(crate) fn authorize(
        &self,
        action: &Create,
        proposal: &Proposal,
    ) -> Result<Authority, Error> {
        let mut registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
        // Burn the reservation first so expired or mismatched proposals never
        // leak a reserved capacity slot.
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

    fn entry(&self, scope: &Scope, id: &str) -> Result<Arc<Entry>, Error> {
        self.check_scope(scope)?;
        let registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
        if let Some((_, Some(budget))) = registry.retired.get(id) {
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
            if state.releasing {
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
        // Release all live handles concurrently (at most eight) so cumulative
        // drain latencies stay well within the outer session close deadline.
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

    #[cfg(test)]
    pub(crate) fn insert_test_active_handle(&self, id: &str) {
        let (changed, _) = watch::channel(());
        let entry = Arc::new(Entry {
            deadline: tokio::time::Instant::now() + Duration::from_secs(60),
            state: Mutex::new(State {
                tail: OutputTail::new(64).unwrap(),
                started: true,
                exit: None,
                released: false,
                releasing: false,
                driver_ok: false,
                failed: false,
                budget: None,
                retention_expired: false,
                expired_budget: None,
                output_reads: VecDeque::with_capacity(MAX_OUTPUT_READS_PER_SECOND),
            }),
            stdin: tokio::sync::Mutex::new(None),
            changed,
            abort: Mutex::new(None),
        });
        self.registry
            .lock()
            .unwrap()
            .entries
            .insert(id.to_owned(), entry);
    }
}

impl Drop for Provider {
    fn drop(&mut self) {
        self.stop();
    }
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

async fn control(entry: &Entry, op: &str) -> Result<(), Error> {
    tokio::time::timeout(Duration::from_secs(1), async {
        let mut stdin = entry.stdin.lock().await;
        let stdin = stdin.as_mut().ok_or(Error::Unavailable)?;
        let bytes = format!("{}\n", json!({"op":op}));
        stdin
            .write_all(bytes.as_bytes())
            .await
            .map_err(|_| Error::Unavailable)?;
        stdin.flush().await.map_err(|_| Error::Unavailable)
    })
    .await
    .map_err(|_| Error::Unavailable)?
}

impl TerminalProvider for Provider {
    type Authority = Authority;

    async fn create(
        &self,
        scope: Scope,
        action: Create,
        authority: Authority,
    ) -> Result<String, Error> {
        let (launch, id, entry, session_ms) = {
            let mut registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
            // Burn authority even on invalid input, without releasing capacity
            // between consuming it and inserting the live entry.
            let known = registry.authorities.remove(&authority.nonce);
            self.check_scope(&scope)?;
            if !known {
                return Err(Error::NotAuthorized);
            }
            let action = self.normalize(action)?;
            if chrono::Utc::now() >= authority.deadline
                || tokio::time::Instant::now() >= authority.expires_at
                || authority.scope != scope
                || authority.digest != digest(&action).map_err(|_| Error::InvalidRequest)?
            {
                return Err(Error::NotAuthorized);
            }
            let session_ms = self.remaining_session_ms();
            if session_ms == 0 {
                return Err(Error::BudgetExceeded(Budget::SessionLifetime));
            }
            let limit = action
                .output_byte_limit
                .unwrap_or(kranz_acp::terminal::DEFAULT_OUTPUT_BYTES as u64)
                as usize;
            let tail = OutputTail::new(limit)?;
            let launch = self.encode_launch(&action, session_ms)?;
            let id = format!("terminal-{}", uuid::Uuid::new_v4());
            let (changed, _) = watch::channel(());
            // Includes launch, guest cleanup/drain and Docker control reaping.
            let deadline = tokio::time::Instant::now()
                + Duration::from_millis(
                    session_ms.min(self.budgets.command_ms + self.budgets.retention_ms),
                )
                + 3 * OPERATION_TIMEOUT;
            let entry = Arc::new(Entry {
                deadline,
                state: Mutex::new(State {
                    tail,
                    started: false,
                    exit: None,
                    released: false,
                    releasing: false,
                    driver_ok: false,
                    failed: false,
                    budget: None,
                    retention_expired: false,
                    expired_budget: None,
                    output_reads: VecDeque::with_capacity(MAX_OUTPUT_READS_PER_SECOND),
                }),
                stdin: tokio::sync::Mutex::new(None),
                changed,
                abort: Mutex::new(None),
            });
            registry.creations += 1;
            registry.entries.insert(id.clone(), entry.clone());
            (launch, id, entry, session_ms)
        };
        let started = async {
            let mut command = self.context.client.attached_command(&[
                "exec".into(),
                "--interactive".into(),
                self.context.container_id.clone(),
                "/usr/local/bin/python3".into(),
                "-I".into(),
                "-S".into(),
                "-u".into(),
                "/kranz-owned-session/terminal.py".into(),
            ]);
            command
                .current_dir("/")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .process_group(0);
            let mut child =
                crate::command_exec::ControlChild(command.spawn().map_err(|_| Error::Unavailable)?);
            let mut stdin = child.0.stdin.take().ok_or(Error::Unavailable)?;
            let stdout = child.0.stdout.take().ok_or(Error::Unavailable)?;
            stdin
                .write_all(&launch)
                .await
                .map_err(|_| Error::Unavailable)?;
            stdin.flush().await.map_err(|_| Error::Unavailable)?;
            *entry.stdin.lock().await = Some(stdin);
            let capture = entry.clone();
            let failed = self.failed.clone();
            let failure = self.failure.clone();
            let receipts = self.receipts.clone();
            let binding = json!({"terminalId":id,"scope":scope,"actionDigest":authority.action_digest,
                "terminalBudgets":self.budgets,"sessionRemainingMs":session_ms});
            let registry = self.registry.clone();
            let handle = id.clone();
            let task = tokio::spawn(async move {
                let outcome = tokio::time::timeout_at(
                    capture.deadline,
                    capture_driver(&capture, &mut child, stdout, receipts, binding),
                )
                .await;
                if !matches!(outcome, Ok(Ok(()))) {
                    if let Ok(mut state) = capture.state.lock() {
                        state.failed = true;
                    }
                    failed.store(true, Ordering::Release);
                    failure.notify_one();
                }
                // Expiry only frees capacity after driver exit and confirmed
                // descendant cleanup. Tombstones are bounded by total creations.
                if matches!(outcome, Ok(Ok(()))) {
                    if let Ok(mut registry) = registry.lock() {
                        if let Ok(state) = capture.state.lock() {
                            if state.retention_expired {
                                if let Some(exit) = &state.exit {
                                    registry.entries.remove(&handle);
                                    registry.retired.insert(
                                        handle,
                                        (
                                            exit.clone(),
                                            Some(state.expired_budget.unwrap_or(Budget::Retention)),
                                        ),
                                    );
                                }
                            }
                        }
                    }
                }
                capture.changed.send_replace(());
            });
            *entry.abort.lock().map_err(|_| Error::Unavailable)? = Some(task.abort_handle());
            wait_state(&entry, |s| s.started.then_some(())).await
        };
        match tokio::time::timeout(OPERATION_TIMEOUT, started).await {
            Ok(Ok(())) => {
                self.check_scope(&scope)?;
                Ok(id)
            }
            _ => {
                self.fail();
                Err(Error::CleanupUnconfirmed)
            }
        }
    }

    async fn output(&self, scope: Scope, id: String) -> Result<OutputSnapshot, Error> {
        let entry = self.entry(&scope, &id)?;
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
        Ok(state.tail.snapshot(state.exit.clone()))
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
        {
            let state = entry.state.lock().map_err(|_| Error::Unavailable)?;
            if state.retention_expired {
                return Err(Error::BudgetExceeded(
                    state.expired_budget.unwrap_or(Budget::Retention),
                ));
            }
            if state.released {
                return Err(Error::InvalidHandle);
            }
        }
        let result = tokio::time::timeout(OPERATION_TIMEOUT, async {
            // A natural exit or retention expiry can race the control write.
            // The receipt, not pipe writability, decides whether cleanup worked.
            let _ = control(&entry, "kill").await;
            let exit_status = wait_state(&entry, |s| s.exit.clone()).await?;
            Ok(CleanupReceipt {
                exit_status,
                terminal_id: id,
                descendants_reaped: true,
                output_drained: true,
                released: false,
            })
        })
        .await
        .map_err(|_| Error::CleanupUnconfirmed)
        .and_then(|v| v);
        if result.is_err() {
            self.fail();
        }
        result
    }

    async fn release(&self, scope: Scope, id: String) -> Result<CleanupReceipt, Error> {
        // Invalidate before awaiting so concurrent/late operations cannot
        // resurrect a handle whose resources may already have been removed.
        self.check_scope(&scope)?;
        let entry = {
            let registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
            if let Some((_, Some(budget))) = registry.retired.get(&id) {
                return Err(Error::BudgetExceeded(*budget));
            }
            let entry = registry
                .entries
                .get(&id)
                .cloned()
                .ok_or(Error::InvalidHandle)?;
            {
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
            }
            entry
        };
        let result = tokio::time::timeout(OPERATION_TIMEOUT, async {
            let _ = control(&entry, "release").await;
            let exit_status = wait_state(&entry, |s| {
                (s.released && s.driver_ok)
                    .then(|| s.exit.clone())
                    .flatten()
            })
            .await?;
            Ok(CleanupReceipt {
                terminal_id: id.clone(),
                exit_status,
                descendants_reaped: true,
                output_drained: true,
                released: true,
            })
        })
        .await
        .map_err(|_| Error::CleanupUnconfirmed)
        .and_then(|v| v);
        if let Ok(receipt) = &result {
            let expired = entry.state.lock().ok().and_then(|s| s.expired_budget);
            let mut registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
            registry.entries.remove(&id);
            registry
                .retired
                .entry(id)
                .or_insert((receipt.exit_status.clone(), expired));
        } else {
            self.fail();
            if let Some(abort) = entry.abort.lock().map_err(|_| Error::Unavailable)?.take() {
                abort.abort();
            }
        }
        result
    }
}

async fn capture_driver(
    entry: &Entry,
    child: &mut crate::command_exec::ControlChild,
    stdout: tokio::process::ChildStdout,
    receipts: Receipts,
    binding: Value,
) -> Result<(), Error> {
    let mut lines = crate::stream_bounds::BoundedLines::new_strict(stdout);
    while let Some(line) = lines.next_line().await.map_err(|_| Error::Unavailable)? {
        if line.len() > 32 * 1024 {
            return Err(Error::InvalidRequest);
        }
        let value =
            crate::strict_json::parse(line.as_bytes()).map_err(|_| Error::InvalidRequest)?;
        {
            let mut state = entry.state.lock().map_err(|_| Error::Unavailable)?;
            match value.get("event").and_then(Value::as_str) {
                Some("started") if !state.started => {
                    let hint = &value["oomScoreAdj"];
                    if hint["requested"] != 500
                        || !(hint["observed"].is_null()
                            || hint["observed"]
                                .as_i64()
                                .is_some_and(|n| (-1000..=1000).contains(&n)))
                    {
                        return Err(Error::InvalidRequest);
                    }
                    let mut started = binding.clone();
                    started["oomScoreAdj"] = hint.clone();
                    receipts.record(json!({"terminalStarted":started}))?;
                    state.started = true;
                }
                Some("output") if state.started && state.exit.is_none() => state
                    .tail
                    .push(value["text"].as_str().ok_or(Error::InvalidRequest)?),
                Some("exit")
                    if state.started
                        && state.exit.is_none()
                        && value["descendantsReaped"] == true
                        && value["outputDrained"] == true =>
                {
                    let status: ExitStatus = serde_json::from_value(value["status"].clone())
                        .map_err(|_| Error::InvalidRequest)?;
                    if status.exit_code.is_some() == status.signal.is_some() {
                        return Err(Error::InvalidRequest);
                    }
                    state.budget = match value["budget"].as_str() {
                        None if value["budget"].is_null() => None,
                        Some("commandLifetime") => Some(Budget::CommandLifetime),
                        Some("sessionLifetime") => Some(Budget::SessionLifetime),
                        _ => return Err(Error::InvalidRequest),
                    };
                    receipts.record(json!({"terminalExited": {
                        "binding":binding,"exitStatus":status,"descendantsReaped":true,"outputDrained":true,"budget":state.budget,
                    }}))?;
                    state.exit = Some(status);
                }
                Some("released") if state.exit.is_some() && !state.released => {
                    state.retention_expired = value["retentionExpired"]
                        .as_bool()
                        .ok_or(Error::InvalidRequest)?;
                    state.expired_budget = match (state.retention_expired, value["budget"].as_str())
                    {
                        (false, None) if value["budget"].is_null() => None,
                        (true, Some("retention")) => Some(Budget::Retention),
                        (true, Some("sessionLifetime")) => Some(Budget::SessionLifetime),
                        _ => return Err(Error::InvalidRequest),
                    };
                    state.released = true;
                }
                _ => return Err(Error::CleanupUnconfirmed),
            }
        }
        entry.changed.send_replace(());
    }
    crate::command_exec::control_leader_exited(child.0.id().ok_or(Error::Unavailable)?)
        .await
        .map_err(|_| Error::Unavailable)?;
    crate::backend_claude::kill_unreaped_group(&child.0);
    let status = child.0.wait().await.map_err(|_| Error::Unavailable)?;
    let mut state = entry.state.lock().map_err(|_| Error::Unavailable)?;
    if !status.success() || !state.released {
        return Err(Error::CleanupUnconfirmed);
    }
    receipts.record(
        json!({"terminalReleased":{"binding":binding,"confirmed":true,
        "retentionExpired":state.retention_expired,"budget":state.expired_budget,"exitStatus":state.exit}}),
    )?;
    state.driver_ok = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Provider, Create, Proposal) {
        // No subprocess is allowed in these refusal tests. /usr/bin/false is a
        // sentinel control executable, never a substitute containment proof.
        let provider = Provider::new(
            Context {
                client: DockerEvaluator::new(std::path::Path::new("/usr/bin/false")).unwrap(),
                container_id: "a".repeat(64),
                image: FIXTURE_IMAGE.into(),
                workspace: "/workspace".into(),
                scratch: "/scratch".into(),
                alive: Arc::new(AtomicBool::new(true)),
            },
            "run".into(),
            "engine".into(),
            "peer".into(),
            Budgets::default(),
        );
        let action: Create = serde_json::from_value(json!({"sessionId":"peer","command":"/bin/sh",
            "args":["-c","echo harmless"]}))
        .unwrap();
        let value = provider.action(&action).unwrap();
        let options = vec![json!({"optionId":"yes","kind":"allow_once"})];
        let now = chrono::Utc::now();
        let proposal = Proposal {
            id: "proposal".into(),
            engine_session_id: "engine".into(),
            peer_session_id: "peer".into(),
            peer_request_id: json!(1),
            tool_call_id: "call".into(),
            action_digest: digest(&value).unwrap(),
            options_digest: digest(&options).unwrap(),
            action: value,
            options,
            observed_at: now,
            deadline: now + chrono::Duration::seconds(10),
            prohibition: None,
        };
        (provider, action, proposal)
    }

    #[tokio::test]
    async fn terminal_authority_is_exact_expiring_and_consumed_on_failed_attempt() {
        let (provider, action, proposal) = fixture();
        for mutation in ["argv", "cwd", "env", "output"] {
            provider.reserve(&proposal.id).unwrap();
            let authority = provider.authorize(&action, &proposal).unwrap();
            let replay = Authority {
                nonce: authority.nonce.clone(),
                scope: authority.scope.clone(),
                digest: authority.digest.clone(),
                deadline: authority.deadline,
                expires_at: authority.expires_at,
                action_digest: authority.action_digest.clone(),
            };
            let mut changed = action.clone();
            match mutation {
                "argv" => changed.args.push("different".into()),
                "cwd" => changed.cwd = Some("/elsewhere".into()),
                "env" => changed.env.push(terminal_env("LANG", "different")),
                _ => changed.output_byte_limit = Some(2),
            }
            assert!(provider
                .create(provider.scope.clone(), changed, authority)
                .await
                .is_err());
            // Even a request refused by normalization must burn the authority.
            assert!(provider
                .create(provider.scope.clone(), action.clone(), replay)
                .await
                .is_err());
        }
        assert_eq!(provider.registry.lock().unwrap().creations, 0);
        let mut expired = proposal.clone();
        expired.observed_at -= chrono::Duration::seconds(20);
        expired.deadline -= chrono::Duration::seconds(20);
        assert!(provider.authorize(&action, &expired).is_err());
        let mut prohibited = proposal.clone();
        prohibited.prohibition = Some("deny".into());
        assert!(provider.authorize(&action, &prohibited).is_err());
        let mut changed = proposal.clone();
        changed.action["rawInput"]["args"] = json!(["different"]);
        changed.action_digest = digest(&changed.action).unwrap();
        assert!(provider.authorize(&action, &changed).is_err());
        provider.reserve(&proposal.id).unwrap();
        let mut authority = provider.authorize(&action, &proposal).unwrap();
        authority.deadline = chrono::Utc::now() - chrono::Duration::seconds(1);
        assert_eq!(
            provider
                .create(provider.scope.clone(), action, authority)
                .await,
            Err(Error::NotAuthorized)
        );
    }

    #[test]
    fn terminal_reservations_precede_consent_and_count_issued_authorities() {
        let (provider, action, proposal) = fixture();
        provider.reserve(&proposal.id).unwrap();
        let _authority = provider.authorize(&action, &proposal).unwrap();
        for i in 1..MAX_TERMINALS {
            provider.reserve(&i.to_string()).unwrap();
        }
        assert_eq!(
            provider.reserve("excess"),
            Err(Error::BudgetExceeded(Budget::Capacity))
        );
        provider.cancel_reservation("1").unwrap();
        provider.reserve("replacement").unwrap();
        assert_eq!(provider.registry.lock().unwrap().creations, 0);
        let (provider, _, _) = fixture();
        provider.registry.lock().unwrap().creations = MAX_CREATIONS - 1;
        provider.reserve("last").unwrap();
        assert_eq!(
            provider.reserve("excess"),
            Err(Error::BudgetExceeded(Budget::Creations))
        );
        provider.cancel_reservation("last").unwrap();
        provider.reserve("replacement").unwrap();
    }

    #[test]
    fn terminal_receipts_bound_three_events_for_each_creation() {
        let receipts = Receipts::default();
        for _ in 0..3 * MAX_CREATIONS {
            receipts.record(json!({})).unwrap();
        }
        assert_eq!(receipts.record(json!({})), Err(Error::Unavailable));
    }

    fn terminal_env(name: &str, value: &str) -> kranz_acp::terminal::EnvironmentVariable {
        kranz_acp::terminal::EnvironmentVariable {
            name: name.into(),
            value: value.into(),
        }
    }

    #[tokio::test]
    async fn terminal_scope_and_generation_reject_before_provider_execution() {
        let (provider, action, proposal) = fixture();
        for field in 0..4 {
            let mut scope = provider.scope.clone();
            match field {
                0 => scope.run_id = "foreign".into(),
                1 => scope.engine_session_id = "foreign".into(),
                2 => scope.peer_session_id = "foreign".into(),
                _ => scope.generation = "old".into(),
            }
            assert_eq!(
                provider.output(scope.clone(), "forged".into()).await,
                Err(Error::InvalidHandle)
            );
            assert_eq!(
                provider.wait_for_exit(scope.clone(), "forged".into()).await,
                Err(Error::InvalidHandle)
            );
            assert_eq!(
                provider.kill(scope.clone(), "forged".into()).await,
                Err(Error::InvalidHandle)
            );
            assert_eq!(
                provider.release(scope.clone(), "forged".into()).await,
                Err(Error::InvalidHandle)
            );
            provider.reserve(&proposal.id).unwrap();
            let authority = provider.authorize(&action, &proposal).unwrap();
            assert_eq!(
                provider.create(scope, action.clone(), authority).await,
                Err(Error::InvalidHandle)
            );
        }
        let (other, _, _) = fixture();
        assert_ne!(provider.scope.generation, other.scope.generation);
        assert_eq!(
            provider
                .output(provider.scope.clone(), "forged".into())
                .await,
            Err(Error::InvalidHandle)
        );
        provider.stop();
        assert_eq!(
            provider
                .output(provider.scope.clone(), "forged".into())
                .await,
            Err(Error::Unavailable)
        );
        assert_eq!(provider.registry.lock().unwrap().creations, 0);
    }
    #[test]
    fn terminal_budget_classification_requires_confirmed_exhaustion_and_surfaces_message() {
        use crate::acp_resources::{
            ContainerState, Counters, FailureClass, ResourceEvidence, Sample,
        };
        let now = chrono::Utc::now();
        let mut evidence = ResourceEvidence {
            container_id: "a".repeat(64),
            image: FIXTURE_IMAGE.into(),
            supervisor_sha256: "b".repeat(64),
            owner: "c".repeat(32),
            limits: crate::acp_worker::Resources {
                memory_mib: 256,
                cpu_millis: 1_000,
                pids: 64,
                nofile: 256,
                fsize_mib: 64,
                tmpfs_mib: 64,
                session_seconds: 600,
            },
            observation_started_at: now,
            observation_finished_at: now,
            namespace_cleanup_confirmed: true,
            state: Some(ContainerState {
                running: false,
                oom_killed: false,
                exit_code: 1,
            }),
            sample: Some(Sample {
                owner: "c".repeat(32),
                elapsed_ms: 100,
                complete: true,
                wall_clock_expired: false,
                baseline: Counters {
                    oom_kill: Some(0),
                    pids_max: Some(0),
                    throttled_usec: Some(0),
                },
                current: Counters {
                    oom_kill: Some(0),
                    pids_max: Some(0),
                    throttled_usec: Some(0),
                },
                usage: None,
            }),
            unavailable: Vec::new(),
            failure: None,
            terminal_budget_hit: Default::default(),
        };
        evidence.classify_with_terminal_budget(false, true);
        assert_eq!(evidence.failure, None);
        evidence.classify_with_terminal_budget(true, true);
        assert_eq!(evidence.failure, Some(FailureClass::TerminalBudget));
        assert!(evidence
            .failure_message()
            .unwrap()
            .contains("ACP terminal budget exhaustion observed"));

        // Concurrent memory OOM + terminal budget hit stays Other rather than guessing one cause.
        evidence.state.as_mut().unwrap().oom_killed = true;
        evidence.classify_with_terminal_budget(true, true);
        assert_eq!(evidence.failure, Some(FailureClass::Other));
    }

    #[tokio::test]
    async fn terminal_near_max_request_bytes_fits_launch_envelope_and_preserves_prespawn_errors() {
        let (provider, _, _) = fixture();
        // Exact 32,768-byte Create with cwd/outputByteLimit omitted (expands on normalize).
        let mut max_create = Create {
            session_id: "peer".into(),
            command: "/bin/sh".into(),
            args: vec!["-c".into(), String::new()],
            env: vec![],
            cwd: None,
            output_byte_limit: None,
            meta: None,
        };
        let overhead = serde_json::to_vec(&max_create).unwrap().len();
        max_create.args[1] = "x".repeat(kranz_acp::terminal::MAX_REQUEST_BYTES - overhead);
        assert_eq!(
            serde_json::to_vec(&max_create).unwrap().len(),
            kranz_acp::terminal::MAX_REQUEST_BYTES
        );
        assert!(max_create.validate().is_ok());

        let normalized = provider.normalize(max_create.clone()).unwrap();
        let launch = provider.encode_launch(&normalized, 600_000).unwrap();
        assert!(launch.ends_with(b"\n"));
        assert!(
            launch.len() <= MAX_LAUNCH_BYTES,
            "launch len {} exceeded MAX_LAUNCH_BYTES {}",
            launch.len(),
            MAX_LAUNCH_BYTES
        );

        // If workspace/scratch metadata makes the internal launch envelope exceed
        // MAX_LAUNCH_BYTES, Provider::create rejects BEFORE command.spawn() with
        // InvalidRequest (never CleanupUnconfirmed, never marking provider failed).
        let huge_workspace = format!("/{}", "w".repeat(5_000));
        let oversized_provider = Provider::new(
            Context {
                client: DockerEvaluator::new(std::path::Path::new("/usr/bin/false")).unwrap(),
                container_id: "a".repeat(64),
                image: FIXTURE_IMAGE.into(),
                workspace: huge_workspace.into(),
                scratch: "/scratch".into(),
                alive: Arc::new(AtomicBool::new(true)),
            },
            "run".into(),
            "engine".into(),
            "peer".into(),
            Budgets::default(),
        );
        let value = oversized_provider.action(&max_create).unwrap();
        let options = vec![json!({"optionId":"yes","kind":"allow_once"})];
        let now = chrono::Utc::now();
        let proposal = Proposal {
            id: "proposal-oversized".into(),
            engine_session_id: "engine".into(),
            peer_session_id: "peer".into(),
            peer_request_id: json!(1),
            tool_call_id: "call".into(),
            action_digest: digest(&value).unwrap(),
            options_digest: digest(&options).unwrap(),
            action: value,
            options,
            observed_at: now,
            deadline: now + chrono::Duration::seconds(10),
            prohibition: None,
        };
        oversized_provider.reserve(&proposal.id).unwrap();
        let authority = oversized_provider
            .authorize(&max_create, &proposal)
            .unwrap();
        let err = oversized_provider
            .create(oversized_provider.scope.clone(), max_create, authority)
            .await;
        assert_eq!(err, Err(Error::InvalidRequest));
        assert!(!oversized_provider.failed.load(Ordering::Acquire));
        let registry = oversized_provider.registry.lock().unwrap();
        assert_eq!(registry.creations, 0);
        assert!(registry.entries.is_empty());
        assert!(registry.authorities.is_empty());
    }

    #[test]
    fn terminal_failed_or_expired_authorize_burns_reservation_slot() {
        let (provider, action, proposal) = fixture();

        // Expired proposal burns its reservation instead of wedging capacity.
        let mut expired = proposal.clone();
        expired.id = "exp".into();
        expired.observed_at -= chrono::Duration::seconds(20);
        expired.deadline -= chrono::Duration::seconds(20);
        provider.reserve(&expired.id).unwrap();
        assert_eq!(
            provider.authorize(&action, &expired),
            Err(Error::NotAuthorized)
        );
        assert!(provider.registry.lock().unwrap().reservations.is_empty());

        // Prohibited proposal also burns its reservation.
        let mut prohibited = proposal.clone();
        prohibited.id = "prohibited".into();
        prohibited.prohibition = Some("deny".into());
        provider.reserve(&prohibited.id).unwrap();
        assert_eq!(
            provider.authorize(&action, &prohibited),
            Err(Error::NotAuthorized)
        );
        assert!(provider.registry.lock().unwrap().reservations.is_empty());

        // Mismatched action burns its reservation.
        let mut mismatch = proposal.clone();
        mismatch.id = "mismatch".into();
        mismatch.action["rawInput"]["args"] = json!(["different"]);
        mismatch.action_digest = digest(&mismatch.action).unwrap();
        provider.reserve(&mismatch.id).unwrap();
        assert_eq!(
            provider.authorize(&action, &mismatch),
            Err(Error::NotAuthorized)
        );
        assert!(provider.registry.lock().unwrap().reservations.is_empty());

        // All MAX_TERMINALS capacity slots remain available afterwards.
        for i in 0..MAX_TERMINALS {
            provider.reserve(&format!("slot-{i}")).unwrap();
        }
    }

    fn insert_mock_entry(provider: &Provider, id: &str, state: State) -> Arc<Entry> {
        let (changed, _) = watch::channel(());
        let entry = Arc::new(Entry {
            deadline: tokio::time::Instant::now() + Duration::from_secs(30),
            state: Mutex::new(state),
            stdin: tokio::sync::Mutex::new(None),
            changed,
            abort: Mutex::new(None),
        });
        provider
            .registry
            .lock()
            .unwrap()
            .entries
            .insert(id.into(), entry.clone());
        entry
    }

    #[tokio::test]
    async fn terminal_post_retention_race_and_session_lifetime_attribution_are_consistent() {
        let (provider, _, _) = fixture();
        let exit_ok = ExitStatus {
            exit_code: Some(0),
            signal: None,
        };

        for (id, budget) in [
            ("ret-race", Budget::Retention),
            ("sess-race", Budget::SessionLifetime),
        ] {
            // [T1, T2] window: driver emitted `released` with retentionExpired=true,
            // but the entry has not yet moved from `entries` to `retired`.
            insert_mock_entry(
                &provider,
                id,
                State {
                    tail: OutputTail::new(64).unwrap(),
                    started: true,
                    exit: Some(exit_ok.clone()),
                    released: true,
                    releasing: true,
                    driver_ok: false,
                    failed: false,
                    budget: None,
                    retention_expired: true,
                    expired_budget: Some(budget),
                    output_reads: VecDeque::new(),
                },
            );
            assert_eq!(
                provider.output(provider.scope.clone(), id.into()).await,
                Err(Error::BudgetExceeded(budget))
            );
            assert_eq!(
                provider
                    .wait_for_exit(provider.scope.clone(), id.into())
                    .await,
                Err(Error::BudgetExceeded(budget))
            );
            assert_eq!(
                provider.kill(provider.scope.clone(), id.into()).await,
                Err(Error::BudgetExceeded(budget))
            );
            assert_eq!(
                provider.release(provider.scope.clone(), id.into()).await,
                Err(Error::BudgetExceeded(budget))
            );
            assert!(!provider.failed.load(Ordering::Acquire));

            // Retired tombstone preserves exact budget (Retention vs SessionLifetime).
            {
                let mut reg = provider.registry.lock().unwrap();
                reg.entries.remove(id);
                reg.retired
                    .insert(id.into(), (exit_ok.clone(), Some(budget)));
            }
            assert_eq!(
                provider.output(provider.scope.clone(), id.into()).await,
                Err(Error::BudgetExceeded(budget))
            );
            assert_eq!(
                provider.release(provider.scope.clone(), id.into()).await,
                Err(Error::BudgetExceeded(budget))
            );
        }

        // Explicitly released (non-expired) handle in [T1, T2] rejects kill() with
        // InvalidHandle rather than succeeding or failing the session.
        insert_mock_entry(
            &provider,
            "explicit-released",
            State {
                tail: OutputTail::new(64).unwrap(),
                started: true,
                exit: Some(exit_ok),
                released: true,
                releasing: false,
                driver_ok: true,
                failed: false,
                budget: None,
                retention_expired: false,
                expired_budget: None,
                output_reads: VecDeque::new(),
            },
        );
        assert_eq!(
            provider
                .kill(provider.scope.clone(), "explicit-released".into())
                .await,
            Err(Error::InvalidHandle)
        );
        assert!(!provider.failed.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn terminal_close_releases_all_handles_concurrently() {
        let (provider, _, _) = fixture();
        let provider = Arc::new(provider);
        for index in 0..MAX_TERMINALS {
            let id = format!("terminal-{index}");
            let entry = insert_mock_entry(
                &provider,
                &id,
                State {
                    tail: OutputTail::new(64).unwrap(),
                    started: true,
                    exit: None,
                    released: false,
                    releasing: false,
                    driver_ok: false,
                    failed: false,
                    budget: None,
                    retention_expired: false,
                    expired_budget: None,
                    output_reads: VecDeque::new(),
                },
            );
            tokio::spawn(async move {
                while !entry.state.lock().unwrap().releasing {
                    tokio::task::yield_now().await;
                }
                // Sequential close across 8 handles would take >= 2.0s;
                // concurrent close finishes in ~250ms.
                tokio::time::sleep(Duration::from_millis(250)).await;
                {
                    let mut state = entry.state.lock().unwrap();
                    state.exit = Some(ExitStatus {
                        exit_code: Some(0),
                        signal: None,
                    });
                    state.released = true;
                    state.driver_ok = true;
                }
                entry.changed.send_replace(());
            });
        }
        let started = std::time::Instant::now();
        let receipts = tokio::time::timeout(Duration::from_millis(1200), provider.close())
            .await
            .expect("concurrent close must finish well under sequential sum")
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(1200));
        assert_eq!(receipts.len(), MAX_TERMINALS);
        for (index, receipt) in receipts.iter().enumerate() {
            assert_eq!(receipt.terminal_id, format!("terminal-{index}"));
            assert!(receipt.released && receipt.descendants_reaped && receipt.output_drained);
        }
    }

    #[tokio::test]
    async fn terminal_monotonic_authority_sub_ms_reserve_and_sliding_output_rate_window() {
        let (mut provider, action, proposal) = fixture();

        // 1. Sub-millisecond remaining session lifetime is rejected at reserve()
        // before consent/authority is ever issued.
        provider.session_deadline = tokio::time::Instant::now() + Duration::from_micros(500);
        assert_eq!(
            provider.reserve("sub-ms"),
            Err(Error::BudgetExceeded(Budget::SessionLifetime))
        );
        provider.session_deadline = tokio::time::Instant::now() + Duration::from_secs(600);

        // 2. Monotonic authority expiry rejects create() when monotonic deadline
        // has elapsed even if wall-clock chrono::Utc::now() has not passed deadline.
        let mut long_wall_proposal = proposal.clone();
        long_wall_proposal.deadline = chrono::Utc::now() + chrono::Duration::seconds(60);
        provider.reserve(&long_wall_proposal.id).unwrap();
        let mut authority = provider.authorize(&action, &long_wall_proposal).unwrap();
        authority.expires_at = tokio::time::Instant::now() - Duration::from_millis(1);
        assert_eq!(
            provider
                .create(provider.scope.clone(), action, authority)
                .await,
            Err(Error::NotAuthorized)
        );

        // 3. Sliding 1s output rate window prevents 20-read boundary bursts:
        // 5 reads aged >1s expire, while 5 reads from 200ms ago remain counted.
        let now = tokio::time::Instant::now();
        let mut output_reads = VecDeque::with_capacity(MAX_OUTPUT_READS_PER_SECOND);
        for _ in 0..5 {
            output_reads.push_back(now - Duration::from_millis(1_100));
        }
        for _ in 0..5 {
            output_reads.push_back(now - Duration::from_millis(200));
        }
        insert_mock_entry(
            &provider,
            "sliding",
            State {
                tail: OutputTail::new(64).unwrap(),
                started: true,
                exit: None,
                released: false,
                releasing: false,
                driver_ok: false,
                failed: false,
                budget: None,
                retention_expired: false,
                expired_budget: None,
                output_reads,
            },
        );
        for _ in 0..5 {
            provider
                .output(provider.scope.clone(), "sliding".into())
                .await
                .unwrap();
        }
        assert_eq!(
            provider
                .output(provider.scope.clone(), "sliding".into())
                .await,
            Err(Error::BudgetExceeded(Budget::OutputRate))
        );
    }
}
