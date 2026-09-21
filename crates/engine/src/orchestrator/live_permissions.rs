//! The existing engine remains the sole writer and approval authority.
use super::*;
use crate::live_permission::{self, Actor, PermissionResponder, Request, Resolution};
use crate::runner::{PermissionNotice, PermissionPacket, PermissionRelay};
use tokio::sync::mpsc;

impl MissionEngine {
    fn permission_authority(&self) -> Result<(String, String)> {
        let events = EventLog::read_events(&self.paths.events_file())?;
        let plan = events
            .iter()
            .rev()
            .find_map(|event| match &event.kind {
                EventKind::PlanApproved { plan, .. } | EventKind::PlanRevised { plan, .. } => {
                    Some(plan)
                }
                _ => None,
            })
            .ok_or_else(|| EngineError::InvalidState("permission needs an approved plan".into()))?;
        let mission = &self.state.mission;
        Ok((
            live_permission::digest(plan)?,
            live_permission::digest(&(
                &self.state.config,
                &mission.base_sha,
                &mission.command_grants,
                &mission.deny_exceptions,
                &mission.egress_grants,
                &mission.touch_set,
                &mission.standards_manifest,
            ))?,
        ))
    }

    pub(super) fn permission_channel(
        &mut self,
    ) -> Result<(PermissionRelay, mpsc::Receiver<PermissionPacket>)> {
        // Restored records have no response capability. Never reconstruct one.
        self.close_permissions(
            None,
            "peer ended or engine restarted; response is not replayable",
        )?;
        let (plan_digest, policy_digest) = self.permission_authority()?;
        let (sender, receiver) = mpsc::channel(live_permission::MAX_PENDING);
        Ok((
            PermissionRelay {
                sender,
                plan_digest,
                policy_digest,
                candidate: None,
            },
            receiver,
        ))
    }

    pub(super) fn handle_permission_packet(&mut self, packet: PermissionPacket) -> Result<()> {
        let PermissionPacket {
            events,
            mut binding,
            candidate,
            notice,
            persisted,
        } = packet;
        let result = (|| {
            for mut event in events {
                if let EventKind::WorkerSpawned {
                    candidate: link, ..
                } = &mut event
                {
                    *link = candidate.clone();
                }
                self.emit(event)?;
            }
            binding.mission_id = self.paths.mission_id.clone();
            match notice {
                PermissionNotice::Requested(proposal, responder) => {
                    let authority = self.permission_authority()?;
                    if authority != (binding.plan_digest.clone(), binding.policy_digest.clone()) {
                        return Err(EngineError::InvalidState(
                            "worker authority changed before permission request".into(),
                        ));
                    }
                    let request = Request::new(*proposal, binding)?;
                    // Never make a redacted preview look like the complete action
                    // an operator authorized. An uninspectable request stops the run.
                    let mut retained = serde_json::to_value(&request)?;
                    if !scrub::scrub_json_value(&mut retained, "permission").is_empty() {
                        return Err(EngineError::InvalidState(
                            "permission action requires redaction and cannot be approved exactly"
                                .into(),
                        ));
                    }
                    let id = request.proposal.id.clone();
                    let prohibition = request.proposal.prohibition.clone();
                    self.emit(EventKind::PermissionRequested {
                        request: request.clone(),
                    })?;
                    self.permission_handles.insert(id.clone(), responder);
                    if let Some(reason) = prohibition {
                        self.resolve_live_permission(Resolution {
                            request_id: id,
                            binding_digest: request.binding_digest,
                            allow: false,
                            actor: Actor::Policy,
                            reason,
                        })?;
                    }
                }
                PermissionNotice::Responded(request_id, delivery) => {
                    let record = self.state.permissions.get(&request_id).ok_or_else(|| {
                        EngineError::InvalidState(
                            "permission receipt has no durable request".into(),
                        )
                    })?;
                    if record.request.binding != binding {
                        return Err(EngineError::InvalidState(
                            "permission receipt came from another run".into(),
                        ));
                    }
                    self.emit(EventKind::PermissionResponseRecorded {
                        request_id: request_id.clone(),
                        delivery,
                    })?;
                    self.emit(EventKind::PermissionClosed {
                        request_id: request_id.clone(),
                        reason: "one-call response recorded; tool outcome remains separate".into(),
                    })?;
                    self.permission_handles.remove(&request_id);
                }
                PermissionNotice::Finished => self.close_permissions(
                    Some(&binding.run_id),
                    "peer ended; no further permission response can be delivered",
                )?,
            }
            Ok(())
        })();
        // A rejected peer loses only its response capability. The runner aborts
        // that session on a negative acknowledgement; sibling candidates keep
        // running. Log/durability failures instead stop and join the whole batch.
        let _ = persisted.send(result.as_ref().map(|_| ()).map_err(ToString::to_string));
        match result {
            Err(EngineError::InvalidState(_) | EngineError::Backend(_)) => Ok(()),
            other => other,
        }
    }

