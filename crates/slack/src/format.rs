//! Pure Slack Block Kit formatting for outbound mission notifications.
//!
//! Every function here is a pure `data -> serde_json::Value` transform (no I/O,
//! no clock, no network) so the message shapes are unit-tested exhaustively.
//! The bridge computes the inputs (from the event log + folded state) and calls
//! these to build the `blocks` array it posts via [`crate::client`].
//!
//! Block Kit reference: a message is an array of block objects. We use
//! `header` (plain_text), `section` (mrkdwn), `context`, `divider`, and an
//! `actions` block carrying `button` elements. Button `action_id`s are the
//! contract the inbound router keys on (see [`crate::inbound`]).

use serde_json::{json, Value};

/// `action_id` of the "approve & queue" button. The inbound router matches this
/// exact string, so it is a shared contract between [`build_plan_ready`] and
/// [`crate::inbound`].
pub const APPROVE_ACTION_ID: &str = "kranz_approve";

/// `action_id` of the "approve & start" button on a plan-review message
/// ([`build_plan_review`]). Distinct from [`APPROVE_ACTION_ID`] (approve &
/// queue) so the two paths can be told apart when button wiring lands; the
/// button `value` carries the mission id, same as approve.
pub const START_ACTION_ID: &str = "kranz_start";

/// `action_id` of the "Merge" button on a Delivered card ([`build_complete`])
/// for a completed-and-unmerged mission. The button `value` carries the
/// mission id, same as approve/start. Routing + gating (dirty-tree refusal,
/// token/allowlist check, running the gate suite) is a sibling concern; this
/// crate only renders the button.
pub const MERGE_ACTION_ID: &str = "kranz_merge";

/// `action_id` of the "Approve revision" button on a proposed-revision card.
/// The button value is `<mission-id>:<revision>`.
pub const APPROVE_REVISION_ACTION_ID: &str = "kranz_approve_revision";

/// `action_id` of the "Reject revision" button on a proposed-revision card.
/// The button value is `<mission-id>:<revision>`.
pub const REJECT_REVISION_ACTION_ID: &str = "kranz_reject_revision";

/// `action_id` of the "Approve grant" button on a grant-request card. The
/// button value is `<mission-id>:<command>` (mission ids never contain `:`, so
/// the first colon splits cleanly and the command may contain more).
pub const APPROVE_GRANT_ACTION_ID: &str = "kranz_approve_grant";

/// `action_id` of the "Deny" button on a grant-request card. The button value
/// is `<mission-id>:<command>`.
pub const DENY_GRANT_ACTION_ID: &str = "kranz_deny_grant";

/// `action_id` of the "Queue" button on a `/kranz todo` Reviewable-ticket row.
/// The button value carries the ticket slug and routes through the same
/// allowlist-gated [`crate::inbound::Action::QueueTicket`] path as
/// `/kranz queue <slug>`; rendering the todo list itself remains read-only.
pub const QUEUE_TICKET_ACTION_ID: &str = "kranz_queue_ticket";

/// `callback_id` of the new-mission modal ([`build_new_mission_modal`]); its
/// `view_submission` routes to [`crate::inbound::Action::NewMission`].
pub const NEW_MISSION_CALLBACK_ID: &str = "kranz_new_mission";
/// block_id / action_id of the modal's goal input, used to dig the goal out
/// of `view.state.values` on submission.
pub const NEW_MISSION_GOAL_BLOCK: &str = "goal";
pub const NEW_MISSION_GOAL_ACTION: &str = "goal_text";

/// `callback_id` of the new-ticket modal ([`build_new_ticket_modal`]); its
/// `view_submission` routes to [`crate::inbound::Action::CreateTicket`].
/// The slug/title (already fixed by `/kranz ticket new <slug> <title...>`)
/// ride in `private_metadata` as a small JSON object rather than as
/// modal inputs — they were already given on the command line.
pub const NEW_TICKET_CALLBACK_ID: &str = "kranz_new_ticket";
pub const NEW_TICKET_GOAL_BLOCK: &str = "ticket_goal";
pub const NEW_TICKET_GOAL_ACTION: &str = "ticket_goal_text";
pub const NEW_TICKET_CONTEXT_BLOCK: &str = "ticket_context";
pub const NEW_TICKET_CONTEXT_ACTION: &str = "ticket_context_text";

/// `callback_id` of the config modal ([`build_config_modal`]); its
/// `view_submission` routes to [`crate::inbound::Action::Config`].
pub const CONFIG_CALLBACK_ID: &str = "kranz_config";
pub const CONFIG_MISSION_BLOCK: &str = "mission";
pub const CONFIG_MISSION_ACTION: &str = "mission_id";
pub const CONFIG_ROLE_BLOCK: &str = "role";
pub const CONFIG_ROLE_ACTION: &str = "role_select";
pub const CONFIG_BACKEND_BLOCK: &str = "backend";
pub const CONFIG_BACKEND_ACTION: &str = "backend_select";
pub const CONFIG_MODEL_BLOCK: &str = "model";
pub const CONFIG_MODEL_ACTION: &str = "model_text";
pub const CONFIG_EFFORT_BLOCK: &str = "effort";
pub const CONFIG_EFFORT_ACTION: &str = "effort_select";

/// A mission whose plan is ready for review. The `value` carried by the approve
/// button is the mission id, so a click round-trips back to the right mission.
#[derive(Debug, Clone)]
pub struct PlanReady {
    pub mission_id: String,
    pub goal: String,
    /// Milestone titles, in plan order (rendered as a compact bullet list).
    pub milestone_titles: Vec<String>,
    /// Number of validation-contract assertions (shown as a count, not dumped).
    pub assertion_count: usize,
}

/// A proposed mid-mission plan revision awaiting human consent.
#[derive(Debug, Clone)]
pub struct RevisionReady {
    pub mission_id: String,
    pub revision: u32,
    pub instructions: String,
    pub milestone_titles: Vec<String>,
    pub assertion_count: usize,
}

/// A parked capability-grant request awaiting human consent.
#[derive(Debug, Clone)]
pub struct GrantReady {
    pub mission_id: String,
    pub milestone_id: String,
    pub kind: kranz_engine::types::GrantKind,
    /// The granted target: a command string, or a path glob for a touch grant.
    pub command: String,
}

/// A ticket that bounced back needing more context, with the orchestrator's
/// verbatim clarifying questions.
#[derive(Debug, Clone)]
pub struct NeedsContext {
    pub ticket_slug: String,
    pub questions: Vec<String>,
}

/// A blocked milestone awaiting a human in the thread.
#[derive(Debug, Clone)]
pub struct Blocked {
    pub mission_id: String,
    pub milestone_id: String,
    pub reason: String,
}

/// Outcome of a mission run (drives the emoji/verb in [`build_complete`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Failed,
}

/// A finished mission, with a short report summary and the branch to review.
#[derive(Debug, Clone)]
pub struct Complete {
    pub mission_id: String,
    pub outcome: Outcome,
    /// One-line summary (report excerpt, or failure reason for a failed run).
    pub summary: String,
    pub branch: String,
    /// Total cost in USD, if known (rendered to cents).
    pub cost_usd: Option<f64>,
    /// `git diff --stat` of `base_sha..mission_branch`, if it could be
    /// computed (needs a pinned `base_sha` and a live git repo). `None`
    /// degrades to omitting the diff-stat line, same as `cost_usd`.
    pub diff_stat: Option<String>,
    /// Optional PR handoff hint (copyable command / unavailable reason).
    /// Never implies kranz pushed.
    pub pr_handoff: Option<String>,
}

/// A just-created mission whose planning conversation opened in a thread. The
/// ack tells the user what was created and how to drive planning from here.
#[derive(Debug, Clone)]
pub struct NewMissionAck {
    pub mission_id: String,
    pub goal: String,
    /// The orchestrator's opening reply (its scoping questions), if the seed
    /// planning turn produced one. Rendered so the user can answer in-thread.
    pub opening_reply: Option<String>,
}

/// A folded status summary for one mission (built from the reduced state, then
/// rendered here). `summary` is the caller's already-rendered status body (a
/// short multi-line mrkdwn string); this builder frames it with a header.
#[derive(Debug, Clone)]
pub struct StatusSummary {
    pub mission_id: String,
    /// Short status word (e.g. `Planning`, `Running`, `Complete`) for the
    /// header pill.
    pub status: String,
    /// Pre-rendered status body (milestone tally, cost, goal excerpt, …).
    pub summary: String,
}

/// One backlog ticket row for `/kranz ticket list`: slug, priority, pipeline
/// state, title, and its `blocked-by` slugs (empty when unblocked).
#[derive(Debug, Clone)]
pub struct TicketRow {
    pub slug: String,
    pub priority: u8,
    /// Pipeline state word (e.g. `New`, `Drafting`, `Review`).
    pub state: String,
    pub title: String,
    pub blocked_by: Vec<String>,
}

/// The full detail for `/kranz ticket show <slug>`: everything a reviewer
/// needs to decide without opening the web UI.
#[derive(Debug, Clone)]
pub struct TicketDetail {
    pub slug: String,
    pub title: String,
    pub goal: String,
    /// Pipeline state word (e.g. `New`, `Drafting`, `Review`).
    pub state: String,
    pub blocked_by: Vec<String>,
    /// Clarifying questions the orchestrator appended (empty when none).
    pub needs_context: Vec<String>,
}

/// Canonical pipeline stages, mirrored from
/// `apps/dashboard/src/lib/pipelineStage.ts`. Keep this enum in that order so
/// Slack status counts render exactly like the dashboard columns/lenses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PipelineStage {
    Captured,
    Drafting,
    NeedsYou,
    Reviewable,
    Queued,
    Running,
    Delivered,
    Landed,
    Failed,
    Abandoned,
}

impl PipelineStage {
    pub const ALL: [PipelineStage; 10] = [
        PipelineStage::Captured,
        PipelineStage::Drafting,
        PipelineStage::NeedsYou,
        PipelineStage::Reviewable,
        PipelineStage::Queued,
        PipelineStage::Running,
        PipelineStage::Delivered,
        PipelineStage::Landed,
        PipelineStage::Failed,
        PipelineStage::Abandoned,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PipelineStage::Captured => "captured",
            PipelineStage::Drafting => "drafting",
            PipelineStage::NeedsYou => "needs-you",
            PipelineStage::Reviewable => "reviewable",
            PipelineStage::Queued => "queued",
            PipelineStage::Running => "running",
            PipelineStage::Delivered => "delivered",
            PipelineStage::Landed => "landed",
            PipelineStage::Failed => "failed",
            PipelineStage::Abandoned => "abandoned",
        }
    }
}

/// Count of work items at one canonical pipeline stage.
#[derive(Debug, Clone)]
pub struct StageCount {
    pub stage: PipelineStage,
    pub count: usize,
}

/// The currently-running mission line in `/kranz status`, if one can be
/// determined from the queue busy lock or the folded pipeline rows.
#[derive(Debug, Clone)]
pub struct RunningMissionSnapshot {
    pub mission_id: String,
    pub title: String,
    pub status: String,
    pub cost_usd: Option<f64>,
}

/// A Delivered row whose mission branch has not landed yet.
#[derive(Debug, Clone)]
pub struct UnmergedMission {
    pub mission_id: String,
    pub title: String,
    pub ticket_slug: Option<String>,
}

/// Fixed, already-derived state for `/kranz status`. The bridge owns all repo
/// reads and pipeline derivation; this formatter only renders the snapshot.
#[derive(Debug, Clone)]
pub struct PipelineStatusSnapshot {
    pub running: Option<RunningMissionSnapshot>,
    pub queue_depth: usize,
    pub stage_counts: Vec<StageCount>,
    pub unmerged: Vec<UnmergedMission>,
}

