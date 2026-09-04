//! The inbound action router — extracted from `bridge.rs` in the monolith
//! split (pure code motion, no behavior change). See that file's module docs
//! for the outbound/inbound runtime this dispatch path hangs off.

use crate::approve_flow::approve_flow;
use crate::bridge::{
    apply_action, ask_answer_blocks, build_home_view, build_outcomes_reply,
    build_pipeline_status_reply, build_roadmap_reply, build_status_reply, build_ticket_list_reply,
    build_ticket_show_reply, build_todo_reply, build_work_reply, change_config, create_ticket,
    error_blocks, esc, grant_control, guidance, is_ticket_slug, looks_like_mission_id,
    looks_like_plan_json, mission_dir_exists, mission_status, new_mission, no_host_blocks,
    not_authorized_blocks_for, not_authorized_text_for, post_thread_note, post_to_mission_thread,
    question_control, reply_ephemeral, revision_control, scaffold_ticket, slugify, steer,
    user_reply, ModalScope, SharedThreads,
};
use crate::client::SlackClient;
use crate::commands::{
    gate_draft_command, gate_merge_command, gate_work_run_command, run_approve_ticket_command,
    run_draft, run_merge, run_work_run, DraftGate, MergeGate, WorkRunGate,
};
use crate::config::SlackConfig;
use crate::host::{PlanOutcome, SharedHost};
use crate::inbound::Action;
use kranz_engine::types::{ControlCommand, MissionStatus};
use serde_json::json;
use std::path::Path;

