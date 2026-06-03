# mc-core

Core types and traits shared across all mavis-claw crates.

## Contents

- **Message model** — `Message`, `Role`, `ToolCall`, `FunctionCall` (OpenAI-compatible format)
- **Conversation model** — `Conversation`, `ConversationMeta`
- **Error types** — `McError` (unified error enum with thiserror)
- **Tool trait** — `Tool` (async trait for function calling), `ToolDefinition`

## Dependencies

- `serde`, `serde_json` — serialization
- `thiserror` — error derives
- `async-trait` — async trait support
- `chrono`, `uuid` — timestamps and IDs

## Usage

```rust
use mc_core::{Message, Role, Conversation, McError, Tool, ToolDefinition};
```
