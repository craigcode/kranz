//! Role prompts (plan §4.6) — embedded at compile time.
//!
//! The markdown sources live in `crates/engine/prompts/` and are the
//! opinionated heart of the product. Each spawned session records
//! [`hash`] of its role prompt (on `worker.spawned`) so transcripts stay
//! traceable to the exact prompt text that produced them.

use crate::types::Role;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::OnceLock;

const ORCHESTRATOR: &str = include_str!("../prompts/orchestrator.md");
const WORKER: &str = include_str!("../prompts/worker.md");
const VALIDATOR_SCRUTINY: &str = include_str!("../prompts/validator-scrutiny.md");
const VALIDATOR_FUNCTIONAL: &str = include_str!("../prompts/validator-functional.md");

/// The raw (unrendered) prompt template for a role.
pub fn text(role: Role) -> &'static str {
    match role {
        Role::Orchestrator => ORCHESTRATOR,
        Role::Worker => WORKER,
        Role::ValidatorScrutiny => VALIDATOR_SCRUTINY,
        Role::ValidatorFunctional => VALIDATOR_FUNCTIONAL,
    }
}

/// First 12 hex chars of the SHA-256 of the role's prompt text. Recorded on
/// `worker.spawned` for traceability.
pub fn hash(role: Role) -> String {
    hash_text(text(role))
}

/// [`hash`] of an arbitrary prompt text — for prompts extended past the
/// embedded template (a configured pack's appended guidance, ticket
/// `pack-contract-gates-prompts`), so the recorded hash still names the
/// exact text the session ran with.
pub fn hash_text(prompt: &str) -> String {
    let digest = Sha256::digest(prompt.as_bytes());
    digest[..6].iter().map(|b| format!("{b:02x}")).collect()
}

/// Render a prompt template: every `{key}` whose key is present in `vars` is
/// replaced by its value; unknown placeholders are left intact. Replacement
/// is a single pass over the template — values are never re-scanned, so a
/// value containing `{otherKey}` is not expanded (no recursion).
pub fn render(template: &str, vars: &HashMap<&str, String>) -> String {
    static PLACEHOLDER: OnceLock<regex::Regex> = OnceLock::new();
    let re = PLACEHOLDER.get_or_init(|| {
        // Identifier-shaped keys only; JSON braces in the prompts never match.
        regex::Regex::new(r"\{([A-Za-z][A-Za-z0-9_]*)\}").expect("static regex")
    });
    re.replace_all(template, |caps: &regex::Captures<'_>| {
        match vars.get(&caps[1]) {
            Some(value) => value.clone(),
            None => caps[0].to_string(),
        }
    })
    .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_prompts_mention_base_sha() {
        for role in [
            Role::Orchestrator,
            Role::Worker,
            Role::ValidatorScrutiny,
            Role::ValidatorFunctional,
        ] {
            assert!(
                text(role).contains("KRANZ_BASE_SHA"),
                "{role:?} prompt does not mention KRANZ_BASE_SHA"
            );
        }
    }
}
