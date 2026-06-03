//! Configuration loading and validation.
//!
//! Implements the loading priority chain: CLI args > env vars → config file → defaults.
//! Supports `env:VAR_NAME` prefix in **all** string fields for environment variable resolution.

use std::path::Path;

use tracing::debug;

use crate::types::*;
use mc_core::McError;

impl AppConfig {
    /// Load configuration from a TOML file with environment variable overrides.
    ///
    /// **Priority chain:** CLI args > env vars > config file > defaults.
    ///
    /// - If `config_path` is `None`, returns the default configuration.
    /// - **All** string fields prefixed with `env:` are resolved from environment variables.
    /// - Specific env vars (`MAVIS_GATEWAY_BIND`, `MAVIS_MEMORY_PATH`, etc.) can
    ///   override file values.
    pub fn load(config_path: Option<&Path>) -> Result<Self, McError> {
        let mut config = match config_path {
            Some(path) => {
                debug!("Loading config from: {}", path.display());
                let content = std::fs::read_to_string(path).map_err(|e| {
                    McError::Config(format!(
                        "Failed to read config file '{}': {}",
                        path.display(),
                        e
                    ))
                })?;
                toml::from_str::<AppConfig>(&content)
                    .map_err(|e| McError::Config(format!("TOML parse error: {e}")))?
            }
            None => {
                debug!("No config file specified, using defaults");
                AppConfig::default()
            }
        };

        // Apply environment variable overrides (MAVIS_* vars)
        config.apply_env_overrides();

        // Resolve env: prefixed values in ALL string fields
        config.resolve_env_refs();

        Ok(config)
    }

    /// Apply environment variable overrides for top-level config values.
    fn apply_env_overrides(&mut self) {
        if let Ok(bind) = std::env::var("MAVIS_GATEWAY_BIND") {
            debug!("Overriding gateway.bind from MAVIS_GATEWAY_BIND");
            self.gateway.bind = bind;
        }

        if let Ok(token) = std::env::var("MAVIS_GATEWAY_AUTH_TOKEN") {
            debug!("Overriding gateway.auth_token from MAVIS_GATEWAY_AUTH_TOKEN");
            self.gateway.auth_token = Some(token);
        }

        if let Ok(path) = std::env::var("MAVIS_MEMORY_PATH") {
            debug!("Overriding memory.path from MAVIS_MEMORY_PATH");
            self.memory.path = path;
        }

        if let Ok(path) = std::env::var("MAVIS_SKILLS_PATH") {
            debug!("Overriding skills.path from MAVIS_SKILLS_PATH");
            self.skills.path = path;
        }
    }

    /// Resolve `env:` prefixed values in **every** string field across the entire config.
    fn resolve_env_refs(&mut self) {
        // Gateway
        resolve_str(&mut self.gateway.bind);
        if let Some(ref mut token) = self.gateway.auth_token {
            resolve_str(token);
        }

        // Providers
        for p in &mut self.providers {
            resolve_str(&mut p.id);
            resolve_str(&mut p.base_url);
            resolve_str(&mut p.api_key);
            for model in &mut p.models {
                resolve_str(model);
            }
        }

        // Agents
        for a in &mut self.agents {
            resolve_str(&mut a.id);
            resolve_str(&mut a.name);
            resolve_str(&mut a.model);
            resolve_str(&mut a.provider);
            if let Some(ref mut s) = a.system_prompt {
                resolve_str(s);
            }
            if let Some(ref mut s) = a.system_prompt_file {
                resolve_str(s);
            }
        }

        // Channels
        for ch in &mut self.channels {
            resolve_str(&mut ch.id);
            resolve_str(&mut ch.agent_id);
            resolve_toml_table(&mut ch.settings);
        }

        // Memory / Skills paths
        resolve_str(&mut self.memory.path);
        resolve_str(&mut self.skills.path);
    }
}

/// Resolve `env:` prefix on a single `&mut String` in-place.
///
/// If the value starts with `env:VAR_NAME`, replaces it with the value of
/// environment variable `VAR_NAME`. If the variable is not found, replaces
/// with an empty string and logs a warning.
fn resolve_str(value: &mut String) {
    if let Some(var_name) = value.strip_prefix("env:") {
        match std::env::var(var_name) {
            Ok(val) => {
                debug!("Resolved env:{var_name} from environment");
                *value = val;
            }
            Err(_) => {
                tracing::warn!(
                    "Environment variable '{var_name}' not found (referenced in config)"
                );
                value.clear();
            }
        }
    }
}

/// Recursively resolve `env:` prefixed string values in a TOML table.
fn resolve_toml_table(table: &mut toml::Table) {
    let keys: Vec<String> = table.keys().cloned().collect();
    for key in keys {
        if let Some(value) = table.get_mut(&key) {
            match value {
                toml::Value::String(s) => {
                    let resolved = resolve_env_string(s);
                    *s = resolved;
                }
                toml::Value::Table(inner) => {
                    resolve_toml_table(inner);
                }
                _ => {}
            }
        }
    }
}

/// Resolve `env:` prefix on a string slice, returning the resolved value.
fn resolve_env_string(value: &str) -> String {
    if let Some(var_name) = value.strip_prefix("env:") {
        match std::env::var(var_name) {
            Ok(val) => {
                debug!("Resolved env:{var_name} from environment");
                val
            }
            Err(_) => {
                tracing::warn!(
                    "Environment variable '{var_name}' not found (referenced in config)"
                );
                String::new()
            }
        }
    } else {
        value.to_string()
    }
}
