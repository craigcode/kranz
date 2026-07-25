//! The slash-command gate + runner layer (`/kranz draft`, `/kranz work run`,
//! `/kranz merge`, `/kranz approve <slug>`) — extracted from `bridge.rs` in
//! the monolith split (pure code motion, no behavior change). Each command
//! splits into a SYNCHRONOUS gate phase (allowlist + host-presence check and
//! the immediate ack blocks — no host call, so it stays unit-testable without
//! a live `SlackClient`) and an async run phase that drives the host only
//! after the caller has posted the ack. The dispatch arms that call them live
//! in [`crate::dispatch`]; see `bridge.rs`'s module docs for the inbound
//! runtime.

use crate::bridge::error_blocks;
use crate::config::SlackConfig;
use crate::host::SharedHost;
use kranz_engine::draft::DraftOutcome;
use serde_json::Value;

/// The outcome of the SYNCHRONOUS `gate_draft_command` phase — no
/// `PlanningHost::draft` call has happened by the time any of these variants
/// is returned. `Ready` carries the immediate hourglass ack (posted first,
/// mirroring [`crate::inbound::Action::NewMission`]) that the caller must post BEFORE
/// awaiting [`run_draft`], so the invoker sees the ack immediately rather
/// than only once the multi-minute draft turn completes.
pub enum DraftGate {
    Unauthorized,
    NoHost(Vec<Value>),
    InvalidSlug(Vec<Value>),
    Ready(Vec<Value>),
}

/// `/kranz draft <slug>` gate/ack phase — SPEND action, gated EXACTLY like
/// `/kranz new` (same gate, same standard refusal). Runs
/// `cfg.is_authorized`, [`kranz_engine::ticket::Ticket::ensure_valid_slug`],
/// and the host-presence check, and builds the hourglass ack for the `Ready`
/// case. Makes NO `PlanningHost::draft` call — that is the caller's job via
/// [`run_draft`], AFTER posting the `Ready` ack — so this phase stays
/// synchronous and unit-testable without a live `SlackClient`.
pub fn gate_draft_command(
    cfg: &SlackConfig,
    host: Option<&SharedHost>,
    slug: &str,
    user_id: Option<&str>,
) -> DraftGate {
    if !cfg.is_authorized(user_id) {
        return DraftGate::Unauthorized;
    }
    if let Err(e) = kranz_engine::ticket::Ticket::ensure_valid_slug(slug) {
        return DraftGate::InvalidSlug(error_blocks(&format!("Couldn't draft `{slug}`: {e}")));
    }
    if host.is_none() {
        return DraftGate::NoHost(error_blocks(&format!(
            "This bridge has no hosted planning engine (it was started without \
             `kranz serve`). Use `kranz ticket draft {slug}` in a terminal, or the \
             web UI via `kranz serve --open`."
        )));
    }
    // Ack IMMEDIATELY: the draft turn (create + seed + drive to a terminal
    // outcome) takes minutes, same reasoning as NewMission/RequestPlan. The
    // caller must post this BEFORE calling `run_draft`.
    DraftGate::Ready(error_blocks(&format!(
        ":hourglass_flowing_sand: Drafting `{slug}` — the seeding planning turn \
         usually takes a minute or two; the result will post here."
    )))
}

/// The async run phase of `/kranz draft <slug>`, called ONLY after the
/// caller has posted the `DraftGate::Ready` ack. Drives [`crate::host::PlanningHost::draft`]
/// to a terminal [`DraftOutcome`] and maps it to the terminal result blocks,
/// posting the orchestrator's clarifying questions back to the invoker on
/// `NeedsContext`.
pub async fn run_draft(host: &SharedHost, slug: &str) -> Vec<Value> {
    match host.draft(slug).await {
        Ok(DraftOutcome::ParkedForReview {
            mission_id,
            mission_branch,
        }) => error_blocks(&format!(
            ":white_check_mark: Draft ready for review — mission `{mission_id}`, \
             branch `{mission_branch}`. Ticket `{slug}` is now in review."
        )),
        Ok(DraftOutcome::PlanAsProse { mission_id }) => error_blocks(&format!(
            ":warning: Draft for `{slug}` NOT queued — mission `{mission_id}`'s orchestrator \
             produced a plan but emitted it as prose instead of through the plan channel, so \
             nothing was queued. Run `/kranz draft {slug}` again."
        )),
        Ok(DraftOutcome::Enqueued { mission_id }) => error_blocks(&format!(
            ":white_check_mark: Draft approved and queued — mission `{mission_id}`. \
             Ticket `{slug}` is now queued."
        )),
        Ok(DraftOutcome::NeedsContext {
            mission_id,
            questions,
        }) => {
            let mut text = format!(
                ":question: Mission `{mission_id}` needs more context before drafting \
                 `{slug}` can continue:\n"
            );
            for q in &questions {
                text.push_str(&format!("• {q}\n"));
            }
            error_blocks(text.trim_end())
        }
        Err(e) => error_blocks(&format!("Couldn't draft `{slug}`: {e}")),
    }
}

