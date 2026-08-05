//! One function per `kranz` subcommand, plus the shared helpers (config
//! loading, backend construction, mission selection).
//!
//! The `ClaudeBackend` is constructed LAZILY — only `plan` and `run` spawn
//! agent sessions, so `status`/`msg`/`pause`/`resume`/`missions`/`serve`
//! work on machines without a `claude` binary installed.

use crate::backlog;
use crate::cli::{Cli, Command, GrantCommand, QuestionCommand, RevisionCommand, TicketCommand};
use crate::output::{self, ansi};
use crate::planning_tui::PlanningOutcome;
use crate::tail::{self, EventRenderer};
use anyhow::{anyhow, bail, Context, Result};
use kranz_engine::backend::AgentBackend;
use kranz_engine::backend_claude::ClaudeBackend;
use kranz_engine::config;
use kranz_engine::control;
use kranz_engine::corpus_export;
use kranz_engine::cost;
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::mission_catalog;
use kranz_engine::orchestrator::{MissionEngine, PlanRequest};
use kranz_engine::paths::MissionPaths;
use kranz_engine::reducer;
use kranz_engine::trace_export;
use kranz_engine::types::{ControlCommand, MissionConfig, MissionState, MissionStatus};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Parse-level entry point: resolve the repo, print the danger banner when
/// requested, dispatch the subcommand, and return the process exit code.
pub async fn run_cli(cli: Cli) -> Result<i32> {
    let lock_force = cli.lock_force();
    let repo = match cli.repo {
        Some(repo) => repo,
        None => std::env::current_dir().context("cannot determine the current directory")?,
    };
    if cli.dangerously_allow_all {
        print_danger_banner();
    }

    match cli.command {
        Command::Init {
            gates,
            register,
            id,
            display_name,
        } => {
            let options = crate::init::InitOptions {
                gates,
                registration: register.then_some(crate::init::Registration { id, display_name }),
                global_config: kranz_engine::paths::global_config(),
            };
            let report = crate::init::initialize(&repo, &options)?;
            print!("{}", crate::init::render(&report));
            Ok(0)
        }
        Command::Plan { goal } => {
            let cfg = load_config(&repo, cli.dangerously_allow_all)?;
            cmd_plan(repo, goal, cfg, cli.mission.as_deref(), lock_force)
                .await
                .map_err(augment_limit_hint)
        }
        Command::Run => {
            let mission = select_mission(&repo, cli.mission.as_deref())?;
            cmd_run(repo, mission, lock_force, cli.dangerously_allow_all)
                .await
                .map_err(augment_limit_hint)
        }
        Command::Status { json } => {
            let mission = select_mission(&repo, cli.mission.as_deref())?;
            let state = load_state(&repo, &mission)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&state)?);
            } else {
                print!("{}", output::render_status(&state));
            }
            Ok(0)
        }
        Command::Outcomes {
            json,
            all,
            window_days,
        } => {
            if all {
                // KRZ-329: the same fold, grouped by repo across the M8 host
                // catalog (mirrors `kranz ready --all`).
                let config = kranz_engine::paths::global_config()
                    .ok_or_else(|| anyhow::anyhow!("cannot locate the home directory"))?;
                let report =
                    crate::merged_costs::assess_all(&config, window_days, chrono::Utc::now());
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print!("{}", crate::merged_costs::render_org(&report));
                }
            } else {
                let outcomes = kranz_engine::outcomes::compute_outcomes(&repo)?;
                if json {
                    println!("{}", output::render_outcomes_json(&outcomes)?);
                } else {
                    print!("{}", output::render_outcomes(&outcomes));
                }
            }
            Ok(0)
        }
        Command::EscalationMetrics { json } => {
            let metrics = kranz_engine::escalation_metrics::compute_escalation_metrics(&repo)?;
            if json {
                println!("{}", output::render_escalation_metrics_json(&metrics)?);
            } else {
                print!("{}", output::render_escalation_metrics(&metrics));
            }
            Ok(0)
        }
        Command::Provenance { mission_id, json } => {
            let mission = select_mission(&repo, mission_id.as_deref().or(cli.mission.as_deref()))?;
            let chain = kranz_engine::provenance::compute_provenance(&repo, &mission)?;
            if json {
                println!("{}", output::render_provenance_json(&chain)?);
            } else {
                print!("{}", output::render_provenance(&chain));
            }
            Ok(0)
        }
        Command::GateScores { gate, json } => {
            let series = kranz_engine::gate_scores::compute_gate_score_series(&repo, &gate)?;
            if json {
                println!("{}", output::render_gate_score_series_json(&series)?);
            } else {
                print!("{}", output::render_gate_score_series(&series));
            }
            Ok(0)
        }
        Command::EvidenceBundle { mission_id, out } => {
            let mission = select_mission(&repo, mission_id.as_deref().or(cli.mission.as_deref()))?;
            let out = out.unwrap_or_else(|| PathBuf::from(format!("evidence-bundle-{mission}")));
            let outcome =
                kranz_engine::evidence_bundle::export_evidence_bundle(&repo, &mission, &out)?;
            println!(
                "evidence bundle for mission {mission} written to {} ({} files; {} resolved artefacts, {} unresolved)",
                outcome.out_dir.display(),
                outcome.files_written,
                outcome.resolved_artefacts,
                outcome.unresolved_artefacts,
            );
            Ok(0)
        }
        Command::ExportTraces {
            mission_id,
            all,
            out,
        } => {
            let jsonl = if all {
                cmd_export_traces_all(&repo)
            } else {
                let mission =
                    select_mission(&repo, mission_id.as_deref().or(cli.mission.as_deref()))?;
                cmd_export_traces(&repo, &mission)?
            };
            match out {
                Some(path) => std::fs::write(&path, &jsonl).with_context(|| {
                    format!("writing export-traces output to {}", path.display())
                })?,
                None => print!("{jsonl}"),
            }
            Ok(0)
        }
        Command::ExportCorpus {
            mission_id,
            all,
            out,
        } => {
            let jsonl = if all {
                cmd_export_corpus_all(&repo)
            } else {
                let mission =
                    select_mission(&repo, mission_id.as_deref().or(cli.mission.as_deref()))?;
                cmd_export_corpus(&repo, &mission)?
            };
            match out {
                Some(path) => std::fs::write(&path, &jsonl).with_context(|| {
                    format!("writing export-corpus output to {}", path.display())
                })?,
                None => print!("{jsonl}"),
            }
            Ok(0)
        }
        Command::Pause => {
            let mission = select_control_mission(&repo, cli.mission.as_deref())?;
            cmd_pause(&repo, &mission)?;
            println!("pause queued for mission {mission} (takes effect between worker runs)");
            if let Some(hint) = control_queue_hint(&repo, &mission) {
                println!("{hint}");
            }
            Ok(0)
        }
        Command::Resume => {
            let mission = select_control_mission(&repo, cli.mission.as_deref())?;
            cmd_resume(&repo, &mission)?;
            println!("resume queued for mission {mission} (takes effect between worker runs)");
            if let Some(hint) = control_queue_hint(&repo, &mission) {
                println!("{hint}");
            }
            Ok(0)
        }
        Command::Msg { text, interrupt } => {
            let mission = select_control_mission(&repo, cli.mission.as_deref())?;
            cmd_msg(&repo, &mission, &text, interrupt)?;
            if interrupt {
                println!(
                    "message queued for mission {mission} with --interrupt: the current worker \
                     run will be aborted (recorded as partial) before the message is injected"
                );
            } else {
                println!(
                    "message queued for mission {mission}; it is processed between worker runs"
                );
            }
            if let Some(hint) = control_queue_hint(&repo, &mission) {
                println!("{hint}");
            }
            Ok(0)
        }
        Command::Revise { id, instructions } => {
            let instructions = instructions.join(" ");
            cmd_request_revision(&repo, &id, &instructions)?;
            println!("revision request queued for mission {id}");
            if let Some(hint) = control_queue_hint(&repo, &id) {
                println!("{hint}");
            }
            Ok(0)
        }
        Command::Revision { command } => {
            match command {
                RevisionCommand::Approve { id, revision } => {
                    cmd_approve_revision(&repo, &id, revision)?;
                    println!("revision {revision} approval queued for mission {id}");
                    if let Some(hint) = control_queue_hint(&repo, &id) {
                        println!("{hint}");
                    }
                }
                RevisionCommand::Reject { id, revision } => {
                    cmd_reject_revision(&repo, &id, revision)?;
                    println!("revision {revision} rejection queued for mission {id}");
                    if let Some(hint) = control_queue_hint(&repo, &id) {
                        println!("{hint}");
                    }
                }
            }
            Ok(0)
        }
        Command::Grant { command } => {
            match command {
                GrantCommand::Approve { id, command } => {
                    cmd_approve_grant(&repo, &id, &command)?;
                    println!("grant approval for `{command}` queued for mission {id}");
                    if let Some(hint) = control_queue_hint(&repo, &id) {
                        println!("{hint}");
                    }
                }
                GrantCommand::Deny {
                    id,
                    command,
                    reason,
                } => {
                    cmd_deny_grant(&repo, &id, &command, &reason)?;
                    println!("grant denial for `{command}` queued for mission {id}");
                    if let Some(hint) = control_queue_hint(&repo, &id) {
                        println!("{hint}");
                    }
                }
            }
            Ok(0)
        }
        Command::Question { command } => {
            match command {
                QuestionCommand::List { id } => {
                    print!("{}", cmd_list_questions(&repo, &id)?);
                }
                QuestionCommand::Answer {
                    id,
                    question_id,
                    answer,
                    option,
                } => {
                    cmd_answer_question(&repo, &id, &question_id, &answer, option)?;
                    println!("answer for question {question_id} queued for mission {id}");
                    if let Some(hint) = control_queue_hint(&repo, &id) {
                        println!("{hint}");
                    }
                }
            }
            Ok(0)
        }
        Command::Missions => {
            print!("{}", cmd_missions(&repo)?);
            Ok(0)
        }
        Command::Abandon { id, reason } => {
            // A positional id wins over the global --mission; otherwise fall
            // back to the usual auto-selection.
            let mission = select_mission(&repo, id.as_deref().or(cli.mission.as_deref()))?;
            let reason = reason.as_deref().unwrap_or("abandoned by operator");
            cmd_abandon(&repo, &mission, reason, lock_force)?;
            println!("mission {mission} ABANDONED ({reason})");
            Ok(0)
        }
        Command::Clean { yes, all } => cmd_clean(&repo, yes, all),
        Command::Ticket { command } => dispatch_ticket(&repo, command, cli.mission.as_deref()),
        Command::Draft {
            slug,
            yes,
            from_mission,
        } => backlog::cmd_draft(
            repo,
            &slug,
            yes,
            from_mission.as_deref(),
            cli.dangerously_allow_all,
        )
        .await
        .map_err(augment_limit_hint),
        Command::Decompose { goal, yes } => {
            backlog::cmd_decompose(repo, &goal, yes, cli.dangerously_allow_all)
                .await
                .map_err(augment_limit_hint)
        }
        Command::Exec {
            file,
            yes,
            max_cycles,
            push,
            allow_unvalidated,
        } => crate::exec::cmd_exec(
            repo,
            file,
            yes,
            max_cycles,
            push,
            cli.dangerously_allow_all,
            allow_unvalidated,
        )
        .await
        .map_err(augment_limit_hint),
        Command::Queue => {
            print!("{}", backlog::cmd_queue(&repo));
            Ok(0)
        }
        Command::Scan { staged, range } => cmd_scan(&repo, staged, range.as_deref()),
        Command::DomainLint { seed_config, json } => {
            cmd_domain_lint(&repo, seed_config.as_deref(), json)
        }
        Command::HookGuard { config } => {
            let mut stdin = std::io::stdin();
            Ok(crate::hook_guard::run_hook_guard(&config, &mut stdin))
        }
        Command::Ready { json, all } => {
            if all {
                let config = kranz_engine::paths::global_config()
                    .ok_or_else(|| anyhow::anyhow!("cannot locate the home directory"))?;
                let report = crate::ready::assess_all(&config);
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print!("{}", crate::ready::render_org(&report));
                }
            } else {
                let report = crate::ready::assess(&repo);
                if json {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    print!("{}", crate::ready::render(&report));
                }
            }
            Ok(0)
        }
        Command::Work { once } => backlog::cmd_work(repo, once)
            .await
            .map_err(augment_limit_hint),
        Command::Serve {
            port,
            host,
            insecure_lan,
            read_auth,
            open,
            dashboard,
            token,
            read_token,
            slack,
        } => {
            cmd_serve(
                repo,
                host,
                port,
                insecure_lan,
                read_auth,
                open,
                dashboard,
                token,
                read_token,
                slack,
            )
            .await
        }
        Command::Release { url, token } => {
            let mission = select_mission(&repo, cli.mission.as_deref())?;
            cmd_release(&repo, &mission, &url, token).await
        }
        Command::Config { command } => {
            crate::config_cmd::cmd_config(&repo, command, cli.mission.as_deref())
        }
        Command::Pack { command } => match command {
            crate::cli::PackCommand::Lint { dir } => cmd_pack_lint(&dir),
        },
        Command::Otel {
            endpoint,
            from_start,
        } => crate::otel::run_otel(repo, cli.mission.clone(), endpoint, from_start).await,
    }
}

