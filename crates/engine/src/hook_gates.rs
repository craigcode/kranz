//! Claude Code lifecycle-hook gate projection (ticket
//! `.kranz/tickets/claude-code-hook-gate-projection.md`, KRZ-302).
//!
//! NOT [`crate::hooks`] — that module is the D-F webhook triggers the engine
//! EMITS to the operator; this module is about hooks the Claude Code CLI runs
//! INSIDE a worker session. The names are kept apart deliberately.
//!
//! # What this is
//!
//! Deterministic kranz gates are projected onto Claude Code lifecycle hooks
//! so a failure is enforced IN-PROCESS during the worker session instead of
//! only being discovered afterwards: a `PreToolUse` hook matcher on the
//! file-writing tools (`Write|Edit|MultiEdit|NotebookEdit`) runs a guard
//! command (`kranz hook-guard`) that judges the tool call's target path
//! against the mission's declared `touch_set` and BLOCKS an out-of-contract
//! write before it happens, leaving a structured record the engine folds
//! into the event log as `hook.gate.fired` events once the session ends.
//!
//! The engine-side gate ladder remains AUTHORITATIVE — hooks are
//! defense-in-depth, never a replacement (same philosophy as
//! sandbox-vs-scrutiny: hooks bound what the session agrees to; the
//! engine-side out-of-contract sweep in [`crate::contract_sweep`] judges
//! what actually happened, committed, afterwards). A hook-BYPASSED failure
//! — hook config removed by the session, a write via `Bash` redirection
//! instead of the `Write` tool, a guard that errored, a CLI too old to know
//! hooks — is still caught by that sweep, and every test pinning the sweep
//! is untouched by this module.
//!
//! # Why the out-of-contract write rule is the first (and only) projection
//!
//! It is the one deterministic gate that maps onto a SINGLE tool call: one
//! path, judged against one glob set, with a verdict the model can act on
//! (write inside the touch set instead, or surface the need for a
//! touch-path grant). The contract `command` assertions deliberately stay
//! engine-side: they are whole shell commands with pipes, timeouts, and
//! anti-vacuity greps evaluated over captured output — re-implementing that
//! inside a per-tool-call hook would duplicate the engine's command
//! execution with strictly worse evidence, and blocking a `Bash` call
//! pre-execution would prejudge a command whose verdict depends on its
//! OUTPUT.
//!
//! # Hooks schema targeted
//!
//! Claude Code hooks as documented 2026-08-04 (the public hooks reference,
//! content current to CLI v2.1.221; hooks themselves GA since CLI v1.0.38,
//! 2025-06-30). The repo's verified-CLI ground truth (docs/design.md) is
//! claude 2.1.198, which fully supports the schema used here. The generated
//! config uses only the long-stable subset — pipe-separated exact-match
//! `matcher`, `type`/`command`/`timeout` handler fields, and exit-code
//! semantics — which behaves identically on every hooks-capable CLI:
//!
//! ```json
//! { "hooks": { "PreToolUse": [ {
//!       "matcher": "Write|Edit|MultiEdit|NotebookEdit",
//!       "hooks": [ { "type": "command",
//!                    "command": "<kranz> hook-guard --config <spec.json>",
//!                    "timeout": 10 } ] } ] } }
//! ```
//!
//! delivered through the session's existing `--settings` JSON (a documented
//! settings tier that honors the `hooks` key). `MultiEdit` is matched for
//! older-CLI compatibility; current CLIs merged it into `Edit`, so the
//! entry is harmless dead weight there.
//!
//! Exit-code semantics the guard relies on (PreToolUse): exit 2 BLOCKS the
//! tool call and feeds stderr back to the model; exit 0 lets the normal
//! permission flow proceed; any OTHER non-zero exit is a non-blocking error
//! (the action proceeds, a hook-error notice lands in the transcript). The
//! guard therefore fails OPEN on its own internal errors (exit 1): a broken
//! guard must never freeze a session, and the miss is still judged by the
//! authoritative engine-side sweep.
//!
//! # Degradation posture
//!
//! - A pre-hooks CLI (< v1.0.38) ignores the unknown `hooks` settings key;
//!   the hook simply never fires, no records exist, and the sweep judges.
//!   [`crate::backend_claude::discover_claude_binary`] probes that a
//!   candidate RUNS `--version` but gates on no version floor, so this
//!   degradation is by-construction (documented here), not probed.
//! - Sandboxed sessions: the guard is a child of the session CLI and runs
//!   with the session's already-cleared env — no new credential or env
//!   channel is introduced (the hook command line carries only paths). Its
//!   spec/record files live under the session-private scratch root
//!   ([`crate::backend_claude::scratch_home_root`]), which every process
//!   sandbox tier keeps writable (Seatbelt's `write_allowlist`, bwrap's
//!   `--bind` set), and the host `kranz` binary is executable there
//!   (Seatbelt `allow process*`, bwrap whole-fs ro-bind). A CONTAINER
//!   sandbox whose image lacks the host kranz binary cannot exec the guard:
//!   the hook then errors non-blocking and enforcement stays engine-side.
//! - The record file is SESSION-WRITABLE evidence, like a worker report: a
//!   hostile session can forge or corrupt it, so folding is tolerant
//!   (malformed lines are skipped), bounded ([`FOLD_CAP`]), scrubbed, and
//!   the events are record-only in the reducer — never consulted to judge.

