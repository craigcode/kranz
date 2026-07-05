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
//! 2. `KRANZ_SLACK_BOT_TOKEN` / `KRANZ_SLACK_APP_TOKEN` / `KRANZ_SLACK_CHANNEL`
//!    (plus the optional `KRANZ_SLACK_DASHBOARD_URL` / `KRANZ_SLACK_INSTANCE`).
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
        NotifyFlags {
            plan_ready: true,
            needs_context: true,
            blocked: true,
            complete: true,
        }
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
    /// Slack user ids (`Uxxxx`) allowed to run money-spending actions
    /// (new / approve / start). See docs/slack-management.md must-have #1.
    /// Empty = solo default: allow anyone (the plumbing exists regardless so
    /// a workspace can lock spend down by listing its operators).
    pub allow_users: Vec<String>,
    /// Base URL of the web dashboard (e.g. `http://127.0.0.1:4600/`). When set,
    /// mission notifications carry an "Open in dashboard" deep link pointing at
    /// `<dashboard_url>#/m/<mission_id>`; when `None`, no link is added (no
    /// behavior change). Resolved from `slack.dashboardUrl` in the config file
    /// or `KRANZ_SLACK_DASHBOARD_URL`, env winning.
    pub dashboard_url: Option<String>,
    /// Human-readable name of THIS Kranz instance (e.g. `studio`, `laptop`,
    /// `cloud`). When several instances post into Slack (one Slack app per
    /// instance — see docs/slack-management.md "Running multiple instances"),
    /// the name is rendered as a `[name]` prefix on every message the bridge
    /// posts so a human can tell which machine is talking. When `None`, no
    /// label is added anywhere (no behavior change). Resolved from
    /// `slack.instanceName` in the config file or `KRANZ_SLACK_INSTANCE`, env
    /// winning. User-supplied text: rendered via mrkdwn escaping (see
    /// [`crate::format::escape_mrkdwn`]), never interpreted.
    pub instance_name: Option<String>,
}

impl SlackConfig {
    /// Whether `user_id` may run a money-spending action (new / approve /
    /// start). Pure gate so the authorization policy is unit-tested without a
    /// socket. An empty allowlist is the solo default — anyone is authorized;
    /// a non-empty allowlist authorizes only its listed users. A blank/absent
    /// `user_id` (Slack omitted it) is denied whenever an allowlist is set, so
    /// a spoofed-empty user can't slip past a configured gate.
    pub fn is_authorized(&self, user_id: Option<&str>) -> bool {
        if self.allow_users.is_empty() {
            return true;
        }
        match user_id.map(str::trim).filter(|u| !u.is_empty()) {
            Some(u) => self.allow_users.iter().any(|allowed| allowed == u),
            None => false,
        }
    }
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
    /// Optional spend allowlist (`allowUsers: ["Uxxxx"]`). Absent = empty.
    #[serde(default)]
    allow_users: Vec<String>,
    /// Optional web-dashboard base URL (`dashboardUrl`) for deep-link buttons.
    #[serde(default)]
    dashboard_url: Option<String>,
    /// Optional instance label (`instanceName`) shown on every posted message.
    #[serde(default)]
    instance_name: Option<String>,
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
    dashboard_url: Option<String>,
    instance_name: Option<String>,
}

impl EnvVars {
    fn from_process() -> Self {
        EnvVars {
            bot_token: non_empty_env("KRANZ_SLACK_BOT_TOKEN"),
            app_token: non_empty_env("KRANZ_SLACK_APP_TOKEN"),
            channel: non_empty_env("KRANZ_SLACK_CHANNEL"),
            dashboard_url: non_empty_env("KRANZ_SLACK_DASHBOARD_URL"),
            instance_name: non_empty_env("KRANZ_SLACK_INSTANCE"),
        }
    }
}

/// Read an env var, mapping absent-or-blank to `None`.
fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
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
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(SlackFileConfig::default()),
        Err(e) => return Err(anyhow!("cannot read {}: {e}", path.display())),
    };
    let root: RootConfig = serde_json::from_str(&text)
        .map_err(|e| anyhow!("invalid JSON in {}: {e}", path.display()))?;
    Ok(root.slack.unwrap_or_default())
}

