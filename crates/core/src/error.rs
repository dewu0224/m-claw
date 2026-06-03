//! Error types for mavis-claw.
//!
//! [`McError`] is the unified error enum used across all crates.
//! Application-level code should use `anyhow::Result` for convenience,
//! while library crates return `Result<T, McError>` for typed errors.

use thiserror::Error;

/// Unified error type for all mavis-claw operations.
#[derive(Debug, Error)]
pub enum McError {
    /// Error from the LLM provider (API call failed, rate limited, etc.).
    #[error("LLM error: {0}")]
    Llm(String),

    /// Error during tool execution.
    #[error("Tool error: {0}")]
    Tool(String),

    /// Configuration loading or validation error.
    #[error("Config error: {0}")]
    Config(String),

    /// File system or network I/O error.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Channel communication error (Feishu, WeChat, QQ, etc.).
    #[error("Channel error: {0}")]
    Channel(String),

    /// Serialization or deserialization error (JSON).
    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Skill loading or parsing error.
    #[error("Skill error: {0}")]
    Skill(String),

    /// Storage or database error (SQLite, session persistence).
    #[error("Storage error: {0}")]
    Storage(String),
}
