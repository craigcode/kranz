//! Outbound mission-tailing engine: the polling/posting half of the Slack
//! bridge (the inbound Socket Mode loop lives in [`crate::bridge`]).
//!
//! [`run_bridge`] polls each mission's `events.jsonl` (the same 250 ms tail
//! the server WS uses), folds state incrementally, and posts Slack messages
//! for the classified event classes (see [`crate::outbound`]). One Slack
//! thread per mission: the first post for a mission becomes the thread root,
//! recorded in [`crate::threads::ThreadMap`] (via [`SharedThreads`]) so
//! restarts re-thread; later posts reply in-thread.
//!
//! Shutdown is any `Future` that resolves when the host wants the bridge to
//! stop; the loop selects on a shared notify fired from it.

use crate::bridge::SharedThreads;
use crate::client::SlackClient;
use crate::config::{NotifyFlags, SlackConfig};
use crate::outbound::{classify, NotifyClass, Outbound};
use anyhow::Result;
use kranz_engine::event_log::EventLog;
use kranz_engine::paths::MissionPaths;
use kranz_engine::reducer;
use kranz_engine::types::MissionState;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

/// Poll interval over each mission's event log (matches the server WS tail).
const POLL_INTERVAL: Duration = Duration::from_millis(500);

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

/// Persisted per-mission `last_seq` so a restarted bridge does not re-announce
/// already-posted notifications. Lives at `.kranz/slack/notify-cursors.json`
/// (next to the thread map's parent dir). Only the seq is persisted — folded
/// state is rebuilt from the event log on first sighting after load.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct NotifyCursors {
    #[serde(default)]
    by_mission: BTreeMap<String, u64>,
}

impl NotifyCursors {
    fn path(repo_root: &Path) -> PathBuf {
        repo_root
            .join(".kranz")
            .join("slack")
            .join("notify-cursors.json")
    }

    fn load(repo_root: &Path) -> Result<Self> {
        let path = Self::path(repo_root);
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("invalid notify cursors {}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(anyhow::anyhow!("cannot read {}: {e}", path.display())),
        }
    }

