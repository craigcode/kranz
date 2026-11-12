//! Consent and asynchronous terminal routing. Production admission remains closed
//! unless an explicit contained fixture or Sgian host execution-grant config is supplied.
use super::*;
use kranz_acp::terminal::{
    self, Budget, CleanupReceipt, Create, Error, ExitStatus, OutputSnapshot, Scope, Target,
    TerminalProvider,
};
use std::collections::HashSet;

const MAX_PENDING_CONTROL: usize = 32;
pub(super) const MAX_REFUSED_TERMINAL_TRANSCRIPT: usize = 256;

enum PreparedBackend {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Contained(
        crate::acp_terminal::Context,
        String,
        crate::acp_terminal::Budgets,
    ),
    Sgian(crate::sgian_terminal::PreparedContext),
}

#[derive(Clone)]
pub(super) enum TerminalBackend {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Contained(Arc<crate::acp_terminal::Provider>),
    Sgian(Arc<crate::sgian_terminal::Provider>),
}

pub(super) enum BackendAuthority {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    Contained(crate::acp_terminal::Authority),
    Sgian(crate::sgian_terminal::Authority),
}

impl TerminalBackend {
    pub(super) fn scope(&self) -> &Scope {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => &p.scope,
            Self::Sgian(p) => &p.scope,
        }
    }

    pub(super) fn is_sgian(&self) -> bool {
        matches!(self, Self::Sgian(_))
    }

    pub(super) fn normalize(&self, action: Create) -> std::result::Result<Create, Error> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.normalize(action),
            Self::Sgian(p) => p.normalize(action),
        }
    }

    pub(super) fn action(&self, request: &Create) -> std::result::Result<Value, Error> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.action(request),
            Self::Sgian(p) => p.action(request),
        }
    }

    pub(super) fn reserve(&self, proposal: &str) -> std::result::Result<(), Error> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.reserve(proposal),
            Self::Sgian(p) => p.reserve(proposal),
        }
    }

    pub(super) fn cancel_reservation(&self, proposal: &str) -> std::result::Result<(), Error> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.cancel_reservation(proposal),
            Self::Sgian(p) => p.cancel_reservation(proposal),
        }
    }

    pub(super) fn exit_status(&self, id: &str) -> Option<ExitStatus> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.exit_status(id),
            Self::Sgian(p) => p.exit_status(id),
        }
    }

    pub(super) fn authorize(
        &self,
        action: &Create,
        proposal: &crate::live_permission::Proposal,
    ) -> std::result::Result<BackendAuthority, Error> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p
                .authorize(action, proposal)
                .map(BackendAuthority::Contained),
            Self::Sgian(p) => p.authorize(action, proposal).map(BackendAuthority::Sgian),
        }
    }

    pub(super) async fn create(
        &self,
        scope: Scope,
        action: Create,
        authority: BackendAuthority,
    ) -> std::result::Result<String, Error> {
        match (self, authority) {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            (Self::Contained(p), BackendAuthority::Contained(auth)) => {
                p.create(scope, action, auth).await
            }
            (Self::Sgian(p), BackendAuthority::Sgian(auth)) => p.create(scope, action, auth).await,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            _ => Err(Error::NotAuthorized),
        }
    }

    pub(super) async fn output(
        &self,
        scope: Scope,
        id: String,
    ) -> std::result::Result<OutputSnapshot, Error> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.output(scope, id).await,
            Self::Sgian(p) => p.output(scope, id).await,
        }
    }

    pub(super) async fn wait_for_exit(
        &self,
        scope: Scope,
        id: String,
    ) -> std::result::Result<ExitStatus, Error> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.wait_for_exit(scope, id).await,
            Self::Sgian(p) => p.wait_for_exit(scope, id).await,
        }
    }

    pub(super) async fn kill(
        &self,
        scope: Scope,
        id: String,
    ) -> std::result::Result<CleanupReceipt, Error> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.kill(scope, id).await,
            Self::Sgian(p) => p.kill(scope, id).await,
        }
    }

    pub(super) async fn release(
        &self,
        scope: Scope,
        id: String,
    ) -> std::result::Result<CleanupReceipt, Error> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.release(scope, id).await,
            Self::Sgian(p) => p.release(scope, id).await,
        }
    }

    pub(super) async fn failure(&self) {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.failure().await,
            Self::Sgian(p) => p.failure().await,
        }
    }

    pub(super) fn stop(&self) {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.stop(),
            Self::Sgian(p) => p.stop(),
        }
    }

    pub(super) async fn next_receipt(&self) -> std::result::Result<Value, Error> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.next_receipt().await,
            Self::Sgian(p) => p.next_receipt().await,
        }
    }

    pub(super) fn drain_receipts(&self) -> std::result::Result<Vec<Value>, Error> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.drain_receipts(),
            Self::Sgian(p) => p.drain_receipts(),
        }
    }

    pub(super) async fn close(&self) -> std::result::Result<Vec<CleanupReceipt>, Error> {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(p) => p.close().await,
            Self::Sgian(p) => p.close().await,
        }
    }

    pub(super) async fn abort_cleanup(&self) -> bool {
        match self {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            Self::Contained(_) => true,
            Self::Sgian(p) => p.abort_cleanup().await,
        }
    }
}

