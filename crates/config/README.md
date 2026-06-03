# mc-config

Configuration loading and validation for mavis-claw.

## Contents

- **AppConfig** — top-level configuration with all sub-configs
- **Sub-configs** — `GatewayConfig`, `AgentConfig`, `ProviderConfig`, `ChannelConfig`, `MemoryConfig`, `SkillsConfig`, `EvolutionConfig`, `ToolsConfig`
- **Enums** — `ProviderKind` (OpenAI/Anthropic), `ChannelKind` (Feishu/WeChat/QQ)
- **Loading logic** — TOML parsing with `env:` prefix resolution

## Loading Priority

1. CLI args (highest)
2. Environment variables (`MAVIS_*`)
3. Config file (TOML)
4. Defaults (lowest)

## Usage

```rust
use std::path::Path;
use mc_config::AppConfig;

let config = AppConfig::load(Some(Path::new("config.toml")))?;
println!("Bind: {}", config.gateway.bind);
```
