//! Message format conversion between Feishu wire format and internal types.

use crate::{IncomingMessage, MessageContent, Sender};
use super::types::MessageEvent;
use mc_core::McError;

/// Convert a parsed Feishu [`MessageEvent`] into an [`IncomingMessage`].
///
/// Handles the double-encoded content field: Feishu sends
/// `content: "{\"text\":\"hello\"}"` (a JSON string inside a JSON string).
pub fn feishu_to_incoming(event: &MessageEvent) -> Result<IncomingMessage, McError> {
    let content = match event.msg_type.as_str() {
        "text" => parse_text_content(&event.content)?,
        _ => MessageContent::Text(format!("[{}]", event.msg_type)),
    };

    Ok(IncomingMessage {
        channel_id: event.message_id.clone(),
        conversation_key: event.chat_id.clone(),
        sender: Sender {
            id: event.sender_open_id.clone(),
            name: String::new(), // Feishu doesn't include name in the event
        },
        content,
        reply_to: None,
    })
}

/// Parse the double-encoded text content.
///
/// Feishu sends: `"{\"text\":\"hello\"}"` — we need to parse the inner JSON.
fn parse_text_content(raw: &str) -> Result<MessageContent, McError> {
    let inner: serde_json::Value = serde_json::from_str(raw).map_err(|e| {
        McError::Channel(format!("failed to parse feishu text content: {e}"))
    })?;

    let text = inner
        .get("text")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    Ok(MessageContent::Text(text))
}

/// Build the `content` JSON string for the Feishu send message API.
///
/// Feishu expects `content` to be a JSON-encoded *string*, not a JSON object.
pub fn outgoing_to_content(msg: &crate::OutgoingMessage) -> Result<String, McError> {
    match &msg.content {
        MessageContent::Text(text) => {
            let inner = serde_json::json!({ "text": text });
            serde_json::to_string(&inner)
                .map_err(|e| McError::Channel(format!("content serialize failed: {e}")))
        }
        MessageContent::Image { .. } => Err(McError::Channel(
            "image messages not yet supported for Feishu send".into(),
        )),
        MessageContent::File { .. } => Err(McError::Channel(
            "file messages not yet supported for Feishu send".into(),
        )),
    }
}

/// Build the `msg_type` string for the Feishu send message API.
pub fn outgoing_msg_type(msg: &crate::OutgoingMessage) -> &'static str {
    match &msg.content {
        MessageContent::Text(_) => "text",
        MessageContent::Image { .. } => "image",
        MessageContent::File { .. } => "file",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OutgoingMessage;

    fn sample_text_event() -> MessageEvent {
        MessageEvent {
            message_id: "om_abc123".into(),
            chat_id: "oc_chat456".into(),
            msg_type: "text".into(),
            content: r#"{"text":"Hello, world!"}"#.into(),
            sender_open_id: "ou_user789".into(),
            sender_union_id: Some("on_union".into()),
        }
    }

    #[test]
    fn convert_text_event_to_incoming() {
        let event = sample_text_event();
        let msg = feishu_to_incoming(&event).unwrap();

        assert_eq!(msg.channel_id, "om_abc123");
        assert_eq!(msg.conversation_key, "oc_chat456");
        assert_eq!(msg.sender.id, "ou_user789");
        assert!(matches!(&msg.content, MessageContent::Text(t) if t == "Hello, world!"));
        assert!(msg.reply_to.is_none());
    }

    #[test]
    fn convert_unknown_msg_type() {
        let mut event = sample_text_event();
        event.msg_type = "image".into();
        let msg = feishu_to_incoming(&event).unwrap();
        assert!(matches!(&msg.content, MessageContent::Text(t) if t == "[image]"));
    }

    #[test]
    fn text_content_with_mentions() {
        // Feishu text with @mention: the text field may contain @user_id placeholders
        let event = MessageEvent {
            message_id: "om_1".into(),
            chat_id: "oc_1".into(),
            msg_type: "text".into(),
            content: r#"{"text":"@_user_1 hello"}"#.into(),
            sender_open_id: "ou_1".into(),
            sender_union_id: None,
        };
        let msg = feishu_to_incoming(&event).unwrap();
        assert!(matches!(&msg.content, MessageContent::Text(t) if t == "@_user_1 hello"));
    }

    #[test]
    fn outgoing_text_to_content() {
        let msg = OutgoingMessage::text("hi there");
        let content = outgoing_to_content(&msg).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["text"], "hi there");
    }

    #[test]
    fn outgoing_msg_type_text() {
        let msg = OutgoingMessage::text("x");
        assert_eq!(outgoing_msg_type(&msg), "text");
    }

    #[test]
    fn outgoing_image_not_supported() {
        let msg = OutgoingMessage {
            content: MessageContent::Image {
                url: "http://example.com/img.png".into(),
                caption: None,
            },
            reply_to: None,
        };
        let result = outgoing_to_content(&msg);
        assert!(result.is_err());
    }

    #[test]
    fn outgoing_file_not_supported() {
        let msg = OutgoingMessage {
            content: MessageContent::File {
                url: "http://example.com/f.pdf".into(),
                name: "f.pdf".into(),
            },
            reply_to: None,
        };
        let result = outgoing_to_content(&msg);
        assert!(result.is_err());
    }

    #[test]
    fn parse_text_content_malformed_json() {
        let result = parse_text_content("not json");
        assert!(result.is_err());
    }

    #[test]
    fn parse_text_content_missing_text_field() {
        let result = parse_text_content(r#"{"other":"value"}"#).unwrap();
        assert!(matches!(&result, MessageContent::Text(t) if t.is_empty()));
    }
}
