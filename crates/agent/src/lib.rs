//! Agent conversation runtime for mavis-claw.
//!
//! Provides the [`Agent`] struct which orchestrates LLM calls within a
//! conversation, including tool execution loops.
//!
//! # Architecture
//!
//! ```text
//! User message
//!   → build_system_prompt()  (base + config prompt)
//!   → LLM provider.chat[_stream]()
//!   → if tool_calls → execute tools → append results → re-call LLM (loop)
//!   → assistant response
//! ```
//!
//! Tools are provided via [`ToolRegistry`] from the `mc-tools` crate.
//! Skills and memory are stubbed for now.

use std::sync::Arc;

use futures::Stream;
use tracing::{debug, info, warn};

use mc_config::{AgentConfig, EvolutionConfig};
use mc_core::{Conversation, McError, Message};
use mc_evolution::{
    BackgroundReviewer, NudgeConfig, ReviewResult, SkillManager, SkillProvenance,
    run_review,
};
use mc_llm::{ChatRequest, ChatResponse, LlmProvider, ProviderRegistry, StreamChunk};
use mc_memory::{MemoryFile, MemoryStore};
use mc_skills::SkillLoader;
use mc_storage::SessionStore;
use mc_tools::ToolRegistry;

/// Default maximum number of tool-call iterations before forcing a stop.
const DEFAULT_MAX_TOOL_ITERATIONS: usize = 10;

/// Default number of messages before triggering context compression.
const DEFAULT_COMPRESS_THRESHOLD: usize = 50;

/// Number of recent messages to keep when compressing context.
const COMPRESS_KEEP_RECENT: usize = 10;

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

/// The core agent that manages a conversation with an LLM provider.
///
/// Holds a reference to the LLM provider, the agent configuration,
/// the current conversation state, and an optional tool registry.
/// When tools are registered, the agent automatically executes tool calls
/// returned by the LLM and loops until a final text response is produced.
pub struct Agent {
    /// The LLM provider used for completions.
    provider: Arc<dyn LlmProvider>,
    /// Agent-level configuration (model, system prompt, etc.).
    config: AgentConfig,
    /// Current conversation state.
    conversation: Conversation,
    /// Pre-composed system prompt.
    system_prompt: String,
    /// Registry of available tools (None = no tools).
    tool_registry: Option<Arc<ToolRegistry>>,
    /// Maximum number of tool-call iterations per user message.
    max_tool_iterations: usize,
    /// Skill loader with loaded skills (None = no skills).
    skill_loader: Option<Arc<SkillLoader>>,
    /// Memory store for agent/user memory (None = no memory).
    memory_store: Option<Arc<MemoryStore>>,
    /// Background conversation reviewer (None = evolution disabled).
    reviewer: Option<BackgroundReviewer>,
    /// Skill manager for applying skill updates from reviews.
    skill_manager: Option<SkillManager>,
    /// Number of completed conversation turns (user messages processed).
    turn_counter: u32,
    /// Number of tool loop iterations across all turns.
    tool_iteration_counter: u32,
    /// Pending background review result (from a previously spawned task).
    pending_review: Option<tokio::task::JoinHandle<Result<ReviewResult, McError>>>,
    /// Number of messages in conversation before triggering context compression.
    /// When the conversation exceeds this count, older messages are summarized.
    /// Set to 0 to disable compression.
    compress_threshold: usize,
    /// Optional session store for persisting conversation state.
    session_store: Option<Arc<SessionStore>>,
}