use crate::backend::SessionSpec;
use crate::contract_sweep;
use crate::events::EventKind;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The gate identity the first projection enforces: the same defect-class
/// name the engine-side sweep reports ([`contract_sweep::FINDING_CLASS`]),
/// so an in-process block and an after-the-fact finding read as ONE gate
/// evaluated at two layers.
pub const HOOK_GATE_ID: &str = contract_sweep::FINDING_CLASS;

/// The `kranz` subcommand the hook command line invokes. Named in the
/// generated settings JSON and matched by the CLI's clap surface.
pub const HOOK_GUARD_SUBCOMMAND: &str = "hook-guard";

/// Schema version of the per-session spec file ([`HookGateSpec`]) and of
/// the record lines ([`HookGateRecord`]) — both bump together.
pub const SPEC_VERSION: u32 = 1;

/// Bounds a wedged hook invocation (seconds; the CLI's default is 600,
/// absurd for a local path check).
const HOOK_TIMEOUT_SECS: u32 = 10;

/// Max hook records folded into the event log per run. The record file is
/// session-writable, so an unbounded fold would let a hostile or looping
/// session flood the append-only log; the records beyond the cap stay in
/// the scratch file and the truncation is logged.
pub const FOLD_CAP: usize = 64;

/// Max chars kept on a folded record's subject / detail (the file is
/// session-authored, so every persisted string is scrubbed AND bounded).
const RECORD_SUBJECT_MAX: usize = 500;
const RECORD_DETAIL_MAX: usize = 1000;

// ---------------------------------------------------------------------------
// Per-session spec file (engine-written, guard-read)
// ---------------------------------------------------------------------------

/// Everything `kranz hook-guard` needs to judge one tool call, written by
/// the engine at spec-build time into the session-private scratch root. The
/// hook command line carries ONLY this file's path: no touch-set on the
/// command line, no env vars — the session's already-cleared env is the
/// whole channel (house rule: no new ambient credential or env channel).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookGateSpec {
    /// [`SPEC_VERSION`] at write time.
    pub version: u32,
    /// Gate identity ([`HOOK_GATE_ID`]).
    pub gate: String,
    /// The session's working directory (the mission worktree in worktree
    /// mode, the repo root in checkout mode): the root touch-set globs are
    /// relative to, and the base relative `file_path`s resolve against.
    pub session_cwd: PathBuf,
    /// The mission's declared touch-set globs, verbatim (gitignore-style:
    /// `!` negates, last match wins — [`contract_sweep::touch_set_includes`]).
    pub touch_set: Vec<String>,
    /// Absolute path of the record file the guard appends to
    /// ([`record_file`]). Session-writable by construction (see module docs).
    pub record_file: PathBuf,
}

impl HookGateSpec {
    /// Load the spec the hook command was pointed at. Any IO/parse failure
    /// is the caller's fail-open (exit 1) branch.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        serde_json::from_str(&text)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }
}

