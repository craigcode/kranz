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

/// A plan awaiting review, with the pieces a reviewer needs before spending:
/// the goal, milestone list, and validation-assertion count. Renders with
/// **Approve & start** / **Approve & queue** buttons carrying the mission id.
#[derive(Debug, Clone)]
pub struct PlanReview {
    pub mission_id: String,
    pub goal: String,
    pub milestone_titles: Vec<String>,
    pub assertion_count: usize,
    /// Optional one-line calibrated cost/time estimate string (rendered as
    /// context when present).
    pub estimate: Option<String>,
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
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
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
    format!("{base}/#/m/{mission_id}")
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

/// Plan-ready message: header + goal + milestone list + assertion count, then an
/// actions block with an [`APPROVE_ACTION_ID`] button carrying the mission id.
/// When `dashboard_url` is set, an "Open in dashboard" deep-link button is
/// appended (see [`dashboard_button`]); when `None`, nothing extra is added.
pub fn build_plan_ready(p: &PlanReady, dashboard_url: Option<&str>) -> Vec<Value> {
    let mut milestones = String::new();
    for title in &p.milestone_titles {
        milestones.push_str("• ");
        milestones.push_str(title.trim());
        milestones.push('\n');
    }
    if milestones.is_empty() {
        milestones.push_str("_(no milestones listed)_");
    }

    let mut blocks = vec![
        header(&format!("Plan ready for review — {}", p.mission_id)),
        section(&format!("*Goal*\n{}", clip(&p.goal))),
        section(&format!("*Milestones*\n{}", clip(milestones.trim_end()))),
        context(&format!(
            "{} validation assertion{} · mission `{}`",
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

/// Completion message: outcome header + summary + branch + optional cost. When
/// `dashboard_url` is set, an "Open in dashboard" deep-link button is appended.
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
        context(&meta),
    ];
    push_dashboard_button(&mut blocks, dashboard_url, &c.mission_id);
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
    if let Some(reply) = a.opening_reply.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
        blocks.push(section(&format!("*Orchestrator*\n{}", clip(reply))));
        blocks.push(context(
            "Reply in this thread to answer — each reply is a planning turn. \
             When you're ready, run `/kranz plan` to see the plan.",
        ));
    } else {
        blocks.push(context(
            "Reply in this thread to plan (each reply is a planning turn). \
             When you're ready, run `/kranz plan` to see the plan.",
        ));
    }
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

/// Plan-review message: goal + milestones + assertion count + optional
/// estimate, then an actions block with **Approve & start** ([`START_ACTION_ID`])
/// and **Approve & queue** ([`APPROVE_ACTION_ID`]) buttons, both carrying the
/// mission id in `value`.
pub fn build_plan_review(p: &PlanReview) -> Vec<Value> {
    let mut milestones = String::new();
    for title in &p.milestone_titles {
        milestones.push_str("• ");
        milestones.push_str(title.trim());
        milestones.push('\n');
    }
    if milestones.is_empty() {
        milestones.push_str("_(no milestones listed)_");
    }

    let mut blocks = vec![
        header(&format!("Review plan — {}", p.mission_id)),
        section(&format!("*Goal*\n{}", clip(p.goal.trim()))),
        section(&format!("*Milestones*\n{}", clip(milestones.trim_end()))),
    ];
    let mut meta = format!(
        "{} validation assertion{} · mission `{}`",
        p.assertion_count,
        plural(p.assertion_count),
        p.mission_id
    );
    if let Some(est) = p.estimate.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
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
             • `/kranz new <goal>` — create a mission and open its planning thread\n\
             • `/kranz plan <id>` — request the plan for review\n\
             • `/kranz approve <id>` — approve the plan and queue the mission\n\
             • `/kranz config [<id>] <role> <model> [effort]` — change a role's model/effort \
             (roles: orchestrator·worker·scrutiny·functional; effort: low·medium·high·xhigh·max)\n\
             • `/kranz pause [<id>]` — pause a running mission (between worker runs)\n\
             • `/kranz resume [<id>]` — resume a paused mission\n\
             • `/kranz work` — show the execution queue (drain it with the `kranz work` dispatcher)\n\
             • `/kranz status [<id>]` — show a mission's status\n\
             • `/kranz ticket <title>` — file a new backlog ticket\n\
             • `/kranz help` — show this message",
        ),
        section(
            "*In a mission thread*\n\
             • *Approve & start* / *Approve & queue* buttons on a plan-review message\n\
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
        blocks.push(context("_No active missions. Create one with_ `/kranz new <goal>`."));
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
            body.push_str(&format!("{}. `{}` · priority {}\n", i + 1, q.mission_id, q.priority));
        }
        blocks.push(section(&clip(body.trim_end())));
    }

    blocks.push(json!({ "type": "divider" }));

    // -- Open tickets --
    blocks.push(section("*Open tickets*"));
    if tickets.is_empty() {
        blocks.push(context("_No open tickets. File one with_ `/kranz ticket <title>`."));
    } else {
        let mut body = String::new();
        for t in tickets {
            body.push_str(&format!("• `{}` — {} · _{}_\n", t.slug, t.title.trim(), t.state));
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
fn clip_to(s: &str, max: usize) -> String {
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
    fn plan_ready_has_mission_goal_milestones_and_approve_button() {
        let blocks = build_plan_ready(&PlanReady {
            mission_id: "m-42".into(),
            goal: "Rate-limit the notes API".into(),
            milestone_titles: vec!["Token bucket".into(), "429 responses".into()],
            assertion_count: 3,
        }, None);
        let text = all_text(&blocks);
        assert!(text.contains("m-42"), "mission id present");
        assert!(text.contains("Rate-limit the notes API"), "goal present");
        assert!(text.contains("Token bucket"), "milestone 1 present");
        assert!(text.contains("429 responses"), "milestone 2 present");
        assert!(text.contains("3 validation assertions"), "assertion count present");

        // The approve button carries the mission id and the shared action id.
        let button = find_button(&blocks).expect("has an approve button");
        assert_eq!(button["action_id"], APPROVE_ACTION_ID);
        assert_eq!(button["value"], "m-42");
    }

    #[test]
    fn plan_ready_single_assertion_is_singular() {
        let blocks = build_plan_ready(&PlanReady {
            mission_id: "m-1".into(),
            goal: "g".into(),
            milestone_titles: vec![],
            assertion_count: 1,
        }, None);
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
        let blocks = build_blocked(&Blocked {
            mission_id: "m-7".into(),
            milestone_id: "ms-2".into(),
            reason: "fix-cycle cap exceeded after 2 rounds".into(),
        }, None);
        let text = all_text(&blocks);
        assert!(text.contains("m-7"), "mission id present");
        assert!(text.contains("ms-2"), "milestone id present");
        assert!(text.contains("fix-cycle cap exceeded"), "reason present");
        assert!(text.to_lowercase().contains("reply in this thread to unblock"));
    }

    #[test]
    fn complete_shows_branch_summary_and_cost() {
        let blocks = build_complete(&Complete {
            mission_id: "m-9".into(),
            outcome: Outcome::Completed,
            summary: "Added rate limiting; all tests pass.".into(),
            branch: "kranz/mission-m-9".into(),
            cost_usd: Some(4.2),
        }, None);
        let text = all_text(&blocks);
        assert!(text.contains("m-9"), "mission id present");
        assert!(text.contains("completed"), "outcome verb present");
        assert!(text.contains("Added rate limiting"), "summary present");
        assert!(text.contains("kranz/mission-m-9"), "branch present");
        assert!(text.contains("$4.20"), "cost rendered to cents");
    }

    #[test]
    fn failed_mission_reads_failed_and_omits_cost_when_absent() {
        let blocks = build_complete(&Complete {
            mission_id: "m-9".into(),
            outcome: Outcome::Failed,
            summary: "worker exhausted respawns".into(),
            branch: "kranz/mission-m-9".into(),
            cost_usd: None,
        }, None);
        let text = all_text(&blocks);
        assert!(text.contains("failed"), "failure verb present");
        assert!(text.contains("worker exhausted respawns"));
        assert!(!text.contains("cost $"), "no cost line when unknown");
    }

    #[test]
    fn long_fields_are_clipped_with_ellipsis() {
        let long = "x".repeat(5000);
        let blocks = build_blocked(&Blocked {
            mission_id: "m".into(),
            milestone_id: "ms".into(),
            reason: long,
        }, None);
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

    /// Find the first `button` element inside any `actions` block.
    fn find_button(blocks: &[Value]) -> Option<Value> {
        for b in blocks {
            if b["type"] == "actions" {
                if let Some(elems) = b["elements"].as_array() {
                    for e in elems {
                        if e["type"] == "button" {
                            return Some(e.clone());
                        }
                    }
                }
            }
        }
        None
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
        assert!(text.contains("What is the test command?"), "opening reply present");
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
        assert!(text.contains("2/3 milestones complete"), "summary body present");
        assert!(text.contains("cost $1.20"));
    }

    #[test]
    fn plan_review_has_two_buttons_carrying_mission_id() {
        let blocks = build_plan_review(&PlanReview {
            mission_id: "m-42".into(),
            goal: "Rate-limit the notes API".into(),
            milestone_titles: vec!["Token bucket".into(), "429 responses".into()],
            assertion_count: 3,
            estimate: Some("~$4.50 · ~12 min".into()),
        });
        let text = all_text(&blocks);
        assert!(text.contains("m-42"), "mission id present");
        assert!(text.contains("Rate-limit the notes API"), "goal present");
        assert!(text.contains("Token bucket") && text.contains("429 responses"));
        assert!(text.contains("3 validation assertions"), "assertion count present");
        assert!(text.contains("~$4.50 · ~12 min"), "estimate rendered");

        let buttons = all_buttons(&blocks);
        assert_eq!(buttons.len(), 2, "approve & start plus approve & queue");
        let start = buttons.iter().find(|b| b["action_id"] == START_ACTION_ID).expect("start btn");
        let queue = buttons.iter().find(|b| b["action_id"] == APPROVE_ACTION_ID).expect("queue btn");
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
            estimate: None,
        });
        let text = all_text(&blocks);
        assert!(text.contains("1 validation assertion "), "singular assertion");
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
        assert!(elem.get("value").is_none(), "link buttons carry a url, not a value");
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
        assert!(all_button_urls(&blocks).is_empty(), "no deep link when unset");
        // Set → exactly one deep-link url, correctly shaped.
        let blocks = build_plan_ready(&p, Some("http://127.0.0.1:4600"));
        assert_eq!(all_button_urls(&blocks), vec!["http://127.0.0.1:4600/#/m/m-42".to_string()]);
    }

    #[test]
    fn blocked_and_complete_carry_dashboard_link_when_set() {
        let blocked = build_blocked(
            &Blocked { mission_id: "m-7".into(), milestone_id: "ms-1".into(), reason: "x".into() },
            Some("http://dash/"),
        );
        assert_eq!(all_button_urls(&blocked), vec!["http://dash/#/m/m-7".to_string()]);

        let complete = build_complete(
            &Complete {
                mission_id: "m-9".into(),
                outcome: Outcome::Completed,
                summary: "done".into(),
                branch: "b".into(),
                cost_usd: None,
            },
            Some("http://dash"),
        );
        assert_eq!(all_button_urls(&complete), vec!["http://dash/#/m/m-9".to_string()]);
        // And absent when unset.
        let complete_no = build_complete(
            &Complete {
                mission_id: "m-9".into(),
                outcome: Outcome::Completed,
                summary: "done".into(),
                branch: "b".into(),
                cost_usd: None,
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
                HomeMission { mission_id: "m-1".into(), status: "Running".into() },
                HomeMission { mission_id: "m-2".into(), status: "Planning".into() },
            ],
            &[HomeQueueItem { mission_id: "m-3".into(), priority: 2 }],
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
                &Blocked { mission_id: "m-1".into(), milestone_id: "ms".into(), reason: "r".into() },
                None,
            ),
            build_complete(
                &Complete {
                    mission_id: "m-1".into(),
                    outcome: Outcome::Completed,
                    summary: "s".into(),
                    branch: "b".into(),
                    cost_usd: Some(1.0),
                },
                None,
            ),
            build_help(),
            build_needs_context(&NeedsContext { ticket_slug: "t".into(), questions: vec![] }),
            build_status(&StatusSummary {
                mission_id: "m-1".into(),
                status: "Running".into(),
                summary: "s".into(),
            }),
            // The single-section ephemeral shape (confirmations / errors).
            vec![json!({ "type": "section", "text": { "type": "mrkdwn", "text": "Queued `m-1`." } })],
        ];
        for blocks in shapes {
            assert_eq!(label_blocks(blocks.clone(), None), blocks, "None must not touch blocks");
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
        let head = labeled[0].pointer("/text/text").and_then(Value::as_str).unwrap();
        assert!(head.starts_with("[studio] "), "leading prefix on the header: {head}");
        assert!(head.contains("Plan ready for review — m-42"), "original header text intact");
        // Only the first block was touched.
        assert_eq!(labeled[1..], unlabeled[1..]);
    }

    #[test]
    fn label_blocks_labels_every_notification_help_and_ephemeral_shape() {
        let blocked = label_blocks(
            build_blocked(
                &Blocked { mission_id: "m-7".into(), milestone_id: "ms".into(), reason: "r".into() },
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
                },
                None,
            ),
            Some("laptop"),
        );
        assert!(all_text(&complete).contains("[laptop] "), "complete labeled");

        // `/kranz help` states the instance name in its header.
        let help = label_blocks(build_help(), Some("laptop"));
        let head = help[0].pointer("/text/text").and_then(Value::as_str).unwrap();
        assert!(head.starts_with("[laptop] "), "help header states the instance: {head}");

        // A single-section ephemeral (approve/config/pause confirmations)
        // carries the label in its mrkdwn text.
        let eph = label_blocks(
            vec![json!({ "type": "section",
                         "text": { "type": "mrkdwn", "text": ":gear: Set `worker` on `m-1`." } })],
            Some("laptop"),
        );
        assert_eq!(eph.len(), 1, "still a single block");
        let text = eph[0].pointer("/text/text").and_then(Value::as_str).unwrap();
        assert!(text.starts_with("[laptop] :gear:"), "confirmation labeled: {text}");
    }

    #[test]
    fn label_blocks_escapes_a_hostile_name_in_mrkdwn_and_keeps_headers_plain() {
        // mrkdwn surface (an ephemeral section): `<&>` must be entity-escaped
        // so it can't smuggle a link/mention or break rendering.
        let eph = label_blocks(
            vec![json!({ "type": "section", "text": { "type": "mrkdwn", "text": "ok" } })],
            Some("<&>"),
        );
        let text = eph[0].pointer("/text/text").and_then(Value::as_str).unwrap();
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
        let head = labeled[0].pointer("/text/text").and_then(Value::as_str).unwrap();
        assert!(head.starts_with("[<&>] "), "plain_text header keeps the raw name: {head}");
    }

    #[test]
    fn label_blocks_reclips_a_labeled_header_to_the_150_char_cap() {
        // A near-cap header plus a prefix must stay within Block Kit's limit.
        let long_goal_header = vec![header(&"x".repeat(400))];
        let labeled = label_blocks(long_goal_header, Some("studio"));
        let head = labeled[0].pointer("/text/text").and_then(Value::as_str).unwrap();
        assert!(head.chars().count() <= 150, "header cap holds: {}", head.chars().count());
        assert!(head.starts_with("[studio] "), "prefix survives the re-clip");
    }

    #[test]
    fn label_home_view_prefixes_the_home_header() {
        let view = build_home_view(
            &[HomeMission { mission_id: "m-1".into(), status: "Running".into() }],
            &[],
            &[],
            None,
        );
        let labeled = label_home_view(view.clone(), Some("cloud"));
        assert_eq!(labeled["type"], "home", "still a home view object");
        let blocks = labeled["blocks"].as_array().unwrap();
        assert_eq!(blocks.len(), view["blocks"].as_array().unwrap().len(), "no blocks added");
        let head = blocks[0].pointer("/text/text").and_then(Value::as_str).unwrap();
        assert!(head.starts_with("[cloud] "), "home header shows the instance: {head}");
        // Every block still has a recognized type (the empty-state test's bar).
        for b in blocks {
            let ty = b["type"].as_str().expect("block type");
            assert!(["header", "section", "context", "divider", "actions"].contains(&ty));
        }
    }

    #[test]
    fn home_view_mission_rows_deep_link_when_dashboard_url_set() {
        let view = build_home_view(
            &[HomeMission { mission_id: "m-1".into(), status: "Running".into() }],
            &[],
            &[],
            Some("http://127.0.0.1:4600"),
        );
        let blocks = view["blocks"].as_array().expect("home blocks");
        // A mission section carries an accessory link button to the deep link.
        let has_link = blocks.iter().any(|b| {
            b["accessory"]["url"] == "http://127.0.0.1:4600/#/m/m-1"
        });
        assert!(has_link, "mission row deep-links to the dashboard when configured");
    }
}