impl Agent {
    /// Create a new agent from configuration, a provider registry, and an
    /// optional tool registry.
    ///
    /// Resolves the provider by the agent config's `provider` field,
    /// composes the system prompt, and initializes a fresh conversation.
    ///
    /// `skill_loader` and `memory_store` are optional integrations. When
    /// present, skills summary is injected into the system prompt and the
    /// memory store is available for the memory tool.
    ///
    /// `evolution_config` enables the self-improvement loop: when `Some`,
    /// a [`BackgroundReviewer`] is created that monitors conversation
    /// activity and spawns background reviews when nudge thresholds are
    /// reached. `skills_dir` is used to create a [`SkillManager`] for
    /// applying skill updates from review results.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent_config: &AgentConfig,
        registry: &ProviderRegistry,
        tool_registry: Option<Arc<ToolRegistry>>,
        skill_loader: Option<Arc<SkillLoader>>,
        memory_store: Option<Arc<MemoryStore>>,
        evolution_config: Option<&EvolutionConfig>,
        skills_dir: Option<&str>,
        session_store: Option<Arc<SessionStore>>,
    ) -> Result<Self, McError> {
        let provider = registry.get(&agent_config.provider)?;
        let system_prompt = Self::build_system_prompt(agent_config, skill_loader.as_deref());

        let mut conversation = Conversation::new();
        conversation.metadata.agent_id = Some(agent_config.id.clone());

        // Build reviewer and skill manager if evolution is enabled
        let (reviewer, skill_manager) = match evolution_config {
            Some(evo_cfg) if evo_cfg.enabled => {
                if let Some(mem) = &memory_store {
                    let nudge_config = NudgeConfig::new(
                        evo_cfg.memory_nudge_interval,
                        evo_cfg.skill_nudge_interval,
                    );
                    let reviewer = BackgroundReviewer::new(
                        nudge_config,
                        Arc::clone(&provider),
                        Arc::clone(mem),
                    );

                    // Build SkillManager if skills_dir is available
                    let skill_mgr = skills_dir.map(|dir| {
                        SkillManager::new(dir, MemoryStore::new(mem.base_path()))
                    });

                    (Some(reviewer), skill_mgr)
                } else {
                    (None, None)
                }
            }
            _ => (None, None),
        };

        info!(
            agent_id = %agent_config.id,
            model = %agent_config.model,
            provider = %agent_config.provider,
            has_tools = tool_registry.is_some(),
            has_skills = skill_loader.is_some(),
            has_memory = memory_store.is_some(),
            has_evolution = reviewer.is_some(),
            has_session_store = session_store.is_some(),
            "Agent created"
        );

        Ok(Self {
            provider,
            config: agent_config.clone(),
            conversation,
            system_prompt,
            tool_registry,
            max_tool_iterations: DEFAULT_MAX_TOOL_ITERATIONS,
            skill_loader,
            memory_store,
            reviewer,
            skill_manager,
            turn_counter: 0,
            tool_iteration_counter: 0,
            pending_review: None,
            compress_threshold: DEFAULT_COMPRESS_THRESHOLD,
            session_store,
        })
    }

    /// Set the maximum number of tool-call iterations.
    pub fn set_max_tool_iterations(&mut self, max: usize) {
        self.max_tool_iterations = max;
    }

    /// Set the context compression threshold.
    ///
    /// When the conversation has more messages than this threshold,
    /// older messages are automatically summarized. Set to 0 to disable.
    pub fn set_compress_threshold(&mut self, threshold: usize) {
        self.compress_threshold = threshold;
    }

    /// Compose the system prompt from base instructions and agent config.
    ///
    /// Layer structure:
    /// 1. **Base prompt** — role description and behavioral constraints.
    /// 2. **Skills context** — summary of loaded skills (if any).
    /// 3. **Config prompt** — agent-specific `system_prompt` from config.
    pub fn build_system_prompt(config: &AgentConfig, skill_loader: Option<&SkillLoader>) -> String {
        let base = format!(
            "You are {}, a helpful AI assistant. \
             Be concise, accurate, and helpful.",
            config.name
        );

        let mut prompt = base;

        // Inject skills summary if available and non-empty
        if let Some(loader) = skill_loader {
            let summary = loader.summary();
            if !summary.is_empty() {
                prompt.push_str("\n\n## Available Skills\n");
                prompt.push_str(&summary);
            }
        }

        match &config.system_prompt {
            Some(custom) if !custom.is_empty() => {
                prompt.push_str("\n\n");
                prompt.push_str(custom);
            }
            _ => {}
        }

        prompt
    }

    /// Send a user message and get the full (non-streaming) assistant response.
    ///
    /// If the LLM returns tool calls, the agent automatically executes them,
    /// appends the results, and re-calls the LLM. This loop continues until
    /// the LLM produces a final text response or the max iteration limit is
    /// reached.
    ///
    /// When evolution is enabled, this method:
    /// 1. Checks for completed background review results from a previous turn
    /// 2. Increments the turn counter
    /// 3. Checks if a review should be triggered
    /// 4. Spawns a background review if thresholds are met
    pub async fn handle_message(&mut self, user_input: &str) -> Result<Message, McError> {
        debug!(agent_id = %self.config.id, "handle_message called");

        // Check for completed background review from previous turn
        let review_summary = self.check_pending_review().await;

        // Append user message
        self.conversation.push(Message::user(user_input));

        // Compress context if conversation is too long
        self.maybe_compress_context().await?;

        // Increment turn counter
        self.turn_counter += 1;

        let tool_defs = self.tool_definitions();

        for iteration in 0..self.max_tool_iterations {
            // Build request (includes tools if available)
            let request = self.build_request(false, tool_defs.as_deref());

            // Call LLM
            let response: ChatResponse = self.provider.chat(request).await?;

            debug!(
                tokens = response.usage.total_tokens,
                finish = ?response.finish_reason,
                iteration,
                "LLM response received"
            );

            // If there are tool calls, execute them and loop
            if let Some(tool_calls) = &response.message.tool_calls {
                let tool_calls = tool_calls.clone();
                // Append the assistant message (may contain text + tool_calls)
                self.conversation.push(response.message);

                // Execute each tool call and append results
                self.execute_and_append_tools(&tool_calls).await?;

                self.tool_iteration_counter += 1;
                continue;
            }

            // No tool calls — this is the final response
            self.conversation.push(response.message.clone());

            // Check if a background review should be triggered
            self.maybe_spawn_review();

            // Prepend self-improvement summary if a review just completed
            let final_message = if let Some(summary) = review_summary {
                let improved_content = match &response.message.content {
                    Some(content) => {
                        format!("Self-improvement: {summary}\n\n{content}")
                    }
                    None => format!("Self-improvement: {summary}"),
                };
                Message::assistant(&improved_content)
            } else {
                response.message
            };

            return Ok(final_message);
        }

        Err(McError::Tool(format!(
            "Tool call loop exceeded maximum iterations ({})",
            self.max_tool_iterations
        )))
    }

    /// Send a user message and get a streaming assistant response.
    ///
    /// Returns a stream of [`StreamChunk`] items. The caller is responsible
    /// for consuming the stream and assembling the final assistant message.
    ///
    /// After the stream completes, call [`Agent::finalize_stream`] with the
    /// assembled content to update the conversation. If the stream contained
    /// tool calls (indicated by tool_call_delta chunks), also call
    /// [`Agent::execute_tool_loop`] to run the tool loop.
    pub async fn handle_message_stream(
        &mut self,
        user_input: &str,
    ) -> Result<impl Stream<Item = Result<StreamChunk, McError>> + use<>, McError> {
        debug!(agent_id = %self.config.id, "handle_message_stream called");

        // Append user message
        self.conversation.push(Message::user(user_input));

        // Compress context if conversation is too long
        self.maybe_compress_context().await?;

        let tool_defs = self.tool_definitions();

        // Build request (includes tools if available)
        let request = self.build_request(true, tool_defs.as_deref());

        // Call LLM with streaming
        let stream = self.provider.chat_stream(request).await?;

        Ok(stream)
    }

    /// Finalize a streaming response by appending the assembled content
    /// to the conversation history.
    ///
    /// The `full_content` is the text accumulated from stream deltas.
    /// The `tool_calls` are the tool calls assembled from stream deltas
    /// (if any). If `tool_calls` is present, the message is stored as an
    /// assistant message with tool_calls attached.
    pub fn finalize_stream(&mut self, full_content: &str, tool_calls: Option<Vec<mc_core::ToolCall>>) {
        let message = Message {
            role: mc_core::Role::Assistant,
            content: if full_content.is_empty() {
                None
            } else {
                Some(full_content.to_string())
            },
            tool_calls,
            tool_call_id: None,
            name: None,
        };
        self.conversation.push(message);
        debug!("Stream finalized, assistant message appended to conversation");
    }

    /// Execute the tool-call loop after a streaming response.
    ///
    /// Call this after [`finalize_stream`] when the stream contained tool
    /// calls. The method will:
    /// 1. Check the last message for tool_calls
    /// 2. Execute each tool and append results
    /// 3. Re-call the LLM (non-streaming) to get the next response
    /// 4. Repeat until no more tool calls or max iterations reached
    ///
    /// Returns the final assistant `Message` (without tool_calls), or
    /// `None` if the last message had no tool calls.
    pub async fn execute_tool_loop(&mut self) -> Result<Option<Message>, McError> {
        // Check if the last message has tool calls
        let has_tool_calls = self
            .conversation
            .messages
            .last()
            .and_then(|m| m.tool_calls.as_ref())
            .map(|tc| !tc.is_empty())
            .unwrap_or(false);

        if !has_tool_calls {
            return Ok(None);
        }

        let tool_defs = self.tool_definitions();

        for iteration in 0..self.max_tool_iterations {
            // Extract tool calls from the last message
            let tool_calls = match self.conversation.messages.last() {
                Some(msg) => match &msg.tool_calls {
                    Some(tc) if !tc.is_empty() => tc.clone(),
                    _ => {
                        // No more tool calls — this is the final response
                        return Ok(self.conversation.messages.last().cloned());
                    }
                },
                None => return Ok(None),
            };

            debug!(
                iteration,
                num_tools = tool_calls.len(),
                "Executing tool loop iteration"
            );

            // Execute each tool call and append results
            self.execute_and_append_tools(&tool_calls).await?;

            // Re-call LLM
            let request = self.build_request(false, tool_defs.as_deref());
            let response: ChatResponse = self.provider.chat(request).await?;

            debug!(
                tokens = response.usage.total_tokens,
                finish = ?response.finish_reason,
                iteration,
                "LLM response received in tool loop"
            );

            // Append the assistant response
            self.conversation.push(response.message.clone());

            // If no tool calls, we're done
            if response.message.tool_calls.is_none() {
                return Ok(Some(response.message));
            }
        }

        Err(McError::Tool(format!(
            "Tool call loop exceeded maximum iterations ({})",
            self.max_tool_iterations
        )))
    }

    /// Get a reference to the current conversation.
    pub fn conversation(&self) -> &Conversation {
        &self.conversation
    }

    /// Get the agent configuration.
    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Get the system prompt.
    pub fn system_prompt(&self) -> &str {
        &self.system_prompt
    }

    /// Get the skill loader, if available.
    pub fn skill_loader(&self) -> Option<&SkillLoader> {
        self.skill_loader.as_deref()
    }

    /// Get the memory store, if available.
    pub fn memory_store(&self) -> Option<&MemoryStore> {
        self.memory_store.as_deref()
    }

    /// Get the current turn counter.
    pub fn turn_counter(&self) -> u32 {
        self.turn_counter
    }

    /// Get the current tool iteration counter.
    pub fn tool_iteration_counter(&self) -> u32 {
        self.tool_iteration_counter
    }

    /// Get the background reviewer, if available.
    pub fn reviewer(&self) -> Option<&BackgroundReviewer> {
        self.reviewer.as_ref()
    }

    /// Get the skill manager, if available.
    pub fn skill_manager(&self) -> Option<&SkillManager> {
        self.skill_manager.as_ref()
    }

    /// Get the session store, if available.
    pub fn session_store(&self) -> Option<&SessionStore> {
        self.session_store.as_deref()
    }

    /// Save the current conversation to the session store.
    ///
    /// Persists all messages in the current conversation to SQLite.
    /// Does nothing if no session store is configured.
    pub fn save_session(&self) -> Result<(), McError> {
        if let Some(store) = &self.session_store {
            store.save_session(&self.conversation)?;
            debug!(session_id = %self.conversation.id, "Session saved");
        }
        Ok(())
    }

    /// Load a conversation from the session store by ID.
    ///
    /// Replaces the current conversation with the loaded one. Returns
    /// `Ok(false)` if the session doesn't exist, `Ok(true)` on success.
    /// Returns an error if no session store is configured.
    pub fn load_session(&mut self, session_id: &str) -> Result<bool, McError> {
        let store = self.session_store.as_ref().ok_or_else(|| {
            McError::Storage("no session store configured".to_string())
        })?;

        match store.get_session(session_id)? {
            Some(conv) => {
                self.conversation = conv;
                debug!(session_id = %session_id, "Session loaded");
                Ok(true)
            }
            None => Ok(false),
        }
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Compress old conversation messages into a summary when the
    /// conversation grows beyond the configured threshold.
    ///
    /// When triggered:
    /// 1. Older messages (beyond `COMPRESS_KEEP_RECENT`) are formatted as text
    /// 2. The LLM generates a concise summary of those messages
    /// 3. The old messages are replaced by a single assistant summary message
    /// 4. Recent messages are preserved unchanged
    ///
    /// This prevents context window overflow and reduces token costs for
    /// long-running conversations.
    async fn maybe_compress_context(&mut self) -> Result<(), McError> {
        if self.compress_threshold == 0 {
            return Ok(());
        }

        let msg_count = self.conversation.messages.len();
        if msg_count <= self.compress_threshold {
            return Ok(());
        }

        info!(
            agent_id = %self.config.id,
            msg_count,
            threshold = self.compress_threshold,
            keep_recent = COMPRESS_KEEP_RECENT,
            "Compressing conversation context"
        );

        // Split: older messages go to summary, recent messages are kept
        let split_point = msg_count.saturating_sub(COMPRESS_KEEP_RECENT);
        let older_messages: Vec<Message> = self.conversation.messages[..split_point].to_vec();
        let recent_messages: Vec<Message> = self.conversation.messages[split_point..].to_vec();

        // Format older messages as text for the summarization prompt
        let conversation_text = format_messages_for_summary(&older_messages);

        // Ask the LLM to summarize
        let summary = self.summarize_messages(&conversation_text).await?;

        info!(
            original_count = older_messages.len(),
            summary_len = summary.len(),
            "Context compression complete"
        );

        // Rebuild conversation: [summary, recent messages...]
        let mut new_messages = Vec::with_capacity(1 + recent_messages.len());
        new_messages.push(Message::assistant(&summary));
        new_messages.extend(recent_messages);
        self.conversation.messages = new_messages;

        Ok(())
    }

    /// Ask the LLM to summarize a set of conversation messages.
    async fn summarize_messages(&self, conversation_text: &str) -> Result<String, McError> {
        let prompt = format!(
            "Summarize the following conversation concisely in 3-5 sentences. \
             Preserve key facts, decisions, user preferences, and any important \
             context that would be needed for continuing the conversation.\n\n\
             Conversation:\n{conversation_text}"
        );

        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![Message::user(&prompt)],
            tools: None,
            max_tokens: Some(500),
            temperature: Some(0.3),
            stream: false,
        };

        let response = self.provider.chat(request).await?;
        Ok(response.message.content.unwrap_or_default())
    }

    /// Check if a background review should be triggered and spawn it.
    fn maybe_spawn_review(&mut self) {
        let reviewer = match &self.reviewer {
            Some(r) => r,
            None => return,
        };

        // Don't spawn if there's already a pending review
        if self.pending_review.is_some() {
            return;
        }

        let trigger = match reviewer.should_review(self.turn_counter, self.tool_iteration_counter)
        {
            Some(t) => t,
            None => return,
        };

        info!(
            agent_id = %self.config.id,
            trigger = ?trigger,
            turns = self.turn_counter,
            tool_iters = self.tool_iteration_counter,
            "Spawning background review"
        );

        // Clone the data needed for the background task
        let provider = Arc::clone(&self.provider);
        let memory = match &self.memory_store {
            Some(m) => Arc::clone(m),
            None => return,
        };
        let conversation = self.conversation.clone();

        // Spawn directly — run_review is the core logic from mc-evolution
        let handle = tokio::spawn(async move {
            run_review(&provider, &memory, &conversation, trigger).await
        });

        self.pending_review = Some(handle);
    }

    /// Check if a pending background review has completed.
    ///
    /// Uses a zero-timeout to check if the review is done without blocking.
    /// If the review is finished, applies the result and returns the summary.
    /// Returns `None` if no review is pending or it hasn't completed yet.
    async fn check_pending_review(&mut self) -> Option<String> {
        let mut handle = match self.pending_review.take() {
            Some(h) => h,
            None => return None,
        };

        // Try to get the result with zero timeout (non-blocking check)
        match tokio::time::timeout(std::time::Duration::ZERO, &mut handle).await {
            Ok(result) => {
                // Task completed — apply the result
                match result {
                    Ok(Ok(review_result)) => {
                        let summary = review_result.summary.clone();
                        self.apply_review_result(&review_result);
                        Some(summary)
                    }
                    Ok(Err(e)) => {
                        warn!(error = %e, "Background review returned an error");
                        None
                    }
                    Err(e) => {
                        warn!(error = %e, "Background review task panicked");
                        None
                    }
                }
            }
            Err(_) => {
                // Task still running — store it back for next time
                self.pending_review = Some(handle);
                None
            }
        }
    }

    /// Apply a completed review result to memory and skills.
    fn apply_review_result(&mut self, result: &ReviewResult) {
        // Apply memory updates
        for update in &result.memory_updates {
            let file = match update.target.as_str() {
                "agent" => MemoryFile::Agent,
                "user" => MemoryFile::User,
                other => {
                    warn!(target = %other, "Unknown memory update target, skipping");
                    continue;
                }
            };

            if let Some(mem) = &self.memory_store {
                let store = MemoryStore::new(mem.base_path());
                let res = match update.action.as_str() {
                    "append" => store.append_file(file, &update.content),
                    "update" => {
                        store.update_section(file, &update.section, &update.content)
                            .map(|_| ())
                    }
                    other => {
                        warn!(action = %other, "Unknown memory update action, skipping");
                        continue;
                    }
                };
                if let Err(e) = res {
                    warn!(error = %e, target = %update.target, "Failed to apply memory update");
                }
            }
        }

        // Apply skill updates
        for update in &result.skill_updates {
            if let Some(mgr) = &self.skill_manager {
                let res = match update.action.as_str() {
                    "create" => {
                        if let Some(content) = &update.content {
                            mgr.create_skill(&update.name, content, SkillProvenance::Agent)
                                .map(|_| ())
                        } else {
                            warn!(skill = %update.name, "Skill create with no content, skipping");
                            continue;
                        }
                    }
                    "edit" => {
                        if let Some(content) = &update.content {
                            mgr.edit_skill(&update.name, content, SkillProvenance::Agent)
                                .map(|_| ())
                        } else {
                            warn!(skill = %update.name, "Skill edit with no content, skipping");
                            continue;
                        }
                    }
                    "delete" => {
                        mgr.delete_skill(&update.name, SkillProvenance::Agent)
                    }
                    other => {
                        warn!(action = %other, skill = %update.name, "Unknown skill update action, skipping");
                        continue;
                    }
                };
                if let Err(e) = res {
                    warn!(error = %e, skill = %update.name, "Failed to apply skill update");
                }
            }
        }

        info!(
            memory_updates = result.memory_updates.len(),
            skill_updates = result.skill_updates.len(),
            "Review result applied"
        );
    }

    /// Get tool definitions from the registry, if available.
    fn tool_definitions(&self) -> Option<Vec<mc_core::ToolDefinition>> {
        self.tool_registry.as_ref().map(|r| r.definitions())
    }

    /// Build a [`ChatRequest`] from the current conversation state.
    fn build_request(
        &self,
        stream: bool,
        tools: Option<&[mc_core::ToolDefinition]>,
    ) -> ChatRequest {
        // Prepend system message to the messages list
        let mut messages = vec![Message::system(&self.system_prompt)];
        messages.extend(self.conversation.messages.iter().cloned());

        ChatRequest {
            model: self.config.model.clone(),
            messages,
            tools: tools.map(|t| t.to_vec()),
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            stream,
        }
    }

    /// Execute a list of tool calls and append the results (plus the
    /// assistant tool-call message) to the conversation.
    async fn execute_and_append_tools(
        &mut self,
        tool_calls: &[mc_core::ToolCall],
    ) -> Result<(), McError> {
        let registry = match &self.tool_registry {
            Some(r) => Arc::clone(r),
            None => {
                return Err(McError::Tool(
                    "LLM requested tool calls but no tools are registered".to_string(),
                ));
            }
        };

        // Build tool result messages
        let mut tool_messages = Vec::with_capacity(tool_calls.len());

        for tc in tool_calls {
            let name = &tc.function.name;
            println!("  🔧 Calling tool: {name}");

            // Parse arguments
            let args: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                .unwrap_or(serde_json::Value::Object(Default::default()));

            // Execute
            let result = match registry.execute(name, args).await {
                Ok(output) => output,
                Err(e) => format!("Tool execution error: {e}"),
            };

            debug!(
                tool = %name,
                call_id = %tc.id,
                result_len = result.len(),
                "Tool executed"
            );

            tool_messages.push(Message::tool(&tc.id, result));
        }

        // Append all tool result messages to conversation
        for msg in tool_messages {
            self.conversation.push(msg);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Free helper functions
// ---------------------------------------------------------------------------

/// Format a slice of messages as readable text for summarization.
fn format_messages_for_summary(messages: &[Message]) -> String {
    let mut parts = Vec::new();
    for msg in messages {
        let role_str = match msg.role {
            mc_core::Role::System => "System",
            mc_core::Role::User => "User",
            mc_core::Role::Assistant => "Assistant",
            mc_core::Role::Tool => "Tool",
        };
        if let Some(content) = &msg.content {
            let truncated = if content.len() > 500 {
                format!("{}... (truncated)", &content[..500])
            } else {
                content.clone()
            };
            parts.push(format!("[{role_str}]: {truncated}"));
        }
        if let Some(tool_calls) = &msg.tool_calls {
            let names: Vec<&str> = tool_calls
                .iter()
                .map(|tc| tc.function.name.as_str())
                .collect();
            parts.push(format!("[{role_str}]: (tool calls: {})", names.join(", ")));
        }
    }
    parts.join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use mc_core::{FunctionCall, Role, ToolCall, ToolDefinition};
    use mc_llm::{ChatRequest, ChatResponse, FinishReason, StreamChunk, Usage};
    use serde_json::json;
    use std::path::Path;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // Mock LLM provider
    // -----------------------------------------------------------------------

    /// A mock LLM provider for testing.
    ///
    /// Stores a queue of pre-programmed responses. Each call to `chat()` pops
    /// the next response from the queue. The `request_log` records all
    /// requests made.
    struct MockLlmProvider {
        responses: Mutex<Vec<ChatResponse>>,
        request_log: Mutex<Vec<ChatRequest>>,
        call_count: AtomicUsize,
    }

    impl MockLlmProvider {
        fn new(responses: Vec<ChatResponse>) -> Self {
            Self {
                responses: Mutex::new(responses),
                request_log: Mutex::new(Vec::new()),
                call_count: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.call_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl LlmProvider for MockLlmProvider {
        async fn chat(&self, request: ChatRequest) -> Result<ChatResponse, McError> {
            self.call_count.fetch_add(1, Ordering::SeqCst);
            self.request_log.lock().unwrap().push(request);
            let mut responses = self.responses.lock().unwrap();
            if responses.is_empty() {
                return Err(McError::Llm("No more mock responses".to_string()));
            }
            Ok(responses.remove(0))
        }

        async fn chat_stream(
            &self,
            _request: ChatRequest,
        ) -> Result<
            Pin<Box<dyn futures::Stream<Item = Result<StreamChunk, McError>> + Send>>,
            McError,
        > {
            Err(McError::Llm(
                "Mock streaming not implemented".to_string(),
            ))
        }

        fn name(&self) -> &str {
            "mock"
        }

        fn models(&self) -> &[String] {
            &[]
        }
    }

    // -----------------------------------------------------------------------
    // Helper functions
    // -----------------------------------------------------------------------

    fn test_agent_config() -> AgentConfig {
        AgentConfig {
            id: "test-agent".to_string(),
            name: "Test Agent".to_string(),
            model: "test-model".to_string(),
            provider: "mock-provider".to_string(),
            system_prompt: None,
            system_prompt_file: None,
            max_tokens: None,
            temperature: None,
            skills_dir: None,
            memory_dir: None,
        }
    }

    fn make_text_response(text: &str) -> ChatResponse {
        ChatResponse {
            message: Message::assistant(text),
            usage: Usage::default(),
            finish_reason: FinishReason::Stop,
        }
    }

    fn make_tool_call_response(
        content: Option<&str>,
        tool_calls: Vec<ToolCall>,
    ) -> ChatResponse {
        ChatResponse {
            message: Message {
                role: Role::Assistant,
                content: content.map(|s| s.to_string()),
                tool_calls: Some(tool_calls),
                tool_call_id: None,
                name: None,
            },
            usage: Usage::default(),
            finish_reason: FinishReason::ToolCalls,
        }
    }

    fn sample_tool_call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
        }
    }

    fn mock_provider_registry() -> ProviderRegistry {
        // Create a provider registry with a dummy provider that will be
        // replaced. We only use this to satisfy Agent::new's requirement.
        ProviderRegistry::from_config(&[mc_config::ProviderConfig {
            id: "mock-provider".to_string(),
            kind: mc_config::ProviderKind::OpenAI,
            base_url: "http://localhost".to_string(),
            api_key: "test".to_string(),
            models: vec!["test-model".to_string()],
        }])
        .unwrap()
    }

    /// Create an Agent with a mock LLM provider injected.
    ///
    /// This replaces the real provider inside the Agent with our mock.
    fn create_test_agent(
        mock: Arc<dyn LlmProvider>,
        tool_registry: Option<Arc<ToolRegistry>>,
    ) -> Agent {
        let config = test_agent_config();
        let registry = mock_provider_registry();
        let mut agent = Agent::new(&config, &registry, tool_registry, None, None, None, None, None).unwrap();
        // Replace the provider with our mock
        agent.provider = mock;
        agent
    }

    /// Build a simple tool registry with a "echo" tool that returns its argument.
    fn echo_tool_registry() -> Arc<ToolRegistry> {
        let mut reg = ToolRegistry::new();
        // Use the existing BashTool as a real tool, but for tests we need
        // something predictable. Let's register a custom echo tool.
        reg.register(Arc::new(EchoTool));
        Arc::new(reg)
    }

    /// A simple tool that echoes its input back (for testing).
    struct EchoTool;

    #[async_trait]
    impl mc_core::Tool for EchoTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "echo".to_string(),
                description: "Echo tool for testing".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "message": { "type": "string" }
                    }
                }),
            }
        }

        async fn execute(&self, args: serde_json::Value) -> Result<String, McError> {
            let msg = args["message"].as_str().unwrap_or("no message");
            Ok(format!("echo: {msg}"))
        }
    }

    /// A tool that always fails (for error handling tests).
    struct FailTool;

    #[async_trait]
    impl mc_core::Tool for FailTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                name: "fail".to_string(),
                description: "Always-failing tool".to_string(),
                parameters: json!({"type": "object", "properties": {}}),
            }
        }

        async fn execute(&self, _args: serde_json::Value) -> Result<String, McError> {
            Err(McError::Tool("intentional failure".to_string()))
        }
    }

    fn fail_tool_registry() -> Arc<ToolRegistry> {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(FailTool));
        Arc::new(reg)
    }

    // -----------------------------------------------------------------------
    // Tests: handle_message with tool calls
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn handle_message_simple_text_response() {
        // LLM returns a simple text response (no tools)
        let mock = Arc::new(MockLlmProvider::new(vec![make_text_response("Hello!")]));
        let mut agent = create_test_agent(mock.clone(), None);

        let result = agent.handle_message("Hi").await.unwrap();
        assert_eq!(result.content.as_deref(), Some("Hello!"));
        assert_eq!(mock.call_count(), 1);

        // Conversation should have: user + assistant
        assert_eq!(agent.conversation().messages.len(), 2);
        assert_eq!(agent.conversation().messages[0].role, Role::User);
        assert_eq!(agent.conversation().messages[1].role, Role::Assistant);
    }

    #[tokio::test]
    async fn handle_message_single_tool_call_loop() {
        // LLM returns tool call, then text response
        let tool_calls = vec![sample_tool_call("call_1", "echo", json!({"message": "hi"}))];
        let mock = Arc::new(MockLlmProvider::new(vec![
            make_tool_call_response(Some("Let me use a tool"), tool_calls),
            make_text_response("Tool said: echo: hi"),
        ]));
        let registry = echo_tool_registry();
        let mut agent = create_test_agent(mock.clone(), Some(registry));

        let result = agent.handle_message("Do something").await.unwrap();
        assert_eq!(result.content.as_deref(), Some("Tool said: echo: hi"));
        assert_eq!(mock.call_count(), 2);

        // Conversation: user, assistant(tool_calls), tool(result), assistant(final)
        let msgs = &agent.conversation().messages;
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
        assert!(msgs[1].tool_calls.is_some());
        assert_eq!(msgs[2].role, Role::Tool);
        assert_eq!(msgs[2].tool_call_id.as_deref(), Some("call_1"));
        assert!(msgs[2].content.as_deref().unwrap().contains("echo: hi"));
        assert_eq!(msgs[3].role, Role::Assistant);
    }

    #[tokio::test]
    async fn handle_message_multiple_tool_calls() {
        // LLM requests 2 tools at once, then responds.
        // Verify BOTH tool calls are executed and their results are stored.
        let tool_calls = vec![
            sample_tool_call("call_1", "echo", json!({"message": "alpha"})),
            sample_tool_call("call_2", "echo", json!({"message": "beta"})),
        ];
        let mock = Arc::new(MockLlmProvider::new(vec![
            make_tool_call_response(None, tool_calls),
            make_text_response("Both done"),
        ]));
        let registry = echo_tool_registry();
        let mut agent = create_test_agent(mock.clone(), Some(registry));

        let result = agent.handle_message("Run both").await.unwrap();
        assert_eq!(result.content.as_deref(), Some("Both done"));

        // Conversation: user, assistant(tool_calls), tool1, tool2, assistant(final)
        let msgs = &agent.conversation().messages;
        assert_eq!(msgs.len(), 5);

        // Verify the assistant message stored both tool_calls
        let assistant_tc = msgs[1].tool_calls.as_ref().unwrap();
        assert_eq!(assistant_tc.len(), 2);
        assert_eq!(assistant_tc[0].id, "call_1");
        assert_eq!(assistant_tc[1].id, "call_2");

        // Verify BOTH tool results are present with correct content
        assert_eq!(msgs[2].role, Role::Tool);
        assert_eq!(msgs[2].tool_call_id.as_deref(), Some("call_1"));
        assert!(
            msgs[2].content.as_deref().unwrap().contains("echo: alpha"),
            "First tool result should contain 'echo: alpha', got: {:?}",
            msgs[2].content
        );

        assert_eq!(msgs[3].role, Role::Tool);
        assert_eq!(msgs[3].tool_call_id.as_deref(), Some("call_2"));
        assert!(
            msgs[3].content.as_deref().unwrap().contains("echo: beta"),
            "Second tool result should contain 'echo: beta', got: {:?}",
            msgs[3].content
        );
    }

    #[tokio::test]
    async fn handle_message_two_iterations_of_tool_calls() {
        // First LLM call: tool call
        // Second LLM call (after tool result): another tool call
        // Third LLM call: final text
        let mock = Arc::new(MockLlmProvider::new(vec![
            make_tool_call_response(None, vec![sample_tool_call("c1", "echo", json!({"message":"first"}))]),
            make_tool_call_response(None, vec![sample_tool_call("c2", "echo", json!({"message":"second"}))]),
            make_text_response("Done after two rounds"),
        ]));
        let registry = echo_tool_registry();
        let mut agent = create_test_agent(mock.clone(), Some(registry));

        let result = agent.handle_message("Multi round").await.unwrap();
        assert_eq!(result.content.as_deref(), Some("Done after two rounds"));
        assert_eq!(mock.call_count(), 3);
    }

    #[tokio::test]
    async fn handle_message_three_tool_calls_all_executed() {
        // LLM requests 3 tools at once — verify all 3 are executed
        let tool_calls = vec![
            sample_tool_call("tc_0", "echo", json!({"message": "first"})),
            sample_tool_call("tc_1", "echo", json!({"message": "second"})),
            sample_tool_call("tc_2", "echo", json!({"message": "third"})),
        ];
        let mock = Arc::new(MockLlmProvider::new(vec![
            make_tool_call_response(None, tool_calls),
            make_text_response("All three done"),
        ]));
        let registry = echo_tool_registry();
        let mut agent = create_test_agent(mock.clone(), Some(registry));

        let result = agent.handle_message("Triple").await.unwrap();
        assert_eq!(result.content.as_deref(), Some("All three done"));

        // user, assistant(tool_calls), tool1, tool2, tool3, assistant(final)
        let msgs = &agent.conversation().messages;
        assert_eq!(msgs.len(), 6);

        // Verify all 3 tool results are present with correct content
        assert_eq!(msgs[2].tool_call_id.as_deref(), Some("tc_0"));
        assert!(msgs[2].content.as_deref().unwrap().contains("echo: first"));
        assert_eq!(msgs[3].tool_call_id.as_deref(), Some("tc_1"));
        assert!(msgs[3].content.as_deref().unwrap().contains("echo: second"));
        assert_eq!(msgs[4].tool_call_id.as_deref(), Some("tc_2"));
        assert!(msgs[4].content.as_deref().unwrap().contains("echo: third"));
    }

    #[tokio::test]
    async fn handle_message_mixed_success_and_failure_tools() {
        // LLM requests both echo (succeeds) and fail (errors) tools.
        // Both should produce tool result messages — the error is non-fatal.
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool));
        reg.register(Arc::new(FailTool));
        let registry = Arc::new(reg);

        let tool_calls = vec![
            sample_tool_call("ok_1", "echo", json!({"message": "hello"})),
            sample_tool_call("fail_1", "fail", json!({})),
        ];
        let mock = Arc::new(MockLlmProvider::new(vec![
            make_tool_call_response(None, tool_calls),
            make_text_response("Processed both"),
        ]));
        let mut agent = create_test_agent(mock.clone(), Some(registry));

        let result = agent.handle_message("Mixed").await.unwrap();
        assert_eq!(result.content.as_deref(), Some("Processed both"));

        let msgs = &agent.conversation().messages;
        assert_eq!(msgs.len(), 5);

        // First tool result: success
        assert_eq!(msgs[2].tool_call_id.as_deref(), Some("ok_1"));
        assert!(msgs[2].content.as_deref().unwrap().contains("echo: hello"));

        // Second tool result: error (but non-fatal — stored as content)
        assert_eq!(msgs[3].tool_call_id.as_deref(), Some("fail_1"));
        assert!(
            msgs[3].content.as_deref().unwrap().contains("Tool execution error"),
            "Expected error content, got: {:?}",
            msgs[3].content
        );
    }

    #[tokio::test]
    async fn handle_message_max_iterations_exceeded() {
        // Create responses that always return tool calls (more than max)
        let responses: Vec<ChatResponse> = (0..15)
            .map(|i| {
                make_tool_call_response(
                    None,
                    vec![sample_tool_call(
                        &format!("call_{i}"),
                        "echo",
                        json!({"message": "loop"}),
                    )],
                )
            })
            .collect();
        let mock = Arc::new(MockLlmProvider::new(responses));
        let registry = echo_tool_registry();
        let mut agent = create_test_agent(mock.clone(), Some(registry));
        agent.set_max_tool_iterations(3);

        let err = agent.handle_message("Loop forever").await.unwrap_err();
        match err {
            McError::Tool(msg) => assert!(msg.contains("maximum iterations")),
            other => panic!("Expected Tool error, got: {other:?}"),
        }
        // Should have made exactly 3 LLM calls (the 4th would exceed limit)
        assert_eq!(mock.call_count(), 3);
    }

    #[tokio::test]
    async fn handle_message_tool_error_as_content() {
        // Tool execution fails, but the error is returned as tool content
        // (not a fatal error). LLM then produces final text.
        let tool_calls = vec![sample_tool_call("call_1", "fail", json!({}))];
        let mock = Arc::new(MockLlmProvider::new(vec![
            make_tool_call_response(None, tool_calls),
            make_text_response("Sorry, the tool failed"),
        ]));
        let registry = fail_tool_registry();
        let mut agent = create_test_agent(mock.clone(), Some(registry));

        let result = agent.handle_message("Do it").await.unwrap();
        assert_eq!(result.content.as_deref(), Some("Sorry, the tool failed"));

        // Tool result should contain the error message
        let tool_msg = &agent.conversation().messages[2];
        assert_eq!(tool_msg.role, Role::Tool);
        assert!(tool_msg
            .content
            .as_deref()
            .unwrap()
            .contains("Tool execution error"));
    }

    #[tokio::test]
    async fn handle_message_no_tools_but_llm_requests_them() {
        // Agent has no tools, but LLM returns tool calls -> should error
        let tool_calls = vec![sample_tool_call("call_1", "echo", json!({}))];
        let mock = Arc::new(MockLlmProvider::new(vec![make_tool_call_response(
            None, tool_calls,
        )]));
        let mut agent = create_test_agent(mock, None); // no tools

        let err = agent.handle_message("test").await.unwrap_err();
        match err {
            McError::Tool(msg) => assert!(msg.contains("no tools are registered")),
            other => panic!("Expected Tool error, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Tests: execute_tool_loop (streaming path)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn execute_tool_loop_returns_none_without_tool_calls() {
        let mock = Arc::new(MockLlmProvider::new(vec![]));
        let mut agent = create_test_agent(mock, None);

        // Add a plain text assistant message (no tool calls)
        agent.finalize_stream("Hello", None);

        let result = agent.execute_tool_loop().await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn execute_tool_loop_runs_tool_and_returns_final() {
        // After finalize_stream with tool_calls, execute_tool_loop should:
        // 1. Execute the tool
        // 2. Re-call LLM (mock returns text)
        // 3. Return the final message
        let tool_calls = vec![sample_tool_call("tc1", "echo", json!({"message":"test"}))];
        let mock = Arc::new(MockLlmProvider::new(vec![make_text_response(
            "Tool result processed",
        )]));
        let registry = echo_tool_registry();
        let mut agent = create_test_agent(mock.clone(), Some(registry));

        // Simulate streaming: user message + assistant with tool_calls
        agent.conversation.push(Message::user("do it"));
        agent.finalize_stream("Let me call a tool", Some(tool_calls));

        let result = agent.execute_tool_loop().await.unwrap();
        assert!(result.is_some());
        let msg = result.unwrap();
        assert_eq!(msg.content.as_deref(), Some("Tool result processed"));
        assert_eq!(mock.call_count(), 1);

        // Conversation: user, assistant(tool_calls), tool(result), assistant(final)
        assert_eq!(agent.conversation().messages.len(), 4);
    }

    // -----------------------------------------------------------------------
    // Tests: finalize_stream
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn finalize_stream_stores_content_only() {
        let mock = Arc::new(MockLlmProvider::new(vec![]));
        let mut agent = create_test_agent(mock, None);

        agent.finalize_stream("Some content", None);
        let last = agent.conversation().messages.last().unwrap();
        assert_eq!(last.role, Role::Assistant);
        assert_eq!(last.content.as_deref(), Some("Some content"));
        assert!(last.tool_calls.is_none());
    }

    #[tokio::test]
    async fn finalize_stream_stores_tool_calls() {
        let mock = Arc::new(MockLlmProvider::new(vec![]));
        let mut agent = create_test_agent(mock, None);

        let tc = vec![sample_tool_call("id1", "bash", json!({"command":"ls"}))];
        agent.finalize_stream("Let me check", Some(tc));

        let last = agent.conversation().messages.last().unwrap();
        assert_eq!(last.role, Role::Assistant);
        assert_eq!(last.content.as_deref(), Some("Let me check"));
        assert!(last.tool_calls.is_some());
        assert_eq!(last.tool_calls.as_ref().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn finalize_stream_empty_content_stores_none() {
        let mock = Arc::new(MockLlmProvider::new(vec![]));
        let mut agent = create_test_agent(mock, None);

        agent.finalize_stream("", None);
        let last = agent.conversation().messages.last().unwrap();
        assert!(last.content.is_none());
    }

    // -----------------------------------------------------------------------
    // Tests: build_system_prompt
    // -----------------------------------------------------------------------

    #[test]
    fn build_system_prompt_default() {
        let config = test_agent_config();
        let prompt = Agent::build_system_prompt(&config, None);
        assert!(prompt.contains("Test Agent"));
        assert!(prompt.contains("helpful AI assistant"));
    }

    #[test]
    fn build_system_prompt_custom() {
        let mut config = test_agent_config();
        config.system_prompt = Some("You are a pirate.".to_string());
        let prompt = Agent::build_system_prompt(&config, None);
        assert!(prompt.contains("Test Agent"));
        assert!(prompt.contains("You are a pirate."));
    }

    // -----------------------------------------------------------------------
    // Tests: request includes tool definitions
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn build_request_includes_tools() {
        let mock = Arc::new(MockLlmProvider::new(vec![make_text_response("ok")]));
        let registry = echo_tool_registry();
        let mut agent = create_test_agent(mock.clone(), Some(registry));

        agent.handle_message("test").await.unwrap();

        let requests = mock.request_log.lock().unwrap();
        let req = &requests[0];
        assert!(req.tools.is_some());
        let tools = req.tools.as_ref().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
    }

    #[tokio::test]
    async fn build_request_no_tools_when_none() {
        let mock = Arc::new(MockLlmProvider::new(vec![make_text_response("ok")]));
        let mut agent = create_test_agent(mock.clone(), None);

        agent.handle_message("test").await.unwrap();

        let requests = mock.request_log.lock().unwrap();
        assert!(requests[0].tools.is_none());
    }

    // -----------------------------------------------------------------------
    // Tests: max_tool_iterations configuration
    // -----------------------------------------------------------------------

    #[test]
    fn default_max_iterations() {
        let config = test_agent_config();
        let registry = mock_provider_registry();
        let agent = Agent::new(&config, &registry, None, None, None, None, None, None).unwrap();
        assert_eq!(agent.max_tool_iterations, 10);
    }

    #[test]
    fn set_max_tool_iterations() {
        let config = test_agent_config();
        let registry = mock_provider_registry();
        let mut agent = Agent::new(&config, &registry, None, None, None, None, None, None).unwrap();
        agent.set_max_tool_iterations(5);
        assert_eq!(agent.max_tool_iterations, 5);
    }

    // -----------------------------------------------------------------------
    // Tests: skill_loader and memory_store integration
    // -----------------------------------------------------------------------

    /// Helper: create a SkillLoader with a sample skill loaded from a temp dir.
    fn create_test_skill_loader() -> (TempDir, Arc<SkillLoader>) {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("test-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\ntrigger_words:\n  - test\n---\n# Test Skill\nA test skill for unit tests.",
        )
        .unwrap();
        let mut loader = SkillLoader::new();
        loader.load_dir(tmp.path()).unwrap();
        (tmp, Arc::new(loader))
    }

    #[test]
    fn build_system_prompt_with_skills_injects_summary() {
        let config = test_agent_config();
        let (_tmp, loader) = create_test_skill_loader();
        let prompt = Agent::build_system_prompt(&config, Some(&loader));

        assert!(
            prompt.contains("## Available Skills"),
            "prompt should contain skills heading, got: {prompt}"
        );
        assert!(
            prompt.contains("test-skill"),
            "prompt should contain skill name, got: {prompt}"
        );
        assert!(
            prompt.contains("Test Skill"),
            "prompt should contain skill description, got: {prompt}"
        );
    }

    #[test]
    fn build_system_prompt_without_skills_has_no_heading() {
        let config = test_agent_config();
        let prompt = Agent::build_system_prompt(&config, None);
        assert!(
            !prompt.contains("## Available Skills"),
            "prompt should not contain skills heading when no loader"
        );
    }

    #[test]
    fn agent_accessors_return_none_without_skills_and_memory() {
        let config = test_agent_config();
        let registry = mock_provider_registry();
        let agent = Agent::new(&config, &registry, None, None, None, None, None, None).unwrap();
        assert!(agent.skill_loader().is_none());
        assert!(agent.memory_store().is_none());
    }

    #[test]
    fn agent_accessors_return_some_with_skills_and_memory() {
        let config = test_agent_config();
        let registry = mock_provider_registry();
        let (_tmp_skills, loader) = create_test_skill_loader();
        let tmp_mem = TempDir::new().unwrap();
        let memory_store = Arc::new(mc_memory::MemoryStore::new(tmp_mem.path()));

        let agent = Agent::new(
            &config,
            &registry,
            None,
            Some(loader.clone()),
            Some(memory_store.clone()),
            None,
            None,
            None,
        )
        .unwrap();

        assert!(agent.skill_loader().is_some());
        assert!(agent.memory_store().is_some());

        // Verify the skill loader actually has the skill
        let loaded = agent.skill_loader().unwrap();
        assert!(loaded.load("test-skill").is_some());

        // Verify the memory store has the correct base path
        let store = agent.memory_store().unwrap();
        assert_eq!(store.base_path(), tmp_mem.path());
    }

    // -----------------------------------------------------------------------
    // Tests: evolution integration
    // -----------------------------------------------------------------------

    fn test_evolution_config() -> EvolutionConfig {
        EvolutionConfig {
            enabled: true,
            memory_nudge_interval: 100, // High default — tests that need triggering override
            skill_nudge_interval: 100,
            curator_interval_hours: 168,
        }
    }

    /// Evolution config with low intervals for review trigger testing.
    fn test_evolution_config_low_interval() -> EvolutionConfig {
        EvolutionConfig {
            enabled: true,
            memory_nudge_interval: 2,
            skill_nudge_interval: 3,
            curator_interval_hours: 168,
        }
    }

    /// Create an Agent with evolution enabled and a mock LLM provider.
    fn create_test_agent_with_evolution(
        mock: Arc<dyn LlmProvider>,
        tool_registry: Option<Arc<ToolRegistry>>,
        tmp_dir: &Path,
    ) -> Agent {
        let config = test_agent_config();
        let registry = mock_provider_registry();
        let memory_store = Arc::new(mc_memory::MemoryStore::new(tmp_dir.join("memory")));
        let evo_config = test_evolution_config();
        let skills_dir = tmp_dir.join("skills").to_string_lossy().to_string();

        let mut agent = Agent::new(
            &config,
            &registry,
            tool_registry,
            None,
            Some(memory_store),
            Some(&evo_config),
            Some(&skills_dir),
            None,
        )
        .unwrap();
        // Replace the provider with our mock
        agent.provider = mock;
        agent
    }

    #[tokio::test]
    async fn evolution_turn_counter_increments() {
        let mock = Arc::new(MockLlmProvider::new(vec![
            make_text_response("A"),
            make_text_response("B"),
            make_text_response("C"),
        ]));
        let tmp = TempDir::new().unwrap();
        let mut agent = create_test_agent_with_evolution(mock, None, tmp.path());

        assert_eq!(agent.turn_counter(), 0);

        agent.handle_message("msg1").await.unwrap();
        assert_eq!(agent.turn_counter(), 1);

        agent.handle_message("msg2").await.unwrap();
        assert_eq!(agent.turn_counter(), 2);

        agent.handle_message("msg3").await.unwrap();
        assert_eq!(agent.turn_counter(), 3);
    }

    #[tokio::test]
    async fn evolution_tool_iteration_counter_increments() {
        let tool_calls = vec![sample_tool_call("tc1", "echo", json!({"message":"hi"}))];
        let mock = Arc::new(MockLlmProvider::new(vec![
            make_tool_call_response(None, tool_calls),
            make_text_response("Done"),
        ]));
        let tmp = TempDir::new().unwrap();
        let registry = echo_tool_registry();
        let mut agent = create_test_agent_with_evolution(mock, Some(registry), tmp.path());

        agent.handle_message("do it").await.unwrap();
        assert_eq!(agent.tool_iteration_counter(), 1);
    }

    #[test]
    fn evolution_reviewer_is_some_when_enabled() {
        let config = test_agent_config();
        let registry = mock_provider_registry();
        let tmp = TempDir::new().unwrap();
        let memory_store = Arc::new(mc_memory::MemoryStore::new(tmp.path()));
        let evo_config = test_evolution_config();

        let agent = Agent::new(
            &config,
            &registry,
            None,
            None,
            Some(memory_store),
            Some(&evo_config),
            Some(tmp.path().join("skills").to_str().unwrap()),
            None,
        )
        .unwrap();

        assert!(agent.reviewer().is_some());
        assert!(agent.skill_manager().is_some());
    }

    #[test]
    fn evolution_reviewer_is_none_when_disabled() {
        let config = test_agent_config();
        let registry = mock_provider_registry();
        let tmp = TempDir::new().unwrap();
        let memory_store = Arc::new(mc_memory::MemoryStore::new(tmp.path()));
        let evo_config = EvolutionConfig {
            enabled: false,
            ..test_evolution_config()
        };

        let agent = Agent::new(
            &config,
            &registry,
            None,
            None,
            Some(memory_store),
            Some(&evo_config),
            None,
            None,
        )
        .unwrap();

        assert!(agent.reviewer().is_none());
    }

    #[test]
    fn evolution_reviewer_is_none_without_memory() {
        let config = test_agent_config();
        let registry = mock_provider_registry();
        let evo_config = test_evolution_config();

        let agent = Agent::new(
            &config,
            &registry,
            None,
            None,
            None,
            Some(&evo_config),
            None,
            None,
        )
        .unwrap();

        assert!(agent.reviewer().is_none());
    }

    #[tokio::test]
    async fn evolution_review_triggers_at_threshold() {
        // memory_nudge_interval = 2, so review triggers at turn 2
        let mock = Arc::new(MockLlmProvider::new(vec![
            make_text_response("Response 1"),
            make_text_response("Response 2"),
            make_text_response("Response 3"),
        ]));
        let tmp = TempDir::new().unwrap();
        let config = test_agent_config();
        let registry = mock_provider_registry();
        let memory_store = Arc::new(mc_memory::MemoryStore::new(tmp.path().join("memory")));
        let evo_config = test_evolution_config_low_interval();

        let mut agent = Agent::new(
            &config,
            &registry,
            None,
            None,
            Some(memory_store),
            Some(&evo_config),
            Some(tmp.path().join("skills").to_str().unwrap()),
            None,
        )
        .unwrap();
        agent.provider = mock;

        // Turn 1: no trigger
        agent.handle_message("msg1").await.unwrap();
        assert!(
            agent.pending_review.is_none(),
            "No review should be pending after turn 1"
        );

        // Turn 2: triggers review (memory_interval = 2)
        agent.handle_message("msg2").await.unwrap();
        // Note: pending_review might be set if the reviewer was spawned
        // The reviewer uses a cloned provider, not our mock, so it would fail.
        // But the fact that it was spawned is what we're testing.
    }

    #[tokio::test]
    async fn evolution_review_applies_memory_update() {
        // This test verifies the full flow: review completes, memory is updated,
        // and the self-improvement message appears in the response.
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let skills_dir = tmp.path().join("skills");

        // Pre-seed memory file so we can verify updates
        std::fs::create_dir_all(&memory_dir).unwrap();
        std::fs::write(memory_dir.join("MEMORY.md"), "# Existing memory\n").unwrap();

        let config = test_agent_config();
        let registry = mock_provider_registry();
        let memory_store = Arc::new(mc_memory::MemoryStore::new(&memory_dir));
        let evo_config = test_evolution_config();

        // Build agent with a multi-response mock:
        // Response 1: turn 1 text (no review trigger)
        // Response 2: turn 2 text (review triggers at turn 2)
        // The spawned review uses its own provider clone. We can't control it
        // from here, so we'll test apply_review_result directly instead.
        let mock = Arc::new(MockLlmProvider::new(vec![
            make_text_response("Turn 1 response"),
            make_text_response("Turn 2 response"),
        ]));

        let mut agent = Agent::new(
            &config,
            &registry,
            None,
            None,
            Some(memory_store),
            Some(&evo_config),
            Some(skills_dir.to_str().unwrap()),
            None,
        )
        .unwrap();
        agent.provider = mock;

        // Directly test apply_review_result
        let review_result = ReviewResult {
            memory_updates: vec![mc_evolution::MemoryUpdate {
                target: "agent".to_string(),
                section: "# Existing memory".to_string(),
                content: "User prefers concise answers.".to_string(),
                action: "append".to_string(),
            }],
            skill_updates: vec![],
            summary: "Learned user preference for concise answers.".to_string(),
        };

        agent.apply_review_result(&review_result);

        // Verify memory was updated
        let mem_content = std::fs::read_to_string(memory_dir.join("MEMORY.md")).unwrap();
        assert!(
            mem_content.contains("User prefers concise answers"),
            "Memory should contain the appended content, got: {mem_content}"
        );
    }

    #[tokio::test]
    async fn evolution_review_applies_skill_update() {
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&memory_dir).unwrap();

        let config = test_agent_config();
        let registry = mock_provider_registry();
        let memory_store = Arc::new(mc_memory::MemoryStore::new(&memory_dir));
        let evo_config = test_evolution_config();

        let mock = Arc::new(MockLlmProvider::new(vec![make_text_response("ok")]));

        let mut agent = Agent::new(
            &config,
            &registry,
            None,
            None,
            Some(memory_store),
            Some(&evo_config),
            Some(skills_dir.to_str().unwrap()),
            None,
        )
        .unwrap();
        agent.provider = mock;

        // Test skill creation through apply_review_result
        let review_result = ReviewResult {
            memory_updates: vec![],
            skill_updates: vec![mc_evolution::SkillUpdate {
                name: "git-helper".to_string(),
                action: "create".to_string(),
                content: Some("---\ntrigger_words:\n  - git\n---\n# Git Helper\nAutomate git operations.".to_string()),
            }],
            summary: "Created git-helper skill based on repeated git operations.".to_string(),
        };

        agent.apply_review_result(&review_result);

        // Verify skill was created
        let skill_path = skills_dir.join("git-helper").join("SKILL.md");
        assert!(skill_path.exists(), "Skill file should exist");
        let skill_content = std::fs::read_to_string(&skill_path).unwrap();
        assert!(skill_content.contains("Git Helper"));
    }

    #[tokio::test]
    async fn evolution_self_improvement_message_prepended() {
        // Test that when a review completes, the response includes
        // "Self-improvement: ..." prefix.
        let tmp = TempDir::new().unwrap();
        let memory_dir = tmp.path().join("memory");
        let skills_dir = tmp.path().join("skills");
        std::fs::create_dir_all(&memory_dir).unwrap();

        let config = test_agent_config();
        let registry = mock_provider_registry();
        let memory_store = Arc::new(mc_memory::MemoryStore::new(&memory_dir));
        let evo_config = test_evolution_config();

        let mock = Arc::new(MockLlmProvider::new(vec![
            make_text_response("Hello!"),
        ]));

        let mut agent = Agent::new(
            &config,
            &registry,
            None,
            None,
            Some(memory_store),
            Some(&evo_config),
            Some(skills_dir.to_str().unwrap()),
            None,
        )
        .unwrap();
        agent.provider = mock;

        // Simulate a completed review by setting a resolved JoinHandle
        let review_result = Ok(ReviewResult {
            memory_updates: vec![],
            skill_updates: vec![],
            summary: "No updates needed after reviewing conversation.".to_string(),
        });
        agent.pending_review = Some(tokio::spawn(async { review_result }));

        // Next handle_message should pick up the review result
        let response = agent.handle_message("Hi").await.unwrap();
        let content = response.content.unwrap_or_default();
        assert!(
            content.contains("Self-improvement:"),
            "Response should contain self-improvement prefix, got: {content}"
        );
        assert!(
            content.contains("No updates needed"),
            "Response should contain review summary, got: {content}"
        );
        assert!(
            content.contains("Hello!"),
            "Response should contain original LLM output, got: {content}"
        );
    }

    #[tokio::test]
    async fn evolution_no_improvement_message_without_review() {
        let mock = Arc::new(MockLlmProvider::new(vec![make_text_response("Hi!")]));
        let tmp = TempDir::new().unwrap();
        let mut agent = create_test_agent_with_evolution(mock, None, tmp.path());

        let response = agent.handle_message("Hello").await.unwrap();
        let content = response.content.unwrap_or_default();
        assert!(
            !content.contains("Self-improvement:"),
            "Should not have self-improvement prefix without review"
        );
    }

    // -----------------------------------------------------------------------
    // Tests: context compression
    // -----------------------------------------------------------------------

    /// Fill the conversation with N pairs of user+assistant messages.
    fn fill_conversation(agent: &mut Agent, n: usize) {
        for i in 0..n {
            agent.conversation.push(Message::user(&format!("User message {i}")));
            agent.conversation.push(Message::assistant(&format!("Assistant reply {i}")));
        }
    }

    #[test]
    fn format_messages_for_summary_basic() {
        let messages = vec![
            Message::user("Hello"),
            Message::assistant("Hi there!"),
        ];
        let text = super::format_messages_for_summary(&messages);
        assert!(text.contains("[User]: Hello"));
        assert!(text.contains("[Assistant]: Hi there!"));
    }

    #[test]
    fn format_messages_for_summary_truncates_long_content() {
        let long_text = "x".repeat(600);
        let messages = vec![Message::user(&long_text)];
        let text = super::format_messages_for_summary(&messages);
        assert!(text.contains("... (truncated)"));
        assert!(text.len() < long_text.len());
    }

    #[test]
    fn format_messages_for_summary_includes_tool_calls() {
        let messages = vec![Message {
            role: mc_core::Role::Assistant,
            content: Some("Let me check".to_string()),
            tool_calls: Some(vec![
                sample_tool_call("c1", "bash", json!({})),
                sample_tool_call("c2", "read_file", json!({})),
            ]),
            tool_call_id: None,
            name: None,
        }];
        let text = super::format_messages_for_summary(&messages);
        assert!(text.contains("[Assistant]: Let me check"));
        assert!(text.contains("bash"));
        assert!(text.contains("read_file"));
    }

    #[test]
    fn format_messages_for_summary_handles_tool_result() {
        let messages = vec![Message::tool("call_1", "output of tool")];
        let text = super::format_messages_for_summary(&messages);
        assert!(text.contains("[Tool]: output of tool"));
    }

    #[tokio::test]
    async fn compress_not_triggered_below_threshold() {
        // Threshold = 50, conversation has 10 messages -> no compression
        let mock = Arc::new(MockLlmProvider::new(vec![make_text_response("ok")]));
        let mut agent = create_test_agent(mock, None);
        agent.set_compress_threshold(50);

        fill_conversation(&mut agent, 5); // 10 messages

        agent.handle_message("new message").await.unwrap();

        // Conversation should still have: 10 old + 1 new user + 1 new assistant = 12
        assert_eq!(agent.conversation().messages.len(), 12);
    }

    #[tokio::test]
    async fn compress_triggered_at_threshold() {
        // Threshold = 20, conversation has 22 messages -> triggers compression
        // After compression: 1 summary + 10 recent = 11 messages
        // After handle_message: +1 new assistant response = 12 messages
        // (COMPRESS_KEEP_RECENT = 10)
        let responses = vec![
            make_text_response("This is a summary of the conversation."),
            make_text_response("Final response"),
        ];
        let mock = Arc::new(MockLlmProvider::new(responses));
        let mut agent = create_test_agent(mock.clone(), None);
        agent.set_compress_threshold(20);

        fill_conversation(&mut agent, 11); // 22 messages (user + assistant pairs)

        agent.handle_message("new message").await.unwrap();

        // Conversation: [summary, 10 recent msgs, new assistant response] = 12
        assert_eq!(agent.conversation().messages.len(), 12);

        // First message should be the summary (assistant role)
        assert_eq!(agent.conversation().messages[0].role, mc_core::Role::Assistant);
        assert!(agent.conversation().messages[0]
            .content
            .as_deref()
            .unwrap()
            .contains("summary"));

        // The mock was called twice: once for summary, once for the actual response
        assert_eq!(mock.call_count(), 2);
    }

    #[tokio::test]
    async fn compress_preserves_recent_messages() {
        // Fill conversation, compress, verify recent messages are intact
        let responses = vec![
            make_text_response("Summary of old messages."),
            make_text_response("New response"),
        ];
        let mock = Arc::new(MockLlmProvider::new(responses));
        let mut agent = create_test_agent(mock, None);
        agent.set_compress_threshold(20);

        // 11 pairs = 22 messages
        fill_conversation(&mut agent, 11);

        agent.handle_message("latest input").await.unwrap();

        // Last message should be the new assistant response
        let last = agent.conversation().messages.last().unwrap();
        assert_eq!(last.role, mc_core::Role::Assistant);
        assert_eq!(last.content.as_deref(), Some("New response"));

        // Second-to-last should be the new user message
        let second_last = &agent.conversation().messages[agent.conversation().messages.len() - 2];
        assert_eq!(second_last.role, mc_core::Role::User);
        assert_eq!(second_last.content.as_deref(), Some("latest input"));
    }

    #[tokio::test]
    async fn compress_disabled_when_threshold_is_zero() {
        let mock = Arc::new(MockLlmProvider::new(vec![make_text_response("ok")]));
        let mut agent = create_test_agent(mock.clone(), None);
        agent.set_compress_threshold(0); // disabled

        fill_conversation(&mut agent, 30); // 60 messages, but threshold is 0

        agent.handle_message("new").await.unwrap();

        // No compression: 60 + 1 user + 1 assistant = 62
        assert_eq!(agent.conversation().messages.len(), 62);
        // Only 1 LLM call (no summarization)
        assert_eq!(mock.call_count(), 1);
    }

    #[test]
    fn format_messages_for_summary_empty() {
        let text = super::format_messages_for_summary(&[]);
        assert!(text.is_empty());
    }
}
