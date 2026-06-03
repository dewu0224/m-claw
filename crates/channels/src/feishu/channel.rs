//! Feishu channel implementation.
//!
//! [`FeishuChannel`] implements the [`Channel`] trait for the Feishu/Lark platform.
//! It handles:
//! - Webhook event parsing and signature verification
//! - Sending messages via the Feishu Open API
//! - Tenant access token management with automatic refresh

use std::sync::Arc;

use async_trait::async_trait;
use axum::http::HeaderMap;
use bytes::Bytes;
use tracing::{debug, info, warn};

use crate::{
    Channel, ChannelKind, ChannelSource, ChannelTarget, MessageHandler,
    OutgoingMessage, WebhookChannel, WebhookResponse,
};
use super::types::MessageEvent;
use mc_core::McError;

use super::config::FeishuConfig;
use super::convert::{feishu_to_incoming, outgoing_msg_type, outgoing_to_content};
use super::token::TokenManager;
use super::types::{FeishuWebhookEvent, SendMessageRequest, SendMessageResponse};
use super::verify::verify_signature;

/// Feishu channel adapter.
///
/// Implements the [`Channel`] trait for Feishu/Lark.
/// Uses HTTP + token auth for sending messages and processes webhook
/// events for receiving messages.
pub struct FeishuChannel {
    config: Arc<FeishuConfig>,
    http: reqwest::Client,
    token_mgr: TokenManager,
    handler: Option<Arc<dyn MessageHandler>>,
}

impl Clone for FeishuChannel {
    fn clone(&self) -> Self {
        Self {
            config: Arc::clone(&self.config),
            http: self.http.clone(),
            token_mgr: self.token_mgr.clone(),
            handler: self.handler.clone(),
        }
    }
}

impl std::fmt::Debug for FeishuChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeishuChannel")
            .field("app_id", &self.config.app_id)
            .field("base_url", &self.config.base_url)
            .finish()
    }
}

impl FeishuChannel {
    /// Create a new Feishu channel from configuration.
    pub fn new(config: FeishuConfig) -> Self {
        let config = Arc::new(config);
        let http = reqwest::Client::new();
        let token_mgr = TokenManager::new(Arc::clone(&config), http.clone());
        Self {
            config,
            http,
            token_mgr,
            handler: None,
        }
    }

    /// Create with a pre-built reqwest client (useful for testing with mock servers).
    pub fn with_http_client(config: FeishuConfig, http: reqwest::Client) -> Self {
        let config = Arc::new(config);
        let token_mgr = TokenManager::new(Arc::clone(&config), http.clone());
        Self {
            config,
            http,
            token_mgr,
            handler: None,
        }
    }

    /// Set the message handler for webhook dispatch (builder pattern).
    pub fn with_handler(mut self, handler: Arc<dyn MessageHandler>) -> Self {
        self.handler = Some(handler);
        self
    }

    /// Verify a webhook request and parse the event.
    ///
    /// # Parameters
    /// - `timestamp`: value of `X-Lark-Request-Timestamp` header
    /// - `signature`: value of `X-Lark-Signature` header
    /// - `body`: raw request body bytes
    ///
    /// # Returns
    /// `Some(challenge)` for URL verification events, `None` for message events
    /// (which are dispatched to the handler).
    pub fn verify_and_parse(
        &self,
        timestamp: &str,
        signature: &str,
        body: &[u8],
    ) -> Result<WebhookResult, McError> {
        // Verify signature
        if !verify_signature(timestamp, signature, body, &self.config.app_secret) {
            return Err(McError::Channel("webhook signature verification failed".into()));
        }

        // Parse the event
        let event: FeishuWebhookEvent = serde_json::from_slice(body)
            .map_err(|e| McError::Channel(format!("webhook event parse failed: {e}")))?;

        match event.event_type.as_str() {
            "url_verification" => {
                let challenge = event
                    .challenge
                    .ok_or_else(|| McError::Channel("url_verification missing challenge".into()))?;
                Ok(WebhookResult::Challenge(challenge))
            }
            "event_callback" => {
                let header = event
                    .header
                    .as_ref()
                    .ok_or_else(|| McError::Channel("event_callback missing header".into()))?;

                // Verify token matches our verification_token
                if let Some(token) = &header.token {
                    if token != &self.config.verification_token {
                        return Err(McError::Channel("verification token mismatch".into()));
                    }
                }

                // Only parse im.message.receive_v1 events; ignore others
                if header.event_type != "im.message.receive_v1" {
                    debug!(
                        event_type = %header.event_type,
                        "ignoring unsupported feishu event type"
                    );
                    return Ok(WebhookResult::Ignored);
                }

                let msg_event = self.parse_message_event(&event)?;
                Ok(WebhookResult::Message(msg_event))
            }
            other => {
                debug!(event_type = other, "ignoring unsupported feishu event type");
                Ok(WebhookResult::Ignored)
            }
        }
    }