/// The per-session hook-gate dir under the session-private scratch root —
/// the one tree every sandbox tier keeps worker-writable (see module docs).
fn hook_gate_dir(session_id: &str) -> PathBuf {
    crate::backend_claude::scratch_home_root(session_id).join("hook-gate")
}

/// The engine-written spec file the hook command is pointed at.
pub fn spec_file(session_id: &str) -> PathBuf {
    hook_gate_dir(session_id).join("spec.json")
}

/// The guard-appended record file the engine folds after the session ends.
pub fn record_file(session_id: &str) -> PathBuf {
    hook_gate_dir(session_id).join("records.jsonl")
}

// ---------------------------------------------------------------------------
// Settings projection (engine side, spec-build time)
// ---------------------------------------------------------------------------

/// The `--settings` JSON block projecting the out-of-contract write rule
/// onto a `PreToolUse` hook. `command` is the fully-quoted hook command
/// line (see [`project_worker_hook_gates`]). Pure so the exact wire shape
/// is unit-testable without spawning anything.
pub fn worker_hook_settings(command: &str) -> Value {
    json!({
        "hooks": {
            "PreToolUse": [
                {
                    "matcher": "Write|Edit|MultiEdit|NotebookEdit",
                    "hooks": [
                        {
                            "type": "command",
                            "command": command,
                            "timeout": HOOK_TIMEOUT_SECS,
                        }
                    ]
                }
            ]
        }
    })
}

/// Project the mission's out-of-contract write rule onto `spec`'s
/// per-session settings (worker sessions only; read-only roles deny the
/// write tools outright and have nothing to project).
///
/// An EMPTY `touch_set` is advisory-off, mirroring the engine-side sweep's
/// skip: no hook config, byte-identical session. Otherwise the per-session
/// spec file is written into the session-private scratch root and
/// `settings_json` becomes the hook block (workers carry no other settings
/// today — the field is `None` by construction at every spec site).
///
/// Every failure here degrades to NO hook config with a loud warning —
/// never a spawn error: the projection is defense-in-depth and the
/// engine-side sweep stays authoritative without it.
pub fn project_worker_hook_gates(spec: &mut SessionSpec, touch_set: &[String]) {
    if touch_set.is_empty() {
        return;
    }
    let session_id = spec.session_id.clone();
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            tracing::warn!(
                session_id = %session_id,
                error = %e,
                "hook gate projection skipped: current_exe unresolved; \
                 the engine-side out-of-contract sweep remains authoritative"
            );
            return;
        }
    };
    let gate_spec = HookGateSpec {
        version: SPEC_VERSION,
        gate: HOOK_GATE_ID.to_string(),
        session_cwd: spec.cwd.clone(),
        touch_set: touch_set.to_vec(),
        record_file: record_file(&session_id),
    };
    let spec_path = spec_file(&session_id);
    let written = (|| -> std::io::Result<()> {
        if let Some(parent) = spec_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(&gate_spec).map_err(std::io::Error::other)?;
        std::fs::write(&spec_path, text)
    })();
    if let Err(e) = written {
        tracing::warn!(
            session_id = %session_id,
            error = %e,
            "hook gate projection skipped: spec file write failed; \
             the engine-side out-of-contract sweep remains authoritative"
        );
        return;
    }
    let command = format!(
        "{} {} --config {}",
        shell_quote(&exe),
        HOOK_GUARD_SUBCOMMAND,
        shell_quote(&spec_path)
    );
    spec.settings_json = Some(worker_hook_settings(&command));
    tracing::info!(
        session_id = %session_id,
        gate = HOOK_GATE_ID,
        "out-of-contract write rule projected onto a PreToolUse lifecycle hook \
         (defense-in-depth; the engine-side sweep remains authoritative)"
    );
}

/// Single-quote a path for the shell-form hook command line (`sh -c`
/// semantics): the only safe interpolation is none at all, so every
/// single-quote in the path is closed-escaped-reopened. Engine-controlled
/// paths make this defensive, but a repo under a quoted directory must not
/// corrupt the command line.
fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', r"'\''"))
}

