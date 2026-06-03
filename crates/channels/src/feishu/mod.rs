//! Feishu / Lark channel adapter.
//!
//! This module provides the [`FeishuChannel`] implementation of the [`Channel`]
//! trait for the Feishu (飞书) messaging platform.
//!
//! # Architecture
//!
//! - **Webhook receiving**: The gateway calls [`FeishuChannel::verify_and_parse`]
//!   for each incoming webhook request. This verifies the HMAC-SHA256 signature,
//!   parses the event, and returns a [`WebhookResult`].
//!
//! - **Message sending**: [`Channel::send`] posts to the Feishu Open API
//!   (`/im/v1/messages`) with a `tenant_access_token` obtained via
//!   [`token::TokenManager`].
//!
//! - **Token management**: `TokenManager` caches the `tenant_access_token`
//!   and refreshes it automatically 60 seconds before expiry.

pub mod channel;
pub mod config;
pub mod convert;
pub mod token;
pub mod types;
pub mod verify;

pub use channel::{FeishuChannel, WebhookResult};
pub use config::FeishuConfig;
pub use types::MessageEvent;
