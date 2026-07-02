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
pub(crate) struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn not_found(message: impl Into<String>) -> Self {
        Self { status: StatusCode::NOT_FOUND, message: message.into() }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self { status: StatusCode::BAD_REQUEST, message: message.into() }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self { status: StatusCode::INTERNAL_SERVER_ERROR, message: message.into() }
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
            _ => ApiError::internal(error.to_string()),
        }
    }
}
