# mc-llm

LLM provider abstraction layer for mavis-claw.

Provides a unified [`LlmProvider`] trait for interacting with various LLM providers via `reqwest` HTTP calls — no SDK dependencies.

## Modules

| Module | Description |
|--------|-------------|
| `types` | `ChatRequest`, `ChatResponse`, `Usage`, `FinishReason`, `StreamChunk`, `ToolCallDelta` |
| `trait_def` | `LlmProvider` async trait (`chat`, `chat_stream`, `name`, `models`) |
| `openai` | `OpenAiProvider` — generic OpenAI-compatible (OpenAI, DeepSeek, local endpoints, etc.) |
| `anthropic` | `AnthropicProvider` — Anthropic Messages API |
| `registry` | `ProviderRegistry` — builds providers from config, lookup by ID or model |

## Usage

```rust
use mc_config::{ProviderConfig, ProviderKind};
use mc_llm::{ProviderRegistry, ChatRequest, LlmProvider};
use mc_core::Message;

// Build registry from config
let configs = vec![ProviderConfig {
    id: "openai".into(),
    kind: ProviderKind::OpenAI,
    base_url: "https://api.openai.com/v1".into(),
    api_key: "sk-...".into(),
    models: vec!["gpt-4o".into()],
}];
let registry = ProviderRegistry::from_config(&configs)?;

// Lookup by model name
let provider = registry.find_by_model("gpt-4o")?;

// Non-streaming call
let response = provider.chat(ChatRequest {
    model: "gpt-4o".into(),
    messages: vec![Message::user("Hello!")],
    tools: None,
    max_tokens: Some(1024),
    temperature: Some(0.7),
    stream: false,
}).await?;
```

## Dependencies

- `mc-core` — `Message`, `ToolDefinition`, `McError`
- `mc-config` — `ProviderConfig`, `ProviderKind`
- `reqwest` (json, stream) — HTTP client
- `futures` — stream combinators
- `async-trait` — async trait support
- `serde`, `serde_json` — serialization
- `tracing` — logging

## Design Notes

- No SDK crate dependencies — all HTTP calls are raw `reqwest` + `serde_json`
- `OpenAiProvider` uses `base_url` to support any OpenAI-compatible endpoint
- `AnthropicProvider` handles system prompt extraction, `tool_use` content blocks, and Anthropic-specific SSE event types
- Both providers reuse a single `reqwest::Client` instance
- API keys are never logged (only debug-level request metadata)
