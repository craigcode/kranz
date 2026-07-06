//! The agent backend seam (plan §4 "sdk-adapter", §8 testing strategy).
//!
//! CONTRACT FILE — do not modify in implementation phases. If a change seems
//! necessary, report it instead of editing.
//!
//! There is no Rust Claude Agent SDK, so the real backend drives the
//! `claude` CLI headless (`--print --output-format stream-json`), which is
//! the same process the official SDKs wrap — Claude Code's native config
//! (CLAUDE.md, .claude/skills, .mcp.json, hooks) is inherited because the
//! session runs with cwd = repo root. The mock backend returns scripted
//! event streams so the entire engine is testable without model calls.

use crate::error::Result;
use crate::types::TokenUsage;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// How the session receives its prompt.
#[derive(Debug, Clone)]
pub enum PromptMode {
    /// One-shot: prompt passed at spawn, session ends after the final result.
    SingleShot(String),
    /// Streaming input (`--input-format stream-json`): initial prompt sent as
    /// the first user message; further messages may be injected while live.
    /// Used by the long-lived orchestrator session (§4.1).
    Streaming(String),
}

/// Everything needed to spawn one agent session.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// Working directory (target repo root) — project config loads from here.
    pub cwd: PathBuf,
    pub prompt: PromptMode,
    /// Appended to the default Claude Code system prompt (role prompt).
    ///
    /// Claude-CLI-ism: `--append-system-prompt` has no equivalent on other
    /// backends, which may ignore this field.
    pub append_system_prompt: Option<String>,
    /// Model alias or full id (passed to --model).
    pub model: String,
    /// low | medium | high | xhigh | max (passed to --effort).
    ///
    /// Claude-CLI-ism: `--effort` is a Claude Code flag; a non-claude backend
    /// may ignore this field.
    pub effort: String,
    /// Engine-chosen session UUID (passed to --session-id) so resume
    /// bookkeeping never depends on parsing CLI output.
    pub session_id: String,
    /// When set, resume this previous session id instead of starting fresh.
    ///
    /// Claude-CLI-ism: session resume is a Claude Code capability; a
    /// non-claude backend may ignore (or reject) this field.
    pub resume: Option<String>,
    /// Passed to --permission-mode (e.g. "acceptEdits", "plan", "dontAsk").
    ///
    /// Claude-CLI-ism: `--permission-mode` has no equivalent on other
    /// backends, which may ignore this field.
    pub permission_mode: Option<String>,
    /// Passed to --allowedTools (patterns like "Bash(npm test*)").
    ///
    /// Claude-CLI-ism: `--allowedTools` is a Claude Code permission concept; a
    /// non-claude backend may ignore this field.
    pub allowed_tools: Vec<String>,
    /// Passed to --disallowedTools (patterns like "Bash(git push*)").
    ///
    /// Claude-CLI-ism: `--disallowedTools` is a Claude Code permission
    /// concept; a non-claude backend may ignore this field.
    pub disallowed_tools: Vec<String>,
    /// Passed to `--tools` (the built-in exclusive tool allow-list): empty = CLI default set, no flag emitted.
    /// Distinct from `allowed_tools`/`disallowed_tools`, which are permission patterns.
    ///
    /// Claude-CLI-ism: `--tools` is a Claude Code flag; a non-claude backend
    /// may ignore this field.
    pub tools: Vec<String>,
    /// Extra settings JSON (hooks etc.) passed via --settings.
    ///
    /// Claude-CLI-ism: `--settings` (hooks, etc.) is a Claude Code concept; a
    /// non-claude backend may ignore this field.
    pub settings_json: Option<serde_json::Value>,
    /// JSON Schema enforced on the session's structured output (--json-schema).
    ///
    /// Claude-CLI-ism: `--json-schema` is a Claude Code flag; a non-claude
    /// backend may ignore this field.
    pub json_schema: Option<serde_json::Value>,
    /// Hard dollar cap for the run (--max-budget-usd).
    ///
    /// Claude-CLI-ism: `--max-budget-usd` is a Claude Code flag; a non-claude
    /// backend may ignore this field (and should enforce cost caps engine-side
    /// via its own pricing table instead).
    pub max_budget_usd: Option<f64>,
    /// Soft turn budget: the engine counts assistant turns and aborts the
    /// session when exceeded (the CLI no longer has --max-turns).
    pub max_turns: Option<u32>,
    /// Extra environment variables for the child process.
    pub env: HashMap<String, String>,
}

/// Normalized events surfaced from a session's output stream.
///
/// `raw` always carries the full original stream-json line for transcript
/// fidelity; the variants extract only what the engine acts on.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AgentEvent {
    /// First message of a session (`type: "system", subtype: "init"`).
    Init {
        session_id: String,
        model: String,
        raw: serde_json::Value,
    },
    /// Assistant text output.
    Text {
        text: String,
        raw: serde_json::Value,
    },
    /// Assistant requested a tool.
    ToolUse {
        tool: String,
        /// Compact human-readable summary of the input (e.g. the Bash command).
        summary: String,
        raw: serde_json::Value,
    },
    /// Tool result returned to the model. `denied` is true when the call was
    /// blocked by permission rules — surfaced so the dashboard shows
    /// guardrails firing (§4.7).
    ToolResult {
        tool: Option<String>,
        denied: bool,
        summary: String,
        raw: serde_json::Value,
    },
    /// Terminal result (`type: "result"`). Exactly one per completed turn in
    /// single-shot mode; in streaming mode one per injected turn.
    Result {
        /// Final result text (worker report JSON lives here when --json-schema
        /// was set).
        text: String,
        is_error: bool,
        usage: TokenUsage,
        cost_usd: Option<f64>,
        num_turns: Option<u32>,
        raw: serde_json::Value,
    },
    /// Anything else (kept for transcripts; engine ignores).
    Other { raw: serde_json::Value },
}

/// Why a session ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionExit {
    /// Process exited normally after a result message.
    Completed,
    /// Aborted by the engine (interrupt, turn budget, shutdown).
    Aborted,
    /// Process died or emitted an error result.
    Failed(String),
}

/// A live agent session. Consumers poll `next_event` until `None`, then call
/// `exit_status`.
#[async_trait::async_trait]
pub trait AgentSession: Send {
    /// The session id actually in use (== spec.session_id unless resumed).
    fn session_id(&self) -> String;

    /// Next event from the session stream; `None` when the stream is closed.
    async fn next_event(&mut self) -> Result<Option<AgentEvent>>;

    /// Inject a user message (streaming-input sessions only; error otherwise).
    async fn send_user_message(&mut self, text: &str) -> Result<()>;

    /// Terminate the underlying process/stream. Must kill the whole process
    /// tree and work on Windows (no bare POSIX signal assumptions — §9).
    async fn abort(&mut self) -> Result<()>;

    /// Available after the stream has closed.
    fn exit_status(&self) -> Option<SessionExit>;
}

/// Factory for agent sessions — the mockable seam.
#[async_trait::async_trait]
pub trait AgentBackend: Send + Sync {
    async fn start(&self, spec: SessionSpec) -> Result<Box<dyn AgentSession>>;
}
