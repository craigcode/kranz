//! Per-assertion `timeoutSecs` bounds for engine-run contract `command`
//! assertions. This module only validates the plan-schema field; it does
//! not change how commands execute (see `command_exec.rs`).

use crate::error::{EngineError, Result};
use crate::types::{Assertion, AssertionCheck};

/// Today's hard cap on a contract `command` assertion, unchanged unless a
/// plan or operator config overrides it (within [`MAX_CONTRACT_COMMAND_TIMEOUT_SECS`]).
/// Wired into command execution by a later feature.
#[allow(dead_code)]
pub const DEFAULT_CONTRACT_COMMAND_TIMEOUT_SECS: u64 = 600;

/// A fixed engine ceiling (4 hours): no plan or config may raise a
/// contract command's timeout above this, so the limit can never be
/// effectively disabled.
pub const MAX_CONTRACT_COMMAND_TIMEOUT_SECS: u64 = 14_400;

fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::InvalidState(format!("contract timeout: {}", message.into()))
}

pub(crate) fn validate(contract: &[Assertion]) -> Result<()> {
    for assertion in contract {
        let Some(timeout) = assertion.timeout_secs else {
            continue;
        };
        if assertion.check != AssertionCheck::Command {
            return Err(invalid(format!(
                "assertion '{}' sets timeoutSecs ({timeout}) but check is not command; \
                 only check=command assertions may set timeoutSecs",
                assertion.id
            )));
        }
        if timeout == 0 || timeout > MAX_CONTRACT_COMMAND_TIMEOUT_SECS {
            return Err(invalid(format!(
                "assertion '{}' has timeoutSecs {timeout}, outside the allowed range \
                 1..={MAX_CONTRACT_COMMAND_TIMEOUT_SECS}",
                assertion.id
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assertion(id: &str, check: AssertionCheck, timeout_secs: Option<u64>) -> Assertion {
        Assertion {
            id: id.to_string(),
            statement: "does the thing".to_string(),
            check,
            command: if check == AssertionCheck::Command {
                Some("true".to_string())
            } else {
                None
            },
            milestone: None,
            pty_script: None,
            negative_control: None,
            timeout_secs,
        }
    }

    #[test]
    fn contract_timeout_schema_deserialises_and_reserialises_present_value() {
        let json = serde_json::json!({
            "id": "a1",
            "statement": "runs the suite",
            "check": "command",
            "command": "cargo test",
            "timeoutSecs": 5400
        });
        let parsed: Assertion = serde_json::from_value(json).unwrap();
        assert_eq!(parsed.timeout_secs, Some(5400));
        let back = serde_json::to_value(&parsed).unwrap();
        assert_eq!(back["timeoutSecs"], serde_json::json!(5400));
    }

    #[test]
    fn contract_timeout_default_absent_value_roundtrips_without_key() {
        let json = serde_json::json!({
            "id": "a1",
            "statement": "runs the suite",
            "check": "command",
            "command": "cargo test"
        });
        let parsed: Assertion = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(parsed.timeout_secs, None);
        let back = serde_json::to_value(&parsed).unwrap();
        assert!(back.get("timeoutSecs").is_none());
        assert_eq!(back, json);
    }

    #[test]
    fn contract_timeout_schema_rejects_over_ceiling() {
        let a = assertion("a1", AssertionCheck::Command, Some(14_401));
        let err = validate(&[a]).unwrap_err().to_string();
        assert!(err.contains("a1"), "{err}");
        assert!(err.contains("14400"), "{err}");
    }

    #[test]
    fn contract_timeout_schema_rejects_zero_and_non_command() {
        let zero = assertion("a1", AssertionCheck::Command, Some(0));
        let err = validate(&[zero]).unwrap_err().to_string();
        assert!(err.contains("a1"), "{err}");

        let non_command = assertion("a2", AssertionCheck::AgentJudgement, Some(100));
        let err = validate(&[non_command]).unwrap_err().to_string();
        assert!(err.contains("a2"), "{err}");
    }

    #[test]
    fn contract_timeout_schema_accepts_none_and_in_range() {
        let a = assertion("a1", AssertionCheck::Command, None);
        let b = assertion("a2", AssertionCheck::Command, Some(14_400));
        validate(&[a, b]).unwrap();
    }
}
