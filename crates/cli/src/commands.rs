//! One function per `kranz` subcommand, plus the shared helpers (config
//! loading, backend construction, mission selection).
//!
//! The `ClaudeBackend` is constructed LAZILY — only `plan` and `run` spawn
//! agent sessions, so `status`/`msg`/`pause`/`resume`/`missions`/`serve`
//! work on machines without a `claude` binary installed.

use crate::backlog;
use crate::cli::{Cli, Command, TicketCommand};
use crate::output::{self, ansi};
use crate::planning_tui::PlanningOutcome;
use crate::tail::{self, EventRenderer};
use anyhow::{anyhow, bail, Context, Result};
use kranz_engine::backend::AgentBackend;
use kranz_engine::backend_claude::ClaudeBackend;
use kranz_engine::config;
use kranz_engine::control;
use kranz_engine::cost;
use kranz_engine::event_log::{EventLog, LockForce};
use kranz_engine::orchestrator::{self, MissionEngine, PlanRequest};
use kranz_engine::paths::MissionPaths;
use kranz_engine::reducer;
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
        Command::Draft { slug, yes } => {
            backlog::cmd_draft(repo, &slug, yes, cli.dangerously_allow_all)
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
        Command::Work { once } => backlog::cmd_work(repo, once)
            .await
            .map_err(augment_limit_hint),
        Command::Serve {
            port,
            host,
            open,
            dashboard,
            token,
            slack,
        } => cmd_serve(repo, host, port, open, dashboard, token, slack).await,
        Command::Release { url, token } => {
            let mission = select_mission(&repo, cli.mission.as_deref())?;
            cmd_release(&repo, &mission, &url, token).await
        }
        Command::Config { command } => {
            crate::config_cmd::cmd_config(&repo, command, cli.mission.as_deref())
        }
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
        TicketCommand::Show { slug } => {
            print!("{}", backlog::cmd_ticket_show(repo, &slug)?);
            Ok(0)
        }
        TicketCommand::New { slug, title, goal } => {
            let path = backlog::cmd_ticket_new(repo, &slug, &title, goal.as_deref())?;
            println!("created ticket '{slug}' at {}", path.display());
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
    if orchestrator::is_terminal_status(status) {
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
    (!orchestrator::mission_lock_is_live(&paths)).then(|| {
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

        match run_result? {
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
// pause / resume / msg (control inbox writers; no lock, no backend)
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
    for id in kranz_engine::orchestrator::mission_index_ids(&index_contents) {
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
        if !MissionPaths::new(repo, &id).events_file().is_file() {
            out.push_str(&format!(
                "{id}  {:<10}  deleted mission (no data recorded)\n",
                "DELETED"
            ));
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
    orchestrator::abandon_mission(repo, mission_id, reason, force_lock).map_err(|e| {
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
/// [`orchestrator::cleanable_class`]; this function layers the filesystem facts
/// (plan.json presence, lock liveness) on top.
pub fn select_cleanable(repo: &Path, all: bool) -> Vec<CleanEntry> {
    let mut out = Vec::new();
    for id in MissionPaths::list_missions(repo) {
        let paths = MissionPaths::new(repo, &id);
        // A live engine owns this directory: never touch it.
        if orchestrator::mission_lock_is_live(&paths) {
            continue;
        }
        let Ok(state) = load_state(repo, &id) else {
            continue; // unreadable/corrupt log: leave it for inspection
        };
        let has_plan = paths.plan_file().is_file();
        if orchestrator::cleanable_class(state.mission.status, has_plan).is_cleaned(all) {
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
        if orchestrator::mission_lock_is_live(&paths) {
            eprintln!("kranz: skipping {} — became live since listing", e.id);
            continue;
        }
        let dir = paths.mission_dir();
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {
                if verbose {
                    println!("removed {}", dir.display());
                }
                orchestrator::prune_mission_index_file(repo, &e.id);
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
/// Every `POST /api/...` requires the mutation token (protocol "Authority:
/// mutation token"): generated per serve (or pinned via `--token` for
/// scripting), printed for the operator, and handed to `--open`'s browser as
/// a `#token=<t>` fragment the dashboard stores.
async fn cmd_serve(
    repo: PathBuf,
    host: String,
    port: u16,
    open: bool,
    dashboard: Option<PathBuf>,
    token: Option<String>,
    slack: bool,
) -> Result<i32> {
    let bind: std::net::IpAddr = host
        .parse()
        .map_err(|e| anyhow!("--host '{host}' is not an IP address: {e}"))?;
    if !bind.is_loopback() {
        eprintln!(
            "WARNING: binding {bind} — the API is reachable beyond this machine. \
             POSTs require the mutation token; GETs (mission states, transcripts) \
             do NOT. Use only on a network you trust (LAN/tailnet)."
        );
    }
    // One hosted-engine registry for BOTH clients: the axum handlers below and
    // (when --slack) the bridge. Handing the bridge its own MissionHost would
    // mean two engines contending for one mission's single-writer lock.
    let host = Arc::new(kranz_server::MissionHost::new(repo.clone()));
    // autoWork (default off) drains the queue automatically whenever entries
    // are waiting; started here (not in `MissionHost::new`) so test hosts
    // stay inert unless they opt in.
    host.ensure_auto_work_started();

    // Opt-in Slack bridge, spawned alongside the server and stopped when the
    // process exits. serve_slack is a no-op (logs) when Slack is unconfigured,
    // so `--slack` is safe to pass unconditionally.
    if slack {
        let repo_slack = repo.clone();
        let bridge_host: kranz_slack::SharedHost =
            Arc::new(crate::host_bridge::HostedPlanning(host.clone()));
        tokio::spawn(async move {
            // No graceful-shutdown wiring for the CLI's long-lived server:
            // this future never resolves, so the bridge runs until the
            // process is killed (same lifetime as the server below).
            let never = std::future::pending::<()>();
            if let Err(e) = kranz_slack::serve_slack(&repo_slack, Some(bridge_host), never).await {
                tracing::error!(error = %e, "slack bridge exited with an error");
            }
        });
    }

    let dashboard_assets = resolve_dashboard_assets(&repo, dashboard);
    let display_host = if bind.is_loopback() {
        "127.0.0.1".to_string()
    } else {
        bind.to_string()
    };
    let url = format!("http://{display_host}:{port}/");
    let token = token.unwrap_or_else(kranz_server::generate_token);

    println!("kranz server on {url}");
    println!("mutation token: {token}");
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
    let result =
        serve_with_token_cleanup(&repo, host, bind, port, static_assets, token, shutdown).await;
    match result {
        Ok(()) => Ok(0),
        Err(e) => Err(anyhow!("server failed: {e}")),
    }
}

/// Owns the write→serve→remove sequence for `.kranz/serve.token` so the
/// removal-on-shutdown behaviour is exercised by tests instead of just
/// asserted by a helper the tests bypass. Removes the token file on both the
/// `Ok` and `Err` serve paths — a server that fails to bind must not leave a
/// stale mutation token behind.
async fn serve_with_token_cleanup(
    repo: &Path,
    host: Arc<kranz_server::MissionHost>,
    bind: std::net::IpAddr,
    port: u16,
    static_assets: Option<kranz_server::DashboardStatic>,
    token: String,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    // Filesystem read access to .kranz/serve.token confers mutation
    // authority — the same trust boundary as the .kranz/ directory itself,
    // so this file must never be written world- or group-readable.
    let token_file = write_serve_token(repo, &token).ok();

    let result =
        kranz_server::serve_with_shutdown(host, bind, port, static_assets, Some(token), shutdown)
            .await;

    if token_file.is_some() {
        remove_serve_token(repo);
    }

    result
}

/// Write the per-serve mutation token to `<repo>/.kranz/serve.token` so
/// local CLI commands can read it automatically instead of requiring
/// `--token`/`$KRANZ_TOKEN`. Filesystem read access to this file confers
/// mutation authority over the served repo — the same trust boundary as the
/// `.kranz/` directory itself, so it is written owner-only (0600 on Unix).
fn write_serve_token(repo: &Path, token: &str) -> std::io::Result<PathBuf> {
    let dir = repo.join(".kranz");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("serve.token");
    std::fs::write(&path, token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(path)
}

/// Remove `<repo>/.kranz/serve.token` on clean shutdown. A process killed by
/// SIGKILL may leave a stale file behind; that's acceptable since the next
/// `kranz serve` overwrites it.
fn remove_serve_token(repo: &Path) {
    let path = repo.join(".kranz").join("serve.token");
    if let Err(err) = std::fs::remove_file(&path) {
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
/// `--token` flag > `$KRANZ_TOKEN` > `<repo>/.kranz/serve.token`.
fn resolve_release_token(repo: &Path, flag: Option<String>) -> Option<String> {
    flag.or_else(|| std::env::var("KRANZ_TOKEN").ok())
        .or_else(|| {
            std::fs::read_to_string(repo.join(".kranz").join("serve.token"))
                .ok()
                .map(|s| s.trim_end().to_string())
        })
}

async fn cmd_release(
    repo: &Path,
    mission_id: &str,
    url: &str,
    token: Option<String>,
) -> Result<i32> {
    let token = resolve_release_token(repo, token).ok_or_else(|| {
        anyhow!(
            "no mutation token available — pass --token, set $KRANZ_TOKEN, or run from a repo \
             with a `kranz serve` still holding <repo>/.kranz/serve.token \
             (the token `kranz serve` prints on startup)"
        )
    })?;

    let base = url.trim_end_matches('/');
    let endpoint = format!("{base}/api/missions/{mission_id}/release");

    let client = reqwest::Client::new();
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
            resolve_release_token(&repo, None),
            Some("file-token".to_string())
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
            resolve_release_token(&repo, Some("flag-token".to_string())),
            Some("flag-token".to_string())
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

        let result = resolve_release_token(&repo, None);
        unsafe {
            std::env::remove_var("KRANZ_TOKEN");
        }
        assert_eq!(result, Some("env-token".to_string()));
    }

    /// When flag, env, and file are all absent, resolution yields `None` so
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

        assert_eq!(resolve_release_token(&repo, None), None);
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

    #[tokio::test]
    async fn serve_token_file_is_removed_after_graceful_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        let path = repo.join(".kranz").join("serve.token");

        let host = std::sync::Arc::new(kranz_server::MissionHost::new(repo.clone()));
        serve_with_token_cleanup(
            &repo,
            host,
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            0,
            None,
            "tok".to_string(),
            std::future::ready(()),
        )
        .await
        .unwrap();

        assert!(!path.exists());
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
}
