//! Tiny typed Slack Web API client (reqwest) — no SDK.
//!
//! We touch exactly two Web API methods:
//! - `apps.connections.open` (auth: **app-level** token) → the ephemeral
//!   `wss://` URL for Socket Mode. Slack rotates this URL, so the bridge
//!   re-opens it on every (re)connect.
//! - `chat.postMessage` (auth: **bot** token) → post a message (optionally as a
//!   threaded reply). Returns the message `ts`, which becomes the mission's
//!   thread root the first time and is reused for every threaded follow-up.
//!
//! Slack Web API idiom: HTTP is 200 even for logical failures; the real result
//! is `{"ok": true|false, "error": "…"}` in the JSON body, so every call checks
//! `ok` and surfaces the `error` string.

use anyhow::{anyhow, Context, Result};
use serde_json::Value;

const POST_MESSAGE_URL: &str = "https://slack.com/api/chat.postMessage";
const CONNECTIONS_OPEN_URL: &str = "https://slack.com/api/apps.connections.open";

/// Authenticated Slack Web API client. Holds the bot + app tokens and a reused
/// `reqwest::Client` (connection pooling).
#[derive(Clone)]
pub struct SlackClient {
    http: reqwest::Client,
    bot_token: String,
    app_token: String,
}

impl std::fmt::Debug for SlackClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the tokens.
        f.debug_struct("SlackClient").finish_non_exhaustive()
    }
}

impl SlackClient {
    /// Build a client from a resolved config.
    pub fn new(cfg: &crate::config::SlackConfig) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .build()
                .context("building reqwest client")?,
            bot_token: cfg.bot_token.clone(),
            app_token: cfg.app_token.clone(),
        })
    }

    /// `apps.connections.open` with the app-level token → the `wss://` URL to
    /// dial for Socket Mode. The URL is single-use-ish and rotates, so callers
    /// fetch a fresh one before every connect/reconnect.
    pub async fn open_connection(&self) -> Result<String> {
        let resp = self
            .http
            .post(CONNECTIONS_OPEN_URL)
            .bearer_auth(&self.app_token)
            .send()
            .await
            .context("apps.connections.open request failed")?;
        let body: Value = resp
            .json()
            .await
            .context("apps.connections.open: response was not JSON")?;
        check_ok(&body, "apps.connections.open")?;
        body.get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("apps.connections.open: response missing `url`"))
    }

    /// `chat.postMessage` with the bot token. `blocks` is the Block Kit array;
    /// `thread_ts` threads the message under an existing root when `Some`.
    /// Returns the new message's `ts`.
    pub async fn post_message(
        &self,
        channel: &str,
        blocks: &[Value],
        thread_ts: Option<&str>,
    ) -> Result<String> {
        let mut payload = serde_json::json!({
            "channel": channel,
            "blocks": blocks,
        });
        if let Some(ts) = thread_ts {
            payload["thread_ts"] = Value::String(ts.to_string());
        }

        let resp = self
            .http
            .post(POST_MESSAGE_URL)
            .bearer_auth(&self.bot_token)
            .json(&payload)
            .send()
            .await
            .context("chat.postMessage request failed")?;
        let body: Value = resp
            .json()
            .await
            .context("chat.postMessage: response was not JSON")?;
        check_ok(&body, "chat.postMessage")?;
        body.get("ts")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("chat.postMessage: response missing `ts`"))
    }
}

/// Surface a Slack `{ "ok": false, "error": "…" }` body as a Rust error.
fn check_ok(body: &Value, method: &str) -> Result<()> {
    if body.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(());
    }
    let err = body.get("error").and_then(Value::as_str).unwrap_or("unknown_error");
    Err(anyhow!("{method} failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn check_ok_accepts_ok_true() {
        assert!(check_ok(&json!({ "ok": true, "ts": "1.2" }), "m").is_ok());
    }

    #[test]
    fn check_ok_surfaces_error_string() {
        let err = check_ok(&json!({ "ok": false, "error": "channel_not_found" }), "chat.postMessage")
            .unwrap_err()
            .to_string();
        assert!(err.contains("channel_not_found"));
        assert!(err.contains("chat.postMessage"));
    }

    #[test]
    fn check_ok_handles_missing_ok() {
        assert!(check_ok(&json!({}), "m").is_err());
    }
}
