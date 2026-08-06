//! Pty-driven functional validation (ticket `.kranz/tickets/pty-functional-validation.md`):
//! terminal-interactive deliverables — REPLs, TUIs, interactive CLIs — are
//! driven by the engine through a scripted pty session, and the functional
//! validator judges the per-step verdicts as authoritative evidence, exactly
//! like it judges engine-run contract-command output (validator repair 3/5).
//!
//! This is VALIDATOR tooling, not an execution feature: the script judges
//! what the delivered software DOES on a terminal and never feeds work back
//! into the mission (the positioning ADR's retained list — same carve-out
//! the M5 browser/computer-use lane already occupies). Nothing here spawns
//! an agent.
//!
//! ## Mechanism decision (minimal-dependency posture)
//!
//! No pty crate is in the tree (`portable-pty` was the candidate — it would
//! add `anyhow`/`filedescriptor`/`shared_library`/`winapi`-adjacent deps to
//! buy Windows ConPTY this ticket does not need). The harness therefore
//! lives on `std::process` plus the platform's own pty facility through
//! `libc::openpty` — `libc` is ALREADY the engine's unix dependency, so the
//! whole mechanism is one contained module with no new dependency. The cost
//! is platform coverage: the session core is `#[cfg(unix)]`, and non-unix
//! hosts degrade LOUDLY — every pty-script assertion renders a SKIP line
//! naming the platform gap (the same posture uncontainable
//! platforms/backends take for validator containment), never a silent pass.
//!
//! ## Sandbox composition
//!
//! A pty session runs under the SAME policy as the contract commands in the
//! same evidence pass: the orchestrator hands the harness the argv that
//! `GateSandbox::wrap_shell` produces for the script's command (plus the
//! offline-adjusted gate env via [`crate::command_exec::prepare_gate_command`]), so `enforce: off`
//! reproduces the pre-wrap `sh -c` byte-for-byte and an enforced posture
//! puts the target inside the same seatbelt/bwrap/container wrap its
//! sibling commands get. The pty ALLOCATION lives in the engine process;
//! only the target tree is wrapped, and the harness never widens what the
//! wrap allows. The wrapped child leads a new session (`setsid` +
//! `TIOCSCTTY`), so the end-of-script SIGKILL reaches the whole group, and
//! the container arm's named teardown runs on a killed client exactly as in
//! the bounded runner.
//!
//! ## Script format
//!
//! Declared inline on the contract assertion ([`crate::types::PtyScript`]):
//! `send` steps write bytes verbatim, `expect` steps assert the accumulated
//! session output contains a literal substring (or matches a regex) within
//! a per-step timeout. The transcript is the pty's raw output stream —
//! terminal echo means sent input appears naturally for canonical-mode
//! targets — bounded at [`MAX_TRANSCRIPT_BYTES`], written under the
//! mission's gitignored `runs/pty-transcripts/`, and referenced from a
//! `validation.pty.transcript` event with the `file:`-scheme ArtefactRef
//! idiom ([`crate::gate_results`]). The transcript FILE is raw target
//! output (runs/ is gitignored scratch, same posture as session transcript
//! .jsonl files); the per-step verdict text handed to the validator and the
//! event detail carries no target output beyond the scrubbed failure tail.
//!
//! ## Skip discipline
//!
//! A contract with no pty-script assertions takes today's path byte-for-byte
//! (no harness, no artifacts, no events) — "targets with no declared run
//! harness skip cleanly" is the absence case, mirroring browser QA. A
//! declared target that fails to spawn is NOT a skip: the deliverable
//! declared runnable does not run, which FAILS the assertion honestly.

use crate::command_exec::GateSandbox;
use crate::types::{Assertion, AssertionCheck, PtyScript};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

/// Default wall-clock cap for one whole scripted session.
pub const DEFAULT_SESSION_TIMEOUT_SECS: u64 = 60;
/// Default per-`expect` timeout.
pub const DEFAULT_EXPECT_TIMEOUT_MS: u64 = 10_000;
/// Transcript bound: output past this is discarded (the `truncated` flag
/// records it), so a runaway target cannot fill the mission dir or the
/// evidence record. 256 KiB holds hours of REPL interaction and minutes of
/// full-screen redraw.
pub const MAX_TRANSCRIPT_BYTES: usize = 256 * 1024;
/// The scrubbed transcript tail attached to a FAILED assertion's evidence —
/// enough to judge the mismatch, bounded so the rendered block stays small.
const FAIL_TAIL_BYTES: usize = 2048;
/// Poll cadence of the drive loop: fine enough to catch prompt output
/// promptly, coarse enough to never busy-spin.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// The session-level verdict of one pty-script assertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PtyVerdict {
    /// Every `expect` step matched within its timeout.
    Pass,
    /// An `expect` step missed (timeout, target exit, invalid pattern), a
    /// `send` could not be delivered, or the target failed to spawn.
    Fail,
    /// The harness could not run at all on this host (non-unix platform) —
    /// rendered loudly, never counted as pass or fail.
    Skipped,
}