/// The three human-awaiting pipeline action classes surfaced by `/kranz todo`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TodoActionKind {
    Reviewable,
    Delivered,
    NeedsYou,
}

impl TodoActionKind {
    fn label(self) -> &'static str {
        match self {
            TodoActionKind::Reviewable => "REVIEWABLE",
            TodoActionKind::Delivered => "DELIVERED",
            TodoActionKind::NeedsYou => "NEEDS-YOU",
        }
    }
}

/// Optional one-tap affordance for a todo row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoTarget {
    QueueTicket { slug: String },
    MergeMission { mission_id: String },
    OpenUrl { label: String, url: String },
    None,
}

/// One operator-facing pipeline action row for `/kranz todo`.
#[derive(Debug, Clone)]
pub struct TodoAction {
    pub kind: TodoActionKind,
    pub id: String,
    pub title: String,
    pub note: Option<String>,
    pub target: TodoTarget,
}

/// A human-only gate from `docs/operator-gates.md`.
#[derive(Debug, Clone)]
pub struct GateItem {
    pub title: String,
}

/// Fixed, already-derived state for `/kranz todo`.
#[derive(Debug, Clone)]
pub struct OperatorTodo {
    pub pipeline_actions: Vec<TodoAction>,
    pub gated_items: Vec<GateItem>,
}

/// One strategic roadmap option from `docs/roadmap-options.md`.
#[derive(Debug, Clone)]
pub struct RoadmapOption {
    pub title: String,
    pub summary: String,
    pub why: Option<String>,
    pub trigger: Option<String>,
    pub source: Option<String>,
    pub ticket: Option<String>,
}

/// A grouped roadmap section, preserving the order from the tracked source.
#[derive(Debug, Clone)]
pub struct RoadmapSection {
    pub name: String,
    pub options: Vec<RoadmapOption>,
}

/// Fixed, already-derived state for `/kranz roadmap`.
#[derive(Debug, Clone)]
pub struct RoadmapSnapshot {
    pub sections: Vec<RoadmapSection>,
}