/// Dispatch the backend-free `kranz ticket …` subcommands. The global
/// `--mission` flag supplies the mission id to `ticket approve` when the ticket
/// goal can't be matched automatically.
fn dispatch_ticket(repo: &Path, command: TicketCommand, mission: Option<&str>) -> Result<i32> {
    match command {
        TicketCommand::List => {
            print!("{}", backlog::cmd_ticket_list(repo));
            Ok(0)
        }
        TicketCommand::Ready { include_deferred } => {
            print!("{}", backlog::cmd_ticket_ready(repo, include_deferred));
            Ok(0)
        }
        TicketCommand::Show { slug } => {
            print!("{}", backlog::cmd_ticket_show(repo, &slug)?);
            Ok(0)
        }
        TicketCommand::New { slug, title, goal } => {
            let path = backlog::cmd_ticket_new(repo, &slug, &title, goal.as_deref())?;
            println!("created ticket '{slug}' at {}", path.display());
            Ok(0)
        }
        TicketCommand::Note { slug, text } => {
            print!(
                "{}",
                crate::ticket_notes::cmd_ticket_note(repo, &slug, &text.join(" "))?
            );
            Ok(0)
        }
        TicketCommand::Notes { slug } => {
            print!("{}", crate::ticket_notes::cmd_ticket_notes(repo, &slug)?);
            Ok(0)
        }
        TicketCommand::Queue {
            slug,
            mission: explicit,
            force,
        } => {
            // A `ticket queue --mission` wins over the global `--mission`.
            backlog::cmd_ticket_queue(repo, &slug, explicit.as_deref().or(mission), force)
        }
        TicketCommand::Approve {
            slug,
            mission: explicit,
            force,
        } => {
            // Deprecated alias for `ticket queue` (D-A); `--mission` still
            // wins over the global `--mission`.
            backlog::cmd_ticket_approve(repo, &slug, explicit.as_deref().or(mission), force)
        }
        TicketCommand::MigrateState { yes } => backlog::cmd_ticket_migrate_state(repo, yes),
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Load + validate the layered config for `repo`; `--dangerously-allow-all`
/// is applied on top of the merged file layers.
pub fn load_config(repo: &Path, dangerously_allow_all: bool) -> Result<MissionConfig> {
    let mut cfg = config::load(repo)?;
    if dangerously_allow_all {
        cfg.dangerously_allow_all = true;
    }
    config::validate(&cfg)?;
    Ok(cfg)
}

/// Construct the real backend. Called lazily — only by `plan`, `run`, and the
/// backlog `draft` handler.
pub(crate) fn build_backend(cfg: &MissionConfig) -> Result<Arc<dyn AgentBackend>> {
    let backend = ClaudeBackend::discover(cfg.claude_binary.as_deref())?;
    Ok(Arc::new(backend))
}

/// Resolve the mission id: `--mission` wins; otherwise the repo's only
/// mission; with several, the one whose `events.jsonl` was modified last.
pub fn select_mission(repo: &Path, explicit: Option<&str>) -> Result<String> {
    if let Some(id) = explicit {
        require_mission(repo, id)?;
        return Ok(id.to_string());
    }
    let ids = MissionPaths::list_missions(repo);
    match ids.len() {
        0 => bail!(
            "no missions found under {}; create one with `kranz plan \"<goal>\"`",
            repo.join(".kranz").join("missions").display()
        ),
        1 => Ok(ids.into_iter().next().expect("len checked")),
        _ => {
            let mut best: Option<(SystemTime, String)> = None;
            for id in ids {
                let events = MissionPaths::new(repo, &id).events_file();
                let mtime = std::fs::metadata(&events)
                    .and_then(|m| m.modified())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                let newer = match &best {
                    Some((t, _)) => mtime >= *t,
                    None => true,
                };
                if newer {
                    best = Some((mtime, id));
                }
            }
            Ok(best.expect("non-empty list").1)
        }
    }
}

/// Resolve the mission a control-inbox WRITE (`kranz pause|resume|msg`)
/// targets. A terminal mission's inbox is never drained (the engine refuses
/// to run terminal missions), so enqueuing there would print success for a
/// silent no-op — the same lie `kranz config role` already refuses via the
/// shared engine resolver:
///
/// - explicit `--mission` id: routed through
///   [`control::resolve_active_mission`] — it must exist and be non-terminal
///   (the resolver's error names the actual status);
/// - no id: keeps [`select_mission`]'s newest-by-mtime defaulting UX, but
///   refuses a terminal pick instead of "succeeding" into a dead inbox.
pub fn select_control_mission(repo: &Path, explicit: Option<&str>) -> Result<String> {
    if explicit.is_some() {
        return Ok(control::resolve_active_mission(repo, explicit)?);
    }
    let mission = select_mission(repo, None)?;
    let status = load_state(repo, &mission)?.mission.status;
    if mission_catalog::is_terminal_status(status) {
        bail!(
            "mission {mission} is {status:?}; control commands apply only to active \
             missions (a terminal mission's inbox is never drained — see \
             `kranz missions`)"
        );
    }
    Ok(mission)
}

/// The honest post-enqueue note for `pause`/`resume`/`msg`: when no live
/// engine holds the mission lock, the command just sits in the inbox — say
/// so instead of implying it takes effect now. `None` while the mission is
/// actually running (a live lock holder will drain the inbox shortly).
pub fn control_queue_hint(repo: &Path, mission_id: &str) -> Option<String> {
    let paths = MissionPaths::new(repo, mission_id);
    (!mission_catalog::mission_lock_is_live(&paths)).then(|| {
        format!(
            "note: mission {mission_id} is not currently running — the command is \
             queued and applies when the mission next runs"
        )
    })
}

/// Pick the mission to resume planning: the explicit `--mission` (validated
/// to be in planning), or the newest-by-mtime mission whose folded status is
/// still `Planning`.
pub fn select_planning_mission(repo: &Path, explicit: Option<&str>) -> Result<String> {
    if let Some(id) = explicit {
        require_mission(repo, id)?;
        return Ok(id.to_string());
    }
    let mut best: Option<(SystemTime, String)> = None;
    for id in MissionPaths::list_missions(repo) {
        let Ok(state) = load_state(repo, &id) else {
            continue; // corrupt/foreign logs never block resume of a healthy one
        };
        if state.mission.status != MissionStatus::Planning {
            continue;
        }
        let events = MissionPaths::new(repo, &id).events_file();
        let mtime = std::fs::metadata(&events)
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if best.as_ref().is_none_or(|(t, _)| mtime >= *t) {
            best = Some((mtime, id));
        }
    }
    best.map(|(_, id)| id).ok_or_else(|| {
        anyhow!(
            "no mission is currently in planning under {} — start one with \
             `kranz plan \"<goal>\"`",
            repo.join(".kranz").join("missions").display()
        )
    })
}

/// When an error is really Claude's subscription usage window (session/rate
/// limit), say so and tell the user how to pick the work back up — the raw
/// backend error reads like a Kranz failure otherwise.
pub fn augment_limit_hint(e: anyhow::Error) -> anyhow::Error {
    let msg = format!("{e:#}").to_ascii_lowercase();
    if [
        "session limit",
        "usage limit",
        "rate limit",
        "hit your limit",
    ]
    .iter()
    .any(|s| msg.contains(s))
    {
        e.context(
            "this is your Claude subscription's usage window, not a Kranz failure. \
             The mission and its conversation are saved: when the limit resets, \
             `kranz plan` (no goal) resumes planning and `kranz run` resumes execution",
        )
    } else {
        e
    }
}

/// A mission exists iff its `events.jsonl` does.
fn require_mission(repo: &Path, mission_id: &str) -> Result<MissionPaths> {
    if !MissionPaths::is_safe_id(mission_id) {
        bail!(
            "invalid mission id '{mission_id}': ids cannot contain path separators, '..', or drive designators"
        );
    }
    let paths = MissionPaths::new(repo, mission_id);
    if !paths.events_file().is_file() {
        bail!(
            "mission '{mission_id}' not found under {} (see `kranz missions`)",
            paths.missions_dir().display()
        );
    }
    Ok(paths)
}

/// Read + fold a mission's event log (no lock — read-only observers are
/// always allowed, §4.3).
pub fn load_state(repo: &Path, mission_id: &str) -> Result<MissionState> {
    let paths = require_mission(repo, mission_id)?;
    let events = EventLog::read_events(&paths.events_file())
        .with_context(|| format!("reading the event log of mission '{mission_id}'"))?;
    let state = reducer::fold(&events)
        .with_context(|| format!("folding the event log of mission '{mission_id}'"))?;
    Ok(state)
}

/// Read + fold a mission's event log, then derive the validation-PASSED
/// instruction-pair dataset and render it as JSONL. Pure function of the
/// on-disk event log (no persisted dataset file), so consecutive
/// invocations over an unchanged log are byte-identical.
pub fn cmd_export_traces(repo: &Path, mission_id: &str) -> Result<String> {
    let paths = require_mission(repo, mission_id)?;
    let events = EventLog::read_events(&paths.events_file())
        .with_context(|| format!("reading the event log of mission '{mission_id}'"))?;
    let state = reducer::fold(&events)
        .with_context(|| format!("folding the event log of mission '{mission_id}'"))?;
    let pairs = trace_export::export_validated_traces(&state, &events);
    Ok(trace_export::to_jsonl(&pairs))
}

/// `--all`: aggregate validation-PASSED traces across every mission under
/// .kranz/missions. A mission whose event log is missing or unreadable (e.g.
/// still Planning, or a corrupt log) is skipped rather than failing the whole
/// export — one bad mission must not block the rest of the dataset.
pub fn cmd_export_traces_all(repo: &Path) -> String {
    let mut pairs = Vec::new();
    for mission_id in MissionPaths::list_missions(repo) {
        let paths = MissionPaths::new(repo, &mission_id);
        let Ok(events) = EventLog::read_events(&paths.events_file()) else {
            continue;
        };
        let Ok(state) = reducer::fold(&events) else {
            continue;
        };
        pairs.extend(trace_export::export_validated_traces(&state, &events));
    }
    trace_export::to_jsonl(&pairs)
}

/// Read a mission's event log, derive the provenance-tagged training corpus
/// (validated worker traces + divergence pairs + escalation judgments), and
/// render it as JSONL. Pure function of the on-disk event log (no persisted
/// dataset file), so consecutive invocations over an unchanged log are
/// byte-identical. No-follow like every read that probes the mission dir
/// (the corpus anchors the provenance replay's artefact resolution there).
pub fn cmd_export_corpus(repo: &Path, mission_id: &str) -> Result<String> {
    let paths = require_mission(repo, mission_id)?;
    paths.require_no_follow()?;
    let events = EventLog::read_events(&paths.events_file())
        .with_context(|| format!("reading the event log of mission '{mission_id}'"))?;
    let records = corpus_export::export_corpus(&paths.mission_dir(), mission_id, &events)
        .with_context(|| format!("deriving the training corpus of mission '{mission_id}'"))?;
    Ok(corpus_export::to_jsonl(&records))
}

/// `--all`: aggregate the corpus across every mission under .kranz/missions
/// (ids sorted, so the aggregate's mission order is deterministic). A
/// mission whose event log is missing, unreadable, or corrupt — or whose
/// path fails the no-follow guard — is skipped rather than failing the
/// whole export, mirroring `cmd_export_traces_all`.
pub fn cmd_export_corpus_all(repo: &Path) -> String {
    let mut records = Vec::new();
    for mission_id in MissionPaths::list_missions(repo) {
        let paths = MissionPaths::new(repo, &mission_id);
        if paths.require_no_follow().is_err() {
            continue;
        }
        let Ok(events) = EventLog::read_events(&paths.events_file()) else {
            continue;
        };
        let Ok(mission_records) =
            corpus_export::export_corpus(&paths.mission_dir(), &mission_id, &events)
        else {
            continue;
        };
        records.extend(mission_records);
    }
    corpus_export::to_jsonl(&records)
}

/// Loud multi-line warning on stderr for `--dangerously-allow-all`.
pub fn print_danger_banner() {
    eprintln!(
        "\n\
         ============================================================\n\
         !!  --dangerously-allow-all IS SET                        !!\n\
         !!                                                        !!\n\
         !!  Permission gating is BYPASSED for every agent         !!\n\
         !!  session (bypassPermissions). Workers can run ANY      !!\n\
         !!  command: file writes, network access, git push,       !!\n\
         !!  package publishes, sudo.                              !!\n\
         !!                                                        !!\n\
         !!  Only use this on a sandboxed, disposable checkout.    !!\n\
         ============================================================\n"
    );
}

/// Stdin as a channel of lines, so prompts can DISCARD type-ahead: a line
/// typed while an orchestrator turn was running must not silently answer the
/// next prompt (an early "/quit" once ate the plan-approval "y").
///
/// The blocking reader thread and its channel are a process-global
/// singleton shared by every instance. A per-instance thread would sit
/// blocked in `read_line` (holding the stdin lock) long after its receiver
/// is gone and steal the first line meant for a later prompt — exactly what
/// would happen when the plan-approval "start execution now" handoff reaches
/// the run loop's blocked-guidance prompt with the planning prompt's reader
/// still alive.
struct StdinLines {
    rx: Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>>>,
}

impl StdinLines {
    fn spawn() -> Self {
        static CHANNEL: std::sync::OnceLock<
            Arc<tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<String>>>,
        > = std::sync::OnceLock::new();
        let rx = CHANNEL
            .get_or_init(|| {
                let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
                std::thread::spawn(move || {
                    let mut buf = String::new();
                    loop {
                        buf.clear();
                        match std::io::stdin().read_line(&mut buf) {
                            Ok(0) | Err(_) => break, // EOF: channel closes on tx drop
                            Ok(_) => {
                                let line = buf.trim_end_matches(['\r', '\n']).to_string();
                                if tx.send(line).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
                Arc::new(tokio::sync::Mutex::new(rx))
            })
            .clone();
        StdinLines { rx }
    }

    /// Next line; `None` on EOF.
    async fn next(&mut self) -> Option<String> {
        self.rx.lock().await.recv().await
    }

    /// Drop everything already typed (returns how many lines were discarded).
    fn drain(&mut self) -> usize {
        // Prompts are strictly sequential, so the lock is always free; if it
        // ever were held, draining nothing is the safe answer.
        let Ok(mut rx) = self.rx.try_lock() else {
            return 0;
        };
        let mut n = 0;
        while rx.try_recv().is_ok() {
            n += 1;
        }
        n
    }

    /// Drain + warn: call right before showing a prompt. Interactive
    /// terminals only — piped stdin (scripted planning) delivers all lines
    /// up-front by design and must never be discarded.
    fn drain_noisily(&mut self, tty: bool) {
        if !std::io::stdin().is_terminal() {
            return;
        }
        let n = self.drain();
        if n > 0 {
            let (dim, reset) = if tty {
                (ansi::DIM, ansi::RESET)
            } else {
                ("", "")
            };
            eprintln!(
                "{dim}(ignored {n} line(s) typed while the orchestrator was working — \
                 the prompt below wants fresh input){reset}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// plan
// ---------------------------------------------------------------------------

/// `kranz plan [<goal>]`: create a mission (goal given) or resume the most
/// recent in-planning mission (no goal), then run the interactive planning
/// conversation until a plan is approved or the user quits.
async fn cmd_plan(
    repo: PathBuf,
    goal: Option<String>,
    cfg: MissionConfig,
    explicit_mission: Option<&str>,
    force_lock: LockForce,
) -> Result<i32> {
    let backend = build_backend(&cfg)?;
    let (mut engine, intro) = match goal {
        Some(goal) => {
            let engine = MissionEngine::create(backend, repo.clone(), &goal, cfg)?;
            let intro = format!("mission {} created (planning)", engine.mission_id());
            (engine, intro)
        }
        None => {
            let mission = select_planning_mission(&repo, explicit_mission)?;
            let engine = MissionEngine::resume(backend, repo.clone(), &mission, force_lock)?;
            if engine.state().mission.status != MissionStatus::Planning {
                return Err(anyhow!(
                    "mission {mission} is {:?}, not in planning — use 'kranz run' \
                     to execute it, or 'kranz plan \"<goal>\"' to start a new mission",
                    engine.state().mission.status
                ));
            }
            let intro = format!(
                "resuming planning for mission {mission} — the conversation continues \
                 where it left off"
            );
            (engine, intro)
        }
    };

    // A real terminal on both ends gets the full-screen planning TUI, which
    // owns the whole interaction (conversation, live activity, /plan +
    // approval, exit hints) — the event-tail printer below must never run
    // concurrently with it. Piped/scripted stdio keeps the line-mode REPL
    // unchanged: lines arrive up-front by design and are never discarded.
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        let mission_id = engine.mission_id().to_string();
        // The TUI tears down completely before returning (terminal restored,
        // engine dropped, mission lock released), so the run path below
        // starts on a clean main screen and can re-acquire the lock.
        return match crate::planning_tui::run(engine, intro).await? {
            PlanningOutcome::ApprovedRun => start_run_after_plan(repo, mission_id).await,
            PlanningOutcome::ApprovedExit | PlanningOutcome::NotApproved => Ok(0),
        };
    }
    println!("{intro}");
    let tty = std::io::stdout().is_terminal();

    // Live activity feed: without it, a long orchestrator turn (opus reading
    // the repo, extended thinking) is indistinguishable from a hang.
    let color = std::io::stderr().is_terminal();
    let stop = Arc::new(AtomicBool::new(false));
    let printer = tokio::spawn(tail::tail_events(
        engine.paths().events_file(),
        engine.state().last_seq,
        EventRenderer::planning(engine.state(), color),
        Arc::clone(&stop),
    ));
    let mut approved = false;
    let mut run_now = false;
    let mut stdin_lines = StdinLines::spawn();

    println!("talk to the orchestrator to shape the plan:");
    println!("  /plan   request the plan + cost estimate and review it for approval");
    println!("  /quit   exit planning (Ctrl-D works too)");

    loop {
        stdin_lines.drain_noisily(tty);
        if tty {
            print!("you> ");
            let _ = std::io::stdout().flush();
        }
        let Some(line) = stdin_lines.next().await else {
            break; // EOF = /quit
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        match line.as_str() {
            "/quit" => break,
            "/plan" => {
                let request = engine.request_plan().await;
                // A seed turn may have run inside this request (fresh session
                // or resume-ack); its reply came first — show it first.
                if let Some(seed) = engine.take_seed_reply() {
                    print_orchestrator_reply(&seed, tty);
                }
                let plan = match request {
                    Ok(PlanRequest::Ready(plan)) => plan,
                    Ok(PlanRequest::NotReady(text)) => {
                        // A conversational state, not an error: the
                        // orchestrator wants answers before emitting.
                        print_orchestrator_reply(&text, tty);
                        println!(
                            "the orchestrator isn't ready to emit the plan yet — answer it \
                             above, then /plan again."
                        );
                        continue;
                    }
                    Ok(PlanRequest::WrongPlan { reason }) => {
                        // Planner-initiated escalation, not an error: it can
                        // plan but believes the plan is likely wrong.
                        print_orchestrator_reply(&reason, tty);
                        println!(
                            "the orchestrator believes a plan here is likely WRONG — reframe \
                             the goal or fix the premise above, then /plan again."
                        );
                        continue;
                    }
                    Err(e) => {
                        eprintln!(
                            "kranz: plan request failed: {:#}",
                            augment_limit_hint(e.into())
                        );
                        continue;
                    }
                };
                println!("{}", output::render_plan(&plan));
                // Estimate with params calibrated from this repo's completed
                // missions (built-in defaults when there are none yet).
                let calibration = cost::calibrate(&repo);
                let estimate = cost::estimate(&plan, &engine.state().config, &calibration.params);
                let estimate = cost::apply_shape(estimate, &plan, &calibration);
                println!(
                    "{}",
                    output::render_cost_estimate(&estimate, calibration.missions_used)
                );

                stdin_lines.drain_noisily(tty);
                print!("approve? [y/N] ");
                let _ = std::io::stdout().flush();
                let answer = stdin_lines.next().await.unwrap_or_default();
                if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    match engine.approve_plan(plan) {
                        Ok(()) => {
                            let branch = engine.state().mission.mission_branch.clone();
                            approved = true;
                            // Interactive stdin gets the run-now offer;
                            // piped/scripted stdin keeps the historical
                            // output and never starts execution — scripts
                            // depend on `kranz plan` exiting after approval.
                            if std::io::stdin().is_terminal() {
                                println!("plan approved and committed on {branch}.");
                                stdin_lines.drain_noisily(tty);
                                print!("start execution now? [Y/n] ");
                                let _ = std::io::stdout().flush();
                                let reply = stdin_lines.next().await;
                                if run_now_answer(reply.as_deref()) {
                                    run_now = true;
                                } else {
                                    println!("run 'kranz run' to execute.");
                                }
                            } else {
                                println!(
                                    "plan approved and committed on {branch}. \
                                     run 'kranz run' to execute."
                                );
                            }
                            break;
                        }
                        Err(e) => eprintln!("kranz: plan approval failed: {e}"),
                    }
                } else {
                    println!("not approved — back to the conversation.");
                }
            }
            _ if line.starts_with('/') => {
                println!("unknown command {line}; use /plan or /quit");
            }
            _ => {
                let result = engine.planning_turn(&line).await;
                // Surface a captured seed reply (session start / re-seed)
                // before this turn's own output — it happened first.
                if let Some(seed) = engine.take_seed_reply() {
                    print_orchestrator_reply(&seed, tty);
                }
                match result {
                    Ok(reply) => print_orchestrator_reply(&reply, tty),
                    Err(e) => eprintln!(
                        "kranz: orchestrator turn failed: {:#}",
                        augment_limit_hint(e.into())
                    ),
                }
            }
        }
    }
    if !approved {
        println!(
            "leaving planning; mission {} was not approved. Resume anytime with `kranz plan`.",
            engine.mission_id()
        );
    }
    let mission_id = engine.mission_id().to_string();
    // Engine drop flushes buffered deltas and releases the lock; the
    // printer's final catch-up read then sees every event.
    drop(engine);
    stop.store(true, Ordering::Relaxed);
    let _ = printer.await;
    if run_now {
        // The planning engine (and its mission lock) is gone; the run path
        // re-acquires the lock itself.
        return start_run_after_plan(repo, mission_id).await;
    }
    Ok(0)
}

/// Parse the answer to the line-mode "start execution now? [Y/n]" prompt.
/// Empty input takes the default (yes); `n`/`no` (any case) decline; EOF
/// (`None`, e.g. Ctrl-D) declines too — execution spend must never start
/// without a live keyboard behind the consent.
pub fn run_now_answer(answer: Option<&str>) -> bool {
    match answer {
        None => false,
        Some(text) => !matches!(text.trim().to_ascii_lowercase().as_str(), "n" | "no"),
    }
}

/// Shared plan→run handoff: announce the transition, then drive the mission
/// loop exactly like `kranz run`. The planning engine must already be
/// dropped — [`run_mission_loop`] re-acquires the mission lock.
async fn start_run_after_plan(repo: PathBuf, mission_id: String) -> Result<i32> {
    println!(
        "starting mission {mission_id} — live event feed follows \
         (Ctrl-C safe; resume with 'kranz run')"
    );
    run_mission_loop(repo, mission_id, LockForce::No, true).await
}

/// Print an orchestrator reply, each line under a dim `orchestrator>` prefix.
fn print_orchestrator_reply(text: &str, tty: bool) {
    let prefix = if tty {
        format!("{}orchestrator>{} ", ansi::DIM, ansi::RESET)
    } else {
        "orchestrator> ".to_string()
    };
    for line in text.lines() {
        println!("{prefix}{line}");
    }
}

// ---------------------------------------------------------------------------
// run
// ---------------------------------------------------------------------------

/// `kranz run`: apply the `--dangerously-allow-all` opt-in (recorded as a
/// config.changed event via the control inbox), then drive the mission loop.
async fn cmd_run(
    repo: PathBuf,
    mission: String,
    force_lock: LockForce,
    dangerously_allow_all: bool,
) -> Result<i32> {
    // The mission's own config lives in the event log; the flag opts in via
    // the control inbox so the change is recorded as a config.changed event.
    if dangerously_allow_all {
        let paths = require_mission(&repo, &mission)?;
        control::enqueue(
            &paths,
            &ControlCommand::ConfigChange {
                patch: serde_json::json!({ "dangerouslyAllowAll": true }),
            },
        )?;
    }
    run_mission_loop(repo, mission, force_lock, true).await
}

/// Resume the mission, tail its events live, drive the loop to a terminal
/// state, and map it to an exit code (0 complete / 2 blocked / 1 failed).
/// Shared by `kranz run` and the plan-approval "start execution now" path.
pub(crate) async fn run_mission_loop(
    repo: PathBuf,
    mission: String,
    force_lock: LockForce,
    // Interactive callers (`kranz run`) prompt for guidance on a blocked
    // milestone; the batch dispatcher (`kranz work`) passes false so a blocked
    // mission returns exit 2 immediately instead of hanging on stdin forever.
    interactive: bool,
) -> Result<i32> {
    let cfg = load_config(&repo, false)?;
    let backend = build_backend(&cfg)?;
    run_mission_loop_with_backend(repo, mission, force_lock, interactive, backend).await
}

/// The body of [`run_mission_loop`], parameterized on the backend so tests
/// can drive it with [`kranz_engine::backend_mock::MockBackend`] instead of
/// discovering a real `claude` binary.
async fn run_mission_loop_with_backend(
    repo: PathBuf,
    mission: String,
    force_lock: LockForce,
    interactive: bool,
    backend: Arc<dyn AgentBackend>,
) -> Result<i32> {
    let paths = require_mission(&repo, &mission)?;

    loop {
        let mut engine =
            MissionEngine::resume(Arc::clone(&backend), repo.clone(), &mission, force_lock)?;

        // Live printer: tail events.jsonl from the pre-run head seq.
        let color = std::io::stderr().is_terminal();
        let renderer = EventRenderer::seeded(engine.state(), color);
        let stop = Arc::new(AtomicBool::new(false));
        let printer = tokio::spawn(tail::tail_events(
            engine.paths().events_file(),
            engine.state().last_seq,
            renderer,
            Arc::clone(&stop),
        ));

        let run_result = engine.run().await;
        // Drop the engine first: it flushes buffered stream deltas and releases
        // the lock, so the printer's final catch-up read sees every event.
        drop(engine);
        stop.store(true, Ordering::Relaxed);
        let _ = printer.await;

        let status = run_result?;
        // Reconcile the linked ticket's .status sidecar to match the mission's
        // terminal/blocked status. Non-fatal: a reconcile failure must never
        // change the exit code below.
        if let Err(e) = kranz_engine::work::reconcile_ticket_for_mission(&repo, &mission) {
            eprintln!("kranz run: warning: failed to reconcile linked ticket: {e}");
        }

        match status {
            MissionStatus::Complete => {
                println!("mission {mission} COMPLETE");
                return Ok(0);
            }
            MissionStatus::Blocked => {
                eprintln!(
                    "\n\
                     ==================== MILESTONE BLOCKED ====================\n\
                     A milestone is blocked (fix-cycle cap reached or blocked by\n\
                     the orchestrator). Inspect it with `kranz status`.\n\
                     ==========================================================="
                );
                // Interactive recovery: ask for guidance right here instead of
                // demanding the kranz msg / kranz run two-step. The batch
                // dispatcher (interactive=false) skips this so it never hangs.
                if interactive && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
                {
                    print!(
                        "guidance for the orchestrator (what to do about the block; \
                         empty line or Ctrl-D exits)\nguidance> "
                    );
                    let _ = std::io::stdout().flush();
                    let mut lines = StdinLines::spawn();
                    if let Some(text) = lines.next().await {
                        let text = text.trim().to_string();
                        if !text.is_empty() {
                            control::enqueue(
                                &paths,
                                &ControlCommand::Msg {
                                    text,
                                    interrupt: false,
                                },
                            )?;
                            println!("guidance queued — resuming the mission…");
                            continue;
                        }
                    }
                }
                eprintln!(
                    "unblock later by sending guidance via `kranz msg \"<text>\"` \
                     and re-running `kranz run`."
                );
                println!("mission {mission} BLOCKED");
                return Ok(2);
            }
            MissionStatus::Failed => {
                println!("mission {mission} FAILED");
                return Ok(1);
            }
            other => {
                println!(
                    "mission {mission} ended as {}",
                    output::mission_status_label(other)
                );
                return Ok(1);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// pause / resume / msg / revision (control inbox writers; no lock, no backend)
// ---------------------------------------------------------------------------

/// Enqueue a Pause control command. Returns the queued file path.
pub fn cmd_pause(repo: &Path, mission_id: &str) -> Result<PathBuf> {
    let paths = require_mission(repo, mission_id)?;
    Ok(control::enqueue(&paths, &ControlCommand::Pause)?)
}

/// Enqueue a Resume control command. Returns the queued file path.
pub fn cmd_resume(repo: &Path, mission_id: &str) -> Result<PathBuf> {
    let paths = require_mission(repo, mission_id)?;
    Ok(control::enqueue(&paths, &ControlCommand::Resume)?)
}

/// Enqueue a Msg control command. Returns the queued file path.
pub fn cmd_msg(repo: &Path, mission_id: &str, text: &str, interrupt: bool) -> Result<PathBuf> {
    let paths = require_mission(repo, mission_id)?;
    let cmd = ControlCommand::Msg {
        text: text.to_string(),
        interrupt,
    };
    Ok(control::enqueue(&paths, &cmd)?)
}

/// Enqueue a RequestRevision control command. Returns the queued file path.
pub fn cmd_request_revision(repo: &Path, mission_id: &str, instructions: &str) -> Result<PathBuf> {
    let instructions = instructions.trim();
    if instructions.is_empty() {
        bail!("revision instructions must not be empty");
    }
    let paths = require_revisable_mission(repo, mission_id)?;
    Ok(control::enqueue(
        &paths,
        &ControlCommand::RequestRevision {
            instructions: instructions.to_string(),
        },
    )?)
}

/// Enqueue an ApproveRevision control command. Returns the queued file path.
pub fn cmd_approve_revision(repo: &Path, mission_id: &str, revision: u32) -> Result<PathBuf> {
    let paths = require_pending_revision(repo, mission_id, revision)?;
    Ok(control::enqueue(
        &paths,
        &ControlCommand::ApproveRevision { revision },
    )?)
}

/// Enqueue a RejectRevision control command. Returns the queued file path.
pub fn cmd_reject_revision(repo: &Path, mission_id: &str, revision: u32) -> Result<PathBuf> {
    let paths = require_pending_revision(repo, mission_id, revision)?;
    Ok(control::enqueue(
        &paths,
        &ControlCommand::RejectRevision { revision },
    )?)
}

/// Enqueue an ApproveGrant control command. Returns the queued file path.
pub fn cmd_approve_grant(repo: &Path, mission_id: &str, command: &str) -> Result<PathBuf> {
    let paths = require_pending_grant(repo, mission_id, command)?;
    Ok(control::enqueue(
        &paths,
        &ControlCommand::ApproveGrant {
            command: command.to_string(),
        },
    )?)
}

/// Enqueue a DenyGrant control command. Returns the queued file path.
pub fn cmd_deny_grant(
    repo: &Path,
    mission_id: &str,
    command: &str,
    reason: &str,
) -> Result<PathBuf> {
    let paths = require_pending_grant(repo, mission_id, command)?;
    Ok(control::enqueue(
        &paths,
        &ControlCommand::DenyGrant {
            command: command.to_string(),
            reason: reason.to_string(),
        },
    )?)
}

/// Render the mission's open structured questions (ticket
/// `structured-human-question-events`) — the pending-decision projection the
/// dashboard and Slack also render — one block per question: id, ask, and
/// the indexed options (or a free-text note).
pub fn cmd_list_questions(repo: &Path, mission_id: &str) -> Result<String> {
    let mission_id = control::resolve_active_mission(repo, Some(mission_id))?;
    let state = load_state(repo, &mission_id)?;
    if state.pending_questions.is_empty() {
        return Ok(format!("mission {mission_id} has no open questions\n"));
    }
    let mut out = String::new();
    for q in &state.pending_questions {
        out.push_str(&format!(
            "{} ({}): {}\n",
            q.question_id,
            q.feature_id.as_deref().unwrap_or("mission"),
            q.text
        ));
        if q.options.is_empty() {
            out.push_str("  free-text answer expected\n");
        } else {
            for (index, option) in q.options.iter().enumerate() {
                out.push_str(&format!("  [{index}] {option}\n"));
            }
        }
    }
    Ok(out)
}

/// Enqueue an AnswerQuestion control command (ticket
/// `structured-human-question-events`). Returns the queued file path.
pub fn cmd_answer_question(
    repo: &Path,
    mission_id: &str,
    question_id: &str,
    answer: &str,
    option: Option<u32>,
) -> Result<PathBuf> {
    let paths = require_pending_question(repo, mission_id, question_id, option, answer)?;
    Ok(control::enqueue(
        &paths,
        &ControlCommand::AnswerQuestion {
            question_id: question_id.to_string(),
            answer: answer.to_string(),
            option,
        },
    )?)
}

/// Resolve the mission and confirm question `question_id` is open (and an
/// option-index answer is in range and matches the offered option), so the
/// enqueued answer can't silently land on a different (or absent) question
/// than the operator saw — the same stale-decision discipline as
/// [`require_pending_grant`]. The engine re-validates at drain time.
fn require_pending_question(
    repo: &Path,
    mission_id: &str,
    question_id: &str,
    option: Option<u32>,
    answer: &str,
) -> Result<MissionPaths> {
    let mission_id = control::resolve_active_mission(repo, Some(mission_id))?;
    let state = load_state(repo, &mission_id)?;
    let Some(pending) = state
        .pending_questions
        .iter()
        .find(|q| q.question_id == question_id)
    else {
        bail!("mission {mission_id} has no open question '{question_id}'");
    };
    if let Some(index) = option {
        match pending.options.get(index as usize) {
            Some(expected) if expected == answer => {}
            Some(expected) => bail!(
                "answer `{answer}` does not match option {index} (`{expected}`) of question '{question_id}'"
            ),
            None => bail!(
                "question '{question_id}' has no option {index} (it offered {})",
                pending.options.len()
            ),
        }
    }
    Ok(MissionPaths::new(repo, &mission_id))
}

fn require_revisable_mission(repo: &Path, mission_id: &str) -> Result<MissionPaths> {
    let mission_id = control::resolve_active_mission(repo, Some(mission_id))?;
    let state = load_state(repo, &mission_id)?;
    if state.mission.status == MissionStatus::Planning {
        bail!("mission {mission_id} has no approved plan to revise yet");
    }
    Ok(MissionPaths::new(repo, &mission_id))
}

fn require_pending_revision(repo: &Path, mission_id: &str, revision: u32) -> Result<MissionPaths> {
    let paths = require_revisable_mission(repo, mission_id)?;
    let state = load_state(repo, mission_id)?;
    match state.pending_revision {
        Some(pending) if pending.revision == revision => Ok(paths),
        Some(pending) => bail!(
            "mission {mission_id} is awaiting revision {}, not {revision}",
            pending.revision
        ),
        None => bail!("mission {mission_id} has no pending revision"),
    }
}

/// Resolve the mission and confirm a grant request for exactly `command` is
/// parked, so the enqueued approve/deny can't silently target a different (or
/// absent) request than the operator saw.
fn require_pending_grant(repo: &Path, mission_id: &str, command: &str) -> Result<MissionPaths> {
    let mission_id = control::resolve_active_mission(repo, Some(mission_id))?;
    let state = load_state(repo, &mission_id)?;
    match state.pending_grant_request {
        Some(pending) if pending.command == command => Ok(MissionPaths::new(repo, &mission_id)),
        Some(pending) => bail!(
            "mission {mission_id} is awaiting a grant for `{}`, not `{command}`",
            pending.command
        ),
        None => bail!("mission {mission_id} has no pending grant request"),
    }
}

pub fn cmd_scan(repo: &Path, staged: bool, range: Option<&str>) -> Result<i32> {
    if staged && range.is_some() {
        bail!("choose either --staged or --range, not both");
    }
    let git = kranz_engine::git_ops::GitRepo::open(repo)?;
    let diff = if staged {
        git.diff_staged()?
    } else if let Some(range) = range {
        git.diff_range(range)?
    } else {
        git.diff_range("HEAD")?
    };
    let allowed = std::fs::read_to_string(repo.join(kranz_engine::scrub::SECRET_ALLOWLIST_PATH))
        .ok()
        .map(|text| kranz_engine::scrub::read_allowlist_text(&text))
        .unwrap_or_default();
    let findings = kranz_engine::scrub::filter_allowed(
        kranz_engine::scrub::scan_unified_diff(&diff),
        &allowed,
    );
    if findings.is_empty() {
        println!("secret scan passed");
        Ok(0)
    } else {
        println!(
            "secret scan failed; add a fingerprint to {} only for a reviewed false positive:\n{}",
            kranz_engine::scrub::SECRET_ALLOWLIST_PATH,
            kranz_engine::scrub::format_findings(&findings)
        );
        Ok(2)
    }
}

/// `kranz domain-lint` (KRZ-314 clean-room boundary — see
/// `kranz_engine::domain_lint` module docs and docs/domain-lint.md).
///
/// Default mode lints the scoped tree against the committed hashed denylist:
/// exit 0 clean, exit 1 with each unwaived hit printed as
/// `<fingerprint> <path>:<line>` — never the matched text, which is the
/// vocabulary the boundary protects. `--seed-config` is the other half of
/// the workflow: regenerate the hash config from the operator-local
/// plaintext terms file (kept outside the repo), preserving the salt so
/// existing waiver fingerprints survive.
pub fn cmd_domain_lint(repo: &Path, seed_config: Option<&Path>, json: bool) -> Result<i32> {
    use kranz_engine::domain_lint as dl;
    let config_path = repo.join(dl::DENYLIST_PATH);

    if let Some(terms_file) = seed_config {
        let terms = std::fs::read_to_string(terms_file)
            .with_context(|| format!("read terms file {}", terms_file.display()))?;
        let existing = std::fs::read_to_string(&config_path).ok();
        let config = dl::seed_config(existing.as_deref(), &terms)?;
        // A repo may not have a .kranz/ directory yet (lint-only use).
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        std::fs::write(&config_path, &config)
            .with_context(|| format!("write {}", config_path.display()))?;
        let denylist = dl::load_denylist(&config)?;
        // The report is the count and the salt's fate — never a term.
        println!(
            "seeded {} ({} hashed terms, {})",
            dl::DENYLIST_PATH,
            denylist.term_count(),
            if existing.is_some() {
                "salt preserved"
            } else {
                "fresh salt"
            }
        );
        warn_if_terms_file_unprotected(repo, terms_file);
        return Ok(0);
    }

    let config_text = std::fs::read_to_string(&config_path).with_context(|| {
        format!(
            "read {} — seed it with `kranz domain-lint --seed-config <terms-file>` (docs/domain-lint.md)",
            dl::DENYLIST_PATH
        )
    })?;
    let denylist = dl::load_denylist(&config_text)?;
    let allowed = std::fs::read_to_string(repo.join(dl::ALLOWLIST_PATH))
        .ok()
        .map(|text| kranz_engine::scrub::read_allowlist_text(&text))
        .unwrap_or_default();
    let report = dl::lint_tree(repo, &denylist, &allowed)?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "passed": report.is_clean(),
                "filesScanned": report.files_scanned,
                "filesSkipped": report.files_skipped,
                "findings": report.findings,
            }))?
        );
    }
    if report.is_clean() {
        if !json {
            println!(
                "domain lint passed ({} files scanned)",
                report.files_scanned
            );
        }
        Ok(0)
    } else {
        if !json {
            println!(
                "domain lint failed: {} unwaived hit(s); add a fingerprint to {} only for a reviewed false positive:",
                report.findings.len(),
                dl::ALLOWLIST_PATH
            );
            for finding in &report.findings {
                println!("{} {}:{}", finding.fingerprint, finding.path, finding.line);
            }
        }
        Ok(1)
    }
}

/// Loudly warn when the plaintext terms file lives inside the repo and is
/// not gitignored — that file IS the protected vocabulary, so tracking it
/// would be the leak the boundary exists to prevent (the lint itself would
/// flag it on the next run; better to say so at seed time).
fn warn_if_terms_file_unprotected(repo: &Path, terms_file: &Path) {
    let (Ok(repo), Ok(terms_file)) = (repo.canonicalize(), terms_file.canonicalize()) else {
        return;
    };
    let Ok(relative) = terms_file.strip_prefix(&repo) else {
        return; // outside the repo: exactly where the plaintext belongs
    };
    let ignored = std::process::Command::new("git")
        .args(["check-ignore", "-q", "--"])
        .arg(relative)
        .current_dir(&repo)
        .status()
        .map(|status| status.success())
        // A failed probe must not nag; the lint is the backstop either way.
        .unwrap_or(true);
    if !ignored {
        eprintln!(
            "warning: {} is inside the repo and NOT gitignored — move it outside the repo or use {} (gitignored)",
            relative.display(),
            kranz_engine::domain_lint::TERMS_LOCAL_PATH
        );
    }
}

/// `kranz pack lint <dir>` (ticket pack-contract-gates-prompts): fully-local
/// pack contract validation — no City infrastructure, no repo needed. A
/// valid pack prints what it registered; a directory without a pack.toml is
/// not a pack and says so plainly (exit 0); an invalid pack fails closed
/// with exit 1 naming the offending field.
fn cmd_pack_lint(dir: &Path) -> Result<i32> {
    match kranz_engine::pack::Pack::load(dir) {
        Ok(Some(pack)) => {
            print!("{}", kranz_engine::pack::render_lint(&pack));
            Ok(0)
        }
        Ok(None) => {
            println!(
                "no pack at {} (no {}) — nothing to lint",
                dir.display(),
                kranz_engine::pack::PACK_MANIFEST
            );
            Ok(0)
        }
        Err(err) => {
            eprintln!("invalid pack at {}: {err}", dir.display());
            Ok(1)
        }
    }
}

// ---------------------------------------------------------------------------
// missions
// ---------------------------------------------------------------------------

/// One line per mission: `<id>  <STATUS>  <goal>`. Corrupt/unreadable logs
/// are reported inline instead of failing the whole listing.
pub fn cmd_missions(repo: &Path) -> Result<String> {
    let index_contents =
        std::fs::read_to_string(MissionPaths::new(repo, "_").missions_dir().join("index.md"))
            .unwrap_or_default();
    let mut ids = MissionPaths::list_missions(repo);
    for id in mission_catalog::mission_index_ids(&index_contents) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids.sort();
    if ids.is_empty() {
        return Ok("no missions\n".to_string());
    }
    // Reverse map mission→ticket from the ticket sidecars (the durable link
    // kranz draft records), so drafted missions are recognizable at a glance.
    let ticket_of: std::collections::HashMap<String, String> =
        kranz_engine::ticket::Ticket::list(repo)
            .into_iter()
            .filter_map(|t| {
                kranz_engine::ticket::Ticket::mission_for(repo, &t.slug).map(|m| (m, t.slug))
            })
            .collect();
    let mut out = String::new();
    for id in ids {
        let ticket = ticket_of
            .get(&id)
            .map(|s| format!("  [ticket: {s}]"))
            .unwrap_or_default();
        let paths = MissionPaths::new(repo, &id);
        if !paths.events_file().is_file() {
            out.push_str(&format!(
                "{id}  {:<10}  deleted mission (no data recorded)\n",
                "DELETED"
            ));
            continue;
        }
        // A symlinked mission dir is refused (P1 mission-path-no-follow),
        // never read into another repository's tree.
        if let Err(error) = paths.require_no_follow() {
            out.push_str(&format!("{id}  {:<10}  (unreadable: {error})\n", "FAILED"));
            continue;
        }
        match load_state(repo, &id) {
            Ok(state) => out.push_str(&format!(
                "{id}  {:<10}  {}{ticket}\n",
                output::mission_status_label(state.mission.status),
                state.mission.goal
            )),
            Err(e) => out.push_str(&format!("{id}  {:<10}  (unreadable: {e:#})\n", "FAILED")),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// abandon / clean (mission hygiene, roadmap M2)
// ---------------------------------------------------------------------------

/// Retire a mission via the engine's abandon path. Maps the engine's
/// `LockHeld` error to an actionable hint (stop the running mission, or pick
/// the right lock-steal tier) since that is the common operator mistake.
pub fn cmd_abandon(
    repo: &Path,
    mission_id: &str,
    reason: &str,
    force_lock: LockForce,
) -> Result<()> {
    require_mission(repo, mission_id)?;
    mission_catalog::abandon_mission(repo, mission_id, reason, force_lock).map_err(|e| {
        if matches!(e, kranz_engine::error::EngineError::LockHeld(_)) {
            anyhow!(
                "cannot abandon mission '{mission_id}' — an engine still holds its lock. \
                 Stop the running `kranz run` first. If the holder is a crashed leftover, \
                 pass --force-lock (steals unless the holder is provably alive); a provably \
                 LIVE holder that you have verified to be a zombie or foreign process \
                 additionally requires --dangerously-steal-live-lock.\n  (underlying: {e})"
            )
        } else {
            anyhow::Error::new(e).context(format!("abandoning mission '{mission_id}'"))
        }
    })
}

/// One mission the cleaner would remove, resolved to a printable row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanEntry {
    pub id: String,
    pub status_label: String,
    pub goal: String,
}

/// Decide which missions under `repo` are cleanable given the `--all` opt-in.
///
/// Never selects a mission whose lock is held by a live engine, nor one whose
/// log is unreadable (a corrupt log is left for the operator to inspect, not
/// silently deleted). The pure status→class decision lives in
/// [`mission_catalog::cleanable_class`]; this function layers the filesystem facts
/// (plan.json presence, lock liveness) on top.
pub fn select_cleanable(repo: &Path, all: bool) -> Vec<CleanEntry> {
    let mut out = Vec::new();
    for id in MissionPaths::list_missions(repo) {
        let paths = MissionPaths::new(repo, &id);
        // A live engine owns this directory: never touch it.
        if mission_catalog::mission_lock_is_live(&paths) {
            continue;
        }
        let Ok(state) = load_state(repo, &id) else {
            continue; // unreadable/corrupt log: leave it for inspection
        };
        let has_plan = paths.plan_file().is_file();
        if mission_catalog::cleanable_class(state.mission.status, has_plan).is_cleaned(all) {
            out.push(CleanEntry {
                id,
                status_label: output::mission_status_label(state.mission.status).to_string(),
                goal: state.mission.goal,
            });
        }
    }
    out
}

/// Render the "would remove" listing: one `<STATUS>  <id>  <goal>` row per
/// entry, or a single "nothing to clean" line when empty.
pub fn render_clean_listing(entries: &[CleanEntry]) -> String {
    if entries.is_empty() {
        return "nothing to clean\n".to_string();
    }
    let mut out = String::new();
    for e in entries {
        out.push_str(&format!("{:<10}  {}  {}\n", e.status_label, e.id, e.goal));
    }
    out
}

/// `kranz clean [--yes] [--all]`: list cleanable mission directories, confirm
/// (unless `--yes`), then `remove_dir_all` each. Only mission directories are
/// removed — branches and tags are never touched, and each removed mission's
/// own `missions/index.md` line is pruned while every other line is left
/// intact.
fn cmd_clean(repo: &Path, yes: bool, all: bool) -> Result<i32> {
    let entries = select_cleanable(repo, all);
    if entries.is_empty() {
        print!("{}", render_clean_listing(&entries));
        return Ok(0);
    }

    print!("{}", render_clean_listing(&entries));
    println!(
        "\n{} mission director{} above would be removed.{}",
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" },
        if all {
            ""
        } else {
            " (Complete missions are kept; pass --all to include them.)"
        }
    );

    if !yes && !confirm_clean()? {
        println!("clean aborted; nothing removed.");
        return Ok(0);
    }

    let removed = remove_missions(repo, &entries, true);
    println!(
        "cleaned {} mission director{}",
        removed.len(),
        if removed.len() == 1 { "y" } else { "ies" }
    );
    Ok(0)
}

/// `remove_dir_all` each entry's `.kranz/missions/<id>` directory, then prune
/// that mission's own line from `missions/index.md` (never a git branch/tag,
/// never any other mission's index line). Returns the ids actually removed;
/// `verbose` echoes each removal. Removal failures are reported on stderr and
/// skipped, never aborting the batch.
pub fn remove_missions(repo: &Path, entries: &[CleanEntry], verbose: bool) -> Vec<String> {
    let mut removed = Vec::new();
    for e in entries {
        let paths = MissionPaths::new(repo, &e.id);
        // Re-check liveness immediately before deleting: a Planning-husk can go
        // live during the confirmation prompt (kranz plan writes the lock file
        // before its session runs), and deleting a mission dir out from under a
        // running engine would corrupt it. This closes the unbounded
        // human-prompt window (a sub-ms race remains but is bounded).
        if mission_catalog::mission_lock_is_live(&paths) {
            eprintln!("kranz: skipping {} — became live since listing", e.id);
            continue;
        }
        let dir = paths.mission_dir();
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                if verbose {
                    println!("removed {}", dir.display());
                }
                mission_catalog::prune_mission_index_file(repo, &e.id);
                removed.push(e.id.clone());
            }
            Err(err) => eprintln!("kranz: could not remove {}: {err}", dir.display()),
        }
    }
    removed
}

/// Read a `[y/N]` answer from stdin. Anything other than y/yes (any case) —
/// including EOF — declines, so a piped/closed stdin never deletes by default.
fn confirm_clean() -> Result<bool> {
    use std::io::BufRead;
    print!("proceed? [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    let n = std::io::stdin().lock().read_line(&mut line)?;
    if n == 0 {
        return Ok(false); // EOF
    }
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

// ---------------------------------------------------------------------------
// serve
// ---------------------------------------------------------------------------

/// `kranz serve`: run the REST/WS server, serving the dashboard build from
/// the first location that exists (see [`resolve_dashboard_dist`]).
///
/// Refuse a non-loopback bind unless the operator passed `--insecure-lan`.
/// Extracted so the gate is unit-testable without starting the server.
pub(crate) fn refuse_non_loopback_without_insecure_lan(
    bind: std::net::IpAddr,
    insecure_lan: bool,
) -> Result<()> {
    if !bind.is_loopback() && !insecure_lan {
        anyhow::bail!(
            "refusing to bind {bind}: non-loopback binds expose the API on the \
             network (every /api GET/POST/WS requires the mutation token). \
             Re-run with `--insecure-lan` if you intentionally trust this \
             network (LAN/tailnet), or keep the default `--host 127.0.0.1`."
        );
    }
    Ok(())
}

/// Maps `(bind_is_loopback, read_auth_flag)` to the effective
/// `require_read_token` boolean threaded into the server. Off-loopback binds
/// always require the read token (unchanged); `--read-auth` additionally
/// forces it on loopback binds — the deployment-ready read-auth mode.
/// Extracted so the gate is unit-testable without starting the server.
pub(crate) fn effective_require_read_token(bind_is_loopback: bool, read_auth: bool) -> bool {
    !bind_is_loopback || read_auth
}

/// Every `POST /api/...` requires the mutation token (protocol "Authority:
/// mutation token"): generated per serve (or pinned via `--token` for
/// scripting), printed for the operator, and handed to `--open`'s browser as
/// a `#token=<t>` fragment the dashboard stores. The read-only token
/// (`--read-token` / `$KRANZ_READ_TOKEN`) is generated alongside and stored
/// in its own file — it authenticates gated GETs and the WS upgrade but
/// never a mutation, so it is the one safe to hand to dashboards and agents.
#[allow(clippy::too_many_arguments)]
async fn cmd_serve(
    repo: PathBuf,
    host: String,
    port: u16,
    insecure_lan: bool,
    read_auth: bool,
    open: bool,
    dashboard: Option<PathBuf>,
    token: Option<String>,
    read_token: Option<String>,
    slack: bool,
) -> Result<i32> {
    let bind: std::net::IpAddr = host
        .parse()
        .map_err(|e| anyhow!("--host '{host}' is not an IP address: {e}"))?;
    refuse_non_loopback_without_insecure_lan(bind, insecure_lan)?;
    if !bind.is_loopback() {
        eprintln!(
            "WARNING: binding {bind} with --insecure-lan — the API is reachable \
             beyond this machine. Every /api GET, POST, and WS upgrade requires \
             the mutation token (header or ?token=). Use only on a network you trust."
        );
    }
    if read_auth && effective_require_read_token(bind.is_loopback(), read_auth) {
        eprintln!(
            "--read-auth: GETs and the WS upgrade now require the mutation token too \
             (same as POSTs), including on loopback."
        );
    }
    // Bind BEFORE printing anything: `--port 0` picks an ephemeral port, and
    // the printed / `--open`ed URL must carry the REAL one.
    let listener = kranz_server::bind_listener(bind, port)
        .await
        .map_err(|e| anyhow!("failed to bind {bind}:{port}: {e}"))?;
    let local_addr = listener
        .local_addr()
        .map_err(|e| anyhow!("failed to read bound address: {e}"))?;
    // The global operator catalog composes one existing MissionHost per root.
    // With no host.repos block this resolves to the historical current-repo
    // host and retains every unscoped route.
    let multi_host = Arc::new(kranz_server::MultiRepoHost::from_global_config(
        kranz_engine::paths::global_config().as_deref(),
        repo.clone(),
    )?);
    // One watcher schedules ready repository queues in round-robin order;
    // each run holds a process-wide maxConcurrentRepos permit until it ends.
    multi_host.ensure_auto_work_started();

    // Opt-in Slack bridge, spawned alongside the server and stopped when the
    // process exits. serve_slack is a no-op (logs) when Slack is unconfigured,
    // so `--slack` is safe to pass unconditionally.
    if slack {
        if multi_host.uses_operator_catalog() {
            let catalog = multi_repo_slack_catalog(&multi_host)?;
            tokio::spawn(async move {
                let never = std::future::pending::<()>();
                if let Err(error) = kranz_slack::serve_slack_catalog(catalog, never).await {
                    tracing::error!(%error, "slack catalog bridge exited with an error");
                }
            });
        } else {
            let context = single_repo_slack_context(&multi_host)?;
            let repo_slack = context.root().to_path_buf();
            let hosted = context.host().ok_or_else(|| {
                anyhow!(
                    "cannot start Slack bridge for unavailable repository '{}': {}",
                    context.id(),
                    context.unavailable_reason().unwrap_or("unavailable")
                )
            })?;
            let bridge_host: kranz_slack::SharedHost =
                Arc::new(crate::host_bridge::HostedPlanning(hosted.clone()));
            tokio::spawn(async move {
                // No graceful-shutdown wiring for the CLI's long-lived server:
                // this future never resolves, so the bridge runs until the
                // process is killed (same lifetime as the server below).
                let never = std::future::pending::<()>();
                if let Err(e) =
                    kranz_slack::serve_slack(&repo_slack, Some(bridge_host), never).await
                {
                    tracing::error!(error = %e, "slack bridge exited with an error");
                }
            });
        }
    }

    let dashboard_assets = resolve_dashboard_assets(&repo, dashboard);
    // Display the ADDRESS ACTUALLY BOUND: `--host ::1` must not print an
    // unconnectable 127.0.0.1 URL, and v6 literals need brackets.
    let display_host = match local_addr.ip() {
        std::net::IpAddr::V6(v6) => format!("[{v6}]"),
        std::net::IpAddr::V4(v4) => v4.to_string(),
    };
    let url = format!("http://{display_host}:{}/", local_addr.port());
    let token = token
        .or_else(|| std::env::var("KRANZ_TOKEN").ok())
        .unwrap_or_else(kranz_server::generate_token);
    let read_token = read_token
        .or_else(|| std::env::var("KRANZ_READ_TOKEN").ok())
        .unwrap_or_else(kranz_server::generate_token);

    println!("kranz server on {url}");
    println!("mutation token: {token}");
    println!("read token: {read_token} (GETs/WS only — safe for dashboards and agents)");
    match &dashboard_assets {
        Some(DashboardAssets::Embedded) => println!(
            "serving embedded dashboard ({})",
            crate::embedded_dashboard::EMBEDDED_DASHBOARD_SOURCE
        ),
        Some(DashboardAssets::Dir(dir)) => println!("serving dashboard from {}", dir.display()),
        None => println!(
            "no dashboard build found (--dashboard, $KRANZ_DASHBOARD_DIST, \
             <repo>/apps/dashboard/dist, installed asset dirs, or the kranz checkout) \
             and no embedded dashboard is available; serving API only"
        ),
    }

    let static_assets = dashboard_assets.map(|assets| match assets {
        DashboardAssets::Dir(dir) => kranz_server::DashboardStatic::Dir(dir),
        DashboardAssets::Embedded => {
            kranz_server::DashboardStatic::Embedded(crate::embedded_dashboard::EMBEDDED_DASHBOARD)
        }
    });

    if open {
        // Give the server a moment to bind before pointing a browser at it.
        // The fragment hands the token to the dashboard without it ever
        // appearing in a request line or server log.
        let url = format!("{url}#token={token}");
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            open_browser(&url);
        });
    }

    let shutdown = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to install ctrl-c handler");
        }
    };
    let result = serve_multi_with_token_cleanup(
        &repo,
        multi_host,
        listener,
        static_assets,
        token,
        read_token,
        read_auth,
        shutdown,
    )
    .await;
    match result {
        Ok(()) => Ok(0),
        Err(e) => Err(anyhow!("server failed: {e}")),
    }
}

fn single_repo_slack_context(
    multi_host: &kranz_server::MultiRepoHost,
) -> Result<Arc<kranz_server::RepoContext>> {
    multi_host
        .compatibility_context()
        .ok_or_else(|| anyhow!("--slack requires one healthy configured repository"))
}

fn multi_repo_slack_catalog(
    multi_host: &kranz_server::MultiRepoHost,
) -> Result<kranz_slack::SlackCatalog> {
    let global_config = kranz_engine::paths::global_config()
        .ok_or_else(|| anyhow!("cannot locate the operator config directory for Slack affinity"))?;
    let affinity_path = global_config
        .parent()
        .expect("global config has a parent")
        .join("slack")
        .join("thread-affinity.json");
    let repos = multi_host
        .contexts()
        .map(|context| {
            let host = context.host().map(|host| {
                Arc::new(crate::host_bridge::HostedPlanning(host.clone()))
                    as kranz_slack::SharedHost
            });
            kranz_slack::SlackRepo {
                id: context.id().to_string(),
                root: context.root().to_path_buf(),
                display_name: context
                    .config()
                    .display_name
                    .clone()
                    .unwrap_or_else(|| context.id().to_string()),
                routes: context
                    .config()
                    .slack
                    .channels
                    .iter()
                    .map(|route| kranz_slack::SlackRoute {
                        team_id: route.team.clone(),
                        channel_id: route.channel.clone(),
                    })
                    .collect(),
                allow_users: context.config().slack.allow_users.clone(),
                available: context.is_healthy(),
                host,
                unavailable_reason: context.unavailable_reason().map(str::to_string),
                default: multi_host.default_repo() == Some(context.id()),
            }
        })
        .collect();
    kranz_slack::SlackCatalog::new(repos, affinity_path)
}

#[allow(clippy::too_many_arguments)]
async fn serve_multi_with_token_cleanup(
    repo: &Path,
    multi_host: Arc<kranz_server::MultiRepoHost>,
    listener: tokio::net::TcpListener,
    static_assets: Option<kranz_server::DashboardStatic>,
    token: String,
    read_token: String,
    read_auth: bool,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let (token_file, read_token_file) = if multi_host.uses_operator_catalog() {
        let address = listener
            .local_addr()
            .context("cannot resolve the bound address for operator token storage")?;
        let token_file = write_operator_serve_token(address, &token)
            .context("cannot securely store the operator serve token")?;
        let read_token_file = write_operator_serve_read_token(address, &read_token)
            .context("cannot securely store the operator serve read token")?;
        (token_file, read_token_file)
    } else {
        let token_file =
            write_serve_token(repo, &token).context("cannot securely store the serve token")?;
        let read_token_file = write_serve_read_token(repo, &read_token)
            .context("cannot securely store the serve read token")?;
        (token_file, read_token_file)
    };
    let result = kranz_server::serve_multi_on_listener(
        multi_host,
        listener,
        static_assets,
        Some(token),
        Some(read_token),
        read_auth,
        shutdown,
    )
    .await;
    remove_token_file(&token_file);
    remove_token_file(&read_token_file);
    result
}

/// Owns the write→serve→remove sequence for `.kranz/serve.token` so the
/// removal-on-shutdown behaviour is exercised by tests instead of just
/// asserted by a helper the tests bypass. Removes the token file on both the
/// `Ok` and `Err` serve paths — a server that fails to bind must not leave a
/// stale mutation token behind.
#[cfg(test)]
async fn serve_with_token_cleanup(
    repo: &Path,
    host: Arc<kranz_server::MissionHost>,
    listener: tokio::net::TcpListener,
    static_assets: Option<kranz_server::DashboardStatic>,
    token: String,
    read_token: String,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    // Filesystem read access to .kranz/serve.token confers mutation
    // authority — the same trust boundary as the .kranz/ directory itself,
    // so this file must never be written world- or group-readable.
    let token_file = write_serve_token(repo, &token)?;
    let read_token_file = write_serve_read_token(repo, &read_token)?;

    let result =
        kranz_server::serve_on_listener(host, listener, static_assets, Some(token), shutdown).await;

    remove_token_file(&token_file);
    remove_token_file(&read_token_file);

    result
}

/// Write the per-serve mutation token to `<repo>/.kranz/serve.token` so
/// local CLI commands can read it automatically instead of requiring
/// `--token`/`$KRANZ_TOKEN`. Filesystem read access to this file confers
/// mutation authority over the served repo — the same trust boundary as the
/// `.kranz/` directory itself, so it is written owner-only (0600 on Unix).
fn write_serve_token(repo: &Path, token: &str) -> std::io::Result<PathBuf> {
    write_token_file(&repo.join(".kranz").join("serve.token"), token)
}

/// Write the per-serve READ-ONLY token to `<repo>/.kranz/serve.read.token`.
/// Same storage discipline as the mutation token: it authenticates gated
/// GETs (mission state, transcripts), so it stays owner-only — but unlike
/// `serve.token` it never carries mutation authority, which is what makes it
/// safe to hand to dashboards and agents.
fn write_serve_read_token(repo: &Path, read_token: &str) -> std::io::Result<PathBuf> {
    write_token_file(&repo.join(".kranz").join("serve.read.token"), read_token)
}

/// Multi-root token location: `~/.kranz/serve/<endpoint>.token`. The complete
/// bound socket address distinguishes servers sharing a port on different
/// interfaces and lets `kranz release --url ...` refuse host-ambiguous
/// automatic credential discovery.
fn write_operator_serve_token(
    address: std::net::SocketAddr,
    token: &str,
) -> std::io::Result<PathBuf> {
    let global = kranz_engine::paths::global_config().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve operator config directory",
        )
    })?;
    let path = operator_serve_token_path(&global, address);
    write_token_file(&path, token)
}

/// The read-only sibling of the operator token: `~/.kranz/serve/<endpoint>.read.token`.
fn write_operator_serve_read_token(
    address: std::net::SocketAddr,
    read_token: &str,
) -> std::io::Result<PathBuf> {
    let global = kranz_engine::paths::global_config().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "cannot resolve operator config directory",
        )
    })?;
    let path = operator_serve_read_token_path(&global, address);
    write_token_file(&path, read_token)
}

/// `<endpoint>.token` → `<endpoint>.read.token` beside the operator token.
fn operator_serve_read_token_path(global_config: &Path, address: std::net::SocketAddr) -> PathBuf {
    operator_serve_token_path(global_config, address).with_extension("read.token")
}

fn operator_serve_token_path(global_config: &Path, address: std::net::SocketAddr) -> PathBuf {
    let endpoint = match address.ip() {
        std::net::IpAddr::V4(ip) => format!("v4-{:08x}-{}", u32::from(ip), address.port()),
        std::net::IpAddr::V6(ip) => format!("v6-{:032x}-{}", u128::from(ip), address.port()),
    };
    global_config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("serve")
        .join(format!("{endpoint}.token"))
}

/// Pre-endpoint migration path: `~/.kranz/serve/<port>.token`. Kept as a
/// last-resort discovery fallback so a live serve that still has only the
/// legacy file remains usable until the next `kranz serve` rewrite.
fn legacy_operator_serve_token_path(global_config: &Path, port: u16) -> PathBuf {
    global_config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("serve")
        .join(format!("{port}.token"))
}

fn write_token_file(path: &Path, token: &str) -> std::io::Result<PathBuf> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "token path has no parent")
    })?;
    std::fs::create_dir_all(dir)?;
    // Write a sibling temp file then rename over the destination so a crash
    // never leaves an empty/truncated credential at the stable path, and an
    // existing inode (or symlink) is replaced rather than rewritten in place.
    // A random suffix avoids PID-reuse create_new collisions after a crash.
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("serve.token"),
        uuid::Uuid::new_v4().as_simple()
    ));
    let write_tmp = || -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(token.as_bytes())?;
        file.flush()?;
        Ok(())
    };
    if let Err(error) = write_tmp() {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    }
    if let Err(error) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(path.to_path_buf())
}

