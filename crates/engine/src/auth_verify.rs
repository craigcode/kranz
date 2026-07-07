//! Auth-preflight seam (docs/scoping/claude-cli-min-env.md; mission m-165b6f).
//!
//! `docs/scoping/claude-cli-min-env.md` claims Keychain auth is
//! HOME-independent; mission m-66aff8 contradicted that claim in practice
//! (workers died at turn 1 with "Not logged in", $0 cost, zero tool-use,
//! producing empty diffs while the mission falsely completed). Rather than
//! trust the heuristic again, [`verify_worker_auth`] drives a real trivial
//! session under a candidate env and classifies what actually happened.
//!
//! Not wired into the spawn path yet — that is a later feature. This module
//! is a pure seam over [`AgentBackend`] so it is exercised entirely offline
//! via [`crate::backend_mock::MockBackend`].

use crate::backend::{AgentBackend, AgentEvent, PromptMode, SessionExit, SessionSpec};
use std::collections::HashMap;

/// Outcome of driving a trivial session under a candidate worker env.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthVerdict {
    /// The session produced a normal assistant reply: the candidate env
    /// authenticates.
    Authenticated,
    /// The session exhibited a known auth-failure signature (an explicit
    /// "not logged in" style message, or terminated with zero assistant
    /// activity at zero cost / a non-success exit).
    Unauthenticated,
    /// Neither of the above could be established (spawn/IO error, or an
    /// ambiguous result) — fail-safe, never a panic.
    Inconclusive,
}

/// Substrings (checked case-insensitively) that indicate the `claude` CLI
/// rejected the session for lack of authentication.
const AUTH_FAILURE_SIGNATURES: &[&str] = &[
    "not logged in",
    "not authenticated",
    "please run /login",
    "please run `claude login`",
];

fn has_auth_failure_signature(text: &str) -> bool {
    let lower = text.to_lowercase();
    AUTH_FAILURE_SIGNATURES
        .iter()
        .any(|sig| lower.contains(sig))
}

/// Build the minimal single-shot [`SessionSpec`] used to probe whether
/// `candidate_env` lets the worker's `claude` CLI authenticate: a tiny prompt
/// asking the model to reply with a single token, cwd-independent (auth is
/// what's under test, not repo state).
fn probe_spec(candidate_env: &HashMap<String, String>) -> SessionSpec {
    SessionSpec {
        cwd: std::env::temp_dir(),
        prompt: PromptMode::SingleShot(
            "Reply with the single word: ack. Nothing else.".to_string(),
        ),
        append_system_prompt: None,
        model: "haiku".to_string(),
        effort: "low".to_string(),
        session_id: uuid::Uuid::new_v4().to_string(),
        resume: None,
        permission_mode: None,
        allowed_tools: Vec::new(),
        disallowed_tools: Vec::new(),
        tools: Vec::new(),
        writable: false,
        settings_json: None,
        json_schema: None,
        max_budget_usd: None,
        max_turns: Some(1),
        env: candidate_env.clone(),
        sandbox: None,
    }
}

