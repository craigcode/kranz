//! Slack bridge configuration (opt-in).
//!
//! Two Slack tokens are required for Socket Mode:
//! - a **bot token** (`xoxb-…`) — authorizes Web API calls (`chat.postMessage`).
//! - an **app-level token** (`xapp-…`) — authorizes `apps.connections.open`,
//!   which returns the ephemeral `wss://` URL the bridge dials for Socket Mode.
//!
//! Config resolves from two sources, environment winning over the file so a
//! shell can override a checked-in `~/.kranz/config.json` for a one-off run:
//!
//! 1. the `slack` object in `~/.kranz/config.json` (never the repo — tokens are
//!    secrets and must not be committed).
//! 2. `KRANZ_SLACK_BOT_TOKEN` / `KRANZ_SLACK_APP_TOKEN` / `KRANZ_SLACK_CHANNEL`.
//!
//! The bridge is opt-in: [`SlackConfig::from_config`] returns `Ok(None)` when
//! neither source supplies a full triple, and `serve_slack` then no-ops.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Which classes of mission event get posted to Slack. All default on — the
/// bridge is already opt-in, so once configured the useful default is "tell me
/// everything". A user can silence a class in the config file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyFlags {
    /// A mission's plan is ready for human review.
    pub plan_ready: bool,
    /// A ticket bounced back needing more context.
    pub needs_context: bool,
    /// A milestone blocked and wants a human in the thread.
    pub blocked: bool,
    /// A mission completed or failed.
    pub complete: bool,
}

impl Default for NotifyFlags {
    fn default() -> Self {
        NotifyFlags { plan_ready: true, needs_context: true, blocked: true, complete: true }
    }
}

/// Resolved Slack bridge configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlackConfig {
    /// Bot token (`xoxb-…`) for Web API calls.
    pub bot_token: String,
    /// App-level token (`xapp-…`) for `apps.connections.open`.
    pub app_token: String,
    /// Channel id to post mission updates into (e.g. `C0123ABC`).
    pub channel: String,
    /// Which event classes to post.
    pub notify: NotifyFlags,
}

/// On-disk shape of the `slack` object inside `~/.kranz/config.json`. Every
/// field is optional so a partial block (e.g. only `notify` overrides) parses;
/// env vars fill the tokens/channel.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SlackFileConfig {
    #[serde(default)]
    bot_token: Option<String>,
    #[serde(default)]
    app_token: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    notify: Option<NotifyFlags>,
}

/// Top-level shape we deserialize `~/.kranz/config.json` into — only the
/// `slack` key matters here; the engine owns the rest and unknown keys are
/// ignored.
#[derive(Debug, Clone, Default, Deserialize)]
struct RootConfig {
    #[serde(default)]
    slack: Option<SlackFileConfig>,
}

impl SlackConfig {
    /// Resolve the effective Slack config, or `None` when the bridge is
    /// unconfigured. `repo_root` is accepted for symmetry with the rest of the
    /// engine (and so a future per-repo override could layer in) but the tokens
    /// are only ever read from the GLOBAL config file — never the repo — so a
    /// checkout can't leak them.
    ///
    /// Precedence per field: env var, else the file's `slack` object. All three
    /// of bot token, app token, and channel must resolve for the bridge to
    /// enable; a partial set is treated as unconfigured (returns `None`) so a
    /// half-set env doesn't start a bridge that can't post.
    pub fn from_config(repo_root: &Path) -> Result<Option<SlackConfig>> {
        let _ = repo_root; // reserved for future per-repo notify overrides
        let file = load_file_config()?;
        Ok(resolve(file, EnvVars::from_process()))
    }
}

/// The three env vars we read, captured so tests can inject them without
/// touching the process environment.
#[derive(Debug, Clone, Default)]
struct EnvVars {
    bot_token: Option<String>,
    app_token: Option<String>,
    channel: Option<String>,
}

impl EnvVars {
    fn from_process() -> Self {
        EnvVars {
            bot_token: non_empty_env("KRANZ_SLACK_BOT_TOKEN"),
            app_token: non_empty_env("KRANZ_SLACK_APP_TOKEN"),
            channel: non_empty_env("KRANZ_SLACK_CHANNEL"),
        }
    }
}

