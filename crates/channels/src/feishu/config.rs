//! Feishu channel configuration.
//!
//! Parsed from `ChannelConfig.settings` for `kind = "Feishu"`.

use mc_core::McError;

/// Feishu application credentials and settings.
#[derive(Debug, Clone)]
pub struct FeishuConfig {
    /// Feishu app ID.
    pub app_id: String,
    /// Feishu app secret.
    pub app_secret: String,
    /// Webhook verification token (used for signature verification).
    pub verification_token: String,
    /// Base URL for the Feishu Open API (default: `https://open.feishu.cn`).
    pub base_url: String,
}

impl FeishuConfig {
    /// Parse from a TOML settings table (as stored in `ChannelConfig.settings`).
    ///
    /// Supports `env:VAR_NAME` prefix for environment variable resolution.
    pub fn from_settings(settings: &toml::Table) -> Result<Self, McError> {
        let app_id = get_setting(settings, "app_id")?;
        let app_secret = get_setting(settings, "app_secret")?;
        let verification_token = get_setting(settings, "verification_token")?;
        let base_url = settings
            .get("base_url")
            .and_then(|v| v.as_str())
            .map(resolve_env)
            .unwrap_or_else(|| "https://open.feishu.cn".to_string());

        Ok(Self {
            app_id,
            app_secret,
            verification_token,
            base_url,
        })
    }
}

/// Get a required string setting, resolving `env:` prefix.
fn get_setting(table: &toml::Table, key: &str) -> Result<String, McError> {
    table
        .get(key)
        .and_then(|v| v.as_str())
        .map(resolve_env)
        .ok_or_else(|| McError::Channel(format!("feishu setting '{}' is required", key)))
}

/// If the value starts with `env:`, resolve it from the environment.
fn resolve_env(value: &str) -> String {
    if let Some(var_name) = value.strip_prefix("env:") {
        std::env::var(var_name).unwrap_or_else(|_| value.to_string())
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_from_toml_table() {
        let mut table = toml::Table::new();
        table.insert("app_id".into(), toml::Value::String("cli_abc".into()));
        table.insert("app_secret".into(), toml::Value::String("secret123".into()));
        table.insert(
            "verification_token".into(),
            toml::Value::String("token_xyz".into()),
        );

        let config = FeishuConfig::from_settings(&table).unwrap();
        assert_eq!(config.app_id, "cli_abc");
        assert_eq!(config.app_secret, "secret123");
        assert_eq!(config.verification_token, "token_xyz");
        assert_eq!(config.base_url, "https://open.feishu.cn");
    }

    #[test]
    fn parse_with_custom_base_url() {
        let mut table = toml::Table::new();
        table.insert("app_id".into(), toml::Value::String("a".into()));
        table.insert("app_secret".into(), toml::Value::String("b".into()));
        table.insert("verification_token".into(), toml::Value::String("c".into()));
        table.insert(
            "base_url".into(),
            toml::Value::String("https://custom.feishu.cn".into()),
        );

        let config = FeishuConfig::from_settings(&table).unwrap();
        assert_eq!(config.base_url, "https://custom.feishu.cn");
    }

    #[test]
    fn missing_required_field() {
        let table = toml::Table::new();
        let result = FeishuConfig::from_settings(&table);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("app_id"));
    }

    #[test]
    fn env_prefix_resolution() {
        // Set a test env var
        // SAFETY: test-only, single-threaded env mutation with unique var name
        unsafe {
            std::env::set_var("MC_TEST_FEISHU_ID", "resolved_id");
        }
        let mut table = toml::Table::new();
        table.insert(
            "app_id".into(),
            toml::Value::String("env:MC_TEST_FEISHU_ID".into()),
        );
        table.insert("app_secret".into(), toml::Value::String("s".into()));
        table.insert("verification_token".into(), toml::Value::String("t".into()));

        let config = FeishuConfig::from_settings(&table).unwrap();
        assert_eq!(config.app_id, "resolved_id");

        // Clean up
        // SAFETY: test-only cleanup
        unsafe {
            std::env::remove_var("MC_TEST_FEISHU_ID");
        }
    }

    #[test]
    fn env_prefix_fallback_when_not_set() {
        let mut table = toml::Table::new();
        table.insert(
            "app_id".into(),
            toml::Value::String("env:MC_NONEXISTENT_VAR_XYZ".into()),
        );
        table.insert("app_secret".into(), toml::Value::String("s".into()));
        table.insert("verification_token".into(), toml::Value::String("t".into()));

        let config = FeishuConfig::from_settings(&table).unwrap();
        // Fallback to the original string when env var doesn't exist.
        assert_eq!(config.app_id, "env:MC_NONEXISTENT_VAR_XYZ");
    }
}
