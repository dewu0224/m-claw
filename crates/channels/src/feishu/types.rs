//! Feishu channel adapter types.
//!
//! Wire-format types for the Feishu/Lark Open API and webhook events.

use serde::{Deserialize, Serialize};

// ── Webhook Event Types ─────────────────────────────────────────────────

/// Top-level webhook request body (Feishu Event v2.0 schema).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FeishuWebhookEvent {
    /// "url_verification" | "event_callback"
    pub schema: Option<String>,
    /// "url_verification" | "im.message.receive_v1"
    #[serde(rename = "type")]
    pub event_type: String,
    /// Present when `type == "url_verification"`.
    pub challenge: Option<String>,
    /// Present when `type == "event_callback"`.
    pub header: Option<EventHeader>,
    /// The actual event payload (for event_callback).
    pub event: Option<serde_json::Value>,
}

/// Event header for `event_callback` type.
#[derive(Debug, Clone, Deserialize)]
pub struct EventHeader {
    /// Event type identifier (e.g. "im.message.receive_v1").
    pub event_type: String,
    /// Token for verifying the event source.
    pub token: Option<String>,
}

/// Extracted fields from the `im.message.receive_v1` event payload.
#[derive(Debug, Clone)]
pub struct MessageEvent {
    /// Feishu message_id (e.g. "om_xxx").
    pub message_id: String,
    /// Chat ID (e.g. "oc_xxx").
    pub chat_id: String,
    /// Message type: "text", "image", "file", etc.
    pub msg_type: String,
    /// JSON-encoded content string (e.g. `{"text":"hello"}`).
    pub content: String,
    /// Sender open_id.
    pub sender_open_id: String,
    /// Sender union_id (optional).
    pub sender_union_id: Option<String>,
}

// ── Token Response ──────────────────────────────────────────────────────

/// Response from the tenant_access_token endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub code: i64,
    pub msg: String,
    pub tenant_access_token: Option<String>,
    /// Token lifetime in seconds (e.g. 7200).
    pub expire: Option<u64>,
}

// ── Send Message Request / Response ─────────────────────────────────────

/// Request body for `POST /im/v1/messages`.
#[derive(Debug, Clone, Serialize)]
pub struct SendMessageRequest {
    /// Chat ID of the target conversation.
    pub receive_id: String,
    /// Message type: "text", "image", "post", etc.
    pub msg_type: String,
    /// JSON-encoded content string.
    pub content: String,
}

/// Response from the send message endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct SendMessageResponse {
    pub code: i64,
    pub msg: String,
    pub data: Option<SendMessageData>,
}

/// Inner data from a successful send response.
#[derive(Debug, Clone, Deserialize)]
pub struct SendMessageData {
    pub message_id: Option<String>,
}
