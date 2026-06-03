//! Retry wrapper with exponential backoff for LLM providers.
//!
//! [`RetryProvider`] wraps any [`LlmProvider`] and automatically retries
//! failed requests with exponential backoff and jitter. Transient errors
//! (network failures, 429 rate limits, 5xx server errors) are retried;
//! permanent errors (4xx client errors, parse failures) fail immediately.

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use rand::Rng;
use tracing::warn;

use mc_core::McError;

use crate::trait_def::LlmProvider;
use crate::types::{ChatRequest, ChatResponse, StreamChunk};

/// Configuration for the retry behavior.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (0 = no retries).
    pub max_retries: u32,
    /// Base delay between retries (doubled each attempt).
    pub base_delay: Duration,
    /// Maximum delay cap (prevents unbounded backoff).
    pub max_delay: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
        }
    }
}

/// An [`LlmProvider`] wrapper that adds automatic retry with exponential backoff.
///
/// Transient errors are retried up to `config.max_retries` times with
/// exponential backoff and jitter. Permanent errors fail immediately.
///
/// # Retryable Errors
///
/// - Network/HTTP errors from `reqwest`
/// - HTTP 429 (rate limit) responses
/// - HTTP 5xx (server error) responses
///
/// # Non-retryable Errors
///
/// - HTTP 4xx (client error) responses (except 429)
/// - JSON parse failures
/// - Stream creation failures after a successful HTTP response
pub struct RetryProvider {
    inner: Arc<dyn LlmProvider>,
    config: RetryConfig,
}

impl RetryProvider {
    /// Wrap an existing provider with retry logic.
    pub fn new(inner: Arc<dyn LlmProvider>, config: RetryConfig) -> Self {
        Self { inner, config }
    }

    /// Wrap with default retry config (3 retries, 500ms base, 30s max).
    pub fn with_defaults(inner: Arc<dyn LlmProvider>) -> Self {
        Self::new(inner, RetryConfig::default())
    }

    /// Check if an error is retryable.
    ///
    /// Retryable errors include network failures, rate limits (429),
    /// and server errors (5xx). Parse failures and client errors (4xx)
    /// are not retryable.
    fn is_retryable(err: &McError) -> bool {
        match err {
            McError::Llm(msg) => {
                // Network errors from reqwest
                if msg.contains("HTTP request failed") {
                    return true;
                }
                // Rate limit
                if msg.contains("HTTP 429") {
                    return true;
                }
                // Server errors (5xx)
                if msg.contains("HTTP 500")
                    || msg.contains("HTTP 502")
                    || msg.contains("HTTP 503")
                    || msg.contains("HTTP 504")
                {
                    return true;
                }
                // Stream read errors
                if msg.contains("Stream read error") {
                    return true;
                }
                false
            }
            // IO errors are typically retryable (network issues)
            McError::Io(_) => true,
            // Everything else is permanent
            _ => false,
        }
    }

    /// Compute the delay before the next retry attempt.
    ///
    /// Uses exponential backoff (base_delay * 2^attempt) capped at max_delay,
    /// with +/-25% jitter to prevent thundering herd.
    fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let base = self.config.base_delay.as_millis() as u64;
        let exponential = base.saturating_mul(1u64 << attempt.min(20));
        let capped = exponential.min(self.config.max_delay.as_millis() as u64);

        // Add +/-25% jitter
        let jitter_range = capped / 4;
        let mut rng = rand::rng();
        let jitter = rng.random_range(0..=jitter_range * 2);
        let final_ms = capped.saturating_sub(jitter_range).saturating_add(jitter);

        Duration::from_millis(final_ms.max(1))
    }
}

#[async_trait]
impl LlmProvider for RetryProvider {
    async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, McError> {
        let mut last_err = None;

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let delay = self.delay_for_attempt(attempt - 1);
                warn!(
                    attempt,
                    max_retries = self.config.max_retries,
                    delay_ms = delay.as_millis(),
                    "Retrying LLM chat request"
                );
                tokio::time::sleep(delay).await;
            }