    fn save(&self, repo_root: &Path) -> Result<()> {
        let path = Self::path(repo_root);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        // Mirror ThreadMap's atomic write (temp + rename).
        let dir = path.parent().unwrap_or_else(|| Path::new("."));
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("notify-cursors.json");
        let tmp = dir.join(format!(".{file_name}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, json.as_bytes())?;
        match std::fs::rename(&tmp, &path) {
            Ok(()) => Ok(()),
            Err(_) if cfg!(windows) => {
                let _ = std::fs::remove_file(&path);
                std::fs::rename(&tmp, &path).map_err(Into::into)
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(e.into())
            }
        }
    }

    fn set(&mut self, mission_id: impl Into<String>, last_seq: u64) {
        self.by_mission.insert(mission_id.into(), last_seq);
    }

    /// Drop cursors whose mission is no longer on disk (deleted/abandoned
    /// cleanup). Returns the number of entries removed.
    fn prune_absent(&mut self, live_ids: &std::collections::HashSet<String>) -> usize {
        let before = self.by_mission.len();
        self.by_mission.retain(|id, _| live_ids.contains(id));
        before.saturating_sub(self.by_mission.len())
    }
}

/// Persist one mission's advanced cursor. Best-effort: a save failure is
/// logged, never fatal — the in-memory cursor still advances, and the next
/// successful save recovers the map.
fn persist_notify_cursor(repo_root: &Path, mission_id: &str, last_seq: u64) {
    let mut cursors = match NotifyCursors::load(repo_root) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load slack notify cursors before save");
            NotifyCursors::default()
        }
    };
    cursors.set(mission_id, last_seq);
    if let Err(e) = cursors.save(repo_root) {
        tracing::warn!(error = %e, "failed to persist slack notify cursors");
    }
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

/// One tick's mission-listing + cursor-prune step, factored from
/// [`run_bridge`] so the error-vs-empty distinction is testable.
///
/// Returns the live mission set on a successful listing, or `None` when the
/// listing FAILED — in which case nothing is pruned, persisted or in-memory.
/// Treating a transient listing error (fd exhaustion, mid-deletion race) as
/// "no missions" would wipe every cursor in one tick; when the missions then
/// "reappear" on the next tick they re-seed from scratch, re-announcing
/// history to Slack and, for idle missions, silently dropping notifications
/// after a later restart.
///
/// The persisted prune (a file read + JSON parse) only runs when the live set
/// actually changed since the last tick — an idle bridge must not reload
/// notify-cursors.json every 500ms. The first tick counts as changed so
/// missions deleted while the bridge was down are still pruned at startup.
///
/// `listing_failed` tracks whether the PREVIOUS tick's listing failed, so a
/// persistent failure (which repeats every 500ms — ~120 ticks/min) warns once
/// at onset, downgrades repeats to debug, and logs an info on recovery.
fn prune_tick(
    repo_root: &Path,
    cursors: &mut HashMap<String, MissionCursor>,
    last_live: &mut Option<std::collections::HashSet<String>>,
    listing_failed: &mut bool,
) -> Option<std::collections::HashSet<String>> {
    let live: std::collections::HashSet<String> = match MissionPaths::try_list_missions(repo_root) {
        Ok(ids) => ids.into_iter().collect(),
        Err(e) => {
            if *listing_failed {
                tracing::debug!(error = %e, "mission listing still failing; outbound poll still skipped");
            } else {
                *listing_failed = true;
                tracing::warn!(error = %e, "failed to list missions; skipping outbound poll until the mission listing recovers");
            }
            return None;
        }
    };
    if std::mem::take(listing_failed) {
        tracing::info!("mission listing recovered; resuming outbound poll");
    }
    if last_live.as_ref() != Some(&live) {
        // Prune persisted cursors for deleted missions (best-effort).
        match NotifyCursors::load(repo_root) {
            Ok(mut persisted) => {
                if persisted.prune_absent(&live) > 0 {
                    if let Err(e) = persisted.save(repo_root) {
                        tracing::warn!(error = %e, "failed to save pruned slack notify cursors");
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to load slack notify cursors for prune");
            }
        }
        *last_live = Some(live.clone());
    }
    cursors.retain(|id, _| live.contains(id));
    Some(live)
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
    // Startup snapshot of persisted last_seq per mission. First sightings
    // re-read the cursor file from disk (it advances while the bridge runs);
    // this snapshot is only the fallback when that re-read fails.
    let persisted_seqs: HashMap<String, u64> = match NotifyCursors::load(&repo_root) {
        Ok(persisted) => persisted.by_mission.into_iter().collect(),
        Err(e) => {
            tracing::warn!(error = %e, "failed to load slack notify cursors; starting empty");
            HashMap::new()
        }
    };
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Live mission set observed on the previous tick; the prune step only
    // touches notify-cursors.json when this changes (cheap idle ticks).
    let mut last_live: Option<std::collections::HashSet<String>> = None;
    // Whether the previous tick's mission listing failed (warn-once state for
    // `prune_tick` — see its doc).
    let mut listing_failed = false;

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                tracing::info!("slack outbound loop shutting down");
                return;
            }
            _ = ticker.tick() => {
                // On a listing failure `prune_tick` prunes nothing and we skip
                // the whole tick (there is no trustworthy live set to poll).
                let Some(live) = prune_tick(&repo_root, &mut cursors, &mut last_live, &mut listing_failed) else {
                    continue;
                };
                for mission_id in &live {
                    if let Err(e) =
                        poll_mission(
                            &cfg,
                            &client,
                            &repo_root,
                            &threads,
                            mission_id,
                            &mut cursors,
                            &persisted_seqs,
                        )
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
    persisted_seqs: &HashMap<String, u64>,
) -> Result<()> {
    let mut poster = SlackPoster;
    poll_mission_with_poster(
        cfg,
        client,
        repo_root,
        threads,
        mission_id,
        cursors,
        persisted_seqs,
        &mut poster,
    )
    .await
}

/// Testable implementation of [`poll_mission`], with Slack posting injected so
/// cursor retry semantics can be exercised without a live Slack endpoint.
#[allow(clippy::too_many_arguments)]
async fn poll_mission_with_poster(
    cfg: &SlackConfig,
    client: &SlackClient,
    repo_root: &Path,
    threads: &SharedThreads,
    mission_id: &str,
    cursors: &mut HashMap<String, MissionCursor>,
    persisted_seqs: &HashMap<String, u64>,
    poster: &mut impl OutboundPoster,
) -> Result<()> {
    let paths = MissionPaths::new(repo_root, mission_id);
    let events_path = paths.events_file();
    if !events_path.is_file() {
        return Ok(());
    }

    // First sighting: fold the whole log. Prefer a persisted last_seq so a
    // restarted bridge does not re-announce already-posted notifications;
    // otherwise seed at head (only NEW events post).
    //
    // The persisted cursor is re-read from disk here rather than trusted from
    // the startup `persisted_seqs` snapshot: the on-disk value advances while
    // the bridge runs, and a cursor can be dropped and re-seeded long after
    // start (fold error, mission briefly absent from a listing). Seeding from
    // the stale startup snapshot would replay every notification posted since
    // the bridge started. The snapshot remains only as a fallback when the
    // disk read itself fails.
    if !cursors.contains_key(mission_id) {
        let events = EventLog::read_events(&events_path)?;
        let state = reducer::fold(&events)?;
        let persisted_seq = match NotifyCursors::load(repo_root) {
            Ok(on_disk) => on_disk.by_mission.get(mission_id).copied(),
            Err(e) => {
                tracing::warn!(
                    mission = %mission_id,
                    error = %e,
                    "failed to re-read slack notify cursors at first sighting; using startup snapshot"
                );
                persisted_seqs.get(mission_id).copied()
            }
        };
        let last_seq = persisted_seq.unwrap_or(state.last_seq).min(state.last_seq);
        if persisted_seq.is_none() {
            // Seeding at head: persist immediately (one write per NEW mission).
            // Without this, a mission first seen at head has no on-disk cursor
            // until its next event posts — a bridge stopped in that window
            // reseeds at the NEW head on restart, silently dropping everything
            // appended while it was down.
            persist_notify_cursor(repo_root, mission_id, last_seq);
        }
        cursors.insert(mission_id.to_string(), MissionCursor { state, last_seq });
        return Ok(());
    }

    let cursor = cursors.get_mut(mission_id).expect("just checked");
    let new_events = EventLog::read_events_after(&events_path, cursor.last_seq)?;
    let seq_at_entry = cursor.last_seq;
    let mut outcome = Ok(());
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
                    let resync = (|| -> Result<MissionState> {
                        let events = EventLog::read_events(&events_path)?;
                        Ok(reducer::fold(&events)?)
                    })();
                    match resync {
                        Ok(state) => {
                            cursor.last_seq = state.last_seq;
                            cursor.state = state;
                        }
                        Err(e) => outcome = Err(e),
                    }
                    break;
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
                    break;
                }
            }
        }
        cursor.last_seq = event.seq;
    }
    // ONE persist per poll, after the event loop. Post-then-persist ordering
    // is deliberate and must stay: persisting before posting would mark
    // notifications as sent that were never posted, silently LOSING them on a
    // crash. The cost of batching is that a crash between the last post and
    // this persist re-announces up to one poll's burst of events (instead of
    // at most one event when we persisted per event) — a consciously accepted
    // trade-off: duplicates are visible and harmless, drops are silent, and
    // batching removes a full load+parse+serialize+rename per event from the
    // hot poll loop.
    if cursor.last_seq != seq_at_entry {
        persist_notify_cursor(repo_root, mission_id, cursor.last_seq);
    }
    outcome
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
        Outbound::RevisionReady(r) => crate::format::build_revision_ready(r, dash),
        Outbound::GrantReady(g) => crate::format::build_grant_ready(g, dash),
        Outbound::QuestionReady(q) => crate::format::build_question_ready(q, dash),
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_cfg() -> SlackConfig {
        SlackConfig {
            bot_token: "xoxb".into(),
            app_token: "xapp".into(),
            channel: "C1".into(),
            notify: NotifyFlags::default(),
            allow_users: vec![],
            allow_all_users: false,
            dashboard_url: None,
            instance_name: None,
        }
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
            considered_alternatives: None,
            command_grants: vec![],
            touch_set: vec![],
            standards_manifest: None,
            reviewer_independence: None,
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

    fn append_mission_completed(repo_root: &Path, mission_id: &str) {
        use kranz_engine::event_log::LockForce;
        use kranz_engine::events::EventKind;

        let paths = MissionPaths::new(repo_root, mission_id);
        let mut log =
            EventLog::acquire(&paths, mission_id, Duration::from_millis(0), LockForce::No).unwrap();
        log.append(EventKind::MissionCompleted {}).unwrap();
    }

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
        let persisted = HashMap::new();

        poll_mission_with_poster(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            mission_id,
            &mut cursors,
            &persisted,
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
            &persisted,
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
            &persisted,
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

    #[test]
    fn notify_cursors_roundtrip_on_disk() {
        let tmp = TempDir::new().unwrap();
        let mut cursors = NotifyCursors::default();
        cursors.set("m-abc123", 7);
        cursors.set("m-def456", 42);
        cursors.save(tmp.path()).unwrap();
        let loaded = NotifyCursors::load(tmp.path()).unwrap();
        assert_eq!(loaded.by_mission.get("m-abc123").copied(), Some(7));
        assert_eq!(loaded.by_mission.get("m-def456").copied(), Some(42));
        assert_eq!(loaded.by_mission.get("m-missing"), None);
        assert!(NotifyCursors::path(tmp.path()).is_file());
    }

    #[tokio::test]
    async fn outbound_cursor_persists_last_seq_after_successful_post() {
        let tmp = TempDir::new().unwrap();
        let mission_id = "m-persist";
        seed_mission(tmp.path(), mission_id, "notify me");

        let cfg = test_cfg();
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        let mut cursors = HashMap::new();
        let mut poster = FailOncePoster::new();
        // Force first post to succeed by priming attempts past the fail-once.
        poster.attempts = 1;
        let persisted = HashMap::new();

        poll_mission_with_poster(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            mission_id,
            &mut cursors,
            &persisted,
            &mut poster,
        )
        .await
        .unwrap();
        append_plan_approved(tmp.path(), mission_id, "notify me");
        poll_mission_with_poster(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            mission_id,
            &mut cursors,
            &persisted,
            &mut poster,
        )
        .await
        .unwrap();
        assert_eq!(cursors.get(mission_id).unwrap().last_seq, 2);
        let on_disk = NotifyCursors::load(tmp.path()).unwrap();
        assert_eq!(
            on_disk.by_mission.get(mission_id).copied(),
            Some(2),
            "successful post must persist last_seq"
        );
    }

    #[tokio::test]
    async fn first_sighting_at_head_persists_the_seed_cursor_immediately() {
        let tmp = TempDir::new().unwrap();
        let mission_id = "m-seedhead";
        seed_mission(tmp.path(), mission_id, "notify me");

        let cfg = test_cfg();
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        let mut cursors = HashMap::new();
        let mut poster = FailOncePoster::new();
        let persisted = HashMap::new();

        poll_mission_with_poster(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            mission_id,
            &mut cursors,
            &persisted,
            &mut poster,
        )
        .await
        .unwrap();

        assert_eq!(poster.attempts, 0, "first sighting never posts");
        let head = cursors.get(mission_id).unwrap().last_seq;
        let on_disk = NotifyCursors::load(tmp.path()).unwrap();
        assert_eq!(
            on_disk.by_mission.get(mission_id).copied(),
            Some(head),
            "a mission first seen at head must persist its seed cursor at once — \
             without the write, a bridge stopped before this mission's next event \
             reseeds at the NEW head on restart and silently drops everything \
             appended while it was down"
        );
    }

    /// Build an in-memory cursor for prune tests; the folded state's content
    /// is irrelevant to pruning, only the map entry's presence matters.
    fn dummy_cursor(last_seq: u64) -> MissionCursor {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-dummy1", "goal");
        let paths = MissionPaths::new(tmp.path(), "m-dummy1");
        let events = EventLog::read_events(&paths.events_file()).unwrap();
        MissionCursor {
            state: reducer::fold(&events).unwrap(),
            last_seq,
        }
    }

    #[cfg(unix)]
    #[test]
    fn prune_tick_prunes_nothing_when_mission_listing_fails() {
        let tmp = TempDir::new().unwrap();
        // A mission the bridge knows about, both persisted and in-memory.
        let mut persisted = NotifyCursors::default();
        persisted.set("m-alive1", 5);
        persisted.save(tmp.path()).unwrap();
        let mut cursors = HashMap::new();
        cursors.insert("m-alive1".to_string(), dummy_cursor(5));
        // Make the listing FAIL (missions path is a file, so read_dir errors
        // with something other than NotFound) — the transient-error shape
        // that previously read as "every mission was deleted".
        std::fs::write(tmp.path().join(".kranz").join("missions"), b"boom").unwrap();

        let mut last_live = None;
        let mut listing_failed = false;
        let live = prune_tick(
            tmp.path(),
            &mut cursors,
            &mut last_live,
            &mut listing_failed,
        );

        assert!(live.is_none(), "a failed listing must not yield a live set");
        assert!(
            cursors.contains_key("m-alive1"),
            "in-memory cursor must survive a listing failure"
        );
        let on_disk = NotifyCursors::load(tmp.path()).unwrap();
        assert_eq!(
            on_disk.by_mission.get("m-alive1").copied(),
            Some(5),
            "persisted cursor must survive a listing failure"
        );
        assert!(
            last_live.is_none(),
            "a failed listing must not be remembered as an observed live set"
        );
    }

    #[cfg(unix)]
    #[test]
    fn prune_tick_marks_a_listing_outage_once_and_clears_it_on_recovery() {
        // The bridge polls every 500ms, so a persistent listing failure must
        // not warn ~120 times a minute. The onset warn / repeat debug /
        // recovery info are keyed off `listing_failed`, so the cheap
        // observable contract is the flag's transitions: set on the first
        // failure (the one warn), still set on repeats (debug only), cleared
        // by a successful listing (the recovery info).
        let tmp = TempDir::new().unwrap();
        let kranz_dir = tmp.path().join(".kranz");
        std::fs::create_dir_all(&kranz_dir).unwrap();
        // Same failure shape as the prune test above: the missions path is a
        // file, so read_dir errors with something other than NotFound.
        std::fs::write(kranz_dir.join("missions"), b"boom").unwrap();
        let mut cursors = HashMap::new();
        let mut last_live = None;
        let mut listing_failed = false;

        assert!(prune_tick(
            tmp.path(),
            &mut cursors,
            &mut last_live,
            &mut listing_failed
        )
        .is_none());
        assert!(listing_failed, "first failure marks the outage onset");
        assert!(prune_tick(
            tmp.path(),
            &mut cursors,
            &mut last_live,
            &mut listing_failed
        )
        .is_none());
        assert!(listing_failed, "repeat failures keep the outage marked");

        // Recovery: remove the bogus file — a missing missions dir is a
        // GENUINE empty listing (NotFound), not a failure.
        std::fs::remove_file(kranz_dir.join("missions")).unwrap();
        assert!(prune_tick(
            tmp.path(),
            &mut cursors,
            &mut last_live,
            &mut listing_failed
        )
        .is_some());
        assert!(
            !listing_failed,
            "a successful listing clears the outage (the recovery info fires once)"
        );
    }

    #[test]
    fn prune_tick_prunes_deleted_missions_on_a_genuinely_empty_listing() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-live11", "goal");
        let mut persisted = NotifyCursors::default();
        persisted.set("m-live11", 3);
        persisted.set("m-gone11", 9);
        persisted.save(tmp.path()).unwrap();
        let mut cursors = HashMap::new();
        cursors.insert("m-live11".to_string(), dummy_cursor(3));
        cursors.insert("m-gone11".to_string(), dummy_cursor(9));

        let mut last_live = None;
        let mut listing_failed = false;
        let live = prune_tick(
            tmp.path(),
            &mut cursors,
            &mut last_live,
            &mut listing_failed,
        )
        .unwrap();

        assert!(live.contains("m-live11") && !live.contains("m-gone11"));
        assert!(cursors.contains_key("m-live11"));
        assert!(!cursors.contains_key("m-gone11"));
        let on_disk = NotifyCursors::load(tmp.path()).unwrap();
        assert_eq!(on_disk.by_mission.get("m-live11").copied(), Some(3));
        assert_eq!(
            on_disk.by_mission.get("m-gone11"),
            None,
            "cursor for a genuinely deleted mission is pruned"
        );
    }

    #[test]
    fn prune_tick_skips_cursor_file_reload_when_live_set_is_unchanged() {
        let tmp = TempDir::new().unwrap();
        seed_mission(tmp.path(), "m-idle11", "goal");
        let mut cursors = HashMap::new();
        let mut last_live = None;
        let mut listing_failed = false;
        prune_tick(
            tmp.path(),
            &mut cursors,
            &mut last_live,
            &mut listing_failed,
        )
        .unwrap();

        // Sneak a stale entry into the persisted file. With an unchanged live
        // set the next tick must not even reload the file (the 500ms idle
        // path stays free of file reads), so the stale entry survives…
        let mut persisted = NotifyCursors::default();
        persisted.set("m-stale1", 7);
        persisted.save(tmp.path()).unwrap();
        prune_tick(
            tmp.path(),
            &mut cursors,
            &mut last_live,
            &mut listing_failed,
        )
        .unwrap();
        assert_eq!(
            NotifyCursors::load(tmp.path())
                .unwrap()
                .by_mission
                .get("m-stale1")
                .copied(),
            Some(7),
            "unchanged live set must skip the per-tick cursor-file prune"
        );

        // …until the live set changes, when the normal prune reclaims it.
        seed_mission(tmp.path(), "m-fresh1", "goal");
        prune_tick(
            tmp.path(),
            &mut cursors,
            &mut last_live,
            &mut listing_failed,
        )
        .unwrap();
        assert_eq!(
            NotifyCursors::load(tmp.path())
                .unwrap()
                .by_mission
                .get("m-stale1"),
            None,
            "a changed live set re-runs the prune"
        );
    }

    /// Poster that records, at each post, the mission's last_seq currently
    /// persisted ON DISK — pinning both persist batching (no write happens
    /// between the posts of one poll's burst) and crash-window ordering
    /// (posts happen before the batch persist, never after).
    struct DiskObservingPoster {
        repo_root: PathBuf,
        mission_id: String,
        disk_seq_at_post: Vec<Option<u64>>,
    }

    impl OutboundPoster for DiskObservingPoster {
        fn post<'a>(
            &'a mut self,
            _cfg: &'a SlackConfig,
            _client: &'a SlackClient,
            _threads: &'a SharedThreads,
            _mission_id: &'a str,
            _outbound: &'a Outbound,
        ) -> PostFuture<'a> {
            let seq = NotifyCursors::load(&self.repo_root)
                .ok()
                .and_then(|c| c.by_mission.get(&self.mission_id).copied());
            self.disk_seq_at_post.push(seq);
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn poll_persists_once_after_the_last_post_of_a_batch() {
        let tmp = TempDir::new().unwrap();
        let mission_id = "m-batch1";
        seed_mission(tmp.path(), mission_id, "notify me");

        let cfg = test_cfg();
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        let mut cursors = HashMap::new();
        let persisted = HashMap::new();
        let mut poster = DiskObservingPoster {
            repo_root: tmp.path().to_path_buf(),
            mission_id: mission_id.to_string(),
            disk_seq_at_post: Vec::new(),
        };

        // First sighting seeds at head (seq 1): no posts, and the seed cursor
        // itself is persisted (the seed-at-head write).
        poll_mission_with_poster(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            mission_id,
            &mut cursors,
            &persisted,
            &mut poster,
        )
        .await
        .unwrap();
        assert!(poster.disk_seq_at_post.is_empty());

        // A burst of two classified events lands before the next poll.
        append_plan_approved(tmp.path(), mission_id, "notify me"); // seq 2
        append_mission_completed(tmp.path(), mission_id); // seq 3
        poll_mission_with_poster(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            mission_id,
            &mut cursors,
            &persisted,
            &mut poster,
        )
        .await
        .unwrap();

        // Both posts observed the PRE-batch on-disk value (the seq-1 seed):
        // no per-event persist ran between them, and no persist ran before a
        // post.
        assert_eq!(
            poster.disk_seq_at_post,
            vec![Some(1), Some(1)],
            "one poll's burst must not persist between (or before) its posts"
        );
        // The single batch persist then recorded the final seq.
        assert_eq!(cursors.get(mission_id).unwrap().last_seq, 3);
        assert_eq!(
            NotifyCursors::load(tmp.path())
                .unwrap()
                .by_mission
                .get(mission_id)
                .copied(),
            Some(3),
            "the batch persist after the last post records the final seq"
        );
    }

    #[tokio::test]
    async fn restart_first_sighting_reseeds_from_disk_and_posts_only_missed_events() {
        let tmp = TempDir::new().unwrap();
        let mission_id = "m-resume1";
        seed_mission(tmp.path(), mission_id, "notify me");
        // Session #1 posted through seq 1 and persisted that cursor…
        let mut session_one = NotifyCursors::default();
        session_one.set(mission_id, 1);
        session_one.save(tmp.path()).unwrap();
        // …then the bridge went down and the plan got approved (seq 2).
        append_plan_approved(tmp.path(), mission_id, "notify me");

        // Session #2: fresh in-memory cursors, and a deliberately EMPTY
        // startup snapshot — first sighting must re-read the persisted file
        // from disk; trusting a stale/missing snapshot would seed at head
        // and silently drop the notification appended while down.
        let cfg = test_cfg();
        let client = SlackClient::new(&cfg).unwrap();
        let threads = SharedThreads::load(tmp.path()).unwrap();
        let mut cursors = HashMap::new();
        let persisted = HashMap::new();
        let mut poster = FailOncePoster::new();
        poster.attempts = 1; // prime past the fail-once: every post succeeds

        poll_mission_with_poster(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            mission_id,
            &mut cursors,
            &persisted,
            &mut poster,
        )
        .await
        .unwrap();
        assert_eq!(
            cursors.get(mission_id).unwrap().last_seq,
            1,
            "first sighting reseeds from the on-disk cursor, not at head"
        );
        assert!(poster.classes.is_empty(), "first sighting never posts");

        poll_mission_with_poster(
            &cfg,
            &client,
            tmp.path(),
            &threads,
            mission_id,
            &mut cursors,
            &persisted,
            &mut poster,
        )
        .await
        .unwrap();
        // Exactly the one event missed while down posts — no duplicate of the
        // already-announced prefix, no silent drop of seq 2.
        assert_eq!(poster.classes, vec![NotifyClass::PlanReady]);
        let cursor = cursors.get(mission_id).unwrap();
        assert_eq!(cursor.last_seq, 2, "cursor ends at head");
        assert_eq!(
            NotifyCursors::load(tmp.path())
                .unwrap()
                .by_mission
                .get(mission_id)
                .copied(),
            Some(2)
        );
    }
}
