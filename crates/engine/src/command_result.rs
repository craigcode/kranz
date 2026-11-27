//! Machine-readable contract command results (plan §3, roadmap M5).
//!
//! Every engine-run contract command (validation round, not-yet-due run,
//! final gate) produces a [`ContractCommandRun`](crate::command_exec::ContractCommandRun)
//! with raw process-level detail. This module turns that run into a stable,
//! serializable [`ContractCommandResult`] suitable for the mission's event
//! log: the assertion id, stage, exit code or signal, a could-not-start or
//! timed-out marker, the duration, scrubbed byte-bounded stdout/stderr tails,
//! and a [`failure_digest`] so two runs of the same failing check can be
//! compared as strings without re-running anything.
//!
//! The digest is computed from the *stored* (already scrubbed, already
//! bounded) tails — anyone holding the event can recompute it themselves.
//! Duration-shaped tokens (`0.53s`, `120ms`, ...) are normalised out of the
//! tails before hashing so two runs that differ only in how long they took
//! still compare equal.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// Maximum number of bytes kept from the end of a stdout/stderr tail.
pub const TAIL_MAX_BYTES: usize = 4096;

/// Which contract phase a command ran for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommandStage {
    Validation,
    #[serde(rename = "validation-notdue")]
    ValidationNotdue,
    #[serde(rename = "final-gate")]
    FinalGate,
}

/// A machine-readable record of one contract command run, suitable for the
/// event log. Every field beyond `assertion_id` and `stage` defaults, so a
/// minimal `{"assertionId": ..., "stage": ...}` payload deserializes cleanly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContractCommandResult {
    pub assertion_id: String,
    pub stage: CommandStage,
    #[serde(default)]
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    #[serde(default)]
    pub timed_out: bool,
    #[serde(default)]
    pub could_not_start: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_error: Option<String>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub stdout_tail: String,
    #[serde(default)]
    pub stderr_tail: String,
    #[serde(default)]
    pub stdout_truncated: bool,
    #[serde(default)]
    pub stderr_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_digest: Option<String>,
}

/// Scrubs `text`, then keeps at most [`TAIL_MAX_BYTES`] bytes from the end,
/// cutting on a UTF-8 char boundary. Returns the bounded text and whether
/// anything was cut.
#[allow(dead_code)] // wired by contract.command.result emission
pub(crate) fn bound_tail(text: &str) -> (String, bool) {
    let scrubbed = crate::scrub::scrub(text);
    if scrubbed.len() <= TAIL_MAX_BYTES {
        return (scrubbed, false);
    }
    let mut start = scrubbed.len() - TAIL_MAX_BYTES;
    while start < scrubbed.len() && !scrubbed.is_char_boundary(start) {
        start += 1;
    }
    (scrubbed[start..].to_string(), true)
}

fn duration_token_re() -> &'static regex::Regex {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"\b\d+(?:\.\d+)?(?:ns|us|µs|ms|s)\b").expect("valid regex")
    })
}

/// Normalises text for digest comparison: CRLF/CR become LF, trailing
/// whitespace is stripped per line, trailing empty lines are dropped, and
/// duration tokens are replaced with `<dur>`.
#[allow(dead_code)] // wired by contract.command.result emission
pub(crate) fn normalise_for_digest(text: &str) -> String {
    let unified = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<String> = unified
        .split('\n')
        .map(|line| {
            let trimmed = line.trim_end();
            duration_token_re()
                .replace_all(trimmed, "<dur>")
                .into_owned()
        })
        .collect();
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// The outcome label used in the failure digest for a run with the given
/// code/signal/could-not-start/timed-out combination.
fn outcome_label(
    code: Option<i32>,
    signal: Option<i32>,
    timed_out: bool,
    could_not_start: bool,
) -> &'static str {
    if could_not_start {
        "could-not-start"
    } else if timed_out {
        "timed-out"
    } else if code.is_some() {
        "exited"
    } else if signal.is_some() {
        "signalled"
    } else {
        "lost"
    }
}

