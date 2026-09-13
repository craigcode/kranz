//! The shared approve path (slash command + both button styles) — extracted
//! from `bridge.rs` in the monolith split (pure code motion, no behavior
//! change). See that file's module docs for the outbound/inbound runtime the
//! dispatch side of this flow hangs off.

use crate::bridge::{
    approve_mission, error_blocks, esc, mission_status, no_host_blocks, not_authorized_blocks_for,
    post_thread_note, reply_ephemeral, retire_plan_card, SharedThreads,
};
use crate::client::SlackClient;
use crate::config::SlackConfig;
use crate::host::{ApprovePendingOutcome, SharedHost};
use kranz_engine::types::MissionStatus;
use serde_json::Value;
use std::path::Path;

/// Shared approve path for the slash command and both buttons.
///
/// The reviewed plan is parked HOST-SIDE by a Ready `request_plan` — one
/// cache shared by every surface (Slack buttons, web, glasses ring), so an
/// approve from any of them consumes the same plan. With a parked plan:
/// commit it through the hosted engine — THE step the first cut of this
/// bridge skipped, which left missions queued-but-unapproved that
/// `kranz work` then refused — and either start execution through the host
/// (`start == true`) or insert into the per-repo queue. Without one
/// (never requested, or forfeited by a serve restart / idle release):
/// honest, state-aware handling (a planning mission needs `/kranz plan`
/// first; an approved-but-idle one can still be queued/started).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn approve_flow(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    host: Option<&SharedHost>,
    mission_id: &str,
    plan_identity: Option<&str>,
    user_id: Option<&str>,
    response_url: Option<&str>,
    start: bool,
    button: bool,
) {
    if !cfg.is_authorized(user_id) {
        reply_ephemeral(cfg, client, response_url, &not_authorized_blocks_for(cfg)).await;
        return;
    }

    // Consume the host-parked plan. `None` = no host, or nothing parked, so
    // fall through to the state-aware routing below. On a transient failure
    // (e.g. a turn in flight) the host re-parked the plan, so the retry
    // click finds it again.
    //
    // A button click carries the identity of the plan its card DISPLAYED,
    // and it must still be the plan parked host-side; otherwise a card the
    // reviewer scrolled back to would commit whatever a later `/kranz plan`
    // parked (M2). Threat (follow-up review M-13): reading the parked
    // identity and then approving is check-then-act across two host lock
    // takes, and `Approve`/`ApproveStart`/`RequestPlan` all run concurrently
    // on spawned tasks, so the check and the commit go through
    // `approve_pending_if`, which does both under ONE lock. The slash twin
    // reviews no card, claims no plan, and keeps the unbound approve.
    let approved = match (button, host) {
        (_, None) => None,
        (true, Some(host)) => match host.approve_pending_if(mission_id, plan_identity).await {
            Ok(ApprovePendingOutcome::Approved(branch)) => Some((branch, host)),
            Ok(ApprovePendingOutcome::StalePlan { parked }) => {
                let blocks = stale_plan_refusal(mission_id, plan_identity, Some(&parked))
                    .unwrap_or_else(|| {
                        // Unreachable: a StalePlan always names a parked plan
                        // that differs from the card's. Refuse anyway rather
                        // than fall through to starting a mission.
                        error_blocks(&format!(
                            "Couldn't confirm which plan `{}` is awaiting; nothing was \
                             approved. Run `/kranz plan {}` and approve from the new card.",
                            esc(mission_id),
                            esc(mission_id)
                        ))
                    });
                reply_ephemeral(cfg, client, response_url, &blocks).await;
                return;
            }
            Ok(ApprovePendingOutcome::NothingParked) => {
                // Threat (follow-up review M-13, the related LOW): a click
                // that NAMED a plan, with nothing parked, means the plan this
                // reviewer read is gone: approved from the web UI, released,
                // or forfeited by a restart. Falling through here started a
                // mission on a plan they never reviewed. Only an unbound
                // click (a card that names no plan) may take the state-aware
                // routing.
                if let Some(card) = plan_identity {
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url,
                        &error_blocks(&format!(
                            "The plan this card reviewed (`{card}`) is no longer parked for \
                             `{mission}`: it was approved or released elsewhere, so nothing \
                             was approved or started here. Run `/kranz plan {mission}` and \
                             approve from the new card.",
                            card = esc(card),
                            mission = esc(mission_id)
                        )),
                    )
                    .await;
                    return;
                }
                None
            }
            Err(e) => {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url,
                    &error_blocks(&format!(
                        "Couldn't approve `{}`: {}",
                        esc(mission_id),
                        esc(&e)
                    )),
                )
                .await;
                return;
            }
        },
        (false, Some(host)) => match host.approve_pending(mission_id).await {
            Ok(branch) => branch.map(|branch| (branch, host)),
            Err(e) => {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url,
                    &error_blocks(&format!(
                        "Couldn't approve `{}`: {}",
                        esc(mission_id),
                        esc(&e)
                    )),
                )
                .await;
                return;
            }
        },
    };
    if let Some((branch, host)) = approved {
        // Retire the plan card FIRST: from a button, rewrite the source
        // message into an outcome card so the second tap the old card invited
        // has nothing left to tap. Best-effort — the state change stands
        // regardless.
        if button {
            retire_plan_card(client, response_url, mission_id, Some(&branch), start).await;
        }
        if start {
            match host.start(mission_id).await {
                Ok(()) => {
                    post_thread_note(
                        cfg,
                        client,
                        threads,
                        mission_id,
                        &format!(
                            ":rocket: Plan approved and execution started (branch `{}`) — \
                         progress posts in this thread; deep inspection in the web UI.",
                            esc(&branch)
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url,
                        &error_blocks(&format!(
                            "Approved `{mission}` (branch `{branch}`) but starting failed: \
                             {error}. Queue it with `/kranz approve {mission}` or run \
                             `kranz work`.",
                            mission = esc(mission_id),
                            branch = esc(&branch),
                            error = esc(&e)
                        )),
                    )
                    .await;
                }
            }
        } else {
            // Free the just-approved engine BEFORE queueing: approve left it
            // attached in the host's registry holding the mission lock, and
            // the `kranz work` dispatcher this queue entry points at would be
            // refused with LockHeld while it stays there. Best-effort: on a
            // failed release the entry still queues, and the dispatcher's own
            // LockHeld refusal stays the honest backstop.
            match host.release(mission_id).await {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(mission = %mission_id, "release after approve found mission running")
                }
                Err(e) => {
                    tracing::warn!(mission = %mission_id, error = %e, "release after approve failed")
                }
            }
            match approve_mission(repo_root, mission_id) {
                Ok(()) => {
                    post_thread_note(
                        cfg,
                        client,
                        threads,
                        mission_id,
                        &format!(
                            ":white_check_mark: Plan approved and queued (branch `{}`) — \
                         the `kranz work` dispatcher runs it next.",
                            esc(&branch)
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url,
                        &error_blocks(&format!(
                            "Approved `{}` (branch `{}`) but queueing failed: {}",
                            esc(mission_id),
                            esc(&branch),
                            esc(&e)
                        )),
                    )
                    .await;
                }
            }
        }
        return;
    }

    // No parked plan (or no host). Route by actual mission state instead of blindly
    // queueing (the old behavior, which dead-ended at run time on unapproved
    // missions).
    match mission_status(repo_root, mission_id) {
        Ok(MissionStatus::Planning) => {
            reply_ephemeral(
                cfg,
                client,
                response_url,
                &error_blocks(&format!(
                    "No reviewed plan is pending for `{mission}` — run \
                     `/kranz plan {mission}` first, then approve from the plan message.",
                    mission = esc(mission_id)
                )),
            )
            .await;
        }
        Ok(
            status @ (MissionStatus::Complete | MissionStatus::Failed | MissionStatus::Abandoned),
        ) => {
            reply_ephemeral(
                cfg,
                client,
                response_url,
                &error_blocks(&format!(
                    "Mission `{}` is {status:?} — nothing to approve or queue.",
                    esc(mission_id)
                )),
            )
            .await;
        }
        // Approved earlier (plan committed, no live run) or paused/blocked:
        // starting/queueing is legitimate — EXCEPT when the mission is
        // actually executing right now. Status alone can't tell (approval
        // folds to Running before any run loop exists), so ask the host to
        // release its idle engine and treat an unreleasable / live-locked
        // mission as executing. This is the guard against a stale second
        // approve tap queueing a mission that is already underway.
        Ok(_) => {
            if start {
                let Some(host) = host else {
                    reply_ephemeral(cfg, client, response_url, &no_host_blocks(mission_id)).await;
                    return;
                };
                // host.start disambiguates on its own: it consumes an idle
                // hosted engine, resumes an unhosted one, and refuses a live
                // run with an honest conflict message.
                match host.start(mission_id).await {
                    Ok(()) => {
                        if button {
                            retire_plan_card(client, response_url, mission_id, None, true).await;
                        }
                        post_thread_note(
                            cfg,
                            client,
                            threads,
                            mission_id,
                            &format!(
                                ":rocket: Execution started for `{}` — progress posts \
                             in this thread.",
                                esc(mission_id)
                            ),
                        )
                        .await;
                    }
                    Err(e) => {
                        reply_ephemeral(
                            cfg,
                            client,
                            response_url,
                            &error_blocks(&format!(
                                "Couldn't start `{}`: {}",
                                esc(mission_id),
                                esc(&e)
                            )),
                        )
                        .await;
                    }
                }
            } else {
                if let Some(host) = host {
                    match host.release(mission_id).await {
                        Ok(true) => {}
                        Ok(false) => {
                            reply_ephemeral(
                                cfg,
                                client,
                                response_url,
                                &error_blocks(&format!(
                                    "`{}` is already executing — steer it by replying \
                                 in its thread; nothing was queued.",
                                    esc(mission_id)
                                )),
                            )
                            .await;
                            return;
                        }
                        Err(e) => {
                            reply_ephemeral(
                                cfg,
                                client,
                                response_url,
                                &error_blocks(&format!(
                                    "Couldn't queue `{}`: {}",
                                    esc(mission_id),
                                    esc(&e)
                                )),
                            )
                            .await;
                            return;
                        }
                    }
                }
                // Host-free (or released): a LIVE lock holder now means an
                // external process is running the mission.
                if kranz_engine::queue::is_repo_busy(repo_root).as_deref() == Some(mission_id) {
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url,
                        &error_blocks(&format!(
                            "`{}` is already executing — steer it by replying in its \
                         thread; nothing was queued.",
                            esc(mission_id)
                        )),
                    )
                    .await;
                    return;
                }
                match approve_mission(repo_root, mission_id) {
                    Ok(()) => {
                        if button {
                            retire_plan_card(client, response_url, mission_id, None, false).await;
                        }
                        reply_ephemeral(
                            cfg,
                            client,
                            response_url,
                            &error_blocks(&format!(
                                ":white_check_mark: Queued `{}` — the `kranz work` \
                                 dispatcher runs it next.",
                                esc(mission_id)
                            )),
                        )
                        .await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to queue mission from Slack");
                        reply_ephemeral(
                            cfg,
                            client,
                            response_url,
                            &error_blocks(&format!(
                                "Couldn't queue `{}`: {}",
                                esc(mission_id),
                                esc(&e)
                            )),
                        )
                        .await;
                    }
                }
            }
        }
        Err(e) => {
            reply_ephemeral(
                cfg,
                client,
                response_url,
                &error_blocks(&format!("Couldn't read `{}`: {}", esc(mission_id), esc(&e))),
            )
            .await;
        }
    }
}