            match self.inner.chat(request.clone()).await {
                Ok(response) => return Ok(response),
                Err(err) => {
                    if Self::is_retryable(&err) && attempt < self.config.max_retries {
                        last_err = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            }
        }

        // All retries exhausted
        Err(last_err.unwrap_or_else(|| McError::Llm("All retries exhausted".to_string())))
    }

    async fn chat_stream(
        &self,
        request: ChatRequest,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, McError>> + Send>>, McError> {
        let mut last_err = None;

        for attempt in 0..=self.config.max_retries {
            if attempt > 0 {
                let delay = self.delay_for_attempt(attempt - 1);
                warn!(
                    attempt,
                    max_retries = self.config.max_retries,
                    delay_ms = delay.as_millis(),
                    "Retrying LLM stream request"
                );
                tokio::time::sleep(delay).await;
            }

            match self.inner.chat_stream(request.clone()).await {
                Ok(stream) => return Ok(stream),
                Err(err) => {
                    if Self::is_retryable(&err) && attempt < self.config.max_retries {
                        last_err = Some(err);
                        continue;
                    }
                    return Err(err);
                }
            }
        }

        Err(last_err.unwrap_or_else(|| McError::Llm("All retries exhausted".to_string())))
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn models(&self) -> &[String] {
        self.inner.models()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{FinishReason, Usage};
    use mc_core::Message;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Mock provider that fails a configurable number of times, then succeeds.
    struct FailThenSucceedProvider {
        fail_count: AtomicUsize,
        call_log: AtomicUsize,
    }

    impl FailThenSucceedProvider {
        fn new(fails: usize) -> Self {
            Self {
                fail_count: AtomicUsize::new(fails),
                call_log: AtomicUsize::new(0),
            }
        }

        fn total_calls(&self) -> usize {
            self.call_log.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl LlmProvider for FailThenSucceedProvider {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse, McError> {
            self.call_log.fetch_add(1, Ordering::SeqCst);
            let remaining = self.fail_count.fetch_sub(1, Ordering::SeqCst);
            if remaining > 0 {
                Err(McError::Llm("HTTP request failed: connection reset".to_string()))
            } else {
                Ok(ChatResponse {
                    message: Message::assistant("Success!"),
                    usage: Usage::default(),
                    finish_reason: FinishReason::Stop,
                })
            }
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, McError>> + Send>>, McError>
        {
            self.call_log.fetch_add(1, Ordering::SeqCst);
            let remaining = self.fail_count.fetch_sub(1, Ordering::SeqCst);
            if remaining > 0 {
                Err(McError::Llm("HTTP request failed: timeout".to_string()))
            } else {
                Ok(Box::pin(futures::stream::iter(vec![Ok(StreamChunk {
                    delta: "Hello".to_string(),
                    tool_call_delta: None,
                    finish_reason: Some(FinishReason::Stop),
                })])))
            }
        }

        fn name(&self) -> &str {
            "fail-then-succeed"
        }

        fn models(&self) -> &[String] {
            &[]
        }
    }

    /// Mock provider that always fails with a non-retryable error.
    struct PermanentFailProvider;

    #[async_trait]
    impl LlmProvider for PermanentFailProvider {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse, McError> {
            Err(McError::Llm("API returned HTTP 401: unauthorized".to_string()))
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<StreamChunk, McError>> + Send>>, McError>
        {
            Err(McError::Llm(
                "API returned HTTP 403: forbidden".to_string(),
            ))
        }

        fn name(&self) -> &str {
            "permanent-fail"
        }

        fn models(&self) -> &[String] {
            &[]
        }
    }

    fn make_request() -> ChatRequest {
        ChatRequest {
            model: "test-model".to_string(),
            messages: vec![Message::user("Hello")],
            tools: None,
            max_tokens: Some(100),
            temperature: None,
            stream: false,
        }
    }

    #[tokio::test]
    async fn retry_succeeds_after_transient_failures() {
        let inner = Arc::new(FailThenSucceedProvider::new(2));
        let config = RetryConfig {
            max_retries: 3,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(100),
        };
        let provider = RetryProvider::new(inner.clone(), config);

        let result = provider.chat(make_request()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().message.content.as_deref(), Some("Success!"));
        // Should have made 3 calls: 2 failures + 1 success
        assert_eq!(inner.total_calls(), 3);
    }

    #[tokio::test]
    async fn retry_exhausts_max_retries() {
        let inner = Arc::new(FailThenSucceedProvider::new(5));
        let config = RetryConfig {
            max_retries: 3,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(100),
        };
        let provider = RetryProvider::new(inner.clone(), config);

        let result = provider.chat(make_request()).await;
        assert!(result.is_err());
        // Should have made 4 calls: 1 initial + 3 retries
        assert_eq!(inner.total_calls(), 4);
    }

    #[tokio::test]
    async fn no_retry_on_permanent_error() {
        let inner = Arc::new(PermanentFailProvider);
        let config = RetryConfig {
            max_retries: 3,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(100),
        };
        let provider = RetryProvider::new(inner, config);

        let result = provider.chat(make_request()).await;
        assert!(result.is_err());
        let err_msg = format!("{}", result.unwrap_err());
        assert!(err_msg.contains("401"));
    }

    #[tokio::test]
    async fn no_retry_when_max_retries_is_zero() {
        let inner = Arc::new(FailThenSucceedProvider::new(1));
        let config = RetryConfig {
            max_retries: 0,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(100),
        };
        let provider = RetryProvider::new(inner.clone(), config);

        let result = provider.chat(make_request()).await;
        assert!(result.is_err());
        assert_eq!(inner.total_calls(), 1);
    }

    #[tokio::test]
    async fn stream_retry_succeeds_after_transient_failure() {
        let inner = Arc::new(FailThenSucceedProvider::new(1));
        let config = RetryConfig {
            max_retries: 2,
            base_delay: Duration::from_millis(10),
            max_delay: Duration::from_millis(100),
        };
        let provider = RetryProvider::new(inner.clone(), config);

        let stream_result = provider.chat_stream(make_request()).await;
        assert!(stream_result.is_ok());
        assert_eq!(inner.total_calls(), 2);
    }

    #[tokio::test]
    async fn provider_name_and_models_pass_through() {
        let inner = Arc::new(PermanentFailProvider);
        let provider = RetryProvider::with_defaults(inner);
        assert_eq!(provider.name(), "permanent-fail");
        assert!(provider.models().is_empty());
    }

    #[test]
    fn is_retryable_classifies_errors() {
        // Network error -> retryable
        assert!(RetryProvider::is_retryable(&McError::Llm(
            "HTTP request failed: connection reset".to_string()
        )));
        // Rate limit -> retryable
        assert!(RetryProvider::is_retryable(&McError::Llm(
            "API returned HTTP 429: rate limited".to_string()
        )));
        // Server error -> retryable
        assert!(RetryProvider::is_retryable(&McError::Llm(
            "API returned HTTP 503: service unavailable".to_string()
        )));
        // Client error -> not retryable
        assert!(!RetryProvider::is_retryable(&McError::Llm(
            "API returned HTTP 400: bad request".to_string()
        )));
        // Auth error -> not retryable
        assert!(!RetryProvider::is_retryable(&McError::Llm(
            "API returned HTTP 401: unauthorized".to_string()
        )));
        // Parse error -> not retryable
        assert!(!RetryProvider::is_retryable(&McError::Llm(
            "Failed to parse response JSON: invalid".to_string()
        )));
        // IO error -> retryable
        assert!(RetryProvider::is_retryable(&McError::Io(
            std::io::Error::new(std::io::ErrorKind::ConnectionReset, "reset")
        )));
        // Tool error -> not retryable
        assert!(!RetryProvider::is_retryable(&McError::Tool(
            "tool failed".to_string()
        )));
    }
}
