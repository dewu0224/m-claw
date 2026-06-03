# Changelog

All notable changes to this project will be documented in this file.

## [0.1.0] - 2026-06-03

### Added
- 初始架构设计文档 (`ARCHITECTURE.md`)
- 项目结构定义：10 个 crate (core, config, llm, agent, tools, skills, memory, evolution, channels, gateway)
- 6 阶段实施计划
- 团队协作模型：builder agents + red-blue reviewer
- 文档规范和审查标准

### Phase 1 完成 — 骨架能跑

**crates/core/**
- 消息模型：Message, Role, ToolCall, FunctionCall, ToolDefinition
- 对话模型：Conversation, ConversationMeta
- 错误类型：McError (thiserror)
- 异步 Tool trait (async_trait)

**crates/config/**
- AppConfig + 8 个子配置结构体
- TOML 加载逻辑（优先级: CLI args > env vars > config file > defaults）
- env: 前缀环境变量解析（覆盖所有 String 字段）
- deny_unknown_fields 防止配置拼写错误

**crates/llm/**
- LlmProvider async trait (chat + chat_stream)
- OpenAiProvider — 通用 OpenAI-compatible HTTP 调用 + SSE 流式
- AnthropicProvider — Anthropic Messages API + 流式
- ProviderRegistry — HashMap 管理 + from_config 构建
- 请求/响应类型：ChatRequest, ChatResponse, Usage, StreamChunk 等

**crates/agent/**
- Agent 结构体 + handle_message (非流式) + handle_message_stream (流式)
- build_system_prompt 三层拼接（base + config）
- CLI chat 子命令：交互式 REPL + 单次消息模式

**src/main.rs**
- clap CLI 入口
- config-check 子命令（加载并打印配置）
- chat 子命令（交互式对话 + 流式输出）

**其他**
- config.example.toml 示例配置
- 所有 crate 的 README.md
- cargo build + cargo clippy 0 errors 0 warnings
- 红蓝对抗审查通过（foundation 被打回修了 env 解析和 unwrap 问题）

### Phase 2 完成 — 工具系统 & 流式

**crates/tools/**
- `ToolRegistry` — 工具注册表，按名称管理工具
- 6 个内置工具：
  - `BashTool` — 执行 shell 命令（Windows: PowerShell, Unix: /bin/sh），支持超时
  - `ReadFileTool` — 读取文件内容，支持行号偏移和限制
  - `WriteFileTool` — 写入文件
  - `ListDirTool` — 列出目录内容
  - `GlobTool` — 匹配文件 glob 模式（支持 `*`, `?`, `**`）
  - `GrepTool` — 搜索文件文本内容，支持 glob 过滤
- `builtin_registry()` — 预加载所有内置工具的工厂函数
- 内置 glob 匹配工具（无外部依赖）

**crates/agent/**
- Agent 结构体集成 ToolRegistry
- `handle_message()` 完整工具调用循环（最多 10 次迭代）
- 流式工具支持：`handle_message_stream()` + `finalize_stream()` + `execute_tool_loop()`
- 32 个单元测试通过（工具循环、错误处理、多工具调用）

**src/main.rs**
- CLI chat 子命令创建 builtin_registry 并传递给 Agent
- 流式输出 + 工具循环支持
- 单次消息和交互式 REPL 模式均支持工具调用

**其他**
- cargo build + cargo clippy 0 errors 0 warnings
- 所有 32 个测试通过
- 红蓝对抗审查通过

**未实现 / stub**
- `crates/channels/` — trait 定义完成，无实际飞书/微信/QQ 实现
- `crates/gateway/` — 路由定义完成，HTTP server 未实现
- `crates/evolution/` — 未开始

### Phase 3 完成 — 技能加载 & 记忆持久化

**crates/skills/**
- `Skill` / `SkillMetadata` 结构体 — SKILL.md 解析 + YAML frontmatter 提取
- `SkillLoader` — 目录扫描（glob `**/SKILL.md`）、缓存加载、模糊匹配（大小写不敏感 + 子串匹配）
- trigger words 匹配 — 按关键词自动识别相关技能
- `summary()` — 生成所有已加载技能的摘要，注入 system prompt
- 13 个单元测试 + 1 个 doc-test 通过

**crates/memory/**
- `MemoryStore` — 基于文件系统的 MEMORY.md / USER.md CRUD
- `MemoryFile` 枚举 — Agent / User 双文件支持
- `read_agent_memory()` / `read_user_memory()` — 读取全文
- `append_agent_memory()` / `append_user_memory()` — 追加内容
- `update_section()` — 按 `## heading` 定位并替换 section 内容
- `remove_section()` — 按 `## heading` 删除整个 section
- "missing = empty" 约定 — 文件不存在时返回空字符串，不报错
- 16 个单元测试 + 1 个 doc-test 通过

