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
}
