//! OpenAI-compatible provider implementation.
//!
//! Works with any API that follows the OpenAI chat completions format
//! (OpenAI, DeepSeek, local vLLM/llama.cpp endpoints, etc.).

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use reqwest::Client;
use serde_json::Value;
use tracing::{debug, warn};

use mc_core::{McError, Message, Role, ToolCall, FunctionCall};

use crate::trait_def::LlmProvider;
use crate::types::{ChatRequest, ChatResponse, FinishReason, StreamChunk, ToolCallDelta, Usage};

/// OpenAI-compatible LLM provider.
///
/// Uses `reqwest` to call the `/chat/completions` endpoint directly.
/// Supports both non-streaming and streaming (SSE) modes.
pub struct OpenAiProvider {
    name: String,
    base_url: String,
    api_key: String,
    models: Vec<String>,
    client: Client,
}

impl OpenAiProvider {
    /// Create a new OpenAI-compatible provider.
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

    /// Build the full URL for the chat completions endpoint.
    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }

    /// Serialize a core `Message` into the OpenAI JSON format.
    fn message_to_json(msg: &Message) -> Value {
        let mut obj = serde_json::Map::new();
        obj.insert(
            "role".to_string(),
            Value::String(match msg.role {
                Role::System => "system".to_string(),
                Role::User => "user".to_string(),
                Role::Assistant => "assistant".to_string(),
                Role::Tool => "tool".to_string(),
            }),
        );
        if let Some(ref content) = msg.content {
            obj.insert("content".to_string(), Value::String(content.clone()));
        }
        if let Some(ref tool_calls) = msg.tool_calls {
            let calls: Vec<Value> = tool_calls
                .iter()
                .map(|tc| {
                    serde_json::json!({
                        "id": tc.id,
                        "type": "function",
                        "function": {
                            "name": tc.function.name,
                            "arguments": tc.function.arguments,
                        }
                    })
                })
                .collect();
            obj.insert("tool_calls".to_string(), Value::Array(calls));
        }
        if let Some(ref tool_call_id) = msg.tool_call_id {
            obj.insert(
                "tool_call_id".to_string(),
                Value::String(tool_call_id.clone()),
            );
        }
        Value::Object(obj)
    }

    /// Serialize a core `ToolDefinition` into the OpenAI JSON format.
    fn tool_to_json(tool: &mc_core::ToolDefinition) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.parameters,
            }
        })
    }

    /// Build the request body JSON for a chat completions call.
    fn build_body(&self, request: &ChatRequest) -> Value {
        let messages: Vec<Value> = request.messages.iter().map(Self::message_to_json).collect();

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
        });

        if let Some(tools) = &request.tools {
            let tools_json: Vec<Value> = tools.iter().map(Self::tool_to_json).collect();
            body["tools"] = Value::Array(tools_json);
        }
        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }
        if let Some(temperature) = request.temperature {
            body["temperature"] = serde_json::json!(temperature);
        }
        if request.stream {
            body["stream"] = serde_json::json!(true);
        }

        body
    }

    /// Parse an OpenAI non-streaming response JSON into a `ChatResponse`.
    fn parse_response(json: Value) -> Result<ChatResponse, McError> {
        let choice = json
            .get("choices")
            .and_then(|c| c.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| McError::Llm("No choices in response".to_string()))?;

        let message_obj = choice
            .get("message")
            .ok_or_else(|| McError::Llm("No message in choice".to_string()))?;

        let content = message_obj
            .get("content")
            .and_then(|c| c.as_str())
            .map(|s| s.to_string());

        let tool_calls = message_obj
            .get("tool_calls")
            .and_then(|tc| tc.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|tc| {
                        let id = tc.get("id")?.as_str()?.to_string();
                        let func = tc.get("function")?;
                        let name = func.get("name")?.as_str()?.to_string();
                        let arguments = func.get("arguments")?.as_str()?.to_string();
                        Some(ToolCall {
                            id,
                            function: FunctionCall { name, arguments },
                        })
                    })
                    .collect::<Vec<_>>()
            });

        let message = Message {
            role: Role::Assistant,
            content,
            tool_calls,
            tool_call_id: None,
            name: None,
        };

        let usage = json.get("usage").map(Self::parse_usage).unwrap_or_default();

        let finish_reason = choice
            .get("finish_reason")
            .and_then(|fr| fr.as_str())
            .map(Self::parse_finish_reason)
            .unwrap_or(FinishReason::Stop);

        Ok(ChatResponse {
            message,
            usage,
            finish_reason,
        })
    }

    /// Parse the `usage` JSON object.
    fn parse_usage(val: &Value) -> Usage {
        Usage {
            prompt_tokens: val
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            completion_tokens: val
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
            total_tokens: val
                .get("total_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32,
        }
    }

    /// Parse a finish_reason string.
    fn parse_finish_reason(s: &str) -> FinishReason {
        match s {
            "stop" => FinishReason::Stop,
            "length" => FinishReason::Length,
            "tool_calls" | "function_call" => FinishReason::ToolCalls,
            "content_filter" => FinishReason::ContentFilter,
            other => FinishReason::Other(other.to_string()),
        }
    }

    /// Parse a single SSE delta chunk JSON into zero or more `StreamChunk`s.
    ///
    /// Returns a `Vec` because one SSE event can contain multiple tool call
    /// deltas (e.g., the LLM requesting two tools at once). Each tool call
    /// delta is emitted as a separate `StreamChunk` so that downstream
    /// accumulators can process them by index.
    fn parse_stream_chunks(json: Value) -> Result<Vec<StreamChunk>, McError> {
        let Some(choices) = json.get("choices") else {
            return Ok(vec![]);
        };
        let Some(arr) = choices.as_array() else {
            return Ok(vec![]);
        };
        let Some(choice) = arr.first() else {
            return Ok(vec![]);
        };

        let delta = choice.get("delta");
        let delta_text = delta
            .and_then(|d| d.get("content"))
            .and_then(|c| c.as_str())
            .unwrap_or("")
            .to_string();

        let finish_reason = choice
            .get("finish_reason")
            .and_then(|fr| fr.as_str())
            .map(Self::parse_finish_reason);

        // Parse ALL tool call deltas in the array, not just the first one.
        let tool_call_deltas: Vec<ToolCallDelta> = delta
            .and_then(|d| d.get("tool_calls"))
            .and_then(|tc_arr| tc_arr.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|tc| {
                        let index =
                            tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                        let id = tc
                            .get("id")
                            .and_then(|i| i.as_str())
                            .map(|s| s.to_string());
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .map(|s| s.to_string());
                        let arguments_delta = tc
                            .get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                            .map(|s| s.to_string());
                        ToolCallDelta {
                            index,
                            id,
                            name,
                            arguments_delta,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        // If there's nothing meaningful, return empty.
        if delta_text.is_empty() && tool_call_deltas.is_empty() && finish_reason.is_none() {
            return Ok(vec![]);
        }

        // Emit one StreamChunk per tool call delta (each tagged with its index),
        // plus one chunk for content + finish_reason if either is present.
        let mut chunks = Vec::new();

        if tool_call_deltas.is_empty() {
            // No tool calls — emit a single content/finish chunk.
            chunks.push(StreamChunk {
                delta: delta_text,
                tool_call_delta: None,
                finish_reason,
            });
        } else {
            // Emit one chunk per tool call delta.
            for tc_delta in tool_call_deltas {
                chunks.push(StreamChunk {
                    delta: String::new(),
                    tool_call_delta: Some(tc_delta),
                    finish_reason: None,
                });
            }
            // Attach content (if any) and finish_reason to the first chunk.
            if let Some(first) = chunks.first_mut() {
                if !delta_text.is_empty() {
                    first.delta = delta_text;
                }
                if finish_reason.is_some() {
                    first.finish_reason = finish_reason;
                }
            }
        }

        Ok(chunks)
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, McError> {
        let mut req = request;
        req.stream = false;

        let body = self.build_body(&req);

        debug!(
            provider = %self.name,
            model = %req.model,
            "Sending non-streaming chat request"
        );

        let resp = self
            .client
            .post(self.chat_url())
            .header("Authorization", format!("Bearer {}", self.api_key))
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
                "API returned HTTP {status}: {body_text}"
            )));
        }

        let json: Value = resp
            .json()
            .await
            .map_err(|e| McError::Llm(format!("Failed to parse response JSON: {e}")))?;

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
            "Sending streaming chat request"
        );

        let resp = self
            .client
            .post(self.chat_url())
            .header("Authorization", format!("Bearer {}", self.api_key))
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
                "API returned HTTP {status}: {body_text}"
            )));
        }

        use futures::StreamExt;

        let provider_name = self.name.clone();
        let stream = resp
            .bytes_stream()
            .then(move |chunk_result| {
                let provider_name = provider_name.clone();
                async move {
                    match chunk_result {
                        Ok(bytes) => {
                            let text = String::from_utf8_lossy(&bytes);
                            let mut results: Vec<Result<StreamChunk, McError>> = Vec::new();
                            // SSE format: lines starting with "data: "
                            for line in text.lines() {
                                let line = line.trim();
                                if line.is_empty() || line.starts_with(':') {
                                    continue;
                                }
                                if let Some(data) = line.strip_prefix("data: ") {
                                    let data = data.trim();
                                    if data == "[DONE]" {
                                        continue;
                                    }
                                    match serde_json::from_str::<Value>(data) {
                                        Ok(json) => {
                                            match OpenAiProvider::parse_stream_chunks(json) {
                                                Ok(chunks) => {
                                                    for chunk in chunks {
                                                        results.push(Ok(chunk));
                                                    }
                                                }
                                                Err(e) => results.push(Err(e)),
                                            }
                                        }
                                        Err(e) => {
                                            warn!(
                                                provider = %provider_name,
                                                "Failed to parse SSE JSON: {e}"
                                            );
                                            continue;
                                        }
                                    }
                                }
                            }
                            results
                        }
                        Err(e) => vec![Err(McError::Llm(format!(
                            "Stream read error: {e}"
                        )))],
                    }
                }
            })
            .flat_map(futures::stream::iter);

        Ok(Box::pin(stream))
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn models(&self) -> &[String] {
        &self.models
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_single_tool_call() {
        let json = json!({
            "choices": [{
                "delta": {
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_abc",
                        "function": { "name": "bash", "arguments": "{\"cmd\":\"ls\"}" }
                    }]
                }
            }]
        });
        let chunks = OpenAiProvider::parse_stream_chunks(json).unwrap();
        assert_eq!(chunks.len(), 1);
        let tc = chunks[0].tool_call_delta.as_ref().unwrap();
        assert_eq!(tc.index, 0);
        assert_eq!(tc.id.as_deref(), Some("call_abc"));
        assert_eq!(tc.name.as_deref(), Some("bash"));
        assert_eq!(tc.arguments_delta.as_deref(), Some("{\"cmd\":\"ls\"}"));
    }

    #[test]
    fn parse_multiple_tool_calls_in_one_event() {
        let json = json!({
            "choices": [{
                "delta": {
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": "call_1",
                            "function": { "name": "bash", "arguments": "{\"command\":\"ls\"}" }
                        },
                        {
                            "index": 1,
                            "id": "call_2",
                            "function": { "name": "read_file", "arguments": "{\"path\":\"/tmp\"}" }
                        }
                    ]
                }
            }]
        });
        let chunks = OpenAiProvider::parse_stream_chunks(json).unwrap();
        assert_eq!(chunks.len(), 2);

        let tc0 = chunks[0].tool_call_delta.as_ref().unwrap();
        assert_eq!(tc0.index, 0);
        assert_eq!(tc0.id.as_deref(), Some("call_1"));
        assert_eq!(tc0.name.as_deref(), Some("bash"));

        let tc1 = chunks[1].tool_call_delta.as_ref().unwrap();
        assert_eq!(tc1.index, 1);
        assert_eq!(tc1.id.as_deref(), Some("call_2"));
        assert_eq!(tc1.name.as_deref(), Some("read_file"));
    }

    #[test]
    fn parse_content_and_tool_calls_mixed() {
        let json = json!({
            "choices": [{
                "delta": {
                    "content": "Let me check that",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_xyz",
                        "function": { "name": "grep", "arguments": "{}" }
                    }]
                }
            }]
        });
        let chunks = OpenAiProvider::parse_stream_chunks(json).unwrap();
        assert_eq!(chunks.len(), 1);
        // Content and finish_reason attach to the first chunk
        assert_eq!(chunks[0].delta, "Let me check that");
        assert!(chunks[0].tool_call_delta.is_some());
    }

    #[test]
    fn parse_argument_deltas_only() {
        let json = json!({
            "choices": [{
                "delta": {
                    "tool_calls": [
                        { "index": 0, "function": { "arguments": " -la" } },
                        { "index": 1, "function": { "arguments": "/test" } }
                    ]
                }
            }]
        });
        let chunks = OpenAiProvider::parse_stream_chunks(json).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(
            chunks[0].tool_call_delta.as_ref().unwrap().arguments_delta.as_deref(),
            Some(" -la")
        );
        assert_eq!(
            chunks[1].tool_call_delta.as_ref().unwrap().arguments_delta.as_deref(),
            Some("/test")
        );
    }

    #[test]
    fn parse_finish_reason_with_tool_calls() {
        let json = json!({
            "choices": [{
                "delta": {},
                "finish_reason": "tool_calls"
            }]
        });
        let chunks = OpenAiProvider::parse_stream_chunks(json).unwrap();
        // finish_reason only, no content/tool_calls — still emits one chunk
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].finish_reason.is_some());
    }

    #[test]
    fn parse_empty_delta_returns_empty() {
        let json = json!({
            "choices": [{ "delta": {} }]
        });
        let chunks = OpenAiProvider::parse_stream_chunks(json).unwrap();
        assert!(chunks.is_empty());
    }

    #[test]
    fn parse_three_tool_calls() {
        let json = json!({
            "choices": [{
                "delta": {
                    "tool_calls": [
                        { "index": 0, "id": "c1", "function": { "name": "bash", "arguments": "{}" } },
                        { "index": 1, "id": "c2", "function": { "name": "glob", "arguments": "{}" } },
                        { "index": 2, "id": "c3", "function": { "name": "grep", "arguments": "{}" } }
                    ]
                }
            }]
        });
        let chunks = OpenAiProvider::parse_stream_chunks(json).unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].tool_call_delta.as_ref().unwrap().name.as_deref(), Some("bash"));
        assert_eq!(chunks[1].tool_call_delta.as_ref().unwrap().name.as_deref(), Some("glob"));
        assert_eq!(chunks[2].tool_call_delta.as_ref().unwrap().name.as_deref(), Some("grep"));
    }
}
