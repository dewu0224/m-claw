//! Background reviewer — periodic conversation analysis.
//!
//! The [`BackgroundReviewer`] monitors conversation activity via counters
//! and spawns a background LLM agent to analyze the conversation when
//! a nudge threshold is reached. The agent is restricted to memory and
//! skill tools only.

use std::sync::Arc;

use mc_core::{Conversation, McError, Message, Role};
use mc_llm::{ChatRequest, ChatResponse, LlmProvider};
use mc_memory::MemoryStore;
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

use crate::nudge::{NudgeConfig, ReviewTrigger};

/// A single proposed update to agent memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryUpdate {
    /// Target file: "agent" (MEMORY.md) or "user" (USER.md).
    pub target: String,
    /// Section heading under which to append/update.
    pub section: String,
    /// Content to write.
    pub content: String,
    /// "append" or "update"
    pub action: String,
}

/// A proposed skill change.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillUpdate {
    /// Skill name (without .md extension).
    pub name: String,
    /// "create", "edit", "patch", or "delete"
    pub action: String,
    /// New content (for create/edit/patch). None for delete.
    pub content: Option<String>,
}

/// Result of a background review.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewResult {
    /// Proposed memory file updates.
    pub memory_updates: Vec<MemoryUpdate>,
    /// Proposed skill changes.
    pub skill_updates: Vec<SkillUpdate>,
    /// Human-readable summary for the user.
    pub summary: String,
}

/// Background conversation reviewer.
///
/// Monitors conversation activity through the nudge counters and spawns
/// a restricted LLM agent when a threshold is reached.
pub struct BackgroundReviewer {
    nudge_config: NudgeConfig,
    llm_provider: Arc<dyn LlmProvider>,
    memory_store: Arc<MemoryStore>,
}

impl BackgroundReviewer {
    /// Create a new reviewer.
    pub fn new(
        nudge_config: NudgeConfig,
        llm_provider: Arc<dyn LlmProvider>,
        memory_store: Arc<MemoryStore>,
    ) -> Self {
        Self {
            nudge_config,
            llm_provider,
            memory_store,
        }
    }

    /// Returns the current nudge configuration.
    pub fn nudge_config(&self) -> &NudgeConfig {
        &self.nudge_config
    }

    /// Check whether a review should be triggered based on current counters.
    ///
    /// Returns `Some(ReviewTrigger::Memory)` when `turns` has reached the
    /// memory interval, or `Some(ReviewTrigger::Skill)` when `tool_iters`
    /// has reached the skill interval. If both thresholds are met, memory
    /// takes priority. Returns `None` if neither threshold is reached.
    pub fn should_review(&self, turns: u32, tool_iters: u32) -> Option<ReviewTrigger> {
        if self.nudge_config.memory_interval > 0
            && turns > 0
            && turns % self.nudge_config.memory_interval == 0
        {
            return Some(ReviewTrigger::Memory);
        }
        if self.nudge_config.skill_interval > 0
            && tool_iters > 0
            && tool_iters % self.nudge_config.skill_interval == 0
        {
            return Some(ReviewTrigger::Skill);
        }
        None
    }

    /// Spawn a background review task.
    ///
    /// Launches a `tokio::spawn` task that:
    /// 1. Builds a review prompt analyzing the conversation
    /// 2. Calls the LLM with the prompt
    /// 3. Parses the response into a [`ReviewResult`]
    ///
    /// The returned `JoinHandle` resolves to the review result.
    pub async fn spawn_review(
        &self,
        conversation: &Conversation,
        trigger: ReviewTrigger,
    ) -> JoinHandle<Result<ReviewResult, McError>> {
        let provider = Arc::clone(&self.llm_provider);
        let memory = Arc::clone(&self.memory_store);
        let conversation = conversation.clone();

        tokio::spawn(async move {
            run_review(&provider, &memory, &conversation, trigger).await
        })
    }
}

/// Build the system prompt for the review agent.
fn build_review_prompt(trigger: ReviewTrigger) -> String {
    let focus = match trigger {
        ReviewTrigger::Memory => {
            "Focus on memory updates: user corrections, technical discoveries, \
             preference changes, and new domain knowledge learned during this conversation."
        }
        ReviewTrigger::Skill => {
            "Focus on skill updates: tool usage patterns, frequently needed but \
             missing skills, skills that need patching, and opportunities for \
             new skill creation."
        }
    };

    [
        "You are a background conversation reviewer. Analyze the conversation and identify:",
        "",
        "1. **User corrections** \u{2014} moments where the user corrected the assistant's approach, output, or understanding.",
        "2. **Technical discoveries** \u{2014} new tools, APIs, patterns, or domain knowledge revealed during the conversation.",
        "3. **Preference changes** \u{2014} shifts in user communication style, output format, workflow, or tooling preferences.",
        "",
        focus,
        "",
        "Respond with a JSON object in this exact format:",
        "{",
        "  \"memory_updates\": [",
        "    {\"target\": \"agent\"|\"user\", \"section\": \"...\", \"content\": \"...\", \"action\": \"append\"|\"update\"}",
        "  ],",
        "  \"skill_updates\": [",
        "    {\"name\": \"skill-name\", \"action\": \"create\"|\"edit\"|\"patch\"|\"delete\", \"content\": \"...\" or null}",
        "  ],",
        "  \"summary\": \"Brief human-readable summary of findings.\"",
        "}",
        "",
        "If no updates are needed, return empty arrays and a summary explaining why.",
    ]
    .join("\n")
}