    pub(super) fn resolve_live_permission(&mut self, resolution: Resolution) -> Result<()> {
        let record = self
            .state
            .permissions
            .get(&resolution.request_id)
            .ok_or_else(|| EngineError::InvalidState("unknown live permission".into()))?;
        let handle: PermissionResponder = self
            .permission_handles
            .get(&resolution.request_id)
            .cloned()
            .ok_or_else(|| EngineError::InvalidState("permission peer is no longer live".into()))?;
        if !record.pending(chrono::Utc::now())
            || record.request.binding_digest != resolution.binding_digest
            || self.permission_authority()?
                != (
                    record.request.binding.plan_digest.clone(),
                    record.request.binding.policy_digest.clone(),
                )
        {
            return Err(EngineError::InvalidState(
                "permission is stale, expired or already resolved".into(),
            ));
        }
        let proposal = record.request.proposal.clone();
        // The reducer rejects policy prohibitions and uncertified allow options.
        self.emit(EventKind::PermissionResolved {
            resolution: resolution.clone(),
        })?;
        if handle.respond(&proposal, resolution.allow).is_err() {
            self.emit(EventKind::PermissionClosed {
                request_id: resolution.request_id.clone(),
                reason: "decision recorded but response channel unavailable; no retry".into(),
            })?;
            self.permission_handles.remove(&resolution.request_id);
        }
        Ok(())
    }

