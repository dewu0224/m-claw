//! The core [`LlmProvider`] async trait.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use mc_core::McError;

use crate::types::{ChatRequest, ChatResponse, StreamChunk};

/// Unified interface for LLM providers.
///
/// Implementors handle protocol-specific details (request format, auth headers,
/// streaming wire format) and return normalized [`ChatResponse`] / [`StreamChunk`]
/// types. This trait is object-safe (`dyn LlmProvider`).
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Perform a non-streaming chat completion.
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, McError>;

    /// Perform a streaming chat completion.
    ///
    /// Returns a stream of [`StreamChunk`] items. The stream ends when the
    /// provider signals completion (a chunk with `finish_reason` set).
    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, McError>> + Send>>, McError>;

    /// The human-readable name / ID of this provider instance.
    fn name(&self) -> &str;

    /// List of model identifiers available through this provider.
    fn models(&self) -> &[String];
}
