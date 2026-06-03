//! Conversation model and metadata.
//!
//! A `Conversation` holds an ordered list of messages along with
//! metadata such as creation time and an optional title.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Message;

/// A conversation containing an ordered sequence of messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Conversation {
    /// Unique conversation identifier.
    pub id: String,
    /// Ordered list of messages in this conversation.
    pub messages: Vec<Message>,
    /// Metadata about the conversation.
    pub metadata: ConversationMeta,
}

impl Conversation {
    /// Create a new empty conversation with generated ID and current timestamp.
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            messages: Vec::new(),
            metadata: ConversationMeta {
                title: None,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                agent_id: None,
            },
        }
    }

    /// Append a message to the conversation and update the timestamp.
    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
        self.metadata.updated_at = Utc::now();
    }
}

impl Default for Conversation {
    fn default() -> Self {
        Self::new()
    }
}

/// Metadata associated with a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMeta {
    /// Optional human-readable title.
    pub title: Option<String>,
    /// When the conversation was created.
    pub created_at: DateTime<Utc>,
    /// When the conversation was last updated.
    pub updated_at: DateTime<Utc>,
    /// The agent ID handling this conversation.
    pub agent_id: Option<String>,
}
