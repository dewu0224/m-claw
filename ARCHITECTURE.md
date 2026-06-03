# Mavis-Claw 架构设计文档

> 用 Rust 重写的个人 AI 助手平台，精简自 OpenClaw，融合 Hermes 自进化机制。

## 1. 项目目标

| 目标 | 说明 |
|------|------|
| **轻量** | 丢弃 OpenClaw 142 个扩展、伴侣应用、Canvas、语音系统等过重模块 |
| **高性能** | Rust + tokio 异步运行时，单二进制部署 |
| **自进化** | 移植 Hermes 的 Background Review + Curator 闭环，agent 能从对话中学习 |
| **个人使用** | 只保留用户实际需要的功能：飞书/微信/QQ + OpenAI-compatible + Anthropic |

## 2. Workspace 结构

```
mavis-claw/
├── Cargo.toml              # workspace 根
├── ARCHITECTURE.md         # 本文档
├── src/
│   └── main.rs             # CLI 入口 (clap)
│
├── crates/
│   ├── core/               # 基础类型、trait 定义、错误类型
│   ├── config/             # 配置系统 (TOML, 环境变量, 默认值)
│   ├── llm/                # LLM Provider 抽象层
│   ├── agent/              # Agent 对话循环 + 上下文管理
│   ├── tools/              # 工具注册表 + 内置工具
│   ├── skills/             # SKILL.md 加载与管理
│   ├── memory/             # 记忆系统 (MEMORY.md / USER.md)
│   ├── evolution/          # 自进化闭环 (Background Review + Curator)
│   ├── channels/           # 频道适配器 trait + 实现 stubs
│   └── gateway/            # HTTP/WebSocket 网关
│
└── data/                   # 运行时数据目录 (gitignore)
    ├── config.toml         # 用户配置
    ├── sessions.db         # SQLite 会话存储
    ├── skills/             # 技能目录
    └── memory/             # MEMORY.md / USER.md
```

## 3. Crate 依赖关系

```
                    ┌──────────┐
                    │ main.rs  │  CLI 入口
                    └────┬─────┘
                         │ depends on
              ┌──────────┼──────────┐
              ▼          ▼          ▼
         ┌────────┐ ┌────────┐ ┌─────────┐
         │ config │ │ agent  │ │ gateway │
         └───┬────┘ └───┬────┘ └────┬────┘
             │          │           │
             │    ┌─────┼───────┐   │
             ▼    ▼     ▼       ▼   ▼
          ┌─────┐ ┌──────┐ ┌──────┐ ┌──────────┐
          │ llm │ │tools │ │skills│ │ channels │
          └──┬──┘ └──┬───┘ └──┬───┘ └─────┬────┘
             │       │        │           │
             └───────┼────────┼───────────┘
                     ▼        ▼
               ┌─────────┐ ┌────────┐
               │ memory  │ │   core │  (基础层，被所有人依赖)
               └────┬────┘ └────────┘
                    │
                    ▼
              ┌───────────┐
              │ evolution │  依赖 agent + memory + skills + llm
              └───────────┘
```

**依赖规则：**
- `core` — 零依赖（仅 serde, chrono, uuid 等基础库），被所有 crate 依赖
- `config` — 依赖 core
- `llm` — 依赖 core + config
- `tools` — 依赖 core
- `skills` — 依赖 core
- `memory` — 依赖 core
- `channels` — 依赖 core
- `agent` — 依赖 core + config + llm + tools + skills + memory（对话循环的编排层）
- `gateway` — 依赖 core + config + agent + channels（HTTP/WS 网关）
- `evolution` — 依赖 core + config + llm + agent + tools + skills + memory（最上层）

## 4. 各 Crate 设计

### 4.1 `core` — 基础类型

**职责：** 定义所有 crate 共享的类型、trait、错误。

```rust
// === 消息模型 ===
pub struct Message {
    pub role: Role,          // System | User | Assistant | Tool
    pub content: Option<String>,
    pub tool_calls: Option<Vec<ToolCall>>,
    pub tool_call_id: Option<String>,
    pub name: Option<String>,
}

pub enum Role { System, User, Assistant, Tool }

pub struct ToolCall {
    pub id: String,
    pub function: FunctionCall,
}

pub struct FunctionCall {
    pub name: String,
    pub arguments: String,  // JSON string
}

// === 对话 ===
pub struct Conversation {
    pub id: String,
    pub messages: Vec<Message>,
    pub metadata: ConversationMeta,
}

// === 工具定义 (OpenAI function calling 格式) ===
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,  // JSON Schema
}

// === 错误 ===
#[derive(Debug, thiserror::Error)]
pub enum McError {
    #[error("LLM error: {0}")]
    Llm(String),
    #[error("Tool error: {0}")]
    Tool(String),
    #[error("Config error: {0}")]
    Config(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Channel error: {0}")]
    Channel(String),
}
```

### 4.2 `config` — 配置系统

**职责：** 加载、验证、合并配置。支持 TOML 文件 + 环境变量 + 默认值。

```rust
// 配置结构
pub struct AppConfig {
    pub gateway: GatewayConfig,
    pub agents: Vec<AgentConfig>,
    pub providers: Vec<ProviderConfig>,
    pub channels: Vec<ChannelConfig>,
    pub memory: MemoryConfig,
    pub skills: SkillsConfig,
    pub evolution: EvolutionConfig,
    pub tools: ToolsConfig,
}

pub struct GatewayConfig {
    pub bind: String,            // "127.0.0.1:3777"
    pub auth_token: Option<String>,
}

pub struct AgentConfig {
    pub id: String,
    pub name: String,
    pub model: String,           // "gpt-4o" / "claude-sonnet-4-20250514"
    pub provider: String,        // 关联的 provider id
    pub system_prompt: Option<String>,
    pub system_prompt_file: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

pub struct ProviderConfig {
    pub id: String,
    pub kind: ProviderKind,      // OpenAI | Anthropic
    pub base_url: String,
    pub api_key: String,         // 支持 env: 前缀引用环境变量
    pub models: Vec<String>,     // 该 provider 支持的模型名
}

pub enum ProviderKind { OpenAI, Anthropic }

pub struct ChannelConfig {
    pub id: String,
    pub kind: ChannelKind,       // Feishu | WeChat | QQ
    pub agent_id: String,        // 绑定的 agent
    // 各渠道特定配置...
    pub settings: toml::Table,
}

pub enum ChannelKind { Feishu, WeChat, QQ }

// 加载逻辑
impl AppConfig {
    /// 优先级: CLI args > env vars > config file > defaults
    pub fn load(config_path: Option<&Path>) -> Result<Self, McError>;
}
```