/// The outcome of the SYNCHRONOUS `gate_work_run_command` phase — mirrors
/// [`DraftGate`]. No [`crate::host::PlanningHost::drain`] call has happened
/// by the time any of these variants is returned; `Ready` carries the
/// immediate ack the caller must post BEFORE awaiting [`run_work_run`].
pub enum WorkRunGate {
    Unauthorized,
    NoHost(Vec<Value>),
    Ready(Vec<Value>),
}

/// `/kranz work run` gate/ack phase — SPEND action, gated EXACTLY like
/// `/kranz new` / `/kranz draft` (same gate, same standard refusal). Makes NO
/// `PlanningHost::drain` call — that is the caller's job via
/// [`run_work_run`], AFTER posting the `Ready` ack — so this phase stays
/// synchronous and unit-testable without a live `SlackClient`.
pub fn gate_work_run_command(
    cfg: &SlackConfig,
    host: Option<&SharedHost>,
    user_id: Option<&str>,
) -> WorkRunGate {
    if !cfg.is_authorized(user_id) {
        return WorkRunGate::Unauthorized;
    }
    if host.is_none() {
        return WorkRunGate::NoHost(error_blocks(
            "This bridge has no hosted planning engine (it was started without \
             `kranz serve`). Use `kranz work` in a terminal to drain the queue.",
        ));
    }
    WorkRunGate::Ready(error_blocks(
        ":hourglass_flowing_sand: Running the queue — draining now; progress posts per mission. \
         Backend readiness (ok/missing/unauthenticated/rate_limited/unsupported/meterless) is on \
         the Mission Control queue; hard failures park with a ticket note instead of a doomed start.",
    ))
}

/// The async run phase of `/kranz work run`, called ONLY after the caller has
/// posted the `WorkRunGate::Ready` ack. Drives [`crate::host::PlanningHost::drain`]
/// — the bridge itself never resumes/runs a mission on the socket read loop;
/// the host spawns the drain as a background task on the serve process.
pub async fn run_work_run(host: &SharedHost) -> Vec<Value> {
    match host.drain().await {
        Ok(()) => error_blocks(":white_check_mark: Queue drain triggered."),
        Err(e) => error_blocks(&format!("Couldn't drain the queue: {e}")),
    }
}

/// The outcome of the SYNCHRONOUS `gate_merge_command` phase — mirrors
/// [`WorkRunGate`]. No [`crate::host::PlanningHost::merge`] call has happened
/// by the time any of these variants is returned; `Ready` carries the
/// immediate ack the caller must post BEFORE awaiting [`run_merge`].
pub enum MergeGate {
    Unauthorized,
    NoHost(Vec<Value>),
    Ready(Vec<Value>),
}

/// `/kranz merge <slug|id>` / Delivered-card Merge button gate/ack phase —
/// spend-adjacent, gated EXACTLY like [`gate_work_run_command`]. Makes NO
/// `PlanningHost::merge` call — that is the caller's job via [`run_merge`],
/// AFTER posting the `Ready` ack — so this phase stays synchronous and
/// unit-testable without a live `SlackClient`.
pub fn gate_merge_command(
    cfg: &SlackConfig,
    host: Option<&SharedHost>,
    user_id: Option<&str>,
) -> MergeGate {
    if !cfg.is_authorized(user_id) {
        return MergeGate::Unauthorized;
    }
    if host.is_none() {
        return MergeGate::NoHost(error_blocks(
            "This bridge has no hosted planning engine (it was started without \
             `kranz serve`). Use `kranz merge <id>` in a terminal.",
        ));
    }
    MergeGate::Ready(error_blocks(
        ":hourglass_flowing_sand: Merging — running the gate suite now; the result posts here.",
    ))
}

