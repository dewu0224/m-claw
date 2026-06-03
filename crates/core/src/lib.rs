//! Core types and traits for mavis-claw.
//!
//! This crate provides the foundational types shared across all other crates:
//! - Message and Role models (OpenAI-compatible)
//! - Conversation and metadata
//! - Error types (McError)
//! - Tool trait and definitions (async function calling)

mod channel;
mod conversation;
mod error;
mod message;
mod tool;

pub use channel::ChannelKind;
pub use conversation::{Conversation, ConversationMeta};
pub use error::McError;
pub use message::{FunctionCall, Message, Role, ToolCall};
pub use tool::{Tool, ToolDefinition};