**配置文件示例 (`config.toml`)：**

```toml
[gateway]
bind = "127.0.0.1:3777"

[[providers]]
id = "openai-main"
kind = "OpenAI"
base_url = "https://api.openai.com/v1"
api_key = "env:OPENAI_API_KEY"
models = ["gpt-4o", "gpt-4o-mini", "o3"]

[[providers]]
id = "anthropic-main"
kind = "Anthropic"
base_url = "https://api.anthropic.com"
api_key = "env:ANTHROPIC_API_KEY"
models = ["claude-sonnet-4-20250514", "claude-opus-4-20250514"]

[[providers]]
id = "deepseek"
kind = "OpenAI"                 # OpenAI-compatible
base_url = "https://api.deepseek.com/v1"
api_key = "env:DEEPSEEK_API_KEY"
models = ["deepseek-chat", "deepseek-reasoner"]

[[agents]]
id = "main"
name = "Mavis"
model = "gpt-4o"
provider = "openai-main"
system_prompt = "You are Mavis, a helpful AI assistant."

[[channels]]
id = "feishu-bot"
kind = "Feishu"
agent_id = "main"
[channels.settings]
app_id = "env:FEISHU_APP_ID"
app_secret = "env:FEISHU_APP_SECRET"
verification_token = "env:FEISHU_VERIFICATION_TOKEN"

[memory]
enabled = true
path = "./data/memory"

[skills]
enabled = true
path = "./data/skills"

[evolution]
enabled = true
memory_nudge_interval = 10     # 每 N 轮触发记忆回顾
skill_nudge_interval = 10      # 每 N 次工具调用触发技能回顾
curator_interval_hours = 168   # Curator 运行间隔 (7天)
```

### 4.3 `llm` — LLM Provider 抽象

**职责：** 统一的 LLM 调用接口，支持流式和非流式，支持 OpenAI-compatible 和 Anthropic 原生协议。

```rust
// === 核心 trait ===
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// 非流式调用
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, McError>;

    /// 流式调用
    async fn chat_stream(&self, request: ChatRequest)
        -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, McError>> + Send>>, McError>;

    /// Provider 名称
    fn name(&self) -> &str;

    /// 支持的模型列表
    fn models(&self) -> &[String];
}

// === 请求/响应 ===
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Option<Vec<ToolDefinition>>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub stream: bool,
}

pub struct ChatResponse {
    pub message: Message,
    pub usage: Usage,
    pub finish_reason: FinishReason,
}

pub struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

pub struct StreamChunk {
    pub delta: String,           // 增量文本
    pub tool_call_delta: Option<ToolCallDelta>,
    pub finish_reason: Option<FinishReason>,
}

// === Provider 注册表 ===
pub struct ProviderRegistry {
    providers: HashMap<String, Arc<dyn LlmProvider>>,
}

impl ProviderRegistry {
    /// 从配置构建
    pub fn from_config(providers: &[ProviderConfig]) -> Result<Self, McError>;

    /// 按 id 获取 provider
    pub fn get(&self, id: &str) -> Result<Arc<dyn LlmProvider>, McError>;

    /// 按 model 名查找 provider（扫描所有 provider 的 models 列表）
    pub fn find_by_model(&self, model: &str) -> Result<Arc<dyn LlmProvider>, McError>;
}
```

**实现计划：**
- `OpenAiProvider` — 通用 OpenAI-compatible 实现，通过 `base_url` 区分不同服务商
- `AnthropicProvider` — Anthropic 原生 Messages API

两个 provider 都用 `reqwest` + `serde_json` 直接调 HTTP，**不依赖任何 SDK crate**，保持轻量。

### 4.4 `agent` — Agent 运行时

**职责：** 对话循环编排、上下文管理、工具调度、系统提示词构建。

```rust
pub struct Agent {
    pub id: String,
    pub config: AgentConfig,
    pub provider: Arc<dyn LlmProvider>,
    pub tool_registry: Arc<ToolRegistry>,
    pub skill_loader: Arc<SkillLoader>,
    pub memory_store: Arc<MemoryStore>,
    pub conversations: HashMap<String, Conversation>,
    // 自进化 nudge 计数器
    pub turns_since_memory: AtomicU32,
    pub iters_since_skill: AtomicU32,
}

impl Agent {
    /// 处理一条用户消息，返回 assistant 回复
    /// 内部循环：call LLM → execute tools → call LLM → ... 直到无 tool_calls
    pub async fn handle_message(
        &mut self,
        conversation_id: &str,
        user_message: Message,
    ) -> Result<Message, McError>;

    /// 构建系统提示词（三层：base + skills + memory）
    fn build_system_prompt(&self, conversation_id: &str) -> String;

    /// 工具调用执行循环
    async fn execute_tool_loop(
        &mut self,
        conversation: &mut Conversation,
        initial_response: ChatResponse,
    ) -> Result<Message, McError>;
}
```

**对话循环流程：**
```
用户消息
  │
  ▼
构建 system prompt (base + loaded skills + memory snapshot)
  │
  ▼
┌─→ LLM.chat(messages)
│     │
│     ▼
│   有 tool_calls?
│     │         │
│    Yes        No → 返回 assistant message
│     │
│     ▼
│   执行 tool_calls → 追加 tool results 到 messages
│     │
│     ▼
│   nudge 计数器 +1
│     │
└─────┘
```

### 4.5 `tools` — 工具系统

**职责：** 工具注册、发现、执行。

```rust
// === 核心 trait ===
#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, args: serde_json::Value) -> Result<String, McError>;
}

// === 注册表 ===
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, tool: Arc<dyn Tool>);
    pub fn definitions(&self) -> Vec<ToolDefinition>;
    pub async fn execute(&self, name: &str, args: serde_json::Value) -> Result<String, McError>;
}

// === 内置工具 ===
// Phase 2: bash, read_file, write_file, list_dir, glob, grep
// Phase 3: memory (读写 MEMORY.md/USER.md, 4 种操作)
```

### 4.6 `skills` — 技能系统