fn remove_token_file(path: &Path) {
    if let Err(err) = std::fs::remove_file(path) {
        if err.kind() != std::io::ErrorKind::NotFound {
            eprintln!("kranz: could not remove {}: {err}", path.display());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DashboardAssets {
    Dir(PathBuf),
    Embedded,
}

#[derive(Debug, Clone, Default)]
struct DashboardResolutionInputs {
    env_dist: Option<PathBuf>,
    home: Option<PathBuf>,
    exe: Option<PathBuf>,
    manifest_dir: Option<PathBuf>,
    embedded_available: bool,
}

impl DashboardResolutionInputs {
    fn runtime() -> Self {
        Self {
            env_dist: std::env::var_os("KRANZ_DASHBOARD_DIST").map(PathBuf::from),
            home: std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from),
            exe: std::env::current_exe().ok(),
            manifest_dir: Some(PathBuf::from(env!("CARGO_MANIFEST_DIR"))),
            embedded_available: !crate::embedded_dashboard::EMBEDDED_DASHBOARD.is_empty(),
        }
    }
}

fn resolve_dashboard_assets(repo: &Path, explicit: Option<PathBuf>) -> Option<DashboardAssets> {
    resolve_dashboard_assets_from(repo, explicit, &DashboardResolutionInputs::runtime())
}

/// Search order:
/// 1. explicit `--dashboard DIR`
/// 2. `$KRANZ_DASHBOARD_DIST`
/// 3. `<repo>/apps/dashboard/dist` (mission repo IS the kranz checkout)
/// 4. installed asset dirs (`~/.kranz/dashboard/dist`, `<prefix>/share/kranz/...`)
/// 5. `apps/dashboard/dist` in the kranz source checkout used to build the binary
/// 6. packaged embedded dashboard assets (future crates.io/source installs)
fn resolve_dashboard_assets_from(
    repo: &Path,
    explicit: Option<PathBuf>,
    inputs: &DashboardResolutionInputs,
) -> Option<DashboardAssets> {
    if let Some(d) = explicit {
        // Explicitly requested: honor it even without index.html so the user
        // sees their own path in the log line (the server 404s clearly).
        return Some(DashboardAssets::Dir(d));
    }

    if let Some(d) = first_dashboard_dir(dashboard_dir_candidates(repo, inputs)) {
        return Some(DashboardAssets::Dir(d));
    }

    inputs
        .embedded_available
        .then_some(DashboardAssets::Embedded)
}

/// Locate a built dashboard (`index.html` + assets) on disk. This excludes the
/// embedded dashboard fallback used by `kranz serve`.
pub fn resolve_dashboard_dist(repo: &Path, explicit: Option<PathBuf>) -> Option<PathBuf> {
    match resolve_dashboard_assets(repo, explicit) {
        Some(DashboardAssets::Dir(dir)) => Some(dir),
        Some(DashboardAssets::Embedded) | None => None,
    }
}

fn dashboard_dir_candidates(repo: &Path, inputs: &DashboardResolutionInputs) -> Vec<PathBuf> {
    let mut candidates = Vec::new();

    candidates.extend(inputs.env_dist.clone());
    candidates.push(repo.join("apps").join("dashboard").join("dist"));

    if let Some(home) = &inputs.home {
        candidates.push(home.join(".kranz").join("dashboard").join("dist"));
        candidates.push(home.join(".kranz").join("dashboard"));
    }

    if let Some(exe) = &inputs.exe {
        candidates.extend(installed_dashboard_dirs(exe));
        candidates.extend(source_checkout_dist_from_exe(exe));
    }

    if let Some(manifest_dir) = &inputs.manifest_dir {
        candidates.extend(source_checkout_dist_from_manifest(manifest_dir));
        candidates.push(manifest_dir.join("assets").join("dashboard").join("dist"));
    }

    candidates
}

fn first_dashboard_dir(candidates: Vec<PathBuf>) -> Option<PathBuf> {
    candidates
        .into_iter()
        .find(|d| d.join("index.html").is_file())
}

fn installed_dashboard_dirs(exe: &Path) -> Vec<PathBuf> {
    let Some(bin_dir) = exe.parent() else {
        return Vec::new();
    };
    let mut dirs = vec![
        bin_dir.join("dashboard").join("dist"),
        bin_dir.join("dashboard"),
    ];
    if let Some(prefix) = bin_dir.parent() {
        dirs.push(
            prefix
                .join("share")
                .join("kranz")
                .join("dashboard")
                .join("dist"),
        );
        dirs.push(prefix.join("share").join("kranz").join("dashboard"));
    }
    dirs
}

fn source_checkout_dist_from_exe(exe: &Path) -> Option<PathBuf> {
    let profile_dir = exe.parent()?;
    let target_dir = profile_dir.parent()?;
    if target_dir.file_name()? != "target" {
        return None;
    }
    Some(
        target_dir
            .parent()?
            .join("apps")
            .join("dashboard")
            .join("dist"),
    )
}

fn source_checkout_dist_from_manifest(manifest_dir: &Path) -> Option<PathBuf> {
    Some(
        manifest_dir
            .parent()?
            .parent()?
            .join("apps")
            .join("dashboard")
            .join("dist"),
    )
}

/// Best-effort browser launch via the platform opener.
fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut c = std::process::Command::new("open");
        c.arg(url);
        c
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut c = std::process::Command::new("cmd");
        // `start` treats its first quoted argument as a window title.
        c.args(["/C", "start", "", url]);
        c
    };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(url);
        c
    };

    match command.spawn() {
        Ok(mut child) => {
            // Reap the opener off-thread; it exits immediately.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => eprintln!("kranz: could not open the browser: {e}"),
    }
}

