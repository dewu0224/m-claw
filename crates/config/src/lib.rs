//! Configuration system for mavis-claw.
//!
//! Provides [`AppConfig`] and all sub-configuration types. Supports loading
//! from TOML files with environment variable overrides and `env:` prefix resolution.
//!
//! ## Loading Priority
//!
//! 1. **CLI args** (highest priority — applied by the binary)
//! 2. **Environment variables** (`MAVIS_*` prefix)
//! 3. **Config file** (TOML)
//! 4. **Defaults** (lowest priority)
//!
//! ## Example
//!
//! ```rust,no_run
//! use std::path::Path;
//! use mc_config::AppConfig;
//!
//! let config = AppConfig::load(Some(Path::new("config.toml")))?;
//! println!("Gateway bind: {}", config.gateway.bind);
//! # Ok::<(), mc_core::McError>(())
//! ```

mod loader;
mod types;

pub use types::*;