**职责：** 从文件系统加载 SKILL.md，解析 frontmatter，支持三级加载。

```rust
pub struct Skill {
    pub name: String,
    pub description: String,
    pub content: String,           // SKILL.md 正文
    pub metadata: SkillMetadata,   // YAML frontmatter
    pub path: PathBuf,
}

pub struct SkillMetadata {
    pub trigger_words: Vec<String>,
    pub version: Option<String>,
    pub author: Option<String>,
    pub dependencies: Vec<String>,
}

pub struct SkillLoader {
    skills_dir: PathBuf,
    cache: HashMap<String, Skill>,
}

impl SkillLoader {
    /// 扫描 skills 目录，加载所有 SKILL.md
    pub fn load_all(&mut self) -> Result<(), McError>;

    /// 按名称加载技能（支持模糊匹配）
    pub fn load(&self, name: &str) -> Option<&Skill>;

    /// 按 trigger words 匹配
    pub fn match_trigger(&self, text: &str) -> Vec<&Skill>;

    /// 返回所有技能的摘要（用于注入 system prompt）
    pub fn summary(&self) -> String;
}
```

### 4.7 `memory` — 记忆系统

**职责：** 管理 MEMORY.md（agent 知识）和 USER.md（用户画像）的 CRUD。

```rust
pub struct MemoryStore {
    base_path: PathBuf,          // data/memory/
}

impl MemoryStore {
    /// 读取 MEMORY.md 全文
    pub fn read_agent_memory(&self) -> Result<String, McError>;

    /// 追加内容到 MEMORY.md
    pub fn append_agent_memory(&self, content: &str) -> Result<(), McError>;

    /// 读取 USER.md 全文
    pub fn read_user_memory(&self) -> Result<String, McError>;

    /// 追加内容到 USER.md
    pub fn append_user_memory(&self, content: &str) -> Result<(), McError>;

    /// 合并/编辑（按 section 标题定位替换）
    pub fn update_section(&self, file: MemoryFile, heading: &str, content: &str) -> Result<(), McError>;

    /// 删除 section
    pub fn remove_section(&self, file: MemoryFile, heading: &str) -> Result<(), McError>;
}

pub enum MemoryFile { Agent, User }
```

### 4.8 `evolution` — 自进化闭环

**职责：** 移植 Hermes 的 Background Review + Curator 机制。

```rust
// === Background Review ===
/// 每 N 轮对话后，后台 fork 一个受限 agent 分析对话
pub struct BackgroundReviewer {
    nudge_config: NudgeConfig,
    llm_provider: Arc<dyn LlmProvider>,
    memory_store: Arc<MemoryStore>,
    skill_manager: Arc<SkillManager>,
}

pub struct NudgeConfig {
    pub memory_interval: u32,    // 默认 10 轮
    pub skill_interval: u32,     // 默认 10 次工具调用
}

impl BackgroundReviewer {
    /// 检查是否应该触发 review
    pub fn should_review(&self, turns: u32, tool_iters: u32) -> ReviewTrigger;

    /// 后台运行 review（tokio::spawn）
    pub async fn spawn_review(
        &self,
        conversation: &Conversation,
        trigger: ReviewTrigger,
    ) -> JoinHandle<ReviewResult>;
}

pub struct ReviewResult {
    pub memory_updates: Vec<MemoryUpdate>,
    pub skill_updates: Vec<SkillUpdate>,
    pub summary: String,         // 展示给用户的摘要
}

// === Curator ===
/// 定期运行的技能生命周期管理器
pub struct Curator {
    skills_dir: PathBuf,
    usage_path: PathBuf,         // .usage.json
    config: CuratorConfig,
}

pub struct CuratorConfig {
    pub stale_after_days: u32,   // 默认 30
    pub archive_after_days: u32, // 默认 90
    pub run_interval_hours: u32, // 默认 168 (7天)
}

/// 技能生命周期状态
pub enum SkillState { Active, Stale, Archived, Pinned }

/// 技能遥测
pub struct SkillUsage {
    pub use_count: u32,
    pub view_count: u32,
    pub patch_count: u32,
    pub last_activity_at: chrono::DateTime<chrono::Utc>,
    pub state: SkillState,
    pub pinned: bool,
    pub created_by: SkillOrigin,  // Bundled | Hub | Agent
}

impl Curator {
    /// 纯状态转换（无 LLM）：active ↔ stale → archived
    pub fn apply_automatic_transitions(&self) -> Result<Vec<Transition>, McError>;

    /// LLM 合并 pass：扫描相似技能，合并为伞形技能
    pub async fn run_consolidation(&self, provider: &dyn LlmProvider) -> Result<ConsolidationReport, McError>;

    /// 运行前快照备份
    pub fn backup(&self) -> Result<PathBuf, McError>;
}
```

**自进化数据流：**
```
对话进行中
  │
  ├── 每轮: turns_since_memory += 1
  ├── 工具调用时: iters_since_skill += 1
  │
  ▼
触发条件达到 (memory_interval / skill_interval)
  │
  ▼
tokio::spawn(background_review)
  │
  ├── 创建受限 Agent（只允许 memory + skill_manage 工具）
  ├── 注入 review prompt（"分析对话中的用户纠正、技术发现..."）
  ├── 受限 Agent 读取对话 → 调用 memory/skill 工具
  │
  ▼
更新结果
  ├── MEMORY.md / USER.md 更新
  ├── 技能 CRUD（patch / create / delete）
  └── 向用户展示摘要: "💾 Self-improvement: ..."
  │
  ▼
定期 Curator (每 7 天)
  ├── 自动状态转换: active → stale → archived
  ├── LLM 合并 pass: 合并相似技能为伞形
  ├── 备份 + 回滚支持
  └── 更新 .usage.json 遥测数据
```

### 4.9 `channels` — 频道适配器

**职责：** 统一的消息收发接口，各渠道实现 trait。

