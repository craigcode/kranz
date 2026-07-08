//! Runtime bridge: outbound mission tailing + inbound Socket Mode websocket.
//!
//! Two long-running tasks run concurrently until shutdown:
//!
//! - **outbound** ([`run_bridge`]) — polls each mission's `events.jsonl` (the
//!   same 250 ms tail the server WS uses), folds state incrementally, and posts
//!   Slack messages for the classified event classes (see [`crate::outbound`]).
//!   One Slack thread per mission: the first post for a mission becomes the
//!   thread root, recorded in [`ThreadMap`] so restarts re-thread; later posts
//!   reply in-thread.
//!
//! - **inbound** ([`run_socket`]) — opens the Socket Mode websocket (URL from
//!   `apps.connections.open`), acks every envelope within Slack's 3 s budget,
//!   routes it with the pure [`crate::inbound::route`], and applies the
//!   resulting [`Action`] (approve→queue, guidance→control inbox,
//!   ticket→scaffold). Reconnects with capped exponential backoff on any close
//!   (Slack rotates the wss URL, so each reconnect re-opens it).
//!
//! Shutdown is any `Future` that resolves when the host wants the bridge to
//! stop; both loops select on a shared notify fired from it.

use crate::client::SlackClient;
use crate::config::{NotifyFlags, SlackConfig};
use crate::health::BridgeHealth;
use crate::host::{PlanOutcome, SharedHost};
use crate::inbound::{route, Action, ThreadLookup};
use crate::outbound::{classify, NotifyClass, Outbound};
use crate::threads::ThreadMap;
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use kranz_engine::draft::DraftOutcome;
use kranz_engine::event_log::EventLog;
use kranz_engine::paths::MissionPaths;
use kranz_engine::reducer;
use kranz_engine::types::{ControlCommand, MissionState, MissionStatus};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

/// Poll interval over each mission's event log (matches the server WS tail).
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Backoff bounds for reconnecting the Socket Mode websocket.
const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// How long to wait for the next frame (of any kind, including Slack's
/// periodic pings) before treating the socket as stalled. A TCP connection
/// that dies silently (no FIN/RST, e.g. after a long idle period following a
/// sibling drain) otherwise parks `read.next()` forever and the reconnect
/// machinery is never reached.
const IDLE_TIMEOUT: Duration = Duration::from_secs(45);

/// How often the periodic health-log task in [`run_socket`] emits a liveness
/// line (info when healthy, warn when disconnected or stale).
const HEALTH_LOG_INTERVAL: Duration = Duration::from_secs(60);

/// Shared, thread-safe view of the mission↔thread map plus its repo root, so
/// both the outbound loop (which appends threads) and the inbound router (which
/// reads them) see a single consistent map, persisted on every change.
#[derive(Clone)]
pub struct SharedThreads {
    repo_root: PathBuf,
    inner: Arc<Mutex<ThreadMap>>,
}

impl SharedThreads {
    /// Load the persisted thread map for a repo into a shared, thread-safe view.
    pub fn load(repo_root: &Path) -> Result<Self> {
        Ok(Self {
            repo_root: repo_root.to_path_buf(),
            inner: Arc::new(Mutex::new(ThreadMap::load(repo_root)?)),
        })
    }

    fn thread_ts(&self, mission_id: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .thread_ts(mission_id)
            .map(str::to_string)
    }

    /// Record a mission's thread root and persist. A persistence error is
    /// logged, not fatal — losing the map only costs re-threading, and the next
    /// successful save recovers it.
    fn set(&self, mission_id: &str, thread_ts: &str) {
        let mut guard = self.inner.lock().unwrap();
        guard.set(mission_id, thread_ts);
        if let Err(e) = guard.save(&self.repo_root) {
            tracing::warn!(error = %e, "failed to persist slack thread map");
        }
    }
}

impl ThreadLookup for SharedThreads {
    fn mission_for_thread(&self, thread_ts: &str) -> Option<String> {
        self.inner
            .lock()
            .unwrap()
            .mission_for_thread(thread_ts)
            .map(str::to_string)
    }
}

/// Whether a notify class is enabled by the configured flags.
fn class_enabled(flags: &NotifyFlags, class: NotifyClass) -> bool {
    match class {
        NotifyClass::PlanReady => flags.plan_ready,
        NotifyClass::Blocked => flags.blocked,
        NotifyClass::Complete => flags.complete,
    }
}

/// Per-mission tailing cursor: the folded state and the last seq posted-through.
struct MissionCursor {
    state: MissionState,
    last_seq: u64,
}

type PostFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

trait OutboundPoster {
    fn post<'a>(
        &'a mut self,
        cfg: &'a SlackConfig,
        client: &'a SlackClient,
        threads: &'a SharedThreads,
        mission_id: &'a str,
        outbound: &'a Outbound,
    ) -> PostFuture<'a>;
}

struct SlackPoster;

impl OutboundPoster for SlackPoster {
    fn post<'a>(
        &'a mut self,
        cfg: &'a SlackConfig,
        client: &'a SlackClient,
        threads: &'a SharedThreads,
        mission_id: &'a str,
        outbound: &'a Outbound,
    ) -> PostFuture<'a> {
        Box::pin(post_outbound(cfg, client, threads, mission_id, outbound))
    }
}

/// Outbound loop: tail every mission under `repo_root`, posting classified
/// notifications to Slack. Runs until `shutdown` resolves.
///
/// New missions that appear after start are picked up on the next poll (the
/// mission list is re-read each tick). Each mission is folded from scratch the
/// first time it is seen, then advanced incrementally; on a fold/apply error we
/// drop the cursor and re-fold next tick (self-healing, like the server WS).
pub async fn run_bridge(
    cfg: SlackConfig,
    client: SlackClient,
    repo_root: PathBuf,
    threads: SharedThreads,
    shutdown: impl std::future::Future<Output = ()>,
) {
    tokio::pin!(shutdown);
    let mut cursors: HashMap<String, MissionCursor> = HashMap::new();
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                tracing::info!("slack outbound loop shutting down");
                return;
            }
            _ = ticker.tick() => {
                for mission_id in MissionPaths::list_missions(&repo_root) {
                    if let Err(e) =
                        poll_mission(&cfg, &client, &repo_root, &threads, &mission_id, &mut cursors)
                            .await
                    {
                        tracing::warn!(mission = %mission_id, error = %e, "slack outbound poll failed");
                    }
                }
            }
        }
    }
}

/// Advance one mission's cursor and post any classified notifications for new
/// events since the last poll.
async fn poll_mission(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    mission_id: &str,
    cursors: &mut HashMap<String, MissionCursor>,
) -> Result<()> {
    let mut poster = SlackPoster;
    poll_mission_with_poster(
        cfg,
        client,
        repo_root,
        threads,
        mission_id,
        cursors,
        &mut poster,
    )
    .await
}

/// Testable implementation of [`poll_mission`], with Slack posting injected so
/// cursor retry semantics can be exercised without a live Slack endpoint.
async fn poll_mission_with_poster(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    mission_id: &str,
    cursors: &mut HashMap<String, MissionCursor>,
    poster: &mut impl OutboundPoster,
) -> Result<()> {
    let paths = MissionPaths::new(repo_root, mission_id);
    let events_path = paths.events_file();
    if !events_path.is_file() {
        return Ok(());
    }

    // First sighting: fold the whole log, but DON'T replay historic
    // notifications — seed the cursor at head so only NEW events post. (A
    // freshly restarted bridge shouldn't re-announce every past mission.)
    if !cursors.contains_key(mission_id) {
        let events = EventLog::read_events(&events_path)?;
        let state = reducer::fold(&events)?;
        let last_seq = state.last_seq;
        cursors.insert(mission_id.to_string(), MissionCursor { state, last_seq });
        return Ok(());
    }

    let cursor = cursors.get_mut(mission_id).expect("just checked");
    let new_events = EventLog::read_events_after(&events_path, cursor.last_seq)?;
    for event in &new_events {
        // Advance the fold first so `classify` sees post-apply state.
        let state_before_event = cursor.state.clone();
        if reducer::apply(&mut cursor.state, event).is_err() {
            // Re-fold up to this event and retry once; on failure, resync the
            // cursor to head and skip (never wedge the loop).
            match refold_at(&events_path, event.seq) {
                Ok(rebuilt) => cursor.state = rebuilt,
                Err(e) => {
                    tracing::warn!(mission = %mission_id, error = %e, "slack re-fold failed; resyncing");
                    let events = EventLog::read_events(&events_path)?;
                    let state = reducer::fold(&events)?;
                    cursor.last_seq = state.last_seq;
                    cursor.state = state;
                    return Ok(());
                }
            }
        }

        if let Some(outbound) = classify(event, &cursor.state, repo_root) {
            if class_enabled(&cfg.notify, outbound.class()) {
                if let Err(e) = poster
                    .post(cfg, client, threads, mission_id, &outbound)
                    .await
                {
                    cursor.state = state_before_event;
                    tracing::warn!(mission = %mission_id, seq = event.seq, error = %e, "slack post failed; will retry");
                    return Ok(());
                }
            }
        }
        cursor.last_seq = event.seq;
    }
    Ok(())
}

/// Rebuild state from the log prefix ending at `seq` (mirrors the server WS).
fn refold_at(events_path: &Path, seq: u64) -> Result<MissionState> {
    let events = EventLog::read_events(events_path)?;
    let upto = usize::try_from(seq).unwrap_or(usize::MAX).min(events.len());
    Ok(reducer::fold(&events[..upto])?)
}

/// Post one notification, threading it under the mission's root (creating the
/// root on first post) and recording the thread ts.
async fn post_outbound(
    cfg: &SlackConfig,
    client: &SlackClient,
    threads: &SharedThreads,
    mission_id: &str,
    outbound: &Outbound,
) -> Result<()> {
    let dash = cfg.dashboard_url.as_deref();
    let blocks = match outbound {
        Outbound::PlanReady(p) => crate::format::build_plan_ready(p, dash),
        Outbound::Blocked(b) => crate::format::build_blocked(b, dash),
        Outbound::Complete(c) => crate::format::build_complete(c, dash),
    };
    // Multi-instance labeling: with `instanceName` configured the message gets
    // a `[name]` prefix; unset → the blocks pass through unchanged.
    let blocks = crate::format::label_blocks(blocks, cfg.instance_name.as_deref());
    let thread_ts = threads.thread_ts(mission_id);
    let posted_ts = client
        .post_message(&cfg.channel, &blocks, thread_ts.as_deref())
        .await?;
    // First post for this mission establishes the thread root.
    if thread_ts.is_none() {
        threads.set(mission_id, &posted_ts);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Inbound Socket Mode
// ---------------------------------------------------------------------------

/// Inbound loop: connect the Socket Mode websocket, handle envelopes, and
/// reconnect with backoff until `shutdown`. Each connect re-opens a fresh wss
/// URL (Slack rotates them).
pub async fn run_socket(
    cfg: SlackConfig,
    client: SlackClient,
    repo_root: PathBuf,
    threads: SharedThreads,
    host: Option<SharedHost>,
    shutdown: impl std::future::Future<Output = ()>,
) {
    // A Notify fired once when shutdown resolves; the per-connection loop selects
    // on it so a mid-connection shutdown is prompt.
    let stop = Arc::new(Notify::new());
    let stop_setter = stop.clone();
    tokio::pin!(shutdown);

    // Shared liveness handle: connect/frame/disconnect events feed it from
    // `connect_once`/`pump_connection`, and the periodic task below logs its
    // snapshot so a stalled or dead bridge is loud rather than silent.
    let health = BridgeHealth::new();
    let health_log_stop = stop.clone();
    let health_log = health.clone();
    let health_task = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(HEALTH_LOG_INTERVAL);
        ticker.tick().await; // first tick fires immediately; skip it
        loop {
            tokio::select! {
                _ = health_log_stop.notified() => return,
                _ = ticker.tick() => {
                    let snapshot = health_log.snapshot(Instant::now());
                    let secs = snapshot.secs_since_frame.unwrap_or(u64::MAX);
                    if snapshot.connected && !snapshot.stale {
                        tracing::info!(secs_since_frame = secs, "slack bridge healthy: connected, last frame {secs}s ago");
                    } else {
                        tracing::warn!(
                            connected = snapshot.connected,
                            secs_since_frame = secs,
                            "slack bridge UNHEALTHY: no frame in {secs}s"
                        );
                    }
                }
            }
        }
    });

    let mut backoff = BACKOFF_MIN;
    // Bounded dedup of processed envelope ids persists across reconnects in
    // this process. Slack may ack-timeout an envelope, reconnect, and redeliver
    // it on the next socket; re-ack those ids without re-running side effects.
    let mut seen = SeenEnvelopes::default();
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                stop_setter.notify_waiters();
                health_task.abort();
                tracing::info!("slack inbound loop shutting down");
                return;
            }
            result = connect_once(&cfg, &client, &repo_root, &threads, &host, &stop, &mut seen, &health) => {
                health.record_disconnected();
                match result {
                    // Clean close requested by shutdown: exit.
                    Ok(true) => {
                        health_task.abort();
                        return;
                    }
                    // Socket connected then closed on its own (Slack rotates the
                    // wss URL, or a network blip): a healthy session, so RESET
                    // the backoff — the drop isn't a failure to reach Slack.
                    Ok(false) => {
                        backoff = BACKOFF_MIN;
                        tracing::warn!("slack socket closed; reconnecting");
                    }
                    // Never even connected (open_connection / dial failed): grow
                    // the backoff so we don't hammer Slack while it's unreachable.
                    Err(e) => {
                        tracing::warn!(error = %e, backoff_ms = backoff.as_millis() as u64, "slack connect failed; backing off");
                    }
                }
            }
        }

        // Backoff before reconnect, but wake immediately on shutdown.
        tokio::select! {
            _ = &mut shutdown => {
                stop_setter.notify_waiters();
                health_task.abort();
                return;
            }
            _ = tokio::time::sleep(backoff) => {}
        }
        // Only failures grow the backoff; a clean reset above starts fresh.
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

/// One websocket lifetime: open a fresh wss URL (Slack rotates it, so each
/// call re-opens), then pump envelopes until the socket closes or `stop` fires.
///
/// Returns:
/// - `Ok(true)`  — `stop` ended it (clean shutdown); the caller exits.
/// - `Ok(false)` — the socket connected and then closed on its own; the caller
///   reconnects promptly (a rotated URL / blip is not a failure to reach Slack).
/// - `Err(_)`    — never connected, or a protocol error mid-stream; the caller
///   reconnects after a growing backoff.
#[allow(clippy::too_many_arguments)]
async fn connect_once(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    host: &Option<SharedHost>,
    stop: &Arc<Notify>,
    seen: &mut SeenEnvelopes,
    health: &BridgeHealth,
) -> Result<bool> {
    let url = client
        .open_connection()
        .await
        .context("opening Socket Mode connection")?;
    let (ws_stream, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .context("dialing Socket Mode websocket")?;
    health.record_connected();
    tracing::info!(
        connect_count = health.connect_count(),
        "slack Socket Mode connected"
    );
    let (mut write, mut read) = ws_stream.split();

    pump_connection(
        &mut read, &mut write, cfg, client, repo_root, threads, host, stop, seen, health,
    )
    .await
}

/// Does `text` parse to a Socket Mode `{"type":"disconnect", ...}` frame?
/// Slack sends this shortly before tearing a socket down (e.g.
/// `reason: "warning"` or `"refresh_requested"`); it carries no
/// `envelope_id`, so it must be caught before routing rather than dispatched
/// as an (ignored) action.
fn is_disconnect_frame(text: &str) -> bool {
    serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| {
            v.get("type")
                .and_then(Value::as_str)
                .map(|t| t == "disconnect")
        })
        .unwrap_or(false)
}

/// Pump one already-open connection's read/write halves until it closes,
/// `stop` fires, or the connection stalls. Extracted out of [`connect_once`]
/// so it is generic over the stream/sink and can be driven by an in-memory
/// stream in tests instead of a live websocket.
///
/// Returns the same contract as [`connect_once`]: `Ok(true)` on `stop`,
/// `Ok(false)` on a clean close, a write-send failure, a disconnect frame, or
/// an idle stall (all reconnect promptly); `Err(_)` on a stream error (grows
/// the backoff).
#[allow(clippy::too_many_arguments)]
async fn pump_connection<R, E, W>(
    read: &mut R,
    write: &mut W,
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    host: &Option<SharedHost>,
    stop: &Arc<Notify>,
    seen: &mut SeenEnvelopes,
    health: &BridgeHealth,
) -> Result<bool>
where
    R: futures_util::Stream<Item = std::result::Result<Message, E>> + Unpin,
    W: futures_util::Sink<Message> + Unpin,
    E: std::fmt::Display,
{
    loop {
        tokio::select! {
            _ = stop.notified() => {
                let _ = write.send(Message::Close(None)).await;
                return Ok(true);
            }
            timed = tokio::time::timeout(IDLE_TIMEOUT, read.next()) => {
                let msg = match timed {
                    Err(_elapsed) => {
                        tracing::warn!(
                            idle_secs = IDLE_TIMEOUT.as_secs(),
                            "slack socket idle; treating as stalled, reconnecting"
                        );
                        return Ok(false);
                    }
                    Ok(msg) => msg,
                };
                // A frame of any kind (text, ping, pong, error) proves the
                // socket is still alive; stamp it before dispatching. `None`
                // is a stream end, not a frame, so it's excluded below.
                if msg.is_some() {
                    health.record_frame();
                }
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if is_disconnect_frame(&text) {
                            tracing::warn!("slack sent a disconnect frame; reconnecting");
                            return Ok(false);
                        }
                        let Some(routed) = parse_envelope(&text, threads) else { continue };
                        // Ack FIRST (within Slack's 3 s budget), before any slow
                        // work — otherwise a claude turn would delay the ack and
                        // block ping handling on this same read loop.
                        if let Some(id) = &routed.envelope_id {
                            if write
                                .send(Message::Text(json!({ "envelope_id": id }).to_string()))
                                .await
                                .is_err()
                            {
                                return Ok(false);
                            }
                            // Redelivery of an already-processed envelope: acked
                            // above, but do not run the side effect again.
                            if !seen.insert(id) {
                                continue;
                            }
                        }
                        // Slow (claude-spawning or engine-touching) actions run
                        // on a spawned task so the read loop keeps answering
                        // pings; fast local actions run inline.
                        if is_slow_action(&routed.action) {
                            let (cfg, client, repo, threads, host) = (
                                cfg.clone(),
                                client.clone(),
                                repo_root.to_path_buf(),
                                threads.clone(),
                                host.clone(),
                            );
                            tokio::spawn(async move {
                                dispatch_action(
                                    &cfg, &client, &repo, &threads, host.as_ref(),
                                    &routed.action,
                                )
                                .await;
                            });
                        } else {
                            dispatch_action(
                                cfg, client, repo_root, threads, host.as_ref(),
                                &routed.action,
                            )
                            .await;
                        }
                    }
                    // Respond to ping with pong so Slack keeps the socket alive.
                    Some(Ok(Message::Ping(payload))) => {
                        if write.send(Message::Pong(payload)).await.is_err() {
                            return Ok(false);
                        }
                    }
                    Some(Ok(Message::Close(_))) | None => return Ok(false),
                    Some(Ok(_)) => {} // pong / binary: ignore
                    Some(Err(e)) => return Err(anyhow::anyhow!("{e}")),
                }
            }
        }
    }
}

/// Parse a Socket Mode text frame into a routed action, resolving thread →
/// mission via the shared map (needed so a thread reply routes to Guidance).
/// `None` on an unparseable frame.
fn parse_envelope(text: &str, threads: &SharedThreads) -> Option<crate::inbound::Routed> {
    match serde_json::from_str::<Value>(text) {
        Ok(envelope) => Some(route(&envelope, threads)),
        Err(e) => {
            tracing::warn!(error = %e, "unparseable Socket Mode frame; ignoring");
            None
        }
    }
}

/// Actions that may spawn a claude session (a multi-minute turn) or touch a
/// live engine (approve commits files under the engine mutex) and so must run
/// off the socket read loop, after the ack. Guidance is here because a reply
/// on a PLANNING mission's thread runs a hosted planning turn; on a running
/// mission it degrades to a fast control-inbox write, and spawning for that is
/// harmless.
fn is_slow_action(action: &Action) -> bool {
    matches!(
        action,
        Action::NewMission { .. }
            | Action::RequestPlan { .. }
            | Action::Guidance { .. }
            | Action::Approve { .. }
            | Action::ApproveStart { .. }
            | Action::ApproveMission { .. }
            | Action::QueueTicket { .. }
            | Action::Draft { .. }
            | Action::WorkRun { .. }
            | Action::CreateTicket { .. }
            | Action::Merge { .. }
    )
}

/// A bounded set of recently-seen envelope ids (FIFO eviction). Human-driven
/// volume is low; the cap only guards against unbounded growth over a
/// long-lived connection.
#[derive(Default)]
struct SeenEnvelopes {
    set: std::collections::HashSet<String>,
    order: std::collections::VecDeque<String>,
}

