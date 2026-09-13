//! Uniform JSON error responses: every failure body carries `{"error": "..."}`
//! with an appropriate status and an optional stable `code` for recovery.
//! Handlers never panic on corrupt input —
//! corrupt logs / files surface as error responses.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use kranz_engine::error::EngineError;
use serde::Serialize;
use serde_json::json;
use std::io::ErrorKind;

/// Stable recovery hints for clients. Human-readable messages remain separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiErrorCode {
    MissionNotHosted,
    TurnInFlight,
    RepositoryBusy,
    StalePlan,
}

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
    pub code: Option<ApiErrorCode>,
}

impl ApiError {
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
            code: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            code: None,
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
            code: None,
        }
    }

    /// Authentication failed or was not presented (the webhook HMAC).
    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
            code: None,
        }
    }

    /// Authenticated or not, this route refuses the request (webhook for the
    /// wrong repository, or hooks not configured — refused closed).
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
            code: None,
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
            code: None,
        }
    }

    /// A gated merge whose gate suite failed — the request was well-formed
    /// but the mission's state is not mergeable yet.
    pub fn unprocessable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNPROCESSABLE_ENTITY,
            message: message.into(),
            code: None,
        }
    }

    pub fn with_code(mut self, code: ApiErrorCode) -> Self {
        self.code = Some(code);
        self
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.message });
        if let Some(code) = self.code {
            body["code"] = json!(code);
        }
        (self.status, Json(body)).into_response()
    }
}

impl From<EngineError> for ApiError {
    fn from(error: EngineError) -> Self {
        match &error {
            EngineError::Io(io) if io.kind() == ErrorKind::NotFound => {
                ApiError::not_found(error.to_string())
            }
            // Another engine (CLI or a hosted run) holds the single-writer
            // lock, or the mission is in the wrong lifecycle state for the
            // requested transition: conflicts, not server failures.
            EngineError::LockHeld(_) | EngineError::InvalidState(_) => {
                ApiError::conflict(error.to_string())
            }
            // Config errors surface from user-supplied patches (and from
            // config files the message names) — the request is at fault.
            EngineError::Config(_) => ApiError::bad_request(error.to_string()),
            _ => ApiError::internal(error.to_string()),
        }
    }
}
