//! Configuration type definitions for mavis-claw.
//!
//! All configuration structures that map to the TOML config file format.
//! Supports `env:` prefix for environment variable resolution in **all** string fields.

use serde::{Deserialize, Serialize};

/// Top-level application configuration.
///
/// Loaded from a TOML file with optional environment variable overrides.
/// Use [`AppConfig::load`] for the full loading chain (file → env vars).
///
/// All string fields support the `env:VAR_NAME` prefix for automatic
/// environment variable resolution.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// Gateway server configuration.
    #[serde(default)]
    pub gateway: GatewayConfig,
    /// Agent definitions.
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
    /// LLM provider definitions.
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// Channel (Feishu/WeChat/QQ) definitions.
    #[serde(default)]
    pub channels: Vec<ChannelConfig>,
    /// Memory system configuration.
    #[serde(default)]
    pub memory: MemoryConfig,
    /// Skills system configuration.
    #[serde(default)]
    pub skills: SkillsConfig,
    /// Evolution system configuration.
    #[serde(default)]
    pub evolution: EvolutionConfig,
    /// Built-in tools configuration.
    #[serde(default)]
    pub tools: ToolsConfig,
}

/// Gateway HTTP/WebSocket server configuration.
///
/// All string fields support `env:VAR_NAME` prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    /// Address to bind the server to (e.g., "127.0.0.1:3777").
    /// Supports `env:VAR_NAME` prefix.
    #[serde(default = "GatewayConfig::default_bind")]
    pub bind: String,
    /// Optional authentication token for the gateway API.
    /// Supports `env:VAR_NAME` prefix.
    pub auth_token: Option<String>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self {
            bind: Self::default_bind(),
            auth_token: None,
        }
    }
}

impl GatewayConfig {
    fn default_bind() -> String {
        "127.0.0.1:3777".to_string()
    }
}

/// An agent definition.
///
/// Each agent binds a model, provider, and system prompt together
/// into a configurable conversation entity.
/// All string fields support `env:VAR_NAME` prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    /// Unique agent identifier (referenced by channels).
    pub id: String,
    /// Human-readable agent name.
    pub name: String,
    /// Model to use (e.g., "gpt-4o", "claude-sonnet-4-20250514").
    pub model: String,
    /// Provider ID this agent uses (must match a `ProviderConfig::id`).
    pub provider: String,
    /// Inline system prompt text.
    pub system_prompt: Option<String>,
    /// Path to a file containing the system prompt.
    pub system_prompt_file: Option<String>,
    /// Maximum tokens for the LLM response.
    pub max_tokens: Option<u32>,
    /// Sampling temperature (0.0 - 2.0).
    pub temperature: Option<f32>,
    /// Per-agent override for the skills directory.
    /// When set, overrides `SkillsConfig::path` for this agent.
    /// Supports `env:VAR_NAME` prefix.
    pub skills_dir: Option<String>,
    /// Per-agent override for the memory directory.
    /// When set, overrides `MemoryConfig::path` for this agent.
    /// Supports `env:VAR_NAME` prefix.
    pub memory_dir: Option<String>,
}

/// An LLM provider definition.
///
/// Supports OpenAI-compatible and Anthropic-native providers.
/// All string fields support `env:VAR_NAME` prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    /// Unique provider identifier.
    pub id: String,
    /// Provider protocol kind.
    pub kind: ProviderKind,
    /// Base URL for the API (e.g., "https://api.openai.com/v1").
    /// Supports `env:VAR_NAME` prefix.
    pub base_url: String,
    /// API key. Supports `env:VAR_NAME` prefix.
    pub api_key: String,
    /// List of model names available through this provider.
    #[serde(default)]
    pub models: Vec<String>,
}

/// LLM provider protocol kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum ProviderKind {
    /// OpenAI-compatible API (also works for DeepSeek, local endpoints, etc.).
    OpenAI,
    /// Anthropic Messages API.
    Anthropic,
}

/// A channel definition (Feishu, WeChat, QQ, etc.).
///
/// Each channel binds to an agent and carries platform-specific settings.
/// All string values in `settings` support `env:VAR_NAME` prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelConfig {
    /// Unique channel identifier.
    pub id: String,
    /// Channel platform kind.
    pub kind: mc_core::ChannelKind,
    /// Agent ID this channel routes messages to.
    pub agent_id: String,
    /// Platform-specific settings as a TOML table.
    /// All nested string values support `env:VAR_NAME` prefix.
    #[serde(default)]
    pub settings: toml::Table,
}

/// Memory system configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryConfig {
    /// Whether memory system is enabled.
    #[serde(default = "MemoryConfig::default_enabled")]
    pub enabled: bool,
    /// Path to the memory directory (contains MEMORY.md and USER.md).
    /// Supports `env:VAR_NAME` prefix.
    #[serde(default = "MemoryConfig::default_path")]
    pub path: String,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            path: Self::default_path(),
        }
    }
}

impl MemoryConfig {
    fn default_enabled() -> bool {
        true
    }
    fn default_path() -> String {
        "./data/memory".to_string()
    }
}

