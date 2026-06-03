//! Tool trait and definitions for the function-calling system.
//!
//! Tools follow the OpenAI function calling convention. Each tool
//! exposes a [`ToolDefinition`] (name, description, JSON Schema parameters)
//! and an async `execute` method.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::McError;

/// Definition of a tool available to the LLM.
///
/// This structure is sent to the LLM as part of the `tools` parameter
/// in a chat request, following the OpenAI function calling format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Unique name of the tool.
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema describing the tool's parameters.
    pub parameters: Value,
}

/// Trait for executable tools.
///
/// Implement this trait to register a tool with the agent's tool registry.
/// The LLM will see the [`ToolDefinition`] and can invoke [`Tool::execute`]
/// with the appropriate arguments.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Returns the tool definition (name, description, parameters schema).
    fn definition(&self) -> ToolDefinition;

    /// Execute the tool with the given JSON arguments.
    ///
    /// Returns the tool's output as a string, or an error if execution fails.
    async fn execute(&self, args: Value) -> Result<String, McError>;
}
