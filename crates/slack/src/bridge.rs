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
use crate::host::{PlanOutcome, SharedHost};
use crate::inbound::{route, Action, ThreadLookup};
use crate::outbound::{classify, NotifyClass, Outbound};
use crate::threads::ThreadMap;
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use kranz_engine::event_log::EventLog;
use kranz_engine::paths::MissionPaths;
use kranz_engine::reducer;
use kranz_engine::types::{ControlCommand, MissionState, MissionStatus, Plan};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_tungstenite::tungstenite::Message;

/// Poll interval over each mission's event log (matches the server WS tail).
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Backoff bounds for reconnecting the Socket Mode websocket.
const BACKOFF_MIN: Duration = Duration::from_millis(500);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

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
        self.inner.lock().unwrap().thread_ts(mission_id).map(str::to_string)
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
        self.inner.lock().unwrap().mission_for_thread(thread_ts).map(str::to_string)
    }
}

/// Plans returned by `/kranz plan`, held for the approve buttons — a Block Kit
/// button `value` can't carry a whole plan, so the bridge keeps the reviewed
/// plan in memory keyed by mission id and the button click consumes it.
///
/// Deliberately NOT persisted: a serve restart between review and approve
/// forfeits the pending plan, and the approve reply says to run `/kranz plan`
/// again — honest and cheap to recover, unlike silently approving a plan that
/// was never re-reviewed against a possibly-changed conversation.
#[derive(Clone, Default)]
pub struct PendingPlans(Arc<Mutex<HashMap<String, Plan>>>);

impl PendingPlans {
    fn put(&self, mission_id: &str, plan: Plan) {
        self.0.lock().unwrap().insert(mission_id.to_string(), plan);
    }

