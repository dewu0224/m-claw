//! Anthropic Messages API provider implementation.
//!
//! Calls the Anthropic `/v1/messages` endpoint directly via `reqwest`.
//! Handles Anthropic-specific requirements:
//! - System prompt is a top-level `system` field (not a message)
//! - `anthropic-version` header required
//! - Streaming uses SSE with `event:` types

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use reqwest::Client;
use serde_json::Value;
use tracing::{debug, warn};

use mc_core::{McError, Message, Role, ToolCall, FunctionCall, ToolDefinition};

use crate::trait_def::LlmProvider;
use crate::types::{ChatRequest, ChatResponse, FinishReason, StreamChunk, ToolCallDelta, Usage};

/// Anthropic Messages API provider.
///
/// Uses `reqwest` to call the `/v1/messages` endpoint directly.
/// Supports both non-streaming and streaming (SSE) modes.
pub struct AnthropicProvider {
    name: String,
    base_url: String,
    api_key: String,
    models: Vec<String>,
    client: Client,
}

impl AnthropicProvider {
    /// Create a new Anthropic provider.
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        models: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            models,
            client: Client::new(),
        }
    }

    /// Build the full URL for the messages endpoint.
    fn messages_url(&self) -> String {
        format!("{}/v1/messages", self.base_url)
    }

    /// Convert core `Message` list to Anthropic format, extracting system prompt.
    ///
    /// Anthropic requires:
    /// - System prompt as a top-level `system` field, not a message
    /// - Only `user` and `assistant` roles in the messages array
    /// - Consecutive messages of the same role must be merged
    fn convert_messages(messages: &[Message]) -> (Option<String>, Vec<Value>) {
        let mut system: Option<String> = None;
        let mut api_messages: Vec<Value> = Vec::new();

        for msg in messages {
            match msg.role {
                Role::System => {
                    // Anthropic takes system as a top-level string
                    let content = msg.content.as_deref().unwrap_or("");
                    system = Some(match system {
                        Some(existing) => format!("{existing}\n{content}"),
                        None => content.to_string(),
                    });
                }
                Role::User => {
                    let content = msg.content.as_deref().unwrap_or("");
                    api_messages.push(serde_json::json!({
                        "role": "user",
                        "content": content,
                    }));
                }
                Role::Assistant => {
                    // If there are tool calls, build content blocks
                    if let Some(tool_calls) = &msg.tool_calls {
                        let mut blocks: Vec<Value> = Vec::new();
                        if let Some(content) = &msg.content {
                            blocks.push(serde_json::json!({
                                "type": "text",
                                "text": content,
                            }));
                        }
                        for tc in tool_calls {
                            // Parse arguments as JSON value
                            let args: Value =
                                serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Object(Default::default()));
                            blocks.push(serde_json::json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.function.name,
                                "input": args,
                            }));
                        }
                        api_messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": blocks,
                        }));
                    } else {
                        let content = msg.content.as_deref().unwrap_or("");
                        api_messages.push(serde_json::json!({
                            "role": "assistant",
                            "content": content,
                        }));
                    }
                }
                Role::Tool => {
                    // Tool results go as a user message with tool_result block
                    let tool_call_id = msg.tool_call_id.as_deref().unwrap_or("");
                    let content = msg.content.as_deref().unwrap_or("");
                    api_messages.push(serde_json::json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": tool_call_id,
                            "content": content,
                        }],
                    }));
                }
            }
        }

        (system, api_messages)
    }

    /// Convert core `ToolDefinition` to Anthropic tool format.
    fn tool_to_json(tool: &ToolDefinition) -> Value {
        serde_json::json!({
            "name": tool.name,
            "description": tool.description,
            "input_schema": tool.parameters,
        })
    }

    /// Build the request body JSON.
    fn build_body(&self, request: &ChatRequest) -> Value {
        let (system, messages) = Self::convert_messages(&request.messages);

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
        });

        if let Some(system_text) = system {
            body["system"] = Value::String(system_text);
        }

        if let Some(tools) = &request.tools {
            let tools_json: Vec<Value> = tools.iter().map(Self::tool_to_json).collect();
            body["tools"] = Value::Array(tools_json);
        }

        // Anthropic uses `max_tokens` as a required field; provide a sensible default
        body["max_tokens"] = serde_json::json!(request.max_tokens.unwrap_or(4096));

        if let Some(temperature) = request.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }

        if request.stream {
            body["stream"] = serde_json::json!(true);
        }

        body
    }

    /// Parse an Anthropic non-streaming response into a `ChatResponse`.
    fn parse_response(json: Value) -> Result<ChatResponse, McError> {
        let stop_reason = json
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("end_turn");

        let finish_reason = match stop_reason {
            "end_turn" | "stop_sequence" => FinishReason::Stop,
            "max_tokens" => FinishReason::Length,
            "tool_use" => FinishReason::ToolCalls,
            other => FinishReason::Other(other.to_string()),
        };

        // Parse content blocks
        let content_blocks = json
            .get("content")
            .and_then(|c| c.as_array())
            .ok_or_else(|| McError::Llm("No content in Anthropic response".to_string()))?;

        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        for block in content_blocks {
            match block.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                        text_parts.push(text.to_string());
                    }
                }
                Some("tool_use") => {
                    let id = block
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("")
                        .to_string();
                    let input = block.get("input").cloned().unwrap_or(Value::Object(Default::default()));
                    let arguments = serde_json::to_string(&input).unwrap_or_else(|_| "{}".to_string());
                    tool_calls.push(ToolCall {
                        id,
                        function: FunctionCall { name, arguments },
                    });
                }
                _ => {}
            }
        }

        let content = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join(""))
        };

        let message = Message {
            role: Role::Assistant,
            content,
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            tool_call_id: None,
            name: None,
        };

        let usage = json.get("usage").map(|u| Usage {
            prompt_tokens: u
                .get("input_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            completion_tokens: u
                .get("output_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            total_tokens: {
                let input = u
                    .get("input_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                let output = u
                    .get("output_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                input + output
            },
        }).unwrap_or_default();

        Ok(ChatResponse {
            message,
            usage,
            finish_reason,
        })
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, McError> {
        let mut req = request;
        req.stream = false;

        let body = self.build_body(&req);

        debug!(
            provider = %self.name,
            model = %req.model,
            "Sending non-streaming Anthropic request"
        );

        let resp = self
            .client
            .post(self.messages_url())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| McError::Llm(format!("HTTP request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(McError::Llm(format!(
                "Anthropic API returned HTTP {status}: {body_text}"
            )));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| McError::Llm(format!("Failed to parse Anthropic response: {e}")))?;

        Self::parse_response(json)
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, McError>> + Send>>, McError> {
        let mut req = request;
        req.stream = true;

        let body = self.build_body(&req);

        debug!(
            provider = %self.name,
            model = %req.model,
            "Sending streaming Anthropic request"
        );

        let resp = self
            .client
            .post(self.messages_url())
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| McError::Llm(format!("HTTP request failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(McError::Llm(format!(
                "Anthropic API returned HTTP {status}: {body_text}"
            )));
        }

        use futures::StreamExt;

        // Anthropic SSE events have `event: <type>` and `data: <json>` lines.
        // We accumulate a line buffer and current event type across chunks.
        let stream = resp.bytes_stream().flat_map(move |chunk_result| {
            match chunk_result {
                Ok(bytes) => {
                    let text = String::from_utf8_lossy(&bytes);
                    let mut results: Vec<Result<StreamChunk, McError>> = Vec::new();
                    // Process lines; we track the last-seen event type in a simple
                    // stack approach: each `event:` line is immediately followed by
                    // its `data:` line(s). This works for Anthropic's SSE format.
                    let mut current_event = String::new();

                    for line in text.lines() {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        if let Some(event_type) = line.strip_prefix("event: ") {
                            current_event = event_type.trim().to_string();
                            continue;
                        }
                        if let Some(data) = line.strip_prefix("data: ") {
                            let data = data.trim();
                            match serde_json::from_str::<Value>(data) {
                                Ok(json) => {
                                    match parse_anthropic_stream_event(
                                        &current_event,
                                        &json,
                                    ) {
                                        Ok(Some(chunk)) => results.push(Ok(chunk)),
                                        Ok(None) => {}
                                        Err(e) => results.push(Err(e)),
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to parse Anthropic SSE JSON: {e}"
                                    );
                                }
                            }
                        }
                    }

                    futures::stream::iter(results)
                }
                Err(e) => futures::stream::iter(vec![Err(McError::Llm(
                    format!("Stream read error: {e}"),
                ))]),
            }
        });
        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn models(&self) -> &[String] {
        &self.models
    }
}