/// The async run phase of `/kranz merge <slug|id>`, called ONLY after the
/// caller has posted the `MergeGate::Ready` ack. Drives
/// [`crate::host::PlanningHost::merge`] and forwards its outcome — merged
/// commit, or the refusal (dirty tree / failing gate / conflict). Long gate
/// failures preserve their useful tail within Slack's Block Kit limit.
pub async fn run_merge(host: &SharedHost, mission_id: &str) -> Vec<Value> {
    match host.merge(mission_id).await {
        Ok(value) => {
            let commit = value.get("commit").and_then(Value::as_str).unwrap_or("?");
            let mut message =
                format!(":white_check_mark: Merged `{mission_id}` — commit `{commit}`.");
            if let Some(warning) = value
                .get("staleBase")
                .and_then(|v| v.get("message"))
                .and_then(Value::as_str)
            {
                message.push_str(&format!("\n:warning: {warning}"));
            }
            error_blocks(&message)
        }
        Err(e) => merge_error_blocks(mission_id, &e.to_string()),
    }
}

/// The outcome of `run_approve_ticket_command`: whether the invoker was
/// authorized, and the reply blocks to post (`None` only when unauthorized,
/// mirroring [`DraftGate`]).
pub struct ApproveTicketInvocation {
    pub authorized: bool,
    pub result: Option<Vec<Value>>,
}

/// `/kranz approve <slug>` — the slug-resolving twin of `/kranz approve
/// <mission-id>`, gated EXACTLY like it (same allowlist, same standard
/// refusal). Holds the gate + host-call logic so it is unit-testable without
/// a live `SlackClient`. Runs [`crate::host::PlanningHost::approve_ticket`] —
/// the SAME `kranz_engine::deps::approve_ticket` gate the REST/CLI approve
/// path runs — and forwards a blocked-by / not-REVIEW / cycle refusal
/// VERBATIM (never paraphrased).
pub async fn run_approve_ticket_command(
    cfg: &SlackConfig,
    host: Option<&SharedHost>,
    slug: &str,
    user_id: Option<&str>,
) -> ApproveTicketInvocation {
    if !cfg.is_authorized(user_id) {
        return ApproveTicketInvocation {
            authorized: false,
            result: None,
        };
    }
    let Some(host) = host else {
        return ApproveTicketInvocation {
            authorized: true,
            result: Some(error_blocks(&format!(
                "This bridge has no hosted planning engine (it was started without \
                 `kranz serve`). Use `kranz ticket approve {slug}` in a terminal, or the \
                 web UI via `kranz serve --open`."
            ))),
        };
    };
    let result = match host.approve_ticket(slug).await {
        Ok(mission_id) => error_blocks(&format!(
            ":white_check_mark: Approved and queued — ticket `{slug}` \u{2192} mission \
             `{mission_id}`. The `kranz work` dispatcher runs it next."
        )),
        // VERBATIM: `e` is the engine's own refusal message (blocked-by,
        // not-REVIEW, or a blocked-by cycle) — forwarded unchanged, never
        // wrapped in extra prose that would obscure it.
        Err(e) => error_blocks(&e.to_string()),
    };
    ApproveTicketInvocation {
        authorized: true,
        result: Some(result),
    }
}

fn merge_error_blocks(mission_id: &str, detail: &str) -> Vec<Value> {
    // Gate runners put the useful assertion summary at the end. Preserve that
    // tail while keeping the complete Block Kit field safely below Slack's
    // 3,000-character limit (and leave room for instance labeling).
    const MAX_DETAIL: usize = 2300;
    let chars: Vec<char> = detail.chars().collect();
    let clipped = if chars.len() > MAX_DETAIL {
        format!(
            "…{}",
            chars[chars.len() - MAX_DETAIL..].iter().collect::<String>()
        )
    } else {
        detail.to_string()
    };
    error_blocks(&format!("Couldn't merge `{mission_id}`:\n{clipped}"))
}