impl SeenEnvelopes {
    const CAP: usize = 512;
    /// Record `id`; returns true if it was NOT seen before.
    fn insert(&mut self, id: &str) -> bool {
        if !self.set.insert(id.to_string()) {
            return false;
        }
        self.order.push_back(id.to_string());
        if self.order.len() > Self::CAP {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
        true
    }
}

/// Parse one text frame as a Socket Mode envelope, route it, apply the action,
/// and return the ack frame to send (the `{"envelope_id":…}` Slack requires
/// within 3 s). Returns `None` when the frame carries no envelope id (e.g.
/// `hello`), so nothing is sent.
///
/// The action is applied inline before the ack is returned: every side effect
/// (queue insert, control enqueue, ticket scaffold) is a local filesystem write
/// with no network round-trip, so the whole handler stays well inside the 3 s
/// ack budget. A failed action is logged but still acked — retrying the same
/// envelope wouldn't fix a local write error, and leaving it un-acked would make
/// Slack redeliver it indefinitely.
///
/// Test-only: production goes through [`connect_once`], which acks first and
/// dispatches slow (claude-spawning) actions off the read loop. This inline
/// variant keeps the existing ack + side-effect tests concise.
#[cfg(test)]
async fn handle_envelope(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    text: &str,
) -> Option<String> {
    let envelope: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "unparseable Socket Mode frame; ignoring");
            return None;
        }
    };
    let routed = route(&envelope, threads);
    dispatch_action(cfg, client, repo_root, threads, None, &routed.action).await;
    // Ack whatever carried an envelope_id, even Ignore, so Slack stops retrying.
    routed
        .envelope_id
        .map(|id| json!({ "envelope_id": id }).to_string())
}

/// Ephemeral reply to the slash `response_url`, best-effort (a failed reply
/// must never wedge the ack). No-op when the action carried no `response_url`.
/// Labels the reply with the configured instance name
/// ([`crate::format::label_blocks`]) so a user driving several Kranz instances
/// can tell WHICH one answered; with no `instanceName` configured the blocks
/// pass through unchanged.
async fn reply_ephemeral(
    cfg: &SlackConfig,
    client: &SlackClient,
    response_url: Option<&str>,
    blocks: &[Value],
) {
    if let Some(url) = response_url {
        let blocks = crate::format::label_blocks(blocks.to_vec(), cfg.instance_name.as_deref());
        if let Err(e) = client.post_response(url, &blocks, true).await {
            tracing::warn!(error = %e, "failed to post ephemeral Slack reply");
        }
    }
}

/// User-only reply for actions that know their channel: over the
/// `response_url` when there is one (slash commands, buttons), else
/// `chat.postEphemeral` (the modal path — a `view_submission` carries no
/// response_url). Best-effort, instance-labeled either way.
async fn user_reply(
    cfg: &SlackConfig,
    client: &SlackClient,
    response_url: Option<&str>,
    channel: &str,
    user_id: Option<&str>,
    blocks: &[Value],
) {
    if response_url.is_some() {
        reply_ephemeral(cfg, client, response_url, blocks).await;
        return;
    }
    let Some(user) = user_id else { return };
    let blocks = crate::format::label_blocks(blocks.to_vec(), cfg.instance_name.as_deref());
    if let Err(e) = client.post_ephemeral(channel, user, &blocks).await {
        tracing::warn!(error = %e, "failed to post ephemeral Slack reply (postEphemeral)");
    }
}

/// A one-line ephemeral "not authorized" reply for a spend-gated action from an
/// unlisted user (docs/slack-management.md must-have #1). `pub` so tests
/// (including external ones, e.g. `tests/tickets.rs`) can assert a refusal is
/// byte-for-byte this standard message, not just "some" error.
pub fn not_authorized_blocks() -> Vec<Value> {
    vec![json!({
        "type": "section",
        "text": {
            "type": "mrkdwn",
            "text": ":no_entry: You're not authorized to spend on missions here. \
                     Ask an admin to add you to `slack.allowUsers` in `~/.kranz/config.json`."
        }
    })]
}

/// Route one inbound action to its handler. The async actions (help, status,
/// new-mission, request-plan, approve-from-slash) reply over the slash
/// `response_url`, so they can't go through the sync [`apply_action`] path;
/// the button/thread actions (approve button, guidance, ticket) are pure local
/// filesystem writes and go through [`apply_action`].
///
/// ## The hosted lifecycle (M2.9 planning-conversation slice)
/// With a [`SharedHost`] wired in (`kranz serve --slack` passes an adapter
/// over the SAME `MissionHost` registry the web UI uses), the full lifecycle
/// runs from Slack:
/// - **Status** — folds the mission's event log and posts a status block.
///   Read-only, so no allowlist gate. Unit-tested end-to-end.
/// - **NewMission** (`/kranz new <goal>`) — allowlist-gated; creates THROUGH
///   the host so the planning engine stays live across turns.
/// - **Guidance** on a PLANNING mission's thread — allowlist-gated hosted
///   planning turn, acked in-thread first (the turn takes minutes); on a
///   running mission it stays the control-inbox guidance write.
/// - **RequestPlan** (`/kranz plan <id>`) — allowlist-gated; immediate
///   ephemeral ack, then `host.request_plan`. Ready → a plan-review block
///   (goal, milestones, estimate, approve buttons) posted to the mission
///   thread; the HOST parked the reviewed plan (one cache, every surface). NotReady →
///   the orchestrator's prose posted threaded.
/// - **Approve / ApproveStart / ApproveMission** — allowlist-gated
///   [`approve_flow`]: commit the pending plan through the host, then queue
///   (`Approve`/slash) or start execution through the host (`ApproveStart`).
///   State-aware without a pending plan (see [`approve_flow`]).
/// - **QueueTicket** (`/kranz queue <slug>`, D-A) — the ticket-queueing verb;
///   allowlist-gated identically to the ticket-slug path of `/kranz approve`,
///   but resolves ONLY through [`run_approve_ticket_command`] — it never
///   falls back to [`approve_flow`], so it can never trigger plan approval.
///
/// Without a host every engine-needing surface degrades to an honest
/// ephemeral refusal pointing at the CLI ([`no_host_blocks`]).
///
/// ## M2.9 slices 2 & 3 additions
/// - **Config** (`/kranz config [<id>] <role> <model> [effort]`) — spend-adjacent,
///   so allowlist-gated like `new`; on authorization enqueues a
///   `config-change` control command with the camelCase patch ([`config_change`]).
///   A pure local write, so it stays inline (fast ack).
/// - **AppHome** (`app_home_opened`) — read-only: folds the repo and publishes
///   the Home view via `views.publish` ([`build_home_view`]). One Web API call,
///   no allowlist gate; a publish failure is logged, never surfaced.
///
/// ## M2.9 slice — steering (pause / resume / work)
/// - **Pause** / **Resume** (`/kranz pause|resume [<id>]`) — STEERING, not
///   spend, but they disrupt a running mission, so they are gated on the
///   allowlist exactly like `config`. On authorization the bridge enqueues
///   `ControlCommand::Pause` / `Resume` on the target mission's control inbox
///   (the same mechanism `kranz pause`/`kranz resume` use), resolved via
///   [`resolve_active_config_target`] so a terminal/ambiguous target is an honest
///   ephemeral error that enqueues NOTHING. A pure local write ([`steer`]), so
///   it stays inline (fast ack).
/// - **Work** (`/kranz work`) — REPORT-ONLY: the bridge must never spawn a
///   mission on the socket read loop, so it reports the queue state
///   (`queue::list` + `is_repo_busy`, [`build_work_reply`]) and points at the
///   `kranz work` CLI / dispatcher for actually draining it. Read-only, no gate.
/// - **Work run** (`/kranz work run`) — SPEND action, gated EXACTLY like
///   `draft` ([`gate_work_run_command`] / [`run_work_run`]). On authorization
///   it triggers the drain THROUGH the host ([`crate::host::PlanningHost::drain`]) —
///   the seam that spawns the background drain on the serve process — never
///   by resuming/running a mission on the socket read loop.
///
/// ## Ack budget (docs must-have #3)
/// `connect_once` acks every envelope FIRST and runs the slow actions
/// ([`is_slow_action`]: claude-spawning or engine-touching) on spawned tasks,
/// deduped by envelope id ([`SeenEnvelopes`]) so a Slack redelivery can't
/// double-create or double-approve. Only pure-local actions run inline on the
/// read loop.
/// The outcome of the SYNCHRONOUS `gate_draft_command` phase — no
/// `PlanningHost::draft` call has happened by the time any of these variants
/// is returned. `Ready` carries the immediate hourglass ack (posted first,
/// mirroring [`Action::NewMission`]) that the caller must post BEFORE
/// awaiting [`run_draft`], so the invoker sees the ack immediately rather
/// than only once the multi-minute draft turn completes.
pub enum DraftGate {
    Unauthorized,
    NoHost(Vec<Value>),
    InvalidSlug(Vec<Value>),
    Ready(Vec<Value>),
}

/// `/kranz draft <slug>` gate/ack phase — SPEND action, gated EXACTLY like
/// `/kranz new` (same gate, same standard refusal). Runs
/// `cfg.is_authorized`, [`kranz_engine::ticket::Ticket::ensure_valid_slug`],
/// and the host-presence check, and builds the hourglass ack for the `Ready`
/// case. Makes NO `PlanningHost::draft` call — that is the caller's job via
/// [`run_draft`], AFTER posting the `Ready` ack — so this phase stays
/// synchronous and unit-testable without a live `SlackClient`.
pub fn gate_draft_command(
    cfg: &SlackConfig,
    host: Option<&SharedHost>,
    slug: &str,
    user_id: Option<&str>,
) -> DraftGate {
    if !cfg.is_authorized(user_id) {
        return DraftGate::Unauthorized;
    }
    if let Err(e) = kranz_engine::ticket::Ticket::ensure_valid_slug(slug) {
        return DraftGate::InvalidSlug(error_blocks(&format!("Couldn't draft `{slug}`: {e}")));
    }
    if host.is_none() {
        return DraftGate::NoHost(error_blocks(&format!(
            "This bridge has no hosted planning engine (it was started without \
             `kranz serve`). Use `kranz ticket draft {slug}` in a terminal, or the \
             web UI via `kranz serve --open`."
        )));
    }
    // Ack IMMEDIATELY: the draft turn (create + seed + drive to a terminal
    // outcome) takes minutes, same reasoning as NewMission/RequestPlan. The
    // caller must post this BEFORE calling `run_draft`.
    DraftGate::Ready(error_blocks(&format!(
        ":hourglass_flowing_sand: Drafting `{slug}` — the seeding planning turn \
         usually takes a minute or two; the result will post here."
    )))
}

/// The async run phase of `/kranz draft <slug>`, called ONLY after the
/// caller has posted the `DraftGate::Ready` ack. Drives [`PlanningHost::draft`]
/// to a terminal [`DraftOutcome`] and maps it to the terminal result blocks,
/// posting the orchestrator's clarifying questions back to the invoker on
/// `NeedsContext`.
pub async fn run_draft(host: &SharedHost, slug: &str) -> Vec<Value> {
    match host.draft(slug).await {
        Ok(DraftOutcome::ParkedForReview {
            mission_id,
            mission_branch,
        }) => error_blocks(&format!(
            ":white_check_mark: Draft ready for review — mission `{mission_id}`, \
             branch `{mission_branch}`. Ticket `{slug}` is now in review."
        )),
        Ok(DraftOutcome::PlanAsProse { mission_id }) => error_blocks(&format!(
            ":warning: Draft for `{slug}` NOT queued — mission `{mission_id}`'s orchestrator \
             produced a plan but emitted it as prose instead of through the plan channel, so \
             nothing was queued. Run `/kranz draft {slug}` again."
        )),
        Ok(DraftOutcome::Enqueued { mission_id }) => error_blocks(&format!(
            ":white_check_mark: Draft approved and queued — mission `{mission_id}`. \
             Ticket `{slug}` is now queued."
        )),
        Ok(DraftOutcome::NeedsContext {
            mission_id,
            questions,
        }) => {
            let mut text = format!(
                ":question: Mission `{mission_id}` needs more context before drafting \
                 `{slug}` can continue:\n"
            );
            for q in &questions {
                text.push_str(&format!("• {q}\n"));
            }
            error_blocks(text.trim_end())
        }
        Err(e) => error_blocks(&format!("Couldn't draft `{slug}`: {e}")),
    }
}

/// The outcome of the SYNCHRONOUS `gate_work_run_command` phase — mirrors
/// [`DraftGate`]. No [`crate::host::PlanningHost::drain`] call has happened
/// by the time any of these variants is returned; `Ready` carries the
/// immediate ack the caller must post BEFORE awaiting [`run_work_run`].
pub enum WorkRunGate {
    Unauthorized,
    NoHost(Vec<Value>),
    Ready(Vec<Value>),
}

/// `/kranz work run` gate/ack phase — SPEND action, gated EXACTLY like
/// `/kranz new` / `/kranz draft` (same gate, same standard refusal). Makes NO
/// `PlanningHost::drain` call — that is the caller's job via
/// [`run_work_run`], AFTER posting the `Ready` ack — so this phase stays
/// synchronous and unit-testable without a live `SlackClient`.
pub fn gate_work_run_command(
    cfg: &SlackConfig,
    host: Option<&SharedHost>,
    user_id: Option<&str>,
) -> WorkRunGate {
    if !cfg.is_authorized(user_id) {
        return WorkRunGate::Unauthorized;
    }
    if host.is_none() {
        return WorkRunGate::NoHost(error_blocks(
            "This bridge has no hosted planning engine (it was started without \
             `kranz serve`). Use `kranz work` in a terminal to drain the queue.",
        ));
    }
    WorkRunGate::Ready(error_blocks(
        ":hourglass_flowing_sand: Running the queue — draining now; progress posts per mission.",
    ))
}

/// The async run phase of `/kranz work run`, called ONLY after the caller has
/// posted the `WorkRunGate::Ready` ack. Drives [`crate::host::PlanningHost::drain`]
/// — the bridge itself never resumes/runs a mission on the socket read loop;
/// the host spawns the drain as a background task on the serve process.
pub async fn run_work_run(host: &SharedHost) -> Vec<Value> {
    match host.drain().await {
        Ok(()) => error_blocks(":white_check_mark: Queue drain triggered."),
        Err(e) => error_blocks(&format!("Couldn't drain the queue: {e}")),
    }
}

/// The outcome of the SYNCHRONOUS `gate_merge_command` phase — mirrors
/// [`WorkRunGate`]. No [`crate::host::PlanningHost::merge`] call has happened
/// by the time any of these variants is returned; `Ready` carries the
/// immediate ack the caller must post BEFORE awaiting [`run_merge`].
pub enum MergeGate {
    Unauthorized,
    NoHost(Vec<Value>),
    Ready(Vec<Value>),
}

/// `/kranz merge <slug|id>` / Delivered-card Merge button gate/ack phase —
/// spend-adjacent, gated EXACTLY like [`gate_work_run_command`]. Makes NO
/// `PlanningHost::merge` call — that is the caller's job via [`run_merge`],
/// AFTER posting the `Ready` ack — so this phase stays synchronous and
/// unit-testable without a live `SlackClient`.
pub fn gate_merge_command(
    cfg: &SlackConfig,
    host: Option<&SharedHost>,
    user_id: Option<&str>,
) -> MergeGate {
    if !cfg.is_authorized(user_id) {
        return MergeGate::Unauthorized;
    }
    if host.is_none() {
        return MergeGate::NoHost(error_blocks(
            "This bridge has no hosted planning engine (it was started without \
             `kranz serve`). Use `kranz merge <id>` in a terminal.",
        ));
    }
    MergeGate::Ready(error_blocks(
        ":hourglass_flowing_sand: Merging — running the gate suite now; the result posts here.",
    ))
}

/// The async run phase of `/kranz merge <slug|id>`, called ONLY after the
/// caller has posted the `MergeGate::Ready` ack. Drives
/// [`crate::host::PlanningHost::merge`] and forwards its outcome — merged
/// commit, or the refusal (dirty tree / failing gate with verbatim output /
/// conflict) — unchanged.
pub async fn run_merge(host: &SharedHost, mission_id: &str) -> Vec<Value> {
    match host.merge(mission_id).await {
        Ok(value) => {
            let commit = value.get("commit").and_then(Value::as_str).unwrap_or("?");
            let mut message =
                format!(":white_check_mark: Merged `{mission_id}` — commit `{commit}`.");
            if let Some(warning) = value
                .get("staleBase")
                .and_then(|v| v.get("message"))
                .and_then(Value::as_str)
            {
                message.push_str(&format!("\n:warning: {warning}"));
            }
            error_blocks(&message)
        }
        Err(e) => error_blocks(&format!("Couldn't merge `{mission_id}`: {e}")),
    }
}

/// Does `arg` name an on-disk backlog ticket? `/kranz approve <arg>` uses this
/// to decide whether `arg` is a ticket slug (resolve through
/// [`run_approve_ticket_command`]) or a mission id (the original
/// pending-plan `approve_flow`). A syntactically invalid slug never matches
/// (no filesystem access for path-traversal attempts).
fn is_ticket_slug(repo_root: &Path, arg: &str) -> bool {
    kranz_engine::ticket::Ticket::valid_slug(arg)
        && kranz_engine::ticket::Ticket::tickets_dir(repo_root)
            .join(format!("{arg}.md"))
            .is_file()
}

/// The outcome of `run_approve_ticket_command`: whether the invoker was
/// authorized, and the reply blocks to post (`None` only when unauthorized,
/// mirroring [`DraftGate`]).
pub struct ApproveTicketInvocation {
    pub authorized: bool,
    pub result: Option<Vec<Value>>,
}

/// `/kranz approve <slug>` — the slug-resolving twin of `/kranz approve
/// <mission-id>`, gated EXACTLY like it (same allowlist, same standard
/// refusal). Holds the gate + host-call logic so it is unit-testable without
/// a live `SlackClient`. Runs [`crate::host::PlanningHost::approve_ticket`] —
/// the SAME `kranz_engine::deps::approve_ticket` gate the REST/CLI approve
/// path runs — and forwards a blocked-by / not-REVIEW / cycle refusal
/// VERBATIM (never paraphrased).
pub async fn run_approve_ticket_command(
    cfg: &SlackConfig,
    host: Option<&SharedHost>,
    slug: &str,
    user_id: Option<&str>,
) -> ApproveTicketInvocation {
    if !cfg.is_authorized(user_id) {
        return ApproveTicketInvocation {
            authorized: false,
            result: None,
        };
    }
    let Some(host) = host else {
        return ApproveTicketInvocation {
            authorized: true,
            result: Some(error_blocks(&format!(
                "This bridge has no hosted planning engine (it was started without \
                 `kranz serve`). Use `kranz ticket approve {slug}` in a terminal, or the \
                 web UI via `kranz serve --open`."
            ))),
        };
    };
    let result = match host.approve_ticket(slug).await {
        Ok(mission_id) => error_blocks(&format!(
            ":white_check_mark: Approved and queued — ticket `{slug}` \u{2192} mission \
             `{mission_id}`. The `kranz work` dispatcher runs it next."
        )),
        // VERBATIM: `e` is the engine's own refusal message (blocked-by,
        // not-REVIEW, or a blocked-by cycle) — forwarded unchanged, never
        // wrapped in extra prose that would obscure it.
        Err(e) => error_blocks(&e.to_string()),
    };
    ApproveTicketInvocation {
        authorized: true,
        result: Some(result),
    }
}

