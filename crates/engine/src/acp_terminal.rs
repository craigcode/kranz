//! Engine-owned terminal fixture provider. It can only address the exact
//! namespace admitted by OwnedContainer; no host command fallback exists.
use crate::gate_evaluation::subprocess::DockerEvaluator;
use crate::live_permission::{digest, Proposal};
use kranz_acp::terminal::{
    CleanupReceipt, Create, Error, ExitStatus, OutputSnapshot, OutputTail, Scope, TerminalProvider,
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

pub(crate) struct Context {
    pub(crate) client: DockerEvaluator,
    pub(crate) container_id: String,
    pub(crate) image: String,
    pub(crate) workspace: PathBuf,
    pub(crate) scratch: PathBuf,
    pub(crate) alive: Arc<AtomicBool>,
}

pub(crate) struct Authority {
    nonce: String,
    scope: Scope,
    digest: String,
    deadline: chrono::DateTime<chrono::Utc>,
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
}

struct Entry {
    state: Mutex<State>,
    stdin: tokio::sync::Mutex<Option<tokio::process::ChildStdin>>,
    changed: watch::Sender<()>,
    abort: Mutex<Option<tokio::task::AbortHandle>>,
}

#[derive(Default)]
struct Registry {
    entries: HashMap<String, Arc<Entry>>,
    authorities: HashSet<String>,
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
        if queue.len() >= 2 * MAX_CREATIONS {
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
    registry: Mutex<Registry>,
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
    ) -> Self {
        Self {
            context,
            scope: Scope {
                run_id: run,
                engine_session_id: engine_session,
                peer_session_id: peer_session,
                generation: uuid::Uuid::new_v4().to_string(),
            },
            registry: Mutex::new(Registry::default()),
            failed: Arc::new(AtomicBool::new(false)),
            failure: Arc::new(Notify::new()),
            receipts: Receipts::default(),
        }
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
            "namespaceId":self.context.container_id}),
        )
    }

    /// Called only after the backend validates the engine's durable response.
    /// The authority is never deserialized from ACP or reconstructed on resume.
    pub(crate) fn authorize(
        &self,
        action: &Create,
        proposal: &Proposal,
    ) -> Result<Authority, Error> {
        self.check_scope(&self.scope)?;
        if proposal.validate().is_err()
            || proposal.prohibition.is_some()
            || proposal.option(true).is_none()
            || proposal.engine_session_id != self.scope.engine_session_id
            || proposal.peer_session_id != self.scope.peer_session_id
            || chrono::Utc::now() >= proposal.deadline
            || proposal.action != self.action(action)?
        {
            return Err(Error::NotAuthorized);
        }
        let mut registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
        if registry.authorities.len() >= crate::live_permission::MAX_PENDING {
            return Err(Error::Unavailable);
        }
        let nonce = uuid::Uuid::new_v4().to_string();
        registry.authorities.insert(nonce.clone());
        Ok(Authority {
            nonce,
            deadline: proposal.deadline,
            action_digest: proposal.action_digest.clone(),
            scope: self.scope.clone(),
            digest: digest(&self.normalize(action.clone())?).map_err(|_| Error::InvalidRequest)?,
        })
    }

    fn entry(&self, scope: &Scope, id: &str) -> Result<Arc<Entry>, Error> {
        self.check_scope(scope)?;
        let entry = self
            .registry
            .lock()
            .map_err(|_| Error::Unavailable)?
            .entries
            .get(id)
            .cloned()
            .ok_or(Error::InvalidHandle)?;
        if entry
            .state
            .lock()
            .map_err(|_| Error::Unavailable)?
            .releasing
        {
            return Err(Error::InvalidHandle);
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

    pub(crate) async fn close(&self) -> Result<Vec<CleanupReceipt>, Error> {
        self.check_scope(&self.scope)?;
        let ids: Vec<_> = self
            .registry
            .lock()
            .map_err(|_| Error::Unavailable)?
            .entries
            .keys()
            .cloned()
            .collect();
        let mut receipts = Vec::new();
        // At most eight handles. Each release is bounded; the outer session
        // applies a shared close deadline and tears down the namespace on error.
        for id in ids {
            receipts.push(self.release(self.scope.clone(), id).await?);
        }
        Ok(receipts)
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
        // Consume even when subsequent validation refuses. No retry can reuse
        // the one-use capability after a changed or stale attempt.
        let known = self
            .registry
            .lock()
            .map_err(|_| Error::Unavailable)?
            .authorities
            .remove(&authority.nonce);
        self.check_scope(&scope)?;
        if !known {
            return Err(Error::NotAuthorized);
        }
        let action = self.normalize(action)?;
        let id = format!("terminal-{}", uuid::Uuid::new_v4());
        let limit = action.validate()?;
        let (changed, _) = watch::channel(());
        let entry = Arc::new(Entry {
            state: Mutex::new(State {
                tail: OutputTail::new(limit)?,
                started: false,
                exit: None,
                released: false,
                releasing: false,
                driver_ok: false,
                failed: false,
            }),
            stdin: tokio::sync::Mutex::new(None),
            changed,
            abort: Mutex::new(None),
        });
        {
            let mut registry = self.registry.lock().map_err(|_| Error::Unavailable)?;
            if chrono::Utc::now() >= authority.deadline
                || authority.scope != scope
                || authority.digest != digest(&action).map_err(|_| Error::InvalidRequest)?
            {
                return Err(Error::NotAuthorized);
            }
            if registry.entries.len() >= MAX_TERMINALS || registry.creations >= MAX_CREATIONS {
                return Err(Error::Unavailable);
            }
            registry.creations += 1;
            registry.entries.insert(id.clone(), entry.clone());
        }
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
            let launch = serde_json::to_vec(&json!({"workspace":self.context.workspace,
                "scratch":self.context.scratch, "action":action}))
            .map_err(|_| Error::InvalidRequest)?;
            if launch.len() > kranz_acp::terminal::MAX_REQUEST_BYTES {
                return Err(Error::InvalidRequest);
            }
            stdin
                .write_all(&launch)
                .await
                .map_err(|_| Error::Unavailable)?;
            stdin
                .write_all(b"\n")
                .await
                .map_err(|_| Error::Unavailable)?;
            stdin.flush().await.map_err(|_| Error::Unavailable)?;
            *entry.stdin.lock().await = Some(stdin);
            let capture = entry.clone();
            let failed = self.failed.clone();
            let failure = self.failure.clone();
            let receipts = self.receipts.clone();
            let binding =
                json!({"terminalId":id,"scope":scope,"actionDigest":authority.action_digest});
            let task = tokio::spawn(async move {
                let outcome = tokio::time::timeout(
                    Duration::from_secs(40),
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
        let state = entry.state.lock().map_err(|_| Error::Unavailable)?;
        if state.failed {
            return Err(Error::CleanupUnconfirmed);
        }
        Ok(state.tail.snapshot(state.exit.clone()))
    }

    async fn wait_for_exit(&self, scope: Scope, id: String) -> Result<ExitStatus, Error> {
        let entry = self.entry(&scope, &id)?;
        tokio::time::timeout(
            Duration::from_secs(35),
            wait_state(&entry, |s| s.exit.clone()),
        )
        .await
        .map_err(|_| {
            self.fail();
            Error::CleanupUnconfirmed
        })?
    }

    async fn kill(&self, scope: Scope, id: String) -> Result<CleanupReceipt, Error> {
        let entry = self.entry(&scope, &id)?;
        let result = tokio::time::timeout(OPERATION_TIMEOUT, async {
            control(&entry, "kill").await?;
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
            let entry = registry
                .entries
                .get(&id)
                .cloned()
                .ok_or(Error::InvalidHandle)?;
            {
                let mut state = entry.state.lock().map_err(|_| Error::Unavailable)?;
                if state.releasing {
                    return Err(Error::InvalidHandle);
                }
                state.releasing = true;
            }
            entry
        };
        let result = tokio::time::timeout(OPERATION_TIMEOUT, async {
            control(&entry, "release").await?;
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
        if result.is_ok() {
            self.registry
                .lock()
                .map_err(|_| Error::Unavailable)?
                .entries
                .remove(&id);
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
                    receipts.record(json!({"terminalStarted":binding}))?;
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
                    receipts.record(json!({"terminalExited": {
                        "binding":binding,"exitStatus":status,"descendantsReaped":true,"outputDrained":true,
                    }}))?;
                    state.exit = Some(status);
                }
                Some("released") if state.exit.is_some() && !state.released => {
                    state.released = true
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
            let authority = provider.authorize(&action, &proposal).unwrap();
            let replay = Authority {
                nonce: authority.nonce.clone(),
                scope: authority.scope.clone(),
                digest: authority.digest.clone(),
                deadline: authority.deadline,
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
        let mut authority = provider.authorize(&action, &proposal).unwrap();
        authority.deadline = chrono::Utc::now() - chrono::Duration::seconds(1);
        assert_eq!(
            provider
                .create(provider.scope.clone(), action, authority)
                .await,
            Err(Error::NotAuthorized)
        );
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
}