/// Read an env var, mapping absent-or-blank to `None`.
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Load and parse the `slack` object from the global config file. A missing
/// file (or a file with no `slack` key) yields the default (empty) file config;
/// a present-but-invalid file is an error so a typo is surfaced rather than
/// silently disabling the bridge.
fn load_file_config() -> Result<SlackFileConfig> {
    let Some(path) = kranz_engine::paths::global_config() else {
        return Ok(SlackFileConfig::default());
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SlackFileConfig::default())
        }
        Err(e) => return Err(anyhow!("cannot read {}: {e}", path.display())),
    };
    let root: RootConfig = serde_json::from_str(&text)
        .map_err(|e| anyhow!("invalid JSON in {}: {e}", path.display()))?;
    Ok(root.slack.unwrap_or_default())
}

/// Pure resolution of file + env into an optional config. Split out so the
/// precedence and "all three required" logic is unit-testable without any I/O.
fn resolve(file: SlackFileConfig, env: EnvVars) -> Option<SlackConfig> {
    let bot_token = env.bot_token.or(file.bot_token).map(trim).filter(|s| non_blank(s));
    let app_token = env.app_token.or(file.app_token).map(trim).filter(|s| non_blank(s));
    let channel = env.channel.or(file.channel).map(trim).filter(|s| non_blank(s));

    match (bot_token, app_token, channel) {
        (Some(bot_token), Some(app_token), Some(channel)) => Some(SlackConfig {
            bot_token,
            app_token,
            channel,
            notify: file.notify.unwrap_or_default(),
        }),
        _ => None,
    }
}

fn trim(s: String) -> String {
    s.trim().to_string()
}

fn non_blank(s: &str) -> bool {
    !s.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_only_resolves() {
        let cfg = resolve(
            SlackFileConfig::default(),
            EnvVars {
                bot_token: Some("xoxb-1".into()),
                app_token: Some("xapp-1".into()),
                channel: Some("C1".into()),
            },
        )
        .expect("full env triple resolves");
        assert_eq!(cfg.bot_token, "xoxb-1");
        assert_eq!(cfg.app_token, "xapp-1");
        assert_eq!(cfg.channel, "C1");
        assert_eq!(cfg.notify, NotifyFlags::default());
    }

    #[test]
    fn file_only_resolves() {
        let file = SlackFileConfig {
            bot_token: Some("xoxb-file".into()),
            app_token: Some("xapp-file".into()),
            channel: Some("Cfile".into()),
            notify: Some(NotifyFlags { plan_ready: false, ..NotifyFlags::default() }),
        };
        let cfg = resolve(file, EnvVars::default()).expect("full file triple resolves");
        assert_eq!(cfg.bot_token, "xoxb-file");
        assert!(!cfg.notify.plan_ready);
        assert!(cfg.notify.blocked);
    }

    #[test]
    fn env_wins_over_file_per_field() {
        let file = SlackFileConfig {
            bot_token: Some("xoxb-file".into()),
            app_token: Some("xapp-file".into()),
            channel: Some("Cfile".into()),
            notify: None,
        };
        let cfg = resolve(
            file,
            EnvVars { channel: Some("Cenv".into()), ..EnvVars::default() },
        )
        .expect("env channel fills, file supplies tokens");
        assert_eq!(cfg.bot_token, "xoxb-file");
        assert_eq!(cfg.channel, "Cenv");
    }

    #[test]
    fn partial_is_unconfigured() {
        // Missing channel → None even though both tokens are present.
        assert!(resolve(
            SlackFileConfig {
                bot_token: Some("xoxb".into()),
                app_token: Some("xapp".into()),
                channel: None,
                notify: None,
            },
            EnvVars::default(),
        )
        .is_none());
    }

    #[test]
    fn blank_values_are_ignored() {
        // A blank channel from the file does not count as configured.
        assert!(resolve(
            SlackFileConfig {
                bot_token: Some("xoxb".into()),
                app_token: Some("xapp".into()),
                channel: Some("   ".into()),
                notify: None,
            },
            EnvVars::default(),
        )
        .is_none());
    }

    #[test]
    fn empty_is_none() {
        assert!(resolve(SlackFileConfig::default(), EnvVars::default()).is_none());
    }
}
