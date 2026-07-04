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

use kranz_engine::types::Plan;
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

    /// Commit an approved plan (plan.json/plan.md/index.md on the mission
    /// branch); returns the mission branch name.
    fn approve<'a>(&'a self, id: &'a str, plan: Plan) -> BoxFuture<'a, anyhow::Result<String>>;

    /// Start execution: the host consumes the engine into a background run.
    fn start<'a>(&'a self, id: &'a str) -> BoxFuture<'a, anyhow::Result<()>>;
}

/// How the bridge holds the host: shared, optional (a bridge without a host —
/// tests, or a hypothetical standalone run — degrades to honest refusals for
/// the operations that need live engines).
pub type SharedHost = Arc<dyn PlanningHost>;
