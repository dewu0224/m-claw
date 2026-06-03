//! HTTP/WebSocket gateway for mavis-claw.
//!
//! Provides the [`Gateway`] server that receives channel webhooks via
//! `POST /webhook/{channel_kind}`, serves an OpenAI-compatible
//! `POST /v1/chat/completions` endpoint, and exposes health / status routes.
//!
//! Channel integration uses [`mc_channels::WebhookChannel`] as the unified
//! trait for webhook handling, resolving the duplicate trait issue from
//! earlier phases.
//!
//! # Quick start
//!
//! ```rust,no_run
//! use std::sync::Arc;
//! use mc_config::GatewayConfig;
//! use mc_gateway::Gateway;
//!
//! # async fn run() -> Result<(), mc_core::McError> {
//! let gw = Gateway::new(GatewayConfig::default());
//! gw.start().await
//! # }
//! ```

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::routing::{get, post};
use axum::Router;
use mc_config::GatewayConfig;
use mc_core::McError;
use serde::{Deserialize, Serialize};
use tracing::info;

pub use mc_channels::{WebhookChannel, WebhookResponse};

pub mod routes;

// ─── Chat handler trait ─────────────────────────────────────────────────────────

/// Handles OpenAI-compatible chat completion requests.
///
/// Will be implemented by the agent crate during phase-4 integration.
#[async_trait]
pub trait ChatHandler: Send + Sync + 'static {
    async fn handle_chat(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, McError>;
}

// ─── OpenAI-compatible request / response types ─────────────────────────────────

/// Chat completion request (OpenAI-compatible).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub stream: Option<bool>,
}

/// A single message in a chat completion request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// Chat completion response (OpenAI-compatible).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub object: String,
    pub created: i64,
    pub model: String,
    pub choices: Vec<ChatChoice>,
    pub usage: ChatUsage,
}

/// One choice in a chat completion response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatChoice {
    pub index: u32,
    pub message: ChatMessage,
    pub finish_reason: String,
}

/// Token usage statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

// ─── Internal shared state ──────────────────────────────────────────────────────

/// Shared state threaded through axum handlers.
#[derive(Clone)]
pub(crate) struct AppState {
    pub channels: Arc<HashMap<String, Arc<dyn WebhookChannel>>>,
    pub chat_handler: Option<Arc<dyn ChatHandler>>,
}

// ─── Gateway ────────────────────────────────────────────────────────────────────

/// HTTP gateway server.
///
/// Receives channel webhooks, serves the OpenAI-compatible API, and exposes
/// health / status endpoints.
pub struct Gateway {
    config: GatewayConfig,
    channels: Vec<Arc<dyn WebhookChannel>>,
    chat_handler: Option<Arc<dyn ChatHandler>>,
}

impl Gateway {
    /// Create a new gateway with the given configuration.
    pub fn new(config: GatewayConfig) -> Self {
        Self {
            config,
            channels: Vec::new(),
            chat_handler: None,
        }
    }

    /// Register a single channel handler (builder pattern).
    pub fn with_channel(mut self, channel: Arc<dyn WebhookChannel>) -> Self {
        self.channels.push(channel);
        self
    }

    /// Register multiple channel handlers at once (builder pattern).
    pub fn with_channels(mut self, channels: Vec<Arc<dyn WebhookChannel>>) -> Self {
        self.channels.extend(channels);
        self
    }

    /// Set the chat completion handler for the OpenAI-compatible API.
    pub fn with_chat_handler(mut self, handler: Arc<dyn ChatHandler>) -> Self {
        self.chat_handler = Some(handler);
        self
    }

    /// Reference to the gateway configuration.
    pub fn config(&self) -> &GatewayConfig {
        &self.config
    }

    /// Registered channel handlers.
    pub fn channels(&self) -> &[Arc<dyn WebhookChannel>] {
        &self.channels
    }