/// Refuse an approve whose card does not name the plan currently parked, in
/// the same "awaiting X, not Y" shape `enqueue_revision_control` uses for
/// the revision buttons. `None` = go ahead.
///
/// Cases, in order:
/// - nothing parked: not this check's business — the flow's own state-aware
///   routing already explains a missing plan honestly.
/// - the card names no plan: a card posted before approve buttons carried a
///   plan identity. Refuse: which plan its reviewer read is unknowable, and
///   guessing is exactly the bug.
/// - identities differ: the plan was re-requested after this card was
///   posted. Refuse and name both.
fn stale_plan_refusal(
    mission_id: &str,
    card: Option<&str>,
    parked: Option<&str>,
) -> Option<Vec<Value>> {
    let parked = parked?;
    match card {
        Some(card) if card == parked => None,
        Some(card) => Some(error_blocks(&format!(
            "That card is stale: mission `{mission}` is awaiting plan `{parked}`, not \
             `{card}`. The plan was re-requested after this card was posted, so approving \
             here would commit a plan you haven't reviewed. Scroll to the newest plan card \
             (or run `/kranz plan {mission}` again) and approve there.",
            mission = esc(mission_id),
            parked = esc(parked),
            card = esc(card)
        ))),
        None => Some(error_blocks(&format!(
            "That card predates plan-bound approve, so it cannot say which plan you \
             reviewed. Run `/kranz plan {}` and approve from the new card.",
            esc(mission_id)
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(blocks: &[Value]) -> String {
        blocks[0]["text"]["text"].as_str().unwrap().to_string()
    }

    /// M2: the load-bearing case. Card A shows plan v1; a re-plan parks v2;
    /// clicking card A must not commit v2.
    #[test]
    fn a_stale_cards_approve_is_refused_naming_both_plans() {
        let blocks = stale_plan_refusal("m-42", Some("aaaaaaaaaaaaaaaa"), Some("bbbbbbbbbbbbbbbb"))
            .expect("stale card must be refused");
        let text = text_of(&blocks);
        assert!(text.contains("stale"), "{text}");
        assert!(text.contains("awaiting plan `bbbbbbbbbbbbbbbb`"), "{text}");
        assert!(text.contains("not `aaaaaaaaaaaaaaaa`"), "{text}");
    }

    #[test]
    fn a_current_cards_approve_is_allowed_through() {
        assert!(
            stale_plan_refusal("m-42", Some("aaaaaaaaaaaaaaaa"), Some("aaaaaaaaaaaaaaaa"))
                .is_none()
        );
    }

    #[test]
    fn a_card_with_no_plan_identity_is_refused_when_a_plan_is_parked() {
        let blocks = stale_plan_refusal("m-42", None, Some("bbbbbbbbbbbbbbbb"))
            .expect("an unbound card must be refused");
        assert!(text_of(&blocks).contains("predates plan-bound approve"));
    }

    /// Nothing parked is the flow's own business (no host, forfeited by a
    /// restart, never requested); this check must not pre-empt its honest,
    /// state-aware reply.
    #[test]
    fn nothing_parked_falls_through_to_the_state_aware_routing() {
        assert!(stale_plan_refusal("m-42", Some("aaaaaaaaaaaaaaaa"), None).is_none());
        assert!(stale_plan_refusal("m-42", None, None).is_none());
    }

    /// M-12 (follow-up review): the refusal interpolates a mission id the
    /// operator typed and identities off a card, into a live mrkdwn section.
    #[test]
    fn the_stale_refusal_escapes_every_interpolation() {
        let blocks = stale_plan_refusal(
            "<!channel> m-42",
            Some("<https://evil.example/a|Approve & start>"),
            Some("bbbbbbbbbbbbbbbb"),
        )
        .expect("a mismatch must be refused");
        let text = text_of(&blocks);
        assert!(!text.contains("<!channel>"), "live broadcast ping: {text}");
        assert!(
            !text.contains("<https://evil.example/a|"),
            "live link: {text}"
        );
        assert!(text.contains("&lt;!channel&gt;"), "payload lost: {text}");
    }

    /// M-13 (follow-up review): the contract every `PlanningHost` must
    /// satisfy: a click whose card names a different plan (or names none)
    /// is refused WITHOUT approving anything, and only a matching card
    /// commits. Pinned against the trait's own default so a host that does
    /// not override it still behaves.
    mod approve_pending_if {
        use crate::host::{ApprovePendingOutcome, BoxFuture, PlanOutcome, PlanningHost};
        use kranz_engine::draft::DraftOutcome;
        use serde_json::Value;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Fake {
            parked: Option<String>,
            approves: AtomicUsize,
        }

        impl Fake {
            fn with(parked: Option<&str>) -> Self {
                Fake {
                    parked: parked.map(str::to_string),
                    approves: AtomicUsize::new(0),
                }
            }
        }

        impl PlanningHost for Fake {
            fn create<'a>(&'a self, _goal: &'a str) -> BoxFuture<'a, anyhow::Result<String>> {
                Box::pin(async { unreachable!() })
            }
            fn planning_turn<'a>(
                &'a self,
                _id: &'a str,
                _text: &'a str,
            ) -> BoxFuture<'a, anyhow::Result<String>> {
                Box::pin(async { unreachable!() })
            }
            fn request_plan<'a>(
                &'a self,
                _id: &'a str,
            ) -> BoxFuture<'a, anyhow::Result<PlanOutcome>> {
                Box::pin(async { unreachable!() })
            }
            fn approve_pending<'a>(
                &'a self,
                _id: &'a str,
            ) -> BoxFuture<'a, anyhow::Result<Option<String>>> {
                self.approves.fetch_add(1, Ordering::SeqCst);
                let parked = self.parked.clone();
                Box::pin(async move { Ok(parked.map(|_| "kranz/mission-m-42".to_string())) })
            }
            fn pending_plan_identity<'a>(
                &'a self,
                _id: &'a str,
            ) -> BoxFuture<'a, anyhow::Result<Option<String>>> {
                let parked = self.parked.clone();
                Box::pin(async move { Ok(parked) })
            }
            fn start<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
                Box::pin(async { unreachable!() })
            }
            fn release<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, anyhow::Result<bool>> {
                Box::pin(async { unreachable!() })
            }
            fn draft<'a>(&'a self, _slug: &'a str) -> BoxFuture<'a, anyhow::Result<DraftOutcome>> {
                Box::pin(async { unreachable!() })
            }
            fn approve_ticket<'a>(
                &'a self,
                _slug: &'a str,
            ) -> BoxFuture<'a, anyhow::Result<String>> {
                Box::pin(async { unreachable!() })
            }
            fn drain<'a>(&'a self) -> BoxFuture<'a, anyhow::Result<()>> {
                Box::pin(async { unreachable!() })
            }
            fn merge<'a>(&'a self, _id: &'a str) -> BoxFuture<'a, anyhow::Result<Value>> {
                Box::pin(async { unreachable!() })
            }
            fn ask<'a>(
                &'a self,
                _q: &'a str,
            ) -> BoxFuture<'a, anyhow::Result<crate::host::AskOutcome>> {
                Box::pin(async { unreachable!() })
            }
        }

        #[tokio::test]
        async fn a_stale_card_is_refused_and_nothing_is_approved() {
            let host = Fake::with(Some("bbbbbbbbbbbbbbbb"));
            let outcome = host
                .approve_pending_if("m-42", Some("aaaaaaaaaaaaaaaa"))
                .await
                .unwrap();
            assert_eq!(
                outcome,
                ApprovePendingOutcome::StalePlan {
                    parked: "bbbbbbbbbbbbbbbb".to_string()
                }
            );
            assert_eq!(
                host.approves.load(Ordering::SeqCst),
                0,
                "a refused click must not approve anything"
            );
        }

        #[tokio::test]
        async fn a_card_naming_no_plan_is_refused_while_one_is_parked() {
            let host = Fake::with(Some("bbbbbbbbbbbbbbbb"));
            assert!(matches!(
                host.approve_pending_if("m-42", None).await.unwrap(),
                ApprovePendingOutcome::StalePlan { .. }
            ));
            assert_eq!(host.approves.load(Ordering::SeqCst), 0);
        }

        #[tokio::test]
        async fn the_matching_card_commits_and_an_empty_cache_reports_nothing_parked() {
            let host = Fake::with(Some("aaaaaaaaaaaaaaaa"));
            assert_eq!(
                host.approve_pending_if("m-42", Some("aaaaaaaaaaaaaaaa"))
                    .await
                    .unwrap(),
                ApprovePendingOutcome::Approved("kranz/mission-m-42".to_string())
            );

            let empty = Fake::with(None);
            assert_eq!(
                empty
                    .approve_pending_if("m-42", Some("aaaaaaaaaaaaaaaa"))
                    .await
                    .unwrap(),
                ApprovePendingOutcome::NothingParked
            );
        }
    }
}
