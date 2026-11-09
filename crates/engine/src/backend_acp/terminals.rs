//! Consent and asynchronous terminal routing. Production admission remains closed.
use super::*;
use crate::acp_terminal::{Budgets, Context, Provider};
use kranz_acp::terminal::{self, Budget, Create, Error, Target, TerminalProvider};
use std::collections::HashSet;

const MAX_PENDING_CONTROL: usize = 32;

#[derive(Default)]
pub(super) struct Broker {
    prepared: Option<(Context, String, Budgets)>,
    pub(super) provider: Option<Arc<Provider>>,
    tasks: tokio::task::JoinSet<TerminalCompletion>,
    pub(super) cleanup_recorded: bool,
    controls: HashSet<String>,
    creating: HashSet<String>,
}

impl Broker {
    pub(super) fn prepare(
        run: Option<(String, Budgets)>,
        container: Option<&crate::acp_container::OwnedContainer>,
        spec: &SessionSpec,
    ) -> Result<Self> {
        let prepared = run
            .map(|(run, budgets)| {
                if run.trim().is_empty() || run.len() > 256 {
                    return Err(failure());
                }
                let context = container.ok_or_else(failure)?.terminal_context(spec)?;
                Ok((context, run, budgets))
            })
            .transpose()?;
        Ok(Self {
            prepared,
            provider: None,
            tasks: tokio::task::JoinSet::new(),
            cleanup_recorded: false,
            controls: HashSet::new(),
            creating: HashSet::new(),
        })
    }

    pub(super) fn admitted(&self) -> bool {
        self.prepared.is_some()
    }

    pub(super) fn bind(&mut self, engine: &str, peer: &str) {
        if let Some((context, run, budgets)) = self.prepared.take() {
            self.provider = Some(Arc::new(Provider::new(
                context,
                run,
                engine.into(),
                peer.into(),
                budgets,
            )));
        }
    }

    pub(super) async fn next(&mut self) -> Result<Input> {
        let Some(provider) = &self.provider else {
            return std::future::pending().await;
        };
        tokio::select! {
            biased;
            _ = provider.failure() => Err(failure()),
            receipt = provider.next_receipt() => receipt.map(Input::TerminalReceipt).map_err(|_| failure()),
            result = self.tasks.join_next(), if !self.tasks.is_empty() =>
                result.ok_or_else(failure)?.map(Input::Terminal).map_err(|_| failure()),
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

fn failure() -> EngineError {
    EngineError::Backend(
        "contained terminal operation or cleanup failed; ending owned namespace".into(),
    )
}

impl AcpSession {
    pub(super) async fn handle_terminal(
        &mut self,
        id: Value,
        method: &str,
        params: Value,
    ) -> Result<()> {
        let provider = self.terminals.provider.clone().ok_or_else(failure)?;
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
        self.queue.push_back(AgentEvent::Other {
            raw: json!({"terminalRequested": {
                "method":method,"requestId":id,"scope":provider.scope,
                "requestDigest":crate::live_permission::digest(&params)?,
            }}),
        });
        let parsed = if method == terminal::CREATE {
            serde_json::from_value::<Create>(params)
                .map_err(|_| Error::InvalidRequest)
                .and_then(|action| provider.normalize(action))
                .map(Ok)
        } else if matches!(
            method,
            terminal::OUTPUT | terminal::WAIT_FOR_EXIT | terminal::KILL | terminal::RELEASE
        ) {
            serde_json::from_value::<Target>(params)
                .map_err(|_| Error::InvalidRequest)
                .and_then(|target| {
                    target.validate()?;
                    if target.session_id != provider.scope.peer_session_id {
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
                return self
                    .complete_terminal(TerminalCompletion {
                        id,
                        method: method.into(),
                        result: Err(error),
                        receipt: json!({"refusedBeforeExecution":true}),
                        permission_id: None,
                    })
                    .await
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
                self.seen_permission_ids.insert(peer_id);
                let value = provider.action(&action).map_err(|_| failure())?;
                let options = vec![
                    json!({"optionId":"execute-once","kind":"allow_once","name":"Execute this exact command"}),
                    json!({"optionId":"deny-once","kind":"reject_once","name":"Deny"}),
                ];
                let now = chrono::Utc::now();
                // Arbitrary deny patterns cannot be soundly matched against a
                // shell program. This fixture refuses any restricted tool set.
                let prohibition = (!self.spec.writable || !self.spec.disallowed_tools.is_empty())
                    .then(|| {
                        "terminal execution is prohibited by this session's tool policy".into()
                    });
                let proposal = crate::live_permission::Proposal {
                    id: proposal_id,
                    engine_session_id: self.session_id.clone(),
                    peer_session_id: provider.scope.peer_session_id.clone(),
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
                    let scope = provider.scope.clone();
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
        let provider = self.terminals.provider.clone().ok_or_else(failure)?;
        if !allow {
            provider
                .cancel_reservation(&proposal.id)
                .map_err(|_| failure())?;
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
            .map_err(|_| failure())?;
        self.terminals
            .creating
            .insert(serde_json::to_string(&proposal.peer_request_id)?);
        self.terminals.tasks.spawn(async move {
            let scope = provider.scope.clone();
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
        // A prompt result while consent or an operation is pending cannot turn
        // a lost command outcome into successful completion.
        if !self.terminals.tasks.is_empty() || !self.pending_permissions.is_empty() {
            return Err(failure());
        }
        let receipts = tokio::time::timeout(std::time::Duration::from_secs(8), provider.close())
            .await
            .map_err(|_| failure())?
            .map_err(|_| failure())?;
        for raw in provider.drain_receipts().map_err(|_| failure())? {
            self.queue.push_back(AgentEvent::Other { raw });
        }
        self.queue.push_back(AgentEvent::Other {
            raw: json!({"terminalSessionCleanup": {
                "scope":provider.scope,"receipts":receipts,"confirmed":true,
            }}),
        });
        Ok(())
    }
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
}
