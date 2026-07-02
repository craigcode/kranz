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

pub mod config;
pub mod cost;
pub mod event_log;
pub mod git_ops;
pub mod prompts;
pub mod reducer;
pub mod scrub;

pub mod backend_claude;
pub mod backend_mock;
pub mod control;
pub mod digest;
pub mod orchestrator;
pub mod permissions;
pub mod runner;