/// A plan awaiting review, with the pieces a reviewer needs before spending:
/// the goal, milestone list, and validation-assertion count. Renders with
/// **Approve & start** / **Approve & queue** buttons carrying the mission id.
#[derive(Debug, Clone)]
pub struct PlanReview {
    pub mission_id: String,
    pub goal: String,
    pub milestone_titles: Vec<String>,
    pub assertion_count: usize,
    pub considered_alternatives: Option<PlanAlternativesReview>,
    /// Optional one-line calibrated cost/time estimate string (rendered as
    /// context when present).
    pub estimate: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PlanAlternativesReview {
    pub chosen: String,
    pub rejected: Vec<RejectedAlternativeReview>,
}

#[derive(Debug, Clone)]
pub struct RejectedAlternativeReview {
    pub approach: String,
    pub trade_off: String,
}

/// Slack truncates and mis-renders very long single strings; keep any one field
/// we interpolate under a sane cap so a runaway reason can't blow the 3000-char
/// per-text-object Block Kit limit.
const MAX_FIELD: usize = 2500;

/// Block Kit `header` text objects are plain_text capped at 150 chars.
const MAX_HEADER: usize = 150;

/// Escape Slack mrkdwn control characters in user-supplied text so it renders
/// as literal text (Slack parses `<…>` as links/mentions and `&` as an entity
/// start in mrkdwn fields). Per Slack's escaping rules only `&`, `<`, `>` need
/// escaping; `&` goes first so already-escaped output isn't double-escaped.
/// plain_text fields (e.g. header blocks) render verbatim and must NOT be
/// escaped (the entities would show literally).
pub fn escape_mrkdwn(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Render milestone titles as a bulleted mrkdwn list, escaping each title.
/// Titles are LLM-authored (the orchestrator's refined plan), so a title like
/// `<https://evil|click>` must not render as a live Slack link. Empty input
/// yields the italic placeholder. Shared by every plan/revision card so the
/// escaping can't drift between them.
fn milestone_bullets(titles: &[String]) -> String {
    if titles.is_empty() {
        return "_(no milestones listed)_".to_string();
    }
    let mut out = String::new();
    for title in titles {
        out.push_str("• ");
        out.push_str(&escape_mrkdwn(title.trim()));
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out
}

// -- instance labels ----------------------------------------------------------

/// Label a message with the configured instance name: a leading `[name] `
/// prefix on the message's first text (the header when there is one, the first
/// section otherwise). ONE mechanism for every surface — notifications, help,
/// ephemerals, App Home — so a multi-instance workspace (one Slack app per
/// instance, docs/slack-management.md "Running multiple instances") always
/// reads the same way. `None`/blank name → the blocks are returned UNCHANGED
/// (byte-identical single-instance behavior).
///
/// The name is user-supplied config, treated strictly as text: in a plain_text
/// header it is inserted verbatim (plain_text never parses markup) and the
/// header is re-clipped to Block Kit's 150-char cap; in an mrkdwn text it is
/// [`escape_mrkdwn`]-escaped so `<&>` can't smuggle links/mentions/entities.
/// No block is ever added or removed for a message that has a leading text, so
/// existing block-count expectations hold.
pub fn label_blocks(mut blocks: Vec<Value>, instance_name: Option<&str>) -> Vec<Value> {
    let Some(name) = instance_name.map(str::trim).filter(|n| !n.is_empty()) else {
        return blocks;
    };
    let prefixed = match blocks.first_mut() {
        Some(first) => prefix_first_text(first, name),
        None => return blocks, // nothing to label
    };
    if !prefixed {
        // Defensive fallback: a message whose first block carries no direct
        // text (none of ours today) gets a leading context line instead.
        blocks.insert(0, context(&format!("[{}]", escape_mrkdwn(name))));
    }
    blocks
}

/// Prefix `[name] ` onto a block's `/text/text`, escaping for mrkdwn and
/// re-clipping headers to their 150-char cap. Returns false when the block has
/// no direct text field (e.g. an `actions` or `context` block).
fn prefix_first_text(block: &mut Value, name: &str) -> bool {
    let is_plain_header = block["type"] == "header";
    let Some(text) = block.pointer_mut("/text/text") else {
        return false;
    };
    let Some(old) = text.as_str() else {
        return false;
    };
    *text = if is_plain_header {
        Value::String(clip_to(&format!("[{name}] {old}"), MAX_HEADER))
    } else {
        Value::String(format!("[{}] {old}", escape_mrkdwn(name)))
    };
    true
}

/// [`label_blocks`] for an App Home **view** object
/// (`{"type":"home","blocks":[…]}`): labels the view's blocks in place with the
/// same mechanism, so the Home header reads `[name] … Kranz — Mission Control`.
/// `None`/blank name → the view is returned unchanged.
pub fn label_home_view(mut view: Value, instance_name: Option<&str>) -> Value {
    if let Some(blocks) = view.get_mut("blocks").and_then(Value::as_array_mut) {
        let labeled = label_blocks(std::mem::take(blocks), instance_name);
        *blocks = labeled;
    }
    view
}

/// The deep-link URL for a mission's dashboard view: `<dashboard_url>#/m/<id>`.
/// A single `/` between the base and the fragment is collapsed so a base with or
/// without a trailing slash both yield exactly one (`…:4600#/m/x` regardless of
/// whether the base ended in `/`). Pure; unit-tested.
pub fn dashboard_deep_link(dashboard_url: &str, mission_id: &str) -> String {
    let base = dashboard_url.trim_end_matches('/');
    if base.contains("#/r/") {
        format!("{base}/m/{mission_id}")
    } else {
        format!("{base}/#/m/{mission_id}")
    }
}

/// Scope a dashboard base URL to one repository without changing legacy
/// single-repository links.
pub fn dashboard_repo_url(dashboard_url: &str, repo_id: &str) -> String {
    format!("{}/#/r/{}", dashboard_url.trim_end_matches('/'), repo_id)
}

/// A Block Kit `actions` block carrying a single "Open in dashboard" link
/// button that deep-links to `<dashboard_url>#/m/<mission_id>`, or `None` when
/// `dashboard_url` is `None`/blank (no button, no behavior change). A link
/// button (has a `url`, no interaction handler) opens the browser directly, so
/// it needs no inbound routing. Pure; unit-tested.
pub fn dashboard_button(dashboard_url: Option<&str>, mission_id: &str) -> Option<Value> {
    let url = dashboard_url.map(str::trim).filter(|u| !u.is_empty())?;
    Some(json!({
        "type": "actions",
        "elements": [
            {
                "type": "button",
                "text": { "type": "plain_text", "text": "Open in dashboard" },
                "url": dashboard_deep_link(url, mission_id),
                "action_id": "kranz_open_dashboard",
            }
        ]
    }))
}

/// Append a dashboard deep-link `actions` block to `blocks` when a dashboard URL
/// is configured; a no-op otherwise. Kept private and shared by the three
/// mission-notification builders so the link shape is identical everywhere.
fn push_dashboard_button(blocks: &mut Vec<Value>, dashboard_url: Option<&str>, mission_id: &str) {
    if let Some(button) = dashboard_button(dashboard_url, mission_id) {
        blocks.push(button);
    }
}

/// Plan-ready announcement for the `plan.approved` event (plan committed on
/// the mission branch, awaiting human queue/start). Header says "ready for
/// review" — not "approved" — because this fires when the *engine* accepts
/// the plan (draft park or interactive approve), before the operator queues
/// it. Carries **Approve & start** / **Approve & queue** so the draft path
/// has the same affordances as [`build_plan_review`]. Re-queue is a no-op
/// ([`kranz_engine::queue::enqueue`]); stale second taps after interactive
/// approve are refused by [`approve_flow`]'s state guards. When
/// `dashboard_url` is set, an "Open in dashboard" deep-link is appended.
pub fn build_plan_ready(p: &PlanReady, dashboard_url: Option<&str>) -> Vec<Value> {
    let milestones = milestone_bullets(&p.milestone_titles);

    let mut blocks = vec![
        header(&format!("Plan ready for review — {}", p.mission_id)),
        section(&format!("*Goal*\n{}", clip(&escape_mrkdwn(&p.goal)))),
        section(&format!("*Milestones*\n{}", clip(milestones.trim_end()))),
        context(&format!(
            "{} validation assertion{} · mission `{}` · plan committed on the mission branch",
            p.assertion_count,
            plural(p.assertion_count),
            p.mission_id
        )),
        json!({
            "type": "actions",
            "elements": [
                {
                    "type": "button",
                    "style": "primary",
                    "text": { "type": "plain_text", "text": "Approve & start" },
                    "action_id": START_ACTION_ID,
                    "value": p.mission_id,
                },
                {
                    "type": "button",
                    "text": { "type": "plain_text", "text": "Approve & queue" },
                    "action_id": APPROVE_ACTION_ID,
                    "value": p.mission_id,
                }
            ]
        }),
    ];
    push_dashboard_button(&mut blocks, dashboard_url, &p.mission_id);
    blocks
}

/// Proposed-revision announcement: shows the operator instructions, revised
/// milestone list, and explicit approve/reject buttons carrying the exact
/// revision number.
pub fn build_revision_ready(r: &RevisionReady, dashboard_url: Option<&str>) -> Vec<Value> {
    let milestones = milestone_bullets(&r.milestone_titles);
    let value = format!("{}:{}", r.mission_id, r.revision);
    let mut blocks = vec![
        header(&format!(
            "Revision {} proposed — {}",
            r.revision, r.mission_id
        )),
        // Instructions are untrusted operator input: escape Slack control
        // sequences (<!channel>, <@Uxxx>, <url|label>) so a revision request
        // can't inject broadcast pings or misleading links into the card.
        section(&format!(
            "*Instructions*\n{}",
            clip(&escape_mrkdwn(r.instructions.trim()))
        )),
        section(&format!(
            "*Revised milestones*\n{}",
            clip(milestones.trim_end())
        )),
        context(&format!(
            "{} validation assertion{} · mission `{}`",
            r.assertion_count,
            plural(r.assertion_count),
            r.mission_id
        )),
        json!({
            "type": "actions",
            "elements": [
                {
                    "type": "button",
                    "style": "primary",
                    "text": { "type": "plain_text", "text": "Approve revision" },
                    "action_id": APPROVE_REVISION_ACTION_ID,
                    "value": value.clone(),
                },
                {
                    "type": "button",
                    "style": "danger",
                    "text": { "type": "plain_text", "text": "Reject revision" },
                    "action_id": REJECT_REVISION_ACTION_ID,
                    "value": value,
                }
            ]
        }),
    ];
    push_dashboard_button(&mut blocks, dashboard_url, &r.mission_id);
    blocks
}

/// Parked-grant announcement: names the exact command a validator was denied
/// and offers approve/deny buttons carrying `<mission-id>:<command>`. Approving
/// extends `command_grants` and re-validates; denying blocks the milestone.
pub fn build_grant_ready(g: &GrantReady, dashboard_url: Option<&str>) -> Vec<Value> {
    let value = format!("{}:{}", g.mission_id, g.command);
    let (blurb, field_label) = match g.kind {
        kranz_engine::types::GrantKind::Command => (
            "A validator is blocked on a command outside its allow-set.",
            "*Blocked command*",
        ),
        kranz_engine::types::GrantKind::TouchPath => (
            "A worker wrote a path outside the mission's touch-set.",
            "*Out-of-contract path*",
        ),
        kranz_engine::types::GrantKind::WorkerDeny => (
            "A worker command was blocked by a deny rule. Approving LIFTS that \
             rule for this mission (a deliberate erosion of a safety guardrail).",
            "*Deny rule to lift*",
        ),
    };
    let mut blocks = vec![
        header(&format!("Grant requested — {}", g.mission_id)),
        section(blurb),
        // The target is scrubbed at capture, but escape Slack control
        // sequences defensively before rendering it as text.
        section(&format!(
            "{field_label}\n{}",
            clip(&escape_mrkdwn(g.command.trim()))
        )),
        context(&format!(
            "milestone `{}` · mission `{}`",
            g.milestone_id, g.mission_id
        )),
        json!({
            "type": "actions",
            "elements": [
                {
                    "type": "button",
                    "style": "primary",
                    "text": { "type": "plain_text", "text": "Approve grant" },
                    "action_id": APPROVE_GRANT_ACTION_ID,
                    "value": value.clone(),
                },
                {
                    "type": "button",
                    "style": "danger",
                    "text": { "type": "plain_text", "text": "Deny" },
                    "action_id": DENY_GRANT_ACTION_ID,
                    "value": value,
                }
            ]
        }),
    ];
    push_dashboard_button(&mut blocks, dashboard_url, &g.mission_id);
    blocks
}

/// Needs-context message: the orchestrator's questions, threaded, with a nudge
/// to reply in-thread with the answers (which become planning guidance).
pub fn build_needs_context(n: &NeedsContext) -> Vec<Value> {
    let mut body = String::new();
    for q in &n.questions {
        body.push_str("• ");
        body.push_str(q.trim());
        body.push('\n');
    }
    if body.is_empty() {
        body.push_str("_(no specific questions were captured)_");
    }

    vec![
        header(&format!("Ticket needs context — {}", n.ticket_slug)),
        section(&format!(
            "The orchestrator couldn't plan this ticket yet. Answer these:\n{}",
            clip(body.trim_end())
        )),
        context("Reply in this thread with the answers to re-draft the plan."),
    ]
}

/// Blocked-milestone message: reason + an explicit "reply in this thread to
/// unblock" instruction (a threaded reply becomes `kranz msg` guidance). When
/// `dashboard_url` is set, an "Open in dashboard" deep-link button is appended.
pub fn build_blocked(b: &Blocked, dashboard_url: Option<&str>) -> Vec<Value> {
    let mut blocks = vec![
        header(&format!("Milestone blocked — {}", b.mission_id)),
        section(&format!(
            "Milestone `{}` is blocked:\n>{}",
            b.milestone_id,
            clip(b.reason.trim()).replace('\n', "\n>")
        )),
        context("Reply in this thread to unblock (your reply becomes orchestrator guidance)."),
    ];
    push_dashboard_button(&mut blocks, dashboard_url, &b.mission_id);
    blocks
}

/// Completion message: outcome header + summary + branch + optional cost +
/// optional diff stat. When `dashboard_url` is set, an "Open in dashboard"
/// deep-link button is appended. A completed mission also carries a **Merge**
/// button ([`MERGE_ACTION_ID`]) — this is the Delivered card for a
/// completed-and-unmerged mission; the button's routing/gating (dirty-tree
/// refusal, token/allowlist check, gate suite) lives elsewhere, this only
/// renders it.
pub fn build_complete(c: &Complete, dashboard_url: Option<&str>) -> Vec<Value> {
    let (emoji, verb) = match c.outcome {
        Outcome::Completed => (":white_check_mark:", "completed"),
        Outcome::Failed => (":x:", "failed"),
    };
    let mut meta = format!("branch `{}`", c.branch);
    if let Some(cost) = c.cost_usd {
        meta.push_str(&format!(" · cost ${cost:.2}"));
    }
    meta.push_str(&format!(" · mission `{}`", c.mission_id));

    let mut blocks = vec![
        header(&format!("{emoji} Mission {verb} — {}", c.mission_id)),
        section(&clip(c.summary.trim())),
    ];
    if let Some(diff_stat) = c
        .diff_stat
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty())
    {
        blocks.push(section(&format!("*Diff stat*\n```{}```", clip(diff_stat))));
    }
    blocks.push(context(&meta));
    if let Some(hint) = c
        .pr_handoff
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
    {
        blocks.push(section(&format!("*PR handoff*\n```{}```", clip(hint))));
    }
    push_dashboard_button(&mut blocks, dashboard_url, &c.mission_id);
    if c.outcome == Outcome::Completed {
        blocks.push(json!({
            "type": "actions",
            "elements": [
                {
                    "type": "button",
                    "style": "primary",
                    "text": { "type": "plain_text", "text": "Merge" },
                    "action_id": MERGE_ACTION_ID,
                    "value": c.mission_id,
                }
            ]
        }));
    }
    blocks
}

/// New-mission ack (M2.9 slice 1): confirms the mission id + goal and, when the
/// seed planning turn produced opening questions, surfaces them with a nudge to
/// answer in-thread (each reply becomes a planning turn). This is the thread
/// root of the mission's planning conversation.
pub fn build_new_mission_ack(a: &NewMissionAck) -> Vec<Value> {
    let mut blocks = vec![
        header(&format!("Planning {} — new mission", a.mission_id)),
        section(&format!("*Goal*\n{}", clip(a.goal.trim()))),
    ];
    let footer = format!(
        "Reply in this thread to answer — each reply is a planning turn. \
         When you're ready, run `/kranz plan {}` from the channel \
         (Slack doesn't deliver slash commands typed inside a thread).",
        a.mission_id
    );
    if let Some(reply) = a
        .opening_reply
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
    {
        blocks.push(section(&format!("*Orchestrator*\n{}", clip(reply))));
    }
    blocks.push(context(&footer));
    blocks
}

/// The new-mission modal: a real MULTILINE goal field — the escape hatch from
/// Slack's one-line slash-command ceiling. The invoking channel rides in
/// `private_metadata` (a `view_submission` doesn't carry the channel), so the
/// planning thread lands where the command was issued. Pure; unit-tested.
pub fn build_new_mission_modal(channel: &str) -> Value {
    build_new_mission_modal_with_metadata(Value::String(channel.to_string()))
}

pub fn build_new_mission_modal_scoped(channel: &str, repo_id: &str, team_id: &str) -> Value {
    build_new_mission_modal_with_metadata(json!({
        "channel": channel,
        "repoId": repo_id,
        "teamId": team_id,
    }))
}

fn build_new_mission_modal_with_metadata(metadata: Value) -> Value {
    json!({
        "type": "modal",
        "callback_id": NEW_MISSION_CALLBACK_ID,
        "private_metadata": match metadata {
            Value::String(value) => value,
            value => value.to_string(),
        },
        "title": { "type": "plain_text", "text": "New mission" },
        "submit": { "type": "plain_text", "text": "Create" },
        "close": { "type": "plain_text", "text": "Cancel" },
        "blocks": [
            {
                "type": "input",
                "block_id": NEW_MISSION_GOAL_BLOCK,
                "label": { "type": "plain_text", "text": "Mission goal" },
                "element": {
                    "type": "plain_text_input",
                    "action_id": NEW_MISSION_GOAL_ACTION,
                    "multiline": true,
                    "placeholder": {
                        "type": "plain_text",
                        "text": "What should this mission accomplish? Constraints, file paths, \
                                 and acceptance criteria all help the planner."
                    }
                }
            },
            {
                "type": "context",
                "elements": [{
                    "type": "mrkdwn",
                    "text": "Creating spends an orchestrator planning turn. The planning \
                             thread opens in this channel — reply there to keep shaping \
                             the plan."
                }]
            }
        ]
    })
}

/// The new-ticket modal opened from `/kranz ticket new <slug> <title...>` —
/// the multiline escape hatch for goal/context that a single-line slash
/// command can't carry. `slug`/`title` are already fixed by the command line,
/// so they ride in `private_metadata` as JSON (never re-entered) and are only
/// shown back to the human as read-only context; the two inputs collect the
/// goal and context bodies that land in the scaffolded ticket. Pure;
/// unit-tested.
pub fn build_new_ticket_modal(slug: &str, title: &str, channel: &str) -> Value {
    let metadata = json!({ "slug": slug, "title": title, "channel": channel }).to_string();
    build_new_ticket_modal_with_metadata(slug, title, metadata)
}

pub fn build_new_ticket_modal_scoped(
    slug: &str,
    title: &str,
    channel: &str,
    repo_id: &str,
    team_id: &str,
) -> Value {
    let metadata = json!({
        "slug": slug,
        "title": title,
        "channel": channel,
        "repoId": repo_id,
        "teamId": team_id,
    })
    .to_string();
    build_new_ticket_modal_with_metadata(slug, title, metadata)
}

fn build_new_ticket_modal_with_metadata(slug: &str, title: &str, metadata: String) -> Value {
    json!({
        "type": "modal",
        "callback_id": NEW_TICKET_CALLBACK_ID,
        "private_metadata": metadata,
        "title": { "type": "plain_text", "text": "New ticket" },
        "submit": { "type": "plain_text", "text": "Create" },
        "close": { "type": "plain_text", "text": "Cancel" },
        "blocks": [
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!("*{}* — `{}`", escape_mrkdwn(title), escape_mrkdwn(slug))
                }
            },
            {
                "type": "input",
                "block_id": NEW_TICKET_GOAL_BLOCK,
                "optional": true,
                "label": { "type": "plain_text", "text": "Goal" },
                "element": {
                    "type": "plain_text_input",
                    "action_id": NEW_TICKET_GOAL_ACTION,
                    "multiline": true,
                    "placeholder": {
                        "type": "plain_text",
                        "text": "One paragraph — becomes the mission goal."
                    }
                }
            },
            {
                "type": "input",
                "block_id": NEW_TICKET_CONTEXT_BLOCK,
                "optional": true,
                "label": { "type": "plain_text", "text": "Context" },
                "element": {
                    "type": "plain_text_input",
                    "action_id": NEW_TICKET_CONTEXT_ACTION,
                    "multiline": true,
                    "placeholder": {
                        "type": "plain_text",
                        "text": "Anything the drafter needs: file paths, constraints, prior discussion."
                    }
                }
            }
        ]
    })
}