    pub(super) fn close_permissions(&mut self, run: Option<&str>, reason: &str) -> Result<()> {
        let ids: Vec<_> = self
            .state
            .permissions
            .iter()
            .filter(|(_, r)| {
                r.closed.is_none() && run.is_none_or(|id| r.request.binding.run_id == id)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for request_id in ids {
            self.emit(EventKind::PermissionClosed {
                request_id: request_id.clone(),
                reason: reason.into(),
            })?;
            self.permission_handles.remove(&request_id);
        }
        Ok(())
    }

    pub(super) async fn permission_tick(&mut self) -> Result<()> {
        self.drain_control().await?;
        let expired: Vec<_> = self
            .state
            .permissions
            .iter()
            .filter(|(_, r)| {
                r.closed.is_none()
                    && r.resolution.is_none()
                    && chrono::Utc::now() >= r.request.proposal.deadline
            })
            .map(|(id, _)| id.clone())
            .collect();
        for request_id in expired {
            self.emit(EventKind::PermissionClosed {
                request_id: request_id.clone(),
                reason: "deadline expired".into(),
            })?;
            self.permission_handles.remove(&request_id);
            if let Some(cancel) = &self.permission_cancel {
                cancel.notify_waiters();
            }
        }
        Ok(())
    }

    pub(super) async fn drive_permission_worker<F, T>(
        &mut self,
        future: F,
        receiver: &mut mpsc::Receiver<PermissionPacket>,
    ) -> Result<T>
    where
        F: std::future::Future<Output = Result<T>>,
    {
        tokio::pin!(future);
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut failure = None;
        loop {
            tokio::select! {
                result = &mut future => return failure.map_or(result, Err),
                Some(packet) = receiver.recv() => {
                    if failure.is_none() { failure = self.handle_permission_packet(packet).err(); }
                }
                _ = tick.tick() => {
                    if failure.is_none() { failure = self.permission_tick().await.err(); }
                }
            }
            if failure.is_some() {
                if let Some(cancel) = &self.permission_cancel {
                    cancel.notify_waiters();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{AgentEvent, AgentSession, SessionExit, SessionSpec};
    use crate::live_permission::{Answer, Delivery, Proposal};
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn engine() -> Option<(tempfile::TempDir, MissionEngine)> {
        let (dir, root) = super::super::tests::lessons_test_repo()?;
        let backend = Arc::new(crate::backend_mock::MockBackend::new());
        let mut engine = MissionEngine::create(
            backend,
            root,
            "permission fixture",
            MissionConfig::default(),
        )
        .unwrap();
        let plan = serde_json::from_value(json!({
            "goal":"permission fixture", "validationContract":[], "milestones":[{
                "title":"M1", "features":[{"title":"F1", "spec":"fixture", "validationCriteria":["one effect"]}]
            }]
        })).unwrap();
        engine
            .emit(EventKind::PlanApproved {
                plan,
                base_sha: None,
            })
            .unwrap();
        engine
            .emit(EventKind::MilestoneStarted {
                milestone_id: "ms-1".into(),
                start_sha: "fixture-base".into(),
            })
            .unwrap();
        engine
            .emit(EventKind::FeatureStarted {
                feature_id: "f-1-1".into(),
            })
            .unwrap();
        Some((dir, engine))
    }

    fn proposal(session_id: &str) -> Proposal {
        let action = json!({"kind":"execute","rawInput":{"command":"printf fixture"}});
        let options = vec![
            json!({"optionId":"one","kind":"allow_once"}),
            json!({"optionId":"no","kind":"reject_once"}),
        ];
        let now = chrono::Utc::now();
        Proposal {
            id: format!("permission-{}", uuid::Uuid::new_v4()),
            engine_session_id: session_id.into(),
            peer_session_id: "peer-fixture".into(),
            peer_request_id: json!(100),
            tool_call_id: "tool-1".into(),
            action_digest: live_permission::digest(&action).unwrap(),
            options_digest: live_permission::digest(&options).unwrap(),
            action,
            options,
            observed_at: now,
            deadline: now + chrono::Duration::seconds(300),
            prohibition: None,
        }
    }

    struct Peer {
        log_path: PathBuf,
        drained: Arc<AtomicUsize>,
        effects: Arc<AtomicUsize>,
    }

    struct Session {
        peer: Peer,
        proposal: Proposal,
        responder: PermissionResponder,
        answers: mpsc::Receiver<Answer>,
        step: usize,
        aborted: bool,
    }

    #[async_trait::async_trait]
    impl AgentBackend for Peer {
        async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>> {
            let (responder, answers) = PermissionResponder::channel();
            Ok(Box::new(Session {
                peer: Peer {
                    log_path: self.log_path.clone(),
                    drained: self.drained.clone(),
                    effects: self.effects.clone(),
                },
                proposal: proposal(&spec.session_id),
                responder,
                answers,
                step: 0,
                aborted: false,
            }))
        }
    }

    #[async_trait::async_trait]
    impl AgentSession for Session {
        fn session_id(&self) -> String {
            self.proposal.engine_session_id.clone()
        }
        fn permission_responder(&self) -> Option<PermissionResponder> {
            Some(self.responder.clone())
        }
        async fn next_event(&mut self) -> Result<Option<AgentEvent>> {
            if self.aborted {
                return Ok(None);
            }
            let event = match self.step {
                0 => AgentEvent::PermissionRequested {
                    proposal: Box::new(self.proposal.clone()),
                    raw: json!({}),
                },
                1 => {
                    self.peer.drained.fetch_add(1, Ordering::SeqCst);
                    AgentEvent::Text {
                        text: "still draining while consent waits".into(),
                        raw: json!({}),
                    }
                }
                2 => {
                    let answer = self.answers.recv().await.expect("broker response");
                    assert_eq!(answer.proposal, self.proposal);
                    // Read the real append-only log at the effect boundary. A
                    // channel message before the fsynced resolution fails here.
                    let state =
                        reducer::fold(&EventLog::read_events(&self.peer.log_path)?).unwrap();
                    let record = &state.permissions[&self.proposal.id];
                    assert!(record.resolution.as_ref().unwrap().allow);
                    assert!(record.delivery.is_none());
                    assert!(answer.allow);
                    assert!(
                        self.answers.try_recv().is_err(),
                        "duplicate click must not deliver twice"
                    );
                    self.peer.effects.fetch_add(1, Ordering::SeqCst);
                    AgentEvent::PermissionResponded {
                        request_id: self.proposal.id.clone(),
                        delivery: Delivery::Sent,
                        raw: json!({}),
                    }
                }
                3 => AgentEvent::Result {
                    text: r#"{"result":"partial","summary":"fixture"}"#.into(),
                    is_error: false,
                    usage: TokenUsage::default(),
                    cost_usd: None,
                    num_turns: None,
                    raw: json!({}),
                },
                _ => return Ok(None),
            };
            self.step += 1;
            Ok(Some(event))
        }
        async fn send_user_message(&mut self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn abort(&mut self) -> Result<()> {
            self.aborted = true;
            Ok(())
        }
        fn exit_status(&self) -> Option<SessionExit> {
            Some(if self.aborted {
                SessionExit::Aborted
            } else {
                SessionExit::Completed
            })
        }
    }

    #[tokio::test]
    async fn live_permission_parallel_buffers_drain_and_persist_before_one_effect_each() {
        let Some((_dir, mut engine)) = engine() else {
            return;
        };
        let (relay, mut receiver) = engine.permission_channel().unwrap();
        let paths = engine.paths.clone();
        let cfg = engine.state.config.clone();
        let feature = engine.state.mission.milestones[0].features[0].clone();
        let drained = Arc::new(AtomicUsize::new(0));
        let effects = Arc::new(AtomicUsize::new(0));
        let peer = Peer {
            log_path: paths.events_file(),
            drained: drained.clone(),
            effects: effects.clone(),
        };
        let operator_paths = paths.clone();
        let operator = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let events = EventLog::read_events(&operator_paths.events_file()).unwrap();
                    let requests: Vec<_> = events
                        .iter()
                        .filter_map(|e| match &e.kind {
                            EventKind::PermissionRequested { request } => Some(request),
                            _ => None,
                        })
                        .collect();
                    if requests.len() == 2 && drained.load(Ordering::SeqCst) == 2 {
                        for request in requests {
                            for binding_digest in [
                                "wrong-binding".to_string(),
                                request.binding_digest.clone(),
                                request.binding_digest.clone(),
                            ] {
                                control::enqueue(
                                    &operator_paths,
                                    &ControlCommand::ResolvePermission {
                                        resolution: Resolution {
                                            request_id: request.proposal.id.clone(),
                                            binding_digest,
                                            allow: true,
                                            actor: Actor::LocalRepositoryAuthority,
                                            reason: "fixture operator".into(),
                                        },
                                    },
                                )
                                .unwrap();
                            }
                        }
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        });
        let cancel = Arc::new(tokio::sync::Notify::new());
        engine.permission_cancel = Some(cancel.clone());
        let run = || {
            runner::run_worker_in_buffered_controlled(
                &peer,
                &paths,
                &cfg,
                &feature,
                "goal",
                "M1",
                None,
                &paths.repo_root,
                None,
                &[],
                &[],
                &[],
                AuthVerdict::Inconclusive,
                &[],
                None,
                None,
                Some(relay.clone()),
                Some(cancel.clone()),
            )
        };
        let future = async {
            let (a, b) = tokio::join!(run(), run());
            assert!(a?.0.is_empty());
            assert!(b?.0.is_empty());
            Ok(())
        };
        tokio::time::timeout(
            Duration::from_secs(8),
            engine.drive_permission_worker(future, &mut receiver),
        )
        .await
        .unwrap()
        .unwrap();
        operator.await.unwrap();
        engine.permission_cancel = None;
        assert_eq!(effects.load(Ordering::SeqCst), 2);
        assert_eq!(engine.state.permissions.len(), 2);
        for record in engine.state.permissions.values() {
            assert_eq!(record.delivery, Some(Delivery::Sent));
            assert!(record.closed.is_some());
            assert!(record.request.proposal.observed_at <= record.resolved_at.unwrap());
            assert!(record.resolved_at <= record.responded_at);
        }
        assert!(engine.state.mission.command_grants.is_empty());
        assert!(engine.state.mission.deny_exceptions.is_empty());
        let events = EventLog::read_events(&paths.events_file()).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.kind, EventKind::PermissionResolved { .. }))
                .count(),
            2
        );
        assert_eq!(reducer::fold(&events).unwrap().permissions.len(), 2);
    }
    fn seed_request(engine: &mut MissionEngine) -> Request {
        engine
            .emit(EventKind::WorkerSpawned {
                backend: Some(BackendKind::Acp),
                run_id: "fixture-run".into(),
                role: Role::Worker,
                feature_id: Some("f-1-1".into()),
                milestone_id: None,
                candidate: None,
                executor_route: None,
                sdk_session_id: "fixture-session".into(),
                model: "fixture".into(),
                quant: "n/a".into(),
                weight_hash: None,
                prompt_hash: "fixture".into(),
                transcript_path: MissionPaths::transcript_rel("fixture-run"),
            })
            .unwrap();
        let (plan_digest, policy_digest) = engine.permission_authority().unwrap();
        let request = Request::new(
            proposal("fixture-session"),
            live_permission::Binding {
                mission_id: engine.paths.mission_id.clone(),
                run_id: "fixture-run".into(),
                workspace: engine.paths.repo_root.display().to_string(),
                plan_digest,
                policy_digest,
            },
        )
        .unwrap();
        engine
            .emit(EventKind::PermissionRequested {
                request: request.clone(),
            })
            .unwrap();
        request
    }

    #[test]
    fn live_permission_restart_closes_without_replay_or_extending_deadline() {
        for checkpoint in ["requested", "decided", "sent"] {
            let Some((_dir, mut engine)) = engine() else {
                return;
            };
            let request = seed_request(&mut engine);
            if checkpoint != "requested" {
                engine
                    .emit(EventKind::PermissionResolved {
                        resolution: Resolution {
                            request_id: request.proposal.id.clone(),
                            binding_digest: request.binding_digest.clone(),
                            allow: true,
                            actor: Actor::LocalRepositoryAuthority,
                            reason: "synthetic crash-window consent".into(),
                        },
                    })
                    .unwrap();
            }
            if checkpoint == "sent" {
                engine
                    .emit(EventKind::PermissionResponseRecorded {
                        request_id: request.proposal.id.clone(),
                        delivery: crate::live_permission::Delivery::Sent,
                    })
                    .unwrap();
            }
            let before_restart = engine.state.permissions[&request.proposal.id].clone();
            let paths = engine.paths.clone();
            drop(engine);
            let mut restored = MissionEngine::resume(
                Arc::new(crate::backend_mock::MockBackend::new()),
                &paths.repo_root,
                &paths.mission_id,
                LockForce::No,
            )
            .unwrap();
            let (_relay, _receiver) = restored.permission_channel().unwrap();
            let record = &restored.state.permissions[&request.proposal.id];
            assert!(record.closed.is_some());
            assert_eq!(record.resolution, before_restart.resolution, "{checkpoint}");
            assert_eq!(record.delivery, before_restart.delivery, "{checkpoint}");
            assert_eq!(record.request.proposal.deadline, request.proposal.deadline);
            let before = std::fs::read(paths.events_file()).unwrap();
            assert!(restored
                .resolve_live_permission(Resolution {
                    request_id: request.proposal.id,
                    binding_digest: request.binding_digest,
                    allow: true,
                    actor: Actor::LocalRepositoryAuthority,
                    reason: "stale click".into(),
                })
                .is_err());
            assert_eq!(std::fs::read(paths.events_file()).unwrap(), before);
        }
    }

    #[test]
    fn live_permission_fold_uses_recorded_deadline_and_rejects_foreign_session() {
        let Some((_dir, mut engine)) = engine() else {
            return;
        };
        let request = seed_request(&mut engine);
        let resolution = Resolution {
            request_id: request.proposal.id.clone(),
            binding_digest: request.binding_digest.clone(),
            allow: true,
            actor: Actor::LocalRepositoryAuthority,
            reason: "fixture".into(),
        };
        let mut state = engine.state.clone();
        let expired = Event {
            seq: state.last_seq + 1,
            ts: request.proposal.deadline,
            mission_id: engine.paths.mission_id.clone(),
            kind: EventKind::PermissionResolved { resolution },
        };
        assert!(reducer::apply(&mut state, &expired).is_err());
        assert!(state.permissions[&request.proposal.id].resolution.is_none());
        let mut foreign = request.proposal.clone();
        foreign.id = format!("permission-{}", uuid::Uuid::new_v4());
        foreign.engine_session_id = "different-session".into();
        let foreign = Request::new(foreign, request.binding).unwrap();
        let before = std::fs::read(engine.paths.events_file()).unwrap();
        assert!(engine
            .emit(EventKind::PermissionRequested { request: foreign })
            .is_err());
        assert_eq!(std::fs::read(engine.paths.events_file()).unwrap(), before);
    }
    #[tokio::test]
    async fn live_permission_prohibition_and_redaction_cannot_be_overridden() {
        let Some((_dir, mut engine)) = engine() else {
            return;
        };
        let original = seed_request(&mut engine);
        let mut prohibited = original.proposal.clone();
        prohibited.id = "permission-prohibited".into();
        prohibited.prohibition = Some("protected path".into());
        let (responder, mut answers) = PermissionResponder::channel();
        let (persisted, acknowledged) = tokio::sync::oneshot::channel();
        engine
            .handle_permission_packet(PermissionPacket {
                events: vec![],
                binding: original.binding.clone(),
                candidate: None,
                notice: PermissionNotice::Requested(Box::new(prohibited.clone()), responder),
                persisted,
            })
            .unwrap();
        acknowledged.await.unwrap().unwrap();
        let answer = answers.try_recv().unwrap();
        assert!(!answer.allow);
        let denied = &engine.state.permissions[&prohibited.id];
        assert_eq!(denied.resolution.as_ref().unwrap().actor, Actor::Policy);
        assert!(!denied.resolution.as_ref().unwrap().allow);
        let before = std::fs::read(engine.paths.events_file()).unwrap();
        assert!(engine
            .resolve_live_permission(Resolution {
                request_id: prohibited.id,
                binding_digest: denied.request.binding_digest.clone(),
                allow: true,
                actor: Actor::LocalRepositoryAuthority,
                reason: "cannot override".into()
            })
            .is_err());
        assert_eq!(std::fs::read(engine.paths.events_file()).unwrap(), before);
        let mut redacted_proposal = original.proposal;
        redacted_proposal.id = "permission-secret".into();
        redacted_proposal.action =
            json!({"kind":"execute","rawInput":{"api_key":"sk-ant-api03-ScrubNofollowTestValue1"}});
        redacted_proposal.action_digest =
            live_permission::digest(&redacted_proposal.action).unwrap();
        let (responder, mut answers) = PermissionResponder::channel();
        let (persisted, acknowledged) = tokio::sync::oneshot::channel();
        engine
            .handle_permission_packet(PermissionPacket {
                events: vec![],
                binding: original.binding,
                candidate: None,
                notice: PermissionNotice::Requested(Box::new(redacted_proposal), responder),
                persisted,
            })
            .unwrap();
        let error = acknowledged.await.unwrap().unwrap_err();
        assert!(error.contains("redaction"), "{error}");
        assert!(answers.try_recv().is_err());
        assert_eq!(std::fs::read(engine.paths.events_file()).unwrap(), before);
    }

    #[tokio::test]
    async fn live_permission_interrupt_preserves_inbox_order_and_invalidates_later_click() {
        let Some((_dir, mut engine)) = engine() else {
            return;
        };
        let request = seed_request(&mut engine);
        let (responder, mut answers) = PermissionResponder::channel();
        engine
            .permission_handles
            .insert(request.proposal.id.clone(), responder);
        let cancel = Arc::new(tokio::sync::Notify::new());
        engine.permission_cancel = Some(cancel.clone());
        let notified = cancel.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        control::enqueue(&engine.paths, &ControlCommand::Pause).unwrap();
        control::enqueue(
            &engine.paths,
            &ControlCommand::ResolvePermission {
                resolution: Resolution {
                    request_id: request.proposal.id.clone(),
                    binding_digest: request.binding_digest,
                    allow: true,
                    actor: Actor::LocalRepositoryAuthority,
                    reason: "late click".into(),
                },
            },
        )
        .unwrap();
        engine.drain_control().await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), notified)
            .await
            .unwrap();
        assert_eq!(control::drain(&engine.paths).unwrap().len(), 2);
        assert!(engine.state.permissions[&request.proposal.id]
            .resolution
            .is_none());
        assert!(answers.try_recv().is_err());
        engine.permission_cancel = None;
        engine.close_permissions(None, "interrupted").unwrap();
        engine.drain_control().await.unwrap();
        assert_eq!(engine.state.mission.status, MissionStatus::Paused);
        assert!(control::drain(&engine.paths).unwrap().is_empty());
        assert!(answers.try_recv().is_err());
        assert!(engine.state.permissions[&request.proposal.id]
            .resolution
            .is_none());
    }

    #[test]
    fn live_permission_changed_policy_and_lost_response_channel_fail_closed() {
        let Some((_dir, mut engine)) = engine() else {
            return;
        };
        let request = seed_request(&mut engine);
        let (responder, receiver) = PermissionResponder::channel();
        engine
            .permission_handles
            .insert(request.proposal.id.clone(), responder);
        let resolution = Resolution {
            request_id: request.proposal.id.clone(),
            binding_digest: request.binding_digest,
            allow: true,
            actor: Actor::LocalRepositoryAuthority,
            reason: "fixture".into(),
        };
        engine
            .state
            .mission
            .command_grants
            .push("changed authority".into());
        assert!(engine.resolve_live_permission(resolution.clone()).is_err());
        assert!(engine.state.permissions[&request.proposal.id]
            .resolution
            .is_none());
        engine.state.mission.command_grants.clear();
        drop(receiver);
        engine.resolve_live_permission(resolution.clone()).unwrap();
        let record = &engine.state.permissions[&request.proposal.id];
        assert!(record.resolution.is_some());
        assert!(record.delivery.is_none());
        assert!(record.closed.as_deref().unwrap().contains("no retry"));
        assert!(engine.resolve_live_permission(resolution).is_err());
    }
}
