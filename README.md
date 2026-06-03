# M-Claw

基于 [OpenClaw](https://github.com/paimoncloud/openclaw) 和 [Hermes Agent](https://github.com/nicepkg/hermes-agent) 做减法开发的个人 AI 助手平台。用 Rust 重写，砍掉了企业级复杂度，只保留个人用户真正需要的功能。

## 为什么做这个项目

OpenClaw 是一个功能丰富的 AI 助手平台，但对个人用户来说太重了——140 多个 crate、Canvas 系统、复杂的插件市场、企业级权限管理……大部分功能用不上。

Hermes Agent 有一套很聪明的自进化机制——agent 能从对话中学习，自动维护记忆和技能。

M-Claw 的思路就是：**把 OpenClaw 瘦身到个人可用的程度，再把 Hermes 的自进化能力嫁接过来。**

## 核心特性

### 🧠 自进化系统（来自 Hermes）

Agent 不只是回答问题，它能从每次对话中学习：

- **Background Review** — 每隔 N 轮对话，后台自动回顾对话内容，提取值得记住的信息写入记忆
- **Memory Nudge** — 对话轮次和工具调用次数达到阈值时，触发记忆回顾
- **Skill Manager** — 自动追踪技能使用频率，记录使用日志
- **Curator** — 定期扫描技能状态（Active → Stale → Archival），自动清理过期技能，保留备份快照

### 🔧 工具调用

内置 7 个工具，Agent 可以执行实际操作：

| 工具 | 说明 |
|------|------|
| `bash` | 执行 shell 命令（Windows: PowerShell, Unix: /bin/sh） |
| `read_file` | 读取文件内容，支持行号偏移 |
| `write_file` | 写入文件 |
| `list_dir` | 列出目录内容 |
| `glob` | 文件模式匹配（`*`, `?`, `**`） |
| `grep` | 文本内容搜索 |
| `memory` | 读写 agent 记忆文件 |

支持多轮工具调用（最多 10 次迭代），流式输出时也能自动检测并执行工具。

### 💾 多种记忆模式

- **Agent Memory** (`MEMORY.md`) — agent 自身的经验和知识，跨会话保留
- **User Memory** (`USER.md`) — 用户偏好和习惯记录
- **Session Memory** (SQLite) — 会话历史持久化 + FTS5 全文搜索
- **自动压缩** — 对话超过阈值时自动摘要旧消息，保持上下文窗口高效

### 📡 LLM 供应商支持

- **OpenAI 兼容** — 支持任何 OpenAI API 格式的供应商（OpenAI、DeepSeek、小米 MiMo、本地 Ollama 等）
- **Anthropic** — 原生支持 Claude API
- **自动重试** — 网络错误、429、5xx 自动指数退避重试（最多 3 次，带抖动）

### 🌐 通道集成

- **飞书** — Webhook 验证 + OpenAPI 消息收发，支持 Event v2.0
- **HTTP Gateway** — axum 服务器，OpenAI 兼容 API（`/v1/chat/completions`）
- 微信/QQ 预留接口（尚未实现）

### 🛡️ 安全防护

- 工具调用安全边界：路径遍历检测、危险命令黑名单
- 可配置的文件访问白名单
- 所有配置字段支持 `env:VAR_NAME` 前缀读取环境变量

## 架构

```
m-claw/
├── Cargo.toml              # workspace 根
├── src/main.rs             # CLI 入口 (clap)
│
├── crates/
│   ├── core/               # 消息模型、trait 定义、错误类型
│   ├── config/             # 配置系统 (TOML, 环境变量, 默认值)
│   ├── llm/                # LLM Provider 抽象 + OpenAI/Anthropic 实现
│   ├── agent/              # Agent 对话循环 + 工具集成
│   ├── tools/              # 工具注册 + 7 个内置工具 + 安全防护
│   ├── skills/             # SKILL.md 技能加载系统
│   ├── memory/             # 记忆系统 (MEMORY.md / USER.md)
│   ├── evolution/          # 自进化闭环 (Background Review + Curator)
│   ├── channels/           # 通道抽象 trait + 飞书实现
│   ├── gateway/            # HTTP 服务器 (axum)
│   └── storage/            # SQLite 会话持久化 + FTS5
│
└── data/                   # 运行时数据 (gitignore)
    ├── sessions.db         # 会话存储
    ├── skills/             # 技能目录
    └── memory/             # 记忆文件
```

11 个 crate，职责清晰，编译时隔离。

## 快速开始

### 环境要求

- Rust 1.85+（edition 2024）
- Windows / macOS / Linux

### 构建

```bash
cargo build --release
```

### 配置

```bash
# 创建配置目录
mkdir -p ~/.m-claw

# 复制示例配置
cp config.example.toml ~/.m-claw/config.toml

# 编辑配置，填入你的 LLM API key
```

配置文件示例：

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
memory_nudge_interval = 10    # 每 10 轮对话触发记忆回顾
skill_nudge_interval = 10     # 每 10 次工具调用触发技能回顾

[tools]
bash = true
filesystem = true
web_search = true
```

### 使用

```bash
# 对话
m-claw chat "你好"
m-claw chat "列出当前目录的文件"
m-claw chat "读取 Cargo.toml 的内容"

# 启动 HTTP 服务
m-claw gateway

# 管理会话
m-claw session list
m-claw session search "关键词"
m-claw session export <session-id>

# 检查配置
m-claw config-check
```

## 从 OpenClaw 砍掉了什么

| 砍掉的 | 理由 |
|--------|------|
| Canvas 系统 | 个人用不上白板协作 |
| 插件市场 | 用内置工具 + 技能系统替代 |
| 多租户 / 权限管理 | 只有一个用户 |
| WebSocket 长连接 | HTTP 轮询够用 |
| 142 个 crate | 精简到 11 个 |
| TypeScript/Node.js | 全部用 Rust 重写 |

## 从 Hermes 学到了什么

| 学来的 | 说明 |
|--------|------|
| Background Review | 后台异步回顾对话，提取记忆 |
| Curator | 定期扫描技能生命周期 |
| Nudge 机制 | 基于对话轮次/工具调用次数触发自审 |
| 使用追踪 | 记录技能使用频率，为 Curator 提供数据 |

## 技术栈

- **语言**: Rust (edition 2024, rust-version 1.85)
- **异步运行时**: tokio
- **HTTP 服务器**: axum
- **HTTP 客户端**: reqwest（流式支持）
- **数据库**: rusqlite + FTS5
- **CLI**: clap (derive)
- **序列化**: serde + serde_json + toml
- **日志**: tracing + tracing-subscriber

## 测试

```bash
# 运行全部测试（318 个）
cargo test --workspace

# 运行 clippy 检查
cargo clippy --workspace
```

## License

MIT