// ---------------------------------------------------------------------------
// release
// ---------------------------------------------------------------------------

/// `kranz release [--url <u>] [--token <t>]`: POST to a running `kranz
/// serve`'s release endpoint to free the mission's single-writer lock. The
/// CLI runs in a different process and cannot reach serve's in-memory
/// registry directly, so this always goes over HTTP — never the event log.
/// Resolve the mutation token for `kranz release`, in precedence order:
/// `--token` > `$KRANZ_TOKEN` > operator process token for `--url` > the
/// single-repo compatibility file (loopback URLs only).
fn resolve_release_token(
    repo: &Path,
    url: &str,
    flag: Option<String>,
) -> Result<String, ReleaseTokenError> {
    let parsed = reqwest::Url::parse(url).ok();
    let operator_lookup = parsed
        .as_ref()
        .and_then(|url| {
            kranz_engine::paths::global_config()
                .as_deref()
                .map(|global| operator_token_for_url(global, url))
        })
        .unwrap_or(OperatorTokenLookup::Absent);
    let allow_repo_fallback = parsed.as_ref().is_some_and(automatic_repo_token_allowed);
    resolve_release_token_from_sources(repo, operator_lookup, allow_repo_fallback, flag)
}

/// Outcome of scanning endpoint-scoped and legacy operator token files for a
/// release URL. `Ambiguous` must not fall through to the single-repo
/// compatibility file — that would bypass the "refuse to guess" policy with
/// a different credential.
#[derive(Debug, Clone, PartialEq, Eq)]
enum OperatorTokenLookup {
    Absent,
    Found(String),
    Legacy(String),
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReleaseTokenError {
    Absent,
    Ambiguous,
}

fn resolve_release_token_from_sources(
    repo: &Path,
    operator_lookup: OperatorTokenLookup,
    allow_repo_fallback: bool,
    flag: Option<String>,
) -> Result<String, ReleaseTokenError> {
    if let Some(flag) = flag {
        return Ok(flag);
    }
    if let Ok(env) = std::env::var("KRANZ_TOKEN") {
        return Ok(env);
    }
    match operator_lookup {
        OperatorTokenLookup::Found(authority) => Ok(authority),
        // The port-only file predates endpoint scoping and may have survived
        // an ungraceful shutdown. A live single-repo serve writes the more
        // specific repository token, so prefer that before using the legacy
        // compatibility credential.
        OperatorTokenLookup::Legacy(authority) => {
            let repo_authority = allow_repo_fallback
                .then(|| read_token_file(&repo.join(".kranz").join("serve.token")))
                .flatten();
            Ok(repo_authority.unwrap_or(authority))
        }
        OperatorTokenLookup::Ambiguous => Err(ReleaseTokenError::Ambiguous),
        OperatorTokenLookup::Absent => allow_repo_fallback
            .then(|| read_token_file(&repo.join(".kranz").join("serve.token")))
            .flatten()
            .ok_or(ReleaseTokenError::Absent),
    }
}

fn scan_operator_token_addresses(
    global_config: &Path,
    addresses: &[std::net::SocketAddr],
) -> OperatorTokenLookup {
    let mut authority = None;
    for address in addresses {
        if let Some(found) = read_token_file(&operator_serve_token_path(global_config, *address)) {
            if authority.is_some() {
                // Several local servers match this URL (for example both v4
                // and v6 localhost). Refuse to guess which authority to send.
                return OperatorTokenLookup::Ambiguous;
            }
            authority = Some(found);
        }
    }
    match authority {
        Some(authority) => OperatorTokenLookup::Found(authority),
        None => OperatorTokenLookup::Absent,
    }
}

fn operator_token_for_url(global_config: &Path, url: &reqwest::Url) -> OperatorTokenLookup {
    let Some(port) = url.port_or_known_default() else {
        return OperatorTokenLookup::Absent;
    };
    let Some(host) = normalized_url_host(url) else {
        return OperatorTokenLookup::Absent;
    };
    // Exact named endpoints first; only if none match, fall back to
    // unspecified-bind aliases. That keeps a live 127.0.0.1 token usable
    // even when a stale 0.0.0.0 file from an earlier bind remains on disk.
    let endpoint_lookup = if host.eq_ignore_ascii_case("localhost") {
        let primary = [
            std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)),
            std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, port)),
        ];
        match scan_operator_token_addresses(global_config, &primary) {
            OperatorTokenLookup::Absent => scan_operator_token_addresses(
                global_config,
                &[
                    std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, port)),
                    std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, port)),
                ],
            ),
            other => other,
        }
    } else {
        let Ok(ip) = host.parse::<std::net::IpAddr>() else {
            return OperatorTokenLookup::Absent;
        };
        // Automatic discovery is loopback-only — same trust boundary as
        // automatic_repo_token_allowed. Non-loopback URLs require --token.
        if !ip.is_loopback() {
            return OperatorTokenLookup::Absent;
        }
        let primary = [std::net::SocketAddr::new(ip, port)];
        match scan_operator_token_addresses(global_config, &primary) {
            OperatorTokenLookup::Absent => {
                let unspecified = match ip {
                    std::net::IpAddr::V4(_) => {
                        std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED)
                    }
                    std::net::IpAddr::V6(_) => {
                        std::net::IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED)
                    }
                };
                scan_operator_token_addresses(
                    global_config,
                    &[std::net::SocketAddr::new(unspecified, port)],
                )
            }
            other => other,
        }
    };
    match endpoint_lookup {
        OperatorTokenLookup::Absent => {
            // Deprecated port-only filename from before endpoint scoping.
            match read_token_file(&legacy_operator_serve_token_path(global_config, port)) {
                Some(token) => OperatorTokenLookup::Legacy(token),
                None => OperatorTokenLookup::Absent,
            }
        }
        other => other,
    }
}