    fn take(&self, mission_id: &str) -> Option<Plan> {
        self.0.lock().unwrap().remove(mission_id)
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
        cursor.last_seq = event.seq;

        if let Some(outbound) = classify(event, &cursor.state) {
            if class_enabled(&cfg.notify, outbound.class()) {
                if let Err(e) = post_outbound(cfg, client, threads, mission_id, &outbound).await {
                    tracing::warn!(mission = %mission_id, error = %e, "slack post failed");
                }
            }
        }
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
    let posted_ts = client.post_message(&cfg.channel, &blocks, thread_ts.as_deref()).await?;
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
    // Reviewed-plan cache for the approve buttons, shared across reconnects
    // (a websocket rotation must not forfeit a plan awaiting approval).
    let pending = PendingPlans::default();
    // A Notify fired once when shutdown resolves; the per-connection loop selects
    // on it so a mid-connection shutdown is prompt.
    let stop = Arc::new(Notify::new());
    let stop_setter = stop.clone();
    tokio::pin!(shutdown);

    let mut backoff = BACKOFF_MIN;
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                stop_setter.notify_waiters();
                tracing::info!("slack inbound loop shutting down");
                return;
            }
            result = connect_once(&cfg, &client, &repo_root, &threads, &host, &pending, &stop) => {
                match result {
                    // Clean close requested by shutdown: exit.
                    Ok(true) => return,
                    // Socket connected then closed on its own (Slack rotates the
                    // wss URL, or a network blip): a healthy session, so RESET
                    // the backoff — the drop isn't a failure to reach Slack.
                    Ok(false) => {
                        backoff = BACKOFF_MIN;
                        tracing::info!("slack socket closed; reconnecting");
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
async fn connect_once(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    host: &Option<SharedHost>,
    pending: &PendingPlans,
    stop: &Arc<Notify>,
) -> Result<bool> {
    let url = client.open_connection().await.context("opening Socket Mode connection")?;
    let (ws_stream, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .context("dialing Socket Mode websocket")?;
    tracing::info!("slack Socket Mode connected");
    let (mut write, mut read) = ws_stream.split();
    // Bounded dedup of processed envelope ids: Slack redelivers an envelope if
    // the ack is late, which for a claude-spawning action (NewMission) would
    // otherwise create a DUPLICATE mission. Seen ids are re-acked but not
    // re-dispatched. Bounded so a long-lived connection can't grow it forever.
    let mut seen = SeenEnvelopes::default();

    loop {
        tokio::select! {
            _ = stop.notified() => {
                let _ = write.send(Message::Close(None)).await;
                return Ok(true);
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
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
                            let (cfg, client, repo, threads, host, pending) = (
                                cfg.clone(),
                                client.clone(),
                                repo_root.to_path_buf(),
                                threads.clone(),
                                host.clone(),
                                pending.clone(),
                            );
                            tokio::spawn(async move {
                                dispatch_action(
                                    &cfg, &client, &repo, &threads, host.as_ref(), &pending,
                                    &routed.action,
                                )
                                .await;
                            });
                        } else {
                            dispatch_action(
                                cfg, client, repo_root, threads, host.as_ref(), pending,
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
                    Some(Err(e)) => return Err(e.into()),
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
    dispatch_action(
        cfg,
        client,
        repo_root,
        threads,
        None,
        &PendingPlans::default(),
        &routed.action,
    )
    .await;
    // Ack whatever carried an envelope_id, even Ignore, so Slack stops retrying.
    routed.envelope_id.map(|id| json!({ "envelope_id": id }).to_string())
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

/// A one-line ephemeral "not authorized" reply for a spend-gated action from an
/// unlisted user (docs/slack-management.md must-have #1).
fn not_authorized_blocks() -> Vec<Value> {
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
///   thread, the plan parked in [`PendingPlans`] for the buttons. NotReady →
///   the orchestrator's prose posted threaded.
/// - **Approve / ApproveStart / ApproveMission** — allowlist-gated
///   [`approve_flow`]: commit the pending plan through the host, then queue
///   (`Approve`/slash) or start execution through the host (`ApproveStart`).
///   State-aware without a pending plan (see [`approve_flow`]).
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
///
/// ## Ack budget (docs must-have #3)
/// `connect_once` acks every envelope FIRST and runs the slow actions
/// ([`is_slow_action`]: claude-spawning or engine-touching) on spawned tasks,
/// deduped by envelope id ([`SeenEnvelopes`]) so a Slack redelivery can't
/// double-create or double-approve. Only pure-local actions run inline on the
/// read loop.
async fn dispatch_action(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    host: Option<&SharedHost>,
    pending: &PendingPlans,
    action: &Action,
) {
    match action {
        Action::Help { response_url } => {
            reply_ephemeral(cfg, client, response_url.as_deref(), &crate::format::build_help()).await;
        }

        Action::Status { mission_id, response_url } => {
            match build_status_reply(repo_root, mission_id.as_deref()) {
                Ok(blocks) => reply_ephemeral(cfg, client, response_url.as_deref(), &blocks).await,
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

        Action::NewMission { goal, user_id, response_url, channel } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                reply_ephemeral(cfg, client, response_url.as_deref(), &not_authorized_blocks()).await;
                return;
            }
            match new_mission(cfg, client, repo_root, threads, host, goal, channel).await {
                Ok(blocks) => reply_ephemeral(cfg, client, response_url.as_deref(), &blocks).await,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to create mission from Slack");
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url.as_deref(),
                        &error_blocks(&format!("Couldn't create the mission: {e}")),
                    )
                    .await;
                }
            }
        }

        Action::RequestPlan { mission_id, user_id, response_url } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                reply_ephemeral(cfg, client, response_url.as_deref(), &not_authorized_blocks()).await;
                return;
            }
            let Some(host) = host else {
                reply_ephemeral(cfg, client, response_url.as_deref(), &no_host_blocks(mission_id))
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
                        milestone_titles:
                            plan.milestones.iter().map(|m| m.title.clone()).collect(),
                        assertion_count: plan.validation_contract.len(),
                        estimate,
                    });
                    // Cache BEFORE posting: once the buttons are visible they
                    // must find the plan.
                    pending.put(mission_id, plan);
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
                    let blocks = crate::format::build_planning_reply(mission_id, &prose);
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

        Action::ApproveMission { mission_id, user_id, response_url } => {
            approve_flow(
                cfg, client, repo_root, threads, host, pending, mission_id,
                user_id.as_deref(), response_url.as_deref(), false,
            )
            .await;
        }

        // Per-role config change. SPEND-ADJACENT (it re-shapes future turns'
        // spend), so it is gated on the allowlist exactly like `/kranz new`.
        Action::Config { mission_id, role, model, effort, user_id, response_url } => {
            if !cfg.is_authorized(user_id.as_deref()) {
                reply_ephemeral(cfg, client, response_url.as_deref(), &not_authorized_blocks()).await;
                return;
            }
            match config_change(repo_root, mission_id.as_deref(), role, model, effort.as_deref()) {
                Ok(applied_to) => {
                    let effort_note = effort
                        .as_deref()
                        .map(|e| format!(", effort `{e}`"))
                        .unwrap_or_default();
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url.as_deref(),
                        &error_blocks(&format!(
                            ":gear: Set `{role}` model `{model}`{effort_note} on `{applied_to}`."
                        )),
                    )
                    .await
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to apply config change from Slack");
                    reply_ephemeral(
                        cfg,
                        client,
                        response_url.as_deref(),
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
        Action::Pause { mission_id, user_id, response_url } => {
            steer(cfg, client, repo_root, mission_id.as_deref(), user_id.as_deref(),
                  response_url.as_deref(), ControlCommand::Pause, "paused").await;
        }
        Action::Resume { mission_id, user_id, response_url } => {
            steer(cfg, client, repo_root, mission_id.as_deref(), user_id.as_deref(),
                  response_url.as_deref(), ControlCommand::Resume, "resumed").await;
        }

        // Queue report. READ-ONLY and REPORT-ONLY: the bridge never drains the
        // queue on the socket loop (that would spawn `claude`); it reads the
        // queue state and points at the `kranz work` dispatcher.
        Action::Work { response_url } => {
            reply_ephemeral(cfg, client, response_url.as_deref(), &build_work_reply(repo_root)).await;
        }

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
        Action::Approve { mission_id, user_id, response_url } => {
            approve_flow(
                cfg, client, repo_root, threads, host, pending, mission_id,
                user_id.as_deref(), response_url.as_deref(), false,
            )
            .await;
        }
        Action::ApproveStart { mission_id, user_id, response_url } => {
            approve_flow(
                cfg, client, repo_root, threads, host, pending, mission_id,
                user_id.as_deref(), response_url.as_deref(), true,
            )
            .await;
        }

        // A threaded reply: on a PLANNING mission this is a hosted planning
        // turn (spend → allowlist-gated, acked in-thread because a message
        // event has no response_url); on anything else it stays the running-
        // mission guidance write it has always been.
        Action::Guidance { mission_id, text, user_id } => {
            match mission_status(repo_root, mission_id) {
                Ok(MissionStatus::Planning) => {
                    let Some(host) = host else {
                        post_thread_note(cfg, client, threads, mission_id, &format!(
                            "This mission is still in planning, and this bridge has no hosted \
                             engine (it was started without `kranz serve`). Continue with \
                             `kranz plan --mission {mission_id}` in a terminal."
                        ))
                        .await;
                        return;
                    };
                    if !cfg.is_authorized(user_id.as_deref()) {
                        post_thread_note(cfg, client, threads, mission_id,
                            "Planning turns spend money and are limited to the \
                             `slack.allowUsers` allowlist — ask an admin to add you.",
                        )
                        .await;
                        return;
                    }
                    post_thread_note(cfg, client, threads, mission_id,
                        ":hourglass_flowing_sand: Planning turn running — the orchestrator's \
                         reply lands here, usually within a couple of minutes.",
                    )
                    .await;
                    match host.planning_turn(mission_id, text).await {
                        Ok(reply) => {
                            let blocks = crate::format::build_planning_reply(mission_id, &reply);
                            if let Err(e) =
                                post_to_mission_thread(cfg, client, threads, mission_id, blocks)
                                    .await
                            {
                                tracing::warn!(mission = %mission_id, error = %e, "failed to post planning reply");
                            }
                        }
                        Err(e) => {
                            post_thread_note(cfg, client, threads, mission_id, &format!(
                                "Planning turn failed: {e}"
                            ))
                            .await;
                        }
                    }
                }
                // Running / paused / blocked (and, unchanged from before,
                // terminal): the control-inbox guidance write.
                Ok(_) => {
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
/// With a reviewed plan pending (from `/kranz plan`): commit it through the
/// hosted engine — THE step the first cut of this bridge skipped, which left
/// missions queued-but-unapproved that `kranz work` then refused — and either
/// start execution through the host (`start == true`) or insert into the
/// per-repo queue. Without a pending plan: honest, state-aware handling (a
/// planning mission needs `/kranz plan` first; an approved-but-idle one can
/// still be queued/started).
#[allow(clippy::too_many_arguments)]
async fn approve_flow(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    host: Option<&SharedHost>,
    pending: &PendingPlans,
    mission_id: &str,
    user_id: Option<&str>,
    response_url: Option<&str>,
    start: bool,
) {
    if !cfg.is_authorized(user_id) {
        reply_ephemeral(cfg, client, response_url, &not_authorized_blocks()).await;
        return;
    }

    if let Some(plan) = pending.take(mission_id) {
        let Some(host) = host else {
            pending.put(mission_id, plan);
            reply_ephemeral(cfg, client, response_url, &no_host_blocks(mission_id)).await;
            return;
        };
        let branch = match host.approve(mission_id, plan.clone()).await {
            Ok(branch) => branch,
            Err(e) => {
                // A transient failure (e.g. a turn in flight) must not forfeit
                // the reviewed plan — put it back for the retry click.
                pending.put(mission_id, plan);
                reply_ephemeral(
                    cfg,
                    client,
                    response_url,
                    &error_blocks(&format!("Couldn't approve `{mission_id}`: {e}")),
                )
                .await;
                return;
            }
        };
        if start {
            match host.start(mission_id).await {
                Ok(()) => {
                    post_thread_note(cfg, client, threads, mission_id, &format!(
                        ":rocket: Plan approved and execution started (branch `{branch}`) — \
                         progress posts in this thread; deep inspection in the web UI."
                    ))
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
            match approve_mission(repo_root, mission_id) {
                Ok(()) => {
                    post_thread_note(cfg, client, threads, mission_id, &format!(
                        ":white_check_mark: Plan approved and queued (branch `{branch}`) — \
                         the `kranz work` dispatcher runs it next."
                    ))
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

    // No pending plan. Route by actual mission state instead of blindly
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
        // starting/queueing is legitimate.
        Ok(_) => {
            if start {
                let Some(host) = host else {
                    reply_ephemeral(cfg, client, response_url, &no_host_blocks(mission_id)).await;
                    return;
                };
                match host.start(mission_id).await {
                    Ok(()) => {
                        post_thread_note(cfg, client, threads, mission_id, &format!(
                            ":rocket: Execution started for `{mission_id}` — progress posts \
                             in this thread."
                        ))
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
                match approve_mission(repo_root, mission_id) {
                    Ok(()) => {
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

/// A mission's current status, folded read-only from its event log.
fn mission_status(repo_root: &Path, mission_id: &str) -> Result<MissionStatus> {
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
    let posted_ts = client.post_message(&cfg.channel, &blocks, thread_ts.as_deref()).await?;
    if thread_ts.is_none() {
        threads.set(mission_id, &posted_ts);
    }
    Ok(())
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
    if let Err(e) = post_to_mission_thread(cfg, client, threads, mission_id, error_blocks(msg)).await
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
        Action::Guidance { mission_id, text, .. } => guidance(repo_root, mission_id, text),
        Action::NewTicket { title, .. } => scaffold_ticket(repo_root, title),
        Action::Help { .. }
        | Action::Status { .. }
        | Action::NewMission { .. }
        | Action::RequestPlan { .. }
        | Action::ApproveMission { .. }
        | Action::ApproveStart { .. }
        | Action::Config { .. }
        | Action::Pause { .. }
        | Action::Resume { .. }
        | Action::Work { .. }
        | Action::AppHome { .. }
        | Action::Ignore => Ok(()),
    }
}

/// Build the status-summary blocks for a mission by folding its event log —
/// fully wired and read-only (no allowlist gate, no backend). When
/// `mission_id` is `None`, the most-recently-created mission is chosen. An
/// unknown/absent mission is a plain error the caller turns into an ephemeral.
fn build_status_reply(repo_root: &Path, mission_id: Option<&str>) -> Result<Vec<Value>> {
    let mission_id = match mission_id {
        Some(id) => id.to_string(),
        None => most_recent_mission(repo_root)
            .ok_or_else(|| anyhow::anyhow!("no missions yet — create one with `/kranz new <goal>`"))?,
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
    Ok(kranz_engine::control::resolve_active_mission(repo_root, explicit)?)
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
    body.push_str(&format!("{done}/{total} milestone{} complete", if total == 1 { "" } else { "s" }));
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
            body.push_str(&format!("{}. `{}` · priority {}\n", i + 1, e.mission_id, e.priority));
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
        missions.push(HomeMission { mission_id: id, status: status_word(state.mission.status) });
    }

    let queue = kranz_engine::queue::list(repo_root)
        .into_iter()
        .map(|e| HomeQueueItem { mission_id: e.mission_id, priority: e.priority })
        .collect::<Vec<_>>();

    // Open tickets: everything not in a terminal (Done/Failed) pipeline state.
    let tickets = kranz_engine::ticket::Ticket::list(repo_root)
        .into_iter()
        .filter_map(|t| {
            let state = kranz_engine::ticket::Ticket::read_state(repo_root, &t.slug);
            if ticket_is_open(state) {
                Some(HomeTicket { slug: t.slug, title: t.title, state: format!("{state:?}") })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    crate::format::build_home_view(&missions, &queue, &tickets, dashboard_url)
}

/// Whether a mission status is terminal (excluded from the active-missions list).
fn is_terminal(status: MissionStatus) -> bool {
    matches!(status, MissionStatus::Complete | MissionStatus::Failed | MissionStatus::Abandoned)
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
        &ControlCommand::Msg { text: text.to_string(), interrupt: false },
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
            let mut engine = MissionEngine::create(
                Arc::new(backend),
                repo_root.to_path_buf(),
                goal,
                cfg_engine,
            )
            .context("creating mission")?;
            let mission_id = engine.mission_id().to_string();
            // One seeding planning turn: the orchestrator's opening scoping
            // questions come back to post in-thread. A captured seed reply
            // (fresh session) happened first, so prepend it.
            let reply = engine.planning_turn(goal).await.context("seeding planning turn")?;
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
    let labeled =
        crate::format::label_blocks(blocks.clone(), cfg.instance_name.as_deref());
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
    use tempfile::TempDir;

    #[test]
    fn class_enabled_respects_flags() {
        let flags = NotifyFlags { plan_ready: false, blocked: true, complete: true, needs_context: true };
        assert!(!class_enabled(&flags, NotifyClass::PlanReady));
        assert!(class_enabled(&flags, NotifyClass::Blocked));
        assert!(class_enabled(&flags, NotifyClass::Complete));
    }

    #[test]
    fn slugify_examples() {
        assert_eq!(slugify("Rate-limit the notes API"), "rate-limit-the-notes-api");
        assert_eq!(slugify("  Fix   the  thing!! "), "fix-the-thing");
        assert_eq!(slugify("***"), "ticket");
    }

    #[test]
    fn approve_action_enqueues_mission() {
        let tmp = TempDir::new().unwrap();
        apply_action(tmp.path(), &Action::Approve { mission_id: "m-1".into(), user_id: None, response_url: None }).unwrap();
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
            &Action::Status { mission_id: None, response_url: None },
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

    /// Seed a mission's `events.jsonl` with a single `mission.created` event,
    /// built from public engine types so it folds exactly like a real log —
    /// no git, no backend. Returns the mission id.
    #[test]
    fn pending_plans_take_consumes_and_put_restores() {
        use kranz_engine::types::{Plan, PlanFeature, PlanMilestone};
        let plan = Plan {
            goal: "g".into(),
            validation_contract: vec![],
            milestones: vec![PlanMilestone {
                title: "M1".into(),
                features: vec![PlanFeature {
                    title: "F1".into(),
                    spec: "s".into(),
                    validation_criteria: vec!["c".into()],
                }],
            }],
        };
        let pending = PendingPlans::default();
        assert!(pending.take("m-1").is_none(), "empty cache has nothing");
        pending.put("m-1", plan.clone());
        let taken = pending.take("m-1").expect("cached plan comes back");
        assert_eq!(taken.goal, "g");
        // take() consumed it — a second click must not find a stale plan…
        assert!(pending.take("m-1").is_none());
        // …but a failed approve puts it back for the retry.
        pending.put("m-1", taken);
        assert!(pending.take("m-1").is_some());
    }

    #[test]
    fn mission_status_folds_planning_and_post_approval() {
        use kranz_engine::events::{Event, EventKind};
        use kranz_engine::types::{Plan, PlanFeature, PlanMilestone};
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-p", "still planning");
        assert_eq!(mission_status(tmp.path(), "m-p").unwrap(), MissionStatus::Planning);

        // Append a PlanApproved: the reducer folds it to Running (approved,
        // executable) — the state the approve_flow no-pending path may queue.
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
                },
            },
        };
        let line = serde_json::to_string(&event).unwrap();
        let mut existing = std::fs::read_to_string(paths.events_file()).unwrap();
        existing.push_str(&line);
        existing.push('\n');
        std::fs::write(paths.events_file(), existing).unwrap();
        assert_eq!(mission_status(tmp.path(), "m-a").unwrap(), MissionStatus::Running);

        assert!(mission_status(tmp.path(), "m-nope").is_err(), "unknown mission is an error");
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

    #[test]
    fn build_status_reply_folds_the_log() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-abc", "Rate-limit the notes API");
        let blocks = build_status_reply(tmp.path(), Some("m-abc")).unwrap();
        let text = serde_json::to_string(&blocks).unwrap();
        assert!(text.contains("m-abc"), "status carries the mission id");
        assert!(text.contains("Rate-limit the notes API"), "status carries the goal");
        // A freshly-created mission is in Planning.
        assert!(text.contains("Planning"), "status pill reflects the folded state");
    }

    #[test]
    fn build_status_reply_unknown_mission_is_error() {
        let tmp = TempDir::new().unwrap();
        let err = build_status_reply(tmp.path(), Some("m-nope")).unwrap_err().to_string();
        assert!(err.contains("m-nope"), "error names the unknown mission");
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
        let err = config_change(tmp.path(), None, "worker", "opus", None).unwrap_err().to_string();
        assert!(err.contains("several active missions"), "asks for an explicit id: {err}");
        for id in ["m-a", "m-b"] {
            assert!(
                kranz_engine::control::drain(&MissionPaths::new(tmp.path(), id)).unwrap().is_empty(),
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
        assert!(err.contains("active missions"), "honest error, not false success: {err}");
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
        let applied =
            enqueue_steer(tmp.path(), Some("m-steer"), ControlCommand::Pause).unwrap();
        assert_eq!(applied, "m-steer");
        let drained = kranz_engine::control::drain(&MissionPaths::new(tmp.path(), "m-steer")).unwrap();
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
        let drained = kranz_engine::control::drain(&MissionPaths::new(tmp.path(), "m-only")).unwrap();
        assert_eq!(drained.len(), 1);
        assert!(matches!(drained[0].1, ControlCommand::Resume), "got {:?}", drained[0].1);
    }

    #[test]
    fn enqueue_steer_refuses_ambiguous_target_and_enqueues_nothing() {
        // Two active missions, no explicit id → error asking for an id; NOTHING
        // enqueued on either (never silently guess which running mission to pause).
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-a", "goal a");
        seed_mission(tmp.path(), "m-b", "goal b");
        let err = enqueue_steer(tmp.path(), None, ControlCommand::Pause).unwrap_err().to_string();
        assert!(err.contains("several active missions"), "asks for an explicit id: {err}");
        for id in ["m-a", "m-b"] {
            assert!(
                kranz_engine::control::drain(&MissionPaths::new(tmp.path(), id)).unwrap().is_empty(),
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
        assert!(err.contains("active missions"), "honest error, not false success: {err}");
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
        assert!(matches!(drained[0].1, ControlCommand::Resume), "got {:?}", drained[0].1);
    }

    #[test]
    fn pause_resume_work_are_noops_in_apply_action() {
        // These reply over the network in dispatch_action; apply_action must not
        // double-handle them (no control file, no side effects).
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-1", "goal");
        for action in [
            Action::Pause { mission_id: Some("m-1".into()), user_id: None, response_url: None },
            Action::Resume { mission_id: Some("m-1".into()), user_id: None, response_url: None },
            Action::Work { response_url: None },
        ] {
            apply_action(tmp.path(), &action).unwrap();
        }
        assert!(
            kranz_engine::control::drain(&MissionPaths::new(tmp.path(), "m-1")).unwrap().is_empty(),
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
        assert!(text.contains("m-q1") && text.contains("m-q2"), "both queued missions listed");
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
        assert!(s.contains("m-a") && s.contains("m-b"), "active missions present");
        assert!(s.contains("Planning"), "status pill present");
        assert!(s.contains("Rate-limit the notes API"), "open ticket title present");
        assert!(s.contains("m/m-a"), "deep link present when dashboard configured");
    }

    #[test]
    fn build_home_view_excludes_terminal_missions_and_done_tickets() {
        let tmp = TempDir::new().unwrap();
        // A mission driven to Complete via mission.completed.
        seed_completed_mission(tmp.path(), "m-done");
        // A ticket in the Done terminal state.
        let tdir = kranz_engine::ticket::Ticket::tickets_dir(tmp.path());
        std::fs::create_dir_all(&tdir).unwrap();
        std::fs::write(tdir.join("finished.md"), "---\ntitle: Finished\n---\n\n## Goal\nx\n").unwrap();
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
        assert!(!text.contains("m-done"), "terminal mission excluded from active list");
        assert!(!text.contains("Finished"), "done ticket excluded from open tickets");
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
        let ack = handle_envelope(&cfg, &client, tmp.path(), &threads, &frame).await.unwrap();
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
        assert!(handle_envelope(&cfg, &client, tmp.path(), &threads, &hello).await.is_none());
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
        // Fast local/one-call actions stay inline.
        assert!(!is_slow_action(&Action::Status { mission_id: None, response_url: None }));
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
}