// ---------------------------------------------------------------------------
// Guard evaluation (shared by the `kranz hook-guard` CLI and tests)
// ---------------------------------------------------------------------------

/// The guard's verdict on one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardVerdict {
    /// The write is inside the touch set; the hook exits 0 and the normal
    /// permission flow proceeds.
    Allow,
    /// The write is out of contract; the hook records it and exits 2 with
    /// `reason` on stderr (fed back to the model by the CLI).
    Block { subject: String, reason: String },
}

/// Judge one `PreToolUse` tool call against `spec`. `file_path` is the
/// target path from the tool input (`tool_input.file_path`, or
/// `notebook_path` on older CLIs' NotebookEdit), as the CLI reported it —
/// absolute, or relative to the session cwd.
///
/// Fail-CLOSED on every unjudgeable shape (no path, path outside the
/// session checkout, broken touch-set glob): the projection exists to stop
/// out-of-contract writes, and a write the guard cannot name is one the
/// engine-side sweep could never attribute either. Blocking is recoverable
/// — the CLI feeds the reason back to the model, which can relocate the
/// write or surface the need for a touch-path grant.
pub fn evaluate(spec: &HookGateSpec, tool_name: &str, file_path: Option<&str>) -> GuardVerdict {
    let Some(raw) = file_path.filter(|p| !p.trim().is_empty()) else {
        return GuardVerdict::Block {
            subject: "(unresolved)".to_string(),
            reason: format!(
                "kranz hook gate ({}) blocked {tool_name}: the tool call carried no file \
                 path the guard can judge, so the write is out of contract by default",
                spec.gate
            ),
        };
    };
    let raw_path = Path::new(raw);
    let absolute = if raw_path.is_absolute() {
        normalize_lexical(raw_path)
    } else {
        normalize_lexical(&spec.session_cwd.join(raw_path))
    };
    let cwd = normalize_lexical(&spec.session_cwd);
    let rel = match absolute.strip_prefix(&cwd) {
        Ok(rel) => rel,
        Err(_) => {
            return GuardVerdict::Block {
                subject: raw.to_string(),
                reason: format!(
                    "kranz hook gate ({}) blocked {tool_name}: {raw} is outside the mission \
                     checkout, which no touch-set glob can ever cover",
                    spec.gate
                ),
            };
        }
    };
    // Forward-slash repo-relative form, matching `git diff --name-only` and
    // the sweep's glob semantics (Windows separators normalized).
    let rel_str = rel.to_string_lossy().replace('\\', "/");
    match contract_sweep::touch_set_includes(&spec.touch_set, &rel_str) {
        Ok(true) => GuardVerdict::Allow,
        Ok(false) => GuardVerdict::Block {
            subject: rel_str.clone(),
            reason: format!(
                "kranz hook gate ({}) blocked {tool_name}: {rel_str} matches none of the \
                 mission's declared touch-set globs — relocate the write under a declared \
                 path, or stop and surface the need for a touch-path grant",
                spec.gate
            ),
        },
        Err(e) => GuardVerdict::Block {
            subject: rel_str,
            reason: format!(
                "kranz hook gate ({}) blocked {tool_name}: touch-set glob compile error: {e}",
                spec.gate
            ),
        },
    }
}

/// Lexical (no-filesystem) normalization: `.` dropped, `..` resolved by
/// popping. Never resolves symlinks — a new `Write` target may not exist
/// yet — and the failure direction of any alias mismatch is a BLOCK (the
/// path fails `strip_prefix`), never an allow.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Records: the guard appends; the engine folds after the session
// ---------------------------------------------------------------------------