/// Skills system configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsConfig {
    /// Whether skills system is enabled.
    #[serde(default = "SkillsConfig::default_enabled")]
    pub enabled: bool,
    /// Path to the skills directory (contains skill subdirectories with SKILL.md).
    /// Supports `env:VAR_NAME` prefix.
    #[serde(default = "SkillsConfig::default_path")]
    pub path: String,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            path: Self::default_path(),
        }
    }
}

impl SkillsConfig {
    fn default_enabled() -> bool {
        true
    }
    fn default_path() -> String {
        "./data/skills".to_string()
    }
}

/// Evolution system configuration (Background Review + Curator).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvolutionConfig {
    /// Whether evolution system is enabled.
    #[serde(default = "EvolutionConfig::default_enabled")]
    pub enabled: bool,
    /// Number of conversation turns between memory review nudges.
    #[serde(default = "EvolutionConfig::default_memory_nudge_interval")]
    pub memory_nudge_interval: u32,
    /// Number of tool iterations between skill review nudges.
    #[serde(default = "EvolutionConfig::default_skill_nudge_interval")]
    pub skill_nudge_interval: u32,
    /// Curator run interval in hours (default: 168 = 7 days).
    #[serde(default = "EvolutionConfig::default_curator_interval_hours")]
    pub curator_interval_hours: u32,
}

impl Default for EvolutionConfig {
    fn default() -> Self {
        Self {
            enabled: Self::default_enabled(),
            memory_nudge_interval: Self::default_memory_nudge_interval(),
            skill_nudge_interval: Self::default_skill_nudge_interval(),
            curator_interval_hours: Self::default_curator_interval_hours(),
        }
    }
}

impl EvolutionConfig {
    fn default_enabled() -> bool {
        true
    }
    fn default_memory_nudge_interval() -> u32 {
        10
    }
    fn default_skill_nudge_interval() -> u32 {
        10
    }
    fn default_curator_interval_hours() -> u32 {
        168
    }
}

/// Built-in tools configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolsConfig {
    /// Whether the bash tool is enabled.
    #[serde(default = "ToolsConfig::default_true")]
    pub bash: bool,
    /// Whether filesystem tools (read_file, write_file, list_dir) are enabled.
    #[serde(default = "ToolsConfig::default_true")]
    pub filesystem: bool,
    /// Whether the web_search tool is enabled.
    #[serde(default = "ToolsConfig::default_true")]
    pub web_search: bool,

    // ── Security settings ────────────────────────────────────────────
    /// Block dangerous shell commands that could destroy the system.
    /// Each entry is a case-insensitive substring to match against
    /// the command (after extracting the first meaningful token).
    /// Set to empty list `[]` to disable the blacklist.
    #[serde(default = "ToolsConfig::default_dangerous_commands")]
    pub bash_dangerous_commands: Vec<String>,

    /// Allowed path prefixes for read_file / write_file tools.
    /// Paths outside these prefixes are rejected. Default: `["."]`
    /// (current working directory only). Use `["."]` for cwd restriction,
    /// or add absolute paths like `["C:\\Users\\me\\project"]` for explicit
    /// allow-lists.
    #[serde(default = "ToolsConfig::default_allowed_paths")]
    pub allowed_paths: Vec<String>,

    /// Whether to reject filesystem paths containing `..` segments.
    /// Default: `true` (enforced).
    #[serde(default = "ToolsConfig::default_true")]
    pub deny_path_traversal: bool,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            bash: true,
            filesystem: true,
            web_search: true,
            bash_dangerous_commands: Self::default_dangerous_commands(),
            allowed_paths: Self::default_allowed_paths(),
            deny_path_traversal: true,
        }
    }
}

impl ToolsConfig {
    fn default_true() -> bool {
        true
    }

    fn default_dangerous_commands() -> Vec<String> {
        vec![
            // Linux/macOS: recursive delete root
            "rm -rf /".into(),
            "rm -rf /*".into(),
            "rm -rf ~".into(),
            // Linux/macOS: fork bomb
            ":(){ :|:& };:".into(),
            "fork bomb".into(),
            // Linux: write zeroes to disk
            "dd if=/dev/zero".into(),
            "dd if=/dev/random".into(),
            // Linux: shred files
            "shred ".into(),
            // Linux: move all to black hole
            "mv /* /dev/null".into(),
            // Windows: format disk
            "format c:".into(),
            "format d:".into(),
            "format /".into(),
            // Windows: force delete
            "del /f".into(),
            "del /s".into(),
            "rmdir /s".into(),
            // Windows: overwrite MBR
            "bootrec".into(),
            // Windows: system restore delete
            "vssadmin delete".into(),
            // Windows: registry delete
            "reg delete".into(),
            // Windows: dangerous shutdown
            "shutdown /s".into(),
            "shutdown /r".into(),
            // Windows: remove system files
            "Remove-Item".into(),
            // Windows: diskpart
            "diskpart".into(),
        ]
    }

    fn default_allowed_paths() -> Vec<String> {
        vec![".".into()]
    }
}
