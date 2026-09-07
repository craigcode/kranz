#![allow(rustdoc::private_intra_doc_links)]

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
pub mod plan_fit;
pub mod types;

pub mod agent_env;
#[cfg(windows)]
pub(crate) mod appcontainer_windows;
pub mod auth_verify;
pub mod backend_readiness;
pub mod command_exec;
pub mod comparison_metrics;
pub mod config;
pub mod container_egress;
pub mod contract_gates;
pub mod contract_health;
pub mod contract_lint;
pub mod contract_sweep;
pub mod corpus_export;
pub mod cost;
pub mod decompose;
pub mod deps;
pub mod disk_preflight;
pub mod domain_lint;
pub mod egress_proxy;
pub mod escalation_metrics;
pub mod event_log;
pub mod evidence_bundle;
pub mod findings;
pub mod gate;
pub mod gate_results;
pub mod gate_score_flags;
pub mod gate_scores;
pub mod git_ops;
pub mod hook_gates;
pub mod hook_status;
pub mod hooks;
pub mod judgement;
pub mod knowledge;
pub mod lessons;
pub mod merge;
pub mod merged;
pub mod migrate_state;
pub mod mission_catalog;
pub mod outcomes;
pub mod pack;
pub mod planning;
pub mod pr_handoff;
pub mod preflight;
pub mod prompts;
pub mod provenance;
pub mod pty_harness;
pub mod queue;
pub mod reducer;
pub mod report_render;
pub mod review_artifact;
pub mod reviewer_independence;
pub mod routing;
pub mod routing_rules;
pub mod sandbox;
pub mod sandbox_container;
pub mod sandbox_windows;
pub mod scrub;
pub mod standards_attestation;
pub mod standards_coverage;
pub mod standards_enforcement;
pub mod standards_metrics;
pub mod standards_waiver;
mod stream_bounds;
/// Runtime-gated test support: skip loudly, and fail where the capability is
/// required, so a silent skip cannot masquerade as a pass.
pub mod test_capability;
/// Portable `sh`/`cmd` snippets for test fixtures; see the module docs for why
/// literal `true`/`false`/`test`/`sleep` must not appear in one.
#[cfg(test)]
pub(crate) mod test_shell;
pub mod ticket;
pub mod ticket_notes;
pub mod trace_export;
pub mod validator_integrity;
pub mod validator_snapshot;
pub mod work;
pub mod workspace_container;
pub mod workspace_contract;
pub mod workspace_data;
pub mod workspace_gate;
pub mod workspace_provider;
pub mod workspace_remote;

pub mod backend_acp;
pub mod backend_claude;
pub mod backend_codex;
pub mod backend_cursor;
pub mod backend_droid;
pub mod backend_kimi;
pub mod backend_local;
pub mod backend_mock;
mod backend_probe;
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