/// One structured hook outcome, appended as one JSON line to
/// [`record_file`] by `kranz hook-guard`. Session-writable evidence —
/// parsed tolerantly, never trusted (see module docs).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HookGateRecord {
    /// [`SPEC_VERSION`] at write time.
    pub v: u32,
    pub ts: DateTime<Utc>,
    /// Gate identity ([`HOOK_GATE_ID`]).
    pub gate: String,
    /// The lifecycle event that fired (`"PreToolUse"`).
    pub hook_event: String,
    /// The tool whose call was judged (`Write`, `Edit`, ...).
    pub tool: String,
    /// The judged target: the repo-relative path when it resolved inside
    /// the checkout, else the raw path / `(unresolved)`.
    pub subject: String,
    /// `"blocked"` (the write was refused in-process) or `"error"` (the
    /// guard itself failed open — the action proceeded and only the
    /// engine-side sweep can judge it).
    pub verdict: String,
    /// The guard's reason / error note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// The CLI's session id from the hook payload, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// The CLI's tool-use id from the hook payload, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
}

impl HookGateRecord {
    /// A `blocked` record for one refused tool call.
    pub fn blocked(
        spec: &HookGateSpec,
        hook_event: &str,
        tool: &str,
        subject: &str,
        reason: &str,
        session_id: Option<&str>,
        tool_use_id: Option<&str>,
    ) -> Self {
        HookGateRecord {
            v: SPEC_VERSION,
            ts: Utc::now(),
            gate: spec.gate.clone(),
            hook_event: hook_event.to_string(),
            tool: tool.to_string(),
            subject: subject.to_string(),
            verdict: "blocked".to_string(),
            detail: Some(reason.to_string()),
            session_id: session_id.map(str::to_string),
            tool_use_id: tool_use_id.map(str::to_string),
        }
    }

    /// An `error` record: the guard loaded its spec but could not judge
    /// (unparseable hook payload, missing tool name) and failed open.
    pub fn error(spec: &HookGateSpec, note: &str) -> Self {
        HookGateRecord {
            v: SPEC_VERSION,
            ts: Utc::now(),
            gate: spec.gate.clone(),
            hook_event: "PreToolUse".to_string(),
            tool: String::new(),
            subject: String::new(),
            verdict: "error".to_string(),
            detail: Some(note.to_string()),
            session_id: None,
            tool_use_id: None,
        }
    }

    /// Append this record as one JSON line to `record_file`. Best-effort by
    /// the caller: an append failure never changes the exit verdict.
    pub fn append_to(&self, record_file: &Path) -> std::io::Result<()> {
        use std::io::Write as _;
        if let Some(parent) = record_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(record_file)?;
        let line = serde_json::to_string(self).map_err(std::io::Error::other)?;
        writeln!(file, "{line}")
    }
}

/// Fold one session's hook records into `hook.gate.fired` event kinds,
/// ready for the run's log target (called by `runner::run_session_to`
/// AFTER the session stream closes, so a gate-failing action inside the
/// session lands as a structured event BEFORE `worker.completed` and the
/// rest of session-end processing).
///
/// `session_id` names the record file ([`record_file`]); `run_id` is
/// STAMPED from the run's own metadata — never read from the
/// session-writable file, so a forged record cannot attach itself to
/// another run (the reducer validates the run reference as a corruption
/// guard). Missing/unreadable file → no events (a session that never fired
/// the hook — or bypassed it — is the ordinary case, and the engine-side
/// sweep judges either way). Malformed lines are skipped with a warning:
/// the file is session-authored and must never break the run that produced
/// it. Bounded by [`FOLD_CAP`]; every persisted string is scrubbed and
/// truncated (same discipline as `worker.message` content).
pub fn records_to_events(session_id: &str, run_id: &str) -> Vec<EventKind> {
    let file = record_file(session_id);
    let contents = match std::fs::read_to_string(&file) {
        Ok(contents) => contents,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            tracing::warn!(
                session_id,
                error = %e,
                "hook gate record file unreadable; folding nothing \
                 (the engine-side sweep remains authoritative)"
            );
            return Vec::new();
        }
    };
    let mut events = Vec::new();
    let mut skipped = 0usize;
    let mut truncated = false;
    for line in contents.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if events.len() >= FOLD_CAP {
            truncated = true;
            break;
        }
        match serde_json::from_str::<HookGateRecord>(line) {
            Ok(record) => events.push(record_into_event(record, run_id)),
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 || truncated {
        tracing::warn!(
            session_id,
            skipped,
            truncated,
            "hook gate record fold dropped session-authored lines \
             (malformed or over the fold cap)"
        );
    }
    events
}