```rust
// === 核心 trait ===
#[async_trait]
pub trait Channel: Send + Sync {
    /// 渠道类型标识
    fn kind(&self) -> ChannelKind;

    /// 启动消息接收（webhook server / websocket / polling）
    async fn start(&self, handler: Arc<dyn MessageHandler>) -> Result<(), McError>;

    /// 发送消息到渠道
    async fn send(&self, target: &ChannelTarget, message: OutgoingMessage) -> Result<(), McError>;

    /// 停止
    async fn stop(&self) -> Result<(), McError>;
}

/// 消息到达时回调
#[async_trait]
pub trait MessageHandler: Send + Sync {
    async fn on_message(&self, source: ChannelSource, message: IncomingMessage) -> Result<(), McError>;
}

pub struct IncomingMessage {
    pub channel_id: String,
    pub conversation_key: String,    // 渠道内会话标识
    pub sender: Sender,
    pub content: MessageContent,
    pub reply_to: Option<String>,
}

pub enum MessageContent {
    Text(String),
    Image { url: String, caption: Option<String> },
    File { url: String, name: String },
}

// === 实现计划 ===
// 飞书: webhook 接收 + 飞书 OpenAPI 发送
// 微信: 企业微信 webhook / 第三方桥接
// QQ: go-cqhttp / Lagrange.OneBot 协议
```

### 4.10 `gateway` — HTTP/WebSocket 网关

**职责：** 统一入口，接收各渠道 webhook，提供管理 API。

```rust
// 路由设计
// POST /webhook/feishu      → 飞书事件回调
// POST /webhook/wechat      → 微信回调
// POST /webhook/qq           → QQ (OneBot) 回调
// POST /v1/chat/completions  → OpenAI 兼容 API
// GET  /health               → 健康检查
// GET  /api/status            → 状态信息
// POST /api/chat              → 手动对话（调试用）

pub struct Gateway {
    config: GatewayConfig,
    channels: Vec<Arc<dyn Channel>>,
    agents: HashMap<String, Arc<Mutex<Agent>>>,
}

impl Gateway {
    pub async fn start(&self) -> Result<(), McError>;
}
```

## 5. 技术选型汇总

| 用途 | 选择 | 理由 |
|------|------|------|
| 异步运行时 | tokio | Rust 生态标准 |
| HTTP 框架 | axum | tokio 原生，类型安全 |
| HTTP 客户端 | reqwest | 异步，支持 stream |
| CLI | clap derive | 声明式，功能完善 |
| 配置 | toml + serde | 简单清晰 |
| 数据库 | rusqlite (bundled) | SQLite，零部署，FTS5 |
| 日志 | tracing + tracing-subscriber | 结构化日志，支持 env-filter |
| 错误 | thiserror + anyhow | thiserror 定义域错误，anyhow 用于应用层 |
| 序列化 | serde + serde_json | 生态标准 |

## 6. 分期实施计划

### Phase 1 — 骨架能跑 (预计 1-2 周)

**目标：** CLI 启动 → 配置加载 → 对话循环能通 → 能跟 LLM 对话

- [ ] `core`: 消息模型、错误类型
- [ ] `config`: TOML 加载 + 验证 + 默认值
- [ ] `llm`: OpenAI-compatible provider (非流式先)
- [ ] `llm`: Anthropic provider (非流式)
- [ ] `agent`: 基础对话循环（无工具）
- [ ] `main.rs`: CLI 入口，`chat` 子命令直接在终端对话

**验收标准：** `mavis-claw chat "hello"` 能返回 LLM 回复

### Phase 2 — 工具 & 流式 (预计 1 周)

**目标：** Agent 能调工具、流式输出

- [ ] `tools`: 工具注册表 + trait
- [ ] `tools`: 内置工具 — `bash`, `read_file`, `write_file`, `list_dir`
- [ ] `agent`: 工具调用循环（call LLM → execute tools → repeat）
- [ ] `llm`: 流式支持 (SSE)
- [ ] `agent`: 流式输出到终端

**验收标准：** `mavis-claw chat "列出当前目录的文件"` 能调 bash 工具返回结果

### Phase 3 — 技能 & 记忆 (预计 1 周)

**目标：** 技能加载、记忆持久化

- [x] `skills`: SKILL.md 加载 + frontmatter 解析
- [x] `skills`: 技能摘要注入 system prompt
- [x] `memory`: MEMORY.md / USER.md CRUD
- [x] `tools`: `memory` 工具（read/append/write/update_section）
- [x] `agent`: system prompt 三层构建（base + skills + memory）
- [ ] `tools`: `web_search` 工具

**验收标准：** Agent 能记住用户偏好并在下次对话中使用 ✅ 已达成

### Phase 4 — 网关 & 频道 (预计 2 周)

**目标：** 飞书/微信/QQ 能收到消息并回复

- [x] `channels`: Channel trait + MessageHandler trait
- [x] `channels`: 飞书适配器（webhook + OpenAPI）
- [ ] `channels`: 微信适配器（企业微信）
- [ ] `channels`: QQ 适配器（OneBot 协议）
- [x] `gateway`: axum HTTP server + webhook 路由
- [x] `gateway`: OpenAI 兼容 API (`/v1/chat/completions`)

**验收标准：** 飞书机器人收到消息后能回复

### Phase 5 — 自进化 (预计 2 周)

**目标：** Agent 能从对话中学习

- [x] `evolution`: Nudge 计数器
- [x] `evolution`: Background Review (受限 Agent + review prompt)
- [x] `evolution`: Skill Manager (create/edit/patch/delete)
- [x] `evolution`: Skill Provenance (agent vs user)
- [x] `evolution`: Curator (状态转换 + LLM 合并 + 遥测)
- [x] `evolution`: Curator 备份/回滚

**验收标准：** 对话 10 轮后自动触发 review，更新 MEMORY.md，用户看到摘要 ✅ 已达成

### Phase 6 — 打磨 (持续)

- [x] 流式 token 显示优化
- [x] 上下文压缩（长对话）
- [x] SQLite 会话持久化 + Session Search (FTS5)
- [ ] Cron 定时任务
- [ ] 更多内置工具
- [x] 错误恢复 & 重试逻辑
- [ ] 测试覆盖

## 7. 关键设计决策

| 决策 | 选择 | 替代方案 | 理由 |
|------|------|---------|------|
| LLM 调用方式 | 直接 HTTP (reqwest) | SDK crate | 保持轻量，不引入 30+ SDK |
| 插件系统 | trait-based | 动态加载 (.so/.dll) | Rust 编译期安全，不需要运行时插件 |
| 配置格式 | TOML | JSON5 / YAML | 可读性好，Rust 生态支持好 |
| 数据库 | SQLite (rusqlite) | sled / redb | FTS5 全文搜索，生态成熟 |
| 频道消息 | webhook (推) | polling (拉) | 实时性好，飞书/微信都支持 |
| QQ 接入 | OneBot 协议 | 原生 QQ API | 生态成熟，有多个 OneBot 实现 |
| 自进化 | 后台线程 (tokio::spawn) | 定时批量 | 即时反馈，不阻塞对话 |

