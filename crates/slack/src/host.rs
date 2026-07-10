//! The bridge's view of a hosted planning engine (design:
//! docs/slack-management.md "one registry, two clients").
//!
//! The registry itself is `kranz_server::MissionHost`; this crate deliberately
//! does not depend on `kranz_server` (routing/rendering stay pure and the dep
//! graph stays a fan: cli → {slack, server} → engine). Instead the bridge
//! programs against this small trait, and `kranz serve --slack` passes in an
//! adapter over the SAME `MissionHost` the web UI uses — one set of live
//! engines, two clients, never two engines fighting over one mission lock.
//!
//! `BoxFuture` (from `futures-util`, already a dependency) keeps the trait
//! object-safe without an `async-trait` dependency.

use kranz_engine::draft::DraftOutcome;
use kranz_engine::types::{Plan, TokenUsage};
use serde_json::Value;
use std::sync::Arc;

/// Re-exported so implementers (the CLI's serve adapter) box their futures
/// without their own `futures-util` dependency.
pub use futures_util::future::BoxFuture;

/// What `request_plan` came back with: a reviewable plan (plus a pre-rendered
/// estimate line) or the orchestrator's prose explaining what it still needs.
#[derive(Debug, Clone)]
pub enum PlanOutcome {
    Ready {
        plan: Plan,
        /// Human-ready estimate line (e.g. `est. $6.10–$30.50 (expected
        /// ~$12.20)`), pre-rendered by the adapter so the bridge never learns
        /// cost internals. `None` when no estimate was available.
        estimate: Option<String>,
    },
    NotReady(String),
}

#[derive(Debug, Clone)]
pub struct AskOutcome {
    pub answer: String,
    pub cost_usd: f64,
    pub tokens: TokenUsage,
}

/// The hosted-engine operations the bridge drives. Implemented over
/// `kranz_server::MissionHost` by the CLI's serve command; tests implement it
/// with canned replies (no `claude` spawn, no server).
///
/// Errors are `anyhow` with user-presentable messages — the bridge forwards
/// them into ephemeral replies verbatim (the host adapter is responsible for
/// keeping them short and honest, e.g. "a turn is in flight").
pub trait PlanningHost: Send + Sync + 'static {
    /// Create a mission (no planning turn yet); returns the mission id.
    fn create<'a>(&'a self, goal: &'a str) -> BoxFuture<'a, anyhow::Result<String>>;

    /// One conversational planning turn; returns the orchestrator's reply
    /// (seed reply already prepended by the host).
    fn planning_turn<'a>(
        &'a self,
        id: &'a str,
        text: &'a str,
    ) -> BoxFuture<'a, anyhow::Result<String>>;

    /// Demand the plan (one orchestrator turn).
    fn request_plan<'a>(&'a self, id: &'a str) -> BoxFuture<'a, anyhow::Result<PlanOutcome>>;

    /// Approve the plan PARKED by the last Ready [`Self::request_plan`] —
    /// the ONE pending-plan cache every surface (Slack, web, glasses ring)
    /// shares, held by the host, never the bridge. `Ok(Some(branch))` =
    /// committed (plan.json/plan.md/index.md on the mission branch);
    /// `Ok(None)` = nothing parked (never requested, or forfeited by a serve
    /// restart / idle release — re-run `/kranz plan`). A failed approve
    /// re-parks the plan so a retry can fire.
    fn approve_pending<'a>(&'a self, id: &'a str) -> BoxFuture<'a, anyhow::Result<Option<String>>>;

    /// Start execution: the host consumes the engine into a background run.
    fn start<'a>(&'a self, id: &'a str) -> BoxFuture<'a, anyhow::Result<()>>;

    /// Release a hosted idle engine (freeing the mission lock) so an external
    /// runner — the `kranz work` dispatcher — can take the mission over.
    /// `true` = the mission is now free of this host (released or never
    /// hosted); `false` = it is actively running here and was left alone.
    fn release<'a>(&'a self, id: &'a str) -> BoxFuture<'a, anyhow::Result<bool>>;

    /// Run a non-interactive draft turn for backlog ticket `slug` (`/kranz
    /// draft <slug>`): validates the slug, creates + seeds the mission
    /// through this host, and drives it to a terminal [`DraftOutcome`]. A
    /// money-spending, multi-minute operation — the bridge runs it off the
    /// socket read loop after posting an immediate ack, mirroring
    /// [`Self::create`] + [`Self::planning_turn`].
    fn draft<'a>(&'a self, slug: &'a str) -> BoxFuture<'a, anyhow::Result<DraftOutcome>>;

    /// Approve backlog ticket `slug` into the queue (`/kranz approve <slug>`,
    /// the slug-resolving twin of `/kranz approve <mission-id>`): runs the
    /// EXACT SAME gate the REST/CLI approve path runs
    /// (`kranz_engine::deps::approve_ticket`, mirrored by
    /// `kranz_server::MissionHost::approve_ticket`) — refuses when the ticket
    /// isn't REVIEW, when a blocked-by cycle is reachable, or when an
    /// unsatisfied blocker exists. `Ok(mission_id)` = the ticket's drafted
    /// mission was queued; `Err` carries the engine's refusal message
    /// VERBATIM (never paraphrased) for the bridge to forward unchanged.
    fn approve_ticket<'a>(&'a self, slug: &'a str) -> BoxFuture<'a, anyhow::Result<String>>;

    /// `/kranz work run` → trigger the queue drain/claim/skip loop THROUGH
    /// the host (`POST /api/queue/drain`'s seam): the host spawns the drain
    /// as a background task on the serve process and returns once it has
    /// been kicked off (idempotent while a drain is already live) — this
    /// call never itself drives a mission turn, so the bridge caller never
    /// resumes/runs a mission on the socket read loop.
    fn drain<'a>(&'a self) -> BoxFuture<'a, anyhow::Result<()>>;

    /// `/kranz merge <slug|id>` / the Delivered card's Merge button → the
    /// human-triggered gated Merge action (`POST /api/missions/:id/merge`'s
    /// seam): refuses on a dirty tracked tree, runs the full CI gate suite,
    /// and merges `--no-ff` into the base branch on green — never pushes.
    /// `Ok(value)` carries the host's `{"merged":true,"commit":…}`; `Err`
    /// carries the refusal (dirty tree / failing gate with captured output /
    /// merge conflict); the bridge clips long output to Slack's field limit.
    fn merge<'a>(&'a self, id: &'a str) -> BoxFuture<'a, anyhow::Result<Value>>;

    /// `/kranz ask <question>` → answer a read-only question grounded in
    /// mission/ticket/repo state. The implementation must not create, enqueue,
    /// approve, start, merge, or write mission state.
    fn ask<'a>(&'a self, question: &'a str) -> BoxFuture<'a, anyhow::Result<AskOutcome>>;
}

/// How the bridge holds the host: shared, optional (a bridge without a host —
/// tests, or a hypothetical standalone run — degrades to honest refusals for
/// the operations that need live engines).
pub type SharedHost = Arc<dyn PlanningHost>;
