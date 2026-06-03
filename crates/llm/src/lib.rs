//! LLM provider abstraction layer for mavis-claw.
//!
//! Provides a unified [`LlmProvider`] trait for interacting with various LLM
//! providers (OpenAI-compatible, Anthropic, etc.) via `reqwest` HTTP calls.
//!
//! # Providers
//!
//! - [`OpenAiProvider`] — generic OpenAI-compatible API (OpenAI, DeepSeek, local endpoints, etc.)
//! - [`AnthropicProvider`] — Anthropic Messages API
//!
//! # Registry
//!
//! [`ProviderRegistry`] builds provider instances from [`ProviderConfig`] and
//! supports lookup by provider ID or model name.

mod anthropic;
mod openai;
mod registry;
pub mod retry;
mod trait_def;
mod types;

pub use anthropic::AnthropicProvider;
pub use openai::OpenAiProvider;
pub use registry::ProviderRegistry;
pub use retry::{RetryConfig, RetryProvider};
pub use trait_def::LlmProvider;
pub use types::{ChatRequest, ChatResponse, FinishReason, StreamChunk, ToolCallDelta, Usage};