async fn dispatch_action(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    host: Option<&SharedHost>,
    action: &Action,
) {
    match action {
        Action::Help { response_url } => {
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &crate::format::build_help(),
            )
            .await;
        }

        Action::Status {
            mission_id,
            response_url,
        } => {
            if mission_id.is_none() {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &build_pipeline_status_reply(repo_root),
                )
                .await;
            } else {
                match build_status_reply(repo_root, mission_id.as_deref()) {
                    Ok(blocks) => {
                        reply_ephemeral(cfg, client, response_url.as_deref(), &blocks).await
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to build Slack status reply");
                        reply_ephemeral(
                            cfg,
                            client,
                            response_url.as_deref(),
                            &error_blocks(&format!("Couldn't read that mission: {e}")),
                        )
                        .await;
                    }
                }
            }
        }

        Action::Todo { response_url } => {
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &build_todo_reply(repo_root, cfg.dashboard_url.as_deref()),
            )
            .await;
        }

        // Read-only backlog verbs (no `user_id`, so structurally not
        // allowlist-gated — same shape as Status).
        Action::TicketList { response_url } => {
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &build_ticket_list_reply(repo_root),
            )
            .await;
        }

        Action::TicketShow { slug, response_url } => {
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &build_ticket_show_reply(repo_root, slug),
            )
            .await;
        }

        Action::NewMission {
            goal,
            user_id,
            response_url,
            channel,
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                user_reply(
                    cfg,
                    client,
                    response_url.as_deref(),
                    channel,
                    user_id.as_deref(),
                    &not_authorized_blocks(),
                )
                .await;
                return;
            }
            // Ack IMMEDIATELY: create + the seeding planning turn take minutes,
            // and a silently-working command reads as a dead one.
            user_reply(
                cfg,
                client,
                response_url.as_deref(),
                channel,
                user_id.as_deref(),
                &error_blocks(
                    ":hourglass_flowing_sand: Creating the mission — the seeding planning \
                     turn usually takes a minute or two; the planning thread will appear \
                     in the channel.",
                ),
            )
            .await;
            match new_mission(cfg, client, repo_root, threads, host, goal, channel).await {
                Ok(blocks) => {
                    user_reply(
                        cfg,
                        client,
                        response_url.as_deref(),
                        channel,
                        user_id.as_deref(),
                        &blocks,
                    )
                    .await;
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to create mission from Slack");
                    user_reply(
                        cfg,
                        client,
                        response_url.as_deref(),
                        channel,
                        user_id.as_deref(),
                        &error_blocks(&format!("Couldn't create the mission: {e}")),
                    )
                    .await;
                }
            }
        }

        // Bare `/kranz config`: open the role/model/effort picker modal.
        // Inline for the same trigger_id-expiry reason as the goal modal.
        Action::ConfigModal {
            trigger_id,
            user_id,
            response_url,
            channel,
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks(),
                )
                .await;
                return;
            }
            let view = crate::format::build_config_modal(channel);
            if let Err(e) = client.open_view(trigger_id, &view).await {
                tracing::warn!(error = %e, "failed to open config modal");
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &error_blocks(&format!(
                        "Couldn't open the config form: {e}. One-line fallback: \
                         `/kranz config [<id>] <role> <model> [effort]`."
                    )),
                )
                .await;
            }
        }

        // Bare `/kranz new`: open the multiline goal modal. MUST run inline —
        // the trigger_id expires ~3 s after the slash — and it's one Web API
        // call, well inside the ack budget.
        Action::NewMissionModal {
            trigger_id,
            user_id,
            response_url,
            channel,
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks(),
                )
                .await;
                return;
            }
            let view = crate::format::build_new_mission_modal(channel);
            if let Err(e) = client.open_view(trigger_id, &view).await {
                tracing::warn!(error = %e, "failed to open new-mission modal");
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &error_blocks(&format!(
                        "Couldn't open the new-mission form: {e}. One-line fallback: \
                         `/kranz new <goal>`."
                    )),
                )
                .await;
            }
        }

        // `/kranz ticket new <slug> <title...>`: open the multiline
        // goal/context modal. MUST run inline — the trigger_id expires ~3s
        // after the slash — mirroring [`Action::NewMissionModal`].
        Action::NewTicketModal {
            trigger_id,
            slug,
            title,
            user_id,
            response_url,
            channel,
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks(),
                )
                .await;
                return;
            }
            let view = crate::format::build_new_ticket_modal(slug, title, channel);
            if let Err(e) = client.open_view(trigger_id, &view).await {
                tracing::warn!(error = %e, "failed to open new-ticket modal");
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &error_blocks(&format!(
                        "Couldn't open the new-ticket form: {e}. One-line fallback: \
                         `/kranz ticket <title>`."
                    )),
                )
                .await;
            }
        }

        // The new-ticket modal's `view_submission`: scaffold the ticket
        // through the same primitive `POST /api/tickets` uses
        // (`Ticket::scaffold`). No `response_url` (a modal submission has
        // none), so the confirmation/error posts straight into `channel`.
        Action::CreateTicket {
            slug,
            title,
            goal,
            context,
            channel,
        } => {
            let goal = (!goal.trim().is_empty()).then_some(goal.as_str());
            let context = (!context.trim().is_empty()).then_some(context.as_str());
            match create_ticket(repo_root, slug, title, goal, context) {
                Ok(()) => {
                    let blocks = vec![json!({
                        "type": "section",
                        "text": {
                            "type": "mrkdwn",
                            "text": format!(
                                ":ticket: Created ticket `{}` — {}",
                                crate::format::escape_mrkdwn(slug),
                                crate::format::escape_mrkdwn(title)
                            )
                        }
                    })];
                    if let Err(e) = client.post_message(channel, &blocks, None).await {
                        tracing::warn!(error = %e, "failed to post ticket-created confirmation");
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to create ticket from Slack modal");
                    if let Err(e) = client
                        .post_message(
                            channel,
                            &error_blocks(&format!("Couldn't create ticket `{slug}`: {e}")),
                            None,
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "failed to post ticket-creation error");
                    }
                }
            }
        }

        Action::RequestPlan {
            mission_id,
            user_id,
            response_url,
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks(),
                )
                .await;
                return;
            }
            let Some(host) = host else {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &no_host_blocks(mission_id),
                )
                .await;
                return;
            };
            // Ack IMMEDIATELY: the request-plan turn takes minutes, and a
            // silently-working command is exactly the confusion this surface
            // is meant to avoid.
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &error_blocks(&format!(
                    ":hourglass_flowing_sand: Requesting the plan for `{mission_id}` — the \
                     orchestrator turn usually takes a minute or two; the plan will post \
                     in the mission thread."
                )),
            )
            .await;
            match host.request_plan(mission_id).await {
                Ok(PlanOutcome::Ready { plan, estimate }) => {
                    let review = crate::format::build_plan_review(&crate::format::PlanReview {
                        mission_id: mission_id.clone(),
                        goal: plan.goal.clone(),
                        milestone_titles: plan.milestones.iter().map(|m| m.title.clone()).collect(),
                        assertion_count: plan.validation_contract.len(),
                        estimate,
                    });
                    // No caching here: the host parked the reviewed plan
                    // when request_plan returned Ready, so the buttons (and
                    // any other surface's approve) consume it host-side.
                    if let Err(e) =
                        post_to_mission_thread(cfg, client, threads, mission_id, review).await
                    {
                        tracing::warn!(mission = %mission_id, error = %e, "failed to post plan review");
                        reply_ephemeral(
                            cfg,
                            client,
                            response_url.as_deref(),
                            &error_blocks(&format!(
                                "The plan is ready but posting it failed: {e}. \
                                 Run `/kranz plan {mission_id}` again."
                            )),
                        )
                        .await;
                    }
                }
                Ok(PlanOutcome::NotReady(prose)) => {
                    // NotReady by definition: never claim a plan was spotted.
                    let blocks = crate::format::build_planning_reply(mission_id, &prose, false);
                    if let Err(e) =
                        post_to_mission_thread(cfg, client, threads, mission_id, blocks).await
                    {
                        tracing::warn!(mission = %mission_id, error = %e, "failed to post not-ready reply");
                    }
                }
                Err(e) => {
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url.as_deref(),
                        &error_blocks(&format!("Couldn't request the plan: {e}")),
                    )
                    .await;
                }
            }
        }

        Action::ApproveMission {
            mission_id,
            user_id,
            response_url,
        } => {
            // `/kranz approve <arg>` accepts EITHER a mission id or a ticket
            // slug: an arg naming an on-disk backlog ticket resolves through
            // the ticket-approve gate (kranz_engine::deps::approve_ticket);
            // anything else keeps the original pending-plan approve flow.
            if is_ticket_slug(repo_root, mission_id) {
                let invocation =
                    run_approve_ticket_command(cfg, host, mission_id, user_id.as_deref()).await;
                if !invocation.authorized {
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url.as_deref(),
                        &not_authorized_blocks(),
                    )
                    .await;
                } else if let Some(result) = &invocation.result {
                    reply_ephemeral(cfg, client, response_url.as_deref(), result).await;
                }
            } else {
                approve_flow(
                    cfg,
                    client,
                    repo_root,
                    threads,
                    host,
                    mission_id,
                    user_id.as_deref(),
                    response_url.as_deref(),
                    false,
                    false,
                )
                .await;
            }
        }

        // `/kranz queue <slug>` — the D-A ticket-queueing verb. MUST NEVER
        // fall back to `approve_flow` (plan approval stays "approve"
        // wholesale): a non-ticket arg is refused with a pointer at
        // `/kranz approve <id>` instead of being interpreted as a mission id.
        Action::QueueTicket {
            slug,
            user_id,
            response_url,
        } => {
            if !is_ticket_slug(repo_root, slug) {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &error_blocks(&format!(
                        "`{slug}` isn't a backlog ticket — `queue` is for tickets. \
                         To approve a mission plan, use `/kranz approve <mission-id>`."
                    )),
                )
                .await;
                return;
            }
            let invocation = run_approve_ticket_command(cfg, host, slug, user_id.as_deref()).await;
            if !invocation.authorized {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks(),
                )
                .await;
            } else if let Some(result) = &invocation.result {
                reply_ephemeral(cfg, client, response_url.as_deref(), result).await;
            }
        }

        // `/kranz draft <slug>` — SPEND action, gated EXACTLY like `new`
        // ([`gate_draft_command`] holds the gate + ack logic so it's
        // unit-testable without a live SlackClient). The hourglass ack MUST
        // post before the slow `run_draft` await, mirroring
        // NewMission/RequestPlan — never build both and post them back to
        // back after the draft finishes.
        Action::Draft {
            slug,
            user_id,
            response_url,
        } => match gate_draft_command(cfg, host, slug, user_id.as_deref()) {
            DraftGate::Unauthorized => {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks(),
                )
                .await;
            }
            DraftGate::NoHost(blocks) | DraftGate::InvalidSlug(blocks) => {
                reply_ephemeral(cfg, client, response_url.as_deref(), &blocks).await;
            }
            DraftGate::Ready(ack) => {
                reply_ephemeral(cfg, client, response_url.as_deref(), &ack).await;
                let host = host.expect("DraftGate::Ready only returned with a host present");
                let result = run_draft(host, slug).await;
                reply_ephemeral(cfg, client, response_url.as_deref(), &result).await;
            }
        },

        // Per-role config change. SPEND-ADJACENT (it re-shapes future turns'
        // spend), so it is gated on the allowlist exactly like `/kranz new`.
        Action::Config {
            mission_id,
            role,
            model,
            effort,
            user_id,
            response_url,
            channel,
        } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                user_reply(
                    cfg,
                    client,
                    response_url.as_deref(),
                    channel.as_deref().unwrap_or(&cfg.channel),
                    user_id.as_deref(),
                    &not_authorized_blocks(),
                )
                .await;
                return;
            }
            match config_change(
                repo_root,
                mission_id.as_deref(),
                role,
                model,
                effort.as_deref(),
            ) {
                Ok(applied_to) => {
                    let effort_note = effort
                        .as_deref()
                        .map(|e| format!(", effort `{e}`"))
                        .unwrap_or_default();
                    user_reply(
                        cfg,
                        client,
                        response_url.as_deref(),
                        channel.as_deref().unwrap_or(&cfg.channel),
                        user_id.as_deref(),
                        &error_blocks(&format!(
                            ":gear: Set `{role}` model `{model}`{effort_note} on `{applied_to}`."
                        )),
                    )
                    .await
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to apply config change from Slack");
                    user_reply(
                        cfg,
                        client,
                        response_url.as_deref(),
                        channel.as_deref().unwrap_or(&cfg.channel),
                        user_id.as_deref(),
                        &error_blocks(&format!("Couldn't change config: {e}")),
                    )
                    .await
                }
            }
        }

        // Pause / resume: STEERING (not spend), but they disrupt a running
        // mission, so they are gated on the allowlist exactly like `config`. On
        // authorization, enqueue the Pause/Resume control command on the target
        // mission (resolved via the same active-mission resolver config uses, so
        // a terminal/ambiguous target is an honest error that enqueues NOTHING).
        // A pure local write, so it stays inline (fast ack).
        Action::Pause {
            mission_id,
            user_id,
            response_url,
        } => {
            steer(
                cfg,
                client,
                repo_root,
                mission_id.as_deref(),
                user_id.as_deref(),
                response_url.as_deref(),
                ControlCommand::Pause,
                "paused",
            )
            .await;
        }
        Action::Resume {
            mission_id,
            user_id,
            response_url,
        } => {
            steer(
                cfg,
                client,
                repo_root,
                mission_id.as_deref(),
                user_id.as_deref(),
                response_url.as_deref(),
                ControlCommand::Resume,
                "resumed",
            )
            .await;
        }

        // Queue report. READ-ONLY and REPORT-ONLY: the bridge never drains the
        // queue on the socket loop (that would spawn `claude`); it reads the
        // queue state and points at the `kranz work` dispatcher.
        Action::Work { response_url } => {
            reply_ephemeral(
                cfg,
                client,
                response_url.as_deref(),
                &build_work_reply(repo_root),
            )
            .await;
        }

        // `/kranz work run` — SPEND action, gated EXACTLY like `draft`
        // ([`gate_work_run_command`] holds the gate + ack logic so it's
        // unit-testable without a live SlackClient). The ack MUST post before
        // the slow `run_work_run` await, mirroring Draft — never build both
        // and post them back to back after the drain call returns.
        Action::WorkRun {
            user_id,
            response_url,
        } => match gate_work_run_command(cfg, host, user_id.as_deref()) {
            WorkRunGate::Unauthorized => {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks(),
                )
                .await;
            }
            WorkRunGate::NoHost(blocks) => {
                reply_ephemeral(cfg, client, response_url.as_deref(), &blocks).await;
            }
            WorkRunGate::Ready(ack) => {
                reply_ephemeral(cfg, client, response_url.as_deref(), &ack).await;
                let host = host.expect("WorkRunGate::Ready only returned with a host present");
                let result = run_work_run(host).await;
                reply_ephemeral(cfg, client, response_url.as_deref(), &result).await;
            }
        },

        // `/kranz merge <slug|id>` / Delivered-card Merge button — gated
        // EXACTLY like `work run` ([`gate_merge_command`] holds the gate + ack
        // logic so it's unit-testable without a live SlackClient). The ack
        // MUST post before the slow `run_merge` await, mirroring WorkRun.
        Action::Merge {
            mission_id,
            user_id,
            response_url,
        } => match gate_merge_command(cfg, host, user_id.as_deref()) {
            MergeGate::Unauthorized => {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url.as_deref(),
                    &not_authorized_blocks(),
                )
                .await;
            }
            MergeGate::NoHost(blocks) => {
                reply_ephemeral(cfg, client, response_url.as_deref(), &blocks).await;
            }
            MergeGate::Ready(ack) => {
                reply_ephemeral(cfg, client, response_url.as_deref(), &ack).await;
                let host = host.expect("MergeGate::Ready only returned with a host present");
                let result = run_merge(host, mission_id).await;
                reply_ephemeral(cfg, client, response_url.as_deref(), &result).await;
            }
        },

        // App Home tab: fold the repo read-only and publish this user's home
        // view. Read-only (no allowlist gate); a publish failure is logged, not
        // surfaced (there's no response_url — the user just opened a tab).
        Action::AppHome { user_id } => {
            let view = crate::format::label_home_view(
                build_home_view(repo_root, cfg.dashboard_url.as_deref()),
                cfg.instance_name.as_deref(),
            );
            if let Err(e) = client.publish_home_view(user_id, &view).await {
                tracing::warn!(user = %user_id, error = %e, "failed to publish App Home view");
            }
        }

        // The approve BUTTONS are the spend twins of `/kranz approve` and must
        // be gated identically — otherwise an unlisted user clicking one queues
        // (or starts) a paid mission, bypassing the allowlist that the slash
        // command enforces.
        Action::Approve {
            mission_id,
            user_id,
            response_url,
        } => {
            approve_flow(
                cfg,
                client,
                repo_root,
                threads,
                host,
                mission_id,
                user_id.as_deref(),
                response_url.as_deref(),
                false,
                true,
            )
            .await;
        }
        Action::ApproveStart {
            mission_id,
            user_id,
            response_url,
        } => {
            approve_flow(
                cfg,
                client,
                repo_root,
                threads,
                host,
                mission_id,
                user_id.as_deref(),
                response_url.as_deref(),
                true,
                true,
            )
            .await;
        }

        // A threaded reply: on a PLANNING mission this is a hosted planning
        // turn (spend → allowlist-gated, acked in-thread because a message
        // event has no response_url); on anything else it stays the running-
        // mission guidance write it has always been.
        Action::Guidance {
            mission_id,
            text,
            user_id,
        } => {
            match mission_status(repo_root, mission_id) {
                Ok(MissionStatus::Planning) => {
                    let Some(host) = host else {
                        post_thread_note(
                            cfg,
                            client,
                            threads,
                            mission_id,
                            &format!(
                                "This mission is still in planning, and this bridge has no hosted \
                             engine (it was started without `kranz serve`). Continue with \
                             `kranz plan --mission {mission_id}` in a terminal."
                            ),
                        )
                        .await;
                        return;
                    };
                    if !cfg.is_authorized(user_id.as_deref()) {
                        post_thread_note(
                            cfg,
                            client,
                            threads,
                            mission_id,
                            "Planning turns spend money and are limited to the \
                             `slack.allowUsers` allowlist — ask an admin to add you.",
                        )
                        .await;
                        return;
                    }
                    post_thread_note(
                        cfg,
                        client,
                        threads,
                        mission_id,
                        ":hourglass_flowing_sand: Planning turn running — the orchestrator's \
                         reply lands here, usually within a couple of minutes.",
                    )
                    .await;
                    match host.planning_turn(mission_id, text).await {
                        Ok(reply) => {
                            let blocks = crate::format::build_planning_reply(
                                mission_id,
                                &reply,
                                looks_like_plan_json(&reply),
                            );
                            if let Err(e) =
                                post_to_mission_thread(cfg, client, threads, mission_id, blocks)
                                    .await
                            {
                                tracing::warn!(mission = %mission_id, error = %e, "failed to post planning reply");
                            }
                        }
                        Err(e) => {
                            post_thread_note(
                                cfg,
                                client,
                                threads,
                                mission_id,
                                &format!("Planning turn failed: {e}"),
                            )
                            .await;
                        }
                    }
                }
                // Running / paused / blocked (and, unchanged from before,
                // terminal): the control-inbox guidance write. Steering a live
                // mission is spend-adjacent — strictly more powerful than the
                // allowlist-gated pause/resume — so gate it on the same
                // allowlist as planning and pause/resume.
                Ok(_) => {
                    if !cfg.is_authorized(user_id.as_deref()) {
                        post_thread_note(
                            cfg,
                            client,
                            threads,
                            mission_id,
                            "Steering a running mission spends money and is limited to the \
                             `slack.allowUsers` allowlist — ask an admin to add you.",
                        )
                        .await;
                        return;
                    }
                    if let Err(e) = guidance(repo_root, mission_id, text) {
                        tracing::warn!(error = %e, "failed to enqueue Slack guidance");
                    }
                }
                Err(e) => {
                    tracing::warn!(mission = %mission_id, error = %e, "failed to read mission status for thread reply");
                }
            }
        }

        // Ticket scaffolding: a pure local write, no reply.
        action => {
            if let Err(e) = apply_action(repo_root, action) {
                tracing::warn!(error = %e, "failed to apply inbound Slack action");
            }
        }
    }
}

