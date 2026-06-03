//! Channel abstraction layer for mavis-claw.
//!
//! Provides a unified [`Channel`] trait for sending and receiving messages
//! across multiple platforms (Feishu, WeChat, QQ).
//!
//! # Architecture
//!
//! Each platform implements the [`Channel`] trait. Incoming messages are
//! dispatched through a [`MessageHandler`] callback; outgoing messages are
//! sent via [`Channel::send`].
//!
//! For webhook-based channels, the [`WebhookChannel`] trait provides a
//! synchronous webhook handling interface used by the HTTP gateway.

use async_trait::async_trait;
use axum::http::HeaderMap;
use bytes::Bytes;
use mc_core::McError;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Re-exports from mc-core
// ---------------------------------------------------------------------------

pub use mc_core::ChannelKind;

// ---------------------------------------------------------------------------
// Sender
// ---------------------------------------------------------------------------

/// Identifies the sender of a message within a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sender {
    /// Platform-specific user identifier (e.g. open_id for Feishu).
    pub id: String,
    /// Display name (may be empty if unavailable).
    pub name: String,
}

// ---------------------------------------------------------------------------
// MessageContent
// ---------------------------------------------------------------------------

/// The content payload of a channel message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum MessageContent {
    /// Plain text message.
    Text(String),
    /// Image with optional caption.
    Image {
        /// URL pointing to the image resource.
        url: String,
        /// Optional caption text.
        caption: Option<String>,
    },
    /// File attachment.
    File {
        /// URL pointing to the file resource.
        url: String,
        /// Original filename.
        name: String,
    },
}

// ---------------------------------------------------------------------------
// IncomingMessage
// ---------------------------------------------------------------------------

/// A message received from a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncomingMessage {
    /// Platform-specific message identifier (used for reply threading).
    pub channel_id: String,
    /// Channel-internal conversation key (e.g. chat_id, group_id).
    pub conversation_key: String,
    /// The sender of this message.
    pub sender: Sender,
    /// The message content.
    pub content: MessageContent,
    /// If this message is a reply, the channel_id of the original message.
    pub reply_to: Option<String>,
}

// ---------------------------------------------------------------------------
// OutgoingMessage
// ---------------------------------------------------------------------------

/// A message to be sent through a channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutgoingMessage {
    /// The content to send.
    pub content: MessageContent,
    /// Optional message ID this is replying to (for thread/reply support).
    pub reply_to: Option<String>,
}

impl OutgoingMessage {
    /// Create a plain text outgoing message.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: MessageContent::Text(text.into()),
            reply_to: None,
        }
    }

    /// Set the reply-to target.
    pub fn with_reply_to(mut self, target: impl Into<String>) -> Self {
        self.reply_to = Some(target.into());
        self
    }
}

// ---------------------------------------------------------------------------
// ChannelTarget / ChannelSource
// ---------------------------------------------------------------------------

/// Identifies where an outgoing message should be delivered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelTarget {
    /// The channel kind this target belongs to.
    pub channel_kind: ChannelKind,
    /// Channel-internal conversation key (e.g. chat_id).
    pub conversation_key: String,
}

/// Identifies where an incoming message originated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSource {
    /// The channel kind this message came from.
    pub channel_kind: ChannelKind,
    /// Channel-internal conversation key.
    pub conversation_key: String,
    /// The sender.
    pub sender: Sender,
}

// ---------------------------------------------------------------------------
// MessageHandler trait
// ---------------------------------------------------------------------------

/// Callback invoked when a channel receives a new message.
///
/// Implementors process incoming messages (e.g. route to an agent).
#[async_trait]
pub trait MessageHandler: Send + Sync {
    /// Handle an incoming message from a channel.
    async fn on_message(&self, source: ChannelSource, message: IncomingMessage)
        -> Result<(), McError>;
}

// ---------------------------------------------------------------------------
// Channel trait
// ---------------------------------------------------------------------------

/// Unified interface for a messaging channel.
///
/// Each platform (Feishu, WeChat, QQ) implements this trait.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Returns the channel type identifier.
    fn kind(&self) -> ChannelKind;

    /// Start receiving messages (webhook server, WebSocket, polling, etc.).
    ///
    /// Incoming messages are dispatched to the provided [`MessageHandler`].
    async fn start(&self, handler: Arc<dyn MessageHandler>) -> Result<(), McError>;

    /// Send a message to the specified target.
    async fn send(&self, target: &ChannelTarget, message: OutgoingMessage) -> Result<(), McError>;

    /// Stop the channel, cleaning up any resources.
    async fn stop(&self) -> Result<(), McError>;
}