**crates/tools/**
- `MemoryTool` — 新增记忆工具，支持 4 种操作：
  - `read` — 读取 MEMORY.md 或 USER.md 全文
  - `append` — 追加内容到记忆文件
  - `write` — 覆盖写入记忆文件
  - `update_section` — 按 heading 更新 section
- 内容大小限制：MAX_CONTENT_SIZE = 1MB，MAX_FILE_SIZE = 10MB
- 10 个 MemoryTool 测试通过

**crates/tools/security.rs**
- `SecurityConfig` — 可配置安全策略（`ToolsConfig` 集成）
- BashTool 危险命令黑名单 — 27 种危险模式（`rm -rf /`、`fork bomb`、`format`、`diskpart` 等）
- 路径遍历保护 — 所有文件工具（ReadFile, WriteFile, ListDir, Glob, Grep）均支持
- `validate_file_path()` / `validate_dir_path()` / `check_glob_pattern()` — 三层防护
- `SecurityGuard` — 可切换开关（`deny_path_traversal`）
- 27 个安全测试通过

**crates/agent/**
- Agent 结构体集成 `SkillLoader` + `MemoryStore`
- `build_system_prompt()` 三层拼接：base + skills summary + memory snapshot
- `accessor()` 方法 — 提供对 skills 和 memory 的只读访问
- 4 个新集成测试（skills 注入 system prompt、accessors 返回值）

**src/main.rs**
- CLI 启动时初始化 `SkillLoader`（扫描 `data/skills/`）和 `MemoryStore`（`data/memory/`）
- Agent 构造时传入 skills + memory 实例

**测试总计**
- 112 个测试全部通过（24 agent + 7 llm + 16 memory + 13 skills + 49 tools + 3 doc-tests）
- cargo clippy 0 errors 0 warnings

**未实现 / stub**
- `crates/channels/` — trait 定义完成，无实际飞书/微信/QQ 实现
- `crates/gateway/` — 路由定义完成，HTTP server 未实现
- `crates/evolution/` — 未开始

### Status
- Phase 1 ✅ 完成
- Phase 2 ✅ 完成
- Phase 3 ✅ 完成
- Phase 4 ✅ 完成（飞书 E2E、channel trait、gateway HTTP server）
- Phase 5 ✅ 完成（自进化闭环：Background Review + SkillManager + Curator）
- Phase 6 ✅ 完成（打磨：SQLite 持久化 + 上下文压缩 + 重试 + 流式 token 计数）

### Phase 4 完成 — 网关 & 频道集成

**crates/channels/**
- `Channel` + `MessageHandler` async trait — 统一消息收发接口
- `WebhookChannel` trait — HTTP webhook 处理与 channels 类型系统组合
- `ChannelKind` 统一到 `mc-core`（serde lowercase + PascalCase alias，兼容现有 TOML 配置）
- 消息类型：`IncomingMessage`, `OutgoingMessage`, `MessageContent`, `Sender`
- 路由类型：`ChannelTarget`, `ChannelSource`

**crates/channels/feishu/**
- `FeishuChannel` — Channel + WebhookChannel trait 实现
- HMAC-SHA256 webhook 签名验证（constant-time comparison）
- Feishu Event v2.0 解析（challenge + im.message.receive_v1）
- 双层编码 content 提取（JSON 字符串内嵌 JSON）
- `TokenManager` — tenant_access_token 自动缓存刷新（60s 提前刷新）
- 消息格式双向转换：Feishu wire format <-> internal types
- 44 个单元测试通过

**crates/gateway/**
- axum HTTP server + builder API
- 4 条路由：`/health`, `/api/status`, `/webhook/{channel_kind}`, `/v1/chat/completions`
- `ChatHandler` trait — OpenAI 兼容 API
- 改用 mc-channels 的 `WebhookChannel`（删除本地 Channel trait）
- 15 个测试通过

**src/main.rs**
- 新增 `gateway` 子命令 — 启动 HTTP server + 注册飞书 channel
- `build_agent()` helper — chat/gateway 共用 Agent 构造逻辑
- `AgentMessageHandler` — 飞书消息 -> Agent -> 飞书回复（全链路）
- `AgentChatHandler` — OpenAI 兼容 `/v1/chat/completions`

**测试总计**
- 177 个测试全部通过（5 core + 44 channels + 15 gateway + 24 agent + 7 llm + 16 memory + 13 skills + 49 tools + 4 doc-tests）
- cargo clippy 0 errors 0 warnings

**未实现 / 未覆盖**
- `channels`: 微信适配器（企业微信）— 计划 Phase 6
- `channels`: QQ 适配器（OneBot 协议）— 计划 Phase 6
- 配置校验 — 启动时校验必填字段/provider 可达性
- Secret redaction — 日志/错误中自动脱敏 API key
- 安全默认关闭危险工具 — bash 默认禁用需显式配置

### Phase 5 完成 — 自进化闭环

**crates/evolution/**
- `NudgeConfig` — memory_interval (默认 10 轮) + skill_interval (默认 10 次工具调用)
- `ReviewTrigger` — Memory | Skill 触发类型
- `BackgroundReviewer` — `should_review(turns, tool_iters)` 模块算术触发 + `spawn_review()` tokio::spawn 后台执行
- `ReviewResult` — memory_updates + skill_updates + summary
- `SkillManager` — CRUD 操作 (create/edit/patch/delete) + read/list/exists
- `SkillProvenance` — Bundled | Agent | User 来源追踪
- `UsageTracker` — `.usage-log.json` 事件日志
- `Curator` — SkillState 状态机 (Active→Stale→Archived) + Pinned 免疫
- `Curator backup` — 运行前快照到 `.backups/` 目录
- `Curator consolidation` — 字符串相似度扫描 + LLM 增强合并建议
- 88 个单元测试通过

**crates/agent/**
- Agent 集成 BackgroundReviewer + SkillManager + turn/tool-iteration 计数器
- `handle_message` 扩展：计数器递增 → 阈值触发 review → 下轮应用结果
- `apply_review_result` — 更新 MemoryStore + SkillManager，响应前拼接 `Self-improvement: ...`
- `Agent::new` 新增 evolution_config + skills_dir 参数
- 8 个新 evolution 集成测试

**src/main.rs**
- `build_agent` 传入 evolution config + skills_dir

**测试总计**
- 275 个测试全部通过（271 unit + 4 doc-tests）
- cargo clippy 0 errors 0 warnings

### Phase 6 完成 — 打磨

**crates/llm/**
- `RetryProvider` — 自动重试包装器，指数退避 + jitter，最多 3 次重试
- `RetryConfig` — 可配置重试策略（base_delay, max_delay, max_retries）
- `ProviderRegistry::wrap_all_with_retry()` — 统一为所有 provider 添加重试
- 8 个 RetryProvider 测试

**crates/storage/ (新建 mc-storage)**
- `SessionStore` — SQLite 会话持久化 (WAL mode) + FTS5 全文搜索
- Session CRUD：save (upsert), get, delete, list, update_title
- `append_messages()` — 增量追加消息（保持顺序）
- `search_messages()` — FTS5 全文搜索（支持短语、布尔、前缀查询）
- `export_session()` — 导出完整会话为 Conversation
- `SessionSummary` / `SearchResult` 类型
- 24 个单元测试

**crates/agent/**
- `session_store` 字段 — Option<Arc<SessionStore>>
- `save_session()` / `load_session()` — 会话持久化读写
- `compress_threshold` 字段 — 上下文压缩触发阈值（默认 50 条消息）
- `maybe_compress_context()` — 超过阈值时 LLM 摘要压缩，保留最近 10 条
- `Agent::new` 新增第 8 参数 session_store
- 6 个上下文压缩测试 + Agent 测试总数升至 43

**src/main.rs**
- `session` 子命令：`list`（列出所有会话）、`search <query>`（FTS5 搜索）、`export <id>`（导出 JSON）
- 流式 token 计数显示：`[N chars, ~N tokens]`
- `build_agent()` 自动 wrap_all_with_retry()

**测试总计**
- 318 个测试全部通过（271 unit + 47 storage/llm/agent 新增 + 7 doc-tests）
- cargo clippy 0 errors 0 warnings

### Phase 4 验收目标

1. **飞书 E2E** — ✅ 通过飞书消息触发对话，Agent 使用工具完成任务并回复
2. **配置校验** — 🔲 启动时校验必填字段、provider 可达性、模型存在性，给出清晰错误
3. **Secret redaction** — 🔲 日志/错误信息中自动脱敏 API key、token 等敏感信息
4. **安全默认关闭危险工具** — 🔲 bash 默认禁用，需显式配置 `tools.bash.enabled = true` 才开启
