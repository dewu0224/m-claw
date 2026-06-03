//! Axum route handlers for the gateway.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use bytes::Bytes;
use serde_json::json;

use crate::{AppState, ChatCompletionRequest};

/// `GET /health` — simple liveness probe.
pub(crate) async fn health() -> impl IntoResponse {
    Json(json!({"status": "ok"}))
}

/// `GET /api/status` — runtime status (registered channels, chat API availability).
pub(crate) async fn status(State(state): State<AppState>) -> impl IntoResponse {
    let channel_kinds: Vec<&str> = state.channels.keys().map(|s| s.as_str()).collect();
    Json(json!({
        "status": "running",
        "channels": channel_kinds,
        "chat_api": state.chat_handler.is_some(),
    }))
}

/// `POST /webhook/{channel_kind}` — dispatch incoming webhook to the matching channel.
pub(crate) async fn webhook(
    State(state): State<AppState>,
    Path(channel_kind): Path<String>,
    headers: axum::http::HeaderMap,
    body: Bytes,
) -> Response {
    let key = channel_kind.to_lowercase();
    let channel = match state.channels.get(&key) {
        Some(c) => c,
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("Unknown channel: {channel_kind}")})),
            )
                .into_response();
        }
    };

    match channel.handle_webhook(&headers, body).await {
        Ok(resp) => {
            // For challenge responses, return just the challenge field
            // to satisfy Feishu's URL verification protocol.
            if let Some(challenge) = &resp.challenge {
                return (StatusCode::OK, Json(json!({"challenge": challenge}))).into_response();
            }
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// `POST /v1/chat/completions` — OpenAI-compatible chat endpoint.
pub(crate) async fn chat_completions(
    State(state): State<AppState>,
    Json(request): Json<ChatCompletionRequest>,
) -> Response {
    let handler = match &state.chat_handler {
        Some(h) => h,
        None => {
            return (
                StatusCode::NOT_IMPLEMENTED,
                Json(json!({"error": "Chat API not configured"})),
            )
                .into_response();
        }
    };

    match handler.handle_chat(request).await {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}