// ---------------------------------------------------------------------------
// WebhookChannel trait
// ---------------------------------------------------------------------------

/// Response produced after processing a webhook request.
///
/// Used by the HTTP gateway to return an appropriate HTTP response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookResponse {
    /// Status string (e.g. "ok").
    pub status: String,
    /// ID of the processed message (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Challenge value for URL verification events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
}

/// Webhook-oriented channel trait for the HTTP gateway.
///
/// This trait is the unified interface that replaces the gateway-local
/// `Channel` trait. Concrete implementations verify signatures, parse
/// platform events, dispatch messages to a [`MessageHandler`], and return
/// a [`WebhookResponse`] suitable for the HTTP layer.
#[async_trait]
pub trait WebhookChannel: Send + Sync + 'static {
    /// Platform kind identifier.
    fn kind(&self) -> ChannelKind;

    /// Process an incoming webhook request.
    ///
    /// Implementations should:
    /// 1. Verify the request signature
    /// 2. Parse the event payload
    /// 3. For challenge events, return the challenge in [`WebhookResponse`]
    /// 4. For message events, dispatch asynchronously to the registered handler
    ///    and return an acknowledgement
    ///
    /// # Parameters
    /// - `headers` — raw HTTP headers (for signature verification)
    /// - `body` — raw request body bytes
    async fn handle_webhook(
        &self,
        headers: &HeaderMap,
        body: Bytes,
    ) -> Result<WebhookResponse, McError>;
}

// ---------------------------------------------------------------------------
// Modules
// ---------------------------------------------------------------------------

pub mod feishu;