/// Drive a trivial session under `candidate_env` via `backend` and classify
/// whether the worker's `claude` CLI can authenticate under it. Never panics;
/// any spawn/IO error or ambiguous outcome yields [`AuthVerdict::Inconclusive`]
/// rather than a guess in either direction.
pub async fn verify_worker_auth(
    backend: &dyn AgentBackend,
    candidate_env: &HashMap<String, String>,
) -> AuthVerdict {
    let spec = probe_spec(candidate_env);

    let mut session = match backend.start(spec).await {
        Ok(session) => session,
        Err(_) => return AuthVerdict::Inconclusive,
    };

    let mut saw_assistant_text = false;
    let mut result: Option<(bool, Option<f64>)> = None; // (is_error, cost_usd)

    loop {
        match session.next_event().await {
            Ok(Some(event)) => match event {
                AgentEvent::Text { text, .. } => {
                    if has_auth_failure_signature(&text) {
                        return AuthVerdict::Unauthenticated;
                    }
                    if !text.trim().is_empty() {
                        saw_assistant_text = true;
                    }
                }
                AgentEvent::Result {
                    text,
                    is_error,
                    cost_usd,
                    ..
                } => {
                    if has_auth_failure_signature(&text) {
                        return AuthVerdict::Unauthenticated;
                    }
                    result = Some((is_error, cost_usd));
                }
                AgentEvent::Other { raw } => {
                    if let Some(text) = raw.as_str() {
                        if has_auth_failure_signature(text) {
                            return AuthVerdict::Unauthenticated;
                        }
                    }
                }
                _ => {}
            },
            Ok(None) => break,
            Err(_) => return AuthVerdict::Inconclusive,
        }
    }

    let exit_failed_message = match session.exit_status() {
        Some(SessionExit::Failed(msg)) => Some(msg),
        _ => None,
    };
    if let Some(msg) = &exit_failed_message {
        if has_auth_failure_signature(msg) {
            return AuthVerdict::Unauthenticated;
        }
    }

    match result {
        Some((is_error, cost_usd)) => {
            let zero_cost = cost_usd.unwrap_or(0.0) <= 0.0;
            if !is_error && saw_assistant_text {
                AuthVerdict::Authenticated
            } else if is_error && !saw_assistant_text && zero_cost {
                AuthVerdict::Unauthenticated
            } else {
                AuthVerdict::Inconclusive
            }
        }
        None => AuthVerdict::Inconclusive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend_mock::{
        mock_init, mock_result_error, mock_result_text, MockBackend, MockScript,
    };

    fn candidate_env() -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert("HOME".to_string(), "/tmp/kranz-scratch-home".to_string());
        env.insert(
            "CLAUDE_CONFIG_DIR".to_string(),
            "/tmp/kranz-scratch-home/.claude".to_string(),
        );
        env
    }

    #[tokio::test]
    async fn verify_worker_auth_normal_reply_is_authenticated() {
        let backend = MockBackend::with_scripts(vec![MockScript::single_shot("ack")]);

        let verdict = verify_worker_auth(&backend, &candidate_env()).await;

        assert_eq!(verdict, AuthVerdict::Authenticated);
        // The candidate env is what was actually handed to the backend.
        let started = backend.started_specs();
        assert_eq!(started.len(), 1);
        assert_eq!(
            started[0].env.get("HOME").map(String::as_str),
            Some("/tmp/kranz-scratch-home")
        );
    }

    #[tokio::test]
    async fn verify_worker_auth_not_logged_in_zero_activity_is_unauthenticated() {
        let script = MockScript {
            events: vec![
                mock_init("mock-session"),
                mock_result_error("Not logged in"),
            ],
            ..Default::default()
        };
        let backend = MockBackend::with_scripts(vec![script]);

        let verdict = verify_worker_auth(&backend, &candidate_env()).await;

        assert_eq!(verdict, AuthVerdict::Unauthenticated);
    }

    #[tokio::test]
    async fn verify_worker_auth_zero_activity_zero_cost_is_unauthenticated() {
        let mut zero_cost_error = mock_result_error("session ended");
        if let AgentEvent::Result { cost_usd, .. } = &mut zero_cost_error {
            *cost_usd = Some(0.0);
        }
        let script = MockScript {
            events: vec![mock_init("mock-session"), zero_cost_error],
            ..Default::default()
        };
        let backend = MockBackend::with_scripts(vec![script]);

        let verdict = verify_worker_auth(&backend, &candidate_env()).await;

        assert_eq!(verdict, AuthVerdict::Unauthenticated);
    }

    #[tokio::test]
    async fn verify_worker_auth_backend_start_error_is_inconclusive() {
        // No scripts queued: MockBackend::start errors with "no script queued".
        let backend = MockBackend::new();

        let verdict = verify_worker_auth(&backend, &candidate_env()).await;

        assert_eq!(verdict, AuthVerdict::Inconclusive);
    }

    #[tokio::test]
    async fn verify_worker_auth_ambiguous_success_with_no_text_is_inconclusive() {
        // Successful result but no assistant text at all: ambiguous, not a
        // confident Authenticated call.
        let script = MockScript {
            events: vec![mock_init("mock-session"), mock_result_text("")],
            ..Default::default()
        };
        let backend = MockBackend::with_scripts(vec![script]);

        let verdict = verify_worker_auth(&backend, &candidate_env()).await;

        assert_eq!(verdict, AuthVerdict::Inconclusive);
    }

    #[test]
    fn has_auth_failure_signature_is_case_insensitive() {
        assert!(has_auth_failure_signature("Not Logged In"));
        assert!(has_auth_failure_signature("ERROR: not authenticated"));
        assert!(!has_auth_failure_signature("ack"));
    }
}