pub(crate) async fn dispatch_action(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    host: Option<&SharedHost>,
    modal_scope: Option<&ModalScope>,
    action: &Action,
) {
    match action {
        Action::Help { response_url } => {
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &crate::format::build_help(),
            )
            .await;
        }

        Action::Status {
            mission_id,
            response_url,
        } => {
            if mission_id.is_none() {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &build_pipeline_status_reply(repo_root),
                )
                .await;
            } else {
                match build_status_reply(repo_root, mission_id.as_deref()) {
                    Ok(blocks) => {
                        reply_ephemeral(cfg, client, response_url.as_deref(), &blocks).await
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to build Slack status reply");
                        reply_ephemeral(
                            cfg,
                            client,
                            response_url.as_deref(),
                            &error_blocks(&format!("Couldn't read that mission: {}", esc(&e))),
                        )
                        .await;
                    }
                }
            }
        }

        Action::Todo { response_url } => {
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &build_todo_reply(repo_root, cfg.dashboard_url.as_deref()),
            )
            .await;
        }

        Action::Roadmap { response_url } => {
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &build_roadmap_reply(repo_root),
            )
            .await;
        }

        Action::Outcomes { response_url } => {
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &build_outcomes_reply(repo_root),
            )
            .await;
        }

        Action::Ask {
            question,
            user_id,
            response_url,
            channel,
            thread_ts,
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks_for(cfg),
                )
                .await;
                return;
            }
            let Some(host) = host else {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &error_blocks("No hosted engine is attached, so `/kranz ask` cannot run here."),
                )
                .await;
                return;
            };
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &error_blocks(":hourglass_flowing_sand: Asking Kranz — the grounded answer will post back here."),
            )
            .await;
            match host.ask(question).await {
                Ok(outcome) => {
                    let blocks = ask_answer_blocks(question, &outcome);
                    let blocks = crate::format::label_blocks(blocks, cfg.instance_name.as_deref());
                    if channel.trim().is_empty() {
                        if let Some(url) = response_url.as_deref() {
                            if let Err(e) = client.post_response(url, &blocks, false).await {
                                tracing::warn!(error = %e, "failed to post Slack ask answer");
                            }
                        }
                    } else if let Err(e) = client
                        .post_message(channel, &blocks, thread_ts.as_deref())
                        .await
                    {
                        tracing::warn!(error = %e, "failed to post Slack ask answer");
                    }
                }
                Err(e) => {
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url.as_deref(),
                        &error_blocks(&format!("Couldn't answer that yet: {}", esc(&e))),
                    )
                    .await;
                }
            }
        }

        // Read-only backlog verbs (no `user_id`, so structurally not
        // allowlist-gated — same shape as Status).
        Action::TicketList { response_url } => {
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &build_ticket_list_reply(repo_root),
            )
            .await;
        }

        Action::TicketShow { slug, response_url } => {
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &build_ticket_show_reply(repo_root, slug),
            )
            .await;
        }

        Action::NewMission {
            goal,
            user_id,
            response_url,
            channel,
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                user_reply(
                    cfg,
                    client,
                    response_url.as_deref(),
                    channel,
                    user_id.as_deref(),
                    &not_authorized_blocks_for(cfg),
                )
                .await;
                return;
            }
            // Ack IMMEDIATELY: create + the seeding planning turn take minutes,
            // and a silently-working command reads as a dead one.
            user_reply(
                cfg,
                client,
                response_url.as_deref(),
                channel,
                user_id.as_deref(),
                &error_blocks(
                    ":hourglass_flowing_sand: Creating the mission — the seeding planning \
                     turn usually takes a minute or two; the planning thread will appear \
                     in the channel.",
                ),
            )
            .await;
            match new_mission(cfg, client, repo_root, threads, host, goal, channel).await {
                Ok(blocks) => {
                    user_reply(
                        cfg,
                        client,
                        response_url.as_deref(),
                        channel,
                        user_id.as_deref(),
                        &blocks,
                    )
                    .await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to create mission from Slack");
                    user_reply(
                        cfg,
                        client,
                        response_url.as_deref(),
                        channel,
                        user_id.as_deref(),
                        &error_blocks(&format!("Couldn't create the mission: {}", esc(&e))),
                    )
                    .await;
                }
            }
        }

        // Bare `/kranz config`: open the role/backend/model/effort picker modal.
        // Inline for the same trigger_id-expiry reason as the goal modal.
        Action::ConfigModal {
            trigger_id,
            user_id,
            response_url,
            channel,
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks_for(cfg),
                )
                .await;
                return;
            }
            let view = match modal_scope {
                Some(scope) => crate::format::build_config_modal_scoped(
                    channel,
                    &scope.repo_id,
                    &scope.team_id,
                ),
                None => crate::format::build_config_modal(channel),
            };
            if let Err(e) = client.open_view(trigger_id, &view).await {
                tracing::warn!(error = %e, "failed to open config modal");
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &error_blocks(&format!(
                        "Couldn't open the config form: {}. One-line fallback: \
                         `/kranz config [<id>] <role> [backend] <model> [effort]`.",
                        esc(&e)
                    )),
                )
                .await;
            }
        }

        // Bare `/kranz new`: open the multiline goal modal. MUST run inline —
        // the trigger_id expires ~3 s after the slash — and it's one Web API
        // call, well inside the ack budget.
        Action::NewMissionModal {
            trigger_id,
            user_id,
            response_url,
            channel,
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks_for(cfg),
                )
                .await;
                return;
            }
            let view = match modal_scope {
                Some(scope) => crate::format::build_new_mission_modal_scoped(
                    channel,
                    &scope.repo_id,
                    &scope.team_id,
                ),
                None => crate::format::build_new_mission_modal(channel),
            };
            if let Err(e) = client.open_view(trigger_id, &view).await {
                tracing::warn!(error = %e, "failed to open new-mission modal");
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &error_blocks(&format!(
                        "Couldn't open the new-mission form: {}. One-line fallback: \
                         `/kranz new <goal>`.",
                        esc(&e)
                    )),
                )
                .await;
            }
        }

        // `/kranz ticket new <slug> <title...>`: open the multiline
        // goal/context modal. MUST run inline — the trigger_id expires ~3s
        // after the slash — mirroring [`Action::NewMissionModal`].
        Action::NewTicketModal {
            trigger_id,
            slug,
            title,
            user_id,
            response_url,
            channel,
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks_for(cfg),
                )
                .await;
                return;
            }
            let view = match modal_scope {
                Some(scope) => crate::format::build_new_ticket_modal_scoped(
                    slug,
                    title,
                    channel,
                    &scope.repo_id,
                    &scope.team_id,
                ),
                None => crate::format::build_new_ticket_modal(slug, title, channel),
            };
            if let Err(e) = client.open_view(trigger_id, &view).await {
                tracing::warn!(error = %e, "failed to open new-ticket modal");
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &error_blocks(&format!(
                        "Couldn't open the new-ticket form: {}. One-line fallback: \
                         `/kranz ticket <title>`.",
                        esc(&e)
                    )),
                )
                .await;
            }
        }

        // The new-ticket modal's `view_submission`: scaffold the ticket
        // through the same primitive `POST /api/tickets` uses
        // (`Ticket::scaffold`). No `response_url` (a modal submission has
        // none), so the confirmation/error posts straight into `channel`.
        // Allowlist-gated like NewTicketModal — an unlisted user must not
        // create backlog tickets when `allowUsers` is non-empty.
        Action::CreateTicket {
            slug,
            title,
            goal,
            context,
            channel,
            user_id,
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                user_reply(
                    cfg,
                    client,
                    None,
                    channel,
                    user_id.as_deref(),
                    &not_authorized_blocks_for(cfg),
                )
                .await;
                return;
            }
            let goal = (!goal.trim().is_empty()).then_some(goal.as_str());
            let context = (!context.trim().is_empty()).then_some(context.as_str());
            match create_ticket(repo_root, slug, title, goal, context) {
                Ok(()) => {
                    let blocks = vec![json!({
                        "type": "section",
                        "text": {
                            "type": "mrkdwn",
                            "text": format!(
                                ":ticket: Created ticket `{}` — {}",
                                crate::format::escape_mrkdwn(slug),
                                crate::format::escape_mrkdwn(title)
                            )
                        }
                    })];
                    if let Err(e) = client.post_message(channel, &blocks, None).await {
                        tracing::warn!(error = %e, "failed to post ticket-created confirmation");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to create ticket from Slack modal");
                    if let Err(e) = client
                        .post_message(
                            channel,
                            &error_blocks(&format!(
                                "Couldn't create ticket `{}`: {}",
                                esc(slug),
                                esc(&e)
                            )),
                            None,
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "failed to post ticket-creation error");
                    }
                }
            }
        }

        // `/kranz ticket <title>`: scaffold a one-line ticket. Allowlist-gated
        // like CreateTicket / NewTicketModal — writes backlog state.
        Action::NewTicket {
            title,
            channel,
            user_id,
            response_url,
            ..
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                user_reply(
                    cfg,
                    client,
                    response_url.as_deref(),
                    channel,
                    user_id.as_deref(),
                    &not_authorized_blocks_for(cfg),
                )
                .await;
                return;
            }
            match scaffold_ticket(repo_root, title) {
                Ok(()) => {
                    let slug = slugify(title);
                    let blocks = vec![json!({
                        "type": "section",
                        "text": {
                            "type": "mrkdwn",
                            "text": format!(
                                ":ticket: Scaffolded ticket `{}` — {}",
                                crate::format::escape_mrkdwn(&slug),
                                crate::format::escape_mrkdwn(title)
                            )
                        }
                    })];
                    user_reply(
                        cfg,
                        client,
                        response_url.as_deref(),
                        channel,
                        user_id.as_deref(),
                        &blocks,
                    )
                    .await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to scaffold ticket from Slack");
                    user_reply(
                        cfg,
                        client,
                        response_url.as_deref(),
                        channel,
                        user_id.as_deref(),
                        &error_blocks(&format!("Couldn't scaffold ticket: {}", esc(&e))),
                    )
                    .await;
                }
            }
        }

        Action::RequestPlan {
            mission_id,
            user_id,
            response_url,
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks_for(cfg),
                )
                .await;
                return;
            }
            let Some(host) = host else {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &no_host_blocks(mission_id),
                )
                .await;
                return;
            };
            // Ack IMMEDIATELY: the request-plan turn takes minutes, and a
            // silently-working command is exactly the confusion this surface
            // is meant to avoid.
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &error_blocks(&format!(
                    ":hourglass_flowing_sand: Requesting the plan for `{}` — the \
                     orchestrator turn usually takes a minute or two; the plan will post \
                     in the mission thread.",
                    esc(mission_id)
                )),
            )
            .await;
            match host.request_plan(mission_id).await {
                Ok(PlanOutcome::Ready { plan, estimate }) => {
                    let review = crate::format::build_plan_review(&crate::format::PlanReview {
                        mission_id: mission_id.clone(),
                        goal: plan.goal.clone(),
                        milestone_titles: plan.milestones.iter().map(|m| m.title.clone()).collect(),
                        assertion_count: plan.validation_contract.len(),
                        // Bind the approve buttons to THIS plan (M2): a
                        // re-plan posts a new card, and the old card's
                        // click is refused rather than committing the new
                        // plan under the old plan's review.
                        plan_identity: crate::format::plan_identity(&plan),
                        considered_alternatives: plan.considered_alternatives.as_ref().map(|a| {
                            crate::format::PlanAlternativesReview {
                                chosen: a.chosen.clone(),
                                rejected: a
                                    .rejected
                                    .iter()
                                    .map(|r| crate::format::RejectedAlternativeReview {
                                        approach: r.approach.clone(),
                                        trade_off: r.trade_off.clone(),
                                    })
                                    .collect(),
                            }
                        }),
                        estimate,
                    });
                    // No caching here: the host parked the reviewed plan
                    // when request_plan returned Ready, so the buttons (and
                    // any other surface's approve) consume it host-side.
                    if let Err(e) =
                        post_to_mission_thread(cfg, client, threads, mission_id, review).await
                    {
                        tracing::warn!(mission = %mission_id, error = %e, "failed to post plan review");
                        reply_ephemeral(
                            cfg,
                            client,
                            response_url.as_deref(),
                            &error_blocks(&format!(
                                "The plan is ready but posting it failed: {}. \
                                 Run `/kranz plan {}` again.",
                                esc(&e),
                                esc(mission_id)
                            )),
                        )
                        .await;
                    }
                }
                Ok(PlanOutcome::NotReady(prose)) => {
                    // NotReady by definition: never claim a plan was spotted.
                    let blocks = crate::format::build_planning_reply(mission_id, &prose, false);
                    if let Err(e) =
                        post_to_mission_thread(cfg, client, threads, mission_id, blocks).await
                    {
                        tracing::warn!(mission = %mission_id, error = %e, "failed to post not-ready reply");
                    }
                }
                Err(e) => {
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url.as_deref(),
                        &error_blocks(&format!("Couldn't request the plan: {}", esc(&e))),
                    )
                    .await;
                }
            }
        }

        Action::ApproveMission {
            mission_id,
            user_id,
            response_url,
        } => {
            // `/kranz approve <arg>` accepts EITHER a mission id or a ticket
            // slug. Ambiguity rule: if `arg` looks like `m-[0-9a-f]{6}` AND
            // that mission directory exists on disk, prefer mission plan
            // approval; otherwise, if it names an on-disk backlog ticket,
            // queue the ticket. (A hex-shaped mission id that also happens
            // to be a ticket slug must not silently queue the ticket.)
            if looks_like_mission_id(mission_id) && mission_dir_exists(repo_root, mission_id) {
                approve_flow(
                    cfg,
                    client,
                    repo_root,
                    threads,
                    host,
                    mission_id,
                    // The slash twin reviews no card, so it claims no plan.
                    None,
                    user_id.as_deref(),
                    response_url.as_deref(),
                    false,
                    false,
                )
                .await;
            } else if is_ticket_slug(repo_root, mission_id) {
                let invocation =
                    run_approve_ticket_command(cfg, host, mission_id, user_id.as_deref()).await;
                if !invocation.authorized {
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url.as_deref(),
                        &not_authorized_blocks_for(cfg),
                    )
                    .await;
                } else if let Some(result) = &invocation.result {
                    reply_ephemeral(cfg, client, response_url.as_deref(), result).await;
                }
            } else {
                approve_flow(
                    cfg,
                    client,
                    repo_root,
                    threads,
                    host,
                    mission_id,
                    // The slash twin reviews no card, so it claims no plan.
                    None,
                    user_id.as_deref(),
                    response_url.as_deref(),
                    false,
                    false,
                )
                .await;
            }
        }

        // `/kranz queue <slug>` — the D-A ticket-queueing verb. MUST NEVER
        // fall back to `approve_flow` (plan approval stays "approve"
        // wholesale): a non-ticket arg is refused with a pointer at
        // `/kranz approve <id>` instead of being interpreted as a mission id.
        Action::QueueTicket {
            slug,
            user_id,
            response_url,
        } => {
            if !is_ticket_slug(repo_root, slug) {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &error_blocks(&format!(
                        "`{}` isn't a backlog ticket — `queue` is for tickets. \
                         To approve a mission plan, use `/kranz approve <mission-id>`.",
                        esc(slug)
                    )),
                )
                .await;
                return;
            }
            let invocation = run_approve_ticket_command(cfg, host, slug, user_id.as_deref()).await;
            if !invocation.authorized {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks_for(cfg),
                )
                .await;
            } else if let Some(result) = &invocation.result {
                reply_ephemeral(cfg, client, response_url.as_deref(), result).await;
            }
        }

        // `/kranz draft <slug>` — SPEND action, gated EXACTLY like `new`
        // ([`gate_draft_command`] holds the gate + ack logic so it's
        // unit-testable without a live SlackClient). The hourglass ack MUST
        // post before the slow `run_draft` await, mirroring
        // NewMission/RequestPlan — never build both and post them back to
        // back after the draft finishes.
        Action::Draft {
            slug,
            user_id,
            response_url,
        } => match gate_draft_command(cfg, host, slug, user_id.as_deref()) {
            DraftGate::Unauthorized => {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks_for(cfg),
                )
                .await;
            }
            DraftGate::NoHost(blocks) | DraftGate::InvalidSlug(blocks) => {
                reply_ephemeral(cfg, client, response_url.as_deref(), &blocks).await;
            }
            DraftGate::Ready(ack) => {
                reply_ephemeral(cfg, client, response_url.as_deref(), &ack).await;
                let host = host.expect("DraftGate::Ready only returned with a host present");
                let result = run_draft(host, slug).await;
                reply_ephemeral(cfg, client, response_url.as_deref(), &result).await;
            }
        },

        // Per-role config change. SPEND-ADJACENT (it re-shapes future turns'
        // spend), so it is gated on the allowlist exactly like `/kranz new`.
        Action::Config {
            mission_id,
            role,
            backend,
            model,
            effort,
            user_id,
            response_url,
            channel,
        } => {
            change_config(
                cfg,
                client,
                repo_root,
                mission_id.as_deref(),
                role,
                backend.as_deref(),
                model,
                effort.as_deref(),
                user_id.as_deref(),
                response_url.as_deref(),
                channel.as_deref(),
            )
            .await;
        }

        // Pause / resume: STEERING (not spend), but they disrupt a running
        // mission, so they are gated on the allowlist exactly like `config`. On
        // authorization, enqueue the Pause/Resume control command on the target
        // mission (resolved via the same active-mission resolver config uses, so
        // a terminal/ambiguous target is an honest error that enqueues NOTHING).
        // A pure local write, so it stays inline (fast ack).
        Action::Pause {
            mission_id,
            user_id,
            response_url,
        } => {
            steer(
                cfg,
                client,
                repo_root,
                mission_id.as_deref(),
                user_id.as_deref(),
                response_url.as_deref(),
                ControlCommand::Pause,
                "paused",
            )
            .await;
        }
        Action::Resume {
            mission_id,
            user_id,
            response_url,
        } => {
            steer(
                cfg,
                client,
                repo_root,
                mission_id.as_deref(),
                user_id.as_deref(),
                response_url.as_deref(),
                ControlCommand::Resume,
                "resumed",
            )
            .await;
        }
        Action::Revise {
            mission_id,
            instructions,
            user_id,
            response_url,
        } => {
            revision_control(
                cfg,
                client,
                repo_root,
                mission_id,
                user_id.as_deref(),
                response_url.as_deref(),
                ControlCommand::RequestRevision {
                    instructions: instructions.clone(),
                },
                "revision request queued",
            )
            .await;
        }
        Action::ApproveRevision {
            mission_id,
            revision,
            user_id,
            response_url,
        } => {
            revision_control(
                cfg,
                client,
                repo_root,
                mission_id,
                user_id.as_deref(),
                response_url.as_deref(),
                ControlCommand::ApproveRevision {
                    revision: *revision,
                },
                "revision approval queued",
            )
            .await;
        }
        Action::RejectRevision {
            mission_id,
            revision,
            user_id,
            response_url,
        } => {
            revision_control(
                cfg,
                client,
                repo_root,
                mission_id,
                user_id.as_deref(),
                response_url.as_deref(),
                ControlCommand::RejectRevision {
                    revision: *revision,
                },
                "revision rejection queued",
            )
            .await;
        }
        Action::ApproveGrant {
            mission_id,
            command,
            user_id,
            response_url,
        } => {
            grant_control(
                cfg,
                client,
                repo_root,
                mission_id,
                user_id.as_deref(),
                response_url.as_deref(),
                ControlCommand::ApproveGrant {
                    command: command.clone(),
                },
                "grant approval queued",
            )
            .await;
        }
        Action::DenyGrant {
            mission_id,
            command,
            user_id,
            response_url,
        } => {
            grant_control(
                cfg,
                client,
                repo_root,
                mission_id,
                user_id.as_deref(),
                response_url.as_deref(),
                ControlCommand::DenyGrant {
                    command: command.clone(),
                    reason: "denied from Slack".to_string(),
                },
                "grant denial queued",
            )
            .await;
        }
        Action::AnswerQuestion {
            mission_id,
            question_id,
            option,
            user_id,
            response_url,
        } => {
            question_control(
                cfg,
                client,
                repo_root,
                mission_id,
                question_id,
                *option,
                user_id.as_deref(),
                response_url.as_deref(),
            )
            .await;
        }

        // Queue report. READ-ONLY and REPORT-ONLY: the bridge never drains the
        // queue on the socket loop (that would spawn `claude`); it reads the
        // queue state and points at the `kranz work` dispatcher.
        Action::Work { response_url } => {
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &build_work_reply(repo_root),
            )
            .await;
        }

        // `/kranz work run` — SPEND action, gated EXACTLY like `draft`
        // ([`gate_work_run_command`] holds the gate + ack logic so it's
        // unit-testable without a live SlackClient). The ack MUST post before
        // the slow `run_work_run` await, mirroring Draft — never build both
        // and post them back to back after the drain call returns.
        Action::WorkRun {
            user_id,
            response_url,
        } => match gate_work_run_command(cfg, host, user_id.as_deref()) {
            WorkRunGate::Unauthorized => {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks_for(cfg),
                )
                .await;
            }
            WorkRunGate::NoHost(blocks) => {
                reply_ephemeral(cfg, client, response_url.as_deref(), &blocks).await;
            }
            WorkRunGate::Ready(ack) => {
                reply_ephemeral(cfg, client, response_url.as_deref(), &ack).await;
                let host = host.expect("WorkRunGate::Ready only returned with a host present");
                let result = run_work_run(host).await;
                reply_ephemeral(cfg, client, response_url.as_deref(), &result).await;
            }
        },

        // `/kranz merge <slug|id>` / Delivered-card Merge button — gated
        // EXACTLY like `work run` ([`gate_merge_command`] holds the gate + ack
        // logic so it's unit-testable without a live SlackClient). The ack
        // MUST post before the slow `run_merge` await, mirroring WorkRun.
        Action::Merge {
            mission_id,
            user_id,
            response_url,
        } => match gate_merge_command(cfg, host, user_id.as_deref()) {
            MergeGate::Unauthorized => {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks_for(cfg),
                )
                .await;
            }
            MergeGate::NoHost(blocks) => {
                reply_ephemeral(cfg, client, response_url.as_deref(), &blocks).await;
            }
            MergeGate::Ready(ack) => {
                reply_ephemeral(cfg, client, response_url.as_deref(), &ack).await;
                let host = host.expect("MergeGate::Ready only returned with a host present");
                let result = run_merge(host, mission_id).await;
                reply_ephemeral(cfg, client, response_url.as_deref(), &result).await;
            }
        },

        // App Home tab: fold the repo read-only and publish this user's home
        // view. Read-only (no allowlist gate); a publish failure is logged, not
        // surfaced (there's no response_url — the user just opened a tab).
        Action::AppHome { user_id } => {
            let view = crate::format::label_home_view(
                build_home_view(repo_root, cfg.dashboard_url.as_deref()),
                cfg.instance_name.as_deref(),
            );
            if let Err(e) = client.publish_home_view(user_id, &view).await {
                tracing::warn!(user = %user_id, error = %e, "failed to publish App Home view");
            }
        }

        // The approve BUTTONS are the spend twins of `/kranz approve` and must
        // be gated identically — otherwise an unlisted user clicking one queues
        // (or starts) a paid mission, bypassing the allowlist that the slash
        // command enforces.
        Action::Approve {
            mission_id,
            plan_identity,
            user_id,
            response_url,
        } => {
            approve_flow(
                cfg,
                client,
                repo_root,
                threads,
                host,
                mission_id,
                plan_identity.as_deref(),
                user_id.as_deref(),
                response_url.as_deref(),
                false,
                true,
            )
            .await;
        }
        Action::ApproveStart {
            mission_id,
            plan_identity,
            user_id,
            response_url,
        } => {
            approve_flow(
                cfg,
                client,
                repo_root,
                threads,
                host,
                mission_id,
                plan_identity.as_deref(),
                user_id.as_deref(),
                response_url.as_deref(),
                true,
                true,
            )
            .await;
        }

        // A threaded reply: on a PLANNING mission this is a hosted planning
        // turn (spend → allowlist-gated, acked in-thread because a message
        // event has no response_url); on anything else it stays the running-
        // mission guidance write it has always been.
        Action::Guidance {
            mission_id,
            text,
            user_id,
        } => {
            match mission_status(repo_root, mission_id) {
                Ok(MissionStatus::Planning) => {
                    let Some(host) = host else {
                        post_thread_note(
                            cfg,
                            client,
                            threads,
                            mission_id,
                            &format!(
                                "This mission is still in planning, and this bridge has no hosted \
                             engine (it was started without `kranz serve`). Continue with \
                             `kranz plan --mission {mission_id}` in a terminal."
                            ),
                        )
                        .await;
                        return;
                    };
                    if !cfg.is_authorized(user_id.as_deref()) {
                        post_thread_note(
                            cfg,
                            client,
                            threads,
                            mission_id,
                            &format!(
                                "Planning turns spend money. {}",
                                not_authorized_text_for(cfg)
                            ),
                        )
                        .await;
                        return;
                    }
                    post_thread_note(
                        cfg,
                        client,
                        threads,
                        mission_id,
                        ":hourglass_flowing_sand: Planning turn running — the orchestrator's \
                         reply lands here, usually within a couple of minutes.",
                    )
                    .await;
                    match host.planning_turn(mission_id, text).await {
                        Ok(reply) => {
                            let blocks = crate::format::build_planning_reply(
                                mission_id,
                                &reply,
                                looks_like_plan_json(&reply),
                            );
                            if let Err(e) =
                                post_to_mission_thread(cfg, client, threads, mission_id, blocks)
                                    .await
                            {
                                tracing::warn!(mission = %mission_id, error = %e, "failed to post planning reply");
                            }
                        }
                        Err(e) => {
                            post_thread_note(
                                cfg,
                                client,
                                threads,
                                mission_id,
                                &format!("Planning turn failed: {e}"),
                            )
                            .await;
                        }
                    }
                }
                // Running / paused / blocked (and, unchanged from before,
                // terminal): the control-inbox guidance write. Steering a live
                // mission is spend-adjacent — strictly more powerful than the
                // allowlist-gated pause/resume — so gate it on the same
                // allowlist as planning and pause/resume.
                Ok(_) => {
                    if !cfg.is_authorized(user_id.as_deref()) {
                        post_thread_note(
                            cfg,
                            client,
                            threads,
                            mission_id,
                            &format!(
                                "Steering a running mission spends money. {}",
                                not_authorized_text_for(cfg)
                            ),
                        )
                        .await;
                        return;
                    }
                    if let Err(e) = guidance(repo_root, mission_id, text) {
                        tracing::warn!(error = %e, "failed to enqueue Slack guidance");
                    }
                }
                Err(e) => {
                    tracing::warn!(mission = %mission_id, error = %e, "failed to read mission status for thread reply");
                }
            }
        }

        // Ticket scaffolding: a pure local write, no reply.
        action => {
            if let Err(e) = apply_action(repo_root, action) {
                tracing::warn!(error = %e, "failed to apply inbound Slack action");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NotifyFlags;
    use kranz_engine::paths::MissionPaths;
    use kranz_engine::queue;
    use tempfile::TempDir;

    fn test_cfg() -> SlackConfig {
        SlackConfig {
            bot_token: "xoxb".into(),
            app_token: "xapp".into(),
            channel: "C1".into(),
            notify: NotifyFlags::default(),
            allow_users: vec![],
            // The tests using this cfg exercise ungated behavior, so they
            // deliberately acknowledge the open posture.
            allow_all_users: true,
            dashboard_url: None,
            instance_name: None,
        }
    }

    fn seed_mission(repo_root: &Path, mission_id: &str, goal: &str) {
        use kranz_engine::events::{Event, EventKind};
        use kranz_engine::types::MissionConfig;
        let paths = MissionPaths::new(repo_root, mission_id);
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        let event = Event {
            seq: 1,
            ts: chrono::Utc::now(),
            mission_id: mission_id.to_string(),
            kind: EventKind::MissionCreated {
                goal: goal.to_string(),
                base_branch: "main".into(),
                mission_branch: format!("kranz/mission-{mission_id}"),
                config: MissionConfig::default(),
            },
        };
        let line = serde_json::to_string(&event).unwrap();
        std::fs::write(paths.events_file(), format!("{line}\n")).unwrap();
    }

    #[test]
    fn new_ticket_action_scaffolds_file() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_cfg();
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        // NewTicket is gated + scaffolded in dispatch_action (not apply_action).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(dispatch_action(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            None,
            None,
            &Action::NewTicket {
                title: "Rate-limit the notes API".into(),
                channel: "C1".into(),
                thread_ts: None,
                user_id: None,
                response_url: None,
            },
        ));
        let path = kranz_engine::ticket::Ticket::tickets_dir(tmp.path())
            .join("rate-limit-the-notes-api.md");
        assert!(path.exists());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("title: Rate-limit the notes API"));
        assert!(text.contains("## Goal"));
    }

    #[tokio::test]
    async fn create_ticket_denies_unlisted_user_when_allowlist_set() {
        let tmp = TempDir::new().unwrap();
        let cfg = SlackConfig {
            bot_token: "xoxb".into(),
            app_token: "xapp".into(),
            channel: "C1".into(),
            notify: NotifyFlags::default(),
            allow_users: vec!["U-allowed".into()],
            allow_all_users: false,
            dashboard_url: None,
            instance_name: None,
        };
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        dispatch_action(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            None,
            None,
            &Action::CreateTicket {
                slug: "secret-ticket".into(),
                title: "Secret".into(),
                goal: "do it".into(),
                context: String::new(),
                channel: "C1".into(),
                user_id: Some("U-outsider".into()),
            },
        )
        .await;
        let path = kranz_engine::ticket::Ticket::tickets_dir(tmp.path()).join("secret-ticket.md");
        assert!(
            !path.exists(),
            "unlisted user must not create a ticket when allowUsers is non-empty"
        );
    }

    #[tokio::test]
    async fn new_ticket_denies_unlisted_user_when_allowlist_set() {
        let tmp = TempDir::new().unwrap();
        let cfg = SlackConfig {
            bot_token: "xoxb".into(),
            app_token: "xapp".into(),
            channel: "C1".into(),
            notify: NotifyFlags::default(),
            allow_users: vec!["U-allowed".into()],
            allow_all_users: false,
            dashboard_url: None,
            instance_name: None,
        };
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        dispatch_action(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            None,
            None,
            &Action::NewTicket {
                title: "Should Not Exist".into(),
                channel: "C1".into(),
                thread_ts: None,
                user_id: Some("U-outsider".into()),
                response_url: None,
            },
        )
        .await;
        let path =
            kranz_engine::ticket::Ticket::tickets_dir(tmp.path()).join("should-not-exist.md");
        assert!(
            !path.exists(),
            "unlisted user must not scaffold a ticket when allowUsers is non-empty"
        );
    }

    /// D-A regression: `Action::QueueTicket` with a mission-id-shaped arg
    /// must be refused (queue is for tickets), and — critically — must NEVER
    /// fall through to `approve_flow`. Prove it by seeding a mission in the
    /// exact state `approve_flow` would happily queue (`Approved`, no host)
    /// and asserting the queue stays empty after dispatching `QueueTicket`.
    #[tokio::test]
    async fn queue_ticket_action_never_invokes_approve_flow_for_a_non_ticket_arg() {
        use kranz_engine::events::{Event, EventKind};
        use kranz_engine::types::{Plan, PlanFeature, PlanMilestone};

        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-a", "approved mission");
        let paths = MissionPaths::new(tmp.path(), "m-a");
        let event = Event {
            seq: 2,
            ts: chrono::Utc::now(),
            mission_id: "m-a".to_string(),
            kind: EventKind::PlanApproved {
                plan: Plan {
                    goal: "approved mission".into(),
                    validation_contract: vec![],
                    milestones: vec![PlanMilestone {
                        title: "M1".into(),
                        features: vec![PlanFeature {
                            title: "F1".into(),
                            spec: "s".into(),
                            validation_criteria: vec!["c".into()],
                        }],
                    }],
                    considered_alternatives: None,
                    command_grants: vec![],
                    touch_set: vec![],
                    standards_manifest: None,
                },
                base_sha: None,
            },
        };
        let line = serde_json::to_string(&event).unwrap();
        let mut existing = std::fs::read_to_string(paths.events_file()).unwrap();
        existing.push_str(&line);
        existing.push('\n');
        std::fs::write(paths.events_file(), existing).unwrap();
        assert_eq!(
            mission_status(tmp.path(), "m-a").unwrap(),
            MissionStatus::Approved,
            "mission is exactly the state approve_flow would queue with no host"
        );
        assert!(
            !is_ticket_slug(tmp.path(), "m-a"),
            "m-a is not a ticket slug"
        );

        let cfg = test_cfg();
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        dispatch_action(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            None,
            None,
            &Action::QueueTicket {
                slug: "m-a".into(),
                user_id: None,
                response_url: None,
            },
        )
        .await;

        assert!(
            queue::list(tmp.path()).is_empty(),
            "QueueTicket with a non-ticket arg must never queue the mission via approve_flow"
        );
        assert_eq!(
            mission_status(tmp.path(), "m-a").unwrap(),
            MissionStatus::Approved,
            "mission state must be untouched by the refused queue attempt"
        );
    }

    #[tokio::test]
    async fn config_action_denies_unlisted_user_at_the_dispatch_arm() {
        // The test above pins the extracted `change_config` fn; this one
        // drives the real `dispatch_action` Action::Config arm end-to-end, so
        // re-inlining ungated config logic in the dispatch arm (bypassing
        // `change_config`) cannot pass the suite.
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-gated-dispatch", "goal");
        let cfg = SlackConfig {
            bot_token: "xoxb".into(),
            app_token: "xapp".into(),
            channel: "C1".into(),
            notify: NotifyFlags::default(),
            allow_users: vec!["U-allowed".into()],
            allow_all_users: false,
            dashboard_url: None,
            instance_name: None,
        };
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();

        dispatch_action(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            None,
            None,
            &Action::Config {
                mission_id: Some("m-gated-dispatch".into()),
                role: "worker".into(),
                backend: Some("codex".into()),
                model: "gpt-5-codex".into(),
                effort: None,
                user_id: Some("U-outsider".into()),
                response_url: None,
                channel: None,
            },
        )
        .await;

        assert!(
            kranz_engine::control::drain(&MissionPaths::new(tmp.path(), "m-gated-dispatch"))
                .unwrap()
                .is_empty(),
            "an unlisted user's Action::Config must enqueue nothing via dispatch_action"
        );
    }
}
