//! Kranz Slack Socket Mode bridge (design: docs/backlog-and-slack.md).
//!
//! Socket Mode means the bridge opens an **outbound** websocket to Slack (the
//! `wss://` URL comes from `apps.connections.open`, authorized by an app-level
//! token), so there is no public URL and no inbound port — the localhost trust
//! model is preserved. The bridge:
//!
//! - **posts** mission notifications to a channel (plan ready, ticket needs
//!   context, milestone blocked, mission complete/failed), one Slack thread per
//!   mission, threading persisted so restarts re-thread; and
//! - **handles** inbound interactions over the same socket (approve button →
//!   queue, threaded reply → orchestrator guidance, `/kranz ticket <title>` →
//!   scaffold a ticket).
//!
//! The bridge is **opt-in**: unless a bot token, app token, and channel are all
//! configured (env or `~/.kranz/config.json`), [`serve_slack`] logs and returns
//! immediately.
//!
//! ## Wiring into the CLI
//!
//! `kranz serve --slack` should spawn the bridge alongside the REST/WS server.
//! The single entry point is:
//!
//! ```no_run
//! # async fn wire(repo_root: std::path::PathBuf, host: Option<kranz_slack::SharedHost>) -> anyhow::Result<()> {
//! let shutdown = async { /* your Ctrl-C / server-shutdown future */ };
//! kranz_slack::serve_slack(&repo_root, host, shutdown).await?;
//! # Ok(()) }
//! ```
//!
//! `serve_slack` runs until `shutdown` resolves (or returns immediately when the
//! bridge is unconfigured), so spawn it on its own task and fire `shutdown` when
//! the server stops.

pub mod bridge;
pub mod client;
pub mod config;
pub mod format;
pub mod health;
pub mod host;
pub mod inbound;
pub mod outbound;
pub mod threads;

pub use client::SlackClient;
pub use config::{NotifyFlags, SlackConfig};
pub use host::{PlanOutcome, PlanningHost, SharedHost};

use anyhow::Result;
use std::path::Path;

/// Start the Slack bridge for `repo_root`, running until `shutdown` resolves.
///
/// Resolves the config ([`SlackConfig::from_config`]); if the bridge is
/// unconfigured, logs `slack not configured` and returns `Ok(())` immediately.
/// Otherwise runs the outbound tailing loop and the inbound Socket Mode loop
/// concurrently, both watching `shutdown`.
///
/// `shutdown` is any future that resolves when the host wants the bridge to
/// stop (e.g. a Ctrl-C signal or a server-shutdown broadcast). It is fanned out
/// to both loops via a shared notify, so a single trigger stops both.
/// `host` is the hosted-engine registry adapter (`kranz serve --slack` passes
/// one over the same `MissionHost` the web UI uses); `None` degrades the
/// planning-conversation / plan-review / approve-and-start surfaces to honest
/// refusals that point at the CLI.
pub async fn serve_slack(
    repo_root: &Path,
    host: Option<SharedHost>,
    shutdown: impl std::future::Future<Output = ()>,
) -> Result<()> {
    let Some(cfg) = SlackConfig::from_config(repo_root)? else {
        tracing::info!("slack not configured; bridge disabled");
        return Ok(());
    };
    tracing::info!(channel = %cfg.channel, "starting slack bridge");

    let client = SlackClient::new(&cfg)?;
    let threads = bridge::SharedThreads::load(repo_root)?;

    // Fan the single shutdown future out to both loops via a notify.
    let notify = std::sync::Arc::new(tokio::sync::Notify::new());
    let out_notify = notify.clone();
    let in_notify = notify.clone();
    let out_shutdown = async move { out_notify.notified().await };
    let in_shutdown = async move { in_notify.notified().await };

    let repo_out = repo_root.to_path_buf();
    let repo_in = repo_root.to_path_buf();
    let cfg_out = cfg.clone();
    let cfg_in = cfg;
    let client_out = client.clone();
    let client_in = client;
    let threads_out = threads.clone();
    let threads_in = threads;

    let outbound = bridge::run_bridge(cfg_out, client_out, repo_out, threads_out, out_shutdown);
    let inbound = bridge::run_socket(cfg_in, client_in, repo_in, threads_in, host, in_shutdown);

    // Wait for the external shutdown, then fire the notify so both loops stop.
    tokio::pin!(shutdown);
    tokio::join!(
        async {
            shutdown.await;
            notify.notify_waiters();
        },
        outbound,
        inbound,
    );
    Ok(())
}
