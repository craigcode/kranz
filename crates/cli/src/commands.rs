//! One function per `kranz` subcommand, plus the shared helpers (config
//! loading, backend construction, mission selection).
//!
//! The `ClaudeBackend` is constructed LAZILY — only `plan` and `run` spawn
//! agent sessions, so `status`/`msg`/`pause`/`resume`/`missions`/`serve`
//! work on machines without a `claude` binary installed.

use crate::cli::{Cli, Command};
use crate::output::{self, ansi};
use crate::tail::{self, EventRenderer};
use anyhow::{anyhow, bail, Context, Result};
use kranz_engine::backend::AgentBackend;
use kranz_engine::backend_claude::ClaudeBackend;
use kranz_engine::config;
use kranz_engine::control;
use kranz_engine::cost;
use kranz_engine::event_log::EventLog;
use kranz_engine::orchestrator::MissionEngine;
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
            cmd_plan(repo, goal, cfg).await
        }
        Command::Run => {
            let mission = select_mission(&repo, cli.mission.as_deref())?;
            cmd_run(repo, mission, cli.force_lock, cli.dangerously_allow_all).await
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
            let mission = select_mission(&repo, cli.mission.as_deref())?;
            cmd_pause(&repo, &mission)?;
            println!("pause queued for mission {mission} (takes effect between worker runs)");
            Ok(0)
        }
        Command::Resume => {
            let mission = select_mission(&repo, cli.mission.as_deref())?;
            cmd_resume(&repo, &mission)?;
            println!("resume queued for mission {mission} (takes effect between worker runs)");
            Ok(0)
        }
        Command::Msg { text, interrupt } => {
            let mission = select_mission(&repo, cli.mission.as_deref())?;
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
            Ok(0)
        }
        Command::Missions => {
            print!("{}", cmd_missions(&repo)?);
            Ok(0)
        }
        Command::Serve { port, open, dashboard } => cmd_serve(repo, port, open, dashboard).await,
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

