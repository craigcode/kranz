use super::*;
use crate::types::{BlockCause, BlockOwner};

fn event(kind: &str, reason: &str, context: Option<serde_json::Value>) -> Event {
    let mut value = serde_json::json!({
        "seq": 1, "ts": "2026-09-12T00:00:00Z", "missionId": "m-block",
        "type": kind, "payload": {"milestoneId": "ms-1", "reason": reason}
    });
    if let Some(context) = context {
        value["payload"]["blockContext"] = context;
    }
    serde_json::from_value(value).unwrap()
}

#[test]
fn typed_block_context_recovery_ignores_prose_and_rejects_unknown_ownership() {
    let legacy_reason = "workspace gate: readiness failed";
    assert!(latest_block_is_gate_owned(
        &[event("milestone.blocked", legacy_reason, None)],
        "ms-1"
    ));
    for (context, expected) in [
        (
            serde_json::json!({"owner": "workspaceGate", "cause": "workspaceCheck"}),
            true,
        ),
        (
            serde_json::json!({"owner": "workspaceGate", "cause": "futureCause"}),
            false,
        ),
        (
            serde_json::json!({"owner": "futureOwner", "cause": "workspaceCheck"}),
            false,
        ),
        (serde_json::json!({"owner": "workspaceGate"}), false),
        (serde_json::json!({"cause": "workspaceCheck"}), false),
        (serde_json::json!({}), false),
        (serde_json::to_value(BlockContext::OPERATOR).unwrap(), false),
        (
            serde_json::to_value(BlockContext::engine(BlockCause::ReviewerIndependence)).unwrap(),
            false,
        ),
    ] {
        for reason in [legacy_reason, "different operator-facing wording"] {
            let block = event("milestone.blocked", reason, Some(context.clone()));
            // The same eligibility must survive a restart/replay round trip.
            let bytes = serde_json::to_vec(&block).unwrap();
            let replay: Event = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                latest_block_is_gate_owned(&[replay], "ms-1"),
                expected,
                "{context}: {reason}"
            );
        }
    }
    // A later unrelated block must never inherit an earlier gate's ownership.
    let gate = event(
        "milestone.blocked",
        "changed text",
        Some(serde_json::to_value(BlockContext::WORKSPACE_GATE).unwrap()),
    );
    let operator = event(
        "milestone.blocked",
        legacy_reason,
        Some(serde_json::to_value(BlockContext::OPERATOR).unwrap()),
    );
    assert!(!latest_block_is_gate_owned(&[gate, operator], "ms-1"));
}

#[test]
fn typed_block_context_unblock_metrics_ignore_text_and_only_fallback_for_old_logs() {
    use crate::escalation_metrics::is_engine_lift;
    for reason in [GATE_LIFT_REASON, "readiness recovered with new wording"] {
        assert!(is_engine_lift(reason, Some(&BlockContext::WORKSPACE_GATE)));
        assert!(!is_engine_lift(reason, Some(&BlockContext::OPERATOR)));
        assert!(!is_engine_lift(
            reason,
            Some(&BlockContext {
                owner: BlockOwner::WorkspaceGate,
                cause: BlockCause::Unknown,
            })
        ));
    }
    assert!(is_engine_lift(GATE_LIFT_REASON, None));
    assert!(!is_engine_lift("operator said proceed", None));
}

#[test]
fn typed_block_context_wire_is_additive_but_explicit_null_is_not_legacy() {
    for kind in ["milestone.blocked", "milestone.unblocked"] {
        let old = event(kind, "original reason", None);
        let value = serde_json::to_value(&old).unwrap();
        assert!(value["payload"].get("blockContext").is_none());
        let mut null = value.clone();
        null["payload"]["blockContext"] = serde_json::Value::Null;
        assert!(serde_json::from_value::<Event>(null).is_err());
        let typed = event(
            kind,
            "original reason",
            Some(serde_json::to_value(BlockContext::WORKSPACE_GATE).unwrap()),
        );
        let value = serde_json::to_value(typed).unwrap();
        assert_eq!(value["payload"]["blockContext"]["owner"], "workspaceGate");
        assert_eq!(value["payload"]["reason"], "original reason");
    }
}