#[derive(Default)]
pub(super) struct Broker {
    prepared: Option<PreparedBackend>,
    pub(super) provider: Option<TerminalBackend>,
    tasks: tokio::task::JoinSet<TerminalCompletion>,
    pub(super) cleanup_recorded: bool,
    pub(super) saw_budget_hit: bool,
    pub(super) refused_terminal_requests: usize,
    pub(super) suppressed_terminal_refusals: usize,
    seen_refusal_errors: HashSet<Error>,
    controls: HashSet<String>,
    creating: HashSet<String>,
}

impl Broker {
    pub(super) fn prepare(
        #[cfg(any(target_os = "macos", target_os = "linux"))] run: Option<(
            String,
            crate::acp_terminal::Budgets,
        )>,
        #[cfg(any(target_os = "macos", target_os = "linux"))] container: Option<
            &crate::acp_container::OwnedContainer,
        >,
        sgian: Option<&crate::sgian_terminal::Config>,
        spec: &SessionSpec,
    ) -> Result<Self> {
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        if run.is_some() && sgian.is_some() {
            return Err(EngineError::Backend(
                "acp terminals cannot combine contained fixture and sgian provider".into(),
            ));
        }

        let prepared = if let Some(sgian_cfg) = sgian {
            if spec.sandbox.is_some() {
                return Err(EngineError::Backend(
                    "sgian terminal provider is only permitted for uncontained host sessions"
                        .into(),
                ));
            }
            match crate::sgian_terminal::PreparedContext::prepare_for_session(
                sgian_cfg,
                &spec.cwd,
                &spec.session_id,
            ) {
                Ok(crate::sgian_terminal::PrepareOutcome::Enabled(ctx)) => {
                    Some(PreparedBackend::Sgian(ctx))
                }
                Ok(crate::sgian_terminal::PrepareOutcome::SkippedOutsideRepo) => None,
                Err(msg) => {
                    return Err(EngineError::Backend(format!(
                        "sgian terminal prepare failed: {msg}"
                    )));
                }
            }
        } else {
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                run.map(|(run, budgets)| {
                    if run.trim().is_empty() || run.len() > 256 {
                        return Err(failure(false));
                    }
                    let context = container
                        .ok_or_else(|| failure(false))?
                        .terminal_context(spec)?;
                    Ok(PreparedBackend::Contained(context, run, budgets))
                })
                .transpose()?
            }
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            {
                None
            }
        };
        Ok(Self {
            prepared,
            provider: None,
            tasks: tokio::task::JoinSet::new(),
            cleanup_recorded: false,
            saw_budget_hit: false,
            refused_terminal_requests: 0,
            suppressed_terminal_refusals: 0,
            seen_refusal_errors: HashSet::new(),
            controls: HashSet::new(),
            creating: HashSet::new(),
        })
    }

    pub(super) fn admitted(&self) -> bool {
        self.prepared.is_some()
    }

    pub(super) fn has_sgian(&self) -> bool {
        self.provider
            .as_ref()
            .is_some_and(TerminalBackend::is_sgian)
    }

    pub(super) fn bind(&mut self, engine: &str, peer: &str) {
        if let Some(prepared) = self.prepared.take() {
            self.provider = Some(match prepared {
                #[cfg(any(target_os = "macos", target_os = "linux"))]
                PreparedBackend::Contained(context, run, budgets) => {
                    TerminalBackend::Contained(Arc::new(crate::acp_terminal::Provider::new(
                        context,
                        run,
                        engine.into(),
                        peer.into(),
                        budgets,
                    )))
                }
                PreparedBackend::Sgian(context) => TerminalBackend::Sgian(Arc::new(
                    crate::sgian_terminal::Provider::from_prepared(
                        context,
                        engine.into(),
                        peer.into(),
                    ),
                )),
            });
        }
    }

    pub(super) async fn next(&mut self) -> Result<Input> {
        let Some(provider) = &self.provider else {
            return std::future::pending().await;
        };
        let is_sgian = provider.is_sgian();
        tokio::select! {
            biased;
            _ = provider.failure() => Err(failure(is_sgian)),
            receipt = provider.next_receipt() => receipt.map(Input::TerminalReceipt).map_err(|_| failure(is_sgian)),
            result = self.tasks.join_next(), if !self.tasks.is_empty() =>
                result.ok_or_else(|| failure(is_sgian))?.map(Input::Terminal).map_err(|_| failure(is_sgian)),
        }
    }

    pub(super) fn pending(&self) -> usize {
        self.creating.len()
    }

    pub(super) fn in_flight(&self, id: &str) -> bool {
        self.controls.contains(id) || self.creating.contains(id)
    }

    fn admit_control(&mut self, id: String) -> std::result::Result<(), Error> {
        if self.controls.len() >= MAX_PENDING_CONTROL {
            return Err(Error::BudgetExceeded(Budget::PendingControl));
        }
        if !self.controls.insert(id) {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }

    pub(super) fn completed(&mut self, id: &str) {
        self.controls.remove(id);
        self.creating.remove(id);
    }

    pub(super) fn observe_receipt(&mut self, raw: &Value) {
        if raw
            .get("terminalExited")
            .and_then(|e| e.get("budget"))
            .is_some_and(|b| !b.is_null())
        {
            self.saw_budget_hit = true;
        }
    }

    pub(super) fn should_emit_requested(&self) -> bool {
        self.refused_terminal_requests < MAX_REFUSED_TERMINAL_TRANSCRIPT
    }

    pub(super) fn record_completion(&mut self, result: &std::result::Result<Value, Error>) -> bool {
        match result {
            Ok(_) => true,
            Err(error) => {
                if matches!(error, Error::BudgetExceeded(_)) {
                    self.saw_budget_hit = true;
                }
                self.refused_terminal_requests = self.refused_terminal_requests.saturating_add(1);
                let first_of_kind = self.seen_refusal_errors.insert(*error);
                let emit = self.refused_terminal_requests <= MAX_REFUSED_TERMINAL_TRANSCRIPT
                    || first_of_kind;
                if !emit {
                    self.suppressed_terminal_refusals =
                        self.suppressed_terminal_refusals.saturating_add(1);
                }
                emit
            }
        }
    }

    pub(super) fn stop(&mut self) {
        self.tasks.abort_all();
        if let Some(provider) = &self.provider {
            provider.stop();
        }
    }
}

