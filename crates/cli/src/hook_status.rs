//! `kranz hook-status` — the command cursor CLI lifecycle hooks invoke
//! inside agent sessions (ticket
//! `.kranz/tickets/agent-hooks-status-signals`; the engine side is
//! [`kranz_engine::hook_status`], which also records the verified cursor
//! hook surface this relays).
//!
//! This is an INTERNAL plumbing command, never an operator surface: the
//! backend installs it into the session-private `~/.cursor/hooks.json` as a
//! command hook on the mapped lifecycle events (`sessionStart`, `stop`,
//! `sessionEnd`, `postToolUseFailure`). The cursor CLI pipes the hook
//! payload JSON to stdin; the relay bounds the read, maps the payload to
//! a coarse [`kranz_engine::hook_status::HookSignal`], and POSTs
//! `{token, missionId, runId, signal, detail}` to the loopback endpoint
//! named in the engine-written spec file.
//!
//! Exit posture differs from `kranz hook-guard` ON PURPOSE: the guard is a
//! gate (exit 2 blocks), this relay is pure observability — EVERY failure
//! (unreadable spec, oversized/unparseable payload, unreachable endpoint,
//! rejected POST) exits **0** with a stderr note. Cursor hook semantics:
//! exit 2 would block the session's action, and any other non-zero code
//! lands a hook-error notice in the transcript; neither is worth it for a
//! signal that is never mission state. The relay also runs with the
//! session's already-cleared environment and reads nothing but the spec
//! file and stdin — no new credential or env channel (the spec's per-run
//! capability token is the whole authority, and it can only write that
//! one run's projection entry).

use kranz_engine::hook_status::{HookStatusSpec, SignalPost, STDIN_PAYLOAD_MAX_BYTES};
use std::io::Read;
use std::path::Path;
use std::time::Duration;

/// Bounds the endpoint round-trip: a wedged or absent server must never
/// hold the session's hook process (and through it, the session) open.
const POST_TIMEOUT: Duration = Duration::from_secs(5);

/// Run the relay: read the hook payload from `stdin`, map it, POST the
/// signal to the spec's endpoint. Always returns 0 (see module docs);
/// `post_fn` is the test seam scripting endpoint outcomes without a
/// network.
pub async fn run_hook_status(
    config: &Path,
    stdin: &mut impl Read,
    post_fn: &impl AsyncFn(&str, &SignalPost) -> Result<(), String>,
) -> i32 {
    let spec = match HookStatusSpec::load(config) {
        Ok(spec) => spec,
        Err(e) => {
            eprintln!(
                "kranz hook-status: failed to load the hook spec {}: {e} \
                 (ignoring; the lane is observational)",
                config.display()
            );
            return 0;
        }
    };

    // Bounded read: the payload is CLI-produced but the channel is
    // session-adjacent — a boundless read_to_string would let a broken or
    // hostile producer exhaust memory in the relay.
    let mut payload_bytes = Vec::new();
    if let Err(e) = stdin
        .take((STDIN_PAYLOAD_MAX_BYTES + 1) as u64)
        .read_to_end(&mut payload_bytes)
    {
        eprintln!("kranz hook-status: failed to read the hook payload on stdin: {e} (ignoring)");
        return 0;
    }
    if payload_bytes.len() > STDIN_PAYLOAD_MAX_BYTES {
        eprintln!(
            "kranz hook-status: hook payload exceeds {} bytes (ignoring)",
            STDIN_PAYLOAD_MAX_BYTES
        );
        return 0;
    }
    let payload: serde_json::Value = match serde_json::from_slice(&payload_bytes) {
        Ok(payload) => payload,
        Err(e) => {
            eprintln!("kranz hook-status: hook payload was not JSON: {e} (ignoring)");
            return 0;
        }
    };

    // Unmapped payloads (unmapped event, malformed shape) are ordinary —
    // the hooks.json installs exactly the events the mapping consumes, but
    // a payload that maps to nothing is ignored, never an error.
    let Some(post) = kranz_engine::hook_status::signal_post_for(&spec, &payload) else {
        return 0;
    };

    if let Err(e) = post_fn(&spec.endpoint, &post).await {
        eprintln!("kranz hook-status: signal POST failed: {e} (ignoring)");
    }
    0
}