/// Shared approve path for the slash command and both buttons.
///
/// The reviewed plan is parked HOST-SIDE by a Ready `request_plan` — one
/// cache shared by every surface (Slack buttons, web, glasses ring), so an
/// approve from any of them consumes the same plan. With a parked plan:
/// commit it through the hosted engine — THE step the first cut of this
/// bridge skipped, which left missions queued-but-unapproved that
/// `kranz work` then refused — and either start execution through the host
/// (`start == true`) or insert into the per-repo queue. Without one
/// (never requested, or forfeited by a serve restart / idle release):
/// honest, state-aware handling (a planning mission needs `/kranz plan`
/// first; an approved-but-idle one can still be queued/started).
#[allow(clippy::too_many_arguments)]
async fn approve_flow(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    host: Option<&SharedHost>,
    mission_id: &str,
    user_id: Option<&str>,
    response_url: Option<&str>,
    start: bool,
    button: bool,
) {
    if !cfg.is_authorized(user_id) {
        reply_ephemeral(cfg, client, response_url, &not_authorized_blocks()).await;
        return;
    }

    // `approve_pending` consumes the host-parked plan. `None` = no host, or
    // nothing parked — fall through to the state-aware routing below. On a
    // transient failure (e.g. a turn in flight) the host re-parked the plan,
    // so the retry click finds it again.
    let approved = match host {
        None => None,
        Some(host) => match host.approve_pending(mission_id).await {
            Ok(branch) => branch.map(|branch| (branch, host)),
            Err(e) => {
                reply_ephemeral(
                    cfg,
                    client,
                    response_url,
                    &error_blocks(&format!("Couldn't approve `{mission_id}`: {e}")),
                )
                .await;
                return;
            }
        },
    };
    if let Some((branch, host)) = approved {
        // Retire the plan card FIRST: from a button, rewrite the source
        // message into an outcome card so the second tap the old card invited
        // has nothing left to tap. Best-effort — the state change stands
        // regardless.
        if button {
            retire_plan_card(client, response_url, mission_id, Some(&branch), start).await;
        }
        if start {
            match host.start(mission_id).await {
                Ok(()) => {
                    post_thread_note(
                        cfg,
                        client,
                        threads,
                        mission_id,
                        &format!(
                            ":rocket: Plan approved and execution started (branch `{branch}`) — \
                         progress posts in this thread; deep inspection in the web UI."
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url,
                        &error_blocks(&format!(
                            "Approved `{mission_id}` (branch `{branch}`) but starting failed: \
                             {e}. Queue it with `/kranz approve {mission_id}` or run \
                             `kranz work`."
                        )),
                    )
                    .await;
                }
            }
        } else {
            // Free the just-approved engine BEFORE queueing: approve left it
            // attached in the host's registry holding the mission lock, and
            // the `kranz work` dispatcher this queue entry points at would be
            // refused with LockHeld while it stays there. Best-effort: on a
            // failed release the entry still queues, and the dispatcher's own
            // LockHeld refusal stays the honest backstop.
            match host.release(mission_id).await {
                Ok(true) => {}
                Ok(false) => {
                    tracing::warn!(mission = %mission_id, "release after approve found mission running")
                }
                Err(e) => {
                    tracing::warn!(mission = %mission_id, error = %e, "release after approve failed")
                }
            }
            match approve_mission(repo_root, mission_id) {
                Ok(()) => {
                    post_thread_note(
                        cfg,
                        client,
                        threads,
                        mission_id,
                        &format!(
                            ":white_check_mark: Plan approved and queued (branch `{branch}`) — \
                         the `kranz work` dispatcher runs it next."
                        ),
                    )
                    .await;
                }
                Err(e) => {
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url,
                        &error_blocks(&format!(
                            "Approved `{mission_id}` (branch `{branch}`) but queueing failed: {e}"
                        )),
                    )
                    .await;
                }
            }
        }
        return;
    }

    // No parked plan (or no host). Route by actual mission state instead of blindly
    // queueing (the old behavior, which dead-ended at run time on unapproved
    // missions).
    match mission_status(repo_root, mission_id) {
        Ok(MissionStatus::Planning) => {
            reply_ephemeral(
                cfg,
                client,
                response_url,
                &error_blocks(&format!(
                    "No reviewed plan is pending for `{mission_id}` — run \
                     `/kranz plan {mission_id}` first, then approve from the plan message."
                )),
            )
            .await;
        }
        Ok(
            status @ (MissionStatus::Complete | MissionStatus::Failed | MissionStatus::Abandoned),
        ) => {
            reply_ephemeral(
                cfg,
                client,
                response_url,
                &error_blocks(&format!(
                    "Mission `{mission_id}` is {status:?} — nothing to approve or queue."
                )),
            )
            .await;
        }
        // Approved earlier (plan committed, no live run) or paused/blocked:
        // starting/queueing is legitimate — EXCEPT when the mission is
        // actually executing right now. Status alone can't tell (approval
        // folds to Running before any run loop exists), so ask the host to
        // release its idle engine and treat an unreleasable / live-locked
        // mission as executing. This is the guard against a stale second
        // approve tap queueing a mission that is already underway.
        Ok(_) => {
            if start {
                let Some(host) = host else {
                    reply_ephemeral(cfg, client, response_url, &no_host_blocks(mission_id)).await;
                    return;
                };
                // host.start disambiguates on its own: it consumes an idle
                // hosted engine, resumes an unhosted one, and refuses a live
                // run with an honest conflict message.
                match host.start(mission_id).await {
                    Ok(()) => {
                        if button {
                            retire_plan_card(client, response_url, mission_id, None, true).await;
                        }
                        post_thread_note(
                            cfg,
                            client,
                            threads,
                            mission_id,
                            &format!(
                                ":rocket: Execution started for `{mission_id}` — progress posts \
                             in this thread."
                            ),
                        )
                        .await;
                    }
                    Err(e) => {
                        reply_ephemeral(
                            cfg,
                            client,
                            response_url,
                            &error_blocks(&format!("Couldn't start `{mission_id}`: {e}")),
                        )
                        .await;
                    }
                }
            } else {
                if let Some(host) = host {
                    match host.release(mission_id).await {
                        Ok(true) => {}
                        Ok(false) => {
                            reply_ephemeral(
                                cfg,
                                client,
                                response_url,
                                &error_blocks(&format!(
                                    "`{mission_id}` is already executing — steer it by replying \
                                 in its thread; nothing was queued."
                                )),
                            )
                            .await;
                            return;
                        }
                        Err(e) => {
                            reply_ephemeral(
                                cfg,
                                client,
                                response_url,
                                &error_blocks(&format!("Couldn't queue `{mission_id}`: {e}")),
                            )
                            .await;
                            return;
                        }
                    }
                }
                // Host-free (or released): a LIVE lock holder now means an
                // external process is running the mission.
                if kranz_engine::queue::is_repo_busy(repo_root).as_deref() == Some(mission_id) {
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url,
                        &error_blocks(&format!(
                            "`{mission_id}` is already executing — steer it by replying in its \
                         thread; nothing was queued."
                        )),
                    )
                    .await;
                    return;
                }
                match approve_mission(repo_root, mission_id) {
                    Ok(()) => {
                        if button {
                            retire_plan_card(client, response_url, mission_id, None, false).await;
                        }
                        reply_ephemeral(
                            cfg,
                            client,
                            response_url,
                            &error_blocks(&format!(
                                ":white_check_mark: Queued `{mission_id}` — the `kranz work` \
                                 dispatcher runs it next."
                            )),
                        )
                        .await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to queue mission from Slack");
                        reply_ephemeral(
                            cfg,
                            client,
                            response_url,
                            &error_blocks(&format!("Couldn't queue `{mission_id}`: {e}")),
                        )
                        .await;
                    }
                }
            }
        }
        Err(e) => {
            reply_ephemeral(
                cfg,
                client,
                response_url,
                &error_blocks(&format!("Couldn't read `{mission_id}`: {e}")),
            )
            .await;
        }
    }
}

/// A single mrkdwn section block for a short status / error / confirmation
/// ephemeral. (Not every reply warrants the full header/section/context frame.)
fn error_blocks(msg: &str) -> Vec<Value> {
    vec![json!({ "type": "section", "text": { "type": "mrkdwn", "text": msg } })]
}

/// The honest refusal for hosted-engine operations when the bridge was started
/// without a host (tests, or embedding `serve_slack` outside `kranz serve`).
fn no_host_blocks(mission_id: &str) -> Vec<Value> {
    error_blocks(&format!(
        "This bridge has no hosted planning engine (it was started without \
         `kranz serve`). Use `kranz plan --mission {mission_id}` in a terminal, \
         or the web UI via `kranz serve --open`."
    ))
}

/// Heuristic: does a planning reply contain what looks like a COMPLETE plan
/// JSON (the orchestrator chatted the plan out instead of waiting for the
/// formal request-plan)? Matches the plan schema's two distinctive top-level
/// keys — prose mentioning them both in quotes-and-colon form doesn't happen
/// in practice, and a false positive only adds a harmless hint line.
fn looks_like_plan_json(reply: &str) -> bool {
    reply.contains("\"validationContract\"") && reply.contains("\"milestones\"")
}

/// A mission's current status, folded read-only from its event log.
fn mission_status(repo_root: &Path, mission_id: &str) -> Result<MissionStatus> {
    if !MissionPaths::is_safe_id(mission_id) {
        anyhow::bail!("unknown mission `{mission_id}`");
    }
    let paths = MissionPaths::new(repo_root, mission_id);
    let events_path = paths.events_file();
    if !events_path.is_file() {
        anyhow::bail!("unknown mission `{mission_id}`");
    }
    let events = EventLog::read_events(&events_path)?;
    let state = reducer::fold(&events)?;
    Ok(state.mission.status)
}

/// Post blocks to a mission's Slack thread (instance-labeled), creating the
/// thread root in the configured channel — and recording the mapping — when
/// the mission has no thread yet (e.g. it was created via the CLI or web UI).
async fn post_to_mission_thread(
    cfg: &SlackConfig,
    client: &SlackClient,
    threads: &SharedThreads,
    mission_id: &str,
    blocks: Vec<Value>,
) -> Result<()> {
    let blocks = crate::format::label_blocks(blocks, cfg.instance_name.as_deref());
    let thread_ts = threads.thread_ts(mission_id);
    let posted_ts = client
        .post_message(&cfg.channel, &blocks, thread_ts.as_deref())
        .await?;
    if thread_ts.is_none() {
        threads.set(mission_id, &posted_ts);
    }
    Ok(())
}

/// The outcome card a consumed plan-review message is rewritten into: what
/// happened, to which mission, on which branch — and, pointedly, NO buttons.
/// Pure; unit-tested.
fn approved_card(mission_id: &str, branch: Option<&str>, started: bool) -> Vec<Value> {
    let (emoji, verb) = if started {
        (":rocket:", "approved & started")
    } else {
        (":white_check_mark:", "approved & queued")
    };
    let mut headline = format!("{emoji} *Plan {verb} — `{mission_id}`*");
    if let Some(branch) = branch {
        headline.push_str(&format!("\nbranch `{branch}`"));
    }
    let followup = if started {
        "Progress posts in the mission thread · deep inspection in the web UI."
    } else {
        "The `kranz work` dispatcher runs it next."
    };
    vec![
        json!({ "type": "section", "text": { "type": "mrkdwn", "text": headline } }),
        json!({ "type": "context", "elements": [{ "type": "mrkdwn", "text": followup }] }),
    ]
}

/// Rewrite the plan-review message a button click came from into an
/// [`approved_card`] (via `replace_original`), retiring its buttons.
/// Best-effort: the approval/start already happened; a failed rewrite only
/// leaves stale buttons, which the state-aware refusals now absorb anyway.
async fn retire_plan_card(
    client: &SlackClient,
    response_url: Option<&str>,
    mission_id: &str,
    branch: Option<&str>,
    started: bool,
) {
    let Some(url) = response_url else { return };
    if let Err(e) = client
        .replace_original(url, &approved_card(mission_id, branch, started))
        .await
    {
        tracing::warn!(mission = %mission_id, error = %e, "failed to retire plan card");
    }
}

/// [`post_to_mission_thread`] for a one-line note (acks, refusals, errors on
/// the thread-reply path, which has no `response_url` to reply ephemerally
/// to). Best-effort: a failed post is logged, never fatal.
async fn post_thread_note(
    cfg: &SlackConfig,
    client: &SlackClient,
    threads: &SharedThreads,
    mission_id: &str,
    msg: &str,
) {
    if let Err(e) =
        post_to_mission_thread(cfg, client, threads, mission_id, error_blocks(msg)).await
    {
        tracing::warn!(mission = %mission_id, error = %e, "failed to post thread note");
    }
}

/// Apply a routed inbound action to the local mission machinery. The actions
/// that reply over the slash `response_url` (Help, Status, NewMission,
/// RequestPlan, ApproveMission) are handled in [`dispatch_action`] because they
/// need the async client; they are no-ops here for exhaustiveness.
fn apply_action(repo_root: &Path, action: &Action) -> Result<()> {
    match action {
        Action::Approve { mission_id, .. } => approve_mission(repo_root, mission_id),
        Action::Guidance {
            mission_id, text, ..
        } => guidance(repo_root, mission_id, text),
        Action::NewTicket { title, .. } => scaffold_ticket(repo_root, title),
        Action::Help { .. }
        | Action::Status { .. }
        | Action::Todo { .. }
        | Action::TicketList { .. }
        | Action::TicketShow { .. }
        | Action::NewMission { .. }
        | Action::NewMissionModal { .. }
        | Action::NewTicketModal { .. }
        | Action::CreateTicket { .. }
        | Action::ConfigModal { .. }
        | Action::RequestPlan { .. }
        | Action::ApproveMission { .. }
        | Action::QueueTicket { .. }
        | Action::ApproveStart { .. }
        | Action::Draft { .. }
        | Action::Config { .. }
        | Action::Pause { .. }
        | Action::Resume { .. }
        | Action::Work { .. }
        | Action::WorkRun { .. }
        | Action::AppHome { .. }
        | Action::Merge { .. }
        | Action::Ignore => Ok(()),
    }
}

/// Build the status-summary blocks for a mission by folding its event log —
/// fully wired and read-only (no allowlist gate, no backend). When
/// `mission_id` is `None`, the most-recently-created mission is chosen. An
/// unknown/absent mission is a plain error the caller turns into an ephemeral.
fn build_status_reply(repo_root: &Path, mission_id: Option<&str>) -> Result<Vec<Value>> {
    let mission_id = match mission_id {
        Some(id) if MissionPaths::is_safe_id(id) => id.to_string(),
        Some(id) => return Err(anyhow::anyhow!("unknown mission `{id}`")),
        None => most_recent_mission(repo_root).ok_or_else(|| {
            anyhow::anyhow!("no missions yet — create one with `/kranz new <goal>`")
        })?,
    };
    let paths = MissionPaths::new(repo_root, &mission_id);
    let events_path = paths.events_file();
    if !events_path.is_file() {
        return Err(anyhow::anyhow!("unknown mission `{mission_id}`"));
    }
    let events = EventLog::read_events(&events_path)?;
    let state = reducer::fold(&events)?;
    let summary = crate::format::StatusSummary {
        mission_id: mission_id.clone(),
        status: status_word(state.mission.status),
        summary: render_status_body(&state),
    };
    Ok(crate::format::build_status(&summary))
}

/// `/kranz status` reply: deterministic pipeline snapshot (no LLM/engine).
fn build_pipeline_status_reply(repo_root: &Path) -> Vec<Value> {
    use crate::format::{PipelineStage, PipelineStatusSnapshot, StageCount, UnmergedMission};

    let rows = build_pipeline_rows(repo_root);
    let mut counts: BTreeMap<PipelineStage, usize> =
        PipelineStage::ALL.iter().map(|stage| (*stage, 0)).collect();
    for row in &rows {
        *counts.entry(row.stage).or_insert(0) += 1;
    }

    let running = running_mission_snapshot(repo_root, &rows);
    let unmerged = rows
        .iter()
        .filter(|row| row.stage == PipelineStage::Delivered)
        .filter_map(|row| {
            let mission_id = row.mission_id.clone()?;
            Some(UnmergedMission {
                mission_id,
                title: row.title.clone(),
                ticket_slug: row.slug.clone(),
            })
        })
        .collect();

    let snapshot = PipelineStatusSnapshot {
        running,
        queue_depth: kranz_engine::queue::list(repo_root).len(),
        stage_counts: PipelineStage::ALL
            .iter()
            .map(|stage| StageCount {
                stage: *stage,
                count: counts.get(stage).copied().unwrap_or(0),
            })
            .collect(),
        unmerged,
    };
    crate::format::build_pipeline_status(&snapshot)
}

fn running_mission_snapshot(
    repo_root: &Path,
    rows: &[PipelineRow],
) -> Option<crate::format::RunningMissionSnapshot> {
    if let Some(busy_id) = kranz_engine::queue::is_repo_busy(repo_root) {
        return rows
            .iter()
            .find(|row| row.mission_id.as_deref() == Some(busy_id.as_str()))
            .map(row_to_running_snapshot)
            .or_else(|| {
                Some(crate::format::RunningMissionSnapshot {
                    mission_id: busy_id,
                    title: "mission holds the repo busy lock".to_string(),
                    status: "Running".to_string(),
                    cost_usd: None,
                })
            });
    }

    rows.iter()
        .find(|row| row.stage == crate::format::PipelineStage::Running && row.mission_id.is_some())
        .map(row_to_running_snapshot)
}

fn row_to_running_snapshot(row: &PipelineRow) -> crate::format::RunningMissionSnapshot {
    crate::format::RunningMissionSnapshot {
        mission_id: row.mission_id.clone().unwrap_or_else(|| row.id.clone()),
        title: row.title.clone(),
        status: row
            .mission_status
            .map(mission_stage_status_word)
            .unwrap_or_else(|| "Running".to_string()),
        cost_usd: row.cost_usd,
    }
}

/// `/kranz todo` reply: deterministic operator worklist (no LLM/engine).
fn build_todo_reply(repo_root: &Path, dashboard_url: Option<&str>) -> Vec<Value> {
    let rows = build_pipeline_rows(repo_root);
    let todo = crate::format::OperatorTodo {
        pipeline_actions: pipeline_todo_actions(&rows, dashboard_url),
        gated_items: read_operator_gates(repo_root),
    };
    crate::format::build_operator_todo(&todo)
}

fn pipeline_todo_actions(
    rows: &[PipelineRow],
    dashboard_url: Option<&str>,
) -> Vec<crate::format::TodoAction> {
    use crate::format::{PipelineStage, TodoAction, TodoActionKind, TodoTarget};

    let mut actions = Vec::new();
    for row in rows {
        match row.stage {
            PipelineStage::Reviewable if row.slug.is_some() => {
                let slug = row.slug.clone().expect("checked above");
                let (note, target) = if row.is_blocked {
                    let note = if row.blocked_by.is_empty() {
                        "Blocked by another ticket.".to_string()
                    } else {
                        format!("Blocked by {}.", row.blocked_by.join(", "))
                    };
                    (note, open_ticket_target(dashboard_url, &slug))
                } else {
                    (
                        "Queue reviewed plan for run.".to_string(),
                        TodoTarget::QueueTicket { slug: slug.clone() },
                    )
                };
                actions.push(TodoAction {
                    kind: TodoActionKind::Reviewable,
                    id: slug,
                    title: row.title.clone(),
                    note: Some(note),
                    target,
                });
            }
            PipelineStage::Delivered if row.mission_id.is_some() => {
                let mission_id = row.mission_id.clone().expect("checked above");
                actions.push(TodoAction {
                    kind: TodoActionKind::Delivered,
                    id: mission_id.clone(),
                    title: row.title.clone(),
                    note: Some("Report is ready; merge the mission branch.".to_string()),
                    target: TodoTarget::MergeMission { mission_id },
                });
            }
            PipelineStage::NeedsYou if row.slug.is_some() => {
                let slug = row.slug.clone().expect("checked above");
                actions.push(TodoAction {
                    kind: TodoActionKind::NeedsYou,
                    id: slug.clone(),
                    title: row.title.clone(),
                    note: Some("Answer drafter questions, then redraft.".to_string()),
                    target: open_ticket_target(dashboard_url, &slug),
                });
            }
            _ => {}
        }
    }
    actions
}

fn open_ticket_target(dashboard_url: Option<&str>, slug: &str) -> crate::format::TodoTarget {
    match dashboard_url.map(str::trim).filter(|u| !u.is_empty()) {
        Some(url) => crate::format::TodoTarget::OpenUrl {
            label: "Open ticket".to_string(),
            url: dashboard_backlog_deep_link(url, slug),
        },
        None => crate::format::TodoTarget::None,
    }
}

fn dashboard_backlog_deep_link(dashboard_url: &str, slug: &str) -> String {
    let base = dashboard_url.trim_end_matches('/');
    format!("{base}/#/backlog/{slug}")
}

#[derive(Debug, Clone)]
struct PipelineRow {
    stage: crate::format::PipelineStage,
    id: String,
    title: String,
    mission_id: Option<String>,
    slug: Option<String>,
    blocked_by: Vec<String>,
    is_blocked: bool,
    mission_status: Option<MissionStageStatus>,
    cost_usd: Option<f64>,
}

