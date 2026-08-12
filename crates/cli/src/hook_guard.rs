//! `kranz hook-guard` — the command Claude Code lifecycle hooks invoke
//! inside worker sessions (ticket
//! `.kranz/tickets/claude-code-hook-gate-projection.md`, KRZ-302; the engine
//! side is [`kranz_engine::hook_gates`], which also records the targeted
//! hooks schema version).
//!
//! This is an INTERNAL plumbing command, never an operator surface: the
//! engine installs it into a worker session's `--settings` JSON as a
//! `PreToolUse` hook on the file-writing tools. The Claude Code CLI pipes
//! the hook payload JSON to stdin; the guard judges the tool call's target
//! path against the per-session spec file the engine wrote, appends a
//! structured record to the session's record file, and exits:
//!
//! - **0** — the write is in contract; the normal permission flow proceeds.
//! - **2** — BLOCK: stderr is fed back to the model as the refusal reason;
//!   a `blocked` record was appended (the engine folds it into a
//!   `hook.gate.fired` event after the session).
//! - **1** — the GUARD itself failed (unreadable spec, unparseable
//!   payload): a non-blocking error in Claude Code, so the action proceeds
//!   and a hook-error notice lands in the transcript. Fail-OPEN by design —
//!   a broken guard must never freeze a session, and the miss is still
//!   judged by the authoritative engine-side out-of-contract sweep.
//!
//! The guard runs with the session's already-cleared environment and reads
//! nothing but the spec file and stdin — no new credential or env channel.

use kranz_engine::hook_gates::{GuardVerdict, HookGateRecord, HookGateSpec};
use kranz_engine::hook_status::STDIN_PAYLOAD_MAX_BYTES;
use std::io::Read;
use std::path::Path;

/// Exit code: the guard itself failed open (non-blocking in Claude Code).
const EXIT_GUARD_ERROR: i32 = 1;
/// Exit code: the tool call is BLOCKED; stderr is shown to the model.
const EXIT_BLOCK: i32 = 2;

/// Run the guard: read the hook payload from `stdin`, judge it against the
/// spec at `config`, record the outcome, and return the process exit code.
/// Split from the clap dispatch so tests drive it in-process.
pub fn run_hook_guard(config: &Path, stdin: &mut impl Read) -> i32 {
    let spec = match HookGateSpec::load(config) {
        Ok(spec) => spec,
        Err(e) => {
            eprintln!(
                "kranz hook-guard: failed to load the hook spec {}: {e} \
                 (failing open; the engine-side out-of-contract sweep remains authoritative)",
                config.display()
            );
            return EXIT_GUARD_ERROR;
        }
    };

    // Bounded read — the same idiom and cap as the hook-status relay
    // (crates/cli/src/hook_status.rs, 14th-pass review: this read was
    // unbounded): the payload is CLI-produced but the channel is
    // session-adjacent, so a boundless read would let a broken or hostile
    // producer exhaust memory in the guard. Over the cap fails OPEN like
    // any guard error — enforcement never silently blocks on guard failure;
    // the engine-side sweep stays authoritative.
    let mut payload_bytes = Vec::new();
    if let Err(e) = stdin
        .take((STDIN_PAYLOAD_MAX_BYTES + 1) as u64)
        .read_to_end(&mut payload_bytes)
    {
        return guard_error(
            &spec,
            &format!("failed to read the hook payload on stdin: {e}"),
        );
    }
    if payload_bytes.len() > STDIN_PAYLOAD_MAX_BYTES {
        return guard_error(
            &spec,
            &format!("hook payload exceeds {STDIN_PAYLOAD_MAX_BYTES} bytes"),
        );
    }
    let payload: serde_json::Value = match serde_json::from_slice(&payload_bytes) {
        Ok(payload) => payload,
        Err(e) => {
            return guard_error(&spec, &format!("hook payload was not JSON: {e}"));
        }
    };
    let Some(tool_name) = payload.get("tool_name").and_then(|v| v.as_str()) else {
        return guard_error(&spec, "hook payload carried no tool_name");
    };
    let hook_event = payload
        .get("hook_event_name")
        .and_then(|v| v.as_str())
        .unwrap_or("PreToolUse");
    // Write/Edit carry `file_path`; older CLIs' NotebookEdit carried
    // `notebook_path` — accept either (defensive: its shape is undocumented).
    let file_path = payload
        .pointer("/tool_input/file_path")
        .or_else(|| payload.pointer("/tool_input/notebook_path"))
        .and_then(|v| v.as_str());
    let session_id = payload.get("session_id").and_then(|v| v.as_str());
    let tool_use_id = payload.get("tool_use_id").and_then(|v| v.as_str());

    match kranz_engine::hook_gates::evaluate(&spec, tool_name, file_path) {
        GuardVerdict::Allow => 0,
        GuardVerdict::Block { subject, reason } => {
            let record = HookGateRecord::blocked(
                &spec,
                hook_event,
                tool_name,
                &subject,
                &reason,
                session_id,
                tool_use_id,
            );
            // Best-effort: an append failure must not change the verdict —
            // the block still stands (and lands in the transcript), only
            // the structured event is lost.
            let _ = record.append_to(&spec.record_file);
            // stderr is fed back to the model on exit 2: the reason tells it
            // how to recover (relocate the write, or surface a grant need).
            eprintln!("{reason}");
            EXIT_BLOCK
        }
    }
}

