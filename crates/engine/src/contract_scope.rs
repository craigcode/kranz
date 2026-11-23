//! Due-at rule for scoped validation assertions (ticket
//! `milestone-scoped-contract-assertions`).
//!
//! An [`Assertion`] may name the plan milestone that must satisfy it
//! (`Assertion.milestone`). This module resolves that title against the
//! plan's milestone list and decides, for a given validation round, whether
//! the assertion is "due" — i.e. a failure should be treated as a blocking
//! finding. An assertion whose milestone cannot be resolved unambiguously
//! fails toward today's behavior: due at every round, exactly as an
//! unscoped (`None`) assertion is.

use crate::error::{EngineError, Result};
use crate::planning::norm_title;
use crate::types::Assertion;

/// The 0-based position of the milestone named by `assertion.milestone`
/// among `milestone_titles`, matching case- and whitespace-insensitively
/// ([`norm_title`]). `None` when the field is unset, or when the title
/// matches zero or more than one milestone.
pub(crate) fn due_index(assertion: &Assertion, milestone_titles: &[&str]) -> Option<usize> {
    let wanted = norm_title(assertion.milestone.as_deref()?);
    let mut matches = milestone_titles
        .iter()
        .enumerate()
        .filter(|(_, title)| norm_title(title) == wanted);
    let (idx, _) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(idx)
}

/// Whether `assertion` is due at validation round `round_index` (0-based,
/// aligned with milestone position). True when unscoped, when the scoped
/// title cannot be resolved to exactly one milestone (fail toward due
/// everywhere), or when the resolved milestone is at or before this round.
pub(crate) fn is_due(assertion: &Assertion, milestone_titles: &[&str], round_index: usize) -> bool {
    match due_index(assertion, milestone_titles) {
        None => true,
        Some(idx) => idx <= round_index,
    }
}

/// Refuse a contract whose assertions name milestones that don't resolve
/// unambiguously against `milestone_titles`. `context` ("plan",
/// "plan.revised", "revised plan") prefixes the error so callers in the
/// approval path, the pre-emit revision gate, and the reducer's revision
/// fold read consistently.
pub(crate) fn validate_milestone_refs(
    contract: &[Assertion],
    milestone_titles: &[&str],
    context: &str,
) -> Result<()> {
    for assertion in contract {
        let Some(wanted) = assertion.milestone.as_deref() else {
            continue;
        };
        let norm_wanted = norm_title(wanted);
        let matches = milestone_titles
            .iter()
            .filter(|title| norm_title(title) == norm_wanted)
            .count();
        if matches == 0 {
            return Err(EngineError::InvalidState(format!(
                "{context} validation assertion '{}' names milestone '{wanted}', which matches \
                 no plan milestone",
                assertion.id
            )));
        }
        if matches > 1 {
            return Err(EngineError::InvalidState(format!(
                "{context} validation assertion '{}' names milestone '{wanted}', which matches \
                 more than one plan milestone",
                assertion.id
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AssertionCheck;

    fn assertion(milestone: Option<&str>) -> Assertion {
        Assertion {
            id: "a-scope".to_string(),
            statement: "the build passes".to_string(),
            check: AssertionCheck::Command,
            command: Some("true".to_string()),
            milestone: milestone.map(str::to_string),
            pty_script: None,
            negative_control: None,
            timeout_secs: None,
        }
    }

    #[test]
    fn assertion_due_rule_none_is_due_everywhere() {
        let a = assertion(None);
        let titles = ["M1", "M2", "M3"];
        for round in 0..titles.len() {
            assert!(is_due(&a, &titles, round));
        }
    }

    #[test]
    fn assertion_due_rule_scoped_not_due_before_due_at_and_due_after() {
        let a = assertion(Some("M2"));
        let titles = ["M1", "M2", "M3"];
        assert!(!is_due(&a, &titles, 0));
        assert!(is_due(&a, &titles, 1));
        assert!(is_due(&a, &titles, 2));
    }

    #[test]
    fn assertion_due_rule_matching_is_trim_and_case_insensitive() {
        let a = assertion(Some("  m2  "));
        let titles = ["M1", "M2", "M3"];
        assert_eq!(due_index(&a, &titles), Some(1));
    }

    #[test]
    fn assertion_due_rule_unknown_title_is_due_everywhere() {
        let a = assertion(Some("Nope"));
        let titles = ["M1", "M2", "M3"];
        for round in 0..titles.len() {
            assert!(is_due(&a, &titles, round));
        }
        assert_eq!(due_index(&a, &titles), None);
    }

    #[test]
    fn assertion_due_rule_ambiguous_title_is_due_everywhere() {
        let a = assertion(Some("Dup"));
        let titles = ["Dup", "M2", "Dup"];
        for round in 0..titles.len() {
            assert!(is_due(&a, &titles, round));
        }
        assert_eq!(due_index(&a, &titles), None);
    }

    #[test]
    fn assertion_due_rule_validate_milestone_refs_ok() {
        let contract = vec![assertion(Some("M2")), assertion(None)];
        let titles = ["M1", "M2", "M3"];
        assert!(validate_milestone_refs(&contract, &titles, "plan").is_ok());
    }

    #[test]
    fn assertion_due_rule_validate_milestone_refs_unknown_error_contains_id() {
        let contract = vec![assertion(Some("Nope"))];
        let titles = ["M1", "M2"];
        let err = validate_milestone_refs(&contract, &titles, "plan").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("a-scope"),
            "error should contain assertion id: {msg}"
        );
        assert!(msg.contains("no plan milestone"), "{msg}");
    }

    #[test]
    fn assertion_due_rule_validate_milestone_refs_ambiguous_error_contains_id() {
        let contract = vec![assertion(Some("Dup"))];
        let titles = ["Dup", "M2", "Dup"];
        let err = validate_milestone_refs(&contract, &titles, "plan").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("a-scope"),
            "error should contain assertion id: {msg}"
        );
        assert!(msg.contains("more than one plan milestone"), "{msg}");
    }
}