fn automatic_repo_token_allowed(url: &reqwest::Url) -> bool {
    normalized_url_host(url).is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

fn normalized_url_host(url: &reqwest::Url) -> Option<&str> {
    let host = url.host_str()?;
    Some(
        host.strip_prefix('[')
            .and_then(|host| host.strip_suffix(']'))
            .unwrap_or(host),
    )
}

fn read_token_file(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let authority = contents.trim_end().to_string();
    if authority.is_empty() {
        None
    } else {
        Some(authority)
    }
}

fn release_http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .build()
        .context("building release HTTP client")
}

fn release_repo_id_is_valid(id: &str) -> bool {
    let mut chars = id.chars();
    let valid_first = chars.next().is_some_and(|ch| ch.is_ascii_alphanumeric());
    let valid_rest = chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'));
    valid_first && valid_rest
}

async fn cmd_release(
    repo: &Path,
    mission_id: &str,
    url: &str,
    token: Option<String>,
) -> Result<i32> {
    let token = match resolve_release_token(repo, url, token) {
        Ok(token) => token,
        Err(ReleaseTokenError::Ambiguous) => {
            bail!(
                "multiple ~/.kranz/serve/<endpoint>.token files match {url}; \
                 pass --token for the intended serve instead of guessing"
            );
        }
        Err(ReleaseTokenError::Absent) => {
            bail!(
                "no mutation token available — pass --token, set $KRANZ_TOKEN, or use a local URL matching \
                 a live ~/.kranz/serve/<endpoint>.token / single-repo .kranz/serve.token \
                 (the token `kranz serve` prints on startup)"
            );
        }
    };

    let client = release_http_client()?;
    let repo_id = resolve_release_repo_id(repo, url, &client, &token).await?;
    if let Some(id) = repo_id.as_deref() {
        if !release_repo_id_is_valid(id) {
            bail!("live serve returned an invalid repository id '{id}'; refusing release");
        }
    }
    let endpoint = release_endpoint(url, mission_id, repo_id.as_deref());

    let response = client
        .post(&endpoint)
        .header("x-kranz-token", token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                anyhow!("no kranz serve reachable at {url} — is it running?")
            } else {
                anyhow::Error::new(e).context(format!("releasing mission '{mission_id}'"))
            }
        })?;

    match response.status() {
        reqwest::StatusCode::OK => {
            let body: serde_json::Value = response.json().await.unwrap_or_default();
            let released = body
                .get("released")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            if released {
                println!("mission {mission_id} released — the lock is now free");
            } else {
                println!("mission {mission_id} was already free (no lock held)");
            }
            Ok(0)
        }
        reqwest::StatusCode::CONFLICT => {
            eprintln!("kranz: mission {mission_id} has a turn in flight — try again shortly");
            Ok(1)
        }
        reqwest::StatusCode::NOT_FOUND => {
            eprintln!("kranz: unknown mission '{mission_id}' at {url}");
            Ok(1)
        }
        reqwest::StatusCode::UNAUTHORIZED => {
            eprintln!(
                "kranz: the token was missing or invalid — check --token / $KRANZ_TOKEN \
                 against the token `kranz serve` printed on startup"
            );
            Ok(1)
        }
        other => {
            let body = response.text().await.unwrap_or_default();
            eprintln!("kranz: release failed ({other}): {body}");
            Ok(1)
        }
    }
}