/// Pure resolution of file + env into an optional config. Split out so the
/// precedence and "all three required" logic is unit-testable without any I/O.
fn resolve(file: SlackFileConfig, env: EnvVars) -> Option<SlackConfig> {
    let bot_token = env
        .bot_token
        .or(file.bot_token)
        .map(trim)
        .filter(|s| non_blank(s));
    let app_token = env
        .app_token
        .or(file.app_token)
        .map(trim)
        .filter(|s| non_blank(s));
    let channel = env
        .channel
        .or(file.channel)
        .map(trim)
        .filter(|s| non_blank(s));
    // Optional: env wins over file, blanks drop to None. Never gates enablement.
    let dashboard_url = env
        .dashboard_url
        .or(file.dashboard_url)
        .map(trim)
        .filter(|s| non_blank(s));
    // Optional instance label, same precedence. Never gates enablement.
    let instance_name = env
        .instance_name
        .or(file.instance_name)
        .map(trim)
        .filter(|s| non_blank(s));

    match (bot_token, app_token, channel) {
        (Some(bot_token), Some(app_token), Some(channel)) => Some(SlackConfig {
            bot_token,
            app_token,
            channel,
            notify: file.notify.unwrap_or_default(),
            // Trim each id and drop blanks so a stray "" in the file can't turn
            // a configured allowlist into an all-allow (empty) one by accident.
            allow_users: file
                .allow_users
                .into_iter()
                .map(|u| u.trim().to_string())
                .filter(|u| !u.is_empty())
                .collect(),
            dashboard_url,
            instance_name,
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
                ..EnvVars::default()
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
            notify: Some(NotifyFlags {
                plan_ready: false,
                ..NotifyFlags::default()
            }),
            ..SlackFileConfig::default()
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
            ..SlackFileConfig::default()
        };
        let cfg = resolve(
            file,
            EnvVars {
                channel: Some("Cenv".into()),
                ..EnvVars::default()
            },
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
                ..SlackFileConfig::default()
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
                ..SlackFileConfig::default()
            },
            EnvVars::default(),
        )
        .is_none());
    }

    #[test]
    fn empty_is_none() {
        assert!(resolve(SlackFileConfig::default(), EnvVars::default()).is_none());
    }

    #[test]
    fn allow_users_flow_through_and_are_trimmed() {
        let file = SlackFileConfig {
            bot_token: Some("xoxb".into()),
            app_token: Some("xapp".into()),
            channel: Some("C1".into()),
            notify: None,
            allow_users: vec![" U123 ".into(), "".into(), "  ".into(), "U456".into()],
            ..SlackFileConfig::default()
        };
        let cfg = resolve(file, EnvVars::default()).expect("Some");
        // Blank entries dropped; surviving ids trimmed.
        assert_eq!(
            cfg.allow_users,
            vec!["U123".to_string(), "U456".to_string()]
        );
    }

    #[test]
    fn empty_allowlist_authorizes_anyone() {
        let cfg = resolve(
            SlackFileConfig {
                bot_token: Some("xoxb".into()),
                app_token: Some("xapp".into()),
                channel: Some("C1".into()),
                notify: None,
                allow_users: vec![],
                ..SlackFileConfig::default()
            },
            EnvVars::default(),
        )
        .unwrap();
        assert!(
            cfg.is_authorized(Some("U999")),
            "solo default: anyone allowed"
        );
        assert!(
            cfg.is_authorized(None),
            "even a missing user id is allowed with no list"
        );
    }

    #[test]
    fn dashboard_url_resolves_from_file_and_defaults_to_none() {
        // Absent everywhere → None (no behavior change).
        let cfg = resolve(
            SlackFileConfig {
                bot_token: Some("xoxb".into()),
                app_token: Some("xapp".into()),
                channel: Some("C1".into()),
                notify: None,
                ..SlackFileConfig::default()
            },
            EnvVars::default(),
        )
        .unwrap();
        assert_eq!(cfg.dashboard_url, None);

        // Present in the file → resolved and trimmed.
        let cfg = resolve(
            SlackFileConfig {
                bot_token: Some("xoxb".into()),
                app_token: Some("xapp".into()),
                channel: Some("C1".into()),
                notify: None,
                dashboard_url: Some("  http://127.0.0.1:4600/  ".into()),
                ..SlackFileConfig::default()
            },
            EnvVars::default(),
        )
        .unwrap();
        assert_eq!(cfg.dashboard_url.as_deref(), Some("http://127.0.0.1:4600/"));
    }

    #[test]
    fn dashboard_url_env_wins_over_file() {
        let cfg = resolve(
            SlackFileConfig {
                bot_token: Some("xoxb".into()),
                app_token: Some("xapp".into()),
                channel: Some("C1".into()),
                notify: None,
                dashboard_url: Some("http://file/".into()),
                ..SlackFileConfig::default()
            },
            EnvVars {
                dashboard_url: Some("http://env/".into()),
                ..EnvVars::default()
            },
        )
        .unwrap();
        assert_eq!(cfg.dashboard_url.as_deref(), Some("http://env/"));
    }

    #[test]
    fn instance_name_absent_everywhere_is_none_and_config_matches_pre_field_shape() {
        // BACKCOMPAT: with no `instanceName` in the file and no
        // KRANZ_SLACK_INSTANCE in the env, the resolved config is exactly what
        // it was before the field existed — every other field identical and
        // `instance_name == None` (which renders zero labels anywhere).
        let cfg = resolve(
            SlackFileConfig {
                bot_token: Some("xoxb".into()),
                app_token: Some("xapp".into()),
                channel: Some("C1".into()),
                ..SlackFileConfig::default()
            },
            EnvVars::default(),
        )
        .unwrap();
        assert_eq!(
            cfg,
            SlackConfig {
                bot_token: "xoxb".into(),
                app_token: "xapp".into(),
                channel: "C1".into(),
                notify: NotifyFlags::default(),
                allow_users: vec![],
                dashboard_url: None,
                instance_name: None,
            }
        );
    }

    #[test]
    fn instance_name_parses_from_file_json_camel_case() {
        // The on-disk key is camelCase `instanceName`, like every other field.
        let root: RootConfig = serde_json::from_str(
            r#"{ "slack": { "botToken": "xoxb", "appToken": "xapp",
                 "channel": "C1", "instanceName": "  studio  " } }"#,
        )
        .unwrap();
        let cfg = resolve(root.slack.unwrap(), EnvVars::default()).unwrap();
        assert_eq!(
            cfg.instance_name.as_deref(),
            Some("studio"),
            "resolved and trimmed"
        );

        // And a file WITHOUT the key still parses (older configs keep working).
        let root: RootConfig = serde_json::from_str(
            r#"{ "slack": { "botToken": "xoxb", "appToken": "xapp", "channel": "C1" } }"#,
        )
        .unwrap();
        let cfg = resolve(root.slack.unwrap(), EnvVars::default()).unwrap();
        assert_eq!(cfg.instance_name, None);
    }

    #[test]
    fn instance_name_env_wins_over_file_and_blank_is_none() {
        let file = SlackFileConfig {
            bot_token: Some("xoxb".into()),
            app_token: Some("xapp".into()),
            channel: Some("C1".into()),
            instance_name: Some("file-name".into()),
            ..SlackFileConfig::default()
        };
        // Env wins over the file, same as every other override.
        let cfg = resolve(
            file.clone(),
            EnvVars {
                instance_name: Some("env-name".into()),
                ..EnvVars::default()
            },
        )
        .unwrap();
        assert_eq!(cfg.instance_name.as_deref(), Some("env-name"));

        // No env → the file value is used.
        let cfg = resolve(file, EnvVars::default()).unwrap();
        assert_eq!(cfg.instance_name.as_deref(), Some("file-name"));

        // A blank value (file or env) drops to None — no empty "[] " labels.
        let cfg = resolve(
            SlackFileConfig {
                bot_token: Some("xoxb".into()),
                app_token: Some("xapp".into()),
                channel: Some("C1".into()),
                instance_name: Some("   ".into()),
                ..SlackFileConfig::default()
            },
            EnvVars::default(),
        )
        .unwrap();
        assert_eq!(cfg.instance_name, None);
    }

    #[test]
    fn instance_name_env_var_is_read_from_the_process_environment() {
        // The wiring test for the KRANZ_SLACK_INSTANCE name itself. Serial
        // hazard is nil: no other test in this crate touches the process env.
        // The host machine might legitimately have the var set, so save and
        // restore it around the probe.
        let saved = std::env::var("KRANZ_SLACK_INSTANCE").ok();
        std::env::set_var("KRANZ_SLACK_INSTANCE", " laptop ");
        let set_read = EnvVars::from_process().instance_name;
        std::env::remove_var("KRANZ_SLACK_INSTANCE");
        let absent_read = EnvVars::from_process().instance_name;
        if let Some(v) = saved {
            std::env::set_var("KRANZ_SLACK_INSTANCE", v);
        }
        assert_eq!(set_read.as_deref(), Some("laptop"), "read and trimmed");
        // Absent env var → absent field, the backcompat default.
        assert_eq!(absent_read, None);
    }

    #[test]
    fn nonempty_allowlist_gates_by_id() {
        let cfg = resolve(
            SlackFileConfig {
                bot_token: Some("xoxb".into()),
                app_token: Some("xapp".into()),
                channel: Some("C1".into()),
                notify: None,
                allow_users: vec!["U123".into()],
                ..SlackFileConfig::default()
            },
            EnvVars::default(),
        )
        .unwrap();
        assert!(cfg.is_authorized(Some("U123")), "listed user authorized");
        assert!(
            cfg.is_authorized(Some(" U123 ")),
            "surrounding whitespace tolerated"
        );
        assert!(!cfg.is_authorized(Some("U999")), "unlisted user denied");
        assert!(
            !cfg.is_authorized(None),
            "missing user id denied when a list is set"
        );
        assert!(
            !cfg.is_authorized(Some("   ")),
            "blank user id denied when a list is set"
        );
    }
}
