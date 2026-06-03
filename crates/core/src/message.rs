//! Core types for the mavis-claw message model.
//!
//! Defines the message, role, tool call, and function call structures
//! used throughout the system, following OpenAI-compatible conventions.

use serde::{Deserialize, Serialize};

/// A single message in a conversation.
///
/// Messages follow the OpenAI chat completions format, supporting
/// system prompts, user input, assistant responses, and tool results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// The role of the message sender.
    pub role: Role,
    /// The text content of the message.
    pub content: Option<String>,
    /// Tool calls requested by the assistant (when role is Assistant).
    pub tool_calls: Option<Vec<ToolCall>>,
    /// The tool call ID this message is responding to (when role is Tool).
    pub tool_call_id: Option<String>,
    /// Optional name for the sender (used for tool results).
    pub name: Option<String>,
}

impl Message {
    /// Create a new system message.
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Create a new user message.
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Create a new assistant message.
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }
    }

    /// Create a new tool result message.
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: Some(tool_call_id.into()),
            name: None,
        }
    }
}

/// The role of a message participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// System-level instruction or context.
    System,
    /// Human user input.
    User,
    /// AI assistant response.
    Assistant,
    /// Tool execution result.
    Tool,
}

/// A request from the assistant to call a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique identifier for this tool call.
    pub id: String,
    /// The function to invoke.
    pub function: FunctionCall,
}

/// The function invocation details within a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    /// Name of the function to call.
    pub name: String,
    /// JSON-encoded arguments to pass to the function.
    pub arguments: String,
}
