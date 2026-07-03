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
use crate::inbound::{route, Action, ThreadLookup};
use crate::outbound::{classify, NotifyClass, Outbound};
use crate::threads::ThreadMap;
use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use kranz_engine::event_log::EventLog;
use kranz_engine::paths::MissionPaths;
use kranz_engine::reducer;
use kranz_engine::types::{ControlCommand, MissionState};
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
    let blocks = match outbound {
        Outbound::PlanReady(p) => crate::format::build_plan_ready(p),
        Outbound::Blocked(b) => crate::format::build_blocked(b),
        Outbound::Complete(c) => crate::format::build_complete(c),
    };
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
    shutdown: impl std::future::Future<Output = ()>,
) {
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
            result = connect_once(&cfg, &client, &repo_root, &threads, &stop) => {
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
    stop: &Arc<Notify>,
) -> Result<bool> {
    let url = client.open_connection().await.context("opening Socket Mode connection")?;
    let (ws_stream, _resp) = tokio_tungstenite::connect_async(&url)
        .await
        .context("dialing Socket Mode websocket")?;
    tracing::info!("slack Socket Mode connected");
    let (mut write, mut read) = ws_stream.split();

    loop {
        tokio::select! {
            _ = stop.notified() => {
                let _ = write.send(Message::Close(None)).await;
                return Ok(true);
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Some(reply) = handle_envelope(cfg, client, repo_root, threads, &text).await {
                            if write.send(Message::Text(reply)).await.is_err() {
                                return Ok(false);
                            }
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
async fn handle_envelope(
    _cfg: &SlackConfig,
    _client: &SlackClient,
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
    if let Err(e) = apply_action(repo_root, &routed.action) {
        tracing::warn!(error = %e, "failed to apply inbound Slack action");
    }
    // Ack whatever carried an envelope_id, even Ignore, so Slack stops retrying.
    routed.envelope_id.map(|id| json!({ "envelope_id": id }).to_string())
}

/// Apply a routed inbound action to the local mission machinery.
fn apply_action(repo_root: &Path, action: &Action) -> Result<()> {
    match action {
        Action::Approve { mission_id } => approve_mission(repo_root, mission_id),
        Action::Guidance { mission_id, text } => guidance(repo_root, mission_id, text),
        Action::NewTicket { title, .. } => scaffold_ticket(repo_root, title),
        Action::Ignore => Ok(()),
    }
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
        apply_action(tmp.path(), &Action::Approve { mission_id: "m-1".into() }).unwrap();
        assert!(queue::contains(tmp.path(), "m-1"));
    }

    #[test]
    fn guidance_action_enqueues_control_msg() {
        let tmp = TempDir::new().unwrap();
        apply_action(
            tmp.path(),
            &Action::Guidance { mission_id: "m-1".into(), text: "use a token bucket".into() },
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

    #[tokio::test]
    async fn handle_envelope_acks_with_envelope_id() {
        let tmp = TempDir::new().unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        let cfg = SlackConfig {
            bot_token: "xoxb".into(),
            app_token: "xapp".into(),
            channel: "C1".into(),
            notify: NotifyFlags::default(),
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
        };
        let client = SlackClient::new(&cfg).unwrap();
        let hello = json!({ "type": "hello" }).to_string();
        assert!(handle_envelope(&cfg, &client, tmp.path(), &threads, &hello).await.is_none());
    }
}
