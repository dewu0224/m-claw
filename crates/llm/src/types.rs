//! Request and response types for the LLM provider abstraction.

use mc_core::{Message, ToolDefinition};

/// A chat completion request to an LLM provider.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    /// Model identifier (e.g., "gpt-4o", "claude-sonnet-4-20250514").
    pub model: String,
    /// Conversation messages.
    pub messages: Vec<Message>,
    /// Optional tool definitions for function calling.
    pub tools: Option<Vec<ToolDefinition>>,
    /// Maximum tokens in the response.
    pub max_tokens: Option<u32>,
    /// Sampling temperature (0.0 - 2.0).
    pub temperature: Option<f32>,
    /// Whether to stream the response.
    pub stream: bool,
}

/// A non-streaming chat completion response.
#[derive(Debug, Clone)]
pub struct ChatResponse {
    /// The assistant's response message.
    pub message: Message,
    /// Token usage statistics.
    pub usage: Usage,
    /// Why the model stopped generating.
    pub finish_reason: FinishReason,
}

/// Token usage statistics from an LLM call.
#[derive(Debug, Clone, Default)]
pub struct Usage {
    /// Number of tokens in the prompt.
    pub prompt_tokens: u32,
    /// Number of tokens in the completion.
    pub completion_tokens: u32,
    /// Total tokens used (prompt + completion).
    pub total_tokens: u32,
}

/// Reason the model stopped generating tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    /// The model reached a natural stopping point.
    Stop,
    /// The model hit the max_tokens limit.
    Length,
    /// The model requested a tool call.
    ToolCalls,
    /// Content was filtered by safety systems.
    ContentFilter,
    /// Other or unknown reason.
    Other(String),
}

/// A single chunk in a streaming response.
#[derive(Debug, Clone)]
pub struct StreamChunk {
    /// Incremental text content delta.
    pub delta: String,
    /// Incremental tool call information (if the model is calling tools).
    pub tool_call_delta: Option<ToolCallDelta>,
    /// If present, the stream is ending with this reason.
    pub finish_reason: Option<FinishReason>,
}

/// Incremental tool call data in a stream chunk.
#[derive(Debug, Clone)]
pub struct ToolCallDelta {
    /// Index of the tool call in the array (may be omitted on continuation).
    pub index: u32,
    /// The tool call ID (sent on the first chunk for this call).
    pub id: Option<String>,
    /// The function name (sent on the first chunk for this call).
    pub name: Option<String>,
    /// Incremental function arguments JSON fragment.
    pub arguments_delta: Option<String>,
}
