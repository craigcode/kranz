//! The shared approve path (slash command + both button styles) — extracted
//! from `bridge.rs` in the monolith split (pure code motion, no behavior
//! change). See that file's module docs for the outbound/inbound runtime the
//! dispatch side of this flow hangs off.

use crate::bridge::{
    approve_mission, error_blocks, mission_status, no_host_blocks, not_authorized_blocks_for,
    post_thread_note, reply_ephemeral, retire_plan_card, SharedThreads,
};
use crate::client::SlackClient;
use crate::config::SlackConfig;
use crate::host::SharedHost;
use kranz_engine::types::MissionStatus;
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
    user_id: Option<&str>,
    response_url: Option<&str>,
    start: bool,
    button: bool,
) {
    if !cfg.is_authorized(user_id) {
        reply_ephemeral(cfg, client, response_url, &not_authorized_blocks_for(cfg)).await;
        return;
    }

    // `approve_pending` consumes the host-parked plan. `None` = no host, or
    // nothing parked — fall through to the state-aware routing below. On a
    // transient failure (e.g. a turn in flight) the host re-parked the plan,
    // so the retry click finds it again.
    let approved = match host {
        None => None,
        Some(host) => match host.approve_pending(mission_id).await {
            Ok(branch) => branch.map(|branch| (branch, host)),
            Err(e) => {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url,
                    &error_blocks(&format!("Couldn't approve `{mission_id}`: {e}")),
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
                            ":rocket: Plan approved and execution started (branch `{branch}`) — \
                         progress posts in this thread; deep inspection in the web UI."
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
                            "Approved `{mission_id}` (branch `{branch}`) but starting failed: \
                             {e}. Queue it with `/kranz approve {mission_id}` or run \
                             `kranz work`."
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
                            ":white_check_mark: Plan approved and queued (branch `{branch}`) — \
                         the `kranz work` dispatcher runs it next."
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
                            "Approved `{mission_id}` (branch `{branch}`) but queueing failed: {e}"
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
                    "No reviewed plan is pending for `{mission_id}` — run \
                     `/kranz plan {mission_id}` first, then approve from the plan message."
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
                    "Mission `{mission_id}` is {status:?} — nothing to approve or queue."
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
                                ":rocket: Execution started for `{mission_id}` — progress posts \
                             in this thread."
                            ),
                        )
                        .await;
                    }
                    Err(e) => {
                        reply_ephemeral(
                            cfg,
                            client,
                            response_url,
                            &error_blocks(&format!("Couldn't start `{mission_id}`: {e}")),
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
                                    "`{mission_id}` is already executing — steer it by replying \
                                 in its thread; nothing was queued."
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
                                &error_blocks(&format!("Couldn't queue `{mission_id}`: {e}")),
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
                            "`{mission_id}` is already executing — steer it by replying in its \
                         thread; nothing was queued."
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
                                ":white_check_mark: Queued `{mission_id}` — the `kranz work` \
                                 dispatcher runs it next."
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
                            &error_blocks(&format!("Couldn't queue `{mission_id}`: {e}")),
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
                &error_blocks(&format!("Couldn't read `{mission_id}`: {e}")),
            )
            .await;
        }
    }
}