/// The config modal: role/backend selects + free-form model + optional effort
/// — the structured twin of `/kranz config [<id>] <role> [backend] <model>
/// [effort]`, for people who prefer pickers to positional args. Channel rides in
/// `private_metadata` (for the ephemeral reply); the mission id input is
/// optional — blank targets the single active mission, same resolution as
/// the slash form. Pure; unit-tested.
pub fn build_config_modal(channel: &str) -> Value {
    build_config_modal_with_metadata(Value::String(channel.to_string()))
}

pub fn build_config_modal_scoped(channel: &str, repo_id: &str, team_id: &str) -> Value {
    build_config_modal_with_metadata(
        json!({ "channel": channel, "repoId": repo_id, "teamId": team_id }),
    )
}

fn build_config_modal_with_metadata(metadata: Value) -> Value {
    let opt = |v: &str| json!({ "text": { "type": "plain_text", "text": v }, "value": v });
    json!({
        "type": "modal",
        "callback_id": CONFIG_CALLBACK_ID,
        "private_metadata": match metadata {
            Value::String(value) => value,
            value => value.to_string(),
        },
        "title": { "type": "plain_text", "text": "Mission config" },
        "submit": { "type": "plain_text", "text": "Apply" },
        "close": { "type": "plain_text", "text": "Cancel" },
        "blocks": [
            {
                "type": "input",
                "block_id": CONFIG_MISSION_BLOCK,
                "optional": true,
                "label": { "type": "plain_text", "text": "Mission id (blank = the single active mission)" },
                "element": {
                    "type": "plain_text_input",
                    "action_id": CONFIG_MISSION_ACTION,
                    "placeholder": { "type": "plain_text", "text": "m-…" }
                }
            },
            {
                "type": "input",
                "block_id": CONFIG_ROLE_BLOCK,
                "label": { "type": "plain_text", "text": "Role" },
                "element": {
                    "type": "static_select",
                    "action_id": CONFIG_ROLE_ACTION,
                    "options": [opt("orchestrator"), opt("worker"), opt("scrutiny"), opt("functional")]
                }
            },
            {
                "type": "input",
                "block_id": CONFIG_BACKEND_BLOCK,
                "label": { "type": "plain_text", "text": "Backend" },
                "element": {
                    "type": "static_select",
                    "action_id": CONFIG_BACKEND_ACTION,
                    "initial_option": opt("claude"),
                    "options": [opt("claude"), opt("codex"), opt("droid")]
                }
            },
            {
                "type": "input",
                "block_id": CONFIG_MODEL_BLOCK,
                "label": { "type": "plain_text", "text": "Model" },
                "element": {
                    "type": "plain_text_input",
                    "action_id": CONFIG_MODEL_ACTION,
                    "placeholder": { "type": "plain_text", "text": "haiku · sonnet · opus · fable · a model id" }
                }
            },
            {
                "type": "input",
                "block_id": CONFIG_EFFORT_BLOCK,
                "optional": true,
                "label": { "type": "plain_text", "text": "Reasoning effort (optional)" },
                "element": {
                    "type": "static_select",
                    "action_id": CONFIG_EFFORT_ACTION,
                    "options": [opt("low"), opt("medium"), opt("high"), opt("xhigh"), opt("max")]
                }
            }
        ]
    })
}

/// A planning-conversation reply, posted threaded: the orchestrator's prose
/// plus the standing "how to continue" context line. Used for both a planning
/// turn's reply and a NotReady `/kranz plan` outcome (which is the same thing:
/// the orchestrator talking back instead of emitting a plan).
///
/// `plan_spotted` inserts an explicit next-step callout: the orchestrator
/// chatted out what looks like a complete plan, which users reasonably read
/// as approvable — it isn't (conversation can't approve; only the formal
/// request-plan validates, prices, and arms the approve buttons).
pub fn build_planning_reply(mission_id: &str, reply: &str, plan_spotted: bool) -> Vec<Value> {
    let mut blocks = vec![section(&format!("*Orchestrator*\n{}", clip(reply.trim())))];
    if plan_spotted {
        blocks.push(section(&format!(
            ":bulb: That looks like a complete plan — but a plan in chat can't be \
             approved. Run `/kranz plan {mission_id}` from the channel to validate \
             and price it; the approve buttons arrive with that message."
        )));
    }
    blocks.push(context(&format!(
        "Reply in this thread to continue planning · `/kranz plan {mission_id}` \
         from the channel when you're ready to review the plan."
    )));
    blocks
}

/// Status summary: a header carrying the mission id + status pill, then the
/// caller's pre-rendered status body.
pub fn build_status(s: &StatusSummary) -> Vec<Value> {
    vec![
        header(&format!("{} — {}", s.mission_id, s.status)),
        section(&clip(s.summary.trim())),
        context(&format!("mission `{}`", s.mission_id)),
    ]
}

/// `/kranz status` global snapshot: running mission, queue depth, canonical
/// stage counts, and the Delivered-but-unmerged set.
pub fn build_pipeline_status(s: &PipelineStatusSnapshot) -> Vec<Value> {
    let running = match &s.running {
        Some(r) => {
            let mut line = format!(
                "`{}` · *{}* · {}",
                escape_mrkdwn(&r.mission_id),
                escape_mrkdwn(&r.status),
                escape_mrkdwn(r.title.trim())
            );
            if let Some(cost) = r.cost_usd {
                line.push_str(&format!(" · cost ${cost:.2}"));
            }
            line
        }
        None => "_No mission is running._".to_string(),
    };

    let stages = s
        .stage_counts
        .iter()
        .map(|c| format!("{} {}", c.stage.label(), c.count))
        .collect::<Vec<_>>()
        .join(" · ");

    let mut blocks = vec![
        header(":bar_chart: Kranz status"),
        section(&format!(
            "*Running*\n{}\n\n*Queue*\n{} waiting\n\n*Stages*\n{}",
            running, s.queue_depth, stages
        )),
    ];

    if s.unmerged.is_empty() {
        blocks.push(context("*UNMERGED* none"));
    } else {
        let mut body = String::new();
        for item in &s.unmerged {
            body.push_str(&format!(
                "• `{}` — {}",
                escape_mrkdwn(&item.mission_id),
                escape_mrkdwn(item.title.trim())
            ));
            if let Some(slug) = &item.ticket_slug {
                body.push_str(&format!(" _(ticket `{}`)_", escape_mrkdwn(slug)));
            }
            body.push('\n');
        }
        blocks.push(section(&format!("*UNMERGED*\n{}", clip(body.trim_end()))));
    }
    blocks
}

/// `/kranz ticket list` reply: one compact line per ticket (slug, priority,
/// state, title, blocked-by note), or a friendly empty-backlog line.
pub fn build_ticket_list(rows: &[TicketRow]) -> Vec<Value> {
    if rows.is_empty() {
        return vec![section(
            "_No backlog tickets yet. File one with_ `/kranz ticket <title>`.",
        )];
    }
    let mut body = String::new();
    for row in rows {
        body.push_str(&format!(
            "• `{}` (p{}) — *{}* — {}",
            row.slug, row.priority, row.state, row.title
        ));
        if !row.blocked_by.is_empty() {
            body.push_str(&format!(" _(blocked by: {})_", row.blocked_by.join(", ")));
        }
        body.push('\n');
    }
    vec![
        header(":clipboard: Backlog tickets"),
        section(&clip(body.trim_end())),
    ]
}

/// `/kranz ticket show <slug>` reply: title/goal/state/blocked-by plus any
/// needs-context questions.
pub fn build_ticket_show(t: &TicketDetail) -> Vec<Value> {
    let mut body = format!("*State:* {}\n", t.state);
    if !t.blocked_by.is_empty() {
        body.push_str(&format!("*Blocked by:* {}\n", t.blocked_by.join(", ")));
    }
    let goal = t.goal.trim();
    if !goal.is_empty() {
        body.push_str(&format!("\n*Goal*\n{goal}\n"));
    }
    let mut blocks = vec![
        header(&format!("{} — {}", t.slug, t.title)),
        section(&clip(&body)),
    ];
    if !t.needs_context.is_empty() {
        let mut q = String::from("*Needs context*\n");
        for question in &t.needs_context {
            q.push_str("• ");
            q.push_str(question);
            q.push('\n');
        }
        blocks.push(section(&clip(q.trim_end())));
    }
    blocks
}

/// `/kranz todo`: operator worklist in two sections. The inputs are fixed
/// state: no repo reads, no clock, no model/engine calls.
pub fn build_operator_todo(todo: &OperatorTodo) -> Vec<Value> {
    let mut blocks = vec![header(":white_check_mark: Kranz todo")];

    blocks.push(section("*PIPELINE ACTIONS*"));
    if todo.pipeline_actions.is_empty() {
        blocks.push(context("_No pipeline actions are waiting on you._"));
    } else {
        for action in &todo.pipeline_actions {
            blocks.push(section(&todo_action_text(action)));
            if let Some(button) = todo_button(&action.target) {
                blocks.push(json!({ "type": "actions", "elements": [button] }));
            }
        }
    }

    blocks.push(json!({ "type": "divider" }));
    blocks.push(section("*GATED ITEMS*"));
    if todo.gated_items.is_empty() {
        blocks.push(context(
            "_No gated items listed in_ `docs/operator-gates.md`.",
        ));
    } else {
        let mut body = String::new();
        for item in &todo.gated_items {
            body.push_str("• ");
            body.push_str(&escape_mrkdwn(item.title.trim()));
            body.push('\n');
        }
        blocks.push(section(&clip(body.trim_end())));
    }

    blocks
}

/// `/kranz roadmap`: strategic options grouped by immediacy/trigger. Inputs
/// are fixed state from `docs/roadmap-options.md`; no repo reads here.
pub fn build_roadmap(snapshot: &RoadmapSnapshot) -> Vec<Value> {
    let mut blocks = vec![header(":map: Kranz roadmap")];
    let sections: Vec<&RoadmapSection> = snapshot
        .sections
        .iter()
        .filter(|section| !section.options.is_empty())
        .collect();
    if sections.is_empty() {
        blocks.push(context(
            "_No roadmap options listed in_ `docs/roadmap-options.md`.",
        ));
        return blocks;
    }

    for roadmap_section in sections {
        blocks.push(section(&format!(
            "*{}*",
            escape_mrkdwn(&roadmap_section.name.to_ascii_uppercase())
        )));
        let mut body = String::new();
        for option in &roadmap_section.options {
            body.push_str(&roadmap_option_text(option));
            body.push('\n');
        }
        blocks.push(section(&clip(body.trim_end())));
    }
    blocks
}