/// Format conversation messages into a readable transcript for the review prompt.
fn format_conversation(conversation: &Conversation) -> String {
    let mut out = String::new();
    for msg in &conversation.messages {
        let role_label = match msg.role {
            Role::System => "System",
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::Tool => "Tool",
        };
        let content = msg.content.as_deref().unwrap_or("(no content)");
        out.push_str(&format!("{role_label}: {content}\n\n"));
    }
    out
}

/// Run the review — call the LLM with the conversation and parse the result.
///
/// This is the core review logic, used by [`BackgroundReviewer::spawn_review`]
/// and available for direct invocation when the caller manages its own
/// async task lifecycle.
pub async fn run_review(
    provider: &Arc<dyn LlmProvider>,
    memory: &Arc<MemoryStore>,
    conversation: &Conversation,
    trigger: ReviewTrigger,
) -> Result<ReviewResult, McError> {
    let system_prompt = build_review_prompt(trigger);
    let transcript = format_conversation(conversation);

    // Load existing memory context for the LLM
    let agent_memory = memory.read_agent_memory().unwrap_or_default();
    let user_memory = memory.read_user_memory().unwrap_or_default();

    let user_content = format!(
        "## Existing Agent Memory\n{agent_memory}\n\n\
         ## Existing User Memory\n{user_memory}\n\n\
         ## Conversation Transcript\n{transcript}"
    );

    let request = ChatRequest {
        model: "default".to_string(),
        messages: vec![
            Message::system(system_prompt),
            Message::user(user_content),
        ],
        tools: None,
        max_tokens: Some(4096),
        temperature: Some(0.3),
        stream: false,
    };

    let response: ChatResponse = provider.chat(request).await?;

    let content = response
        .message
        .content
        .as_deref()
        .unwrap_or("{}");

    parse_review_result(content)
}

/// Parse the LLM's JSON response into a `ReviewResult`.
///
/// Falls back to a summary-only result if JSON parsing fails.
fn parse_review_result(raw: &str) -> Result<ReviewResult, McError> {
    // Try to extract JSON from the response (LLM might wrap in markdown fences)
    let json_str = extract_json(raw);

    match serde_json::from_str::<ReviewResult>(&json_str) {
        Ok(result) => Ok(result),
        Err(_) => {
            // Fall back to a summary-only result
            tracing::warn!(
                "Failed to parse review JSON, using raw text as summary"
            );
            Ok(ReviewResult {
                memory_updates: Vec::new(),
                skill_updates: Vec::new(),
                summary: raw.trim().to_string(),
            })
        }
    }
}