/// Ask the target serve which repository the selected root maps to. The
/// process-global config may have changed since that serve started, or the
/// URL may name a different process entirely, so it is never authoritative
/// for a mutation target.
async fn resolve_release_repo_id(
    repo: &Path,
    url: &str,
    client: &reqwest::Client,
    token: &str,
) -> Result<Option<String>> {
    live_release_repo_id(repo, url, client, token).await
}

fn release_repo_id_from_summaries(
    repo: &Path,
    repos: &[kranz_server::RepoSummary],
) -> Result<Option<String>> {
    if repos.is_empty() {
        bail!("the live serve returned an empty repository catalog; refusing an unscoped release");
    }
    let root = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let root_str = root.to_string_lossy();
    let matches: Vec<&kranz_server::RepoSummary> = repos
        .iter()
        .filter(|entry| {
            let entry_root = PathBuf::from(&entry.root);
            let entry_canon =
                std::fs::canonicalize(&entry_root).unwrap_or_else(|_| entry_root.clone());
            entry_canon == root || entry.root == root_str
        })
        .collect();
    match matches.as_slice() {
        [one] => Ok(Some(one.id.clone())),
        [] => Err(anyhow!(
            "selected repository '{}' is not present in the live serve catalog; refusing an unscoped release",
            repo.display()
        )),
        _ => Err(anyhow!(
            "selected repository '{}' matches multiple live serve catalog entries; refusing release",
            repo.display()
        )),
    }
}

