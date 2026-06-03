//! Feishu tenant_access_token management.
//!
//! Fetches and caches the tenant_access_token from the Feishu Open API.
//! The token is refreshed automatically 60 seconds before expiry.

use std::sync::Arc;
use std::time::Instant;

use tokio::sync::RwLock;
use tracing::{debug, warn};

use super::config::FeishuConfig;
use super::types::TokenResponse;

/// Cached token with its expiration time.
#[derive(Debug, Clone)]
struct CachedToken {
    token: String,
    /// Monotonic time when the token expires.
    expires_at: Instant,
}

/// Thread-safe tenant_access_token manager.
#[derive(Debug, Clone)]
pub struct TokenManager {
    config: Arc<FeishuConfig>,
    http: reqwest::Client,
    cache: Arc<RwLock<Option<CachedToken>>>,
}

/// Safety margin: refresh the token 60 seconds before it actually expires.
const REFRESH_MARGIN_SECS: u64 = 60;

impl TokenManager {
    pub fn new(config: Arc<FeishuConfig>, http: reqwest::Client) -> Self {
        Self {
            config,
            http,
            cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Get a valid token, refreshing if needed.
    pub async fn get_token(&self) -> Result<String, mc_core::McError> {
        // Fast path: read lock, check if cached and not expired.
        {
            let cache = self.cache.read().await;
            if let Some(ref cached) = *cache {
                if Instant::now() < cached.expires_at {
                    return Ok(cached.token.clone());
                }
            }
        }

        // Slow path: write lock, double-check, then fetch.
        let mut cache = self.cache.write().await;
        // Double-check after acquiring write lock.
        if let Some(ref cached) = *cache {
            if Instant::now() < cached.expires_at {
                return Ok(cached.token.clone());
            }
        }

        let cached = self.fetch_token().await?;
        let token = cached.token.clone();
        cache.replace(cached);
        Ok(token)
    }

    /// Fetch a fresh token from the Feishu API.
    async fn fetch_token(&self) -> Result<CachedToken, mc_core::McError> {
        let url = "https://open.feishu.cn/open-apis/auth/v3/tenant_access_token/internal";

        let body = serde_json::json!({
            "app_id": self.config.app_id,
            "app_secret": self.config.app_secret,
        });

        debug!("Fetching tenant_access_token from Feishu");

        let resp = self
            .http
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| mc_core::McError::Channel(format!("token request failed: {e}")))?;

        let token_resp: TokenResponse = resp
            .json()
            .await
            .map_err(|e| mc_core::McError::Channel(format!("token response parse failed: {e}")))?;

        if token_resp.code != 0 {
            return Err(mc_core::McError::Channel(format!(
                "Feishu token error (code {}): {}",
                token_resp.code, token_resp.msg
            )));
        }

        let token = token_resp
            .tenant_access_token
            .ok_or_else(|| mc_core::McError::Channel("token response missing token".into()))?;

        let expire_secs = token_resp.expire.unwrap_or(7200);
        // Subtract margin to ensure we refresh before actual expiry.
        let effective_secs = expire_secs.saturating_sub(REFRESH_MARGIN_SECS);

        let cached = CachedToken {
            token,
            expires_at: Instant::now() + std::time::Duration::from_secs(effective_secs),
        };

        debug!(
            expires_in = expire_secs,
            effective = effective_secs,
            "tenant_access_token cached"
        );
        Ok(cached)
    }

    /// Force-invalidate the cached token (e.g. after an auth error).
    pub async fn invalidate(&self) {
        let mut cache = self.cache.write().await;
        if cache.take().is_some() {
            warn!("tenant_access_token invalidated");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cached_token_not_expired() {
        let cached = CachedToken {
            token: "tok".into(),
            expires_at: Instant::now() + std::time::Duration::from_secs(3600),
        };
        assert!(Instant::now() < cached.expires_at);
    }

    #[test]
    fn cached_token_expired() {
        let cached = CachedToken {
            token: "tok".into(),
            expires_at: Instant::now(),
        };
        // Already at or past expiry.
        assert!(Instant::now() >= cached.expires_at);
    }
}