    /// Build the axum [`Router`] with all registered routes and state.
    pub fn router(&self) -> Router<()> {
        let channels_map: HashMap<String, Arc<dyn WebhookChannel>> = self
            .channels
            .iter()
            .map(|c| {
                let key = c.kind().to_string();
                (key, c.clone())
            })
            .collect();

        let state = AppState {
            channels: Arc::new(channels_map),
            chat_handler: self.chat_handler.clone(),
        };

        Router::new()
            .route("/health", get(routes::health))
            .route("/api/status", get(routes::status))
            .route("/webhook/{channel_kind}", post(routes::webhook))
            .route("/v1/chat/completions", post(routes::chat_completions))
            .with_state(state)
    }

    /// Start the HTTP server. Blocks until the server shuts down.
    pub async fn start(&self) -> Result<(), McError> {
        let addr: SocketAddr = self.config.bind.parse().map_err(|e| {
            McError::Config(format!(
                "Invalid bind address '{}': {}",
                self.config.bind, e
            ))
        })?;

        let router = self.router();
        info!("Gateway listening on {addr}");

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, router).await.map_err(|e| {
            McError::Io(std::io::Error::other(e.to_string()))
        })?;

        Ok(())
    }
}

// ─── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode};
    use serde_json::Value;
    use tower::ServiceExt;

    // ── Mock channel ────────────────────────────────────────────────────────

    struct MockChannel {
        kind: mc_core::ChannelKind,
    }

    #[async_trait]
    impl WebhookChannel for MockChannel {
        fn kind(&self) -> mc_core::ChannelKind {
            self.kind
        }

        async fn handle_webhook(
            &self,
            _headers: &HeaderMap,
            _body: bytes::Bytes,
        ) -> Result<WebhookResponse, McError> {
            Ok(WebhookResponse {
                status: "ok".into(),
                message_id: Some("msg_001".into()),
                challenge: None,
            })
        }
    }

    // ── Mock chat handler ───────────────────────────────────────────────────

    struct MockChatHandler;

    #[async_trait]
    impl ChatHandler for MockChatHandler {
        async fn handle_chat(
            &self,
            request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, McError> {
            let reply = format!("Echo: {}", request.messages.last().unwrap().content);
            Ok(ChatCompletionResponse {
                id: "chatcmpl-test".into(),
                object: "chat.completion".into(),
                created: 1700000000,
                model: request.model,
                choices: vec![ChatChoice {
                    index: 0,
                    message: ChatMessage {
                        role: "assistant".into(),
                        content: reply,
                    },
                    finish_reason: "stop".into(),
                }],
                usage: ChatUsage {
                    prompt_tokens: 10,
                    completion_tokens: 5,
                    total_tokens: 15,
                },
            })
        }
    }

    // ── Failing chat handler ────────────────────────────────────────────────

    struct FailingChatHandler;

    #[async_trait]
    impl ChatHandler for FailingChatHandler {
        async fn handle_chat(
            &self,
            _request: ChatCompletionRequest,
        ) -> Result<ChatCompletionResponse, McError> {
            Err(McError::Channel("LLM provider unavailable".into()))
        }
    }

    // Helper to build a gateway with a mock feishu channel
    fn test_gateway() -> Gateway {
        let channel = Arc::new(MockChannel {
            kind: mc_core::ChannelKind::Feishu,
        });
        Gateway::new(GatewayConfig::default()).with_channel(channel)
    }

    // ── Gateway construction ────────────────────────────────────────────────

    #[test]
    fn gateway_new_has_default_config() {
        let gw = Gateway::new(GatewayConfig::default());
        assert_eq!(gw.config().bind, "127.0.0.1:3777");
        assert!(gw.channels().is_empty());
    }

    #[test]
    fn gateway_builder_registers_channels() {
        let ch = Arc::new(MockChannel {
            kind: mc_core::ChannelKind::Feishu,
        });
        let gw = Gateway::new(GatewayConfig::default()).with_channel(ch);
        assert_eq!(gw.channels().len(), 1);
    }

    #[test]
    fn gateway_builder_registers_multiple_channels() {
        let channels: Vec<Arc<dyn WebhookChannel>> = vec![
            Arc::new(MockChannel { kind: mc_core::ChannelKind::Feishu }),
            Arc::new(MockChannel { kind: mc_core::ChannelKind::WeChat }),
            Arc::new(MockChannel { kind: mc_core::ChannelKind::QQ }),
        ];
        let gw = Gateway::new(GatewayConfig::default()).with_channels(channels);
        assert_eq!(gw.channels().len(), 3);
    }

    // ── Health endpoint ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn health_returns_ok() {
        let app = test_gateway().router();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_body(resp).await;
        assert_eq!(body["status"], "ok");
    }

    // ── Status endpoint ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn status_lists_channels() {
        let app = test_gateway().router();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_body(resp).await;
        assert_eq!(body["status"], "running");
        assert!(body["channels"].as_array().unwrap().contains(&json("feishu")));
        assert_eq!(body["chat_api"], false);
    }

    // ── Webhook endpoint ────────────────────────────────────────────────────

    #[tokio::test]
    async fn webhook_feishu_success() {
        let app = test_gateway().router();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/feishu")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"event":"message"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_body(resp).await;
        assert_eq!(body["status"], "ok");
        assert_eq!(body["message_id"], "msg_001");
    }

    #[tokio::test]
    async fn webhook_case_insensitive() {
        let app = test_gateway().router();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/Feishu")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn webhook_unknown_channel_returns_404() {
        let app = test_gateway().router();
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/webhook/telegram")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let body = collect_body(resp).await;
        assert!(body["error"].as_str().unwrap().contains("telegram"));
    }

    // ── Chat completions endpoint ───────────────────────────────────────────

    #[tokio::test]
    async fn chat_completions_success() {
        let handler = Arc::new(MockChatHandler);
        let app = test_gateway()
            .with_chat_handler(handler)
            .router();

        let req_body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hello"}]
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = collect_body(resp).await;
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["model"], "gpt-4o");
        assert_eq!(body["choices"][0]["message"]["content"], "Echo: hello");
    }

    #[tokio::test]
    async fn chat_completions_not_configured_returns_501() {
        let app = test_gateway().router(); // no chat handler
        let req_body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn chat_completions_handler_error_returns_500() {
        let handler = Arc::new(FailingChatHandler);
        let app = test_gateway()
            .with_chat_handler(handler)
            .router();

        let req_body = serde_json::json!({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        });

        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/chat/completions")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    // ── Request / response serialization ────────────────────────────────────

    #[test]
    fn chat_request_roundtrip() {
        let json_str = r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}],"temperature":0.7}"#;
        let req: ChatCompletionRequest = serde_json::from_str(json_str).unwrap();
        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.messages.len(), 1);
        assert_eq!(req.temperature, Some(0.7));
        assert!(req.max_tokens.is_none());
        assert!(req.stream.is_none());
    }

    #[test]
    fn chat_request_defaults() {
        let json_str = r#"{"model":"gpt-4o","messages":[]}"#;
        let req: ChatCompletionRequest = serde_json::from_str(json_str).unwrap();
        assert!(req.temperature.is_none());
        assert!(req.max_tokens.is_none());
        assert!(req.stream.is_none());
    }

    #[test]
    fn webhook_response_serialization() {
        let resp = WebhookResponse {
            status: "ok".into(),
            message_id: None,
            challenge: None,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(!json.contains("message_id")); // skip_serializing_if
        assert!(!json.contains("challenge")); // skip_serializing_if
    }

    #[test]
    fn webhook_response_with_challenge() {
        let resp = WebhookResponse {
            status: "ok".into(),
            message_id: None,
            challenge: Some("abc123".into()),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("abc123"));
    }

    // ── Helper ──────────────────────────────────────────────────────────────

    async fn collect_body(resp: axum::response::Response) -> Value {
        use http_body_util::BodyExt;
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn json(s: &str) -> Value {
        Value::String(s.to_string())
    }
}
