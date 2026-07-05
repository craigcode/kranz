//! `kranz serve --slack` glue: the Slack bridge's [`PlanningHost`] implemented
//! over the SAME [`kranz_server::MissionHost`] the web UI serves from — one
//! hosted-engine registry, two clients (docs/slack-management.md). This is the
//! only place the two crates meet; `kranz_slack` stays server-free and
//! `kranz_server` stays Slack-free.

use kranz_engine::types::Plan;
use kranz_server::{ApiError, MissionHost};
use kranz_slack::host::{BoxFuture, PlanOutcome, PlanningHost};
use serde_json::Value;
use std::sync::Arc;

/// Adapter handed to [`kranz_slack::serve_slack`] by the serve command.
pub struct HostedPlanning(pub Arc<MissionHost>);

impl PlanningHost for HostedPlanning {
    fn create<'a>(&'a self, goal: &'a str) -> BoxFuture<'a, anyhow::Result<String>> {
        Box::pin(async move { self.0.create(goal, None).await.map_err(plain) })
    }

    fn planning_turn<'a>(
        &'a self,
        id: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<String>> {
        Box::pin(async move { self.0.planning_turn(id, text).await.map_err(plain) })
    }

    fn request_plan<'a>(&'a self, id: &'a str) -> BoxFuture<'a, anyhow::Result<PlanOutcome>> {
        Box::pin(async move {
            let value = self.0.request_plan(id).await.map_err(plain)?;
            if value.get("ready").and_then(Value::as_bool) == Some(true) {
                let plan: Plan =
                    serde_json::from_value(value.get("plan").cloned().unwrap_or(Value::Null))
                        .map_err(|e| anyhow::anyhow!("host returned an unparseable plan: {e}"))?;
                Ok(PlanOutcome::Ready {
                    plan,
                    estimate: estimate_line(&value),
                })
            } else {
                let reply = value
                    .get("reply")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                Ok(PlanOutcome::NotReady(reply))
            }
        })
    }

    fn approve_pending<'a>(&'a self, id: &'a str) -> BoxFuture<'a, anyhow::Result<Option<String>>> {
        Box::pin(async move { self.0.try_approve_pending(id).await.map_err(plain) })
    }

    fn start<'a>(&'a self, id: &'a str) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move { self.0.start(id).await.map_err(plain) })
    }

    fn release<'a>(&'a self, id: &'a str) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move { self.0.release(id).map_err(plain) })
    }
}

/// The host's errors already carry user-presentable messages (409 "a turn is
/// in flight…", 404 "unknown mission…"); the status code adds nothing in a
/// Slack ephemeral, so forward the message alone.
fn plain(e: ApiError) -> anyhow::Error {
    anyhow::anyhow!("{}", e.message)
}

/// One estimate line for the plan-review context row, from the host's
/// `{"estimate":{"lowUsd":…,"expectedUsd":…,"highUsd":…}}` payload — the same
/// numbers the planning TUI prints.
fn estimate_line(value: &Value) -> Option<String> {
    let est = value.get("estimate")?;
    let low = est.get("lowUsd").and_then(Value::as_f64)?;
    let expected = est.get("expectedUsd").and_then(Value::as_f64)?;
    let high = est.get("highUsd").and_then(Value::as_f64)?;
    Some(format!(
        "estimated ${low:.2}–${high:.2} (expected ~${expected:.2})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn estimate_line_renders_all_three_numbers() {
        let value = json!({ "estimate": { "lowUsd": 6.1, "expectedUsd": 12.2, "highUsd": 30.5 } });
        assert_eq!(
            estimate_line(&value).unwrap(),
            "estimated $6.10–$30.50 (expected ~$12.20)"
        );
    }

    #[test]
    fn estimate_line_absent_when_estimate_missing_or_partial() {
        assert!(estimate_line(&json!({})).is_none());
        assert!(estimate_line(&json!({ "estimate": { "lowUsd": 1.0 } })).is_none());
    }
}
