# mc-agent

Agent conversation runtime for mavis-claw.

**Status:** Phase 1 implemented — basic conversation loop with streaming support.

## Overview

The `Agent` struct manages a conversation with an LLM provider. It composes
a system prompt from configuration, tracks conversation history, and delegates
LLM calls to the configured provider.

## Architecture

```text
User message
  → build_system_prompt()  (base prompt + config system_prompt)
  → LLM provider.chat[_stream]()
  → assistant response (streamed token-by-token)
  → conversation history updated
```

## Key Types

| Type | Description |
|------|-------------|
| `Agent` | Core struct — holds provider, config, conversation, system prompt |
| `ToolExecutor` | Trait for tool execution (Phase 2+, currently stubbed) |
| `NoopToolExecutor` | No-op implementation used in Phase 1 |

## Key Methods

| Method | Description |
|--------|-------------|
| `Agent::new(config, registry)` | Create agent from config + provider registry |
| `Agent::build_system_prompt(config)` | Compose system prompt (base + config) |
| `Agent::handle_message(input)` | Non-streaming: send message, get full response |
| `Agent::handle_message_stream(input)` | Streaming: send message, get chunk stream |
| `Agent::finalize_stream(content)` | Append streamed response to conversation history |

## Dependencies

- `mc-core` — Message, Conversation, McError types
- `mc-config` — AgentConfig
- `mc-llm` — LlmProvider trait, ProviderRegistry, ChatRequest/Response

## Phase 1 Scope

- ✅ System prompt composition (base + config)
- ✅ Conversation tracking
- ✅ Non-streaming and streaming LLM calls
- ✅ Token-by-token stdout streaming in CLI
- ⏳ Tool execution loop (Phase 2)
- ⏳ Skills injection (Phase 2)
- ⏳ Memory injection (Phase 2)
- ⏳ Nudge counters for evolution triggers (Phase 2)