impl Drop for Broker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn failure(is_sgian: bool) -> EngineError {
    if is_sgian {
        EngineError::Backend("sgian terminal operation or cleanup failed; ending session".into())
    } else {
        EngineError::Backend(
            "contained terminal operation or cleanup failed; ending owned namespace".into(),
        )
    }
}

impl AcpSession {
    pub(super) async fn handle_terminal(
        &mut self,
        id: Value,
        method: &str,
        params: Value,
    ) -> Result<()> {
        let provider = self
            .terminals
            .provider
            .clone()
            .ok_or_else(|| failure(false))?;
        let is_sgian = provider.is_sgian();
        // Request IDs are retained for replay detection, so bound them before
        // insertion as well as bounding the typed payload and operation count.
        if id
            .as_str()
            .is_some_and(|id| id.is_empty() || id.len() > 256)
            || method.len() > 128
            || serde_json::to_vec(&params)?.len() > terminal::MAX_REQUEST_BYTES
        {
            return Err(EngineError::Backend(
                "ACP terminal request exceeds its byte budget".into(),
            ));
        }
        let peer_id = serde_json::to_string(&id)?;
        if self.terminals.in_flight(&peer_id)
            || self
                .pending_permissions
                .values()
                .any(|p| p.proposal.peer_request_id == id)
        {
            return Err(EngineError::Backend(
                "duplicate in-flight ACP request".into(),
            ));
        }
        let make_requested = |params: &Value| -> Result<AgentEvent> {
            Ok(AgentEvent::Other {
                raw: json!({"terminalRequested": {
                    "method":method,"requestId":id,"scope":provider.scope(),
                    "requestDigest":crate::live_permission::digest(params)?,
                }}),
            })
        };
        let parsed = if method == terminal::CREATE {
            serde_json::from_value::<Create>(params.clone())
                .map_err(|_| Error::InvalidRequest)
                .and_then(|action| {
                    provider.normalize(action.clone())?;
                    Ok(Ok(action))
                })
        } else if matches!(
            method,
            terminal::OUTPUT | terminal::WAIT_FOR_EXIT | terminal::KILL | terminal::RELEASE
        ) {
            serde_json::from_value::<Target>(params.clone())
                .map_err(|_| Error::InvalidRequest)
                .and_then(|target| {
                    target.validate()?;
                    if target.session_id != provider.scope().peer_session_id {
                        return Err(Error::InvalidHandle);
                    }
                    Ok(target)
                })
                .map(Err)
        } else {
            Err(Error::InvalidRequest)
        };
        let request = match parsed {
            Ok(request) => request,
            Err(error) => {
                if self.terminals.should_emit_requested() {
                    self.queue.push_back(make_requested(&params)?);
                }
                return self
                    .complete_terminal(TerminalCompletion {
                        id,
                        method: method.into(),
                        result: Err(error),
                        receipt: json!({"refusedBeforeExecution":true}),
                        permission_id: None,
                    })
                    .await;
            }
        };
        match request {
            Ok(action) => {
                if self.seen_permission_ids.len() >= 1024
                    || self.seen_permission_ids.contains(&peer_id)
                {
                    return Err(EngineError::Backend(
                        "duplicate or excessive ACP consent requests".into(),
                    ));
                }
                let proposal_id = format!("permission-{}", uuid::Uuid::new_v4());
                let capacity = if self.pending_permissions.len() + self.terminals.pending()
                    >= crate::live_permission::MAX_PENDING
                {
                    Err(Error::BudgetExceeded(Budget::PendingConsent))
                } else {
                    provider.reserve(&proposal_id)
                };
                if let Err(error) = capacity {
                    if self.terminals.should_emit_requested() {
                        self.queue.push_back(make_requested(&params)?);
                    }
                    return self
                        .complete_terminal(TerminalCompletion {
                            id,
                            method: method.into(),
                            result: Err(error),
                            receipt: json!({"refusedBeforeConsent":true}),
                            permission_id: None,
                        })
                        .await;
                }
                self.queue.push_back(make_requested(&params)?);
                self.seen_permission_ids.insert(peer_id);
                let value = provider.action(&action).map_err(|_| failure(is_sgian))?;
                let options = vec![
                    json!({"optionId":"execute-once","kind":"allow_once","name":"Execute this exact command"}),
                    json!({"optionId":"deny-once","kind":"reject_once","name":"Deny"}),
                ];
                let now = chrono::Utc::now();
                let prohibition = if is_sgian {
                    sgian_terminal_prohibition(&self.spec, &action, &value)
                } else {
                    // Arbitrary deny patterns cannot be soundly matched against a
                    // shell program. This fixture refuses any restricted tool set.
                    (!self.spec.writable || !self.spec.disallowed_tools.is_empty()).then(|| {
                        "terminal execution is prohibited by this session's tool policy".into()
                    })
                };
                let mut proposal = crate::live_permission::Proposal {
                    id: proposal_id,
                    engine_session_id: self.session_id.clone(),
                    peer_session_id: provider.scope().peer_session_id.clone(),
                    peer_request_id: id,
                    tool_call_id: format!("terminal-create-{}", uuid::Uuid::new_v4()),
                    action_digest: crate::live_permission::digest(&value)?,
                    options_digest: crate::live_permission::digest(&options)?,
                    action: value,
                    options,
                    observed_at: now,
                    deadline: now
                        + chrono::Duration::seconds(crate::live_permission::REQUEST_TTL_SECS),
                    prohibition,
                };
                if is_sgian && proposal.prohibition.is_none() && proposal.ambiguous_display() {
                    proposal.prohibition =
                        Some("permission contains invisible or terminal control characters".into());
                }
                proposal.validate()?;
                self.pending_permissions.insert(
                    proposal.id.clone(),
                    PendingPermission {
                        proposal: proposal.clone(),
                        expires_at: tokio::time::Instant::now()
                            + std::time::Duration::from_secs(
                                crate::live_permission::REQUEST_TTL_SECS as u64,
                            ),
                        terminal: Some(action),
                    },
                );
                self.queue.push_back(AgentEvent::PermissionRequested {
                    raw:json!({"terminalCreateConsent":proposal.id,"actionDigest":proposal.action_digest}),
                    proposal:Box::new(proposal),
                });
            }
            Err(target) => {
                if self.terminals.should_emit_requested() {
                    self.queue.push_back(make_requested(&params)?);
                }
                if let Err(error) = self.terminals.admit_control(peer_id) {
                    return self
                        .complete_terminal(TerminalCompletion {
                            id,
                            method: method.into(),
                            result: Err(error),
                            receipt: json!({"refusedBeforeExecution":true}),
                            permission_id: None,
                        })
                        .await;
                }
                let method = method.to_owned();
                self.terminals.tasks.spawn(async move {
                    let scope = provider.scope().clone();
                    let mut receipt = json!({"scope":scope,"terminalId":target.terminal_id});
                    let terminal_id = target.terminal_id.clone();
                    let result =
                        match method.as_str() {
                            terminal::OUTPUT => provider
                                .output(scope, target.terminal_id)
                                .await
                                .map(|output| {
                                    receipt["retainedBytes"] = json!(output.output.len());
                                    receipt["truncated"] = json!(output.truncated);
                                    receipt["exitStatus"] = json!(output.exit_status);
                                    json!(output)
                                }),
                            terminal::WAIT_FOR_EXIT => provider
                                .wait_for_exit(scope, target.terminal_id)
                                .await
                                .map(|exit| {
                                    receipt["exitStatus"] = json!(exit);
                                    json!(exit)
                                }),
                            terminal::KILL | terminal::RELEASE => {
                                let stopped = if method == terminal::KILL {
                                    provider.kill(scope, target.terminal_id).await
                                } else {
                                    provider.release(scope, target.terminal_id).await
                                };
                                stopped.map(|cleanup| {
                                    receipt["cleanup"] = json!(cleanup);
                                    json!({})
                                })
                            }
                            _ => Err(Error::InvalidRequest),
                        };
                    if matches!(
                        result,
                        Err(Error::BudgetExceeded(
                            Budget::CommandLifetime | Budget::SessionLifetime
                        ))
                    ) {
                        receipt["exitStatus"] = json!(provider.exit_status(&terminal_id));
                        receipt["descendantsReaped"] = json!(true);
                        receipt["outputDrained"] = json!(true);
                    }
                    TerminalCompletion {
                        id,
                        method,
                        result,
                        receipt,
                        permission_id: None,
                    }
                });
            }
        }
        Ok(())
    }