// Re-export commonly used feishu types
pub use feishu::{FeishuChannel, FeishuConfig, WebhookResult};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_kind_display() {
        assert_eq!(ChannelKind::Feishu.to_string(), "feishu");
        assert_eq!(ChannelKind::WeChat.to_string(), "wechat");
        assert_eq!(ChannelKind::QQ.to_string(), "qq");
    }

    #[test]
    fn channel_kind_serde_roundtrip() {
        let kinds = [ChannelKind::Feishu, ChannelKind::WeChat, ChannelKind::QQ];
        for kind in &kinds {
            let json = serde_json::to_string(kind).unwrap();
            let back: ChannelKind = serde_json::from_str(&json).unwrap();
            assert_eq!(*kind, back);
        }
    }

    #[test]
    fn channel_kind_json_values() {
        assert_eq!(serde_json::to_string(&ChannelKind::Feishu).unwrap(), "\"feishu\"");
        assert_eq!(serde_json::to_string(&ChannelKind::WeChat).unwrap(), "\"wechat\"");
        assert_eq!(serde_json::to_string(&ChannelKind::QQ).unwrap(), "\"qq\"");
    }

    #[test]
    fn incoming_message_serde_roundtrip() {
        let msg = IncomingMessage {
            channel_id: "msg_001".into(),
            conversation_key: "chat_abc".into(),
            sender: Sender {
                id: "user_1".into(),
                name: "Alice".into(),
            },
            content: MessageContent::Text("hello".into()),
            reply_to: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: IncomingMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.channel_id, "msg_001");
        assert_eq!(back.conversation_key, "chat_abc");
        assert_eq!(back.sender.id, "user_1");
        assert!(matches!(back.content, MessageContent::Text(ref t) if t == "hello"));
        assert!(back.reply_to.is_none());
    }

    #[test]
    fn outgoing_message_builder() {
        let msg = OutgoingMessage::text("hi").with_reply_to("msg_001");
        assert!(matches!(msg.content, MessageContent::Text(ref t) if t == "hi"));
        assert_eq!(msg.reply_to.as_deref(), Some("msg_001"));
    }

    #[test]
    fn message_content_image_serde() {
        let content = MessageContent::Image {
            url: "https://example.com/img.png".into(),
            caption: Some("a photo".into()),
        };
        let json = serde_json::to_string(&content).unwrap();
        let back: MessageContent = serde_json::from_str(&json).unwrap();
        match back {
            MessageContent::Image { url, caption } => {
                assert_eq!(url, "https://example.com/img.png");
                assert_eq!(caption.as_deref(), Some("a photo"));
            }
            _ => panic!("expected Image"),
        }
    }

    #[test]
    fn message_content_file_serde() {
        let content = MessageContent::File {
            url: "https://example.com/doc.pdf".into(),
            name: "doc.pdf".into(),
        };
        let json = serde_json::to_string(&content).unwrap();
        let back: MessageContent = serde_json::from_str(&json).unwrap();
        match back {
            MessageContent::File { url, name } => {
                assert_eq!(url, "https://example.com/doc.pdf");
                assert_eq!(name, "doc.pdf");
            }
            _ => panic!("expected File"),
        }
    }

    #[test]
    fn channel_target_serde_roundtrip() {
        let target = ChannelTarget {
            channel_kind: ChannelKind::Feishu,
            conversation_key: "oc_123".into(),
        };
        let json = serde_json::to_string(&target).unwrap();
        let back: ChannelTarget = serde_json::from_str(&json).unwrap();
        assert_eq!(back.channel_kind, ChannelKind::Feishu);
        assert_eq!(back.conversation_key, "oc_123");
    }

    #[test]
    fn channel_source_serde_roundtrip() {
        let source = ChannelSource {
            channel_kind: ChannelKind::QQ,
            conversation_key: "group_456".into(),
            sender: Sender {
                id: "u789".into(),
                name: "Bob".into(),
            },
        };
        let json = serde_json::to_string(&source).unwrap();
        let back: ChannelSource = serde_json::from_str(&json).unwrap();
        assert_eq!(back.channel_kind, ChannelKind::QQ);
        assert_eq!(back.sender.name, "Bob");
    }

    #[test]
    fn channel_kind_equality_and_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(ChannelKind::Feishu);
        set.insert(ChannelKind::Feishu);
        set.insert(ChannelKind::WeChat);
        assert_eq!(set.len(), 2);
        assert!(set.contains(&ChannelKind::Feishu));
    }

    /// Minimal mock to verify the Channel trait object compiles and is callable.
    struct MockChannel;

    #[async_trait]
    impl Channel for MockChannel {
        fn kind(&self) -> ChannelKind {
            ChannelKind::Feishu
        }

        async fn start(&self, _handler: Arc<dyn MessageHandler>) -> Result<(), McError> {
            Ok(())
        }

        async fn send(
            &self,
            _target: &ChannelTarget,
            _message: OutgoingMessage,
        ) -> Result<(), McError> {
            Ok(())
        }

        async fn stop(&self) -> Result<(), McError> {
            Ok(())
        }
    }

    struct MockHandler;

    #[async_trait]
    impl MessageHandler for MockHandler {
        async fn on_message(
            &self,
            _source: ChannelSource,
            _message: IncomingMessage,
        ) -> Result<(), McError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn mock_channel_trait_object() {
        let channel: Arc<dyn Channel> = Arc::new(MockChannel);
        assert_eq!(channel.kind(), ChannelKind::Feishu);

        let handler: Arc<dyn MessageHandler> = Arc::new(MockHandler);
        channel.start(handler).await.unwrap();

        let target = ChannelTarget {
            channel_kind: ChannelKind::Feishu,
            conversation_key: "chat_1".into(),
        };
        let msg = OutgoingMessage::text("test");
        channel.send(&target, msg).await.unwrap();

        channel.stop().await.unwrap();
    }

    #[tokio::test]
    async fn mock_handler_receives_message() {
        let handler = MockHandler;
        let source = ChannelSource {
            channel_kind: ChannelKind::Feishu,
            conversation_key: "chat_1".into(),
            sender: Sender {
                id: "u1".into(),
                name: "Test".into(),
            },
        };
        let msg = IncomingMessage {
            channel_id: "m1".into(),
            conversation_key: "chat_1".into(),
            sender: Sender {
                id: "u1".into(),
                name: "Test".into(),
            },
            content: MessageContent::Text("hello".into()),
            reply_to: None,
        };
        handler.on_message(source, msg).await.unwrap();
    }

    /// No-op handler used by feishu submodule tests.
    pub struct NoopHandler;

    #[async_trait]
    impl MessageHandler for NoopHandler {
        async fn on_message(
            &self,
            _source: ChannelSource,
            _message: IncomingMessage,
        ) -> Result<(), McError> {
            Ok(())
        }
    }
}
