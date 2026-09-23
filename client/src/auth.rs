use crate::state::AppState;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use std::sync::Arc;

/// Optional bearer-token gate for `/api/*`, enabled by TOKSCALE_API_TOKEN.
/// Same credential convention the future Cloudflare Worker ingest will use.
pub async fn require_token(
    State(state): State<Arc<AppState>>,
    req: Request,
    next: Next,
) -> Response {
    let Some(expected) = state.cfg.api_token.as_deref() else {
        return next.run(req).await;
    };
    let authorized = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| token == expected);
    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({
                "error": { "code": "unauthorized", "message": "missing or invalid bearer token" }
            })),
        )
            .into_response();
    }
    next.run(req).await
}