/// Map one parsed record onto its event, scrubbing and bounding every
/// session-authored string before it lands in the append-only log.
fn record_into_event(record: HookGateRecord, run_id: &str) -> EventKind {
    EventKind::HookGateFired {
        run_id: run_id.to_string(),
        gate: crate::scrub::scrub(&record.gate),
        hook_event: crate::scrub::scrub(&record.hook_event),
        tool: crate::scrub::scrub(&record.tool),
        subject: crate::scrub::scrub_and_truncate(&record.subject, RECORD_SUBJECT_MAX),
        verdict: crate::scrub::scrub(&record.verdict),
        detail: record
            .detail
            .map(|d| crate::scrub::scrub_and_truncate(&d, RECORD_DETAIL_MAX)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec_fixture(cwd: &Path, touch_set: &[&str]) -> HookGateSpec {
        HookGateSpec {
            version: SPEC_VERSION,
            gate: HOOK_GATE_ID.to_string(),
            session_cwd: cwd.to_path_buf(),
            touch_set: touch_set.iter().map(|s| s.to_string()).collect(),
            record_file: cwd.join("records.jsonl"),
        }
    }

    /// The generated `--settings` block has exactly the documented hooks
    /// schema shape (see module docs): PreToolUse, the write-tool matcher,
    /// one command handler with the quoted guard invocation and a bounded
    /// timeout.
    #[test]
    fn hook_gate_projection_settings_shape_matches_the_hooks_schema() {
        let settings =
            worker_hook_settings("'/usr/local/bin/kranz' hook-guard --config '/tmp/s/spec.json'");
        let groups = settings["hooks"]["PreToolUse"]
            .as_array()
            .expect("PreToolUse matcher groups");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["matcher"], "Write|Edit|MultiEdit|NotebookEdit");
        let handlers = groups[0]["hooks"].as_array().expect("handlers");
        assert_eq!(handlers.len(), 1);
        assert_eq!(handlers[0]["type"], "command");
        assert_eq!(
            handlers[0]["command"],
            "'/usr/local/bin/kranz' hook-guard --config '/tmp/s/spec.json'"
        );
        assert!(
            handlers[0]["timeout"].as_u64().unwrap() <= 30,
            "the guard is a local path check; the CLI's 600s default must not stand"
        );
    }

    /// Projection onto a worker spec: the spec file lands under the
    /// session-private scratch root (the tree every sandbox tier keeps
    /// writable), carrying the touch set verbatim, and `settings_json`
    /// names the guard command and that spec file.
    #[test]
    fn hook_gate_projection_worker_spec_carries_settings_and_spec_file() {
        let session_id = format!("hook-gate-projection-{}", uuid::Uuid::new_v4());
        let mut spec = SessionSpec {
            cwd: PathBuf::from("/repo/worktree"),
            prompt: crate::backend::PromptMode::SingleShot("task".to_string()),
            append_system_prompt: None,
            model: "stub".to_string(),
            effort: "low".to_string(),
            session_id: session_id.clone(),
            resume: None,
            permission_mode: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            tools: Vec::new(),
            writable: true,
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: Default::default(),
            sandbox: None,
            hook_status: None,
        };
        let touch_set = vec!["src/**".to_string(), "!src/generated/**".to_string()];
        project_worker_hook_gates(&mut spec, &touch_set);

        let settings = spec.settings_json.expect("hook settings projected");
        let command = settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        assert!(command.contains(HOOK_GUARD_SUBCOMMAND), "{command}");
        assert!(command.contains("--config"), "{command}");

        let loaded = HookGateSpec::load(&spec_file(&session_id)).expect("spec file written");
        assert_eq!(loaded.version, SPEC_VERSION);
        assert_eq!(loaded.gate, HOOK_GATE_ID);
        assert_eq!(loaded.session_cwd, PathBuf::from("/repo/worktree"));
        assert_eq!(loaded.touch_set, touch_set);
        assert_eq!(loaded.record_file, record_file(&session_id));
        assert!(
            loaded
                .record_file
                .starts_with(crate::backend_claude::scratch_home_root(&session_id)),
            "the record file must live under the session-private scratch root"
        );

        let _ = std::fs::remove_dir_all(crate::backend_claude::scratch_home_root(&session_id));
    }

    /// An empty touch set is advisory-off (the sweep's posture): no
    /// settings, no spec file, byte-identical session.
    #[test]
    fn hook_gate_projection_empty_touch_set_projects_nothing() {
        let session_id = format!("hook-gate-projection-{}", uuid::Uuid::new_v4());
        let mut spec = SessionSpec {
            cwd: PathBuf::from("/repo"),
            prompt: crate::backend::PromptMode::SingleShot("task".to_string()),
            append_system_prompt: None,
            model: "stub".to_string(),
            effort: "low".to_string(),
            session_id: session_id.clone(),
            resume: None,
            permission_mode: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            tools: Vec::new(),
            writable: true,
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: Default::default(),
            sandbox: None,
            hook_status: None,
        };
        project_worker_hook_gates(&mut spec, &[]);
        assert!(spec.settings_json.is_none());
        assert!(!spec_file(&session_id).exists());
    }

    /// An absolute fixture root for the platform (`/repo/wt` unix,
    /// `C:\repo\wt` Windows): the guard's path resolution is lexical and
    /// platform-relative, so tests must anchor on a genuinely absolute path
    /// or `is_absolute()` splits the fixtures across platforms.
    fn test_cwd() -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(r"C:\repo\wt")
        } else {
            PathBuf::from("/repo/wt")
        }
    }

    /// The display form of `cwd.join(rel)` — the shape a hook payload's
    /// absolute `file_path` takes.
    fn under(cwd: &Path, rel: &str) -> String {
        cwd.join(rel).display().to_string()
    }

    /// The guard blocks a write outside the touch set, allows one inside
    /// it, honors `!`-negation, and resolves relative and `..`-carrying
    /// paths against the session cwd before judging.
    #[test]
    fn hook_gate_projection_evaluate_judges_against_the_touch_set() {
        let cwd = test_cwd();
        let spec = spec_fixture(&cwd, &["src/**", "!src/generated/**"]);

        // In-contract absolute and relative paths both allow.
        assert_eq!(
            evaluate(&spec, "Write", Some(&under(&cwd, "src/lib.rs"))),
            GuardVerdict::Allow
        );
        assert_eq!(
            evaluate(&spec, "Edit", Some("src/lib.rs")),
            GuardVerdict::Allow
        );
        // Dot-dot normalized before judging: this lands in-contract.
        assert_eq!(
            evaluate(&spec, "Write", Some(&under(&cwd, "docs/../src/lib.rs"))),
            GuardVerdict::Allow
        );

        // Out-of-contract blocks, with the repo-relative subject named.
        let blocked = evaluate(&spec, "Write", Some(&under(&cwd, "docs/oops.md")));
        let GuardVerdict::Block { subject, reason } = blocked else {
            panic!("docs/oops.md must block: {blocked:?}")
        };
        assert_eq!(subject, "docs/oops.md");
        assert!(reason.contains("touch-set"), "{reason}");

        // The `!`-negated subtree is out of contract even under src/**.
        assert!(matches!(
            evaluate(&spec, "Edit", Some("src/generated/x.rs")),
            GuardVerdict::Block { .. }
        ));
    }

    /// Fail-closed shapes: a path outside the checkout and a missing path
    /// both block (the sweep could never attribute what the guard cannot
    /// name).
    #[test]
    fn hook_gate_projection_evaluate_fails_closed_on_unjudgeable_writes() {
        let cwd = test_cwd();
        let spec = spec_fixture(&cwd, &["src/**"]);
        let outside = if cfg!(windows) {
            r"C:\outside\checkout.md"
        } else {
            "/outside/checkout.md"
        };

        match evaluate(&spec, "Write", Some(outside)) {
            GuardVerdict::Block { subject, reason } => {
                assert_eq!(subject, outside);
                assert!(reason.contains("outside the mission checkout"), "{reason}");
            }
            GuardVerdict::Allow => panic!("{outside} must never allow"),
        }
        // `..` escaping the checkout normalizes to an outside path → block.
        assert!(matches!(
            evaluate(&spec, "Write", Some("../../etc/passwd")),
            GuardVerdict::Block { .. }
        ));
        match evaluate(&spec, "NotebookEdit", None) {
            GuardVerdict::Block { subject, .. } => assert_eq!(subject, "(unresolved)"),
            GuardVerdict::Allow => panic!("a path-less write tool call must fail closed"),
        }
        // A broken touch-set glob blocks loudly rather than waving writes through.
        let broken = spec_fixture(&cwd, &["["]);
        assert!(matches!(
            evaluate(&broken, "Write", Some("src/lib.rs")),
            GuardVerdict::Block { .. }
        ));
    }

    /// Records round-trip through the JSONL file, and the fold is tolerant
    /// (garbage lines skipped), bounded, scrubbed, and stamps the run id
    /// from the RUN — never from the session-writable line.
    #[test]
    fn hook_gate_projection_records_fold_tolerantly_and_stamp_the_run() {
        let session_id = format!("hook-gate-projection-{}", uuid::Uuid::new_v4());
        let spec = spec_fixture(&test_cwd(), &["src/**"]);
        let file = record_file(&session_id);

        let record = HookGateRecord::blocked(
            &spec,
            "PreToolUse",
            "Write",
            "docs/oops.md",
            "outside the touch set",
            Some("cli-session-1"),
            Some("toolu_1"),
        );
        record.append_to(&file).unwrap();
        // A session-authored garbage line between valid ones.
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&file)
                .unwrap();
            writeln!(f, "{{not json").unwrap();
            writeln!(f).unwrap();
        }
        HookGateRecord::error(&spec, "stdin was not JSON")
            .append_to(&file)
            .unwrap();

        let events = records_to_events(&session_id, "run-xyz");
        assert_eq!(events.len(), 2, "garbage lines must be skipped");
        match &events[0] {
            EventKind::HookGateFired {
                run_id,
                gate,
                hook_event,
                tool,
                subject,
                verdict,
                detail,
            } => {
                assert_eq!(run_id, "run-xyz", "the run id is engine-stamped");
                assert_eq!(gate, HOOK_GATE_ID);
                assert_eq!(hook_event, "PreToolUse");
                assert_eq!(tool, "Write");
                assert_eq!(subject, "docs/oops.md");
                assert_eq!(verdict, "blocked");
                assert_eq!(detail.as_deref(), Some("outside the touch set"));
            }
            other => panic!("expected hook.gate.fired, got {other:?}"),
        }
        match &events[1] {
            EventKind::HookGateFired { verdict, .. } => assert_eq!(verdict, "error"),
            other => panic!("expected the error record, got {other:?}"),
        }

        // A missing file folds to nothing (hook never fired / bypassed).
        assert!(records_to_events("no-such-session-hook-gate-projection", "r").is_empty());

        let _ = std::fs::remove_dir_all(crate::backend_claude::scratch_home_root(&session_id));
    }

    /// The fold cap bounds a flooding (or hostile) record file.
    #[test]
    fn hook_gate_projection_fold_is_capped() {
        let session_id = format!("hook-gate-projection-{}", uuid::Uuid::new_v4());
        let spec = spec_fixture(&test_cwd(), &["src/**"]);
        let file = record_file(&session_id);
        for i in 0..(FOLD_CAP + 10) {
            HookGateRecord::blocked(
                &spec,
                "PreToolUse",
                "Write",
                &format!("docs/{i}.md"),
                "r",
                None,
                None,
            )
            .append_to(&file)
            .unwrap();
        }
        let events = records_to_events(&session_id, "run-cap");
        assert_eq!(events.len(), FOLD_CAP);
        let _ = std::fs::remove_dir_all(crate::backend_claude::scratch_home_root(&session_id));
    }

    /// Shell quoting: a path containing a single quote must not corrupt the
    /// command line.
    #[test]
    fn hook_gate_projection_shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote(Path::new("/a/b")), "'/a/b'");
        assert_eq!(shell_quote(Path::new("/a/o'brien")), r"'/a/o'\''brien'");
    }
}