/// Construct the real backend. Called lazily — only by `plan` and `run`.
fn build_backend(cfg: &MissionConfig) -> Result<Arc<dyn AgentBackend>> {
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

/// One blocking line from stdin (spawn_blocking keeps the tokio runtime
/// responsive). `None` means EOF or a read error.
async fn read_stdin_line() -> Option<String> {
    tokio::task::spawn_blocking(|| {
        let mut buf = String::new();
        match std::io::stdin().read_line(&mut buf) {
            Ok(0) => None,
            Ok(_) => Some(buf.trim_end_matches(['\r', '\n']).to_string()),
            Err(_) => None,
        }
    })
    .await
    .unwrap_or(None)
}

// ---------------------------------------------------------------------------
// plan
// ---------------------------------------------------------------------------

/// `kranz plan <goal>`: create the mission and run the interactive planning
/// conversation until a plan is approved or the user quits.
async fn cmd_plan(repo: PathBuf, goal: String, cfg: MissionConfig) -> Result<i32> {
    let backend = build_backend(&cfg)?;
    let mut engine = MissionEngine::create(backend, repo, &goal, cfg)?;
    let tty = std::io::stdout().is_terminal();

    println!("mission {} created (planning)", engine.mission_id());
    println!("talk to the orchestrator to shape the plan:");
    println!("  /plan   request the plan + cost estimate and review it for approval");
    println!("  /quit   exit planning (Ctrl-D works too)");

    loop {
        if tty {
            print!("you> ");
            let _ = std::io::stdout().flush();
        }
        let Some(line) = read_stdin_line().await else {
            break; // EOF = /quit
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        match line.as_str() {
            "/quit" => break,
            "/plan" => {
                let plan = match engine.request_plan().await {
                    Ok(plan) => plan,
                    Err(e) => {
                        eprintln!("kranz: plan request failed: {e}");
                        continue;
                    }
                };
                println!("{}", output::render_plan(&plan));
                let estimate = cost::estimate(
                    &plan,
                    &engine.state().config,
                    &cost::EstimateParams::default(),
                );
                println!("{}", output::render_cost_estimate(&estimate));

                print!("approve? [y/N] ");
                let _ = std::io::stdout().flush();
                let answer = read_stdin_line().await.unwrap_or_default();
                if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    match engine.approve_plan(plan) {
                        Ok(()) => {
                            println!(
                                "plan approved and committed on {}. run 'kranz run' to execute.",
                                engine.state().mission.mission_branch
                            );
                            return Ok(0);
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
            _ => match engine.planning_turn(&line).await {
                Ok(reply) => print_orchestrator_reply(&reply, tty),
                Err(e) => eprintln!("kranz: orchestrator turn failed: {e}"),
            },
        }
    }
    println!("leaving planning; mission {} was not approved.", engine.mission_id());
    Ok(0)
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

/// `kranz run`: resume the mission, tail its events live, drive the loop to
/// a terminal state, and map it to an exit code (0 complete / 2 blocked /
/// 1 failed).
async fn cmd_run(
    repo: PathBuf,
    mission: String,
    force_lock: bool,
    dangerously_allow_all: bool,
) -> Result<i32> {
    let cfg = load_config(&repo, dangerously_allow_all)?;
    let backend = build_backend(&cfg)?;
    let paths = require_mission(&repo, &mission)?;

    // The mission's own config lives in the event log; the flag opts in via
    // the control inbox so the change is recorded as a config.changed event.
    if dangerously_allow_all {
        control::enqueue(
            &paths,
            &ControlCommand::ConfigChange {
                patch: serde_json::json!({ "dangerouslyAllowAll": true }),
            },
        )?;
    }

    let mut engine = MissionEngine::resume(backend, repo, &mission, force_lock)?;

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
            Ok(0)
        }
        MissionStatus::Blocked => {
            eprintln!(
                "\n\
                 ==================== MILESTONE BLOCKED ====================\n\
                 A milestone is blocked (fix-cycle cap reached or blocked by\n\
                 the orchestrator). Inspect it with `kranz status`, then\n\
                 unblock it by sending guidance via `kranz msg \"<text>\"`\n\
                 and re-running `kranz run`.\n\
                 ==========================================================="
            );
            println!("mission {mission} BLOCKED");
            Ok(2)
        }
        MissionStatus::Failed => {
            println!("mission {mission} FAILED");
            Ok(1)
        }
        other => {
            println!("mission {mission} ended as {}", output::mission_status_label(other));
            Ok(1)
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
    let cmd = ControlCommand::Msg { text: text.to_string(), interrupt };
    Ok(control::enqueue(&paths, &cmd)?)
}

// ---------------------------------------------------------------------------
// missions
// ---------------------------------------------------------------------------

/// One line per mission: `<id>  <STATUS>  <goal>`. Corrupt/unreadable logs
/// are reported inline instead of failing the whole listing.
pub fn cmd_missions(repo: &Path) -> Result<String> {
    let ids = MissionPaths::list_missions(repo);
    if ids.is_empty() {
        return Ok("no missions\n".to_string());
    }
    let mut out = String::new();
    for id in ids {
        match load_state(repo, &id) {
            Ok(state) => out.push_str(&format!(
                "{id}  {:<10}  {}\n",
                output::mission_status_label(state.mission.status),
                state.mission.goal
            )),
            Err(e) => out.push_str(&format!("{id}  {:<10}  (unreadable: {e:#})\n", "FAILED")),
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// serve
// ---------------------------------------------------------------------------

/// `kranz serve`: run the REST/WS server, serving the dashboard build from
/// the first location that exists (see [`resolve_dashboard_dist`]).
async fn cmd_serve(repo: PathBuf, port: u16, open: bool, dashboard: Option<PathBuf>) -> Result<i32> {
    let static_dir = resolve_dashboard_dist(&repo, dashboard);
    let url = format!("http://127.0.0.1:{port}/");

    println!("kranz server on {url}");
    match &static_dir {
        Some(dir) => println!("serving dashboard from {}", dir.display()),
        None => println!(
            "no dashboard build found (--dashboard, $KRANZ_DASHBOARD_DIST, \
             <repo>/apps/dashboard/dist, or the kranz checkout); serving API only"
        ),
    }

    if open {
        // Give the server a moment to bind before pointing a browser at it.
        let url = url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            open_browser(&url);
        });
    }

    match kranz_server::serve(repo, port, static_dir).await {
        Ok(()) => Ok(0),
        Err(e) => Err(anyhow!("server failed: {e}")),
    }
}

/// Locate a built dashboard (`index.html` + assets). Search order:
/// 1. explicit `--dashboard DIR`
/// 2. `$KRANZ_DASHBOARD_DIST`
/// 3. `<repo>/apps/dashboard/dist` (mission repo IS the kranz checkout)
/// 4. `apps/dashboard/dist` relative to the running executable's checkout
///    (`target/{debug,release}/kranz` in the Kranz source tree) — this makes
///    `kranz serve` from any mission repo find the UI without configuration.
pub fn resolve_dashboard_dist(repo: &Path, explicit: Option<PathBuf>) -> Option<PathBuf> {
    let has_index = |d: &Path| d.join("index.html").is_file();

    if let Some(d) = explicit {
        // Explicitly requested: honor it even without index.html so the user
        // sees their own path in the log line (the server 404s clearly).
        return Some(d);
    }
    if let Some(d) = std::env::var_os("KRANZ_DASHBOARD_DIST").map(PathBuf::from) {
        if has_index(&d) {
            return Some(d);
        }
    }
    let repo_dist = repo.join("apps").join("dashboard").join("dist");
    if has_index(&repo_dist) {
        return Some(repo_dist);
    }
    if let Ok(exe) = std::env::current_exe() {
        // target/<profile>/kranz -> checkout root is two levels above target.
        if let Some(target_dir) = exe.parent().and_then(|p| p.parent()) {
            if let Some(checkout) = target_dir.parent() {
                let d = checkout.join("apps").join("dashboard").join("dist");
                if has_index(&d) {
                    return Some(d);
                }
            }
        }
    }
    None
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
