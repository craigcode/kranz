//! Uniform JSON error responses: every failure body is `{"error": "..."}`
//! with an appropriate status code. Handlers never panic on corrupt input —
//! corrupt logs / files surface as error responses.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use kranz_engine::error::EngineError;
use serde_json::json;
use std::io::ErrorKind;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(json!({ "error": self.message }))).into_response()
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