fn roadmap_option_text(option: &RoadmapOption) -> String {
    let mut text = format!("• *{}*", escape_mrkdwn(option.title.trim()));
    let summary = option.summary.trim();
    if !summary.is_empty() {
        text.push_str(" - ");
        text.push_str(&escape_mrkdwn(summary));
    }
    let why = option
        .why
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let trigger = option
        .trigger
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if why.is_some() || trigger.is_some() {
        text.push('\n');
        if let Some(why) = why {
            text.push_str("_Why:_ ");
            text.push_str(&escape_mrkdwn(why));
        }
        if let Some(trigger) = trigger {
            if why.is_some() {
                text.push(' ');
            }
            text.push_str("_Trigger:_ ");
            text.push_str(&escape_mrkdwn(trigger));
        }
    }

    let source = option
        .source
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let ticket = option
        .ticket
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if source.is_some() || ticket.is_some() {
        text.push('\n');
        if let Some(source) = source {
            text.push_str("_Source:_ `");
            text.push_str(&escape_mrkdwn(source));
            text.push('`');
        }
        if let Some(ticket) = ticket {
            if source.is_some() {
                text.push(' ');
            }
            text.push_str("_Ticket:_ `");
            text.push_str(&escape_mrkdwn(ticket));
            text.push('`');
        }
    }

    text
}

fn todo_action_text(action: &TodoAction) -> String {
    let mut text = format!(
        "*{}* `{}` — {}",
        action.kind.label(),
        escape_mrkdwn(&action.id),
        escape_mrkdwn(action.title.trim())
    );
    if let Some(note) = action
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        text.push('\n');
        text.push('_');
        text.push_str(&escape_mrkdwn(note));
        text.push('_');
    }
    clip(&text)
}

fn todo_button(target: &TodoTarget) -> Option<Value> {
    match target {
        TodoTarget::QueueTicket { slug } => Some(json!({
            "type": "button",
            "style": "primary",
            "text": { "type": "plain_text", "text": "Queue" },
            "action_id": QUEUE_TICKET_ACTION_ID,
            "value": slug,
        })),
        TodoTarget::MergeMission { mission_id } => Some(json!({
            "type": "button",
            "style": "primary",
            "text": { "type": "plain_text", "text": "Merge" },
            "action_id": MERGE_ACTION_ID,
            "value": mission_id,
        })),
        TodoTarget::OpenUrl { label, url } => Some(json!({
            "type": "button",
            "text": { "type": "plain_text", "text": label },
            "url": url,
            "action_id": "kranz_todo_open",
        })),
        TodoTarget::None => None,
    }
}

/// Plan-review message: goal + milestones + assertion count + optional
/// estimate, then an actions block with **Approve & start** ([`START_ACTION_ID`])
/// and **Approve & queue** ([`APPROVE_ACTION_ID`]) buttons, both carrying the
/// mission id in `value`.
pub fn build_plan_review(p: &PlanReview) -> Vec<Value> {
    let milestones = milestone_bullets(&p.milestone_titles);

    let mut blocks = vec![
        header(&format!("Review plan — {}", p.mission_id)),
        section(&format!("*Goal*\n{}", clip(&escape_mrkdwn(p.goal.trim())))),
        section(&format!("*Milestones*\n{}", clip(milestones.trim_end()))),
    ];
    if let Some(alternatives) = &p.considered_alternatives {
        let mut body = format!("*Chosen:* {}", alternatives.chosen.trim());
        if !alternatives.rejected.is_empty() {
            body.push_str("\n*Rejected:*");
            for rejected in &alternatives.rejected {
                body.push_str(&format!(
                    "\n• {} — {}",
                    rejected.approach.trim(),
                    rejected.trade_off.trim()
                ));
            }
        }
        blocks.push(section(&format!(
            "*Considered alternatives*\n{}",
            clip(&body)
        )));
    }
    let mut meta = format!(
        "{} validation assertion{} · mission `{}`",
        p.assertion_count,
        plural(p.assertion_count),
        p.mission_id
    );
    if let Some(est) = p
        .estimate
        .as_deref()
        .map(str::trim)
        .filter(|e| !e.is_empty())
    {
        meta.push_str(&format!(" · {est}"));
    }
    blocks.push(context(&meta));
    blocks.push(json!({
        "type": "actions",
        "elements": [
            {
                "type": "button",
                "style": "primary",
                "text": { "type": "plain_text", "text": "Approve & start" },
                "action_id": START_ACTION_ID,
                "value": p.mission_id,
            },
            {
                "type": "button",
                "text": { "type": "plain_text", "text": "Approve & queue" },
                "action_id": APPROVE_ACTION_ID,
                "value": p.mission_id,
            }
        ]
    }));
    blocks
}

/// `/kranz help` reply — the command list. Honest about what works TODAY: the
/// bridge currently drives tickets + interactive steering; the full lifecycle
/// (new/plan/approve/start from Slack) is M2.9 and this list grows as it lands.
pub fn build_help() -> Vec<Value> {
    vec![
        header(":sparkles: Kranz — Slack commands"),
        section(
            "*Slash commands*\n\
             • `/kranz new <goal>` — create a mission and open its planning thread \
             (bare `/kranz new` opens a form with a full multiline goal field)\n\
             • `/kranz plan <id>` — request the plan for review\n\
             • `/kranz approve <id>` — approve the plan and queue the mission\n\
             • `/kranz revise <id> <instructions>` — request a mid-mission plan revision\n\
             • `/kranz revision approve|reject <id> <rev>` — decide a proposed revision\n\
             • `/kranz queue <slug>` — queue a reviewed backlog ticket\n\
             • `/kranz config [<id>] <role> [backend] <model> [effort]` — change a role's lane \
             (backends: claude·codex·droid; roles: orchestrator·worker·scrutiny·functional)\n\
             • `/kranz pause [<id>]` — pause a running mission (between worker runs)\n\
             • `/kranz resume [<id>]` — resume a paused mission\n\
             • `/kranz work` — show the execution queue (drain it with the `kranz work` dispatcher)\n\
             • `/kranz work run` — trigger the queue drain through the host (progress posts per mission)\n\
             • `/kranz status` — show the pipeline snapshot; `/kranz status <id>` shows one mission\n\
             • `/kranz todo` — show operator pipeline actions and human-only gates\n\
             • `/kranz roadmap` — show strategic options grouped by trigger/status\n\
             • `/kranz ask <question>` — ask a grounded, read-only question about mission/ticket state\n\
             • `/kranz ticket <title>` — file a new backlog ticket\n\
             • `/kranz ticket list` — list backlog tickets\n\
             • `/kranz ticket show <slug>` — show a ticket's detail\n\
             • `/kranz help` — show this message",
        ),
        section(
            "*In a mission thread*\n\
             • *Approve & start* / *Approve & queue* buttons on a plan-review message\n\
             • *Approve revision* / *Reject revision* buttons on a revision message\n\
             • *Reply in the thread* — during planning your message is a planning turn; \
             on a running mission it becomes orchestrator guidance \
             (unblocks a blocked milestone, steers a running one)",
        ),
        context(
            "Money-spending actions (`new` · `plan` · `approve` · `config`) and disruptive \
             steering (`pause` · `resume`) are gated by the `slack.allowUsers` allowlist. \
             Deep forensic inspection (full transcripts, the four-pane live view) lives in \
             the web UI via `kranz serve --open`.",
        ),
    ]
}

// -- App Home tab -----------------------------------------------------------

/// One active mission row for the App Home dashboard: id + a status word for the
/// pill. Folded read-only from the mission's event log by the bridge.
#[derive(Debug, Clone)]
pub struct HomeMission {
    pub mission_id: String,
    /// Short status word (e.g. `Running`, `Planning`) — the reducer's status.
    pub status: String,
}

/// One queued mission row: id + priority (lower = sooner).
#[derive(Debug, Clone)]
pub struct HomeQueueItem {
    pub mission_id: String,
    pub priority: u8,
}

/// One open ticket row: slug + title + pipeline state word.
#[derive(Debug, Clone)]
pub struct HomeTicket {
    pub slug: String,
    pub title: String,
    /// Pipeline state word (e.g. `New`, `Drafting`, `NeedsContext`).
    pub state: String,
}

/// Build the Block Kit **home** view object (the payload for `views.publish`):
/// active missions (id + status pill), the queue, and open tickets — a
/// lightweight Mission Control inside Slack. Pure `data -> Value`; the bridge
/// folds the inputs read-only from the repo and publishes the result.
///
/// The returned value is a full view object (`{"type":"home","blocks":[…]}`),
/// ready to hand to [`crate::client::SlackClient::publish_home_view`]. When
/// `dashboard_url` is set, each mission row carries an "Open" deep-link to the
/// web UI; when `None`, the rows render without links.
pub fn build_home_view(
    missions: &[HomeMission],
    queue: &[HomeQueueItem],
    tickets: &[HomeTicket],
    dashboard_url: Option<&str>,
) -> Value {
    let mut blocks = vec![header(":satellite_antenna: Kranz — Mission Control")];

    // -- Active missions --
    blocks.push(section("*Active missions*"));
    if missions.is_empty() {
        blocks.push(context(
            "_No active missions. Create one with_ `/kranz new <goal>`.",
        ));
    } else {
        for m in missions {
            let line = format!("`{}` · *{}*", m.mission_id, m.status);
            match dashboard_url.map(str::trim).filter(|u| !u.is_empty()) {
                Some(url) => blocks.push(section_with_link(
                    &line,
                    "Open",
                    &dashboard_deep_link(url, &m.mission_id),
                    &format!("kranz_home_open_{}", m.mission_id),
                )),
                None => blocks.push(section(&line)),
            }
        }
    }

    blocks.push(json!({ "type": "divider" }));

    // -- Queue --
    blocks.push(section("*Queue*"));
    if queue.is_empty() {
        blocks.push(context("_The execution queue is empty._"));
    } else {
        let mut body = String::new();
        for (i, q) in queue.iter().enumerate() {
            body.push_str(&format!(
                "{}. `{}` · priority {}\n",
                i + 1,
                q.mission_id,
                q.priority
            ));
        }
        blocks.push(section(&clip(body.trim_end())));
    }

    blocks.push(json!({ "type": "divider" }));

    // -- Open tickets --
    blocks.push(section("*Open tickets*"));
    if tickets.is_empty() {
        blocks.push(context(
            "_No open tickets. File one with_ `/kranz ticket <title>`.",
        ));
    } else {
        let mut body = String::new();
        for t in tickets {
            body.push_str(&format!(
                "• `{}` — {} · _{}_\n",
                t.slug,
                t.title.trim(),
                t.state
            ));
        }
        blocks.push(section(&clip(body.trim_end())));
    }

    json!({ "type": "home", "blocks": blocks })
}

/// A `section` block with a trailing link-button `accessory` (a URL button, so
/// it opens the browser directly and needs no inbound routing).
fn section_with_link(mrkdwn: &str, label: &str, url: &str, action_id: &str) -> Value {
    json!({
        "type": "section",
        "text": { "type": "mrkdwn", "text": mrkdwn },
        "accessory": {
            "type": "button",
            "text": { "type": "plain_text", "text": label },
            "url": url,
            "action_id": action_id,
        }
    })
}

// -- block primitives -------------------------------------------------------

fn header(text: &str) -> Value {
    // header blocks only accept plain_text and cap at 150 chars.
    json!({ "type": "header", "text": { "type": "plain_text", "text": clip_to(text, MAX_HEADER) } })
}

fn section(mrkdwn: &str) -> Value {
    json!({ "type": "section", "text": { "type": "mrkdwn", "text": mrkdwn } })
}