/// The per-step verdict record. One entry per EXECUTED step (the run stops
/// at the first failed `expect`, so a failing run's last entry is the
/// failure; `send` steps are recorded too so the validator sees how far the
/// script drove).
#[derive(Debug, Clone)]
pub struct PtyStepOutcome {
    /// Zero-based index into the script's `steps`.
    pub step: usize,
    pub ok: bool,
    /// What happened, naming the step — e.g. `expect `> ` matched (812ms)`
    /// or `expect `echo:hello` timed out after 500ms`.
    pub detail: String,
}

/// What one scripted pty session produced: the verdict, per-step verdicts,
/// and the bounded transcript.
#[derive(Debug)]
pub struct PtyRunOutcome {
    pub verdict: PtyVerdict,
    pub steps: Vec<PtyStepOutcome>,
    /// Raw pty output bytes, capped at [`MAX_TRANSCRIPT_BYTES`].
    pub transcript: Vec<u8>,
    /// True when output past the cap was discarded.
    pub truncated: bool,
    /// Session-level context: spawn failure reason, target exit status,
    /// harness termination note.
    pub note: Option<String>,
}

/// One transcript artifact ready for event emission: the assertion, the
/// verdict, and the mission-relative path the transcript was written to
/// (the `file:` scheme is glued on at emit via
/// [`crate::gate_results::file_artefact_ref`]).
#[derive(Debug)]
pub(crate) struct PtyAssertionArtifact {
    pub assertion_id: String,
    pub pass: bool,
    /// Mission-relative transcript path (`runs/pty-transcripts/<…>.log`).
    pub transcript_rel: String,
    /// Per-step summary (contract-authored patterns and timings only — no
    /// raw target output), carried as the event's `detail`.
    pub detail: String,
}

/// The result of running every pty-script assertion of a validation
/// contract: rendered evidence lines for the functional validator's task
/// (same block the command-assertion results feed) plus the transcript
/// artifacts for event emission. `rendered` is `None` when the contract
/// declares no pty scripts — today's behavior byte-for-byte.
#[derive(Debug)]
pub(crate) struct PtyAssertionRun {
    pub rendered: Option<String>,
    pub artifacts: Vec<PtyAssertionArtifact>,
}

/// Run every pty-script assertion in `contract` as part of the validation
/// round's engine-run evidence pass. Called exactly where the bounded
/// contract commands run, with the same `root`, cleared contract `env`, and
/// resolved `sandbox` — a pty session is posture-identical to a contract
/// command, only interactive. Assertions are driven sequentially (the round
/// is sequential today; parallel ptys would interleave transcript writes
/// and muddy the evidence order).
///
/// Never fails the ROUND: every failure mode lands as a rendered FAIL/SKIP
/// line against the named assertion (evidence for the validator), the same
/// fail-closed-as-evidence posture the bounded command path takes.
pub(crate) async fn run_pty_assertions(
    contract: &[Assertion],
    root: &Path,
    env: &HashMap<String, String>,
    sandbox: &GateSandbox,
    runs_dir: &Path,
) -> PtyAssertionRun {
    let pty_assertions: Vec<&Assertion> = contract
        .iter()
        .filter(|a| a.check == AssertionCheck::PtyScript)
        .collect();
    if pty_assertions.is_empty() {
        return PtyAssertionRun {
            rendered: None,
            artifacts: Vec::new(),
        };
    }
    let mut rendered = String::new();
    let mut artifacts = Vec::new();
    for assertion in pty_assertions {
        let Some(script) = assertion.pty_script.clone() else {
            // Mirrors the `(check=command but no command — cannot run)` arm:
            // a malformed contract entry is rendered, never silently dropped.
            rendered.push_str(&format!(
                "- [{}] (check=pty-script but no pty script — cannot run)\n",
                assertion.id
            ));
            continue;
        };
        // The same final env + wrap the bounded runner computes per command;
        // a wrap failure fails CLOSED as evidence (the session did not run).
        let (wrapped, env) =
            match crate::command_exec::prepare_gate_command(&script.command, env, sandbox) {
                Ok(prepared) => prepared,
                Err(error) => {
                    rendered.push_str(&format!(
                        "- [{}] pty-script `{}` → FAIL\n\
                     gate sandbox wrap failed closed (the pty session did not run): {error}\n",
                        assertion.id, script.command
                    ));
                    continue;
                }
            };
        let root = root.to_path_buf();
        let outcome =
            tokio::task::spawn_blocking(move || imp::run_session(&script, &wrapped, &root, &env))
                .await
                .unwrap_or_else(|join_error| PtyRunOutcome {
                    // A panicking driver must not take the round down — surface it
                    // as an honest FAIL against the assertion instead.
                    verdict: PtyVerdict::Fail,
                    steps: Vec::new(),
                    transcript: Vec::new(),
                    truncated: false,
                    note: Some(format!("pty driver task failed: {join_error}")),
                });
        if outcome.verdict == PtyVerdict::Skipped {
            let note = outcome.note.as_deref().unwrap_or("unsupported host");
            rendered.push_str(&format!(
                "- [{}] pty-script → SKIP ({note})\n",
                assertion.id
            ));
            continue;
        }
        // The transcript lands as a validation artifact regardless of
        // verdict — a FAILING session's transcript is the most valuable
        // evidence of all. A write failure drops the reference (never emit
        // a file: ref whose bytes are absent) but keeps the verdict.
        let transcript_rel = write_transcript(runs_dir, &assertion.id, &outcome);
        let pass = outcome.verdict == PtyVerdict::Pass;
        let detail = step_summary(&outcome);
        let verdict = if pass { "PASS" } else { "FAIL" };
        let reference = transcript_rel
            .as_deref()
            .map(crate::gate_results::file_artefact_ref)
            .unwrap_or_else(|| "(transcript write failed)".to_string());
        rendered.push_str(&format!(
            "- [{}] pty-script `{}` → {verdict} ({detail}; transcript {reference})\n",
            assertion.id,
            script_command_str(assertion),
        ));
        if !pass {
            let tail = tail_text(&outcome.transcript, FAIL_TAIL_BYTES);
            if !tail.is_empty() {
                rendered.push_str(&format!("{}\n", crate::scrub::scrub(&tail)));
            }
        }
        if let Some(rel) = transcript_rel {
            artifacts.push(PtyAssertionArtifact {
                assertion_id: assertion.id.clone(),
                pass,
                transcript_rel: rel,
                detail,
            });
        }
    }
    PtyAssertionRun {
        rendered: Some(rendered),
        artifacts,
    }
}