## 8. 目录约定

```
~/.mavis-claw/                  # 用户数据目录
├── config.toml                 # 主配置
├── data/
│   ├── sessions.db             # 会话 + FTS5
│   ├── skills/                 # 技能目录
│   │   ├── web-search/
│   │   │   └── SKILL.md
│   │   └── .usage.json         # 技能遥测
│   └── memory/
│       ├── MEMORY.md           # Agent 记忆
│       └── USER.md             # 用户画像
└── logs/                       # 日志
```

## 9. 团队协作模型

### 9.1 Agent 分工

| Agent | 职责范围 | 负责 Crates |
|-------|---------|-------------|
| **Mavis (我)** | 全局调度、任务分配、进度追踪、文档维护 | 不写代码，负责协调 |
| **core-builder** | 基础层 + 配置层 | `core`, `config` |
| **llm-agent-builder** | LLM 抽象 + Agent 运行时 | `llm`, `agent` |
| **tools-builder** | 工具 + 技能 + 记忆 | `tools`, `skills`, `memory` |
| **net-builder** | 网络层 + 频道 | `channels`, `gateway` |
| **red-blue-reviewer** | 审查 + 红蓝对抗 | 不写代码，负责审查所有交付物 |

### 9.2 工作流

```
Mavis (调度)
  │
  ├── 1. 拆任务 → 分配给对应 agent
  ├── 2. 并行派发（无依赖的任务同时跑）
  │
  ▼
Builder Agents (生产)
  │
  ├── 写代码 + 写文档
  ├── 完成后交付给 Mavis
  │
  ▼
Mavis 汇总
  │
  ├── 收齐一个阶段的所有交付物
  ├── 打包发给 red-blue-reviewer
  │
  ▼
Red-Blue Reviewer (审查)
  │
  ├── 红队视角：找 bug、找设计缺陷、找安全问题
  ├── 蓝队视角：验证功能正确性、测试边界条件
  ├── 输出审查报告（通过 / 需修改 / 需重新设计）
  │
  ▼
Mavis 决策
  ├── 通过 → 合并，更新文档，进入下一阶段
  └── 打回 → 分配修改任务给对应 agent，重新审查
```

### 9.3 红蓝对抗审查标准

Reviewer 需检查：

| 维度 | 红队（攻击方） | 蓝队（防守方） |
|------|--------------|--------------|
| **正确性** | 构造能触发 bug 的输入 | 验证正常路径输出正确 |
| **安全性** | 注入攻击、路径遍历、SSRF | 验证输入校验、权限边界 |
| **性能** | 构造大数据量/高并发场景 | 验证在合理负载下的响应时间 |
| **架构** | 找耦合过紧、违反依赖方向的地方 | 验证 trait 边界是否合理 |
| **文档** | 找文档和代码不一致的地方 | 验证文档示例能跑通 |

## 10. 文档规范

### 10.1 文档清单

| 文档 | 位置 | 维护时机 |
|------|------|---------|
| 架构设计 | `ARCHITECTURE.md` | 每次架构变更后更新 |
| CHANGELOG | `CHANGELOG.md` | 每个 Phase 结束时记录 |
| 各 crate README | `crates/*/README.md` | crate 功能变更时更新 |
| API 文档 | `docs/api.md` | 接口变更时更新 |
| 配置说明 | `docs/config.md` | 配置项变更时更新 |
| 部署指南 | `docs/deployment.md` | Phase 4 完成后初版 |

### 10.2 文档更新规则

1. **代码和文档同步** — 改了代码就必须更新对应文档，不允许"先改代码后补文档"
2. **文档先行** — 新功能先写文档（接口设计），再写代码实现
3. **审查含文档** — Red-Blue Reviewer 必须检查文档与代码的一致性
4. **CHANGELOG 追踪** — 每个 Phase 完成后记录：新增了什么、改了什么、修了什么

## 11. Phase 1 执行计划（详细）

**目标：** CLI 能启动 → 配置能加载 → 能跟 LLM 对话

### 任务拆分

| # | 任务 | 负责 Agent | 依赖 |
|---|------|-----------|------|
| 1.1 | 创建 workspace + 所有 crate 骨架 | core-builder | 无 |
| 1.2 | `core`: 消息模型、错误类型、基础 trait | core-builder | 1.1 |
| 1.3 | `config`: TOML 加载 + 验证 + 默认值 | core-builder | 1.2 |
| 1.4 | `llm`: OpenAI-compatible provider | llm-agent-builder | 1.2, 1.3 |
| 1.5 | `llm`: Anthropic provider | llm-agent-builder | 1.4 |
| 1.6 | `agent`: 基础对话循环（无工具） | llm-agent-builder | 1.4 |
| 1.7 | `main.rs`: CLI 入口 + `chat` 子命令 | llm-agent-builder | 1.3, 1.6 |
| 1.8 | 初始 config.toml 示例 + 文档 | core-builder | 1.3 |
| 1.9 | **红蓝审查** — Phase 1 全部交付物 | red-blue-reviewer | 1.1-1.8 |

### 并行策略

```
core-builder:  1.1 → 1.2 → 1.3 → 1.8
                                    ↗
llm-agent-builder:     1.4 → 1.5 → 1.6 → 1.7
                            ↑ 依赖 1.2+1.3

red-blue-reviewer:                        1.9 (等全部完成)
```

core-builder 先行（1.1→1.2→1.3），完成后 llm-agent-builder 可以并行推进。
两线全部完成后交给 reviewer。

## 12. Phase 2 执行记录 — 工具系统 & 流式

**完成日期:** 2026-06-03

### crates/tools/ 实现

| 模块 | 文件 | 说明 |
|------|------|------|
| ToolRegistry | 
egistry.rs | 工具注册与执行 (HashMap<String, Arc<dyn Tool>>) |
| BashTool | ash.rs | 执行 shell 命令，支持 timeout_ms (默认 30s) |
| ReadFileTool | 
ead_file.rs | 读取文件，支持 offset/limit |
| WriteFileTool | write_file.rs | 写入文件 |
| ListDirTool | list_dir.rs | 列出目录内容 |
| GlobTool | glob.rs | glob 模式匹配 (无外部依赖) |
| GrepTool | grep.rs | 文本搜索，支持 include glob 过滤 |