/// The fail-open branch: record the guard error (best-effort) and exit
/// non-blocking, so a broken guard never freezes the session and the miss
/// stays visible to the engine-side sweep.
fn guard_error(spec: &HookGateSpec, note: &str) -> i32 {
    let _ = HookGateRecord::error(spec, note).append_to(&spec.record_file);
    eprintln!("kranz hook-guard: {note} (failing open)");
    EXIT_GUARD_ERROR
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_spec(dir: &Path, touch_set: &[&str]) -> std::path::PathBuf {
        let spec = HookGateSpec {
            version: kranz_engine::hook_gates::SPEC_VERSION,
            gate: kranz_engine::hook_gates::HOOK_GATE_ID.to_string(),
            session_cwd: dir.to_path_buf(),
            touch_set: touch_set.iter().map(|s| s.to_string()).collect(),
            record_file: dir.join("records.jsonl"),
        };
        let path = dir.join("spec.json");
        std::fs::write(&path, serde_json::to_string_pretty(&spec).unwrap()).unwrap();
        path
    }

    fn payload(tool: &str, file_path: Option<&str>) -> String {
        let input = match file_path {
            Some(path) => serde_json::json!({ "file_path": path }),
            None => serde_json::json!({}),
        };
        serde_json::json!({
            "session_id": "cli-session-1",
            "transcript_path": "/tmp/t.jsonl",
            "cwd": "/tmp",
            "hook_event_name": "PreToolUse",
            "tool_name": tool,
            "tool_input": input,
            "tool_use_id": "toolu_1",
        })
        .to_string()
    }

    /// An out-of-contract Write is BLOCKED (exit 2) and lands in the record
    /// file as a structured `blocked` record; an in-contract Write exits 0
    /// and records nothing.
    #[test]
    fn hook_gate_projection_guard_blocks_and_records_out_of_contract_writes() {
        let dir = tempfile::tempdir().unwrap();
        let config = write_spec(dir.path(), &["src/**"]);

        let stdin = payload("Write", Some("/outside/the/checkout.md")).into_bytes();
        // Path outside the checkout → blocked. (session_cwd is the tempdir.)
        let code = run_hook_guard(&config, &mut stdin.as_slice());
        assert_eq!(code, 2);
        let records = std::fs::read_to_string(dir.path().join("records.jsonl")).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(records.lines().next().unwrap()).unwrap();
        assert_eq!(record["verdict"], "blocked");
        assert_eq!(record["gate"], kranz_engine::hook_gates::HOOK_GATE_ID);
        assert_eq!(record["hookEvent"], "PreToolUse");
        assert_eq!(record["tool"], "Write");
        assert_eq!(record["sessionId"], "cli-session-1");
        assert_eq!(record["toolUseId"], "toolu_1");

        // In-contract relative path → allowed, no record appended.
        let stdin = payload("Edit", Some("src/lib.rs")).into_bytes();
        let code = run_hook_guard(&config, &mut stdin.as_slice());
        assert_eq!(code, 0);
        let records = std::fs::read_to_string(dir.path().join("records.jsonl")).unwrap();
        assert_eq!(records.lines().count(), 1, "an allow records nothing");

        // Out-of-contract relative path → blocked, repo-relative subject.
        let stdin = payload("Write", Some("docs/oops.md")).into_bytes();
        let code = run_hook_guard(&config, &mut stdin.as_slice());
        assert_eq!(code, 2);
        let records = std::fs::read_to_string(dir.path().join("records.jsonl")).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(records.lines().nth(1).unwrap()).unwrap();
        assert_eq!(record["subject"], "docs/oops.md");
    }

    /// Guard failures fail OPEN (exit 1, non-blocking in Claude Code) and
    /// leave an `error` record when the spec was loadable — never a frozen
    /// session, never a silent miss.
    #[test]
    fn hook_gate_projection_guard_failures_fail_open_loudly() {
        let dir = tempfile::tempdir().unwrap();
        let config = write_spec(dir.path(), &["src/**"]);

        // Unparseable payload → exit 1 + error record.
        let mut stdin = b"{not json".as_slice();
        let code = run_hook_guard(&config, &mut stdin);
        assert_eq!(code, 1);
        let records = std::fs::read_to_string(dir.path().join("records.jsonl")).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(records.lines().next().unwrap()).unwrap();
        assert_eq!(record["verdict"], "error");

        // A missing spec file → exit 1 (no record possible: the record file
        // path lives in the spec).
        let missing = dir.path().join("no-such-spec.json");
        let stdin = payload("Write", Some("src/lib.rs")).into_bytes();
        let code = run_hook_guard(&missing, &mut stdin.as_slice());
        assert_eq!(code, 1);
    }

    /// 14th-pass review: the stdin read is bounded like the hook-status
    /// relay's — an oversized payload fails OPEN (exit 1, an `error`
    /// record), never an unbounded buffer in the guard.
    #[test]
    fn hook_guard_stdin_read_is_bounded_fail_open() {
        let dir = tempfile::tempdir().unwrap();
        let config = write_spec(dir.path(), &["src/**"]);

        let oversized = vec![b'x'; STDIN_PAYLOAD_MAX_BYTES + 1];
        let code = run_hook_guard(&config, &mut oversized.as_slice());
        assert_eq!(code, 1, "over the cap is a guard error, failing open");
        let records = std::fs::read_to_string(dir.path().join("records.jsonl")).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(records.lines().next().unwrap()).unwrap();
        assert_eq!(record["verdict"], "error");

        // Exactly AT the cap the read still proceeds (and fails open on the
        // non-JSON bytes) — the bound does not eat legitimate payloads.
        let at_cap = vec![b'x'; STDIN_PAYLOAD_MAX_BYTES];
        let code = run_hook_guard(&config, &mut at_cap.as_slice());
        assert_eq!(code, 1, "at the cap the payload is read and judged");
    }
}