/// The command string of an assertion's pty script for rendering; the
/// caller only reaches here with `Some`, but stay total anyway.
fn script_command_str(assertion: &Assertion) -> &str {
    assertion
        .pty_script
        .as_ref()
        .map(|s| s.command.as_str())
        .unwrap_or("MISSING")
}

/// The one-line per-step summary carried in the evidence line and the
/// event detail: step kinds, patterns (contract-authored — no target
/// output), and outcomes, joined compactly.
fn step_summary(outcome: &PtyRunOutcome) -> String {
    let mut parts: Vec<String> = outcome
        .steps
        .iter()
        .map(|s| format!("step {} {}", s.step + 1, if s.ok { "ok" } else { "FAILED" }))
        .collect();
    if let Some(failed) = outcome.steps.iter().find(|s| !s.ok) {
        parts.push(format!("({})", failed.detail));
    }
    if let Some(note) = &outcome.note {
        parts.push(format!("({note})"));
    }
    if parts.is_empty() {
        "no steps executed".to_string()
    } else {
        parts.join(" ")
    }
}

/// Write the bounded transcript under `runs/pty-transcripts/` and return
/// the mission-relative path (no scheme). `None` on any io failure — the
/// caller then renders the verdict WITHOUT a file reference rather than
/// emitting one whose bytes are missing (resolve_artefact's unresolved
/// case is for pruned missions, not for bytes we never wrote).
fn write_transcript(
    runs_dir: &Path,
    assertion_id: &str,
    outcome: &PtyRunOutcome,
) -> Option<String> {
    let dir = runs_dir.join("pty-transcripts");
    std::fs::create_dir_all(&dir).ok()?;
    // Assertion ids are plan-authored (`a-1`, `fix-3`); keep the filename
    // charset boring anyway, and suffix a uuid so re-run rounds never
    // overwrite an earlier round's evidence.
    let safe_id: String = assertion_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = format!(
        "{}-{}.log",
        safe_id,
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    );
    let mut bytes = outcome.transcript.clone();
    if outcome.truncated {
        bytes.extend_from_slice(
            format!("\n[kranz: transcript truncated at {MAX_TRANSCRIPT_BYTES} bytes]\n").as_bytes(),
        );
    }
    std::fs::write(dir.join(&name), &bytes).ok()?;
    Some(format!("runs/pty-transcripts/{name}"))
}

/// The last `max` bytes of the transcript as lossy text, for the scrubbed
/// failure tail in the rendered evidence.
fn tail_text(transcript: &[u8], max: usize) -> String {
    let start = transcript.len().saturating_sub(max);
    String::from_utf8_lossy(&transcript[start..]).into_owned()
}

// ---------------------------------------------------------------------------
// Platform cores
// ---------------------------------------------------------------------------

/// The unix session core: allocate a pty pair, spawn the (already wrapped)
/// target with the slave as its controlling terminal, and drive the script
/// against the master. Blocking by design — the caller parks it on
/// `spawn_blocking`; the drive loop is sleep-polled at [`POLL_INTERVAL`].
#[cfg(unix)]
mod imp {
    use super::*;
    use crate::command_exec::WrappedCommand;
    use crate::types::PtyStep;
    use std::io::{Read, Write};
    use std::os::unix::io::FromRawFd;
    use std::os::unix::process::CommandExt;
    use std::time::Instant;