/// The real POST: JSON body to the spec's loopback endpoint with a hard
/// timeout. A non-2xx is an Err naming the status (the relay still exits
/// 0 — the note is for the transcript/stderr, never for retry).
pub async fn post_signal(endpoint: &str, post: &SignalPost) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(POST_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(endpoint)
        .json(post)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("endpoint returned {}", response.status()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn write_spec(dir: &Path) -> std::path::PathBuf {
        let spec = HookStatusSpec {
            version: kranz_engine::hook_status::SPEC_VERSION,
            endpoint: "http://127.0.0.1:9/api/hook-status".to_string(),
            token: "tok-1".to_string(),
            mission_id: "m-1".to_string(),
            run_id: "r-1".to_string(),
        };
        let path = dir.join("spec.json");
        std::fs::write(&path, serde_json::to_string_pretty(&spec).unwrap()).unwrap();
        path
    }

    /// A mapped payload is POSTed to the spec's endpoint in the shared wire
    /// shape; every step's failure still exits 0.
    #[tokio::test]
    async fn hook_status_signal_relay_posts_mapped_payloads_and_never_fails() {
        let dir = tempfile::tempdir().unwrap();
        let config = write_spec(dir.path());
        let captured: Arc<Mutex<Vec<(String, serde_json::Value)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let sink = {
            let captured = Arc::clone(&captured);
            move |endpoint: &str, post: &SignalPost| {
                let captured = Arc::clone(&captured);
                let endpoint = endpoint.to_string();
                let post = post.clone();
                async move {
                    captured
                        .lock()
                        .unwrap()
                        .push((endpoint, serde_json::to_value(post).unwrap()));
                    Ok(())
                }
            }
        };

        let payload = serde_json::json!({
            "hook_event_name": "postToolUseFailure",
            "tool_name": "Shell",
            "failure_type": "permission_denied",
        })
        .to_string();
        let code = run_hook_status(&config, &mut payload.as_bytes(), &sink).await;
        assert_eq!(code, 0, "the relay always exits 0");

        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, "http://127.0.0.1:9/api/hook-status");
        let body = &captured[0].1;
        assert_eq!(body["token"], "tok-1");
        assert_eq!(body["missionId"], "m-1");
        assert_eq!(body["runId"], "r-1");
        assert_eq!(body["signal"], "needs-input");
        assert!(body["detail"].as_str().unwrap().contains("Shell"));
    }

    /// Unmapped / malformed / oversized inputs POST nothing and exit 0 —
    /// the malformed-payloads-ignored acceptance hint at the relay seam.
    #[tokio::test]
    async fn hook_status_signal_relay_ignores_malformed_unmapped_and_oversized() {
        let dir = tempfile::tempdir().unwrap();
        let config = write_spec(dir.path());
        let calls = Arc::new(Mutex::new(0usize));
        let counting = {
            let calls = Arc::clone(&calls);
            move |_: &str, _: &SignalPost| {
                let calls = Arc::clone(&calls);
                async move {
                    *calls.lock().unwrap() += 1;
                    Ok(())
                }
            }
        };

        // Not JSON.
        let mut bad = b"{not json".as_slice();
        assert_eq!(run_hook_status(&config, &mut bad, &counting).await, 0);
        // Valid JSON, unmapped event.
        let payload = serde_json::json!({ "hook_event_name": "preCompact" }).to_string();
        assert_eq!(
            run_hook_status(&config, &mut payload.as_bytes(), &counting).await,
            0
        );
        // Oversized body.
        let oversized = vec![b'x'; STDIN_PAYLOAD_MAX_BYTES + 1];
        assert_eq!(
            run_hook_status(&config, &mut oversized.as_slice(), &counting).await,
            0
        );
        // Missing spec file.
        let missing = dir.path().join("no-such-spec.json");
        let payload = serde_json::json!({ "hook_event_name": "sessionStart" }).to_string();
        assert_eq!(
            run_hook_status(&missing, &mut payload.as_bytes(), &counting).await,
            0
        );

        assert_eq!(*calls.lock().unwrap(), 0, "nothing was POSTed");
    }

    /// A failing endpoint (connection refused, non-2xx) is a stderr note,
    /// never a non-zero exit: the session must never feel the lane.
    #[tokio::test]
    async fn hook_status_signal_relay_swallows_endpoint_failures() {
        let dir = tempfile::tempdir().unwrap();
        let config = write_spec(dir.path());
        let failing = |_: &str, _: &SignalPost| async move {
            Err("endpoint returned 403".to_string()) as Result<(), String>
        };
        let payload = serde_json::json!({ "hook_event_name": "sessionStart" }).to_string();
        assert_eq!(
            run_hook_status(&config, &mut payload.as_bytes(), &failing).await,
            0
        );
    }
}
