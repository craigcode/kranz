//! Kranz engine — orchestration core (no UI deps).
//!
//! Kranz is a local mission-control harness: an orchestrator plans, fresh
//! Claude Code sessions implement features one at a time, independent
//! validator sessions judge each milestone, and git is the source of truth.
//! An append-only event log makes every mission resumable (`kill -9` safe).
//!
//! Module map (build order per plan §5 Phase 1):
//! - contracts: [`types`], [`events`], [`error`], [`backend`]
//! - foundation: [`paths`], [`event_log`], [`reducer`], [`scrub`], [`git_ops`],
//!   [`config`], [`cost`], [`prompts`], [`backend_mock`]
//! - orchestration: [`backend_claude`], [`permissions`], [`runner`],
//!   [`control`], [`digest`], [`orchestrator`]

pub mod backend;
pub mod error;
pub mod events;
pub mod paths;
pub mod types;

pub mod auth_verify;
pub mod config;
pub mod contract_sweep;
pub mod cost;
pub mod deps;
pub mod event_log;
pub mod git_ops;
pub mod lessons;
pub mod merge;
pub mod merged;
pub mod prompts;
pub mod queue;
pub mod reducer;
pub mod sandbox;
pub mod scrub;
pub mod ticket;
pub mod work;

pub mod backend_claude;
pub mod backend_codex;
pub mod backend_droid;
pub mod backend_mock;
pub mod control;
pub mod digest;
pub mod draft;
pub mod merge_gate;
pub mod orchestrator;
pub mod permissions;
pub mod runner;

// Re-exported at the crate root so callers (and tests) that only need to
// inject a scripted backend don't need the full `backend_mock` path.
pub use backend_mock::MockBackend;