/// Parse a single Anthropic SSE event into an optional `StreamChunk`.
fn parse_anthropic_stream_event(event_type: &str, json: &Value) -> Result<Option<StreamChunk>, McError> {
    match event_type {
        "content_block_delta" => {
            let delta = json.get("delta");
            let delta_type = delta.and_then(|d| d.get("type")).and_then(|t| t.as_str());

            match delta_type {
                Some("text_delta") => {
                    let text = delta
                        .and_then(|d| d.get("text"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string();
                    if text.is_empty() {
                        return Ok(None);
                    }
                    Ok(Some(StreamChunk {
                        delta: text,
                        tool_call_delta: None,
                        finish_reason: None,
                    }))
                }
                Some("input_json_delta") => {
                    let partial_json = delta
                        .and_then(|d| d.get("partial_json"))
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string();
                    // We need the index from content_block_start to know which tool call
                    let index = json
                        .get("index")
                        .and_then(|i| i.as_u64())
                        .unwrap_or(0) as u32;
                    Ok(Some(StreamChunk {
                        delta: String::new(),
                        tool_call_delta: Some(ToolCallDelta {
                            index,
                            id: None,
                            name: None,
                            arguments_delta: Some(partial_json),
                        }),
                        finish_reason: None,
                    }))
                }
                _ => Ok(None),
            }
        }
        "content_block_start" => {
            let content_block = json.get("content_block");
            let block_type = content_block
                .and_then(|cb| cb.get("type"))
                .and_then(|t| t.as_str());

            if block_type == Some("tool_use") {
                let index = json
                    .get("index")
                    .and_then(|i| i.as_u64())
                    .unwrap_or(0) as u32;
                let id = content_block
                    .and_then(|cb| cb.get("id"))
                    .and_then(|i| i.as_str())
                    .map(|s| s.to_string());
                let name = content_block
                    .and_then(|cb| cb.get("name"))
                    .and_then(|n| n.as_str())
                    .map(|s| s.to_string());
                Ok(Some(StreamChunk {
                    delta: String::new(),
                    tool_call_delta: Some(ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments_delta: None,
                    }),
                    finish_reason: None,
                }))
            } else {
                Ok(None)
            }
        }
        "message_delta" => {
            let stop_reason = json
                .get("delta")
                .and_then(|d| d.get("stop_reason"))
                .and_then(|sr| sr.as_str());

            let finish_reason = stop_reason.map(|sr| match sr {
                "end_turn" | "stop_sequence" => FinishReason::Stop,
                "max_tokens" => FinishReason::Length,
                "tool_use" => FinishReason::ToolCalls,
                other => FinishReason::Other(other.to_string()),
            });

            Ok(Some(StreamChunk {
                delta: String::new(),
                tool_call_delta: None,
                finish_reason,
            }))
        }
        "message_start" | "ping" | "content_block_stop" => Ok(None),
        other => {
            debug!("Unknown Anthropic SSE event type: {other}");
            Ok(None)
        }
    }
}