#[derive(Debug, Clone)]
struct MissionProjection {
    id: String,
    status: MissionStageStatus,
    merged: Option<bool>,
    goal: String,
    cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MissionStageStatus {
    Known(MissionStatus),
    Deleted,
}

/// Build the same flat work-item rows as `PipelineView.tsx::buildRows`:
/// every ticket is a row, and every mission not consumed by a ticket is a row.
fn build_pipeline_rows(repo_root: &Path) -> Vec<PipelineRow> {
    use kranz_engine::ticket::Ticket;

    let missions = mission_projections(repo_root);
    let missions_by_id: HashMap<String, MissionProjection> = missions
        .iter()
        .cloned()
        .map(|mission| (mission.id.clone(), mission))
        .collect();
    let mut consumed_missions = std::collections::BTreeSet::new();
    let mut rows = Vec::new();

    for ticket in Ticket::list(repo_root) {
        let state = Ticket::read_state(repo_root, &ticket.slug);
        let mission_id = Ticket::mission_for(repo_root, &ticket.slug);
        let mission = mission_id.as_deref().and_then(|id| missions_by_id.get(id));
        if let Some(mission) = mission {
            consumed_missions.insert(mission.id.clone());
        }
        rows.push(PipelineRow {
            stage: pipeline_stage_for_ticket(state, mission),
            id: ticket.slug.clone(),
            title: ticket.title.clone(),
            mission_id,
            slug: Some(ticket.slug.clone()),
            blocked_by: ticket.blocked_by.clone(),
            is_blocked: kranz_engine::deps::is_blocked(repo_root, &ticket.slug).unwrap_or(false),
            mission_status: mission.map(|m| m.status),
            cost_usd: mission.and_then(|m| m.cost_usd),
        });
    }

    for mission in missions {
        if mission.status == MissionStageStatus::Deleted || consumed_missions.contains(&mission.id)
        {
            continue;
        }
        rows.push(PipelineRow {
            stage: stage_from_mission(mission.status, mission.merged),
            id: mission.id.clone(),
            title: mission.goal.clone(),
            mission_id: Some(mission.id),
            slug: None,
            blocked_by: vec![],
            is_blocked: false,
            mission_status: Some(mission.status),
            cost_usd: mission.cost_usd,
        });
    }

    rows
}

/// Fold missions into the subset of the REST `/api/missions` projection the
/// dashboard stage model needs: status, merged bit, goal, and live cost.
fn mission_projections(repo_root: &Path) -> Vec<MissionProjection> {
    let mut ids = MissionPaths::list_missions(repo_root);
    ids.sort();
    let repo = kranz_engine::git_ops::GitRepo::open(repo_root).ok();
    ids.into_iter()
        .map(|id| {
            let paths = MissionPaths::new(repo_root, &id);
            if !paths.events_file().is_file() {
                return MissionProjection {
                    id,
                    status: MissionStageStatus::Deleted,
                    merged: None,
                    goal: "deleted mission (no data recorded)".to_string(),
                    cost_usd: None,
                };
            }
            match EventLog::read_events(&paths.events_file()).and_then(|e| reducer::fold(&e)) {
                Ok(state) => MissionProjection {
                    id,
                    status: MissionStageStatus::Known(state.mission.status),
                    merged: repo
                        .as_ref()
                        .and_then(|repo| kranz_engine::merged::merged_bit(repo, &state.mission)),
                    goal: state.mission.goal,
                    cost_usd: (state.total_cost_usd > 0.0).then_some(state.total_cost_usd),
                },
                Err(e) => MissionProjection {
                    id,
                    status: MissionStageStatus::Known(MissionStatus::Failed),
                    merged: None,
                    goal: format!("unreadable mission ({e})"),
                    cost_usd: None,
                },
            }
        })
        .collect()
}

/// Direct Rust mirror of `pipelineStage.ts::stageFromMission`.
fn stage_from_mission(
    status: MissionStageStatus,
    merged: Option<bool>,
) -> crate::format::PipelineStage {
    use crate::format::PipelineStage;
    match status {
        MissionStageStatus::Known(MissionStatus::Failed) => PipelineStage::Failed,
        MissionStageStatus::Known(MissionStatus::Complete) => {
            if merged == Some(true) {
                PipelineStage::Landed
            } else {
                PipelineStage::Delivered
            }
        }
        MissionStageStatus::Known(
            MissionStatus::Running
            | MissionStatus::Paused
            | MissionStatus::Blocked
            | MissionStatus::Validating,
        ) => PipelineStage::Running,
        MissionStageStatus::Known(MissionStatus::Planning) => PipelineStage::Reviewable,
        MissionStageStatus::Known(MissionStatus::Approved) => PipelineStage::Queued,
        MissionStageStatus::Known(MissionStatus::Abandoned) | MissionStageStatus::Deleted => {
            PipelineStage::Abandoned
        }
    }
}

/// Direct Rust mirror of `pipelineStage.ts::pipelineStage` for ticket rows.
fn pipeline_stage_for_ticket(
    state: kranz_engine::ticket::TicketState,
    mission: Option<&MissionProjection>,
) -> crate::format::PipelineStage {
    use crate::format::PipelineStage;
    use kranz_engine::ticket::TicketState;
    match state {
        TicketState::New => PipelineStage::Captured,
        TicketState::Drafting => PipelineStage::Drafting,
        TicketState::NeedsContext => PipelineStage::NeedsYou,
        TicketState::Review => PipelineStage::Reviewable,
        TicketState::Queued => PipelineStage::Queued,
        TicketState::Running => PipelineStage::Running,
        TicketState::Done => match mission.map(|m| m.status) {
            None
            | Some(MissionStageStatus::Known(MissionStatus::Abandoned))
            | Some(MissionStageStatus::Deleted) => PipelineStage::Landed,
            Some(_) => {
                if mission.and_then(|m| m.merged) == Some(true) {
                    PipelineStage::Landed
                } else {
                    PipelineStage::Delivered
                }
            }
        },
        TicketState::Failed => PipelineStage::Failed,
    }
}

fn mission_stage_status_word(status: MissionStageStatus) -> String {
    match status {
        MissionStageStatus::Known(status) => status_word(status),
        MissionStageStatus::Deleted => "Deleted".to_string(),
    }
}

fn read_operator_gates(repo_root: &Path) -> Vec<crate::format::GateItem> {
    let path = repo_root.join("docs").join("operator-gates.md");
    let Ok(markdown) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_operator_gates(&markdown)
}

fn parse_operator_gates(markdown: &str) -> Vec<crate::format::GateItem> {
    markdown
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim_start();
            let item = trimmed
                .strip_prefix("- [ ] ")
                .or_else(|| trimmed.strip_prefix("- [x] "))
                .or_else(|| trimmed.strip_prefix("- [X] "))
                .or_else(|| trimmed.strip_prefix("- "))?
                .trim();
            (!item.is_empty()).then(|| crate::format::GateItem {
                title: item.to_string(),
            })
        })
        .collect()
}

/// `/kranz ticket list` reply: one row per parseable ticket under
/// `.kranz/tickets/`, via [`kranz_engine::ticket::Ticket::list`] (the same
/// primitive the REST `GET /api/tickets` route uses). Pure read + render, so
/// it is unit-tested directly; never fails (an empty/missing tickets dir just
/// yields an empty list).
pub fn build_ticket_list_reply(repo_root: &Path) -> Vec<Value> {
    let rows: Vec<crate::format::TicketRow> = kranz_engine::ticket::Ticket::list(repo_root)
        .iter()
        .map(|t| crate::format::TicketRow {
            slug: t.slug.clone(),
            priority: t.priority,
            state: ticket_terminal_state_label(repo_root, &t.slug),
            title: t.title.clone(),
            blocked_by: t.blocked_by.clone(),
        })
        .collect();
    crate::format::build_ticket_list(&rows)
}

/// A ticket's display-state label, splitting `Done` into `Delivered`
/// (mission complete but its branch is not yet merged into base) vs
/// `Landed` (mission branch merged, or no mission-merge information to
/// distinguish otherwise) — reusing the engine's merged-ancestor probe
/// ([`kranz_engine::ticket::ticket_merged`]) so this can never drift from
/// the REST `/api/tickets` projection or the CLI's `ticket_terminal_label`.
/// Non-`Done` states render exactly as `{:?}` did before.
fn ticket_terminal_state_label(repo_root: &Path, slug: &str) -> String {
    let state = kranz_engine::ticket::Ticket::read_state(repo_root, slug);
    if state != kranz_engine::ticket::TicketState::Done {
        return format!("{state:?}");
    }
    match kranz_engine::merged::ticket_merged(repo_root, slug) {
        Some(false) => "Delivered".to_string(),
        Some(true) | None => "Landed".to_string(),
    }
}

/// `/kranz ticket show <slug>` reply: the ticket's detail via
/// [`kranz_engine::ticket::Ticket::load`] + `read_state`. A graceful ephemeral
/// error (never a panic) for an invalid slug (checked with
/// [`kranz_engine::ticket::Ticket::ensure_valid_slug`] BEFORE touching the
/// filesystem) or one with no ticket file.
pub fn build_ticket_show_reply(repo_root: &Path, slug: &str) -> Vec<Value> {
    use kranz_engine::ticket::Ticket;

    if let Err(e) = Ticket::ensure_valid_slug(slug) {
        return error_blocks(&format!("Invalid ticket slug `{slug}`: {e}"));
    }
    let path = Ticket::tickets_dir(repo_root).join(format!("{slug}.md"));
    if !path.is_file() {
        return error_blocks(&format!("Unknown ticket `{slug}`."));
    }
    let ticket = match Ticket::load(&path) {
        Ok(t) => t,
        Err(e) => return error_blocks(&format!("Couldn't read ticket `{slug}`: {e}")),
    };
    let state = ticket_terminal_state_label(repo_root, slug);
    let detail = crate::format::TicketDetail {
        slug: ticket.slug.clone(),
        title: ticket.title.clone(),
        goal: ticket.goal.clone(),
        state,
        blocked_by: ticket.blocked_by.clone(),
        needs_context: needs_context_questions(&ticket.raw_body),
    };
    crate::format::build_ticket_show(&detail)
}

/// The orchestrator's clarifying questions appended under a `## Needs
/// context` heading in a ticket's raw body. Mirrors
/// `kranz_server::tickets::needs_context_questions` (itself a port of
/// `kranz_cli::backlog::needs_context_block`'s heading scan) — a small
/// derived-data extraction, not ticket parsing, so it stays a thin per-crate
/// copy rather than a shared engine primitive.
fn needs_context_questions(raw_body: &str) -> Vec<String> {
    let mut collecting = false;
    let mut out = Vec::new();
    for line in raw_body.lines() {
        let is_section = line.trim_start().starts_with("##");
        if collecting && is_section {
            break;
        }
        if is_section
            && line
                .trim_start_matches('#')
                .trim()
                .to_ascii_lowercase()
                .starts_with("needs context")
        {
            collecting = true;
            continue;
        }
        if collecting {
            if let Some(item) = bullet_item(line) {
                out.push(item);
            }
        }
    }
    out
}

/// The content of a dash bullet (`- item`), trimmed, else `None`.
fn bullet_item(line: &str) -> Option<String> {
    let t = line.trim_start();
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(marker) {
            let item = rest.trim().to_string();
            if !item.is_empty() {
                return Some(item);
            }
        }
    }
    None
}

/// The most-recently-created mission id under `repo_root`, if any. Missions are
/// listed by [`MissionPaths::list_missions`]; "most recent" is the one whose
/// event log was modified last (creation writes `mission.created`), which is a
/// good-enough "the mission you just made" heuristic for a bare `/kranz status`.
/// Resolve the target of a spend-reshaping `/kranz config` (also pause/resume):
/// an ACTIVE (non-terminal) mission, chosen unambiguously. Thin delegation to
/// the engine's [`kranz_engine::control::resolve_active_mission`], which the
/// CLI's `kranz config role` shares — both surfaces refuse the same hazardous
/// targets (unknown, terminal, or ambiguous without an explicit id).
fn resolve_active_config_target(repo_root: &Path, explicit: Option<&str>) -> Result<String> {
    Ok(kranz_engine::control::resolve_active_mission(
        repo_root, explicit,
    )?)
}

fn most_recent_mission(repo_root: &Path) -> Option<String> {
    MissionPaths::list_missions(repo_root)
        .into_iter()
        .filter_map(|id| {
            let events = MissionPaths::new(repo_root, &id).events_file();
            let mtime = std::fs::metadata(&events).and_then(|m| m.modified()).ok()?;
            Some((id, mtime))
        })
        .max_by_key(|(_, mtime)| *mtime)
        .map(|(id, _)| id)
}

/// A short status word for the header pill (the `MissionStatus` Debug name,
/// e.g. `Planning`, `Running`, `Complete`).
fn status_word(status: MissionStatus) -> String {
    format!("{status:?}")
}

/// A compact multi-line status body: goal excerpt, milestone tally, and cost.
fn render_status_body(state: &MissionState) -> String {
    let total = state.mission.milestones.len();
    let done = state
        .mission
        .milestones
        .iter()
        .filter(|m| matches!(m.status, kranz_engine::types::MilestoneStatus::Complete))
        .count();
    let mut body = String::new();
    let goal = state.mission.goal.trim();
    if !goal.is_empty() {
        body.push_str("*Goal*\n");
        body.push_str(goal);
        body.push_str("\n\n");
    }
    body.push_str(&format!(
        "{done}/{total} milestone{} complete",
        if total == 1 { "" } else { "s" }
    ));
    if state.total_cost_usd > 0.0 {
        body.push_str(&format!(" · cost ${:.2}", state.total_cost_usd));
    }
    body
}

/// Approve-and-queue a mission from a Slack button: enqueue it in the per-repo
/// execution queue and mark the ticket (if any) Queued. Mirrors the server's
/// approve→queue path.
fn approve_mission(repo_root: &Path, mission_id: &str) -> Result<()> {
    use kranz_engine::queue::{self, QueueEntry};
    // Priority mirrors the ticket if one drove this mission; default 2 (normal)
    // when the mission wasn't ticket-born. We don't have the ticket slug here
    // (it lives with the mission dir if at all), so enqueue at the default and
    // let the queue's own ordering apply.
    let entry = QueueEntry {
        mission_id: mission_id.to_string(),
        ticket_slug: None,
        priority: 2,
        seq: 0, // assigned by enqueue
    };
    queue::enqueue(repo_root, entry).context("enqueue approved mission")?;
    tracing::info!(mission = %mission_id, "approved+queued from Slack");
    Ok(())
}

/// `/kranz config [<id>] <role> <model> [effort]` → enqueue a `config-change`
/// control command on the target mission's inbox. `role` is the canonical
/// friendly name from the router; the camelCase engine patch is built by
/// [`crate::inbound::config_patch`]. When `mission_id` is `None`, the repo's
/// single active mission is targeted (several active → refusal). Returns the
/// mission id the change was applied to (for the reply).
fn config_change(
    repo_root: &Path,
    mission_id: Option<&str>,
    role: &str,
    model: &str,
    effort: Option<&str>,
) -> Result<String> {
    // Resolve to an ACTIVE (non-terminal) mission by identity, not filesystem
    // mtime. Two hazards the mtime `most_recent_mission` heuristic (fine for a
    // read-only status guess) causes for a spend-reshaping config change:
    //  - a running mission's log mtime keeps advancing, so a bare
    //    `/kranz config` after `/kranz new` would hijack the RUNNING mission;
    //  - a terminal mission's control inbox is never drained (run() refuses
    //    terminal missions), so enqueuing there is a silent no-op reported as
    //    success. So: reject terminal targets, and require an explicit id when
    //    more than one mission is active.
    let mission_id = resolve_active_config_target(repo_root, mission_id)?;
    let paths = MissionPaths::new(repo_root, &mission_id);
    let patch = crate::inbound::config_patch(role, model, effort)
        .ok_or_else(|| anyhow::anyhow!("unknown role `{role}`"))?;
    kranz_engine::control::enqueue(&paths, &ControlCommand::ConfigChange { patch })
        .context("enqueue config change")?;
    tracing::info!(mission = %mission_id, role, model, "config change enqueued from Slack");
    Ok(mission_id)
}

/// `/kranz pause|resume [<id>]` handler: allowlist-gate, resolve the target
/// active mission, enqueue the control command, and reply ephemerally. Shared by
/// both the pause and resume arms — only the `ControlCommand` and the verb (for
/// the confirmation) differ. A bad/ambiguous/terminal target is an honest
/// ephemeral error that enqueues NOTHING (the resolver refuses those).
#[allow(clippy::too_many_arguments)]
async fn steer(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    mission_id: Option<&str>,
    user_id: Option<&str>,
    response_url: Option<&str>,
    cmd: ControlCommand,
    verb: &str,
) {
    if !cfg.is_authorized(user_id) {
        reply_ephemeral(cfg, client, response_url, &not_authorized_blocks()).await;
        return;
    }
    match enqueue_steer(repo_root, mission_id, cmd) {
        Ok(applied_to) => {
            reply_ephemeral(
                cfg,
                client,
                response_url,
                &error_blocks(&format!(
                    ":pause_button: {verb} `{applied_to}` (takes effect between worker runs)."
                )),
            )
            .await
        }
        Err(e) => {
            tracing::warn!(error = %e, "failed to enqueue steering command from Slack");
            reply_ephemeral(
                cfg,
                client,
                response_url,
                &error_blocks(&format!("Couldn't {verb} that mission: {e}")),
            )
            .await
        }
    }
}

/// Resolve the target ACTIVE mission (same policy as `/kranz config`: an
/// explicit id must exist and be non-terminal; a bare command needs exactly one
/// active mission) and enqueue `cmd` on its control inbox. Returns the mission
/// id the command was applied to (for the reply). Pure local write, so it is
/// unit-tested directly. A terminal/ambiguous/unknown target returns an error
/// and enqueues NOTHING (the resolver refuses before any file is written) — a
/// terminal mission's control inbox is never drained, so enqueuing there would
/// be a silent no-op reported as success.
fn enqueue_steer(
    repo_root: &Path,
    mission_id: Option<&str>,
    cmd: ControlCommand,
) -> Result<String> {
    let mission_id = resolve_active_config_target(repo_root, mission_id)?;
    let paths = MissionPaths::new(repo_root, &mission_id);
    kranz_engine::control::enqueue(&paths, &cmd).context("enqueue steering command")?;
    tracing::info!(mission = %mission_id, ?cmd, "steering command enqueued from Slack");
    Ok(mission_id)
}

/// `/kranz work` reply: report the per-repo execution queue (from
/// [`kranz_engine::queue::list`]) and whether the repo is currently busy
/// ([`kranz_engine::queue::is_repo_busy`]), then point at the `kranz work`
/// dispatcher for actually draining it. REPORT-ONLY: the bridge never runs a
/// mission on the socket loop, so this reads state and hands off. Pure read +
/// render, so it is unit-tested directly.
fn build_work_reply(repo_root: &Path) -> Vec<Value> {
    let queue = kranz_engine::queue::list(repo_root);
    let busy = kranz_engine::queue::is_repo_busy(repo_root);

    let mut body = String::new();
    match &busy {
        Some(running) => body.push_str(&format!(":running: Running `{running}` now.\n\n")),
        None => body.push_str(":white_circle: No mission is running.\n\n"),
    }
    if queue.is_empty() {
        body.push_str("The execution queue is empty.");
    } else {
        body.push_str(&format!("*Queue* ({} waiting)\n", queue.len()));
        for (i, e) in queue.iter().enumerate() {
            body.push_str(&format!(
                "{}. `{}` · priority {}\n",
                i + 1,
                e.mission_id,
                e.priority
            ));
        }
    }
    body.push_str(
        "\n\nDraining runs via the `kranz work` dispatcher \
         (`kranz work` drains the whole queue, `kranz work --once` the front entry). \
         The Slack bridge reports the queue; it does not run missions.",
    );
    error_blocks(&body)
}