    /// Extract a `MessageEvent` from the event callback payload.
    fn parse_message_event(&self, event: &FeishuWebhookEvent) -> Result<MessageEvent, McError> {
        let header = event
            .header
            .as_ref()
            .ok_or_else(|| McError::Channel("event_callback missing header".into()))?;

        if header.event_type != "im.message.receive_v1" {
            return Err(McError::Channel(format!(
                "unsupported event type: {}",
                header.event_type
            )));
        }

        let event_data = event
            .event
            .as_ref()
            .ok_or_else(|| McError::Channel("event_callback missing event data".into()))?;

        let message = event_data
            .get("message")
            .ok_or_else(|| McError::Channel("event missing 'message' field".into()))?;

        let message_id = message
            .get("message_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McError::Channel("message missing 'message_id'".into()))?
            .to_string();

        let chat_id = message
            .get("chat_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McError::Channel("message missing 'chat_id'".into()))?
            .to_string();

        let msg_type = message
            .get("message_type")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McError::Channel("message missing 'message_type'".into()))?
            .to_string();

        let content = message
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| McError::Channel("message missing 'content'".into()))?
            .to_string();

        // Extract sender open_id — check both possible locations
        let sender_open_id = event_data
            .get("sender")
            .and_then(|s| s.get("sender_id"))
            .and_then(|sid| sid.get("open_id"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                // Fallback: sender embedded in message
                message
                    .get("sender")
                    .and_then(|s| s.get("sender_id"))
                    .and_then(|sid| sid.get("open_id"))
                    .and_then(|v| v.as_str())
            })
            .ok_or_else(|| McError::Channel("sender open_id not found".into()))?
            .to_string();

        let sender_union_id = event_data
            .get("sender")
            .and_then(|s| s.get("sender_id"))
            .and_then(|sid| sid.get("union_id"))
            .and_then(|v| v.as_str())
            .map(String::from);

        Ok(MessageEvent {
            message_id,
            chat_id,
            msg_type,
            content,
            sender_open_id,
            sender_union_id,
        })
    }

    /// Send a message to a Feishu chat via the Open API.
    async fn send_message_api(
        &self,
        chat_id: &str,
        msg_type: &str,
        content: &str,
        reply_to: Option<&str>,
    ) -> Result<String, McError> {
        let token = self.token_mgr.get_token().await?;

        let request_body = SendMessageRequest {
            receive_id: chat_id.to_string(),
            msg_type: msg_type.to_string(),
            content: content.to_string(),
        };

        let url = if let Some(reply_id) = reply_to {
            format!(
                "{}/open-apis/im/v1/messages/{}/reply",
                self.config.base_url, reply_id
            )
        } else {
            format!(
                "{}/open-apis/im/v1/messages?receive_id_type=chat_id",
                self.config.base_url
            )
        };

        debug!(url = %url, msg_type = msg_type, "sending feishu message");

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&request_body)
            .send()
            .await
            .map_err(|e| McError::Channel(format!("send message request failed: {e}")))?;

        let status = resp.status();
        let send_resp: SendMessageResponse = resp
            .json()
            .await
            .map_err(|e| McError::Channel(format!("send message response parse failed: {e}")))?;

        if send_resp.code != 0 {
            // Auth errors: invalidate token and retry once
            if is_auth_error(send_resp.code) {
                self.token_mgr.invalidate().await;
                return Err(McError::Channel(format!(
                    "Feishu auth error (code {}, HTTP {}): {}",
                    send_resp.code, status, send_resp.msg
                )));
            }
            return Err(McError::Channel(format!(
                "Feishu send error (code {}): {}",
                send_resp.code, send_resp.msg
            )));
        }

        let message_id = send_resp
            .data
            .and_then(|d| d.message_id)
            .unwrap_or_default();

        info!(message_id = %message_id, "feishu message sent");
        Ok(message_id)
    }
}

/// Check if a Feishu API error code indicates an auth issue.
fn is_auth_error(code: i64) -> bool {
    matches!(
        code,
        99991663 | // token invalid
        99991664 | // token expired
        99991668   // app secret invalid
    )
}

/// Result of processing a webhook request.
#[derive(Debug)]
pub enum WebhookResult {
    /// URL verification event — respond with the challenge value.
    Challenge(String),
    /// A message event that should be dispatched.
    Message(MessageEvent),
    /// An event type we don't handle (e.g. chat_member_user_added).
    Ignored,
}

#[async_trait]
impl Channel for FeishuChannel {
    fn kind(&self) -> ChannelKind {
        ChannelKind::Feishu
    }