async fn live_release_repo_id(
    repo: &Path,
    url: &str,
    client: &reqwest::Client,
    token: &str,
) -> Result<Option<String>> {
    let base = url.trim_end_matches('/');
    // On loopback binds GETs are tokenless (docs/protocol.md). Attaching the
    // mutation token would leak it to any process listening on a wrong
    // loopback port. Off-loopback serves require the read token — send it then.
    let mut request = client.get(format!("{base}/api/repos"));
    let loopback_catalog = reqwest::Url::parse(url)
        .ok()
        .is_some_and(|parsed| automatic_repo_token_allowed(&parsed));
    if !loopback_catalog {
        request = request.header("x-kranz-token", token);
    }
    let send_error = |e: reqwest::Error| {
        if e.is_connect() {
            anyhow!("no kranz serve reachable at {url} — is it running?")
        } else {
            anyhow::Error::new(e).context("listing live serve repositories")
        }
    };
    let mut response = request.send().await.map_err(send_error)?;
    // A loopback URL does not imply a loopback bind: `serve --host 0.0.0.0`
    // gates reads even when reached via 127.0.0.1. Only once the tokenless
    // read is refused is the token proven necessary — resend it then, to the
    // very server that just demanded it.
    if loopback_catalog && response.status() == reqwest::StatusCode::UNAUTHORIZED {
        response = client
            .get(format!("{base}/api/repos"))
            .header("x-kranz-token", token)
            .send()
            .await
            .map_err(send_error)?;
    }
    if !response.status().is_success() {
        bail!(
            "cannot list repositories at {url}: HTTP {}",
            response.status()
        );
    }
    let repos: Vec<kranz_server::RepoSummary> = response
        .json()
        .await
        .context("parsing GET /api/repos response")?;
    release_repo_id_from_summaries(repo, &repos)
}

