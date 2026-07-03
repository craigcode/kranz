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
        Command::Serve {
            port,
            open,
            dashboard,
        } => cmd_serve(repo, port, open, dashboard).await,
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
    println!(
        "leaving planning; mission {} was not approved.",
        engine.mission_id()
    );
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
            println!(
                "mission {mission} ended as {}",
                output::mission_status_label(other)
            );
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
    let cmd = ControlCommand::Msg {
        text: text.to_string(),
        interrupt,
    };
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
async fn cmd_serve(
    repo: PathBuf,
    port: u16,
    open: bool,
    dashboard: Option<PathBuf>,
) -> Result<i32> {
    let dashboard_assets = resolve_dashboard_assets(&repo, dashboard);
    let url = format!("http://127.0.0.1:{port}/");

    println!("kranz server on {url}");
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
        let url = url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(600)).await;
            open_browser(&url);
        });
    }

    match kranz_server::serve_with_static(repo, port, static_assets).await {
        Ok(()) => Ok(0),
        Err(e) => Err(anyhow!("server failed: {e}")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