    async fn start(&self, _handler: Arc<dyn MessageHandler>) -> Result<(), McError> {
        // Feishu uses push-based webhooks — the gateway HTTP server handles
        // incoming requests. The channel itself doesn't start a listener.
        // The gateway calls `verify_and_parse()` for each webhook request,
        // converts to IncomingMessage, and calls `handler.on_message()`.
        //
        // NOTE: For the WebhookChannel path, the handler is set via `with_handler()`.
        // This method is kept for the Channel trait interface.
        info!("Feishu channel started (webhook mode)");
        Ok(())
    }

    async fn send(&self, target: &ChannelTarget, message: OutgoingMessage) -> Result<(), McError> {
        let msg_type = outgoing_msg_type(&message);
        let content = outgoing_to_content(&message)?;

        self.send_message_api(
            &target.conversation_key,
            msg_type,
            &content,
            message.reply_to.as_deref(),
        )
        .await?;

        Ok(())
    }

    async fn stop(&self) -> Result<(), McError> {
        self.token_mgr.invalidate().await;
        info!("Feishu channel stopped");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// WebhookChannel implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl WebhookChannel for FeishuChannel {
    fn kind(&self) -> ChannelKind {
        ChannelKind::Feishu
    }

    async fn handle_webhook(
        &self,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<WebhookResponse, McError> {
        // Extract Feishu signature headers
        let timestamp = headers
            .get("X-Lark-Request-Timestamp")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let signature = headers
            .get("X-Lark-Signature")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        let result = self.verify_and_parse(timestamp, signature, &body)?;

        match result {
            WebhookResult::Challenge(challenge) => {
                info!("Feishu URL verification challenge received");
                Ok(WebhookResponse {
                    status: "ok".into(),
                    message_id: None,
                    challenge: Some(challenge),
                })
            }
            WebhookResult::Message(event) => {
                let message_id = event.message_id.clone();
                info!(message_id = %message_id, "Feishu message event received");

                // Spawn background processing if handler is registered
                if let Some(handler) = &self.handler {
                    let channel = self.clone();
                    let handler = Arc::clone(handler);
                    tokio::spawn(async move {
                        if let Err(e) =
                            dispatch_message_event(&channel, handler.as_ref(), event).await
                        {
                            warn!(error = %e, "Failed to process Feishu message event");
                        }
                    });
                } else {
                    warn!("Feishu message received but no handler registered");
                }

                Ok(WebhookResponse {
                    status: "ok".into(),
                    message_id: Some(message_id),
                    challenge: None,
                })
            }
            WebhookResult::Ignored => Ok(WebhookResponse {
                status: "ok".into(),
                message_id: None,
                challenge: None,
            }),
        }
    }
}

/// Dispatch a message event: convert to IncomingMessage, call handler, send reply.
async fn dispatch_message_event(
    _channel: &FeishuChannel,
    handler: &dyn MessageHandler,
    event: MessageEvent,
) -> Result<(), McError> {
    let incoming = feishu_to_incoming(&event)?;
    let source = ChannelSource {
        channel_kind: ChannelKind::Feishu,
        conversation_key: event.chat_id.clone(),
        sender: crate::Sender {
            id: event.sender_open_id.clone(),
            name: String::new(),
        },
    };

    handler.on_message(source, incoming).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> FeishuConfig {
        FeishuConfig {
            app_id: "cli_test_app".into(),
            app_secret: "test_secret".into(),
            verification_token: "test_verification_token".into(),
            base_url: "https://open.feishu.cn".into(),
        }
    }

    #[test]
    fn channel_kind() {
        let ch = FeishuChannel::new(test_config());
        assert_eq!(Channel::kind(&ch), ChannelKind::Feishu);
    }

    #[test]
    fn debug_format() {
        let ch = FeishuChannel::new(test_config());
        let dbg = format!("{:?}", ch);
        assert!(dbg.contains("FeishuChannel"));
        assert!(dbg.contains("cli_test_app"));
    }

    #[test]
    fn verify_and_parse_challenge() {
        let ch = FeishuChannel::new(test_config());

        let body = serde_json::json!({
            "type": "url_verification",
            "challenge": "abc123challenge",
            "token": "test_verification_token"
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();

        // Compute valid signature
        let timestamp = "1234567890";
        let sig = compute_test_sig(timestamp, &body_bytes, &ch.config.app_secret);

        let result = ch.verify_and_parse(timestamp, &sig, &body_bytes).unwrap();
        match result {
            WebhookResult::Challenge(c) => assert_eq!(c, "abc123challenge"),
            _ => panic!("expected Challenge"),
        }
    }

    #[test]
    fn verify_and_parse_message_event() {
        let ch = FeishuChannel::new(test_config());

        let body = serde_json::json!({
            "schema": "2.0",
            "type": "event_callback",
            "header": {
                "event_id": "ev_123",
                "event_type": "im.message.receive_v1",
                "create_time": "1234567890",
                "token": "test_verification_token",
                "app_id": "cli_test_app",
                "tenant_key": "tenant_1"
            },
            "event": {
                "sender": {
                    "sender_id": {
                        "open_id": "ou_user123",
                        "union_id": "on_union456"
                    },
                    "sender_type": "user"
                },
                "message": {
                    "message_id": "om_msg789",
                    "chat_id": "oc_chat012",
                    "chat_type": "p2p",
                    "message_type": "text",
                    "content": "{\"text\":\"Hello from Feishu!\"}"
                }
            }
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let timestamp = "9999999999";
        let sig = compute_test_sig(timestamp, &body_bytes, &ch.config.app_secret);

        let result = ch.verify_and_parse(timestamp, &sig, &body_bytes).unwrap();
        match result {
            WebhookResult::Message(msg) => {
                assert_eq!(msg.message_id, "om_msg789");
                assert_eq!(msg.chat_id, "oc_chat012");
                assert_eq!(msg.msg_type, "text");
                assert_eq!(msg.content, "{\"text\":\"Hello from Feishu!\"}");
                assert_eq!(msg.sender_open_id, "ou_user123");
                assert_eq!(msg.sender_union_id.as_deref(), Some("on_union456"));
            }
            _ => panic!("expected Message"),
        }
    }

    #[test]
    fn verify_and_parse_bad_signature() {
        let ch = FeishuChannel::new(test_config());
        let body = br#"{"type":"url_verification","challenge":"x"}"#;
        let result = ch.verify_and_parse("123", "badsig", body);
        assert!(result.is_err());
    }

    #[test]
    fn verify_and_parse_token_mismatch() {
        let ch = FeishuChannel::new(test_config());

        let body = serde_json::json!({
            "schema": "2.0",
            "type": "event_callback",
            "header": {
                "event_id": "ev_1",
                "event_type": "im.message.receive_v1",
                "create_time": "123",
                "token": "wrong_token",
                "app_id": "cli_test_app",
                "tenant_key": "t1"
            },
            "event": {
                "sender": {
                    "sender_id": { "open_id": "ou_1" }
                },
                "message": {
                    "message_id": "om_1",
                    "chat_id": "oc_1",
                    "chat_type": "p2p",
                    "message_type": "text",
                    "content": "{\"text\":\"hi\"}"
                }
            }
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let timestamp = "100";
        let sig = compute_test_sig(timestamp, &body_bytes, &ch.config.app_secret);

        let result = ch.verify_and_parse(timestamp, &sig, &body_bytes);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("token mismatch"));
    }

    #[test]
    fn verify_and_parse_ignored_event() {
        let ch = FeishuChannel::new(test_config());

        let body = serde_json::json!({
            "type": "event_callback",
            "header": {
                "event_type": "chat_member_user_added",
                "token": "test_verification_token"
            },
            "event": {}
        });
        let body_bytes = serde_json::to_vec(&body).unwrap();
        let timestamp = "200";
        let sig = compute_test_sig(timestamp, &body_bytes, &ch.config.app_secret);

        let result = ch.verify_and_parse(timestamp, &sig, &body_bytes).unwrap();
        assert!(matches!(result, WebhookResult::Ignored));
    }

    #[test]
    fn verify_and_parse_malformed_json() {
        let ch = FeishuChannel::new(test_config());
        let body = b"not json at all";
        let timestamp = "300";
        let sig = compute_test_sig(timestamp, body, &ch.config.app_secret);

        let result = ch.verify_and_parse(timestamp, &sig, body);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn start_succeeds() {
        let ch = FeishuChannel::new(test_config());
        let handler: Arc<dyn MessageHandler> = Arc::new(crate::tests::NoopHandler);
        ch.start(handler).await.unwrap();
    }

    #[tokio::test]
    async fn stop_succeeds() {
        let ch = FeishuChannel::new(test_config());
        ch.stop().await.unwrap();
    }

    #[test]
    fn is_auth_error_codes() {
        assert!(is_auth_error(99991663));
        assert!(is_auth_error(99991664));
        assert!(is_auth_error(99991668));
        assert!(!is_auth_error(0));
        assert!(!is_auth_error(400));
    }

    #[test]
    fn webhook_result_debug() {
        let r = WebhookResult::Challenge("abc".into());
        let dbg = format!("{:?}", r);
        assert!(dbg.contains("Challenge"));
    }

    /// Helper to compute HMAC-SHA256 signature for tests.
    fn compute_test_sig(timestamp: &str, body: &[u8], secret: &str) -> String {
        use base64::Engine;
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(timestamp.as_bytes());
        mac.update(b"\n");
        mac.update(body);
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    }
}
