English | [中文](./README.md)

# M-Claw

A personal AI assistant platform built in Rust, derived from [OpenClaw](https://github.com/paimoncloud/openclaw) (subtraction) and [Hermes Agent](https://github.com/nicepkg/hermes-agent) (self-evolution).

## Why This Project Exists

OpenClaw is a feature-rich AI assistant platform, but it's overkill for personal use — 140+ crates, Canvas system, complex plugin marketplace, enterprise-grade permission management... Most features go unused.

Hermes Agent has an elegant self-evolution mechanism — the agent learns from conversations, automatically maintaining memory and skills.

M-Claw's approach: **Slim OpenClaw down to personal-use scale, then graft on Hermes's self-evolution capabilities.**

## Key Features

### 🧠 Self-Evolution System (from Hermes)

The agent doesn't just answer questions — it learns from every conversation:

- **Background Review** — After N turns of conversation, automatically reviews the dialogue in the background and extracts memorable information
- **Memory Nudge** — Triggers memory review when conversation turns or tool-call counts hit configurable thresholds
- **Skill Manager** — Tracks skill usage frequency, logs usage history automatically
- **Curator** — Periodically scans skill lifecycle (Active → Stale → Archival), auto-cleans stale skills, preserves backup snapshots

### 🔧 Tool Calling

7 built-in tools so the agent can take real actions:

| Tool | Description |
|------|-------------|
| `bash` | Execute shell commands (Windows: PowerShell, Unix: /bin/sh) |
| `read_file` | Read file contents with offset/limit support |
| `write_file` | Write to files |
| `list_dir` | List directory contents |
| `glob` | File pattern matching (`*`, `?`, `**`) |
| `grep` | Search file contents |
| `memory` | Read/write agent memory files |

Supports multi-turn tool calls (up to 10 iterations), with tool detection during streaming output.

### 💾 Multiple Memory Modes

- **Agent Memory** (`MEMORY.md`) — Agent's own experience and knowledge, persists across sessions
- **User Memory** (`USER.md`) — User preferences and habits
- **Session Memory** (SQLite) — Session history persistence + FTS5 full-text search
- **Auto-Compression** — Summarizes older messages when conversation exceeds threshold, keeping context window efficient

### 📡 LLM Provider Support

- **OpenAI-compatible** — Any provider using OpenAI API format (OpenAI, DeepSeek, Xiaomi MiMo, local Ollama, etc.)
- **Anthropic** — Native Claude API support
- **Auto-retry** — Exponential backoff with jitter on network errors, 429, and 5xx (up to 3 retries)

### 🌐 Channel Integration

- **Feishu/Lark** — Webhook verification + OpenAPI messaging, Event v2.0 support
- **HTTP Gateway** — axum server with OpenAI-compatible API (`/v1/chat/completions`)
- WeChat/QQ interfaces reserved (not yet implemented)

### 🛡️ Security Sandbox

- Tool-call security boundaries: path traversal detection, dangerous command blacklist
- Configurable file access whitelist
- All config fields support `env:VAR_NAME` prefix for environment variable resolution

## Architecture

```
m-claw/
├── Cargo.toml              # workspace root
├── src/main.rs             # CLI entry (clap)
│
├── crates/
│   ├── core/               # Message model, trait definitions, error types
│   ├── config/             # Configuration system (TOML, env vars, defaults)
│   ├── llm/                # LLM Provider abstraction + OpenAI/Anthropic impls
│   ├── agent/              # Agent conversation loop + tool integration
│   ├── tools/              # Tool registry + 7 built-in tools + security
│   ├── skills/             # SKILL.md skill loading system
│   ├── memory/             # Memory system (MEMORY.md / USER.md)
│   ├── evolution/          # Self-evolution loop (Background Review + Curator)
│   ├── channels/           # Channel abstraction trait + Feishu impl
│   ├── gateway/            # HTTP server (axum)
│   └── storage/            # SQLite session persistence + FTS5
│
└── data/                   # Runtime data (gitignored)
    ├── sessions.db         # Session storage
    ├── skills/             # Skills directory
    └── memory/             # Memory files
```

11 crates, clear responsibilities, compile-time isolation.

## Quick Start

### Prerequisites

- Rust 1.85+ (edition 2024)
- Windows / macOS / Linux

### Build

```bash
cargo build --release
```

### Configure

```bash
# Create config directory
mkdir -p ~/.m-claw

# Copy example config
cp config.example.toml ~/.m-claw/config.toml

# Edit config, fill in your LLM API key
```

Example config:

```toml
[gateway]
bind = "127.0.0.1:3777"

[[providers]]
id = "my-llm"
kind = "OpenAI"                    # OpenAI | Anthropic
base_url = "https://api.openai.com/v1"
api_key = "sk-your-key-here"
models = ["gpt-4o"]

[[agents]]
id = "main"
name = "M-Claw"
model = "gpt-4o"
provider = "my-llm"
system_prompt = "You are a helpful AI assistant."

[memory]
enabled = true
path = "./data/memory"

[skills]
enabled = true
path = "./data/skills"

[evolution]
enabled = true
memory_nudge_interval = 10    # Trigger memory review every 10 turns
skill_nudge_interval = 10     # Trigger skill review every 10 tool calls

[tools]
bash = true
filesystem = true
web_search = true
```

### Usage

```bash
# Chat
m-claw chat "Hello"
m-claw chat "List files in the current directory"
m-claw chat "Read the contents of Cargo.toml"

# Start HTTP server
m-claw gateway

# Manage sessions
m-claw session list
m-claw session search "keyword"
m-claw session export <session-id>

# Check config
m-claw config-check
```

## What Was Removed from OpenClaw

| Removed | Reason |
|---------|--------|
| Canvas system | Personal use doesn't need whiteboard collaboration |
| Plugin marketplace | Replaced by built-in tools + skill system |
| Multi-tenant / permissions | Only one user |
| WebSocket long connections | HTTP polling is sufficient |
| 142 crates | Slimmed down to 11 |
| TypeScript/Node.js | Rewritten entirely in Rust |

## What Was Learned from Hermes

| Adopted | Description |
|---------|-------------|
| Background Review | Async background conversation review, memory extraction |
| Curator | Periodic skill lifecycle scanning |
| Nudge mechanism | Conversation turn / tool call count triggers self-review |
| Usage tracking | Records skill usage frequency, feeds data to Curator |

## Tech Stack

- **Language**: Rust (edition 2024, rust-version 1.85)
- **Async runtime**: tokio
- **HTTP server**: axum
- **HTTP client**: reqwest (streaming support)
- **Database**: rusqlite + FTS5
- **CLI**: clap (derive)
- **Serialization**: serde + serde_json + toml
- **Logging**: tracing + tracing-subscriber

## Testing

```bash
# Run all tests (318)
cargo test --workspace

# Run clippy lint
cargo clippy --workspace
```

## License

MIT