### crates/agent/ 变更

- Agent 持有 Option<Arc<ToolRegistry>>
- handle_message() 完整工具循环 (最多 10 次迭代)
- handle_message_stream() + inalize_stream() + xecute_tool_loop() 流式路径
- 32 个单元测试全部通过

### src/main.rs 变更

- cmd_chat() 创建 uiltin_registry() 并注入 Agent
- 流式输出 + 工具循环，单次和交互模式均支持

### 测试覆盖

- 5 个 glob 匹配测试
- 7 个 OpenAI 多工具调用解析测试
- 18 个 agent 工具循环测试 (单工具、多工具、错误处理、最大迭代、流式路径)
- cargo build + cargo clippy (-D warnings) 零报错

### 未实现 / stub 模块

- crates/channels/ — Channel/MessageHandler trait 已定义，无实际飞书/微信/QQ 实现
- crates/gateway/ — 路由定义完成，axum HTTP server 未实现
- crates/evolution/ — 未启动

## 13. Phase 3 执行记录 — 技能加载 & 记忆持久化

**完成日期:** 2026-06-03

### crates/skills/ 实现

| 模块 | 文件 | 说明 |
|------|------|------|
| Skill | skill.rs | Skill + SkillMetadata 结构体 |
| SkillLoader | loader.rs | 目录扫描 (glob **/SKILL.md)、缓存、模糊匹配、trigger words、summary |
| lib.rs | lib.rs | 公共导出 + doc-test |

**关键实现细节:**
- YAML frontmatter 解析：--- 分隔，serde_yaml::from_str() 解析
- 模糊匹配三层策略：exact key → case-insensitive → substring containment
- summary() 生成所有技能摘要，用于注入 system prompt

### crates/memory/ 实现

| 模块 | 文件 | 说明 |
|------|------|------|
| MemoryStore | store.rs | 文件 CRUD + section 级更新/删除 |
| MemoryFile | types.rs | Agent/User 枚举 |
| lib.rs | lib.rs | 公共导出 + doc-test |

**关键实现细节:**
- "missing = empty" 约定：文件不存在返回空字符串，不报错
- ind_section_start() / ind_section_end()：按 ## heading 精确定位
- 
eplace_section() / 
emove_section()：纯函数，与 I/O 分离

### crates/tools/ 新增

| 模块 | 文件 | 说明 |
|------|------|------|
| MemoryTool | memory.rs | 4 种操作: read/append/write/update_section |
| SecurityConfig | security.rs | 可配置安全策略 (27 种危险模式黑名单) |
| SecurityGuard | security.rs | 路径遍历保护 (validate_file_path/validate_dir_path/check_glob_pattern) |

**安全防护覆盖:**
- BashTool: 27 种危险命令模式 (rm -rf /, fork bomb, format, diskpart 等)
- ReadFileTool / WriteFileTool: 路径遍历检测
- ListDirTool / GlobTool / GrepTool: 目录路径 + glob 模式验证

### crates/agent/ 更新

- Agent 结构体集成 SkillLoader + MemoryStore
- uild_system_prompt() 三层拼接：base config + skills summary + memory snapshot
- 新增 skills() / memory() accessor 方法

### 测试统计

| Crate | 测试数 | 说明 |
|-------|--------|------|
| mc_agent | 24 | 对话循环 + 工具调用 + skills/memory 集成 |
| mc_llm | 7 | OpenAI 流式解析 |
| mc_memory | 16 | CRUD + section 操作 + 中文内容 |
| mc_skills | 13 | 加载 + 模糊匹配 + trigger words + summary |
| mc_tools | 49 | 6 工具 + MemoryTool + 27 安全测试 + glob |
| doc-tests | 3 | mc_config + mc_memory + mc_skills |
| **总计** | **112** | 全部通过 |

### 未实现 / stub 模块

- crates/channels/ — Channel/MessageHandler trait 已定义，无实际飞书/微信/QQ 实现
- crates/gateway/ — 路由定义完成，axum HTTP server 未实现
- crates/evolution/ — 未启动

### Phase 4 验收目标

1. **飞书 E2E** — 飞书消息触发对话 → Agent 工具调用 → 回复飞书
2. **配置校验** — 启动时校验必填字段 + provider 可达性 + 模型存在性
3. **Secret redaction** — 日志/错误中自动脱敏 API key、token
4. **安全默认关闭危险工具** — bash 默认禁用，需 tools.bash.enabled = true 显式开启

## 14. Phase 4 执行记录 - 网关 & 频道集成

**完成日期:** 2026-06-03

### crates/channels/ 实现

| 模块 | 文件 | 说明 |
|------|------|------|
| Channel trait | lib.rs | `Channel` + `MessageHandler` async trait, `WebhookChannel` trait |
| ChannelKind | core/channel.rs | 统一到 mc-core, serde lowercase + PascalCase alias |
| Message types | lib.rs | `IncomingMessage`, `OutgoingMessage`, `MessageContent`, `Sender` |
| Routing types | lib.rs | `ChannelTarget`, `ChannelSource` |
| FeishuChannel | feishu/channel.rs | Channel trait impl + WebhookChannel impl |
| FeishuConfig | feishu/config.rs | TOML settings + env: prefix 解析 |
| Feishu verify | feishu/verify.rs | HMAC-SHA256 webhook 签名验证 (constant-time) |
| Token manager | feishu/token.rs | tenant_access_token 自动缓存刷新 (60s 提前) |
| Message convert | feishu/convert.rs | Feishu wire format <-> internal types |
| Feishu types | feishu/types.rs | Wire-format 类型定义 (事件、token、消息) |

### crates/gateway/ 实现

| 模块 | 文件 | 说明 |
|------|------|------|
| Gateway | lib.rs | axum HTTP server + builder API |
| Routes | routes.rs | 4 条路由: health, status, webhook, chat_completions |
| ChatHandler trait | lib.rs | OpenAI 兼容 API handler trait |
| WebhookChannel | lib.rs | 改用 mc-channels 的 WebhookChannel (删除本地 Channel) |

### 路由表

