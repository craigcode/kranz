//! ACP v1 terminal data and the consumer-owned provider seam.
//!
//! Wire identity is not authority. Providers must validate the supplied scope,
//! consume exact-action authority once, and execute in their admitted boundary.
//! The client does not spawn processes, grant permission or impose containment.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::future::Future;

pub const CREATE: &str = "terminal/create";
pub const OUTPUT: &str = "terminal/output";
pub const WAIT_FOR_EXIT: &str = "terminal/wait_for_exit";
pub const KILL: &str = "terminal/kill";
pub const RELEASE: &str = "terminal/release";
pub const DEFAULT_OUTPUT_BYTES: usize = 64 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
pub const MAX_REQUEST_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvironmentVariable {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Create {
    pub session_id: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: Vec<EnvironmentVariable>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_byte_limit: Option<u64>,
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

impl Create {
    /// Resource/schema checks only. Path and environment policy belong to the
    /// provider; passing this check never supplies execution authority.
    pub fn validate(&self) -> Result<usize, Error> {
        let text = |s: &str| !s.contains('\0');
        if self.session_id.trim().is_empty()
            || self.session_id.len() > 256
            || !text(&self.session_id)
            || self.command.trim().is_empty()
            || self.command.len() > 4096
            || !text(&self.command)
            || self.args.len() > 128
            || self.args.iter().any(|s| !text(s))
            || self.env.len() > 32
            || self.env.iter().any(|v| {
                v.name.is_empty()
                    || v.name.len() > 128
                    || !v
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    || !text(&v.value)
            })
            || self.cwd.as_ref().is_some_and(|s| s.is_empty() || !text(s))
            || self.meta.as_ref().is_some_and(|v| !v.is_object())
            || serde_json::to_vec(self)
                .map_err(|_| Error::InvalidRequest)?
                .len()
                > MAX_REQUEST_BYTES
        {
            return Err(Error::InvalidRequest);
        }
        let mut names = std::collections::BTreeSet::new();
        if self
            .env
            .iter()
            .any(|v| !names.insert(v.name.to_ascii_uppercase()))
        {
            return Err(Error::InvalidRequest);
        }
        let limit = self
            .output_byte_limit
            .unwrap_or(DEFAULT_OUTPUT_BYTES as u64);
        if limit > MAX_OUTPUT_BYTES as u64 {
            return Err(Error::InvalidRequest);
        }
        Ok(limit as usize)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Target {
    pub session_id: String,
    pub terminal_id: String,
    #[serde(default, rename = "_meta", skip_serializing_if = "Option::is_none")]
    pub meta: Option<serde_json::Value>,
}

impl Target {
    pub fn validate(&self) -> Result<(), Error> {
        if [&self.session_id, &self.terminal_id]
            .iter()
            .any(|s| s.trim().is_empty() || s.len() > 256 || s.contains('\0'))
            || self.meta.as_ref().is_some_and(|v| !v.is_object())
            || serde_json::to_vec(self)
                .map_err(|_| Error::InvalidRequest)?
                .len()
                > MAX_REQUEST_BYTES
        {
            return Err(Error::InvalidRequest);
        }
        Ok(())
    }
}

/// Constructed by the trusted consumer, never decoded from peer parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub run_id: String,
    pub engine_session_id: String,
    pub peer_session_id: String,
    pub generation: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExitStatus {
    pub exit_code: Option<u32>,
    pub signal: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputSnapshot {
    pub output: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_status: Option<ExitStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupReceipt {
    pub terminal_id: String,
    pub descendants_reaped: bool,
    pub output_drained: bool,
    pub released: bool,
    pub exit_status: ExitStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidRequest,
    InvalidHandle,
    NotAuthorized,
    Unavailable,
    CleanupUnconfirmed,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidRequest => "terminal request refused by bounds or policy",
            Self::InvalidHandle => "terminal handle is foreign, stale or released",
            Self::NotAuthorized => "terminal action lacks exact one-use authority",
            Self::Unavailable => "terminal provider unavailable or deadline expired",
            Self::CleanupUnconfirmed => "terminal cleanup could not be confirmed",
        })
    }
}

impl std::error::Error for Error {}

/// All waits must be bounded by the consumer, and must not hold up its ACP
/// reader. Dropping an operation is not proof that the command was stopped.
pub trait TerminalProvider: Send + Sync {
    type Authority: Send;

    fn create(
        &self,
        scope: Scope,
        action: Create,
        authority: Self::Authority,
    ) -> impl Future<Output = Result<String, Error>> + Send;
    fn output(
        &self,
        scope: Scope,
        id: String,
    ) -> impl Future<Output = Result<OutputSnapshot, Error>> + Send;
    fn wait_for_exit(
        &self,
        scope: Scope,
        id: String,
    ) -> impl Future<Output = Result<ExitStatus, Error>> + Send;
    fn kill(
        &self,
        scope: Scope,
        id: String,
    ) -> impl Future<Output = Result<CleanupReceipt, Error>> + Send;
    fn release(
        &self,
        scope: Scope,
        id: String,
    ) -> impl Future<Output = Result<CleanupReceipt, Error>> + Send;
}

/// A UTF-8 tail without an in-band truncation marker. Memory and push work are
/// bounded; ACP carries truncation separately from command output.
#[derive(Debug)]
pub struct OutputTail {
    bytes: VecDeque<u8>,
    limit: usize,
    truncated: bool,
}

impl OutputTail {
    pub fn new(limit: usize) -> Result<Self, Error> {
        if limit > MAX_OUTPUT_BYTES {
            return Err(Error::InvalidRequest);
        }
        Ok(Self {
            bytes: VecDeque::new(),
            limit,
            truncated: false,
        })
    }

    pub fn push(&mut self, text: &str) {
        // A single oversized frame cannot make retained allocation unbounded.
        let mut start = text.len().saturating_sub(self.limit);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        if start > 0 {
            self.bytes.clear();
            self.truncated = true;
        }
        for byte in text.as_bytes()[start..].iter().copied() {
            if self.bytes.len() == self.limit {
                self.bytes.pop_front();
                self.truncated = true;
            }
            if self.limit > 0 {
                self.bytes.push_back(byte);
            }
        }
        while self.bytes.front().is_some_and(|b| b & 0xc0 == 0x80) {
            self.bytes.pop_front();
        }
    }

    pub fn snapshot(&self, exit_status: Option<ExitStatus>) -> OutputSnapshot {
        OutputSnapshot {
            output: String::from_utf8(self.bytes.iter().copied().collect())
                .expect("UTF-8 boundary"),
            truncated: self.truncated,
            exit_status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn terminal_schema_refuses_ambiguous_and_excessive_requests() {
        let base = json!({"sessionId":"s","command":"/bin/sh"});
        let valid: Create = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(valid.validate(), Ok(DEFAULT_OUTPUT_BYTES));
        for (key, value) in [
            ("sessionId", json!("s\u{0}")),
            ("command", json!("")),
            ("args", json!(["a\u{0}b"])),
            ("_meta", json!(true)),
            ("outputByteLimit", json!(MAX_OUTPUT_BYTES + 1)),
            (
                "env",
                json!([{"name":"LANG","value":"a"},{"name":"lang","value":"b"}]),
            ),
            ("args", json!(["x".repeat(MAX_REQUEST_BYTES)])),
        ] {
            let mut changed = base.clone();
            changed[key] = value;
            assert_eq!(
                serde_json::from_value::<Create>(changed)
                    .unwrap()
                    .validate(),
                Err(Error::InvalidRequest)
            );
        }
        let mut unknown = base;
        unknown["authority"] = json!("allow");
        assert!(serde_json::from_value::<Create>(unknown).is_err());
        assert!(crate::strict_json::parse(
            br#"{"sessionId":"s","command":"safe","command":"unsafe"}"#
        )
        .is_err());
        assert!(serde_json::from_value::<Create>(
            json!({"sessionId":"s","command":"x","outputByteLimit":-1})
        )
        .is_err());
    }

    #[test]
    fn terminal_tail_obeys_byte_cap_at_every_character_boundary() {
        let chunks = ["abc", "é", "雪", "🦀", "", "tail", "abc雪🦀é"];
        for limit in 0..20 {
            let mut tail = OutputTail::new(limit).unwrap();
            let mut all = String::new();
            for _ in 0..20 {
                for chunk in chunks {
                    all.push_str(chunk);
                    tail.push(chunk);
                    let output = tail.snapshot(None);
                    assert!(output.output.len() <= limit);
                    assert!(all.ends_with(&output.output));
                    assert_eq!(output.truncated, all.len() > limit);
                }
            }
        }
        let mut tail = OutputTail::new(7).unwrap();
        tail.push(&"🦀".repeat(100_000));
        assert_eq!(tail.snapshot(None).output, "🦀");
        assert!(tail.snapshot(None).truncated);
    }

    #[test]
    fn terminal_capability_requires_explicit_consumer_opt_in() {
        let mut ordinary = crate::Client::new();
        let mut configured = crate::Client::with_terminal_support();
        assert_eq!(
            ordinary
                .initialize(json!({"name":"fixture","version":"1"}))
                .unwrap()
                .message["params"]["clientCapabilities"]["terminal"],
            false
        );
        assert_eq!(
            configured
                .initialize(json!({"name":"fixture","version":"1"}))
                .unwrap()
                .message["params"]["clientCapabilities"]["terminal"],
            true
        );
        assert!(configured
            .initialize(json!({"name":"fixture","version":"1"}))
            .is_err());
    }
}