    pub fn run_session(
        script: &PtyScript,
        wrapped: &WrappedCommand,
        cwd: &Path,
        env: &HashMap<String, String>,
    ) -> PtyRunOutcome {
        let mut master: libc::c_int = -1;
        let mut slave: libc::c_int = -1;
        // A fixed 80x24 window: TUIs lay out against the winsize, and a
        // deterministic size keeps transcripts reproducible across hosts.
        let mut winsize = libc::winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: all pointers valid; null `name`/`termp` accept the
        // platform defaults (canonical mode + echo, the boring terminal a
        // REPL expects). The pointer mutability differs across platforms
        // (macOS declares `termp`/`winp` mutable, glibc const), hence the
        // null_mut/&mut shapes that coerce to both.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut winsize,
            )
        };
        if rc != 0 {
            return spawn_failure(format!(
                "openpty failed: {}",
                std::io::Error::last_os_error()
            ));
        }

        // The child gets the slave on stdin/stdout/stderr. dup it twice and
        // hand the original over as the third — each Stdio owns exactly one
        // fd.
        let (dup1, dup2) = unsafe { (libc::dup(slave), libc::dup(slave)) };
        if dup1 == -1 || dup2 == -1 {
            let err = std::io::Error::last_os_error();
            unsafe {
                libc::close(master);
                libc::close(slave);
                if dup1 != -1 {
                    libc::close(dup1);
                }
                if dup2 != -1 {
                    libc::close(dup2);
                }
            }
            return spawn_failure(format!("dup of pty slave failed: {err}"));
        }

        let mut cmd = std::process::Command::new(&wrapped.program);
        cmd.args(&wrapped.args)
            .current_dir(cwd)
            .env_clear()
            .envs(env)
            // SAFETY: from_raw_fd takes ownership of the dup'd fds exactly
            // once each; the originals are not used afterwards.
            .stdin(unsafe { std::process::Stdio::from_raw_fd(slave) })
            .stdout(unsafe { std::process::Stdio::from_raw_fd(dup1) })
            .stderr(unsafe { std::process::Stdio::from_raw_fd(dup2) });
        // New session + the pty slave as controlling terminal: full-screen
        // targets (vim/gdb-style) require a ctty, not merely isatty(stdin),
        // and the session-leader pid doubling as the process-group id is
        // what makes the end-of-script group SIGKILL reach the whole tree
        // (the configure_bounded_child discipline, interactive variant).
        // SAFETY: runs only in the forked child pre-exec; setsid/ioctl are
        // async-signal-safe, and `slave` still names the open pty there
        // (std's fd cleanup runs after pre_exec, right before exec).
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                // The ioctl request parameter is c_ulong on both macOS and
                // linux-gnu, but the TIOCSCTTY constant's type varies (u32
                // on macOS, c_ulong on linux-gnu) — the cast is load-bearing
                // on macOS and an identity on linux, so allow the identity
                // case rather than cfg-split a one-liner.
                #[allow(clippy::unnecessary_cast)]
                let request = libc::TIOCSCTTY as libc::c_ulong;
                if libc::ioctl(slave, request, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(error) => {
                unsafe { libc::close(master) };
                return spawn_failure(format!("target failed to spawn: {error}"));
            }
        };
        let pid = child.id() as i32;

        // The parent drives the master only; nonblocking so the poll loop
        // owns the timing (per-step and whole-session deadlines).
        // SAFETY: master is a valid open fd; O_NONBLOCK is the only change.
        unsafe {
            libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK);
        }
        let mut master = unsafe { std::fs::File::from_raw_fd(master) };

        let mut session = Session {
            transcript: Vec::new(),
            truncated: false,
            child_eof: false,
        };
        let session_deadline = Instant::now()
            + Duration::from_secs(script.timeout_secs.unwrap_or(DEFAULT_SESSION_TIMEOUT_SECS));
        let mut steps = Vec::new();
        let mut failed = false;
        for (index, step) in script.steps.iter().enumerate() {
            let outcome = match step {
                PtyStep::Send { text } => {
                    drive_send(&mut master, &mut session, text, session_deadline, index)
                }
                PtyStep::Expect {
                    pattern,
                    regex,
                    timeout_ms,
                } => drive_expect(
                    &mut master,
                    &mut session,
                    &mut child,
                    pattern,
                    *regex,
                    Duration::from_millis(timeout_ms.unwrap_or(DEFAULT_EXPECT_TIMEOUT_MS)),
                    session_deadline,
                    index,
                ),
            };
            let ok = outcome.ok;
            steps.push(outcome);
            if !ok {
                failed = true;
                break;
            }
        }

        // Termination: a target still running at script end gets the group
        // SIGKILL (session leader's pgid IS its pid); an already-exited
        // target is only reaped. The container arm's named teardown runs
        // exactly when the bounded runner would run it — the client was
        // killed before an exit code arrived.
        let exited = child.try_wait().ok().flatten();
        let note = match exited {
            Some(status) => Some(format!("target exited ({status})")),
            None => {
                // SAFETY: kill(-pid) targets the child's process group —
                // valid while the child is ours; ESRCH (already gone) is
                // harmless.
                unsafe {
                    libc::kill(-pid, libc::SIGKILL);
                }
                let _ = child.kill();
                // Reap WITHOUT wedging: a SIGKILLed pty target can block in
                // kernel exit while its slave-side output queue stays
                // undrained (observed on macOS: the target lingers in
                // 'trying to exit' state and wait() never returns), so pump
                // the master while polling the reap. A target STILL
                // unreaped after the bound — never observed, defense only —
                // is dropped (a SIGKILLed process reaps to init via the
                // zombie path) rather than allowed to hang the validation
                // round in an unbounded wait().
                let reap_deadline = Instant::now() + Duration::from_secs(10);
                let reaped = loop {
                    drain(&mut master, &mut session);
                    if child.try_wait().ok().flatten().is_some() {
                        break true;
                    }
                    if session.child_eof || Instant::now() >= reap_deadline {
                        break false;
                    }
                    std::thread::sleep(POLL_INTERVAL);
                };
                if reaped || child.try_wait().ok().flatten().is_some() {
                    let _ = child.wait();
                } else {
                    tracing::warn!(
                        "pty target did not reap within 10s of SIGKILL despite a drained \
                         pty; dropping the handle (the round continues, the killed target \
                         reaps to init)"
                    );
                }
                if let Some((program, args)) = &wrapped.timeout_teardown {
                    // Best-effort, bounded — the same 30s teardown bound
                    // the bounded runner applies to a killed container
                    // client; a teardown failure is ignored.
                    let _ = crate::command_exec::run_with_timeout(
                        program,
                        args,
                        Duration::from_secs(30),
                    );
                }
                Some("target terminated by harness (script complete)".to_string())
            }
        };

        PtyRunOutcome {
            verdict: if failed {
                PtyVerdict::Fail
            } else {
                PtyVerdict::Pass
            },
            steps,
            transcript: session.transcript,
            truncated: session.truncated,
            note,
        }
    }

    fn spawn_failure(reason: String) -> PtyRunOutcome {
        PtyRunOutcome {
            verdict: PtyVerdict::Fail,
            steps: Vec::new(),
            transcript: Vec::new(),
            truncated: false,
            note: Some(reason),
        }
    }

    /// The mutable driver state threaded through every step.
    struct Session {
        transcript: Vec<u8>,
        truncated: bool,
        /// The master returned EOF/EIO — the target closed the pty, so
        /// later expects can never match and sends can never land.
        child_eof: bool,
    }

    /// Drain whatever the master has right now into the bounded transcript.
    /// Returns the number of fresh bytes appended (0 also covers EOF, which
    /// flips `child_eof`).
    fn drain(master: &mut std::fs::File, session: &mut Session) -> usize {
        let mut fresh = 0usize;
        let mut buf = [0u8; 8192];
        loop {
            match master.read(&mut buf) {
                Ok(0) => {
                    session.child_eof = true;
                    break;
                }
                Ok(n) => {
                    let remaining = MAX_TRANSCRIPT_BYTES.saturating_sub(session.transcript.len());
                    if n > remaining {
                        session.transcript.extend_from_slice(&buf[..remaining]);
                        session.truncated = true;
                    } else {
                        session.transcript.extend_from_slice(&buf[..n]);
                    }
                    fresh += n;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.raw_os_error() == Some(libc::EIO) => {
                    // Linux/macOS pty master reads EIO once the slave side
                    // is fully closed — the target's EOF.
                    session.child_eof = true;
                    break;
                }
                Err(_) => break,
            }
        }
        fresh
    }

    fn drive_send(
        master: &mut std::fs::File,
        session: &mut Session,
        text: &str,
        session_deadline: Instant,
        index: usize,
    ) -> PtyStepOutcome {
        let mut written = 0usize;
        let bytes = text.as_bytes();
        while written < bytes.len() {
            if session.child_eof {
                return PtyStepOutcome {
                    step: index,
                    ok: false,
                    detail: format!("send step {} failed: target closed the pty", index + 1),
                };
            }
            if Instant::now() >= session_deadline {
                return PtyStepOutcome {
                    step: index,
                    ok: false,
                    detail: format!("send step {} failed: session timeout", index + 1),
                };
            }
            match master.write(&bytes[written..]) {
                Ok(n) => written += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // The pty's input buffer is full (a target not reading);
                    // drain pending output and retry within the session
                    // deadline rather than failing outright.
                    drain(master, session);
                    std::thread::sleep(POLL_INTERVAL);
                }
                Err(e) => {
                    return PtyStepOutcome {
                        step: index,
                        ok: false,
                        detail: format!("send step {} failed: {e}", index + 1),
                    };
                }
            }
        }
        PtyStepOutcome {
            step: index,
            ok: true,
            detail: format!("send step {} wrote {} bytes", index + 1, bytes.len()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn drive_expect(
        master: &mut std::fs::File,
        session: &mut Session,
        child: &mut std::process::Child,
        pattern: &str,
        regex: bool,
        step_timeout: Duration,
        session_deadline: Instant,
        index: usize,
    ) -> PtyStepOutcome {
        let deadline = (Instant::now() + step_timeout).min(session_deadline);
        // Compile once per step; an invalid pattern is a contract-authoring
        // error and fails the assertion NAMED, exactly like a contract
        // command that cannot run.
        let compiled = if regex {
            match regex::Regex::new(pattern) {
                Ok(re) => Some(re),
                Err(error) => {
                    return PtyStepOutcome {
                        step: index,
                        ok: false,
                        detail: format!(
                            "expect step {} has an invalid regex `{pattern}`: {error}",
                            index + 1
                        ),
                    };
                }
            }
        } else {
            None
        };
        let matched = |transcript: &[u8]| {
            let text = String::from_utf8_lossy(transcript);
            match &compiled {
                Some(re) => re.is_match(&text),
                None => text.contains(pattern),
            }
        };
        let started = Instant::now();
        loop {
            let fresh = drain(master, session);
            if matched(&session.transcript) {
                return PtyStepOutcome {
                    step: index,
                    ok: true,
                    detail: format!(
                        "expect `{pattern}` matched ({}ms)",
                        started.elapsed().as_millis()
                    ),
                };
            }
            if session.child_eof {
                let status = child.try_wait().ok().flatten();
                return PtyStepOutcome {
                    step: index,
                    ok: false,
                    detail: format!(
                        "expect `{pattern}` unmatched: target exited ({})",
                        status
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "status unknown".to_string())
                    ),
                };
            }
            if Instant::now() >= deadline {
                return PtyStepOutcome {
                    step: index,
                    ok: false,
                    detail: format!(
                        "expect `{pattern}` timed out after {}ms",
                        step_timeout.as_millis()
                    ),
                };
            }
            // Sleep only when the poll produced nothing: a spewing target
            // (a TUI redrawing, a build log) is drained at full speed,
            // while an idle pty never busy-spins.
            if fresh == 0 {
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    }
}

/// The non-unix core: no pty facility in the dependency set (see the module
/// docs' mechanism decision). Every assertion degrades to a LOUD skip —
/// rendered into the evidence block — never a silent pass or an
/// unsandboxed fallback.
#[cfg(not(unix))]
mod imp {
    use super::*;
    use crate::command_exec::WrappedCommand;

    pub fn run_session(
        _script: &PtyScript,
        _wrapped: &WrappedCommand,
        _cwd: &Path,
        _env: &HashMap<String, String>,
    ) -> PtyRunOutcome {
        PtyRunOutcome {
            verdict: PtyVerdict::Skipped,
            steps: Vec::new(),
            transcript: Vec::new(),
            truncated: false,
            note: Some(
                "pty validation is implemented for unix hosts only (libc openpty); \
                 this platform cannot drive terminal-interactive targets"
                    .to_string(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::types::PtyScript;
    use crate::types::PtyStep;

    /// The fixture interactive target shipped in-test: a tiny sh REPL with
    /// a `> ` prompt that echoes input back as `echo:<line>` and says `bye`
    /// on `quit`. Driven through the harness exactly like a contract's
    /// pty-script command (GateSandbox::Disabled → `/bin/sh -c …`).
    #[cfg(unix)]
    const REPL_OK: &str = "printf '> '; while IFS= read -r line; do case \"$line\" in quit) \
         printf 'bye\\n'; exit 0;; *) printf 'echo:%s\\n> ' \"$line\";; esac; done";
    /// The seeded-defect variant: the same REPL, but the echo is wrong.
    #[cfg(unix)]
    const REPL_DEFECT: &str = "printf '> '; while IFS= read -r line; do case \"$line\" in quit) \
         printf 'bye\\n'; exit 0;; *) printf 'echo:WRONG:%s\\n> ' \"$line\";; esac; done";

    #[cfg(unix)]
    fn pty_assertion(id: &str, command: &str, steps: Vec<PtyStep>) -> Assertion {
        Assertion {
            id: id.to_string(),
            statement: "the REPL echoes input back".to_string(),
            check: AssertionCheck::PtyScript,
            command: None,
            pty_script: Some(PtyScript {
                command: command.to_string(),
                steps,
                timeout_secs: Some(20),
            }),
        }
    }

    #[cfg(unix)]
    fn repl_steps() -> Vec<PtyStep> {
        vec![
            PtyStep::Expect {
                pattern: "> ".to_string(),
                regex: false,
                timeout_ms: Some(10_000),
            },
            PtyStep::Send {
                text: "hello\n".to_string(),
            },
            PtyStep::Expect {
                pattern: "echo:hello".to_string(),
                regex: false,
                timeout_ms: Some(10_000),
            },
            PtyStep::Send {
                text: "quit\n".to_string(),
            },
            PtyStep::Expect {
                pattern: "bye".to_string(),
                regex: false,
                timeout_ms: Some(10_000),
            },
        ]
    }

    /// A correct interactive target driven through scripted input PASSES,
    /// with every expect step's verdict recorded and the session transcript
    /// capturing the exchange (prompt, echoed input, response).
    #[cfg(unix)]
    #[tokio::test]
    async fn pty_validation_correct_target_passes_and_names_assertion() {
        let dir = tempfile::tempdir().unwrap();
        let contract = vec![pty_assertion("a-pty", REPL_OK, repl_steps())];
        let run = run_pty_assertions(
            &contract,
            dir.path(),
            &HashMap::new(),
            &GateSandbox::Disabled,
            &dir.path().join("runs"),
        )
        .await;
        assert_eq!(run.artifacts.len(), 1, "one transcript artifact: {run:?}");
        assert!(run.artifacts[0].pass, "correct REPL passes: {run:?}");
        assert_eq!(run.artifacts[0].assertion_id, "a-pty");
        let rendered = run.rendered.expect("pty assertions render evidence");
        assert!(rendered.contains("[a-pty]"), "assertion named: {rendered}");
        assert!(rendered.contains("→ PASS"), "verdict rendered: {rendered}");
        let transcript = std::fs::read(dir.path().join(&run.artifacts[0].transcript_rel)).unwrap();
        let text = String::from_utf8_lossy(&transcript);
        assert!(text.contains("echo:hello"), "transcript captured: {text}");
        assert!(text.contains("bye"), "full session captured: {text}");
    }

    /// The seeded-defect variant FAILS, with the assertion id, the failed
    /// step, and the unmatched pattern all named — and the failing
    /// session's transcript still lands as the artifact.
    #[cfg(unix)]
    #[tokio::test]
    async fn pty_validation_seeded_defect_fails_and_names_assertion() {
        let dir = tempfile::tempdir().unwrap();
        let mut steps = repl_steps();
        // Bound the failing expect so the test stays fast.
        if let PtyStep::Expect { timeout_ms, .. } = &mut steps[2] {
            *timeout_ms = Some(1_000);
        }
        let contract = vec![pty_assertion("a-pty", REPL_DEFECT, steps)];
        let run = run_pty_assertions(
            &contract,
            dir.path(),
            &HashMap::new(),
            &GateSandbox::Disabled,
            &dir.path().join("runs"),
        )
        .await;
        assert_eq!(run.artifacts.len(), 1, "failing session still artifacts");
        assert!(!run.artifacts[0].pass, "defect must fail");
        let rendered = run.rendered.unwrap();
        assert!(rendered.contains("[a-pty]"), "assertion named: {rendered}");
        assert!(rendered.contains("→ FAIL"), "verdict rendered: {rendered}");
        assert!(
            rendered.contains("echo:hello"),
            "unmatched pattern named: {rendered}"
        );
        assert!(
            run.artifacts[0].detail.contains("FAILED"),
            "failed step named in the event detail: {}",
            run.artifacts[0].detail
        );
    }

    /// The transcript lands as a validation artifact referenced the way
    /// events carry file evidence: a mission-relative `file:`-schemed
    /// reference that resolve_artefact classifies Resolved against the
    /// mission dir (the gate_results ArtefactRef idiom).
    #[cfg(unix)]
    #[tokio::test]
    async fn pty_validation_transcript_is_event_resolvable_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let mission_dir = dir.path();
        let runs_dir = mission_dir.join("runs");
        let contract = vec![pty_assertion("a-pty", REPL_OK, repl_steps())];
        let run = run_pty_assertions(
            &contract,
            mission_dir,
            &HashMap::new(),
            &GateSandbox::Disabled,
            &runs_dir,
        )
        .await;
        let artifact = &run.artifacts[0];
        assert!(
            artifact.transcript_rel.starts_with("runs/pty-transcripts/"),
            "mission-relative runs/ path: {}",
            artifact.transcript_rel
        );
        let reference = crate::gate_results::file_artefact_ref(&artifact.transcript_rel);
        assert!(
            reference.starts_with("file:runs/"),
            "file: scheme: {reference}"
        );
        match crate::gate_results::resolve_artefact(mission_dir, &reference) {
            crate::gate_results::ArtefactResolution::Resolved { .. } => {}
            other => panic!("transcript must resolve against the mission dir: {other:?}"),
        }
    }

    /// Skip discipline: a contract with no pty-script assertion takes
    /// today's path byte-for-byte — no rendered evidence, no artifacts (the
    /// "no declared run harness" case, mirroring browser QA).
    #[tokio::test]
    async fn pty_validation_contract_without_harness_skips() {
        let dir = tempfile::tempdir().unwrap();
        let contract = vec![
            Assertion {
                id: "a-1".to_string(),
                statement: "s".to_string(),
                check: AssertionCheck::Command,
                command: Some("true".to_string()),
                pty_script: None,
            },
            Assertion {
                id: "a-2".to_string(),
                statement: "s".to_string(),
                check: AssertionCheck::AgentJudgement,
                command: None,
                pty_script: None,
            },
        ];
        let run = run_pty_assertions(
            &contract,
            dir.path(),
            &HashMap::new(),
            &GateSandbox::Disabled,
            dir.path(),
        )
        .await;
        assert!(run.rendered.is_none(), "no pty assertions → no evidence");
        assert!(run.artifacts.is_empty(), "no pty assertions → no artifacts");

        // A declared pty-script check without a script is a malformed
        // contract entry — rendered as cannot-run, never silently dropped.
        let malformed = vec![Assertion {
            id: "a-3".to_string(),
            statement: "s".to_string(),
            check: AssertionCheck::PtyScript,
            command: None,
            pty_script: None,
        }];
        let run = run_pty_assertions(
            &malformed,
            dir.path(),
            &HashMap::new(),
            &GateSandbox::Disabled,
            dir.path(),
        )
        .await;
        let rendered = run.rendered.unwrap();
        assert!(
            rendered.contains("[a-3] (check=pty-script but no pty script — cannot run)"),
            "{rendered}"
        );
        assert!(run.artifacts.is_empty());
    }

    /// The transcript is bounded: a target spewing output past
    /// MAX_TRANSCRIPT_BYTES gets the cap enforced and the truncation
    /// recorded, so runaway output cannot fill the mission dir.
    #[cfg(unix)]
    #[tokio::test]
    async fn pty_validation_transcript_is_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let contract = vec![pty_assertion(
            "a-pty",
            // 4 KiB per write: fills the 256 KiB transcript cap well
            // inside the 2s expect timeout even on a loaded host.
            "x=$(printf '%04096d' 0); while :; do printf '%s' \"$x\"; done",
            vec![PtyStep::Expect {
                pattern: "this-pattern-never-appears".to_string(),
                regex: false,
                timeout_ms: Some(2_000),
            }],
        )];
        let run = run_pty_assertions(
            &contract,
            dir.path(),
            &HashMap::new(),
            &GateSandbox::Disabled,
            &dir.path().join("runs"),
        )
        .await;
        assert!(!run.artifacts[0].pass, "never-matching expect fails");
        let transcript = std::fs::read(dir.path().join(&run.artifacts[0].transcript_rel)).unwrap();
        // File bytes = capped transcript + the truncation marker line.
        assert!(
            transcript.len() <= MAX_TRANSCRIPT_BYTES + 128,
            "bounded on disk: {} bytes",
            transcript.len()
        );
        let text = String::from_utf8_lossy(&transcript);
        assert!(text.contains("transcript truncated"), "truncation recorded");
    }

    /// The contract addition is serde-additive: a pre-field assertion
    /// (no ptyScript key) still parses, the new check decodes from its
    /// kebab-case wire name, and every field default fills in.
    #[test]
    fn pty_validation_contract_serde_is_additive() {
        let old: Assertion = serde_json::from_str(
            r#"{"id":"a-1","statement":"s","check":"command","command":"true"}"#,
        )
        .unwrap();
        assert!(old.pty_script.is_none(), "absent key decodes to None");
        let old: Assertion =
            serde_json::from_str(r#"{"id":"a-1","statement":"s","check":"agent-judgement"}"#)
                .unwrap();
        assert!(old.pty_script.is_none());

        let new: Assertion = serde_json::from_str(
            r#"{"id":"a-2","statement":"s","check":"pty-script",
                "ptyScript":{"command":"./repl","steps":[
                    {"op":"expect","pattern":"> "},
                    {"op":"send","text":"help\n"},
                    {"op":"expect","pattern":"usage","regex":true,"timeoutMs":500}
                ]}}"#,
        )
        .unwrap();
        assert_eq!(new.check, AssertionCheck::PtyScript);
        // The wire shape stays camelCase/tagged and omits defaulted Nones.
        let json = serde_json::to_value(&new).unwrap();
        assert_eq!(json["check"], "pty-script");
        assert!(json["ptyScript"].get("timeoutSecs").is_none());
        assert!(json["ptyScript"]["steps"][0].get("timeoutMs").is_none());
        assert_eq!(json["ptyScript"]["steps"][1]["op"], "send");
        let script = new.pty_script.unwrap();
        assert_eq!(script.command, "./repl");
        assert_eq!(script.timeout_secs, None, "session timeout defaults");
        assert_eq!(script.steps.len(), 3);
        match &script.steps[0] {
            PtyStep::Expect {
                pattern,
                regex,
                timeout_ms,
            } => {
                assert_eq!(pattern, "> ");
                assert!(!regex, "regex defaults to literal substring");
                assert_eq!(*timeout_ms, None, "step timeout defaults");
            }
            other => panic!("wrong step: {other:?}"),
        }
    }
}