    pub(super) async fn answer_terminal(
        &mut self,
        proposal: crate::live_permission::Proposal,
        action: Create,
        allow: bool,
    ) -> Result<()> {
        let provider = self
            .terminals
            .provider
            .clone()
            .ok_or_else(|| failure(false))?;
        let is_sgian = provider.is_sgian();
        if !allow {
            provider
                .cancel_reservation(&proposal.id)
                .map_err(|_| failure(is_sgian))?;
            return self
                .complete_terminal(TerminalCompletion {
                    id: proposal.peer_request_id,
                    method: terminal::CREATE.into(),
                    result: Err(Error::NotAuthorized),
                    receipt: json!({"denied":true,"actionDigest":proposal.action_digest}),
                    permission_id: Some(proposal.id),
                })
                .await;
        }
        let authority = provider
            .authorize(&action, &proposal)
            .map_err(|_| failure(is_sgian))?;
        self.terminals
            .creating
            .insert(serde_json::to_string(&proposal.peer_request_id)?);
        self.terminals.tasks.spawn(async move {
            let scope = provider.scope().clone();
            let result = provider.create(scope.clone(), action, authority).await;
            let receipt = json!({"scope":scope,"actionDigest":proposal.action_digest,
                "started":result.is_ok(),"terminalId":result.as_ref().ok()});
            TerminalCompletion {
                id: proposal.peer_request_id,
                method: terminal::CREATE.into(),
                result: result.map(|id| json!({"terminalId":id})),
                receipt,
                permission_id: Some(proposal.id),
            }
        });
        Ok(())
    }