| Method | Path | Handler | 说明 |
|--------|------|---------|------|
| GET | `/health` | `routes::health` | 存活探针 |
| GET | `/api/status` | `routes::status` | 已注册 channel + chat API 可用性 |
| POST | `/webhook/{channel_kind}` | `routes::webhook` | 分发到对应 channel handler |
| POST | `/v1/chat/completions` | `routes::chat_completions` | OpenAI 兼容 API |

### src/main.rs 更新

- 新增 `gateway` 子命令: 启动 HTTP server + 注册飞书 channel
- `build_agent()` helper: 复用 Agent 构造逻辑 (chat/gateway 共用)
- `AgentMessageHandler`: 飞书消息 -> Agent -> 飞书回复 (全链路)
- `AgentChatHandler`: OpenAI 兼容 `/v1/chat/completions` 实现

### 消息流 (飞书 E2E)

```
POST /webhook/feishu
  -> Gateway routes to FeishuChannel (WebhookChannel::handle_webhook)
  -> verify_and_parse() -> HMAC-SHA256 签名验证
  -> Challenge? -> return {"challenge": "..."}
  -> Message?  -> spawn background task:
      -> feishu_to_incoming() -> AgentMessageHandler::on_message()
      -> Agent::handle_message(text)
      -> FeishuChannel::send(reply) via Feishu Open API
  -> return 200 OK immediately
```

### 关键设计决策

1. **ChannelKind 统一到 mc-core**: 从两个独立定义合并为 mc-core 唯一 canonical 定义, serde `rename_all = "lowercase"` + alias 支持 PascalCase, 兼容现有 TOML 配置
2. **WebhookChannel trait**: mc-channels 新增 trait 替代 gateway 本地 Channel, 组合 webhook HTTP 处理和 channels 类型系统
3. **后台消息处理**: 飞书 webhook 立即返回 200 OK, Agent 处理在 tokio::spawn 中运行, 避免飞书重试超时
4. **Agent 复用限制**: 当前实现一个 Agent 共享所有飞书对话 (Arc<Mutex<Agent>>), 按会话隔离是后续增强

### 测试统计

| Crate | 测试数 | 说明 |
|-------|--------|------|
| mc_core | 5 | ChannelKind 序列化/显示/Hash |
| mc_channels | 44 | Channel trait + 飞书适配器 |
| mc_gateway | 15 | HTTP 路由 + ChatHandler + 集成 |
| mc_agent | 24 | 对话循环 + 工具调用 |
| mc_llm | 7 | OpenAI 格式解析 |
| mc_memory | 16 | CRUD + section 操作 |
| mc_skills | 13 | 加载 + 模糊匹配 + summary |
| mc_tools | 49 | 7 内置工具 + 安全防护 |
| doc-tests | 4 | mc_config + mc_memory + mc_skills + mc_gateway |
| **总计** | **177** | 全部通过 |

### 未实现 / 未覆盖的验收目标

| 验收目标 | 状态 | 说明 |
|----------|------|------|
| 飞书 E2E | ✅ 完成 | webhook -> Agent -> 飞书回复全链路 |
| 配置校验 | 🔲 未开始 | 启动时校验必填字段/provider 可达性 |
| Secret redaction | 🔲 未开始 | 日志/错误中自动脱敏 API key |
| 安全默认关闭危险工具 | 🔲 未开始 | bash 默认禁用需显式配置 |
| 微信适配器 | 🔲 未开始 | 计划 Phase 6 |
| QQ 适配器 | 🔲 未开始 | 计划 Phase 6 |

## 15. Phase 5 执行记录 — 自进化闭环

**完成日期:** 2026-06-03

### crates/evolution/ 实现

| 模块 | 文件 | 说明 |
|------|------|------|
| NudgeConfig | nudge.rs | `NudgeConfig` (memory_interval=10, skill_interval=10) + `ReviewTrigger` 枚举 (Memory \| Skill) |
| BackgroundReviewer | reviewer.rs | `should_review(turns, tool_iters)` 模块算术触发 + `spawn_review()` tokio::spawn 后台执行 + JSON 解析 fallback |
| ReviewResult | reviewer.rs | `memory_updates` + `skill_updates` + `summary` 结构体 |
| SkillManager | manager.rs | CRUD 操作 (create/edit/patch/delete) + read/list/exists |
| SkillProvenance | types.rs | `Bundled \| Agent \| User` 来源追踪 |
| UsageTracker | usage.rs | `.usage-log.json` 事件日志 (Create/Edit/Patch/Delete) |
| Curator | curator.rs | `SkillState` 状态机 (Active→Stale→Archived) + Pinned 免疫 + 自动转换 |
| Curator backup | curator.rs | 运行前快照到 `.backups/YYYYMMDDTHHMMSS/` |
| Curator consolidation | curator.rs | 字符串相似度扫描 + LLM 增强合并建议 |

### crates/agent/ 集成

| 变更 | 说明 |
|------|------|
| `Agent` 新增字段 | `BackgroundReviewer` + `SkillManager` + turn/tool-iteration 计数器 |
| `handle_message` 扩展 | 计数器递增 → 阈值触发 `maybe_spawn_review` → 下轮 `check_pending_review` 应用结果 |
| `apply_review_result` | ReviewResult 更新 MemoryStore + SkillManager，响应前拼接 `Self-improvement: ...` |
| `Agent::new` 签名 | 新增 `evolution_config: Option<&EvolutionConfig>` + `skills_dir: Option<PathBuf>` |
| `src/main.rs` | `build_agent` 传入 evolution config + skills_dir |

### src/main.rs 更新

- `build_agent()` 传入 `Some(&config.evolution)` 和 `Some(skills_dir)` 给 `Agent::new`

### 关键设计决策

1. **`run_review` 直接调用**: 原设计 `spawn_review` 内部再 `tokio::spawn` (double-spawn)。将 `run_review` 设为 `pub`，`maybe_spawn_review` 直接调用，消除冗余开销。
2. **零超时检查**: `check_pending_review` 使用 `tokio::time::timeout(Duration::ZERO, &mut handle)` 非阻塞检查，结果在下一轮 `handle_message` 应用。
3. **独立 MemoryStore**: `SkillManager` 创建独立的 `MemoryStore` 实例（同目录），避免与 Agent 的 `Arc<MemoryStore>` 冲突。
4. **Rust 2024 兼容**: `r##"..."##` 多 hash 保留字报错，改用 `join("\n")` 构建 review prompt。

