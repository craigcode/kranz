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

/// Plan-ready message: header + goal + milestone list + assertion count, then an
/// actions block with an [`APPROVE_ACTION_ID`] button carrying the mission id.
pub fn build_plan_ready(p: &PlanReady) -> Vec<Value> {
    let mut milestones = String::new();
    for title in &p.milestone_titles {
        milestones.push_str("• ");
        milestones.push_str(title.trim());
        milestones.push('\n');
    }
    if milestones.is_empty() {
        milestones.push_str("_(no milestones listed)_");
    }

    vec![
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
    ]
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
/// unblock" instruction (a threaded reply becomes `kranz msg` guidance).
pub fn build_blocked(b: &Blocked) -> Vec<Value> {
    vec![
        header(&format!("Milestone blocked — {}", b.mission_id)),
        section(&format!(
            "Milestone `{}` is blocked:\n>{}",
            b.milestone_id,
            clip(b.reason.trim()).replace('\n', "\n>")
        )),
        context("Reply in this thread to unblock (your reply becomes orchestrator guidance)."),
    ]
}

/// Completion message: outcome header + summary + branch + optional cost.
pub fn build_complete(c: &Complete) -> Vec<Value> {
    let (emoji, verb) = match c.outcome {
        Outcome::Completed => (":white_check_mark:", "completed"),
        Outcome::Failed => (":x:", "failed"),
    };
    let mut meta = format!("branch `{}`", c.branch);
    if let Some(cost) = c.cost_usd {
        meta.push_str(&format!(" · cost ${cost:.2}"));
    }
    meta.push_str(&format!(" · mission `{}`", c.mission_id));

    vec![
        header(&format!("{emoji} Mission {verb} — {}", c.mission_id)),
        section(&clip(c.summary.trim())),
        context(&meta),
    ]
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
            "Money-spending actions (`new` · `plan` · `approve`) are gated by the \
             `slack.allowUsers` allowlist. Deep forensic inspection (full transcripts, \
             the four-pane live view) lives in the web UI via `kranz serve --open`.",
        ),
    ]
}

// -- block primitives -------------------------------------------------------

fn header(text: &str) -> Value {
    // header blocks only accept plain_text and cap at 150 chars.
    json!({ "type": "header", "text": { "type": "plain_text", "text": clip_to(text, 150) } })
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
        });
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
        });
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
        });
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
        });
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
        });
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
        });
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
}