/// Computes a stable `sha256:`-prefixed digest from the canonical,
/// length-prefixed encoding of the outcome, exit code, signal, and the
/// normalised start error / stdout tail / stderr tail, in that order.
#[allow(dead_code)] // wired by contract.command.result emission
pub(crate) fn failure_digest(
    outcome: &str,
    exit_code: Option<i32>,
    signal: Option<i32>,
    start_error: &str,
    stdout_tail: &str,
    stderr_tail: &str,
) -> String {
    let mut hasher = Sha256::new();
    let mut field = |value: &str| {
        hasher.update((value.len() as u64).to_le_bytes());
        hasher.update(value.as_bytes());
    };
    field(outcome);
    field(&exit_code.map_or_else(|| "-".to_string(), |c| c.to_string()));
    field(&signal.map_or_else(|| "-".to_string(), |s| s.to_string()));
    field(&normalise_for_digest(start_error));
    field(&normalise_for_digest(stdout_tail));
    field(&normalise_for_digest(stderr_tail));
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// Builds a [`ContractCommandResult`] from a completed
/// [`ContractCommandRun`](crate::command_exec::ContractCommandRun).
#[allow(dead_code)] // wired by contract.command.result emission
pub(crate) fn from_run(
    assertion_id: &str,
    stage: CommandStage,
    run: &crate::command_exec::ContractCommandRun,
) -> ContractCommandResult {
    let passed = !run.timed_out && run.code == Some(0);
    let (stdout_tail, stdout_truncated) = bound_tail(&run.stdout_tail);
    let (stderr_tail, stderr_truncated) = bound_tail(&run.stderr_tail);
    let could_not_start = run.could_not_start.is_some();
    let start_error = run
        .could_not_start
        .as_deref()
        .map(|text| bound_tail(text).0);
    let exit_code = if could_not_start { None } else { run.code };
    let signal = if could_not_start { None } else { run.signal };
    let duration_ms = run.duration.as_millis() as u64;

    let failure_digest = if passed {
        None
    } else {
        let outcome = outcome_label(exit_code, signal, run.timed_out, could_not_start);
        Some(failure_digest(
            outcome,
            exit_code,
            signal,
            start_error.as_deref().unwrap_or(""),
            &stdout_tail,
            &stderr_tail,
        ))
    };

    ContractCommandResult {
        assertion_id: assertion_id.to_string(),
        stage,
        passed,
        exit_code,
        signal,
        timed_out: run.timed_out,
        could_not_start,
        start_error,
        duration_ms,
        stdout_tail,
        stderr_tail,
        stdout_truncated,
        stderr_truncated,
        failure_digest,
    }
}

/// Builds a [`ContractCommandResult`] for an assertion with `check=command`
/// but no command text to run.
#[allow(dead_code)] // wired by contract.command.result emission
pub(crate) fn missing_command(assertion_id: &str, stage: CommandStage) -> ContractCommandResult {
    let start_error = "assertion has check=command but no command";
    let digest = failure_digest("could-not-start", None, None, start_error, "", "");
    ContractCommandResult {
        assertion_id: assertion_id.to_string(),
        stage,
        passed: false,
        exit_code: None,
        signal: None,
        timed_out: false,
        could_not_start: true,
        start_error: Some(start_error.to_string()),
        duration_ms: 0,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        failure_digest: Some(digest),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_exec::ContractCommandRun;
    use std::time::Duration;

    fn base_run() -> ContractCommandRun {
        ContractCommandRun {
            code: Some(3),
            output: String::new(),
            timed_out: false,
            signal: None,
            could_not_start: None,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            duration: Duration::from_millis(100),
        }
    }

    #[test]
    fn command_result_digest_equal_modulo_durations_and_trailing_whitespace() {
        let mut a = base_run();
        a.stderr_tail = "boom\nfinished in 0.53s   \n".to_string();
        let mut b = base_run();
        b.stderr_tail = "boom\r\nfinished in 1.20s\r\n".to_string();

        let ra = from_run("a-1", CommandStage::Validation, &a);
        let rb = from_run("a-1", CommandStage::Validation, &b);

        assert_eq!(ra.failure_digest, rb.failure_digest);
        assert!(ra.failure_digest.is_some());
    }

    #[test]
    fn command_result_digest_differs_on_exit_code_or_tail() {
        let base = base_run();
        let mut different_code = base_run();
        different_code.code = Some(4);
        let mut different_tail = base_run();
        different_tail.stderr_tail = "different".to_string();

        let r_base = from_run("a-1", CommandStage::Validation, &base);
        let r_code = from_run("a-1", CommandStage::Validation, &different_code);
        let r_tail = from_run("a-1", CommandStage::Validation, &different_tail);

        assert_ne!(r_base.failure_digest, r_code.failure_digest);
        assert_ne!(r_base.failure_digest, r_tail.failure_digest);

        let mut signalled_9 = base_run();
        signalled_9.code = None;
        signalled_9.signal = Some(9);
        let mut signalled_15 = base_run();
        signalled_15.code = None;
        signalled_15.signal = Some(15);

        let r_signal_9 = from_run("a-1", CommandStage::Validation, &signalled_9);
        let r_signal_15 = from_run("a-1", CommandStage::Validation, &signalled_15);

        assert_ne!(r_signal_9.failure_digest, r_signal_15.failure_digest);
    }

    #[test]
    fn command_result_digest_absent_on_pass() {
        let mut run = base_run();
        run.code = Some(0);
        let result = from_run("a-1", CommandStage::Validation, &run);
        assert!(result.passed);
        assert_eq!(result.failure_digest, None);
    }

    #[test]
    fn command_result_digest_format_is_sha256_hex() {
        let run = base_run();
        let result = from_run("a-1", CommandStage::Validation, &run);
        let digest = result.failure_digest.expect("failing run has a digest");
        let re = regex::Regex::new(r"^sha256:[0-9a-f]{64}$").unwrap();
        assert!(re.is_match(&digest), "{digest}");
    }

    #[test]
    fn command_result_tail_bounded_to_4096_bytes_keeps_end_on_char_boundary() {
        let multibyte = "é".repeat(3000); // 2 bytes each => 6000 bytes, straddles cut
        let mut run = base_run();
        run.stdout_tail = format!("{multibyte}END");

        let result = from_run("a-1", CommandStage::Validation, &run);

        assert!(result.stdout_truncated);
        assert!(result.stdout_tail.len() <= TAIL_MAX_BYTES);
        assert!(result.stdout_tail.ends_with("END"));
        // Validity asserted implicitly: `String` is always valid UTF-8.

        let short = "short tail".to_string();
        let mut short_run = base_run();
        short_run.stdout_tail = short.clone();
        let short_result = from_run("a-1", CommandStage::Validation, &short_run);
        assert!(!short_result.stdout_truncated);
        assert_eq!(short_result.stdout_tail, short);
    }

    #[test]
    fn command_result_tail_redacts_secret_before_bounding() {
        let secret = ["sk-ant-api03-", &"A".repeat(40)].concat();
        let filler = "x ".repeat((TAIL_MAX_BYTES - 20) / 2);
        let text = format!("{secret}{filler} tail-end");
        let mut run = base_run();
        run.stderr_tail = text.clone();

        // Control assertion: proves the secret straddles the raw last-4096-byte
        // cut point, so bounding before scrubbing would leak part of it.
        let raw_window = &text[text.len() - TAIL_MAX_BYTES..];
        assert!(raw_window.contains("AAAAAAAAAA"));
        assert!(!raw_window.contains("sk-ant-"));

        let result = from_run("a-1", CommandStage::Validation, &run);

        assert!(!result.stderr_tail.contains("AAAAAAAAAA"));
        assert!(result.stderr_tail.len() <= TAIL_MAX_BYTES);
    }

    #[test]
    fn command_result_could_not_start_record_has_no_exit_code() {
        let mut run = base_run();
        run.code = None;
        run.could_not_start = Some("spawn failed: no such file".to_string());
        let result = from_run("a-1", CommandStage::Validation, &run);

        assert!(result.could_not_start);
        assert_eq!(result.exit_code, None);
        assert_eq!(result.signal, None);
        assert_eq!(
            result.start_error.as_deref(),
            Some("spawn failed: no such file")
        );
        assert!(result.failure_digest.is_some());
    }

    #[test]
    fn command_result_minimal_json_deserializes() {
        let json = r#"{"assertionId":"a-1","stage":"final-gate"}"#;
        let result: ContractCommandResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.assertion_id, "a-1");
        assert_eq!(result.stage, CommandStage::FinalGate);
        assert!(!result.passed);
        assert_eq!(result.exit_code, None);
        assert_eq!(result.signal, None);
        assert!(!result.timed_out);
        assert!(!result.could_not_start);
        assert_eq!(result.start_error, None);
        assert_eq!(result.duration_ms, 0);
        assert_eq!(result.stdout_tail, "");
        assert_eq!(result.stderr_tail, "");
        assert!(!result.stdout_truncated);
        assert!(!result.stderr_truncated);
        assert_eq!(result.failure_digest, None);
    }

    #[test]
    fn command_result_stage_serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&CommandStage::Validation).unwrap(),
            "\"validation\""
        );
        assert_eq!(
            serde_json::to_string(&CommandStage::ValidationNotdue).unwrap(),
            "\"validation-notdue\""
        );
        assert_eq!(
            serde_json::to_string(&CommandStage::FinalGate).unwrap(),
            "\"final-gate\""
        );
    }
}
