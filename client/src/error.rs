use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug)]
pub enum AppError {
    BadRequest(String),
    Core(String),
    ScanPending,
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            AppError::BadRequest(message) => (StatusCode::BAD_REQUEST, "bad_request", message),
            AppError::Core(message) => (StatusCode::INTERNAL_SERVER_ERROR, "core_error", message),
            AppError::ScanPending => (
                StatusCode::SERVICE_UNAVAILABLE,
                "scan_pending",
                "initial scan has not completed yet; retry shortly".to_string(),
            ),
        };
        (
            status,
            Json(json!({ "error": { "code": code, "message": message } })),
        )
            .into_response()
    }
}