    pub(super) async fn close_terminals(&mut self) -> Result<()> {
        let Some(provider) = self.terminals.provider.clone() else {
            return Ok(());
        };
        let is_sgian = provider.is_sgian();
        // A prompt result while consent or an operation is pending cannot turn
        // a lost command outcome into successful completion.
        if !self.terminals.tasks.is_empty() || !self.pending_permissions.is_empty() {
            return Err(failure(is_sgian));
        }
        let receipts = tokio::time::timeout(std::time::Duration::from_secs(8), provider.close())
            .await
            .map_err(|_| failure(is_sgian))?
            .map_err(|_| failure(is_sgian))?;
        for raw in provider.drain_receipts().map_err(|_| failure(is_sgian))? {
            self.terminals.observe_receipt(&raw);
            self.queue.push_back(AgentEvent::Other { raw });
        }
        self.queue.push_back(AgentEvent::Other {
            raw: json!({"terminalSessionCleanup": {
                "scope":provider.scope(),"receipts":receipts,"confirmed":true,
                "refusedRequests":self.terminals.refused_terminal_requests,
                "suppressedRefusals":self.terminals.suppressed_terminal_refusals,
            }}),
        });
        Ok(())
    }
}

fn join_command_args(command: &str, args: &[String]) -> String {
    let mut full = command.to_string();
    for arg in args {
        full.push(' ');
        full.push_str(arg);
    }
    full
}

fn push_command_and_basename(subjects: &mut Vec<String>, command_line: &str) {
    let trimmed = command_line.trim();
    if trimmed.is_empty() {
        return;
    }
    subjects.push(trimmed.to_string());
    let (first, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, Some(rest.trim_start())),
        None => (trimmed, None),
    };
    if first.contains(['/', '\\']) {
        let base = first.rsplit(['/', '\\']).next().unwrap_or(first);
        if !base.is_empty() {
            subjects.push(match rest {
                Some(r) if !r.is_empty() => format!("{base} {r}"),
                _ => base.to_string(),
            });
        }
    }
}