/// Extract JSON from a string that might be wrapped in markdown code fences.
fn extract_json(raw: &str) -> String {
    let trimmed = raw.trim();

    // Check for ```json ... ``` or ``` ... ```
    if let Some(start) = trimmed.find("```") {
        let after_fence = &trimmed[start + 3..];
        // Skip optional "json" language tag
        let after_lang = if after_fence.starts_with("json\n") || after_fence.starts_with("json\r\n") {
            after_fence[after_fence.find('\n').unwrap_or(4) + 1..].to_string()
        } else if after_fence.starts_with('\n') || after_fence.starts_with("\r\n") {
            after_fence[after_fence.find('\n').unwrap_or(0) + 1..].to_string()
        } else {
            return trimmed.to_string();
        };

        if let Some(end) = after_lang.find("```") {
            return after_lang[..end].trim().to_string();
        }
    }

    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mc_core::Message;
    use mc_llm::Usage;
    use std::pin::Pin;
    use futures::Stream;

    // ── Mock LLM Provider ────────────────────────────────────────────

    struct MockLlmProvider {
        response_content: String,
    }

    impl MockLlmProvider {
        fn new(response_content: impl Into<String>) -> Self {
            Self {
                response_content: response_content.into(),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for MockLlmProvider {
        async fn chat(&self, _request: ChatRequest) -> Result<ChatResponse, McError> {
            Ok(ChatResponse {
                message: Message::assistant(&self.response_content),
                usage: Usage {
                    prompt_tokens: 100,
                    completion_tokens: 50,
                    total_tokens: 150,
                },
                finish_reason: mc_llm::FinishReason::Stop,
            })
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<Pin<Box<dyn Stream<Item = Result<mc_llm::StreamChunk, McError>> + Send>>, McError>
        {
            unimplemented!("mock does not support streaming")
        }

        fn name(&self) -> &str {
            "mock"
        }

        fn models(&self) -> &[String] {
            &[]
        }
    }

    // ── Helper ───────────────────────────────────────────────────────

    fn make_conversation(messages: Vec<(&str, &str)>) -> Conversation {
        let mut conv = Conversation::new();
        for (role, content) in messages {
            let msg = match role {
                "user" => Message::user(content),
                "assistant" => Message::assistant(content),
                "system" => Message::system(content),
                _ => Message::user(content),
            };
            conv.push(msg);
        }
        conv
    }

    // ── should_review tests ──────────────────────────────────────────

    #[test]
    fn test_should_review_none_when_below_threshold() {
        let reviewer = BackgroundReviewer::new(
            NudgeConfig::new(10, 10),
            Arc::new(MockLlmProvider::new("{}")),
            Arc::new(MemoryStore::new("/tmp/test")),
        );
        assert_eq!(reviewer.should_review(5, 3), None);
    }

    #[test]
    fn test_should_review_memory_at_interval() {
        let reviewer = BackgroundReviewer::new(
            NudgeConfig::new(10, 10),
            Arc::new(MockLlmProvider::new("{}")),
            Arc::new(MemoryStore::new("/tmp/test")),
        );
        assert_eq!(reviewer.should_review(10, 3), Some(ReviewTrigger::Memory));
        assert_eq!(reviewer.should_review(20, 3), Some(ReviewTrigger::Memory));
    }

    #[test]
    fn test_should_review_skill_at_interval() {
        let reviewer = BackgroundReviewer::new(
            NudgeConfig::new(10, 5),
            Arc::new(MockLlmProvider::new("{}")),
            Arc::new(MemoryStore::new("/tmp/test")),
        );
        assert_eq!(reviewer.should_review(3, 5), Some(ReviewTrigger::Skill));
        assert_eq!(reviewer.should_review(3, 10), Some(ReviewTrigger::Skill));
    }

    #[test]
    fn test_should_review_memory_priority_when_both() {
        let reviewer = BackgroundReviewer::new(
            NudgeConfig::new(10, 10),
            Arc::new(MockLlmProvider::new("{}")),
            Arc::new(MemoryStore::new("/tmp/test")),
        );
        // Both 10 and 10 hit at turn=10, tool_iters=10 — memory takes priority
        assert_eq!(reviewer.should_review(10, 10), Some(ReviewTrigger::Memory));
    }

    #[test]
    fn test_should_review_zero_turns_no_trigger() {
        let reviewer = BackgroundReviewer::new(
            NudgeConfig::new(10, 10),
            Arc::new(MockLlmProvider::new("{}")),
            Arc::new(MemoryStore::new("/tmp/test")),
        );
        assert_eq!(reviewer.should_review(0, 0), None);
    }

    #[test]
    fn test_should_review_non_multiple_no_trigger() {
        let reviewer = BackgroundReviewer::new(
            NudgeConfig::new(10, 10),
            Arc::new(MockLlmProvider::new("{}")),
            Arc::new(MemoryStore::new("/tmp/test")),
        );
        assert_eq!(reviewer.should_review(7, 8), None);
        assert_eq!(reviewer.should_review(13, 8), None);
    }

    // ── spawn_review tests ───────────────────────────────────────────

    #[tokio::test]
    async fn test_spawn_review_returns_valid_result() {
        let review_json = serde_json::json!({
            "memory_updates": [
                {
                    "target": "user",
                    "section": "## Preferences",
                    "content": "Prefers concise answers",
                    "action": "append"
                }
            ],
            "skill_updates": [],
            "summary": "User corrected the assistant twice about formatting preferences."
        });

        let reviewer = BackgroundReviewer::new(
            NudgeConfig::default(),
            Arc::new(MockLlmProvider::new(review_json.to_string())),
            Arc::new(MemoryStore::new("/tmp/test")),
        );

        let conv = make_conversation(vec![
            ("user", "Please be concise."),
            ("assistant", "Got it!"),
            ("user", "Even shorter."),
            ("assistant", "Understood."),
        ]);

        let handle = reviewer.spawn_review(&conv, ReviewTrigger::Memory).await;
        let result = handle.await.unwrap().unwrap();

        assert_eq!(result.memory_updates.len(), 1);
        assert_eq!(result.memory_updates[0].target, "user");
        assert_eq!(result.memory_updates[0].section, "## Preferences");
        assert_eq!(result.memory_updates[0].action, "append");
        assert!(result.skill_updates.is_empty());
        assert!(!result.summary.is_empty());
    }

    #[tokio::test]
    async fn test_spawn_review_skill_trigger() {
        let review_json = serde_json::json!({
            "memory_updates": [],
            "skill_updates": [
                {
                    "name": "git-status",
                    "action": "create",
                    "content": "Run git status and parse output"
                }
            ],
            "summary": "Repeated git operations suggest a git skill would be useful."
        });

        let reviewer = BackgroundReviewer::new(
            NudgeConfig::default(),
            Arc::new(MockLlmProvider::new(review_json.to_string())),
            Arc::new(MemoryStore::new("/tmp/test")),
        );

        let conv = make_conversation(vec![
            ("user", "Check git status"),
            ("assistant", "On branch main..."),
        ]);

        let handle = reviewer.spawn_review(&conv, ReviewTrigger::Skill).await;
        let result = handle.await.unwrap().unwrap();

        assert_eq!(result.skill_updates.len(), 1);
        assert_eq!(result.skill_updates[0].name, "git-status");
        assert_eq!(result.skill_updates[0].action, "create");
        assert!(result.memory_updates.is_empty());
    }

    #[tokio::test]
    async fn test_spawn_review_malformed_json_fallback() {
        let reviewer = BackgroundReviewer::new(
            NudgeConfig::default(),
            Arc::new(MockLlmProvider::new("No updates needed. Everything looks good.")),
            Arc::new(MemoryStore::new("/tmp/test")),
        );

        let conv = make_conversation(vec![("user", "Hello")]);
        let handle = reviewer.spawn_review(&conv, ReviewTrigger::Memory).await;
        let result = handle.await.unwrap().unwrap();

        assert!(result.memory_updates.is_empty());
        assert!(result.skill_updates.is_empty());
        assert!(result.summary.contains("No updates needed"));
    }

    // ── extract_json tests ───────────────────────────────────────────

    #[test]
    fn test_extract_json_plain() {
        let input = "{\"memory_updates\": [], \"skill_updates\": [], \"summary\": \"ok\"}";
        assert_eq!(extract_json(input), input);
    }

    #[test]
    fn test_extract_json_with_fences() {
        let input = "```json\n{\"summary\": \"ok\"}\n```";
        assert_eq!(extract_json(input), "{\"summary\": \"ok\"}");
    }

    #[test]
    fn test_extract_json_with_fences_no_lang() {
        let input = "```\n{\"summary\": \"ok\"}\n```";
        assert_eq!(extract_json(input), "{\"summary\": \"ok\"}");
    }

    // ── parse_review_result tests ────────────────────────────────────

    #[test]
    fn test_parse_review_result_valid_json() {
        let json = "{\"memory_updates\": [], \"skill_updates\": [], \"summary\": \"All clear\"}";
        let result = parse_review_result(json).unwrap();
        assert_eq!(result.summary, "All clear");
    }

    #[test]
    fn test_parse_review_result_with_updates() {
        let review = serde_json::json!({
            "memory_updates": [
                {"target": "agent", "section": "## Tools", "content": "new tool found", "action": "append"}
            ],
            "skill_updates": [
                {"name": "deploy", "action": "delete", "content": null}
            ],
            "summary": "Found new tool, removed unused skill."
        });
        let json = review.to_string();
        let result = parse_review_result(&json).unwrap();
        assert_eq!(result.memory_updates.len(), 1);
        assert_eq!(result.skill_updates.len(), 1);
        assert_eq!(result.skill_updates[0].action, "delete");
        assert_eq!(result.skill_updates[0].content, None);
    }

    #[test]
    fn test_parse_review_result_garbage_falls_back() {
        let result = parse_review_result("This is not JSON at all.").unwrap();
        assert_eq!(result.summary, "This is not JSON at all.");
        assert!(result.memory_updates.is_empty());
        assert!(result.skill_updates.is_empty());
    }

    // ── format_conversation test ─────────────────────────────────────

    #[test]
    fn test_format_conversation() {
        let conv = make_conversation(vec![
            ("user", "Hello"),
            ("assistant", "Hi there!"),
        ]);
        let formatted = format_conversation(&conv);
        assert!(formatted.contains("User: Hello"));
        assert!(formatted.contains("Assistant: Hi there!"));
    }

    // ── build_review_prompt test ─────────────────────────────────────

    #[test]
    fn test_build_review_prompt_variants() {
        let memory_prompt = build_review_prompt(ReviewTrigger::Memory);
        assert!(memory_prompt.contains("memory updates"));
        assert!(memory_prompt.contains("user corrections"));

        let skill_prompt = build_review_prompt(ReviewTrigger::Skill);
        assert!(skill_prompt.contains("skill updates"));
        assert!(skill_prompt.contains("tool usage patterns"));
    }
}