fn context(text: &str) -> Value {
    json!({ "type": "context", "elements": [{ "type": "mrkdwn", "text": text }] })
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Clip a field to the default field cap.
fn clip(s: &str) -> String {
    clip_to(s, MAX_FIELD)
}

/// Clip on a char boundary, appending an ellipsis when truncated.
/// `pub(crate)` so the bridge's reply builders share this one helper — a
/// second local truncation (with its own constant) would silently drift.
pub(crate) fn clip_to(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let take = max.saturating_sub(1);
    let mut out: String = s.chars().take(take).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Concatenate all `text` fields anywhere in a blocks array so a test can
    /// assert on rendered content without walking the exact block shape.
    fn all_text(blocks: &[Value]) -> String {
        fn walk(v: &Value, out: &mut String) {
            match v {
                Value::Object(map) => {
                    for (k, val) in map {
                        if k == "text" {
                            if let Some(s) = val.as_str() {
                                out.push_str(s);
                                out.push('\n');
                            }
                        }
                        walk(val, out);
                    }
                }
                Value::Array(items) => {
                    for item in items {
                        walk(item, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = String::new();
        for b in blocks {
            walk(b, &mut out);
        }
        out
    }

    #[test]
    fn plan_ready_announcement_has_facts_and_approve_buttons() {
        let blocks = build_plan_ready(
            &PlanReady {
                mission_id: "m-42".into(),
                goal: "Rate-limit the notes API".into(),
                milestone_titles: vec!["Token bucket".into(), "429 responses".into()],
                assertion_count: 3,
            },
            None,
        );
        let text = all_text(&blocks);
        assert!(text.contains("m-42"), "mission id present");
        assert!(text.contains("Rate-limit the notes API"), "goal present");
        assert!(text.contains("Token bucket"), "milestone 1 present");
        assert!(text.contains("429 responses"), "milestone 2 present");
        assert!(
            text.contains("3 validation assertions"),
            "assertion count present"
        );
        assert!(
            text.contains("Plan ready for review"),
            "asks for review, not claiming the human already approved"
        );

        // Draft-path surface: same affordances as build_plan_review.
        let buttons = all_buttons(&blocks);
        let ids: Vec<&str> = buttons
            .iter()
            .filter_map(|b| b["action_id"].as_str())
            .collect();
        assert!(
            ids.contains(&APPROVE_ACTION_ID),
            "Approve & queue button present: {ids:?}"
        );
        assert!(
            ids.contains(&START_ACTION_ID),
            "Approve & start button present: {ids:?}"
        );
    }

    #[test]
    fn grant_ready_names_the_command_with_approve_deny_buttons() {
        let blocks = build_grant_ready(
            &GrantReady {
                mission_id: "m-42".into(),
                milestone_id: "ms-1".into(),
                kind: kranz_engine::types::GrantKind::Command,
                command: "gc audit --deep".into(),
            },
            None,
        );
        let text = all_text(&blocks);
        assert!(text.contains("m-42"), "mission id present");
        assert!(text.contains("ms-1"), "milestone present");
        assert!(
            text.contains("gc audit --deep"),
            "the blocked command is named"
        );
        assert!(text.contains("Grant requested"), "asks for a decision");

        let buttons = all_buttons(&blocks);
        let ids: Vec<&str> = buttons
            .iter()
            .filter_map(|b| b["action_id"].as_str())
            .collect();
        assert!(
            ids.contains(&APPROVE_GRANT_ACTION_ID),
            "approve button: {ids:?}"
        );
        assert!(ids.contains(&DENY_GRANT_ACTION_ID), "deny button: {ids:?}");
        // The button value carries `<mission-id>:<command>` so the inbound
        // router can round-trip it back to the exact parked request.
        let values: Vec<&str> = buttons.iter().filter_map(|b| b["value"].as_str()).collect();
        assert!(
            values.contains(&"m-42:gc audit --deep"),
            "button value round-trips: {values:?}"
        );
    }

    #[test]
    fn grant_ready_labels_a_touch_path_grant() {
        let blocks = build_grant_ready(
            &GrantReady {
                mission_id: "m-9".into(),
                milestone_id: "ms-2".into(),
                kind: kranz_engine::types::GrantKind::TouchPath,
                command: "docs/report.md".into(),
            },
            None,
        );
        let text = all_text(&blocks);
        assert!(text.contains("docs/report.md"), "the path is named");
        assert!(
            text.contains("touch-set"),
            "card frames it as a touch-set write, not a command: {text}"
        );
        // Same approve/deny affordances as a command grant.
        let buttons = all_buttons(&blocks);
        let ids: Vec<&str> = buttons
            .iter()
            .filter_map(|b| b["action_id"].as_str())
            .collect();
        assert!(ids.contains(&APPROVE_GRANT_ACTION_ID));
        assert!(ids.contains(&DENY_GRANT_ACTION_ID));
    }

    #[test]
    fn plan_ready_single_assertion_is_singular() {
        let blocks = build_plan_ready(
            &PlanReady {
                mission_id: "m-1".into(),
                goal: "g".into(),
                milestone_titles: vec![],
                assertion_count: 1,
            },
            None,
        );
        assert!(all_text(&blocks).contains("1 validation assertion "));
    }

    #[test]
    fn needs_context_lists_questions() {
        let blocks = build_needs_context(&NeedsContext {
            ticket_slug: "rate-limit".into(),
            questions: vec![
                "What is the test command?".into(),
                "Which endpoints are in scope?".into(),
            ],
        });
        let text = all_text(&blocks);
        assert!(text.contains("rate-limit"), "slug present");
        assert!(text.contains("What is the test command?"));
        assert!(text.contains("Which endpoints are in scope?"));
        assert!(text.to_lowercase().contains("reply in this thread"));
    }

    #[test]
    fn blocked_carries_reason_and_unblock_hint() {
        let blocks = build_blocked(
            &Blocked {
                mission_id: "m-7".into(),
                milestone_id: "ms-2".into(),
                reason: "fix-cycle cap exceeded after 2 rounds".into(),
            },
            None,
        );
        let text = all_text(&blocks);
        assert!(text.contains("m-7"), "mission id present");
        assert!(text.contains("ms-2"), "milestone id present");
        assert!(text.contains("fix-cycle cap exceeded"), "reason present");
        assert!(text
            .to_lowercase()
            .contains("reply in this thread to unblock"));
    }

    #[test]
    fn complete_shows_branch_summary_and_cost() {
        let blocks = build_complete(
            &Complete {
                mission_id: "m-9".into(),
                outcome: Outcome::Completed,
                summary: "Added rate limiting; all tests pass.".into(),
                branch: "kranz/mission-m-9".into(),
                cost_usd: Some(4.2),
                diff_stat: None,
                pr_handoff: None,
            },
            None,
        );
        let text = all_text(&blocks);
        assert!(text.contains("m-9"), "mission id present");
        assert!(text.contains("completed"), "outcome verb present");
        assert!(text.contains("Added rate limiting"), "summary present");
        assert!(text.contains("kranz/mission-m-9"), "branch present");
        assert!(text.contains("$4.20"), "cost rendered to cents");
    }

    #[test]
    fn delivered_card_carries_diff_stat_deep_link_and_merge_button() {
        let blocks = build_complete(
            &Complete {
                mission_id: "m-9".into(),
                outcome: Outcome::Completed,
                summary: "Added rate limiting; all tests pass.".into(),
                branch: "kranz/mission-m-9".into(),
                cost_usd: Some(4.2),
                diff_stat: Some(" 2 files changed, 40 insertions(+), 3 deletions(-)".into()),
                pr_handoff: None,
            },
            Some("http://dash"),
        );
        let text = all_text(&blocks);
        assert!(
            text.contains("Added rate limiting"),
            "report summary present"
        );
        assert!(
            text.contains("2 files changed, 40 insertions(+), 3 deletions(-)"),
            "diff stat present: {text}"
        );
        assert_eq!(
            all_button_urls(&blocks),
            vec!["http://dash/#/m/m-9".to_string()],
            "dashboard deep-link button present"
        );
        let merge = all_buttons(&blocks)
            .into_iter()
            .find(|b| b["action_id"] == MERGE_ACTION_ID)
            .expect("merge button present");
        assert_eq!(merge["value"], "m-9");
    }

    #[test]
    fn failed_mission_reads_failed_and_omits_cost_when_absent() {
        let blocks = build_complete(
            &Complete {
                mission_id: "m-9".into(),
                outcome: Outcome::Failed,
                summary: "worker exhausted respawns".into(),
                branch: "kranz/mission-m-9".into(),
                cost_usd: None,
                diff_stat: None,
                pr_handoff: None,
            },
            None,
        );
        let text = all_text(&blocks);
        assert!(text.contains("failed"), "failure verb present");
        assert!(text.contains("worker exhausted respawns"));
        assert!(!text.contains("cost $"), "no cost line when unknown");
        assert!(
            all_buttons(&blocks)
                .iter()
                .all(|b| b["action_id"] != MERGE_ACTION_ID),
            "a failed mission has nothing to merge"
        );
    }

    #[test]
    fn long_fields_are_clipped_with_ellipsis() {
        let long = "x".repeat(5000);
        let blocks = build_blocked(
            &Blocked {
                mission_id: "m".into(),
                milestone_id: "ms".into(),
                reason: long,
            },
            None,
        );
        let text = all_text(&blocks);
        assert!(text.contains('…'), "clipped fields end with an ellipsis");
        // The clipped field itself must stay under the cap; the assembled
        // section adds a fixed template prefix, so allow a small template
        // margin on top of the field cap.
        const TEMPLATE_MARGIN: usize = 64;
        for b in &blocks {
            if let Some(t) = b.pointer("/text/text").and_then(Value::as_str) {
                assert!(
                    t.chars().count() <= MAX_FIELD + TEMPLATE_MARGIN,
                    "section stays near the field cap (was {})",
                    t.chars().count()
                );
            }
        }
    }

    /// All `button` elements across every `actions` block.
    fn all_buttons(blocks: &[Value]) -> Vec<Value> {
        let mut out = Vec::new();
        for b in blocks {
            if b["type"] == "actions" {
                if let Some(elems) = b["elements"].as_array() {
                    for e in elems {
                        if e["type"] == "button" {
                            out.push(e.clone());
                        }
                    }
                }
            }
        }
        out
    }

    #[test]
    fn new_mission_ack_carries_id_goal_and_opening_reply() {
        let blocks = build_new_mission_ack(&NewMissionAck {
            mission_id: "m-42".into(),
            goal: "Rate-limit the notes API".into(),
            opening_reply: Some("What is the test command?".into()),
        });
        let text = all_text(&blocks);
        assert!(text.contains("m-42"), "mission id present");
        assert!(text.contains("Rate-limit the notes API"), "goal present");
        assert!(
            text.contains("What is the test command?"),
            "opening reply present"
        );
        assert!(text.to_lowercase().contains("reply in this thread"));
        assert!(text.contains("/kranz plan"), "nudges toward request-plan");
    }

    #[test]
    fn new_mission_ack_without_reply_still_nudges_planning() {
        let blocks = build_new_mission_ack(&NewMissionAck {
            mission_id: "m-1".into(),
            goal: "g".into(),
            opening_reply: None,
        });
        let text = all_text(&blocks);
        assert!(text.contains("m-1"));
        assert!(text.to_lowercase().contains("planning turn"));
        // No empty "Orchestrator" section when there's no reply.
        assert!(!text.contains("*Orchestrator*"));
    }

    #[test]
    fn status_frames_the_summary_with_id_and_pill() {
        let blocks = build_status(&StatusSummary {
            mission_id: "m-7".into(),
            status: "Running".into(),
            summary: "2/3 milestones complete\ncost $1.20".into(),
        });
        let text = all_text(&blocks);
        assert!(text.contains("m-7"), "mission id present");
        assert!(text.contains("Running"), "status pill present");
        assert!(
            text.contains("2/3 milestones complete"),
            "summary body present"
        );
        assert!(text.contains("cost $1.20"));
    }

    #[test]
    fn pipeline_status_renders_fixed_snapshot_counts_and_unmerged() {
        let blocks = build_pipeline_status(&PipelineStatusSnapshot {
            running: Some(RunningMissionSnapshot {
                mission_id: "m-run".into(),
                title: "Ship the Slack bridge".into(),
                status: "Running".into(),
                cost_usd: Some(1.25),
            }),
            queue_depth: 2,
            stage_counts: PipelineStage::ALL
                .iter()
                .map(|stage| StageCount {
                    stage: *stage,
                    count: match stage {
                        PipelineStage::Captured => 1,
                        PipelineStage::NeedsYou => 1,
                        PipelineStage::Reviewable => 2,
                        PipelineStage::Running => 1,
                        PipelineStage::Delivered => 1,
                        _ => 0,
                    },
                })
                .collect(),
            unmerged: vec![UnmergedMission {
                mission_id: "m-delivered".into(),
                title: "Ready to land".into(),
                ticket_slug: Some("ready-ticket".into()),
            }],
        });

        let text = all_text(&blocks);
        assert!(text.contains("Kranz status"));
        assert!(text.contains("m-run"));
        assert!(text.contains("cost $1.25"));
        assert!(text.contains("2 waiting"));
        assert!(text.contains("captured 1"));
        assert!(text.contains("needs-you 1"));
        assert!(text.contains("reviewable 2"));
        assert!(text.contains("delivered 1"));
        assert!(text.contains("m-delivered"));
        assert!(text.contains("ready-ticket"));
    }

    #[test]
    fn operator_todo_renders_fixed_pipeline_actions_gates_and_buttons() {
        let blocks = build_operator_todo(&OperatorTodo {
            pipeline_actions: vec![
                TodoAction {
                    kind: TodoActionKind::Reviewable,
                    id: "review-ticket".into(),
                    title: "Approve this plan".into(),
                    note: Some("Queue reviewed plan for run.".into()),
                    target: TodoTarget::QueueTicket {
                        slug: "review-ticket".into(),
                    },
                },
                TodoAction {
                    kind: TodoActionKind::Delivered,
                    id: "m-delivered".into(),
                    title: "Merge this report".into(),
                    note: None,
                    target: TodoTarget::MergeMission {
                        mission_id: "m-delivered".into(),
                    },
                },
                TodoAction {
                    kind: TodoActionKind::NeedsYou,
                    id: "needs-context".into(),
                    title: "Answer drafter questions".into(),
                    note: Some("Answer questions, then redraft.".into()),
                    target: TodoTarget::OpenUrl {
                        label: "Open ticket".into(),
                        url: "http://dash/#/backlog/needs-context".into(),
                    },
                },
            ],
            gated_items: vec![
                GateItem {
                    title: "repo public + history scrub".into(),
                },
                GateItem {
                    title: "M6 live deploy".into(),
                },
            ],
        });

        let text = all_text(&blocks);
        assert!(text.contains("PIPELINE ACTIONS"));
        assert!(text.contains("REVIEWABLE"));
        assert!(text.contains("DELIVERED"));
        assert!(text.contains("NEEDS-YOU"));
        assert!(text.contains("GATED ITEMS"));
        assert!(text.contains("repo public + history scrub"));

        let buttons = all_buttons(&blocks);
        assert_eq!(buttons.len(), 3);
        assert!(buttons
            .iter()
            .any(|b| b["action_id"] == QUEUE_TICKET_ACTION_ID && b["value"] == "review-ticket"));
        assert!(buttons
            .iter()
            .any(|b| b["action_id"] == MERGE_ACTION_ID && b["value"] == "m-delivered"));
        assert!(buttons
            .iter()
            .any(|b| b["url"] == "http://dash/#/backlog/needs-context"));
    }

    #[test]
    fn roadmap_renders_sections_options_and_metadata() {
        let blocks = build_roadmap(&RoadmapSnapshot {
            sections: vec![
                RoadmapSection {
                    name: "Now".into(),
                    options: vec![RoadmapOption {
                        title: "Core safety".into(),
                        summary: "finish reliability polish".into(),
                        why: Some("protects mission trust".into()),
                        trigger: Some("pick before demos".into()),
                        source: Some("docs/roadmap.md".into()),
                        ticket: Some("stale-base-merge-warning".into()),
                    }],
                },
                RoadmapSection {
                    name: "Parked/demo".into(),
                    options: vec![RoadmapOption {
                        title: "Even Realities".into(),
                        summary: "demo lane".into(),
                        why: None,
                        trigger: Some("after ecosystem work".into()),
                        source: None,
                        ticket: Some("none".into()),
                    }],
                },
            ],
        });

        let text = all_text(&blocks);
        assert!(text.contains("Kranz roadmap"));
        assert!(text.contains("NOW"));
        assert!(text.contains("Core safety"));
        assert!(text.contains("finish reliability polish"));
        assert!(text.contains("protects mission trust"));
        assert!(text.contains("pick before demos"));
        assert!(text.contains("docs/roadmap.md"));
        assert!(text.contains("stale-base-merge-warning"));
        assert!(text.contains("PARKED/DEMO"));
        assert!(text.contains("Even Realities"));
    }

    #[test]
    fn new_mission_modal_carries_channel_and_multiline_goal_input() {
        let view = build_new_mission_modal("C0BF6SAJLJ0");
        assert_eq!(view["type"], "modal");
        assert_eq!(view["callback_id"], NEW_MISSION_CALLBACK_ID);
        // The invoking channel rides in private_metadata: a view_submission
        // carries no channel, and the planning thread must land where the
        // command was issued.
        assert_eq!(view["private_metadata"], "C0BF6SAJLJ0");
        let input = &view["blocks"][0];
        assert_eq!(input["block_id"], NEW_MISSION_GOAL_BLOCK);
        assert_eq!(input["element"]["action_id"], NEW_MISSION_GOAL_ACTION);
        assert_eq!(
            input["element"]["multiline"], true,
            "the whole point: a multiline goal field"
        );
    }

    #[test]
    fn config_modal_carries_a_backend_picker_defaulting_to_claude() {
        let view = build_config_modal("C0BF6SAJLJ0");
        let backend = view["blocks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|block| block["block_id"] == CONFIG_BACKEND_BLOCK)
            .expect("backend input");
        assert_eq!(backend["element"]["action_id"], CONFIG_BACKEND_ACTION);
        assert_eq!(backend["element"]["initial_option"]["value"], "claude");
        let values: Vec<&str> = backend["element"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|option| option["value"].as_str())
            .collect();
        assert_eq!(values, vec!["claude", "codex", "droid"]);
    }

    #[test]
    fn plan_review_has_two_buttons_carrying_mission_id() {
        let blocks = build_plan_review(&PlanReview {
            mission_id: "m-42".into(),
            goal: "Rate-limit the notes API".into(),
            milestone_titles: vec!["Token bucket".into(), "429 responses".into()],
            assertion_count: 3,
            considered_alternatives: None,
            estimate: Some("~$4.50 · ~12 min".into()),
        });
        let text = all_text(&blocks);
        assert!(text.contains("m-42"), "mission id present");
        assert!(text.contains("Rate-limit the notes API"), "goal present");
        assert!(text.contains("Token bucket") && text.contains("429 responses"));
        assert!(
            text.contains("3 validation assertions"),
            "assertion count present"
        );
        assert!(text.contains("~$4.50 · ~12 min"), "estimate rendered");

        let buttons = all_buttons(&blocks);
        assert_eq!(buttons.len(), 2, "approve & start plus approve & queue");
        let start = buttons
            .iter()
            .find(|b| b["action_id"] == START_ACTION_ID)
            .expect("start btn");
        let queue = buttons
            .iter()
            .find(|b| b["action_id"] == APPROVE_ACTION_ID)
            .expect("queue btn");
        assert_eq!(start["value"], "m-42");
        assert_eq!(queue["value"], "m-42");
    }

    #[test]
    fn plan_review_omits_estimate_when_absent() {
        let blocks = build_plan_review(&PlanReview {
            mission_id: "m-1".into(),
            goal: "g".into(),
            milestone_titles: vec![],
            assertion_count: 1,
            considered_alternatives: None,
            estimate: None,
        });
        let text = all_text(&blocks);
        assert!(
            text.contains("1 validation assertion "),
            "singular assertion"
        );
        assert!(text.contains("no milestones listed"));
    }

    // -- deep-link buttons --------------------------------------------------

    #[test]
    fn dashboard_deep_link_shape_with_and_without_trailing_slash() {
        assert_eq!(
            dashboard_deep_link("http://127.0.0.1:4600", "m-42"),
            "http://127.0.0.1:4600/#/m/m-42"
        );
        // A trailing slash on the base is collapsed to exactly one.
        assert_eq!(
            dashboard_deep_link("http://127.0.0.1:4600/", "m-42"),
            "http://127.0.0.1:4600/#/m/m-42"
        );
        assert_eq!(
            dashboard_deep_link(
                &dashboard_repo_url("http://127.0.0.1:4600/", "alpha"),
                "m-42"
            ),
            "http://127.0.0.1:4600/#/r/alpha/m/m-42"
        );
    }

    /// Extract every `url` field found on any button element in the blocks.
    fn all_button_urls(blocks: &[Value]) -> Vec<String> {
        let mut out = Vec::new();
        for b in blocks {
            if b["type"] == "actions" {
                if let Some(elems) = b["elements"].as_array() {
                    for e in elems {
                        if let Some(url) = e["url"].as_str() {
                            out.push(url.to_string());
                        }
                    }
                }
            }
        }
        out
    }

    #[test]
    fn dashboard_button_present_only_when_url_set() {
        // None → no button.
        assert!(dashboard_button(None, "m-1").is_none());
        // Blank → treated as unset.
        assert!(dashboard_button(Some("   "), "m-1").is_none());
        // Set → a link button whose url is the deep link.
        let btn = dashboard_button(Some("http://127.0.0.1:4600"), "m-1").expect("button");
        let elem = &btn["elements"][0];
        assert_eq!(elem["url"], "http://127.0.0.1:4600/#/m/m-1");
        assert_eq!(elem["action_id"], "kranz_open_dashboard");
        assert!(
            elem.get("value").is_none(),
            "link buttons carry a url, not a value"
        );
    }

    #[test]
    fn plan_ready_adds_dashboard_button_only_when_url_set() {
        let p = PlanReady {
            mission_id: "m-42".into(),
            goal: "g".into(),
            milestone_titles: vec![],
            assertion_count: 1,
        };
        // Unset → no link button (only the approve button, which has no url).
        let blocks = build_plan_ready(&p, None);
        assert!(
            all_button_urls(&blocks).is_empty(),
            "no deep link when unset"
        );
        // Set → exactly one deep-link url, correctly shaped.
        let blocks = build_plan_ready(&p, Some("http://127.0.0.1:4600"));
        assert_eq!(
            all_button_urls(&blocks),
            vec!["http://127.0.0.1:4600/#/m/m-42".to_string()]
        );
    }

    #[test]
    fn blocked_and_complete_carry_dashboard_link_when_set() {
        let blocked = build_blocked(
            &Blocked {
                mission_id: "m-7".into(),
                milestone_id: "ms-1".into(),
                reason: "x".into(),
            },
            Some("http://dash/"),
        );
        assert_eq!(
            all_button_urls(&blocked),
            vec!["http://dash/#/m/m-7".to_string()]
        );

        let complete = build_complete(
            &Complete {
                mission_id: "m-9".into(),
                outcome: Outcome::Completed,
                summary: "done".into(),
                branch: "b".into(),
                cost_usd: None,
                diff_stat: None,
                pr_handoff: None,
            },
            Some("http://dash"),
        );
        assert_eq!(
            all_button_urls(&complete),
            vec!["http://dash/#/m/m-9".to_string()]
        );
        // And absent when unset.
        let complete_no = build_complete(
            &Complete {
                mission_id: "m-9".into(),
                outcome: Outcome::Completed,
                summary: "done".into(),
                branch: "b".into(),
                cost_usd: None,
                diff_stat: None,
                pr_handoff: None,
            },
            None,
        );
        assert!(all_button_urls(&complete_no).is_empty());
    }

    // -- App Home view ------------------------------------------------------

    #[test]
    fn home_view_lists_missions_queue_and_tickets() {
        let view = build_home_view(
            &[
                HomeMission {
                    mission_id: "m-1".into(),
                    status: "Running".into(),
                },
                HomeMission {
                    mission_id: "m-2".into(),
                    status: "Planning".into(),
                },
            ],
            &[HomeQueueItem {
                mission_id: "m-3".into(),
                priority: 2,
            }],
            &[HomeTicket {
                slug: "rate-limit".into(),
                title: "Rate-limit the notes API".into(),
                state: "New".into(),
            }],
            None,
        );
        assert_eq!(view["type"], "home", "a home view object");
        let blocks = view["blocks"].as_array().expect("home blocks");
        let text = all_text(blocks);
        // Missions with their status pills.
        assert!(text.contains("m-1") && text.contains("Running"));
        assert!(text.contains("m-2") && text.contains("Planning"));
        // Queue entry.
        assert!(text.contains("m-3") && text.contains("priority 2"));
        // Ticket slug + title + state.
        assert!(text.contains("rate-limit"));
        assert!(text.contains("Rate-limit the notes API"));
        assert!(text.contains("New"));
        // The whole view must serialize (it's the views.publish body).
        assert!(serde_json::to_string(&view).is_ok());
    }

    #[test]
    fn home_view_empty_state_is_friendly_and_valid() {
        let view = build_home_view(&[], &[], &[], None);
        let blocks = view["blocks"].as_array().expect("home blocks");
        let text = all_text(blocks);
        assert!(text.to_lowercase().contains("no active missions"));
        assert!(text.to_lowercase().contains("queue is empty"));
        assert!(text.to_lowercase().contains("no open tickets"));
        // Every block still has a recognized type.
        for b in blocks {
            let ty = b["type"].as_str().expect("block type");
            assert!(
                ["header", "section", "context", "divider", "actions"].contains(&ty),
                "unexpected home block type {ty}"
            );
        }
    }

    // -- instance labels ------------------------------------------------------

    #[test]
    fn escape_mrkdwn_escapes_amp_lt_gt_and_nothing_else() {
        assert_eq!(escape_mrkdwn("<&>"), "&lt;&amp;&gt;");
        assert_eq!(escape_mrkdwn("a & b <c> d"), "a &amp; b &lt;c&gt; d");
        // `&` is escaped first, so entities aren't double-escaped into &amp;lt;.
        assert_eq!(escape_mrkdwn("&lt;"), "&amp;lt;");
        assert_eq!(escape_mrkdwn("plain studio-2"), "plain studio-2");
    }

    #[test]
    fn label_blocks_none_is_byte_identical_for_every_builder() {
        // BACKCOMPAT: with no instance name, labeling must be a perfect no-op
        // on every message shape the bridge posts.
        let shapes: Vec<Vec<Value>> = vec![
            build_plan_ready(
                &PlanReady {
                    mission_id: "m-1".into(),
                    goal: "g".into(),
                    milestone_titles: vec!["a".into()],
                    assertion_count: 1,
                },
                Some("http://dash"),
            ),
            build_blocked(
                &Blocked {
                    mission_id: "m-1".into(),
                    milestone_id: "ms".into(),
                    reason: "r".into(),
                },
                None,
            ),
            build_complete(
                &Complete {
                    mission_id: "m-1".into(),
                    outcome: Outcome::Completed,
                    summary: "s".into(),
                    branch: "b".into(),
                    cost_usd: Some(1.0),
                    diff_stat: None,
                    pr_handoff: None,
                },
                None,
            ),
            build_help(),
            build_needs_context(&NeedsContext {
                ticket_slug: "t".into(),
                questions: vec![],
            }),
            build_status(&StatusSummary {
                mission_id: "m-1".into(),
                status: "Running".into(),
                summary: "s".into(),
            }),
            // The single-section ephemeral shape (confirmations / errors).
            vec![
                json!({ "type": "section", "text": { "type": "mrkdwn", "text": "Queued `m-1`." } }),
            ],
        ];
        for blocks in shapes {
            assert_eq!(
                label_blocks(blocks.clone(), None),
                blocks,
                "None must not touch blocks"
            );
            // A blank name is treated as unset, not rendered as `[] `.
            assert_eq!(label_blocks(blocks.clone(), Some("   ")), blocks);
        }
        // Same for the home view object.
        let view = build_home_view(&[], &[], &[], None);
        assert_eq!(label_home_view(view.clone(), None), view);
    }

    #[test]
    fn label_blocks_prefixes_the_header_and_adds_no_blocks() {
        let p = PlanReady {
            mission_id: "m-42".into(),
            goal: "g".into(),
            milestone_titles: vec![],
            assertion_count: 1,
        };
        let unlabeled = build_plan_ready(&p, None);
        let labeled = label_blocks(unlabeled.clone(), Some("studio"));
        // Block count unchanged — the label rides on the existing header.
        assert_eq!(labeled.len(), unlabeled.len(), "labeling never adds blocks");
        let head = labeled[0]
            .pointer("/text/text")
            .and_then(Value::as_str)
            .unwrap();
        assert!(
            head.starts_with("[studio] "),
            "leading prefix on the header: {head}"
        );
        assert!(
            head.contains("Plan ready for review — m-42"),
            "original header text intact"
        );
        // Only the first block was touched.
        assert_eq!(labeled[1..], unlabeled[1..]);
    }

    #[test]
    fn label_blocks_labels_every_notification_help_and_ephemeral_shape() {
        let blocked = label_blocks(
            build_blocked(
                &Blocked {
                    mission_id: "m-7".into(),
                    milestone_id: "ms".into(),
                    reason: "r".into(),
                },
                None,
            ),
            Some("laptop"),
        );
        assert!(all_text(&blocked).contains("[laptop] "), "blocked labeled");

        let complete = label_blocks(
            build_complete(
                &Complete {
                    mission_id: "m-9".into(),
                    outcome: Outcome::Failed,
                    summary: "s".into(),
                    branch: "b".into(),
                    cost_usd: None,
                    diff_stat: None,
                    pr_handoff: None,
                },
                None,
            ),
            Some("laptop"),
        );
        assert!(
            all_text(&complete).contains("[laptop] "),
            "complete labeled"
        );

        // `/kranz help` states the instance name in its header.
        let help = label_blocks(build_help(), Some("laptop"));
        let head = help[0]
            .pointer("/text/text")
            .and_then(Value::as_str)
            .unwrap();
        assert!(
            head.starts_with("[laptop] "),
            "help header states the instance: {head}"
        );

        // A single-section ephemeral (approve/config/pause confirmations)
        // carries the label in its mrkdwn text.
        let eph = label_blocks(
            vec![json!({ "type": "section",
                         "text": { "type": "mrkdwn", "text": ":gear: Set `worker` on `m-1`." } })],
            Some("laptop"),
        );
        assert_eq!(eph.len(), 1, "still a single block");
        let text = eph[0]
            .pointer("/text/text")
            .and_then(Value::as_str)
            .unwrap();
        assert!(
            text.starts_with("[laptop] :gear:"),
            "confirmation labeled: {text}"
        );
    }

    #[test]
    fn label_blocks_escapes_a_hostile_name_in_mrkdwn_and_keeps_headers_plain() {
        // mrkdwn surface (an ephemeral section): `<&>` must be entity-escaped
        // so it can't smuggle a link/mention or break rendering.
        let eph = label_blocks(
            vec![json!({ "type": "section", "text": { "type": "mrkdwn", "text": "ok" } })],
            Some("<&>"),
        );
        let text = eph[0]
            .pointer("/text/text")
            .and_then(Value::as_str)
            .unwrap();
        assert_eq!(text, "[&lt;&amp;&gt;] ok", "hostile name escaped in mrkdwn");

        // plain_text surface (a header): rendered verbatim — plain_text never
        // parses markup, and escaping would display the entities literally.
        let labeled = label_blocks(
            build_status(&StatusSummary {
                mission_id: "m-1".into(),
                status: "Running".into(),
                summary: "s".into(),
            }),
            Some("<&>"),
        );
        let head = labeled[0]
            .pointer("/text/text")
            .and_then(Value::as_str)
            .unwrap();
        assert!(
            head.starts_with("[<&>] "),
            "plain_text header keeps the raw name: {head}"
        );
    }

    #[test]
    fn label_blocks_reclips_a_labeled_header_to_the_150_char_cap() {
        // A near-cap header plus a prefix must stay within Block Kit's limit.
        let long_goal_header = vec![header(&"x".repeat(400))];
        let labeled = label_blocks(long_goal_header, Some("studio"));
        let head = labeled[0]
            .pointer("/text/text")
            .and_then(Value::as_str)
            .unwrap();
        assert!(
            head.chars().count() <= 150,
            "header cap holds: {}",
            head.chars().count()
        );
        assert!(head.starts_with("[studio] "), "prefix survives the re-clip");
    }

    #[test]
    fn label_home_view_prefixes_the_home_header() {
        let view = build_home_view(
            &[HomeMission {
                mission_id: "m-1".into(),
                status: "Running".into(),
            }],
            &[],
            &[],
            None,
        );
        let labeled = label_home_view(view.clone(), Some("cloud"));
        assert_eq!(labeled["type"], "home", "still a home view object");
        let blocks = labeled["blocks"].as_array().unwrap();
        assert_eq!(
            blocks.len(),
            view["blocks"].as_array().unwrap().len(),
            "no blocks added"
        );
        let head = blocks[0]
            .pointer("/text/text")
            .and_then(Value::as_str)
            .unwrap();
        assert!(
            head.starts_with("[cloud] "),
            "home header shows the instance: {head}"
        );
        // Every block still has a recognized type (the empty-state test's bar).
        for b in blocks {
            let ty = b["type"].as_str().expect("block type");
            assert!(["header", "section", "context", "divider", "actions"].contains(&ty));
        }
    }

    #[test]
    fn home_view_mission_rows_deep_link_when_dashboard_url_set() {
        let view = build_home_view(
            &[HomeMission {
                mission_id: "m-1".into(),
                status: "Running".into(),
            }],
            &[],
            &[],
            Some("http://127.0.0.1:4600"),
        );
        let blocks = view["blocks"].as_array().expect("home blocks");
        // A mission section carries an accessory link button to the deep link.
        let has_link = blocks
            .iter()
            .any(|b| b["accessory"]["url"] == "http://127.0.0.1:4600/#/m/m-1");
        assert!(
            has_link,
            "mission row deep-links to the dashboard when configured"
        );
    }
}