fn sgian_terminal_prohibition(
    spec: &SessionSpec,
    action: &kranz_acp::terminal::Create,
    value: &Value,
) -> Option<String> {
    if !spec.writable {
        return Some("terminal execution is prohibited by this session's tool policy".into());
    }
    if spec.disallowed_tools.iter().any(|pattern| {
        let (name, glob) = super::split_pattern(pattern);
        glob == "*"
            && ["Write", "Edit", "MultiEdit", "NotebookEdit"]
                .iter()
                .any(|tool| tool.eq_ignore_ascii_case(name))
    }) {
        return Some("terminal execution is prohibited by this session's tool policy".into());
    }

    let mut subjects = super::tool_call_subject("execute", &action.command, value);
    if subjects.is_empty() {
        return Some("terminal execution is prohibited by this session's tool policy".into());
    }
    let base = action
        .command
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&action.command);
    if !base.trim().is_empty() && base != action.command {
        subjects.push(join_command_args(base, &action.args));
    }

    if base.eq_ignore_ascii_case("env") {
        let mut idx = 0usize;
        while idx < action.args.len() {
            let tok = action.args[idx].as_str();
            if tok == "--" {
                idx += 1;
                break;
            }
            if matches!(
                tok,
                "-u" | "--unset" | "-C" | "--chdir" | "-S" | "--split-string"
            ) {
                idx += 2;
                continue;
            }
            if tok.starts_with('-') || tok.contains('=') {
                idx += 1;
                continue;
            }
            break;
        }
        if idx < action.args.len() {
            push_command_and_basename(
                &mut subjects,
                &join_command_args(&action.args[idx], &action.args[idx + 1..]),
            );
        }
    }

    if matches!(
        base.to_ascii_lowercase().as_str(),
        "sh" | "bash"
            | "zsh"
            | "dash"
            | "ksh"
            | "ash"
            | "fish"
            | "csh"
            | "tcsh"
            | "pwsh"
            | "powershell"
            | "cmd"
            | "cmd.exe"
    ) {
        for (idx, arg) in action.args.iter().enumerate() {
            let is_cmd_flag =
                (arg.starts_with('-') && !arg.starts_with("--") && arg.ends_with('c'))
                    || arg.eq_ignore_ascii_case("/c")
                    || arg == "--command";
            if is_cmd_flag {
                if let Some(script) = action.args.get(idx + 1) {
                    push_command_and_basename(&mut subjects, script);
                    for segment in script.split(['\n', ';', '|', '&', '(', ')', '`']) {
                        push_command_and_basename(&mut subjects, segment);
                    }
                }
            }
        }
    }

    if let super::PermissionDecision::Deny(reason) = super::decide_permission(
        spec,
        &super::ToolCallInfo {
            kind: "execute".into(),
            title: action.command.clone(),
            subject: subjects,
        },
    ) {
        return Some(reason);
    }

    let path_patterns: Vec<(&str, &str)> = spec
        .disallowed_tools
        .iter()
        .filter_map(|pattern| {
            let (name, glob) = super::split_pattern(pattern);
            let is_file_rule = ["Read", "Write", "Edit", "MultiEdit", "NotebookEdit"]
                .iter()
                .any(|tool| tool.eq_ignore_ascii_case(name));
            (is_file_rule && glob != "*").then_some((pattern.as_str(), glob))
        })
        .collect();
    if path_patterns.is_empty() {
        return None;
    }

    let mut candidates = Vec::new();
    let mut push_candidate = |raw_arg: &str| {
        let cleaned = raw_arg.trim_matches(|c: char| {
            c.is_whitespace() || matches!(c, '\'' | '"' | '`' | '<' | '>' | ';' | '(' | ')')
        });
        if !cleaned.is_empty() {
            candidates.push(cleaned.to_string());
            if let Some((_, rhs)) = cleaned.split_once('=') {
                let rhs_clean = rhs.trim_matches(|c: char| matches!(c, '\'' | '"' | '`'));
                if !rhs_clean.is_empty() {
                    candidates.push(rhs_clean.to_string());
                }
            }
        }
    };
    push_candidate(&action.command);
    for arg in &action.args {
        push_candidate(arg);
        for sub in arg.split(|c: char| c.is_whitespace() || matches!(c, ';' | '|' | '&')) {
            push_candidate(sub);
        }
    }

    let spec_cwd = super::normalized_policy_path(&spec.cwd.to_string_lossy());
    let eff_cwd = action
        .cwd
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| spec.cwd.clone());
    let mut home_prefixes = Vec::new();
    if let Some(h) = spec.env.get("HOME") {
        let norm = super::normalized_policy_path(h);
        if !norm.is_empty() {
            home_prefixes.push(norm);
        }
    }
    if let Ok(h) = std::env::var("HOME") {
        let norm = super::normalized_policy_path(&h);
        if !norm.is_empty() && !home_prefixes.contains(&norm) {
            home_prefixes.push(norm);
        }
    }

    for (pattern, glob) in path_patterns {
        for cand in &candidates {
            let normalized = super::normalized_policy_path(cand);
            if normalized.is_empty() {
                continue;
            }
            let abs_eff =
                super::normalized_policy_path(&eff_cwd.join(&normalized).to_string_lossy());
            let abs_spec =
                super::normalized_policy_path(&spec.cwd.join(&normalized).to_string_lossy());
            let rel_spec = abs_eff
                .strip_prefix(&format!("{spec_cwd}/"))
                .unwrap_or(&normalized);
            let tilde_match = home_prefixes.iter().any(|home| {
                abs_eff
                    .strip_prefix(&format!("{home}/"))
                    .is_some_and(|rest| super::wildcard_match(glob, &format!("~/{rest}")))
            });
            if super::wildcard_match(glob, cand)
                || super::wildcard_match(glob, &normalized)
                || super::wildcard_match(glob, &abs_eff)
                || super::wildcard_match(glob, &abs_spec)
                || super::wildcard_match(glob, rel_spec)
                || tilde_match
            {
                return Some(format!(
                    "matches SessionSpec.disallowed_tools pattern {pattern:?}"
                ));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_control_ids_are_bounded_only_while_in_flight() {
        let mut broker = Broker::default();
        for _ in 0..2048 {
            broker.admit_control("reused".into()).unwrap();
            assert!(broker.in_flight("reused"));
            assert_eq!(broker.pending(), 0);
            assert_eq!(
                broker.admit_control("reused".into()),
                Err(Error::InvalidRequest)
            );
            broker.completed("reused");
        }
        for i in 0..MAX_PENDING_CONTROL {
            broker.admit_control(i.to_string()).unwrap();
        }
        assert_eq!(
            broker.admit_control("excess".into()),
            Err(Error::BudgetExceeded(Budget::PendingControl))
        );
        assert_eq!(broker.pending(), 0);
        broker.completed("0");
        broker.admit_control("excess".into()).unwrap();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    async fn test_session_with_provider(
        sink: &std::path::Path,
    ) -> (AcpSession, Arc<crate::acp_terminal::Provider>) {
        let provider = Arc::new(crate::acp_terminal::Provider::new(
            crate::acp_terminal::Context {
                client: crate::gate_evaluation::subprocess::DockerEvaluator::new(
                    std::path::Path::new("/usr/bin/false"),
                )
                .unwrap(),
                container_id: "a".repeat(64),
                image: crate::acp_terminal::FIXTURE_IMAGE.into(),
                workspace: "/workspace".into(),
                scratch: "/scratch".into(),
                alive: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            },
            "run-fixture".into(),
            "sess-1".into(),
            "peer-1".into(),
            crate::acp_terminal::Budgets::default(),
        ));
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.args(["-c", "cat > \"$1\"", "sh"])
            .arg(sink)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let stderr_buf = Arc::new(Mutex::new(String::new()));
        let stderr_task = {
            let buf = Arc::clone(&stderr_buf);
            tokio::spawn(async move {
                let tail = drain_to_tail(stderr, STDERR_TAIL_CAP).await;
                *buf.lock().unwrap() = tail;
            })
        };
        let (permission_responder, permission_answers) =
            crate::live_permission::PermissionResponder::channel();
        let mut broker = Broker::default();
        broker.provider = Some(TerminalBackend::Contained(Arc::clone(&provider)));
        let session = AcpSession {
            terminals: broker,
            session_id: "sess-1".into(),
            protocol: Client::with_terminal_support(),
            model: "fixture".into(),
            spec: SessionSpec {
                cwd: PathBuf::from("."),
                prompt: PromptMode::SingleShot("test".into()),
                append_system_prompt: None,
                model: "fixture".into(),
                effort: "high".into(),
                session_id: "sess-1".into(),
                resume: None,
                permission_mode: None,
                allowed_tools: vec![],
                disallowed_tools: vec![],
                tools: vec![],
                writable: true,
                settings_json: None,
                json_schema: None,
                max_budget_usd: None,
                max_turns: None,
                env: Default::default(),
                sandbox: None,
                hook_status: None,
            },
            child,
            container: None,
            profile_home: None,
            stdin: Some(stdin),
            child_status: None,
            cleanup_failure: None,
            drain_deadline: None,
            lines: BoundedLines::new_strict(stdout),
            stderr_buf,
            stderr_task: Some(stderr_task),
            queue: VecDeque::new(),
            tool_calls: HashMap::new(),
            permission_responder,
            permission_answers,
            deferred_permission_answer: None,
            pending_permissions: HashMap::new(),
            seen_permission_ids: std::collections::HashSet::new(),
            message_text: String::new(),
            message_id: None,
            last_usage: None,
            previous_cost_total: None,
            handshake_bytes: 0,
            handshake_complete: true,
            saw_result: false,
            exit: None,
            test_resource_evidence: None,
        };
        (session, provider)
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    fn base_resource_evidence() -> crate::acp_resources::ResourceEvidence {
        let now = chrono::Utc::now();
        let counters = crate::acp_resources::Counters {
            oom_kill: Some(0),
            pids_max: Some(0),
            throttled_usec: Some(0),
        };
        crate::acp_resources::ResourceEvidence {
            container_id: "a".repeat(64),
            image: crate::acp_terminal::FIXTURE_IMAGE.into(),
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
            state: Some(crate::acp_resources::ContainerState {
                running: false,
                oom_killed: false,
                exit_code: 0,
            }),
            sample: Some(crate::acp_resources::Sample {
                owner: "c".repeat(32),
                elapsed_ms: 100,
                complete: true,
                wall_clock_expired: false,
                baseline: counters.clone(),
                current: counters,
                usage: None,
            }),
            unavailable: vec![],
            namespace_cleanup_confirmed: true,
            failure: None,
            terminal_budget_hit: Default::default(),
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn terminal_rapid_output_rate_refusals_bound_transcript_while_returning_rpc_errors() {
        let dir = tempfile::tempdir().unwrap();
        let sink = dir.path().join("responses.ndjson");
        let (mut session, provider) = test_session_with_provider(&sink).await;
        provider.insert_test_active_handle("terminal-live");

        // 10 allowed reads + 2,000 rapid OutputRate refusals = 2,010 requests.
        let total_requests = 2_010u64;
        for req_id in 1..=total_requests {
            session
                .handle_terminal(
                    json!(req_id),
                    terminal::OUTPUT,
                    json!({"sessionId":"peer-1","terminalId":"terminal-live"}),
                )
                .await
                .unwrap();
            let completion = session.terminals.next().await.unwrap();
            session.handle_frame(completion).await.unwrap();
        }
        assert_eq!(session.terminals.refused_terminal_requests, 2_000);
        assert_eq!(
            session.terminals.suppressed_terminal_refusals,
            2_000 - MAX_REFUSED_TERMINAL_TRANSCRIPT
        );
        // Transcript stays O(MAX_REFUSED_TERMINAL_TRANSCRIPT), far below 4,020 events.
        assert!(
            session.queue.len() <= 2 * (10 + MAX_REFUSED_TERMINAL_TRANSCRIPT),
            "unexpectedly large transcript queue len: {}",
            session.queue.len()
        );

        // Close stdin and verify every single JSON-RPC response was still delivered.
        session.stdin.take();
        let _ = session.child.wait().await;
        let wire = std::fs::read_to_string(&sink).unwrap();
        let lines: Vec<Value> = wire
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), total_requests as usize);
        let rpc_refusals = lines
            .iter()
            .filter(|v| v["error"]["data"]["budget"] == "outputRate")
            .count();
        assert_eq!(rpc_refusals, 2_000);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[tokio::test]
    async fn terminal_budget_hit_wires_into_session_resource_evidence_and_exit_status() {
        use crate::acp_resources::FailureClass;

        let dir = tempfile::tempdir().unwrap();
        let sink = dir.path().join("responses.ndjson");
        let (mut session, provider) = test_session_with_provider(&sink).await;
        session.test_resource_evidence = Some(base_resource_evidence());
        provider.insert_test_active_handle("terminal-live");

        // Trigger OutputRate budget hit via complete_terminal.
        for req_id in 1..=11u64 {
            session
                .handle_terminal(
                    json!(req_id),
                    terminal::OUTPUT,
                    json!({"sessionId":"peer-1","terminalId":"terminal-live"}),
                )
                .await
                .unwrap();
            let completion = session.terminals.next().await.unwrap();
            session.handle_frame(completion).await.unwrap();
        }
        assert!(session.terminals.saw_budget_hit);

        // (a) Recoverable terminal budget hit + successful session => failure == None.
        session.exit = Some(SessionExit::Completed);
        assert_eq!(session.exit_status(), Some(SessionExit::Completed));
        let mut passing_evidence = session.resource_evidence().unwrap();
        passing_evidence.classify(false);
        assert_eq!(passing_evidence.failure, None);

        // (b) Terminal budget hit + failed session => FailureClass::TerminalBudget.
        session.exit = Some(SessionExit::Failed("adapter aborted turn".into()));
        match session.exit_status() {
            Some(SessionExit::Failed(msg)) => {
                assert!(
                    msg.contains("ACP terminal budget exhaustion observed"),
                    "{msg}"
                );
                assert!(
                    msg.contains("Session failure: adapter aborted turn"),
                    "{msg}"
                );
            }
            other => panic!("expected Failed exit_status, got {other:?}"),
        }
        let mut failing_evidence = session.resource_evidence().unwrap();
        failing_evidence.classify(true);
        assert_eq!(failing_evidence.failure, Some(FailureClass::TerminalBudget));

        session.kill_child().await;

        // Also verify receipt-driven attribution (`terminalExited` with non-null vs null `budget`).
        let sink2 = dir.path().join("responses2.ndjson");
        let (mut session2, _) = test_session_with_provider(&sink2).await;
        session2.test_resource_evidence = Some(base_resource_evidence());
        session2
            .handle_frame(Input::TerminalReceipt(
                json!({"terminalExited": {"budget": null}}),
            ))
            .await
            .unwrap();
        assert!(!session2.terminals.saw_budget_hit);
        session2.exit = Some(SessionExit::Failed("non-budget error".into()));
        let mut non_budget_ev = session2.resource_evidence().unwrap();
        non_budget_ev.classify(true);
        assert_eq!(non_budget_ev.failure, Some(FailureClass::Other));

        session2
            .handle_frame(Input::TerminalReceipt(
                json!({"terminalExited": {"budget": "commandLifetime"}}),
            ))
            .await
            .unwrap();
        assert!(session2.terminals.saw_budget_hit);
        let mut exited_budget_ev = session2.resource_evidence().unwrap();
        exited_budget_ev.classify(true);
        assert_eq!(exited_budget_ev.failure, Some(FailureClass::TerminalBudget));
        session2.kill_child().await;
    }

    #[test]
    fn sgian_terminal_prohibition_enforces_worker_and_authority_deny_patterns() {
        let mut spec = SessionSpec {
            cwd: PathBuf::from("/repo"),
            prompt: PromptMode::SingleShot("test".into()),
            append_system_prompt: None,
            model: "fixture".into(),
            effort: "high".into(),
            session_id: "sess-p".into(),
            resume: None,
            permission_mode: None,
            allowed_tools: vec!["Bash".into()],
            disallowed_tools: vec![
                "Bash(git push*)".into(),
                "Bash(curl*)".into(),
                "Read(.kranz/serve.grant.key)".into(),
                "Read(~/.kranz/**)".into(),
            ],
            tools: vec![],
            writable: true,
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: std::collections::HashMap::from([("HOME".into(), "/home/tester".into())]),
            sandbox: None,
            hook_status: None,
        };
        let make_create = |cmd: &str, args: &[&str]| {
            let create = kranz_acp::terminal::Create {
                session_id: "peer-1".into(),
                command: cmd.into(),
                args: args.iter().map(|s| (*s).to_string()).collect(),
                env: vec![],
                cwd: Some("/repo".into()),
                output_byte_limit: Some(65_536),
                meta: None,
            };
            let val = json!({
                "kind": "execute",
                "rawInput": {
                    "command": create.command,
                    "args": create.args,
                }
            });
            (create, val)
        };

        let (ok_cmd, ok_val) = make_create("/bin/sh", &["-c", "cargo test --workspace"]);
        assert!(sgian_terminal_prohibition(&spec, &ok_cmd, &ok_val).is_none());

        for (cmd, args) in [
            ("/usr/bin/curl", &["https://example.invalid"][..]),
            ("env", &["FOO=1", "git", "push", "origin", "main"][..]),
            ("/bin/sh", &["-lc", "echo a; /usr/bin/git push"][..]),
            ("cat", &[".kranz/serve.grant.key"][..]),
            (
                "/bin/sh",
                &["-c", "cat /home/tester/.kranz/config.json"][..],
            ),
        ] {
            let (create, val) = make_create(cmd, args);
            assert!(
                sgian_terminal_prohibition(&spec, &create, &val).is_some(),
                "expected prohibition for {cmd} {args:?}"
            );
        }

        spec.writable = false;
        assert!(sgian_terminal_prohibition(&spec, &ok_cmd, &ok_val).is_some());
    }
}