fn release_endpoint(url: &str, mission_id: &str, repo_id: Option<&str>) -> String {
    let base = url.trim_end_matches('/');
    match repo_id {
        Some(repo_id) => {
            format!("{base}/api/repos/{repo_id}/missions/{mission_id}/release")
        }
        None => format!("{base}/api/missions/{mission_id}/release"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// `cargo test` runs unit tests concurrently on multiple threads by
    /// default, but `$KRANZ_TOKEN` is process-global state. Every test below
    /// that reads or writes it must hold this lock for its whole body so the
    /// mutations don't interleave across threads.
    static KRANZ_TOKEN_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// With no --token and no $KRANZ_TOKEN, `cmd_release` fails fast with an
    /// actionable error instead of attempting the HTTP call.
    #[tokio::test]
    // The guard is a plain std Mutex held only to serialize this test's
    // $KRANZ_TOKEN mutation against sibling tests in this module; the await
    // below never touches the lock itself.
    #[allow(clippy::await_holding_lock)]
    async fn release_without_a_token_errors_clearly() {
        let _guard = KRANZ_TOKEN_ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by KRANZ_TOKEN_ENV_LOCK above.
        unsafe {
            std::env::remove_var("KRANZ_TOKEN");
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        let err = cmd_release(&repo, "m-1", "http://127.0.0.1:4560", None)
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("--token"), "{msg}");
        assert!(msg.contains("KRANZ_TOKEN"), "{msg}");
    }

    /// With no --token and no $KRANZ_TOKEN, resolution falls back to
    /// `<repo>/.kranz/serve.token`.
    #[test]
    fn release_reads_token_file_when_flag_and_env_absent() {
        let _guard = KRANZ_TOKEN_ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by KRANZ_TOKEN_ENV_LOCK above.
        unsafe {
            std::env::remove_var("KRANZ_TOKEN");
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        write_serve_token(&repo, "file-token").unwrap();

        assert_eq!(
            resolve_release_token_from_sources(&repo, OperatorTokenLookup::Absent, true, None),
            Ok("file-token".to_string())
        );
    }

    #[test]
    fn release_reads_token_file_but_flag_overrides() {
        let _guard = KRANZ_TOKEN_ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by KRANZ_TOKEN_ENV_LOCK above.
        unsafe {
            std::env::remove_var("KRANZ_TOKEN");
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        write_serve_token(&repo, "file-token").unwrap();

        assert_eq!(
            resolve_release_token_from_sources(
                &repo,
                OperatorTokenLookup::Absent,
                true,
                Some("flag-token".to_string()),
            ),
            Ok("flag-token".to_string())
        );
    }

    #[test]
    fn release_reads_token_file_but_env_overrides() {
        let _guard = KRANZ_TOKEN_ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by KRANZ_TOKEN_ENV_LOCK above.
        unsafe {
            std::env::set_var("KRANZ_TOKEN", "env-token");
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        write_serve_token(&repo, "file-token").unwrap();

        let result =
            resolve_release_token_from_sources(&repo, OperatorTokenLookup::Absent, true, None);
        unsafe {
            std::env::remove_var("KRANZ_TOKEN");
        }
        assert_eq!(result, Ok("env-token".to_string()));
    }

    /// When flag, env, and file are all absent, resolution yields Absent so
    /// `cmd_release` can raise its actionable error.
    #[test]
    fn release_reads_token_file_none_present_yields_none() {
        let _guard = KRANZ_TOKEN_ENV_LOCK.lock().unwrap();
        // SAFETY: serialized by KRANZ_TOKEN_ENV_LOCK above.
        unsafe {
            std::env::remove_var("KRANZ_TOKEN");
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();

        assert_eq!(
            resolve_release_token_from_sources(&repo, OperatorTokenLookup::Absent, true, None),
            Err(ReleaseTokenError::Absent)
        );
    }

    #[test]
    fn release_prefers_operator_process_token_over_repo_compatibility_token() {
        let _guard = KRANZ_TOKEN_ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("KRANZ_TOKEN");
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        write_serve_token(&repo, "repo-token").unwrap();
        let global = tmp.path().join("operator").join("config.json");
        let address = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 4560));
        let operator = operator_serve_token_path(&global, address);
        write_token_file(&operator, "operator-token").unwrap();
        let url = reqwest::Url::parse("http://127.0.0.1:4560").unwrap();

        assert_eq!(
            resolve_release_token_from_sources(
                &repo,
                operator_token_for_url(&global, &url),
                true,
                None,
            ),
            Ok("operator-token".to_string())
        );
    }

    #[test]
    fn ambiguous_operator_token_does_not_fall_through_to_repo_file() {
        let _guard = KRANZ_TOKEN_ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("KRANZ_TOKEN");
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        write_serve_token(&repo, "repo-token").unwrap();

        assert_eq!(
            resolve_release_token_from_sources(&repo, OperatorTokenLookup::Ambiguous, true, None,),
            Err(ReleaseTokenError::Ambiguous)
        );
    }

    #[test]
    fn operator_token_discovery_is_ambiguous_for_dual_localhost_endpoint_files() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join(".kranz").join("config.json");
        let v4 = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 4560));
        let v6 = std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 4560));
        write_token_file(&operator_serve_token_path(&global, v4), "v4-token").unwrap();
        write_token_file(&operator_serve_token_path(&global, v6), "v6-token").unwrap();
        let url = reqwest::Url::parse("http://localhost:4560").unwrap();

        assert_eq!(
            operator_token_for_url(&global, &url),
            OperatorTokenLookup::Ambiguous
        );
    }

    #[test]
    fn operator_token_path_is_scoped_by_full_bound_endpoint() {
        let global = Path::new("/operator/.kranz/config.json");
        let a =
            operator_serve_token_path(global, std::net::SocketAddr::from(([127, 0, 0, 1], 4560)));
        let b =
            operator_serve_token_path(global, std::net::SocketAddr::from(([127, 0, 0, 2], 4560)));
        assert_ne!(a, b);
        assert_eq!(
            a,
            Path::new("/operator/.kranz/serve/v4-7f000001-4560.token")
        );
    }

    #[test]
    fn operator_token_discovery_handles_ipv6_url_brackets() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join(".kranz").join("config.json");
        let address = std::net::SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 4560));
        write_token_file(&operator_serve_token_path(&global, address), "ipv6-token").unwrap();
        let url = reqwest::Url::parse("http://[::1]:4560").unwrap();

        assert_eq!(
            operator_token_for_url(&global, &url),
            OperatorTokenLookup::Found("ipv6-token".to_string())
        );
    }

    #[test]
    fn automatic_token_discovery_refuses_remote_domain_urls() {
        let _guard = KRANZ_TOKEN_ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("KRANZ_TOKEN");
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        write_serve_token(&repo, "repo-token").unwrap();

        assert_eq!(
            resolve_release_token(&repo, "https://example.com:4560", None),
            Err(ReleaseTokenError::Absent)
        );
    }

    #[test]
    fn operator_token_discovery_refuses_non_loopback_ip_literals() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join(".kranz").join("config.json");
        let address = std::net::SocketAddr::from(([203, 0, 113, 10], 4560));
        write_token_file(&operator_serve_token_path(&global, address), "remote-token").unwrap();
        let url = reqwest::Url::parse("http://203.0.113.10:4560").unwrap();

        assert_eq!(
            operator_token_for_url(&global, &url),
            OperatorTokenLookup::Absent
        );
    }

    #[test]
    fn operator_token_discovery_prefers_exact_loopback_over_stale_unspecified() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join(".kranz").join("config.json");
        let loopback = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 4560));
        let unspecified = std::net::SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, 4560));
        write_token_file(&operator_serve_token_path(&global, loopback), "live-token").unwrap();
        write_token_file(
            &operator_serve_token_path(&global, unspecified),
            "stale-token",
        )
        .unwrap();
        let url = reqwest::Url::parse("http://127.0.0.1:4560").unwrap();

        assert_eq!(
            operator_token_for_url(&global, &url),
            OperatorTokenLookup::Found("live-token".to_string())
        );
    }

    #[test]
    fn operator_token_discovery_falls_back_to_legacy_port_token() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join(".kranz").join("config.json");
        write_token_file(
            &legacy_operator_serve_token_path(&global, 4560),
            "legacy-token",
        )
        .unwrap();
        let url = reqwest::Url::parse("http://127.0.0.1:4560").unwrap();

        assert_eq!(
            operator_token_for_url(&global, &url),
            OperatorTokenLookup::Legacy("legacy-token".to_string())
        );
    }

    #[test]
    fn stale_legacy_operator_token_does_not_mask_live_repo_token() {
        let _guard = KRANZ_TOKEN_ENV_LOCK.lock().unwrap();
        unsafe {
            std::env::remove_var("KRANZ_TOKEN");
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        write_serve_token(&repo, "live-repo-token").unwrap();
        let global = tmp.path().join(".kranz").join("config.json");
        write_token_file(
            &legacy_operator_serve_token_path(&global, 4560),
            "stale-legacy-token",
        )
        .unwrap();
        let url = reqwest::Url::parse("http://127.0.0.1:4560").unwrap();
        let operator = operator_token_for_url(&global, &url);

        assert_eq!(
            resolve_release_token_from_sources(&repo, operator, true, None),
            Ok("live-repo-token".to_string())
        );
    }

    #[test]
    fn operator_token_discovery_prefers_endpoint_file_over_legacy_port_token() {
        let tmp = tempfile::tempdir().unwrap();
        let global = tmp.path().join(".kranz").join("config.json");
        let loopback = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 4560));
        write_token_file(
            &operator_serve_token_path(&global, loopback),
            "endpoint-token",
        )
        .unwrap();
        write_token_file(
            &legacy_operator_serve_token_path(&global, 4560),
            "legacy-token",
        )
        .unwrap();
        let url = reqwest::Url::parse("http://127.0.0.1:4560").unwrap();

        assert_eq!(
            operator_token_for_url(&global, &url),
            OperatorTokenLookup::Found("endpoint-token".to_string())
        );
    }

    #[test]
    fn release_repo_id_from_summaries_refuses_duplicate_root() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let root = repo.to_string_lossy().into_owned();
        let summaries = vec![
            kranz_server::RepoSummary {
                id: "alpha".to_string(),
                root: root.clone(),
                display_name: "alpha".to_string(),
                group: None,
                pinned: false,
                is_default: true,
                status: "healthy".to_string(),
                error: None,
                activity: kranz_server::RepoActivity::default(),
            },
            kranz_server::RepoSummary {
                id: "beta".to_string(),
                root,
                display_name: "beta".to_string(),
                group: None,
                pinned: false,
                is_default: false,
                status: "healthy".to_string(),
                error: None,
                activity: kranz_server::RepoActivity::default(),
            },
        ];
        let error = release_repo_id_from_summaries(&repo, &summaries).unwrap_err();
        assert!(error.to_string().contains("matches multiple"));
    }

    #[test]
    fn slack_operator_catalog_is_not_rejected_by_legacy_context_helper() {
        fn init_git(root: &Path) {
            std::fs::create_dir_all(root).unwrap();
            let status = std::process::Command::new("git")
                .args(["init", "-q"])
                .arg(root)
                .status()
                .unwrap();
            assert!(status.success());
        }

        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a");
        init_git(&a);
        let multi = kranz_server::MultiRepoHost::from_config(kranz_server::HostConfig {
            default_repo: Some("a".to_string()),
            max_concurrent_repos: 1,
            repos: vec![kranz_server::RepoConfig {
                id: "a".to_string(),
                root: a,
                display_name: None,
                group: None,
                pinned: false,
                slack: kranz_server::RepoSlackConfig::default(),
            }],
        })
        .unwrap();
        let context = single_repo_slack_context(&multi).unwrap();
        assert_eq!(context.id(), "a");
    }

    #[test]
    fn release_endpoint_is_repo_scoped_when_catalog_id_is_known() {
        assert_eq!(
            release_endpoint("http://127.0.0.1:4560/", "same-id", Some("repo-b")),
            "http://127.0.0.1:4560/api/repos/repo-b/missions/same-id/release"
        );
    }

    #[test]
    fn release_repo_id_from_summaries_matches_live_catalog_root() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let summaries = vec![kranz_server::RepoSummary {
            id: "alpha".to_string(),
            root: repo.to_string_lossy().into_owned(),
            display_name: "alpha".to_string(),
            group: None,
            pinned: false,
            is_default: true,
            status: "healthy".to_string(),
            error: None,
            activity: kranz_server::RepoActivity::default(),
        }];
        assert_eq!(
            release_repo_id_from_summaries(&repo, &summaries)
                .unwrap()
                .as_deref(),
            Some("alpha")
        );
    }

    #[test]
    fn release_repo_id_from_summaries_refuses_unmatched_root() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("local");
        let other = tmp.path().join("other");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let summaries = vec![kranz_server::RepoSummary {
            id: "other".to_string(),
            root: other.to_string_lossy().into_owned(),
            display_name: "other".to_string(),
            group: None,
            pinned: false,
            is_default: false,
            status: "healthy".to_string(),
            error: None,
            activity: kranz_server::RepoActivity::default(),
        }];
        let error = release_repo_id_from_summaries(&repo, &summaries).unwrap_err();
        assert!(error
            .to_string()
            .contains("not present in the live serve catalog"));
    }

    #[test]
    fn release_repo_id_from_summaries_refuses_empty_catalog() {
        let tmp = tempfile::tempdir().unwrap();
        let error = release_repo_id_from_summaries(tmp.path(), &[]).unwrap_err();
        assert!(error.to_string().contains("empty repository catalog"));
    }

    #[tokio::test]
    async fn live_release_repo_lookup_authenticates_protected_catalog() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap();
        assert!(status.success());

        let catalog = Arc::new(kranz_server::MultiRepoHost::single(repo.clone()).unwrap());
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        // Loopback bind => read routes stay tokenless (matches production serve).
        let app = kranz_server::router_with_multi_repo_host_and_addr(
            catalog,
            None,
            Some("catalog-token".to_string()),
            Some(address),
            true,
            false,
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = release_http_client().unwrap();
        let url = format!("http://{address}");

        let repo_id = live_release_repo_id(&repo, &url, &client, "catalog-token")
            .await
            .unwrap();

        server.abort();
        let _ = server.await;
        assert_eq!(repo_id.as_deref(), Some("repo"));
    }

    #[tokio::test]
    async fn live_release_repo_lookup_retries_with_token_when_read_gated() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap();
        assert!(status.success());

        let catalog = Arc::new(kranz_server::MultiRepoHost::single(repo.clone()).unwrap());
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        // Non-loopback bind => reads are token-gated, but the operator on the
        // serve host still reaches it through a loopback URL. The tokenless
        // first read 401s; the lookup must retry with the token instead of
        // failing (`serve --host 0.0.0.0` + default `kranz release` URL).
        let app = kranz_server::router_with_multi_repo_host_and_addr(
            catalog,
            None,
            Some("catalog-token".to_string()),
            Some(address),
            false,
            true,
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = release_http_client().unwrap();
        let url = format!("http://{address}");

        let repo_id = live_release_repo_id(&repo, &url, &client, "catalog-token")
            .await
            .unwrap();

        server.abort();
        let _ = server.await;
        assert_eq!(repo_id.as_deref(), Some("repo"));
    }

    #[test]
    fn empty_token_files_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.token");
        std::fs::write(&path, "").unwrap();
        assert_eq!(read_token_file(&path), None);
    }

    #[cfg(unix)]
    #[test]
    fn serve_token_file_is_written_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        let path = write_serve_token(&repo, "secret").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[cfg(unix)]
    #[test]
    fn serve_read_token_file_is_written_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        let path = write_serve_read_token(&repo, "read-secret").unwrap();
        assert!(path.ends_with(".kranz/serve.read.token"));
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn operator_read_token_path_sits_beside_the_operator_token() {
        let global = Path::new("/home/op/.kranz/config.json");
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], 4560));
        let mutation = operator_serve_token_path(global, address);
        let read = operator_serve_read_token_path(global, address);
        assert_eq!(read, mutation.with_extension("read.token"));
        assert!(read.to_string_lossy().ends_with(".read.token"));
    }

    #[cfg(unix)]
    #[test]
    fn existing_token_permissions_are_hardened_before_replacement() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("serve.token");
        std::fs::write(&path, "old-token").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_token_file(&path, "new-token").unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new-token");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[tokio::test]
    async fn serve_token_file_is_removed_after_graceful_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        let path = repo.join(".kranz").join("serve.token");
        let read_path = repo.join(".kranz").join("serve.read.token");

        let host = std::sync::Arc::new(kranz_server::MissionHost::new(repo.clone()));
        let listener =
            kranz_server::bind_listener(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), 0)
                .await
                .unwrap();
        serve_with_token_cleanup(
            &repo,
            host,
            listener,
            None,
            "tok".to_string(),
            "read-tok".to_string(),
            std::future::ready(()),
        )
        .await
        .unwrap();

        assert!(!path.exists());
        assert!(!read_path.exists());
    }

    fn dashboard_at(path: PathBuf) -> PathBuf {
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("index.html"), "<!doctype html>").unwrap();
        path
    }

    fn inputs() -> DashboardResolutionInputs {
        DashboardResolutionInputs {
            embedded_available: false,
            ..Default::default()
        }
    }

    #[test]
    fn dashboard_resolution_honors_explicit_path_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let explicit = tmp.path().join("missing-dashboard");

        assert_eq!(
            resolve_dashboard_assets_from(&repo, Some(explicit.clone()), &inputs()),
            Some(DashboardAssets::Dir(explicit))
        );
    }

    #[test]
    fn dashboard_resolution_env_precedes_repo_and_invalid_env_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let repo_dist = dashboard_at(repo.join("apps").join("dashboard").join("dist"));
        let env_dist = dashboard_at(tmp.path().join("env-dist"));

        let mut with_env = inputs();
        with_env.env_dist = Some(env_dist.clone());
        assert_eq!(
            resolve_dashboard_assets_from(&repo, None, &with_env),
            Some(DashboardAssets::Dir(env_dist))
        );

        let mut with_invalid_env = inputs();
        with_invalid_env.env_dist = Some(tmp.path().join("missing-env-dist"));
        assert_eq!(
            resolve_dashboard_assets_from(&repo, None, &with_invalid_env),
            Some(DashboardAssets::Dir(repo_dist))
        );
    }

    #[test]
    fn dashboard_resolution_finds_installed_asset_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let exe = tmp.path().join("prefix").join("bin").join("kranz");
        let installed = dashboard_at(
            tmp.path()
                .join("prefix")
                .join("share")
                .join("kranz")
                .join("dashboard")
                .join("dist"),
        );

        let mut inputs = inputs();
        inputs.exe = Some(exe);
        assert_eq!(
            resolve_dashboard_assets_from(&repo, None, &inputs),
            Some(DashboardAssets::Dir(installed))
        );
    }

    #[test]
    fn dashboard_resolution_finds_checkout_used_to_build_installed_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("mission-repo");
        let checkout = tmp.path().join("kranz");
        let manifest_dir = checkout.join("crates").join("cli");
        let checkout_dist = dashboard_at(checkout.join("apps").join("dashboard").join("dist"));

        let mut inputs = inputs();
        inputs.manifest_dir = Some(manifest_dir);
        inputs.exe = Some(tmp.path().join("cargo-home").join("bin").join("kranz"));
        assert_eq!(
            resolve_dashboard_assets_from(&repo, None, &inputs),
            Some(DashboardAssets::Dir(checkout_dist))
        );
    }

    #[test]
    fn dashboard_resolution_falls_back_to_embedded_assets() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let mut inputs = inputs();
        inputs.embedded_available = true;

        assert_eq!(
            resolve_dashboard_assets_from(&repo, None, &inputs),
            Some(DashboardAssets::Embedded)
        );
    }

    #[test]
    fn embedded_dashboard_bundle_contains_index() {
        assert!(
            crate::embedded_dashboard::EMBEDDED_DASHBOARD
                .iter()
                .any(|file| file.path == "index.html"),
            "embedded dashboard source: {}",
            crate::embedded_dashboard::EMBEDDED_DASHBOARD_SOURCE
        );
    }

    #[test]
    fn serve_refuses_non_loopback_without_insecure_lan() {
        let bind: std::net::IpAddr = "0.0.0.0".parse().unwrap();
        let err = refuse_non_loopback_without_insecure_lan(bind, false).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("refusing to bind") && msg.contains("--insecure-lan"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn serve_allows_non_loopback_with_insecure_lan() {
        let bind: std::net::IpAddr = "0.0.0.0".parse().unwrap();
        refuse_non_loopback_without_insecure_lan(bind, true).unwrap();
    }

    #[test]
    fn serve_allows_loopback_without_insecure_lan() {
        let bind: std::net::IpAddr = "127.0.0.1".parse().unwrap();
        refuse_non_loopback_without_insecure_lan(bind, false).unwrap();
    }

    #[test]
    fn read_auth_on_loopback_requires_read_token() {
        assert!(effective_require_read_token(true, true));
    }

    #[test]
    fn read_auth_off_loopback_bind_does_not_require_read_token() {
        assert!(!effective_require_read_token(true, false));
    }

    #[test]
    fn read_auth_non_loopback_always_requires_read_token() {
        assert!(effective_require_read_token(false, true));
        assert!(effective_require_read_token(false, false));
    }

    // -----------------------------------------------------------------------
    // reconcile-on-terminal: `kranz run`'s loop heals a linked ticket
    // -----------------------------------------------------------------------

    fn reconcile_turn(reply: &str) -> Vec<kranz_engine::backend::AgentEvent> {
        vec![
            kranz_engine::backend_mock::mock_text(reply),
            kranz_engine::backend_mock::mock_result_text(reply),
        ]
    }

    fn reconcile_worker_pass() -> kranz_engine::backend_mock::MockScript {
        kranz_engine::backend_mock::MockScript::single_shot_json(&serde_json::json!({
            "result": "pass",
            "summary": "implemented and tested",
            "filesTouched": ["delivered.txt"],
            "testsAdded": [],
            "testEvidence": "all green",
            "commits": []
        }))
        .writes_file("delivered.txt", "delivered by the mock worker\n")
    }

    fn reconcile_plan_json() -> serde_json::Value {
        serde_json::json!({
            "goal": "ship the demo",
            "validationContract": [],
            "milestones": [{
                "title": "M1",
                "features": [{
                    "title": "F1",
                    "spec": "build the thing",
                    "validationCriteria": ["it works"]
                }]
            }]
        })
    }

    /// `run_mission_loop`'s post-run reconcile call must heal the linked
    /// ticket's stale `.status` sidecar once the mission reaches Complete —
    /// proving the f-1-3 wiring in `commands.rs` (not just the engine-level
    /// helper unit tests). Seeds the ticket at Running/Failed (a stale
    /// mismatch) so the assertion only passes if the reconcile call actually
    /// ran, not merely if the ticket happened to already be Done. Fails if
    /// the `reconcile_ticket_for_mission` call is removed from
    /// `run_mission_loop_with_backend`.
    #[tokio::test]
    async fn reconcile_on_terminal_after_cli_run_marks_ticket_done() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        let status = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());
        std::process::Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(&repo)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(&repo)
            .status()
            .unwrap();
        std::fs::write(repo.join("README.md"), "seed\n").unwrap();
        std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(&repo)
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(&repo)
            .status()
            .unwrap();
        let repo = std::fs::canonicalize(&repo).unwrap();

        let judgement = serde_json::json!({
            "decision": "complete",
            "guidance": "",
            "summary": "worker did the job"
        });
        // Two orchestrator sessions: `drop(engine)` + `resume()` between the
        // plan-approval phase and the run phase means the run phase gets a
        // fresh orchestrator session, not a continuation of the first.
        let orch_setup = kranz_engine::backend_mock::MockScript::streaming(vec![
            kranz_engine::backend_mock::mock_init("orch-session"),
            kranz_engine::backend_mock::mock_result_text("seed-hi"),
        ])
        .responding(vec![
            reconcile_turn("let's scope the demo"),
            reconcile_turn(&reconcile_plan_json().to_string()),
        ]);
        let orch_run = kranz_engine::backend_mock::MockScript::streaming(vec![
            kranz_engine::backend_mock::mock_init("orch-session-2"),
            kranz_engine::backend_mock::mock_result_text("ack"),
        ])
        .responding(vec![
            reconcile_turn("ack"),
            reconcile_turn(
                &serde_json::json!({"action": "commit-as-is", "note": "worker delivered files"})
                    .to_string(),
            ),
            reconcile_turn(&judgement.to_string()),
            reconcile_turn("NONE"),
            // Padding: extra decision turns the run loop may make (report,
            // milestone-complete, second judgement). Unused responses are
            // harmless; under-provisioning parks the streaming mock forever.
            reconcile_turn("NONE"),
            reconcile_turn("NONE"),
            reconcile_turn("NONE"),
        ]);
        let backend: Arc<dyn AgentBackend> =
            Arc::new(kranz_engine::backend_mock::MockBackend::with_scripts(vec![
                orch_setup,
                // The run-phase auth probe (orchestrator.rs:2711) fires BEFORE
                // the run's first orchestrator turn in this resumed-approved
                // flow, so it consumes the second script. It must be a
                // single-shot — a streaming script parks the probe forever.
                kranz_engine::backend_mock::MockScript::single_shot("ok"),
                // Consumption order in this flow: planning-orch, probe, worker,
                // run-orchestrator (its session starts at the judgement turn).
                reconcile_worker_pass(),
                orch_run,
            ]));

        let cfg = MissionConfig {
            skip_scrutiny: true,
            skip_functional: true,
            ..Default::default()
        };
        let mut engine =
            MissionEngine::create(Arc::clone(&backend), repo.clone(), "ship the demo", cfg)
                .unwrap();
        let mission_id = engine.mission_id().to_string();
        engine.planning_turn("ship the demo").await.unwrap();
        let request = engine.request_plan().await.unwrap();
        let plan = match request {
            PlanRequest::Ready(plan) => plan,
            PlanRequest::NotReady(text) => panic!("expected a ready plan, got: {text}"),
            PlanRequest::WrongPlan { reason } => {
                panic!("expected a ready plan, got a wrong-plan escalation: {reason}")
            }
        };
        engine.approve_plan(plan).unwrap();
        drop(engine);

        // Link a ticket to this mission and stamp it Running/Failed — a
        // stale mismatch the drove-to-Complete run must heal.
        kranz_engine::ticket::Ticket::record_mission(&repo, "my-ticket", &mission_id).unwrap();
        kranz_engine::ticket::Ticket::write_state(
            &repo,
            "my-ticket",
            kranz_engine::ticket::TicketState::Failed,
            None,
        )
        .unwrap();
        assert_eq!(
            kranz_engine::ticket::Ticket::read_state(&repo, "my-ticket"),
            kranz_engine::ticket::TicketState::Failed
        );

        let exit_code =
            run_mission_loop_with_backend(repo.clone(), mission_id, LockForce::No, false, backend)
                .await
                .unwrap();
        assert_eq!(exit_code, 0);

        assert_eq!(
            kranz_engine::ticket::Ticket::read_state(&repo, "my-ticket"),
            kranz_engine::ticket::TicketState::Done,
            "run_mission_loop must reconcile the linked ticket to Done on Complete"
        );
    }
}