/// Fold the repo read-only into an App Home view: active (non-terminal)
/// missions with their status, the execution queue, and open (not-done) tickets.
/// Pure rendering lives in [`crate::format::build_home_view`]; this is the
/// read-only fold that feeds it.
fn build_home_view(repo_root: &Path, dashboard_url: Option<&str>) -> Value {
    use crate::format::{HomeMission, HomeQueueItem, HomeTicket};

    // Active missions: fold each log, keep the non-terminal ones.
    let mut missions = Vec::new();
    for id in MissionPaths::list_missions(repo_root) {
        let events_path = MissionPaths::new(repo_root, &id).events_file();
        if !events_path.is_file() {
            continue;
        }
        let state = match EventLog::read_events(&events_path).and_then(|e| reducer::fold(&e)) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(mission = %id, error = %e, "skipping mission in home view (fold failed)");
                continue;
            }
        };
        if is_terminal(state.mission.status) {
            continue;
        }
        missions.push(HomeMission {
            mission_id: id,
            status: status_word(state.mission.status),
        });
    }

    let queue = kranz_engine::queue::list(repo_root)
        .into_iter()
        .map(|e| HomeQueueItem {
            mission_id: e.mission_id,
            priority: e.priority,
        })
        .collect::<Vec<_>>();

    // Open tickets: everything not in a terminal (Done/Failed) pipeline state.
    let tickets = kranz_engine::ticket::Ticket::list(repo_root)
        .into_iter()
        .filter_map(|t| {
            let state = kranz_engine::ticket::Ticket::read_state(repo_root, &t.slug);
            if ticket_is_open(state) {
                Some(HomeTicket {
                    slug: t.slug,
                    title: t.title,
                    state: format!("{state:?}"),
                })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    crate::format::build_home_view(&missions, &queue, &tickets, dashboard_url)
}

/// Whether a mission status is terminal (excluded from the active-missions list).
fn is_terminal(status: MissionStatus) -> bool {
    matches!(
        status,
        MissionStatus::Complete | MissionStatus::Failed | MissionStatus::Abandoned
    )
}

/// Whether a ticket is still "open" (surfaced in App Home) — anything not in a
/// terminal pipeline state.
fn ticket_is_open(state: kranz_engine::ticket::TicketState) -> bool {
    use kranz_engine::ticket::TicketState;
    !matches!(state, TicketState::Done | TicketState::Failed)
}

/// Threaded reply → orchestrator guidance via the mission's control inbox.
fn guidance(repo_root: &Path, mission_id: &str, text: &str) -> Result<()> {
    let paths = MissionPaths::new(repo_root, mission_id);
    kranz_engine::control::enqueue(
        &paths,
        &ControlCommand::Msg {
            text: text.to_string(),
            interrupt: false,
        },
    )
    .context("enqueue guidance message")?;
    tracing::info!(mission = %mission_id, "guidance enqueued from Slack thread");
    Ok(())
}

/// `/kranz new <goal>` → create a mission and seed planning (M2.9 slice 1).
///
/// WIRED but NOT unit-tested: this builds the REAL backend
/// (`ClaudeBackend::discover`) and runs one `planning_turn`, which spawns a
/// `claude` child — exactly the same create + first-turn path the M2.5 host and
/// the CLI take. Tests never call it (they would spawn `claude`); the parsing,
/// allowlist gate, and rendering around it are all tested.
///
/// Flow (mirrors `kranz_server::MissionHost::create` + one planning turn):
/// 1. `config::load(repo_root)` → the layered `MissionConfig`.
/// 2. `ClaudeBackend::discover(cfg.claude_binary)` → the agent backend.
/// 3. `MissionEngine::create(backend, repo_root, goal, cfg)` → the mission id.
/// 4. one `planning_turn(goal)` to seed the ticket/goal and draw out the
///    orchestrator's opening scoping questions.
/// 5. post the ack (id + goal + opening reply) to a NEW Slack thread, and
///    record the mission↔thread mapping so in-thread replies route back
///    (subsequent multi-turn planning is a follow-up; the mapping is the hook
///    it will build on).
///
/// Returns the ack blocks the caller sends ephemerally to the invoker; the
/// public thread root is posted here. The public post is instance-labeled
/// directly; the RETURNED blocks are unlabeled because the caller routes them
/// through [`reply_ephemeral`], which applies the label (labeling here too
/// would double-prefix).
async fn new_mission(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    host: Option<&SharedHost>,
    goal: &str,
    channel: &str,
) -> Result<Vec<Value>> {
    // Hosted path (`kranz serve --slack`): create THROUGH the registry so the
    // planning engine stays live across turns — this is what makes replying in
    // the thread (a follow-up planning turn) work. The engine-dropping direct
    // path below survives only for a host-less bridge.
    let (mission_id, opening) = match host {
        Some(host) => {
            let mission_id = host.create(goal).await.context("creating mission")?;
            let reply = host
                .planning_turn(&mission_id, goal)
                .await
                .context("seeding planning turn")?;
            (mission_id, reply)
        }
        None => {
            use kranz_engine::backend_claude::ClaudeBackend;
            use kranz_engine::config;
            use kranz_engine::orchestrator::MissionEngine;
            use std::sync::Arc;

            let cfg_engine = config::load(repo_root).context("loading mission config")?;
            let backend = ClaudeBackend::discover(cfg_engine.claude_binary.as_deref())
                .context("discovering claude backend")?;
            let mut engine =
                MissionEngine::create(Arc::new(backend), repo_root.to_path_buf(), goal, cfg_engine)
                    .context("creating mission")?;
            let mission_id = engine.mission_id().to_string();
            // One seeding planning turn: the orchestrator's opening scoping
            // questions come back to post in-thread. A captured seed reply
            // (fresh session) happened first, so prepend it.
            let reply = engine
                .planning_turn(goal)
                .await
                .context("seeding planning turn")?;
            (mission_id, prepend_seed(engine.take_seed_reply(), reply))
        }
    };
    let opening = opening.trim();
    let opening_reply = (!opening.is_empty()).then(|| opening.to_string());

    let blocks = crate::format::build_new_mission_ack(&crate::format::NewMissionAck {
        mission_id: mission_id.clone(),
        goal: goal.to_string(),
        opening_reply,
    });

    // Post the planning thread root publicly (instance-labeled), then record
    // the mapping so in-thread replies route back to this mission.
    let labeled = crate::format::label_blocks(blocks.clone(), cfg.instance_name.as_deref());
    let posted_ts = client
        .post_message(channel, &labeled, None)
        .await
        .context("posting new-mission thread root")?;
    threads.set(&mission_id, &posted_ts);

    Ok(blocks)
}

/// Prepend a captured seed reply (fresh orchestrator session / re-seed) to a
/// turn's reply — the seed happened first in the conversation. Mirrors the
/// server host's `prepend_seed`.
fn prepend_seed(seed: Option<String>, reply: String) -> String {
    match seed.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        Some(seed) if !reply.trim().is_empty() => format!("{seed}\n\n{reply}"),
        Some(seed) => seed,
        None => reply,
    }
}

/// `/kranz ticket <title>` → scaffold a ticket markdown file the human can then
/// flesh out (the design's "the thread IS the context dump"; here we seed a
/// skeleton with the title and a pointer to fill in from the thread).
fn scaffold_ticket(repo_root: &Path, title: &str) -> Result<()> {
    use kranz_engine::ticket::Ticket;
    let slug = slugify(title);
    let dir = Ticket::tickets_dir(repo_root);
    std::fs::create_dir_all(&dir).context("creating tickets dir")?;
    let path = dir.join(format!("{slug}.md"));
    if path.exists() {
        tracing::info!(slug = %slug, "ticket already exists; not overwriting");
        return Ok(());
    }
    let body = format!(
        "---\ntitle: {title}\npriority: 2\nschedule: once\n---\n\n\
         ## Goal\n<one paragraph — becomes the mission goal>\n\n\
         ## Context\n<filled from the Slack thread>\n\n\
         ## Scoping answers\n- Test command: \n- Conventions: \n- Out of scope: \n\n\
         ## Acceptance hints\n- \n",
    );
    std::fs::write(&path, body).with_context(|| format!("writing ticket {}", path.display()))?;
    tracing::info!(slug = %slug, "ticket scaffolded from Slack slash command");
    Ok(())
}

/// Create a ticket from `/kranz ticket new`'s modal submission, through the
/// same primitive `POST /api/tickets` uses
/// (`kranz_engine::ticket::Ticket::scaffold`) rather than the bare-bones
/// template [`scaffold_ticket`] writes — this path already has a validated
/// slug, and an optional goal/context to seed the ticket body with.
fn create_ticket(
    repo_root: &Path,
    slug: &str,
    title: &str,
    goal: Option<&str>,
    context: Option<&str>,
) -> Result<()> {
    use kranz_engine::ticket::Ticket;
    Ticket::ensure_valid_slug(slug).with_context(|| format!("invalid ticket slug '{slug}'"))?;
    Ticket::scaffold(repo_root, slug, title, goal, context)
        .with_context(|| format!("scaffolding ticket '{slug}'"))?;
    tracing::info!(slug = %slug, "ticket created from Slack new-ticket modal");
    Ok(())
}

/// A filesystem-safe slug from a free-text title (lowercase, alnum + single
/// dashes). Mirrors the ticket file-stem convention.
fn slugify(title: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in title.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_end_matches('-').to_string();
    if trimmed.is_empty() {
        "ticket".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kranz_engine::queue;
    use std::sync::Once;
    use tempfile::TempDir;

    #[test]
    fn class_enabled_respects_flags() {
        let flags = NotifyFlags {
            plan_ready: false,
            blocked: true,
            complete: true,
            needs_context: true,
        };
        assert!(!class_enabled(&flags, NotifyClass::PlanReady));
        assert!(class_enabled(&flags, NotifyClass::Blocked));
        assert!(class_enabled(&flags, NotifyClass::Complete));
    }

    #[test]
    fn slugify_examples() {
        assert_eq!(
            slugify("Rate-limit the notes API"),
            "rate-limit-the-notes-api"
        );
        assert_eq!(slugify("  Fix   the  thing!! "), "fix-the-thing");
        assert_eq!(slugify("***"), "ticket");
    }

    #[test]
    fn approve_action_enqueues_mission() {
        let tmp = TempDir::new().unwrap();
        apply_action(
            tmp.path(),
            &Action::Approve {
                mission_id: "m-1".into(),
                user_id: None,
                response_url: None,
            },
        )
        .unwrap();
        assert!(queue::contains(tmp.path(), "m-1"));
    }

    #[test]
    fn guidance_action_enqueues_control_msg() {
        let tmp = TempDir::new().unwrap();
        apply_action(
            tmp.path(),
            &Action::Guidance {
                mission_id: "m-1".into(),
                text: "use a token bucket".into(),
                user_id: None,
            },
        )
        .unwrap();
        let paths = MissionPaths::new(tmp.path(), "m-1");
        let drained = kranz_engine::control::drain(&paths).unwrap();
        assert_eq!(drained.len(), 1);
        match &drained[0].1 {
            ControlCommand::Msg { text, interrupt } => {
                assert_eq!(text, "use a token bucket");
                assert!(!interrupt);
            }
            other => panic!("unexpected control command: {other:?}"),
        }
    }

    #[test]
    fn new_ticket_action_scaffolds_file() {
        let tmp = TempDir::new().unwrap();
        apply_action(
            tmp.path(),
            &Action::NewTicket {
                title: "Rate-limit the notes API".into(),
                channel: "C1".into(),
                thread_ts: None,
            },
        )
        .unwrap();
        let path = kranz_engine::ticket::Ticket::tickets_dir(tmp.path())
            .join("rate-limit-the-notes-api.md");
        assert!(path.exists());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("title: Rate-limit the notes API"));
        assert!(text.contains("## Goal"));
    }

    #[test]
    fn ignore_action_is_a_noop() {
        let tmp = TempDir::new().unwrap();
        apply_action(tmp.path(), &Action::Ignore).unwrap();
        assert!(queue::list(tmp.path()).is_empty());
    }

    #[test]
    fn async_handled_actions_are_noops_in_apply_action() {
        // Status / NewMission / RequestPlan / ApproveMission all reply over the
        // network in dispatch_action; apply_action must not double-handle them.
        let tmp = TempDir::new().unwrap();
        apply_action(
            tmp.path(),
            &Action::Status {
                mission_id: None,
                response_url: None,
            },
        )
        .unwrap();
        apply_action(
            tmp.path(),
            &Action::RequestPlan {
                mission_id: "m-1".into(),
                user_id: None,
                response_url: None,
            },
        )
        .unwrap();
        // Neither queued anything nor created a mission dir.
        assert!(queue::list(tmp.path()).is_empty());
    }

    fn test_cfg() -> SlackConfig {
        SlackConfig {
            bot_token: "xoxb".into(),
            app_token: "xapp".into(),
            channel: "C1".into(),
            notify: NotifyFlags::default(),
            allow_users: vec![],
            dashboard_url: None,
            instance_name: None,
        }
    }

    struct FailOncePoster {
        attempts: usize,
        classes: Vec<NotifyClass>,
    }

    impl FailOncePoster {
        fn new() -> Self {
            Self {
                attempts: 0,
                classes: Vec::new(),
            }
        }
    }

    impl OutboundPoster for FailOncePoster {
        fn post<'a>(
            &'a mut self,
            _cfg: &'a SlackConfig,
            _client: &'a SlackClient,
            _threads: &'a SharedThreads,
            _mission_id: &'a str,
            outbound: &'a Outbound,
        ) -> PostFuture<'a> {
            self.attempts += 1;
            self.classes.push(outbound.class());
            let fail = self.attempts == 1;
            Box::pin(async move {
                if fail {
                    Err(anyhow::anyhow!("temporary Slack failure"))
                } else {
                    Ok(())
                }
            })
        }
    }

    #[tokio::test]
    async fn outbound_cursor_retries_failed_post_before_advancing() {
        let tmp = TempDir::new().unwrap();
        let mission_id = "m-retry";
        seed_mission(tmp.path(), mission_id, "notify me");

        let cfg = test_cfg();
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        let mut cursors = HashMap::new();
        let mut poster = FailOncePoster::new();

        poll_mission_with_poster(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            mission_id,
            &mut cursors,
            &mut poster,
        )
        .await
        .unwrap();
        assert_eq!(poster.attempts, 0, "initial seed does not replay history");
        assert_eq!(cursors.get(mission_id).unwrap().last_seq, 1);

        append_plan_approved(tmp.path(), mission_id, "notify me");

        poll_mission_with_poster(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            mission_id,
            &mut cursors,
            &mut poster,
        )
        .await
        .unwrap();
        let cursor = cursors.get(mission_id).unwrap();
        assert_eq!(poster.attempts, 1);
        assert_eq!(poster.classes, vec![NotifyClass::PlanReady]);
        assert_eq!(cursor.last_seq, 1, "failed post leaves cursor retryable");
        assert_eq!(
            cursor.state.last_seq, 1,
            "failed post rolls folded state back with the cursor"
        );

        poll_mission_with_poster(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            mission_id,
            &mut cursors,
            &mut poster,
        )
        .await
        .unwrap();
        let cursor = cursors.get(mission_id).unwrap();
        assert_eq!(poster.attempts, 2);
        assert_eq!(
            poster.classes,
            vec![NotifyClass::PlanReady, NotifyClass::PlanReady]
        );
        assert_eq!(cursor.last_seq, 2);
        assert_eq!(cursor.state.last_seq, 2);
    }

    /// D-A: `is_ticket_slug` is the gate `Action::QueueTicket` uses to decide
    /// whether an arg is a backlog ticket at all.
    #[test]
    fn is_ticket_slug_true_for_on_disk_ticket_false_for_mission_id() {
        let tmp = TempDir::new().unwrap();
        scaffold_ticket(tmp.path(), "Rate-limit the notes API").unwrap();
        assert!(is_ticket_slug(tmp.path(), "rate-limit-the-notes-api"));
        assert!(!is_ticket_slug(tmp.path(), "m-42"));
    }

    /// D-A regression: `Action::QueueTicket` with a mission-id-shaped arg
    /// must be refused (queue is for tickets), and — critically — must NEVER
    /// fall through to `approve_flow`. Prove it by seeding a mission in the
    /// exact state `approve_flow` would happily queue (`Approved`, no host)
    /// and asserting the queue stays empty after dispatching `QueueTicket`.
    #[tokio::test]
    async fn queue_ticket_action_never_invokes_approve_flow_for_a_non_ticket_arg() {
        use kranz_engine::events::{Event, EventKind};
        use kranz_engine::types::{Plan, PlanFeature, PlanMilestone};

        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-a", "approved mission");
        let paths = MissionPaths::new(tmp.path(), "m-a");
        let event = Event {
            seq: 2,
            ts: chrono::Utc::now(),
            mission_id: "m-a".to_string(),
            kind: EventKind::PlanApproved {
                plan: Plan {
                    goal: "approved mission".into(),
                    validation_contract: vec![],
                    milestones: vec![PlanMilestone {
                        title: "M1".into(),
                        features: vec![PlanFeature {
                            title: "F1".into(),
                            spec: "s".into(),
                            validation_criteria: vec!["c".into()],
                        }],
                    }],
                    command_grants: vec![],
                    touch_set: vec![],
                },
                base_sha: None,
            },
        };
        let line = serde_json::to_string(&event).unwrap();
        let mut existing = std::fs::read_to_string(paths.events_file()).unwrap();
        existing.push_str(&line);
        existing.push('\n');
        std::fs::write(paths.events_file(), existing).unwrap();
        assert_eq!(
            mission_status(tmp.path(), "m-a").unwrap(),
            MissionStatus::Approved,
            "mission is exactly the state approve_flow would queue with no host"
        );
        assert!(
            !is_ticket_slug(tmp.path(), "m-a"),
            "m-a is not a ticket slug"
        );

        let cfg = test_cfg();
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        dispatch_action(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            None,
            &Action::QueueTicket {
                slug: "m-a".into(),
                user_id: None,
                response_url: None,
            },
        )
        .await;

        assert!(
            queue::list(tmp.path()).is_empty(),
            "QueueTicket with a non-ticket arg must never queue the mission via approve_flow"
        );
        assert_eq!(
            mission_status(tmp.path(), "m-a").unwrap(),
            MissionStatus::Approved,
            "mission state must be untouched by the refused queue attempt"
        );
    }

    /// Seed a mission's `events.jsonl` with a single `mission.created` event,
    /// built from public engine types so it folds exactly like a real log —
    /// no git, no backend. Returns the mission id.
    #[test]
    fn approved_card_carries_outcome_and_no_buttons() {
        let started = approved_card("m-9", Some("kranz/mission-m-9"), true);
        let text = serde_json::to_string(&started).unwrap();
        assert!(text.contains("approved &amp; started") || text.contains("approved & started"));
        assert!(text.contains("kranz/mission-m-9"));
        assert!(
            !text.contains("\"button\""),
            "outcome card must retire the buttons"
        );

        let queued = approved_card("m-9", None, false);
        let text = serde_json::to_string(&queued).unwrap();
        assert!(text.contains("approved & queued"));
        assert!(
            text.contains("kranz work"),
            "queue outcome points at the dispatcher"
        );
        assert!(
            !text.contains("branch"),
            "no branch line when branch is unknown"
        );
        assert!(!text.contains("\"button\""));
    }

    #[test]
    fn plan_json_heuristic_spots_chatted_plans_not_prose() {
        // Observed live (m-c9c915): the orchestrator chatted the full plan
        // JSON into the thread and said "approve this" — users need the
        // "run /kranz plan" nudge exactly then.
        assert!(looks_like_plan_json(
            r#"Here it is: {"goal":"x","validationContract":[],"milestones":[]}"#
        ));
        assert!(!looks_like_plan_json(
            "I'll draft milestones around the validation contract."
        ));
        assert!(!looks_like_plan_json(
            r#"the "milestones" key alone is not a plan"#
        ));
    }

    #[test]
    fn mission_status_folds_planning_and_post_approval() {
        use kranz_engine::events::{Event, EventKind};
        use kranz_engine::types::{Plan, PlanFeature, PlanMilestone};
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-p", "still planning");
        assert_eq!(
            mission_status(tmp.path(), "m-p").unwrap(),
            MissionStatus::Planning
        );

        // Append a PlanApproved: the reducer folds it to Approved (the run
        // loop hasn't started yet — Running is reserved for after a
        // milestone or worker spawns).
        seed_mission(tmp.path(), "m-a", "approved");
        let paths = MissionPaths::new(tmp.path(), "m-a");
        let event = Event {
            seq: 2,
            ts: chrono::Utc::now(),
            mission_id: "m-a".to_string(),
            kind: EventKind::PlanApproved {
                plan: Plan {
                    goal: "approved".into(),
                    validation_contract: vec![],
                    milestones: vec![PlanMilestone {
                        title: "M1".into(),
                        features: vec![PlanFeature {
                            title: "F1".into(),
                            spec: "s".into(),
                            validation_criteria: vec!["c".into()],
                        }],
                    }],
                    command_grants: vec![],
                    touch_set: vec![],
                },
                base_sha: None,
            },
        };
        let line = serde_json::to_string(&event).unwrap();
        let mut existing = std::fs::read_to_string(paths.events_file()).unwrap();
        existing.push_str(&line);
        existing.push('\n');
        std::fs::write(paths.events_file(), existing).unwrap();
        assert_eq!(
            mission_status(tmp.path(), "m-a").unwrap(),
            MissionStatus::Approved
        );

        assert!(
            mission_status(tmp.path(), "m-nope").is_err(),
            "unknown mission is an error"
        );
    }

    fn seed_mission(repo_root: &Path, mission_id: &str, goal: &str) {
        use kranz_engine::events::{Event, EventKind};
        use kranz_engine::types::MissionConfig;
        let paths = MissionPaths::new(repo_root, mission_id);
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        let event = Event {
            seq: 1,
            ts: chrono::Utc::now(),
            mission_id: mission_id.to_string(),
            kind: EventKind::MissionCreated {
                goal: goal.to_string(),
                base_branch: "main".into(),
                mission_branch: format!("kranz/mission-{mission_id}"),
                config: MissionConfig::default(),
            },
        };
        let line = serde_json::to_string(&event).unwrap();
        std::fs::write(paths.events_file(), format!("{line}\n")).unwrap();
    }

    fn sample_plan(goal: &str) -> kranz_engine::types::Plan {
        use kranz_engine::types::{Plan, PlanFeature, PlanMilestone};
        Plan {
            goal: goal.to_string(),
            validation_contract: vec![],
            milestones: vec![PlanMilestone {
                title: "M1".into(),
                features: vec![PlanFeature {
                    title: "F1".into(),
                    spec: "build it".into(),
                    validation_criteria: vec!["works".into()],
                }],
            }],
            command_grants: vec![],
            touch_set: vec![],
        }
    }

    fn append_plan_approved(repo_root: &Path, mission_id: &str, goal: &str) {
        use kranz_engine::event_log::LockForce;
        use kranz_engine::events::EventKind;

        let paths = MissionPaths::new(repo_root, mission_id);
        let mut log =
            EventLog::acquire(&paths, mission_id, Duration::from_millis(0), LockForce::No).unwrap();
        log.append(EventKind::PlanApproved {
            plan: sample_plan(goal),
            base_sha: None,
        })
        .unwrap();
    }

    /// Seed a mission that folds to [`MissionStatus::Complete`] — a `created`
    /// event followed by a `mission.completed` — so App Home's terminal-filter
    /// is exercised without a backend.
    fn seed_completed_mission(repo_root: &Path, mission_id: &str) {
        use kranz_engine::events::{Event, EventKind};
        use kranz_engine::types::MissionConfig;
        seed_mission(repo_root, mission_id, "goal");
        let paths = MissionPaths::new(repo_root, mission_id);
        let created = {
            let e = Event {
                seq: 1,
                ts: chrono::Utc::now(),
                mission_id: mission_id.to_string(),
                kind: EventKind::MissionCreated {
                    goal: "goal".into(),
                    base_branch: "main".into(),
                    mission_branch: format!("kranz/mission-{mission_id}"),
                    config: MissionConfig::default(),
                },
            };
            serde_json::to_string(&e).unwrap()
        };
        let completed = {
            let e = Event {
                seq: 2,
                ts: chrono::Utc::now(),
                mission_id: mission_id.to_string(),
                kind: EventKind::MissionCompleted {},
            };
            serde_json::to_string(&e).unwrap()
        };
        std::fs::write(paths.events_file(), format!("{created}\n{completed}\n")).unwrap();
    }

    fn basic_plan(goal: &str) -> kranz_engine::types::Plan {
        use kranz_engine::types::{Plan, PlanFeature, PlanMilestone};
        Plan {
            goal: goal.to_string(),
            validation_contract: vec![],
            milestones: vec![PlanMilestone {
                title: "M1".into(),
                features: vec![PlanFeature {
                    title: "F1".into(),
                    spec: "s".into(),
                    validation_criteria: vec!["c".into()],
                }],
            }],
            command_grants: vec![],
            touch_set: vec![],
        }
    }

    fn seed_running_mission(repo_root: &Path, mission_id: &str, goal: &str) {
        use kranz_engine::events::{Event, EventKind};
        seed_mission(repo_root, mission_id, goal);
        let paths = MissionPaths::new(repo_root, mission_id);
        let approved = Event {
            seq: 2,
            ts: chrono::Utc::now(),
            mission_id: mission_id.to_string(),
            kind: EventKind::PlanApproved {
                plan: basic_plan(goal),
                base_sha: None,
            },
        };
        let started = Event {
            seq: 3,
            ts: chrono::Utc::now(),
            mission_id: mission_id.to_string(),
            kind: EventKind::MilestoneStarted {
                milestone_id: "ms-1".into(),
                start_sha: "abc123".into(),
            },
        };
        let mut existing = std::fs::read_to_string(paths.events_file()).unwrap();
        existing.push_str(&serde_json::to_string(&approved).unwrap());
        existing.push('\n');
        existing.push_str(&serde_json::to_string(&started).unwrap());
        existing.push('\n');
        std::fs::write(paths.events_file(), existing).unwrap();
    }

    #[test]
    fn build_status_reply_folds_the_log() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-abc", "Rate-limit the notes API");
        let blocks = build_status_reply(tmp.path(), Some("m-abc")).unwrap();
        let text = serde_json::to_string(&blocks).unwrap();
        assert!(text.contains("m-abc"), "status carries the mission id");
        assert!(
            text.contains("Rate-limit the notes API"),
            "status carries the goal"
        );
        // A freshly-created mission is in Planning.
        assert!(
            text.contains("Planning"),
            "status pill reflects the folded state"
        );
    }

    #[test]
    fn build_status_reply_unknown_mission_is_error() {
        let tmp = TempDir::new().unwrap();
        let err = build_status_reply(tmp.path(), Some("m-nope"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("m-nope"), "error names the unknown mission");
    }

    #[test]
    fn build_status_reply_rejects_traversal_mission_id_before_paths() {
        let tmp = TempDir::new().unwrap();
        let victim = tmp.path().join("victim");
        let sibling = tmp.path().join("sibling");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::create_dir_all(&sibling).unwrap();
        seed_mission(&sibling, "m-secret", "do not disclose this mission");

        let err = build_status_reply(&victim, Some("../../../sibling/.kranz/missions/m-secret"))
            .unwrap_err()
            .to_string();

        assert!(err.contains("unknown mission"), "{err}");
        assert!(!err.contains("do not disclose"), "{err}");
    }

    #[test]
    fn build_status_reply_no_missions_is_error() {
        let tmp = TempDir::new().unwrap();
        // No mission id and no missions on disk → a helpful error.
        assert!(build_status_reply(tmp.path(), None).is_err());
    }

    #[test]
    fn most_recent_mission_picks_a_seeded_mission() {
        let tmp = TempDir::new().unwrap();
        // No missions → None.
        assert!(most_recent_mission(tmp.path()).is_none());
        seed_mission(tmp.path(), "m-one", "first goal");
        seed_mission(tmp.path(), "m-two", "second goal");
        // With missions present, a bare status resolves to one of them (which
        // exact one depends on filesystem mtime resolution, so don't pin it).
        let picked = most_recent_mission(tmp.path()).expect("a mission is picked");
        assert!(picked == "m-one" || picked == "m-two");
        // And build_status_reply(None) succeeds by folding that mission.
        assert!(build_status_reply(tmp.path(), None).is_ok());
    }

    #[test]
    fn status_word_reflects_mission_status() {
        assert_eq!(status_word(MissionStatus::Planning), "Planning");
        assert_eq!(status_word(MissionStatus::Complete), "Complete");
    }

    #[test]
    fn approved_status_word_reads_approved_not_running() {
        assert_eq!(status_word(MissionStatus::Approved), "Approved");
        assert_ne!(
            status_word(MissionStatus::Approved),
            status_word(MissionStatus::Running)
        );
    }

    #[test]
    fn pipeline_stage_derivation_mirrors_dashboard_reference_cases() {
        use crate::format::PipelineStage;
        use kranz_engine::ticket::TicketState;

        assert_eq!(
            pipeline_stage_for_ticket(TicketState::New, None),
            PipelineStage::Captured
        );
        assert_eq!(
            pipeline_stage_for_ticket(TicketState::NeedsContext, None),
            PipelineStage::NeedsYou
        );
        assert_eq!(
            pipeline_stage_for_ticket(TicketState::Review, None),
            PipelineStage::Reviewable
        );
        assert_eq!(
            pipeline_stage_for_ticket(TicketState::Queued, None),
            PipelineStage::Queued
        );
        assert_eq!(
            pipeline_stage_for_ticket(TicketState::Done, None),
            PipelineStage::Landed
        );

        let complete_unmerged = MissionProjection {
            id: "m-u".into(),
            status: MissionStageStatus::Known(MissionStatus::Complete),
            merged: Some(false),
            goal: "g".into(),
            cost_usd: None,
        };
        let complete_unknown = MissionProjection {
            merged: None,
            ..complete_unmerged.clone()
        };
        let complete_merged = MissionProjection {
            merged: Some(true),
            ..complete_unmerged.clone()
        };
        let abandoned = MissionProjection {
            status: MissionStageStatus::Known(MissionStatus::Abandoned),
            merged: None,
            ..complete_unmerged.clone()
        };

        assert_eq!(
            stage_from_mission(
                MissionStageStatus::Known(MissionStatus::Complete),
                Some(false)
            ),
            PipelineStage::Delivered
        );
        assert_eq!(
            stage_from_mission(MissionStageStatus::Known(MissionStatus::Complete), None),
            PipelineStage::Delivered
        );
        assert_eq!(
            stage_from_mission(
                MissionStageStatus::Known(MissionStatus::Complete),
                Some(true)
            ),
            PipelineStage::Landed
        );
        assert_eq!(
            stage_from_mission(MissionStageStatus::Known(MissionStatus::Planning), None),
            PipelineStage::Reviewable
        );
        assert_eq!(
            stage_from_mission(MissionStageStatus::Known(MissionStatus::Approved), None),
            PipelineStage::Queued
        );
        assert_eq!(
            stage_from_mission(MissionStageStatus::Deleted, None),
            PipelineStage::Abandoned
        );
        assert_eq!(
            pipeline_stage_for_ticket(TicketState::Done, Some(&complete_unmerged)),
            PipelineStage::Delivered
        );
        assert_eq!(
            pipeline_stage_for_ticket(TicketState::Done, Some(&complete_unknown)),
            PipelineStage::Delivered
        );
        assert_eq!(
            pipeline_stage_for_ticket(TicketState::Done, Some(&complete_merged)),
            PipelineStage::Landed
        );
        assert_eq!(
            pipeline_stage_for_ticket(TicketState::Done, Some(&abandoned)),
            PipelineStage::Landed
        );
        assert_eq!(
            pipeline_stage_for_ticket(TicketState::Review, Some(&complete_merged)),
            PipelineStage::Reviewable,
            "ticket head stages must ignore joined mission status"
        );
    }

    #[test]
    fn build_pipeline_status_reply_reports_counts_queue_running_and_unmerged() {
        use kranz_engine::ticket::{Ticket, TicketState};

        let tmp = TempDir::new().unwrap();
        Ticket::scaffold(tmp.path(), "captured", "Captured ticket", None, None).unwrap();
        Ticket::scaffold(tmp.path(), "reviewable", "Reviewable ticket", None, None).unwrap();
        Ticket::write_state(tmp.path(), "reviewable", TicketState::Review, None).unwrap();
        Ticket::scaffold(tmp.path(), "needs-you", "Needs context ticket", None, None).unwrap();
        Ticket::write_state(tmp.path(), "needs-you", TicketState::NeedsContext, None).unwrap();

        seed_running_mission(tmp.path(), "m-run", "Running mission");
        seed_completed_mission(tmp.path(), "m-delivered");
        queue::enqueue(
            tmp.path(),
            queue::QueueEntry {
                mission_id: "m-queued".into(),
                ticket_slug: None,
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();

        let blocks = build_pipeline_status_reply(tmp.path());
        let text = serde_json::to_string(&blocks).unwrap();
        assert!(text.contains("m-run"), "running mission included");
        assert!(text.contains("1 waiting"), "queue depth included");
        assert!(text.contains("captured 1"), "captured count included");
        assert!(text.contains("needs-you 1"), "needs-you count included");
        assert!(
            text.contains("reviewable 1"),
            "reviewable ticket count included"
        );
        assert!(text.contains("running 1"), "running count included");
        assert!(text.contains("delivered 1"), "delivered count included");
        assert!(
            text.contains("m-delivered"),
            "delivered-but-unmerged set included"
        );
    }

    #[test]
    fn build_todo_reply_lists_pipeline_actions_and_tracked_gates() {
        use kranz_engine::ticket::{Ticket, TicketState};

        let tmp = TempDir::new().unwrap();
        Ticket::scaffold(tmp.path(), "reviewable", "Approve plan", None, None).unwrap();
        Ticket::write_state(tmp.path(), "reviewable", TicketState::Review, None).unwrap();
        Ticket::scaffold(tmp.path(), "needs-you", "Answer questions", None, None).unwrap();
        Ticket::write_state(tmp.path(), "needs-you", TicketState::NeedsContext, None).unwrap();
        seed_completed_mission(tmp.path(), "m-delivered");

        let docs = tmp.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(
            docs.join("operator-gates.md"),
            "# Gates\n\n- [ ] repo public + history scrub\n- [ ] M6 live deploy\n",
        )
        .unwrap();

        let blocks = build_todo_reply(tmp.path(), Some("http://127.0.0.1:4600"));
        let text = serde_json::to_string(&blocks).unwrap();
        assert!(text.contains("PIPELINE ACTIONS"));
        assert!(text.contains("reviewable"));
        assert!(text.contains("needs-you"));
        assert!(text.contains("m-delivered"));
        assert!(text.contains(crate::format::QUEUE_TICKET_ACTION_ID));
        assert!(text.contains(crate::format::MERGE_ACTION_ID));
        assert!(text.contains("http://127.0.0.1:4600/#/backlog/needs-you"));
        assert!(text.contains("repo public + history scrub"));
        assert!(text.contains("M6 live deploy"));
    }

    #[test]
    fn config_change_enqueues_camelcase_patch_on_the_mission() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-cfg", "goal");
        // scrutiny → validatorScrutiny, with effort → reasoningEffort.
        let applied =
            config_change(tmp.path(), Some("m-cfg"), "scrutiny", "opus", Some("high")).unwrap();
        assert_eq!(applied, "m-cfg");
        let paths = MissionPaths::new(tmp.path(), "m-cfg");
        let drained = kranz_engine::control::drain(&paths).unwrap();
        assert_eq!(drained.len(), 1);
        match &drained[0].1 {
            ControlCommand::ConfigChange { patch } => {
                assert_eq!(
                    *patch,
                    json!({ "validatorScrutiny": { "model": "opus", "reasoningEffort": "high" } })
                );
            }
            other => panic!("expected ConfigChange, got {other:?}"),
        }
    }

    #[test]
    fn config_change_without_id_targets_most_recent_and_omits_effort() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-only", "goal");
        let applied = config_change(tmp.path(), None, "worker", "sonnet", None).unwrap();
        assert_eq!(applied, "m-only");
        let drained =
            kranz_engine::control::drain(&MissionPaths::new(tmp.path(), "m-only")).unwrap();
        match &drained[0].1 {
            ControlCommand::ConfigChange { patch } => {
                assert_eq!(*patch, json!({ "worker": { "model": "sonnet" } }));
            }
            other => panic!("expected ConfigChange, got {other:?}"),
        }
    }

    #[test]
    fn config_change_unknown_mission_is_error() {
        let tmp = TempDir::new().unwrap();
        let err = config_change(tmp.path(), Some("m-nope"), "worker", "sonnet", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("m-nope"), "error names the unknown mission");
    }

    #[test]
    fn config_change_refuses_a_bare_command_when_several_missions_are_active() {
        // Two active (Planning) missions: a bare `/kranz config` must NOT guess
        // (an mtime race would otherwise hijack a running mission) — it errors
        // and asks for an explicit id, and enqueues nothing.
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-a", "goal a");
        seed_mission(tmp.path(), "m-b", "goal b");
        let err = config_change(tmp.path(), None, "worker", "opus", None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("several active missions"),
            "asks for an explicit id: {err}"
        );
        for id in ["m-a", "m-b"] {
            assert!(
                kranz_engine::control::drain(&MissionPaths::new(tmp.path(), id))
                    .unwrap()
                    .is_empty(),
                "nothing enqueued on {id}"
            );
        }
    }

    #[test]
    fn config_change_refuses_a_terminal_mission_and_enqueues_nothing() {
        // A completed mission's control inbox is never drained, so a config
        // change there would be a silent no-op reported as success. Reject it.
        let tmp = TempDir::new().unwrap();
        seed_completed_mission(tmp.path(), "m-done");
        let err = config_change(tmp.path(), Some("m-done"), "worker", "opus", None)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("active missions"),
            "honest error, not false success: {err}"
        );
        assert!(
            kranz_engine::control::drain(&MissionPaths::new(tmp.path(), "m-done"))
                .unwrap()
                .is_empty(),
            "no control file leaked into a terminal mission"
        );
    }

    // --- /kranz pause | resume (steering via the control inbox) ------------

    #[test]
    fn enqueue_steer_enqueues_exactly_one_pause_on_the_resolved_mission() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-steer", "goal");
        // Explicit id, active mission → one Pause on that mission.
        let applied = enqueue_steer(tmp.path(), Some("m-steer"), ControlCommand::Pause).unwrap();
        assert_eq!(applied, "m-steer");
        let drained =
            kranz_engine::control::drain(&MissionPaths::new(tmp.path(), "m-steer")).unwrap();
        assert_eq!(drained.len(), 1, "exactly one control command enqueued");
        assert!(
            matches!(drained[0].1, ControlCommand::Pause),
            "the command is Pause, got {:?}",
            drained[0].1
        );
    }

    #[test]
    fn enqueue_steer_bare_resume_targets_the_single_active_mission() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-only", "goal");
        // No id, exactly one active mission → resolved to it; one Resume enqueued.
        let applied = enqueue_steer(tmp.path(), None, ControlCommand::Resume).unwrap();
        assert_eq!(applied, "m-only");
        let drained =
            kranz_engine::control::drain(&MissionPaths::new(tmp.path(), "m-only")).unwrap();
        assert_eq!(drained.len(), 1);
        assert!(
            matches!(drained[0].1, ControlCommand::Resume),
            "got {:?}",
            drained[0].1
        );
    }

    #[test]
    fn enqueue_steer_refuses_ambiguous_target_and_enqueues_nothing() {
        // Two active missions, no explicit id → error asking for an id; NOTHING
        // enqueued on either (never silently guess which running mission to pause).
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-a", "goal a");
        seed_mission(tmp.path(), "m-b", "goal b");
        let err = enqueue_steer(tmp.path(), None, ControlCommand::Pause)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("several active missions"),
            "asks for an explicit id: {err}"
        );
        for id in ["m-a", "m-b"] {
            assert!(
                kranz_engine::control::drain(&MissionPaths::new(tmp.path(), id))
                    .unwrap()
                    .is_empty(),
                "nothing enqueued on {id}"
            );
        }
    }

    #[test]
    fn enqueue_steer_refuses_a_terminal_mission_and_enqueues_nothing() {
        // A completed mission's control inbox is never drained, so a Pause there
        // would be a silent no-op reported as success. Reject it, enqueue nothing.
        let tmp = TempDir::new().unwrap();
        seed_completed_mission(tmp.path(), "m-done");
        let err = enqueue_steer(tmp.path(), Some("m-done"), ControlCommand::Resume)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("active missions"),
            "honest error, not false success: {err}"
        );
        assert!(
            kranz_engine::control::drain(&MissionPaths::new(tmp.path(), "m-done"))
                .unwrap()
                .is_empty(),
            "no control file leaked into a terminal mission"
        );
    }

    #[test]
    fn enqueue_steer_unknown_mission_is_an_error() {
        let tmp = TempDir::new().unwrap();
        let err = enqueue_steer(tmp.path(), Some("m-nope"), ControlCommand::Pause)
            .unwrap_err()
            .to_string();
        assert!(err.contains("m-nope"), "error names the unknown mission");
    }

    #[tokio::test]
    async fn steer_denies_an_unlisted_user_and_enqueues_nothing() {
        // A non-empty allowlist gates pause/resume exactly like config: an
        // unlisted user is refused and NO control command is enqueued. Passing
        // response_url = None keeps reply_ephemeral a no-op (no network).
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-gated", "goal");
        let cfg = SlackConfig {
            bot_token: "xoxb".into(),
            app_token: "xapp".into(),
            channel: "C1".into(),
            notify: NotifyFlags::default(),
            allow_users: vec!["U-allowed".into()],
            dashboard_url: None,
            instance_name: None,
        };
        let client = SlackClient::new(&cfg).unwrap();
        steer(
            &cfg,
            &client,
            tmp.path(),
            Some("m-gated"),
            Some("U-outsider"),
            None,
            ControlCommand::Pause,
            "paused",
        )
        .await;
        assert!(
            kranz_engine::control::drain(&MissionPaths::new(tmp.path(), "m-gated"))
                .unwrap()
                .is_empty(),
            "an unlisted user's pause enqueues nothing"
        );
    }

    #[tokio::test]
    async fn steer_allows_a_listed_user_and_enqueues_the_command() {
        // The gate's positive half: a listed user's pause is enqueued.
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-ok", "goal");
        let cfg = SlackConfig {
            bot_token: "xoxb".into(),
            app_token: "xapp".into(),
            channel: "C1".into(),
            notify: NotifyFlags::default(),
            allow_users: vec!["U-allowed".into()],
            dashboard_url: None,
            instance_name: None,
        };
        let client = SlackClient::new(&cfg).unwrap();
        steer(
            &cfg,
            &client,
            tmp.path(),
            Some("m-ok"),
            Some("U-allowed"),
            None,
            ControlCommand::Resume,
            "resumed",
        )
        .await;
        let drained = kranz_engine::control::drain(&MissionPaths::new(tmp.path(), "m-ok")).unwrap();
        assert_eq!(drained.len(), 1, "listed user's resume is enqueued");
        assert!(
            matches!(drained[0].1, ControlCommand::Resume),
            "got {:?}",
            drained[0].1
        );
    }

    #[test]
    fn pause_resume_work_are_noops_in_apply_action() {
        // These reply over the network in dispatch_action; apply_action must not
        // double-handle them (no control file, no side effects).
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-1", "goal");
        for action in [
            Action::Pause {
                mission_id: Some("m-1".into()),
                user_id: None,
                response_url: None,
            },
            Action::Resume {
                mission_id: Some("m-1".into()),
                user_id: None,
                response_url: None,
            },
            Action::Work { response_url: None },
        ] {
            apply_action(tmp.path(), &action).unwrap();
        }
        assert!(
            kranz_engine::control::drain(&MissionPaths::new(tmp.path(), "m-1"))
                .unwrap()
                .is_empty(),
            "apply_action is inert for pause/resume/work"
        );
    }

    // --- /kranz work (report-only queue read) ------------------------------

    #[test]
    fn build_work_reply_reports_empty_queue_and_points_at_the_dispatcher() {
        let tmp = TempDir::new().unwrap();
        let blocks = build_work_reply(tmp.path());
        let text = serde_json::to_string(&blocks).unwrap();
        assert!(text.to_lowercase().contains("queue is empty"));
        assert!(text.contains("kranz work"), "points at the dispatcher");
        assert!(text.to_lowercase().contains("no mission is running"));
    }

    #[test]
    fn build_work_reply_lists_queued_entries() {
        let tmp = TempDir::new().unwrap();
        queue::enqueue(
            tmp.path(),
            queue::QueueEntry {
                mission_id: "m-q1".into(),
                ticket_slug: None,
                priority: 1,
                seq: 0,
            },
        )
        .unwrap();
        queue::enqueue(
            tmp.path(),
            queue::QueueEntry {
                mission_id: "m-q2".into(),
                ticket_slug: None,
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();
        let blocks = build_work_reply(tmp.path());
        let text = serde_json::to_string(&blocks).unwrap();
        assert!(
            text.contains("m-q1") && text.contains("m-q2"),
            "both queued missions listed"
        );
        assert!(text.contains("2 waiting"), "queue depth reported");
        // is_repo_busy is None here (no live lock), so it reports no running mission.
        assert!(text.to_lowercase().contains("no mission is running"));
    }

    #[test]
    fn config_action_is_a_noop_in_apply_action() {
        // Config replies over the network in dispatch_action; apply_action must
        // not double-handle it (no control file, no mission dir).
        let tmp = TempDir::new().unwrap();
        apply_action(
            tmp.path(),
            &Action::Config {
                mission_id: Some("m-1".into()),
                role: "worker".into(),
                model: "sonnet".into(),
                effort: None,
                user_id: None,
                response_url: None,
                channel: None,
            },
        )
        .unwrap();
        // Nothing was enqueued (apply_action is inert for Config).
        assert!(!MissionPaths::new(tmp.path(), "m-1").control_dir().exists());
    }

    #[test]
    fn build_home_view_folds_missions_queue_and_tickets() {
        let tmp = TempDir::new().unwrap();
        // Two missions (both Planning → both active), one queued, one ticket.
        seed_mission(tmp.path(), "m-a", "goal a");
        seed_mission(tmp.path(), "m-b", "goal b");
        queue::enqueue(
            tmp.path(),
            queue::QueueEntry {
                mission_id: "m-b".into(),
                ticket_slug: None,
                priority: 2,
                seq: 0,
            },
        )
        .unwrap();
        kranz_engine::ticket::Ticket::write_state(
            tmp.path(),
            "rate-limit",
            kranz_engine::ticket::TicketState::New,
            None,
        )
        .unwrap();
        // A ticket .md is needed for Ticket::list to surface it.
        let tdir = kranz_engine::ticket::Ticket::tickets_dir(tmp.path());
        std::fs::write(
            tdir.join("rate-limit.md"),
            "---\ntitle: Rate-limit the notes API\n---\n\n## Goal\nx\n",
        )
        .unwrap();

        let view = build_home_view(tmp.path(), Some("http://127.0.0.1:4600"));
        let s = serde_json::to_string(&view).unwrap();
        assert_eq!(view["type"], "home");
        assert!(
            s.contains("m-a") && s.contains("m-b"),
            "active missions present"
        );
        assert!(s.contains("Planning"), "status pill present");
        assert!(
            s.contains("Rate-limit the notes API"),
            "open ticket title present"
        );
        assert!(
            s.contains("m/m-a"),
            "deep link present when dashboard configured"
        );
    }

    #[test]
    fn build_home_view_excludes_terminal_missions_and_done_tickets() {
        let tmp = TempDir::new().unwrap();
        // A mission driven to Complete via mission.completed.
        seed_completed_mission(tmp.path(), "m-done");
        // A ticket in the Done terminal state.
        let tdir = kranz_engine::ticket::Ticket::tickets_dir(tmp.path());
        std::fs::create_dir_all(&tdir).unwrap();
        std::fs::write(
            tdir.join("finished.md"),
            "---\ntitle: Finished\n---\n\n## Goal\nx\n",
        )
        .unwrap();
        kranz_engine::ticket::Ticket::write_state(
            tmp.path(),
            "finished",
            kranz_engine::ticket::TicketState::Done,
            None,
        )
        .unwrap();

        let view = build_home_view(tmp.path(), None);
        let blocks = view["blocks"].as_array().unwrap();
        let text = {
            let mut out = String::new();
            for b in blocks {
                if let Some(t) = b.pointer("/text/text").and_then(Value::as_str) {
                    out.push_str(t);
                    out.push('\n');
                }
                if let Some(elems) = b["elements"].as_array() {
                    for e in elems {
                        if let Some(t) = e["text"].as_str() {
                            out.push_str(t);
                            out.push('\n');
                        }
                    }
                }
            }
            out
        };
        assert!(
            !text.contains("m-done"),
            "terminal mission excluded from active list"
        );
        assert!(
            !text.contains("Finished"),
            "done ticket excluded from open tickets"
        );
        assert!(text.to_lowercase().contains("no active missions"));
        assert!(text.to_lowercase().contains("no open tickets"));
    }

    #[tokio::test]
    async fn handle_envelope_acks_with_envelope_id() {
        let tmp = TempDir::new().unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        let cfg = SlackConfig {
            bot_token: "xoxb".into(),
            app_token: "xapp".into(),
            channel: "C1".into(),
            notify: NotifyFlags::default(),
            allow_users: vec![],
            dashboard_url: None,
            instance_name: None,
        };
        let client = SlackClient::new(&cfg).unwrap();
        let frame = json!({
            "type": "slash_commands",
            "envelope_id": "env-xyz",
            "payload": { "command": "/kranz", "text": "ticket Fix it", "channel_id": "C1" }
        })
        .to_string();
        let ack = handle_envelope(&cfg, &client, tmp.path(), &threads, &frame)
            .await
            .unwrap();
        let parsed: Value = serde_json::from_str(&ack).unwrap();
        assert_eq!(parsed["envelope_id"], "env-xyz");
        // And the side effect happened.
        assert!(kranz_engine::ticket::Ticket::tickets_dir(tmp.path())
            .join("fix-it.md")
            .exists());
    }

    #[tokio::test]
    async fn handle_envelope_without_id_returns_none() {
        let tmp = TempDir::new().unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        let cfg = SlackConfig {
            bot_token: "xoxb".into(),
            app_token: "xapp".into(),
            channel: "C1".into(),
            notify: NotifyFlags::default(),
            allow_users: vec![],
            dashboard_url: None,
            instance_name: None,
        };
        let client = SlackClient::new(&cfg).unwrap();
        let hello = json!({ "type": "hello" }).to_string();
        assert!(handle_envelope(&cfg, &client, tmp.path(), &threads, &hello)
            .await
            .is_none());
    }

    #[test]
    fn slow_actions_are_the_claude_spawning_ones() {
        // NewMission / RequestPlan spawn a turn → must run off the read loop.
        assert!(is_slow_action(&Action::NewMission {
            goal: "g".into(),
            user_id: None,
            response_url: None,
            channel: "C1".into(),
        }));
        assert!(is_slow_action(&Action::RequestPlan {
            mission_id: "m-1".into(),
            user_id: None,
            response_url: None,
        }));
        // Engine-touching actions moved off the read loop with the hosted
        // registry: approvals commit files under the engine mutex, and a
        // thread reply on a planning mission runs a full planning turn.
        assert!(is_slow_action(&Action::Approve {
            mission_id: "m-1".into(),
            user_id: None,
            response_url: None,
        }));
        assert!(is_slow_action(&Action::ApproveStart {
            mission_id: "m-1".into(),
            user_id: None,
            response_url: None,
        }));
        assert!(is_slow_action(&Action::Guidance {
            mission_id: "m-1".into(),
            text: "hi".into(),
            user_id: None,
        }));
        // Draft runs a full mission create+seed+drive turn → must run off the
        // read loop, same as NewMission.
        assert!(is_slow_action(&Action::Draft {
            slug: "s-1".into(),
            user_id: None,
            response_url: None,
        }));
        // WorkRun triggers a live-host drain (spawns work) → must run off the
        // read loop, same as Draft.
        assert!(is_slow_action(&Action::WorkRun {
            user_id: None,
            response_url: None,
        }));
        // Fast local/one-call actions stay inline.
        assert!(!is_slow_action(&Action::Status {
            mission_id: None,
            response_url: None
        }));
        assert!(!is_slow_action(&Action::Ignore));
    }

    #[test]
    fn seen_envelopes_dedups_and_bounds() {
        let mut seen = SeenEnvelopes::default();
        assert!(seen.insert("env-a"), "first sighting is new");
        assert!(!seen.insert("env-a"), "redelivery is not new");
        assert!(seen.insert("env-b"));
        // Overflow the cap; the oldest id is evicted and can be re-seen.
        for i in 0..SeenEnvelopes::CAP {
            seen.insert(&format!("fill-{i}"));
        }
        assert!(seen.insert("env-a"), "evicted id counts as new again");
        assert!(seen.set.len() <= SeenEnvelopes::CAP + 1);
    }

    async fn pump_one_frame(
        cfg: &SlackConfig,
        client: &SlackClient,
        repo_root: &Path,
        threads: &SharedThreads,
        seen: &mut SeenEnvelopes,
        frame: Value,
    ) {
        let stop = Arc::new(Notify::new());
        let health = BridgeHealth::new();
        let mut read = futures_util::stream::iter(vec![Ok::<Message, std::convert::Infallible>(
            Message::Text(frame.to_string()),
        )]);
        let mut write = futures_util::sink::drain::<Message>();

        let result = pump_connection(
            &mut read, &mut write, cfg, client, repo_root, threads, &None, &stop, seen, &health,
        )
        .await;
        assert!(
            matches!(result, Ok(false)),
            "expected stream close after one frame, got {result:?}"
        );
    }

    fn guidance_envelope(envelope_id: &str) -> Value {
        json!({
            "type": "events_api",
            "envelope_id": envelope_id,
            "payload": {
                "event": {
                    "type": "message",
                    "channel": "C1",
                    "user": "U1",
                    "text": "please adjust",
                    "thread_ts": "111.111",
                    "ts": "111.222"
                }
            }
        })
    }

    async fn drain_control_after_spawn(
        repo_root: &Path,
        mission_id: &str,
    ) -> Vec<(PathBuf, ControlCommand)> {
        let paths = MissionPaths::new(repo_root, mission_id);
        for _ in 0..50 {
            let drained = kranz_engine::control::drain(&paths).unwrap();
            if !drained.is_empty() {
                return drained;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        Vec::new()
    }

    #[tokio::test]
    async fn pump_connection_dedups_envelopes_across_reconnects() {
        let tmp = TempDir::new().unwrap();
        let mission_id = "m-reconnect";
        seed_mission(tmp.path(), mission_id, "steer me");
        append_plan_approved(tmp.path(), mission_id, "steer me");

        let cfg = test_cfg();
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        threads.set(mission_id, "111.111");
        let mut seen = SeenEnvelopes::default();

        pump_one_frame(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            &mut seen,
            guidance_envelope("env-redeliver"),
        )
        .await;
        let drained = drain_control_after_spawn(tmp.path(), mission_id).await;
        assert_eq!(drained.len(), 1, "first delivery enqueues guidance");
        match &drained[0].1 {
            ControlCommand::Msg { text, .. } => assert_eq!(text, "please adjust"),
            other => panic!("expected guidance message, got {other:?}"),
        }
        assert!(seen.set.contains("env-redeliver"));
        for (path, _) in drained {
            std::fs::remove_file(path).unwrap();
        }

        pump_one_frame(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            &mut seen,
            guidance_envelope("env-redeliver"),
        )
        .await;
        tokio::task::yield_now().await;
        let drained =
            kranz_engine::control::drain(&MissionPaths::new(tmp.path(), mission_id)).unwrap();
        assert!(
            drained.is_empty(),
            "redelivery after reconnect must be acked but not dispatched"
        );
    }

    #[test]
    fn is_disconnect_frame_true_for_disconnect_false_for_action_envelope() {
        assert!(is_disconnect_frame(
            r#"{"type":"disconnect","reason":"refresh_requested"}"#
        ));
        assert!(is_disconnect_frame(
            r#"{"type":"disconnect","reason":"warning"}"#
        ));
        // A real Socket Mode envelope, e.g. a slash_commands frame — must NOT
        // be misclassified as a disconnect.
        assert!(!is_disconnect_frame(
            r#"{"envelope_id":"1","type":"slash_commands","payload":{}}"#
        ));
        assert!(!is_disconnect_frame("not json at all"));
    }

    /// No frames ever arrive: `pump_connection` must not park forever on a
    /// silently-dead TCP connection. Advancing virtual time past
    /// `IDLE_TIMEOUT` must make it return `Ok(false)` so the caller reconnects.
    #[tokio::test(start_paused = true)]
    async fn idle_timeout_triggers_reconnect() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_cfg();
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        let stop = Arc::new(Notify::new());
        let mut seen = SeenEnvelopes::default();
        let health = BridgeHealth::new();

        let mut read = futures_util::stream::pending::<
            std::result::Result<Message, std::convert::Infallible>,
        >();
        let mut write = futures_util::sink::drain::<Message>();

        let handle = tokio::spawn(async move {
            pump_connection(
                &mut read,
                &mut write,
                &cfg,
                &client,
                tmp.path(),
                &threads,
                &None,
                &stop,
                &mut seen,
                &health,
            )
            .await
        });

        tokio::time::advance(IDLE_TIMEOUT + Duration::from_secs(1)).await;
        let result = handle.await.unwrap();
        assert!(
            matches!(result, Ok(false)),
            "expected Ok(false), got {result:?}"
        );
    }

    /// A `{"type":"disconnect"}` frame must short-circuit the loop with
    /// `Ok(false)` (reconnect) WITHOUT being routed/dispatched as an action —
    /// it carries no `envelope_id` and today's `route()` would otherwise
    /// silently classify it `Action::Ignore` and keep pumping a doomed socket.
    #[tokio::test]
    async fn disconnect_frame_triggers_reconnect() {
        let tmp = TempDir::new().unwrap();
        let cfg = test_cfg();
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        let stop = Arc::new(Notify::new());
        let mut seen = SeenEnvelopes::default();
        let health = BridgeHealth::new();

        let frame = Message::Text(r#"{"type":"disconnect","reason":"refresh_requested"}"#.into());
        let mut read =
            futures_util::stream::iter(vec![Ok::<Message, std::convert::Infallible>(frame)]);
        let mut write = futures_util::sink::drain::<Message>();

        let result = pump_connection(
            &mut read,
            &mut write,
            &cfg,
            &client,
            tmp.path(),
            &threads,
            &None,
            &stop,
            &mut seen,
            &health,
        )
        .await;
        assert!(
            matches!(result, Ok(false)),
            "expected Ok(false), got {result:?}"
        );
        // Nothing was acked or dedup-recorded: the disconnect frame carries no
        // envelope_id, and it must never reach `parse_envelope`/dispatch.
        assert!(seen.set.is_empty());
    }

    // -----------------------------------------------------------------
    // slack_ticket_delivered_landed — Delivered/Landed split on the Slack
    // ticket list/show renderers, mirroring
    // `kranz_cli::backlog`'s `cli_ticket_delivered_landed_*` tests so the
    // Slack surface can never drift from the CLI/REST projection.
    // -----------------------------------------------------------------

    static GIT_ENV_ISOLATION: Once = Once::new();

    /// Mask the host's global/system git config so identity, signing, and
    /// hooks never leak into the throwaway repos (mirrors
    /// `crates/engine/tests/merged_test.rs::isolate_git_env`).
    fn isolate_git_env() {
        GIT_ENV_ISOLATION.call_once(|| {
            let missing = std::env::temp_dir().join(format!(
                "kranz-slack-bridge-test-no-config-{}",
                std::process::id()
            ));
            std::env::set_var("GIT_CONFIG_GLOBAL", &missing);
            std::env::set_var("GIT_CONFIG_SYSTEM", &missing);
            if let Ok(ceiling) = std::fs::canonicalize(std::env::temp_dir()) {
                std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling);
            }
        });
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn setup_git() -> bool {
        isolate_git_env();
        if git_available() {
            true
        } else {
            eprintln!("skipping test: git is not on PATH");
            false
        }
    }

    fn raw_git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Fresh repo on branch `main` with one seed commit; returns
    /// (tempdir, canonicalized root, seed commit sha).
    fn init_git_repo() -> (TempDir, PathBuf, String) {
        let dir = TempDir::new().unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(dir.path())
            .output()
            .expect("spawn git init");
        if !init.status.success() {
            raw_git(dir.path(), &["init"]);
            raw_git(dir.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);
        }
        raw_git(dir.path(), &["config", "user.name", "test"]);
        raw_git(dir.path(), &["config", "user.email", "test@example.com"]);
        std::fs::write(dir.path().join("README.md"), "seed\n").unwrap();
        raw_git(dir.path(), &["add", "-A"]);
        raw_git(dir.path(), &["commit", "-m", "seed"]);
        let root = std::fs::canonicalize(dir.path()).expect("canonicalize repo root");
        let sha = {
            let out = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&root)
                .output()
                .expect("rev-parse HEAD");
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        (dir, root, sha)
    }

    /// Create a Done ticket linked to `mission_id`, with the mission's branch
    /// branched off `base_sha` and (optionally) merged back into `main`
    /// before the mission's events are written as Complete.
    fn scaffold_done_ticket_with_mission(
        repo_root: &Path,
        slug: &str,
        mission_id: &str,
        base_sha: &str,
        merge_into_base: bool,
    ) {
        use kranz_engine::ticket::{Ticket, TicketState};
        Ticket::scaffold(repo_root, slug, "fixture ticket", None, None).unwrap();
        Ticket::write_state(repo_root, slug, TicketState::Done, None).unwrap();
        Ticket::record_mission(repo_root, slug, mission_id).unwrap();

        let branch = format!("kranz/mission-{mission_id}");
        raw_git(repo_root, &["checkout", "-b", &branch, base_sha]);
        std::fs::write(repo_root.join("feature.txt"), "new feature\n").unwrap();
        raw_git(repo_root, &["add", "--", "feature.txt"]);
        raw_git(repo_root, &["commit", "-m", "add feature"]);
        raw_git(repo_root, &["checkout", "main"]);
        if merge_into_base {
            raw_git(repo_root, &["merge", "--no-ff", "--no-edit", &branch]);
        }

        seed_completed_mission_on_branch(repo_root, mission_id, &branch);
    }

    /// Like [`seed_completed_mission`], but with a caller-chosen mission
    /// branch (so it lines up with the branch actually created in git).
    fn seed_completed_mission_on_branch(repo_root: &Path, mission_id: &str, branch: &str) {
        use kranz_engine::events::{Event, EventKind};
        use kranz_engine::types::MissionConfig;
        let paths = MissionPaths::new(repo_root, mission_id);
        std::fs::create_dir_all(paths.mission_dir()).unwrap();
        let created = Event {
            seq: 1,
            ts: chrono::Utc::now(),
            mission_id: mission_id.to_string(),
            kind: EventKind::MissionCreated {
                goal: "goal".into(),
                base_branch: "main".into(),
                mission_branch: branch.to_string(),
                config: MissionConfig::default(),
            },
        };
        let completed = Event {
            seq: 2,
            ts: chrono::Utc::now(),
            mission_id: mission_id.to_string(),
            kind: EventKind::MissionCompleted {},
        };
        std::fs::write(
            paths.events_file(),
            format!(
                "{}\n{}\n",
                serde_json::to_string(&created).unwrap(),
                serde_json::to_string(&completed).unwrap()
            ),
        )
        .unwrap();
    }

    #[test]
    fn slack_ticket_delivered_landed_when_done_and_unmerged() {
        if !setup_git() {
            return;
        }
        let (_dir, repo_root, base_sha) = init_git_repo();
        scaffold_done_ticket_with_mission(&repo_root, "unmerged", "m-unmerged", &base_sha, false);

        let list_text = serde_json::to_string(&build_ticket_list_reply(&repo_root)).unwrap();
        assert!(
            list_text.contains("Delivered"),
            "ticket list should show Delivered for a Done+unmerged ticket: {list_text}"
        );

        let show_text =
            serde_json::to_string(&build_ticket_show_reply(&repo_root, "unmerged")).unwrap();
        assert!(
            show_text.contains("Delivered"),
            "ticket show should show Delivered for a Done+unmerged ticket: {show_text}"
        );
    }

    #[test]
    fn slack_ticket_delivered_landed_when_done_and_merged() {
        if !setup_git() {
            return;
        }
        let (_dir, repo_root, base_sha) = init_git_repo();
        scaffold_done_ticket_with_mission(&repo_root, "merged", "m-merged", &base_sha, true);

        let list_text = serde_json::to_string(&build_ticket_list_reply(&repo_root)).unwrap();
        assert!(
            list_text.contains("Landed"),
            "ticket list should show Landed for a Done+merged ticket: {list_text}"
        );

        let show_text =
            serde_json::to_string(&build_ticket_show_reply(&repo_root, "merged")).unwrap();
        assert!(
            show_text.contains("Landed"),
            "ticket show should show Landed for a Done+merged ticket: {show_text}"
        );
    }

    #[test]
    fn slack_ticket_delivered_landed_when_done_and_no_mission() {
        if !setup_git() {
            return;
        }
        let (_dir, repo_root, _base_sha) = init_git_repo();
        use kranz_engine::ticket::{Ticket, TicketState};
        Ticket::scaffold(&repo_root, "no-mission", "fixture ticket", None, None).unwrap();
        Ticket::write_state(&repo_root, "no-mission", TicketState::Done, None).unwrap();

        let list_text = serde_json::to_string(&build_ticket_list_reply(&repo_root)).unwrap();
        assert!(
            list_text.contains("Landed"),
            "ticket list should show Landed for a Done ticket with no linked mission: {list_text}"
        );

        let show_text =
            serde_json::to_string(&build_ticket_show_reply(&repo_root, "no-mission")).unwrap();
        assert!(
            show_text.contains("Landed"),
            "ticket show should show Landed for a Done ticket with no linked mission: {show_text}"
        );
    }
}
