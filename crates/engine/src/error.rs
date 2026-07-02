//! Engine error type.
//!
//! CONTRACT FILE — do not modify in implementation phases. If a change seems
//! necessary, report it instead of editing.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("event log corruption: {0}")]
    LogCorruption(String),

    #[error("another kranz engine holds the lock for this mission: {0}")]
    LockHeld(String),

    #[error("git operation failed: {0}")]
    Git(String),

    #[error("agent backend error: {0}")]
    Backend(String),

    #[error("mission is in an invalid state for this operation: {0}")]
    InvalidState(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, EngineError>;