### 测试统计

| Crate | 测试数 | 说明 |
|-------|--------|------|
| mc_core | 5 | ChannelKind 序列化/显示/Hash |
| mc_channels | 44 | Channel trait + 飞书适配器 |
| mc_gateway | 15 | HTTP 路由 + ChatHandler |
| mc_agent | 34 | 对话循环 + 工具调用 + evolution 集成 (8 新增) |
| mc_llm | 7 | OpenAI 格式解析 |
| mc_memory | 16 | CRUD + section 操作 |
| mc_skills | 13 | 加载 + 模糊匹配 + summary |
| mc_tools | 49 | 7 内置工具 + 安全防护 |
| mc_evolution | 88 | Curator(29) + SkillManager(26) + UsageTracker(4) + Nudge/Reviewer(21) + Types(8) |
| doc-tests | 4 | mc_config + mc_memory + mc_skills + mc_gateway |
| **总计** | **275** | 全部通过 (271 unit + 4 doc-tests) |

### Known Gaps / Follow-ups

1. **单 Agent 状态共享**: 一个 Agent 实例共享所有对话的 turn counter 和 review 状态，按会话隔离是后续增强。
2. **`patch` 动作未接入**: `apply_review_result` 处理 create/edit/delete 但不处理 patch (SkillPatch)，LLM review prompt 当前不生成 patch 动作。
3. **Review 超时**: 后台 review LLM 调用可能耗时数秒，零超时检查意味着结果在下一轮应用 (by design, non-blocking)。
4. **`EvolutionConfig` 级别**: 当前为 AppConfig 级别，如需 per-agent 差异化配置需迁移到 AgentConfig。

## 16. Phase 6 执行记录 — 打磨

**完成日期:** 2026-06-03

### crates/llm/ 新增

| 模块 | 文件 | 说明 |
|------|------|------|
| RetryProvider | retry.rs | 自动重试包装器：指数退避 + jitter，最多 3 次，可配置 RetryConfig |
| ProviderRegistry | registry.rs | 新增 wrap_with_retry() / wrap_all_with_retry() 方法 |

### crates/storage/ 新建 (mc-storage)

| 模块 | 文件 | 说明 |
|------|------|------|
| SessionStore | lib.rs | SQLite 会话持久化 (WAL mode) + FTS5 全文搜索 |
| SessionSummary | lib.rs | 会话列表摘要（id, title, message_count, timestamps） |
| SearchResult | lib.rs | FTS5 搜索结果（session_id, message, rank, created_at） |

**关键实现细节:**
- Mutex<Connection> 线程安全封装 (rusqlite Connection 含 RefCell，非 Sync)
- FTS5 external content mode：messages_fts USING fts5(content, content=messages, content_rowid=id)
- 3 个触发器 (messages_ai/ad/au) 保持 FTS 索引同步
- upsert 语义：save_session() 替换全部消息（delete + insert in transaction）
- 外键级联删除：ON DELETE CASCADE

### crates/agent/ 扩展

| 变更 | 说明 |
|------|------|
| session_store 字段 | Option<Arc<SessionStore>> |
| save_session() / load_session() | 会话持久化读写 |
| compress_threshold 字段 | 上下文压缩触发阈值（默认 50 条消息） |
| maybe_compress_context() | 超过阈值时 LLM 摘要压缩，保留最近 10 条 |
| Agent::new 签名 | 新增第 8 参数 session_store |

### src/main.rs 扩展

| 子命令 | 说明 |
|--------|------|
| session list | 列出所有会话（按更新时间倒序） |
| session search <query> | FTS5 全文搜索消息内容 |
| session export <id> | 导出完整会话为 JSON |

**其他变更:**
- streaming token 计数显示：[N chars, ~N tokens]
- build_agent() 中 wrap_all_with_retry() 自动为所有 provider 添加重试

### 设计决策

1. **RetryProvider 装饰器模式**: 包装任意 LlmProvider，不侵入原实现。通过 ProviderRegistry::wrap_all_with_retry() 统一应用。
2. **上下文压缩惰性触发**: 仅在消息数超过阈值时触发，不阻塞主对话流。摘要作为 assistant message 插入，保留最近 N 条原文。
3. **SessionStore 独立 crate**: mc-storage 不依赖 agent/llm/evolution，仅依赖 core + rusqlite + chrono。
4. **FTS5 外部内容模式**: FTS 索引不复制消息内容，通过触发器保持同步，节省存储空间。

### 测试总计

| Crate | 测试数 | 说明 |
|-------|--------|------|
| mc_core | 5 | ChannelKind 序列化/显示/Hash |
| mc_config | 5 | 配置加载 + 校验 |
| mc_channels | 44 | Channel trait + 飞书实现 |
| mc_gateway | 15 | HTTP 路由 + ChatHandler |
| mc_agent | 43 | 对话循环 + 工具调用 + evolution + 上下文压缩 |
| mc_llm | 14 | OpenAI 流式 + RetryProvider |
| mc_memory | 16 | CRUD + section 操作 |
| mc_skills | 13 | 加载 + 模糊匹配 + summary |
| mc_tools | 49 | 7 内置工具 + 安全防护 |
| mc_evolution | 88 | Curator + SkillManager + UsageTracker + Nudge/Reviewer |
| mc_storage | 24 | Session CRUD + FTS5 搜索 + 持久化 |
| doc-tests | 7 | mc_config + mc_memory + mc_skills + mc_gateway |
| **总计** | **318** | 全部通过 |

### Phase 6 完成度

| 计划项 | 状态 | 说明 |
|--------|------|------|
| 流式 token 显示优化 | ✅ 完成 | 每次流式输出后打印 [N chars, ~N tokens] |
| 上下文压缩（长对话） | ✅ 完成 | 超过 50 条消息时 LLM 摘要压缩 |
| SQLite 会话持久化 + FTS5 | ✅ 完成 | mc-storage crate，Agent 集成 + CLI session 子命令 |
| 错误恢复 & 重试逻辑 | ✅ 完成 | RetryProvider，指数退避 + jitter，最多 3 次 |
| Cron 定时任务 | 🔲 未开始 | 计划后续版本 |
| 更多内置工具 | 🔲 未开始 | 计划后续版本 |
| 测试覆盖 | 🔲 未开始 | 计划后续版本 |