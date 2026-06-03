//! mavis-claw: A personal AI assistant platform written in Rust.
//!
//! CLI entry point with subcommands for chat, config checking, and gateway startup.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use futures::StreamExt;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use mc_agent::Agent;
use mc_channels::{
    Channel, ChannelKind, ChannelSource, ChannelTarget, FeishuChannel, FeishuConfig,
    IncomingMessage, MessageContent, MessageHandler, OutgoingMessage,
};
use mc_config::AppConfig;
use mc_core::ToolCall;
use mc_gateway::{ChatCompletionRequest, ChatHandler, ChatCompletionResponse, ChatChoice, ChatMessage, ChatUsage, Gateway};
use mc_llm::{ProviderRegistry, RetryConfig};
use mc_memory::MemoryStore;
use mc_skills::SkillLoader;
use mc_storage::SessionStore;

/// mavis-claw — personal AI assistant platform.
#[derive(Parser)]
#[command(name = "mavis-claw", version, about)]
struct Cli {
    /// Path to the configuration file.
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Log level override (e.g., "debug", "mavis_claw=trace").
    #[arg(long, global = true)]
    log_level: Option<String>,

    /// Agent ID to use (defaults to first agent in config).
    #[arg(short, long, global = true)]
    agent: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Load and print the resolved configuration.
    ConfigCheck,
    /// Start an interactive chat session with an agent.
    Chat {
        /// Optional initial message to send (single-shot mode).
        message: Option<String>,
    },
    /// Start the HTTP gateway server with channel integrations.
    Gateway,
    /// Manage stored sessions (list, search, export).
    Session {
        #[command(subcommand)]
        action: SessionAction,
    },
}

#[derive(Subcommand)]
enum SessionAction {
    /// List all stored sessions.
    List {
        /// Filter by agent ID.
        #[arg(short, long)]
        agent: Option<String>,
    },
    /// Search message content using full-text search.
    Search {
        /// The search query (supports FTS5 syntax).
        query: String,
        /// Maximum number of results.
        #[arg(short, long, default_value = "10")]
        limit: usize,
    },
    /// Export a session as JSON.
    Export {
        /// The session ID to export.
        session_id: String,
        /// Output file path (prints to stdout if omitted).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize tracing
    let filter = match &cli.log_level {
        Some(level) => EnvFilter::try_new(level)?,
        None => EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
    };
    tracing_subscriber::fmt().with_env_filter(filter).init();

    match cli.command {
        Commands::ConfigCheck => cmd_config_check(cli.config.as_deref()),
        Commands::Chat { message } => cmd_chat(cli.config.as_deref(), cli.agent.as_deref(), message).await,
        Commands::Gateway => cmd_gateway(cli.config.as_deref(), cli.agent.as_deref()).await,
        Commands::Session { action } => cmd_session(cli.config.as_deref(), action).await,
    }
}

/// Load and print the resolved configuration.
fn cmd_config_check(config_path: Option<&Path>) -> Result<()> {
    let config = mc_config::AppConfig::load(config_path)?;

    println!("=== mavis-claw configuration ===\n");
    println!("{}", serde_json::to_string_pretty(&config)?);
    println!("\n✅ Configuration loaded successfully.");
    Ok(())
}

/// Resolve the agent configuration to use.
///
/// If `agent_id` is provided, find it by ID. Otherwise, use the first agent.
fn resolve_agent_config<'a>(
    config: &'a AppConfig,
    agent_id: Option<&str>,
) -> Result<&'a mc_config::AgentConfig> {
    match agent_id {
        Some(id) => config
            .agents
            .iter()
            .find(|a| a.id == id)
            .with_context(|| format!("Agent '{id}' not found in configuration")),
        None => config
            .agents
            .first()
            .context("No agents defined in configuration"),
    }
}

/// Build an Agent from configuration (shared between chat and gateway modes).
fn build_agent(
    config: &AppConfig,
    agent_cfg: &mc_config::AgentConfig,
) -> Result<Agent> {
    let mut registry = ProviderRegistry::from_config(&config.providers)
        .context("Failed to initialize LLM provider registry")?;

    // Wrap all providers with automatic retry (3 retries, exponential backoff)
    registry.wrap_all_with_retry(RetryConfig::default());

    // Load skills
    let skills_dir = agent_cfg
        .skills_dir
        .as_deref()
        .unwrap_or(&config.skills.path);
    let skill_loader = if config.skills.enabled {
        let mut loader = SkillLoader::new();
        let skills_path = Path::new(skills_dir);
        if skills_path.exists() {
            match loader.load_dir(skills_path) {
                Ok(count) => {
                    info!(skills_dir = %skills_dir, count, "Skills loaded");
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to load skills (continuing without)");
                }
            }
        }
        Some(Arc::new(loader))
    } else {
        None
    };

    // Initialize memory store
    let memory_dir = agent_cfg
        .memory_dir
        .as_deref()
        .unwrap_or(&config.memory.path);
    let memory_store = if config.memory.enabled {
        let store = MemoryStore::new(memory_dir);
        info!(memory_dir = %memory_dir, "Memory store initialized");
        Some(Arc::new(store))
    } else {
        None
    };

    // Create tool registry with memory tool
    let tool_registry = Arc::new(mc_tools::builtin_registry(
        &config.tools,
        memory_store.as_ref().map(|ms| MemoryStore::new(ms.base_path())),
    ));

    // Initialize session store (SQLite persistence)
    let data_dir = dirs_data_dir().join("mavis-claw").join("data");
    let db_path = data_dir.join("sessions.db");
    let session_store = match SessionStore::new(&db_path) {
        Ok(store) => {
            info!(db_path = %db_path.display(), "Session store initialized");
            Some(Arc::new(store))
        }
        Err(e) => {
            warn!(error = %e, "Failed to initialize session store (continuing without persistence)");
            None
        }
    };

    Agent::new(
        agent_cfg,
        &registry,
        Some(tool_registry),
        skill_loader,
        memory_store,
        Some(&config.evolution),
        Some(skills_dir),
        session_store,
    )
    .context("Failed to create agent")
}

/// Get the platform-appropriate data directory.
fn dirs_data_dir() -> PathBuf {
    if let Ok(profile) = std::env::var("USERPROFILE") {
        PathBuf::from(profile)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home)
    } else {
        PathBuf::from(".")
    }
}

// ─── Agent-backed ChatHandler (OpenAI-compatible API) ───────────────────────────

/// Wraps an Agent to implement the gateway's ChatHandler trait.
struct AgentChatHandler {
    agent: Arc<Mutex<Agent>>,
}

#[async_trait::async_trait]
impl ChatHandler for AgentChatHandler {
    async fn handle_chat(
        &self,
        request: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, mc_core::McError> {
        let user_input = request
            .messages
            .last()
            .map(|m| m.content.as_str())
            .unwrap_or("");

        let mut agent = self.agent.lock().await;
        let response = agent.handle_message(user_input).await?;

        let content = response.content.unwrap_or_default();
        let model = request.model.clone();

        Ok(ChatCompletionResponse {
            id: uuid::Uuid::new_v4().to_string(),
            object: "chat.completion".into(),
            created: chrono::Utc::now().timestamp(),
            model,
            choices: vec![ChatChoice {
                index: 0,
                message: ChatMessage {
                    role: "assistant".into(),
                    content,
                },
                finish_reason: "stop".into(),
            }],
            usage: ChatUsage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
            },
        })
    }
}

// ─── Agent-backed MessageHandler (Feishu webhook → Agent → reply) ───────────────

/// Bridges incoming channel messages to the Agent and sends replies.
struct AgentMessageHandler {
    agent: Arc<Mutex<Agent>>,
    channel: Arc<FeishuChannel>,
}

#[async_trait::async_trait]
impl MessageHandler for AgentMessageHandler {
    async fn on_message(
        &self,
        source: ChannelSource,
        message: IncomingMessage,
    ) -> Result<(), mc_core::McError> {
        // Extract text content
        let user_text = match &message.content {
            MessageContent::Text(t) => t.clone(),
            _ => {
                info!(sender = %source.sender.id, "Ignoring non-text message");
                return Ok(());
            }
        };

        if user_text.trim().is_empty() {
            return Ok(());
        }

        info!(
            sender = %source.sender.id,
            conversation = %source.conversation_key,
            text_len = user_text.len(),
            "Processing incoming message"
        );

        // Call the agent
        let response = {
            let mut agent = self.agent.lock().await;
            agent.handle_message(&user_text).await?
        };

        let reply_text = response.content.unwrap_or_default();
        if reply_text.is_empty() {
            return Ok(());
        }

        // Send reply back through the channel
        let target = ChannelTarget {
            channel_kind: source.channel_kind,
            conversation_key: source.conversation_key,
        };
        let outgoing = OutgoingMessage::text(&reply_text)
            .with_reply_to(&message.channel_id);

        self.channel.send(&target, outgoing).await?;

        info!("Reply sent successfully");
        Ok(())
    }
}

// ─── Session command ─────────────────────────────────────────────────────────

/// Handle the `session` subcommand: list, search, export.
async fn cmd_session(_config_path: Option<&Path>, action: SessionAction) -> Result<()> {
    let data_dir = dirs_data_dir().join("mavis-claw").join("data");
    let db_path = data_dir.join("sessions.db");

    if !db_path.exists() {
        bail!(
            "No session database found at {}. Run `mavis-claw chat` first to create sessions.",
            db_path.display()
        );
    }

    let store = SessionStore::new(&db_path)
        .context("Failed to open session store")?;

    match action {
        SessionAction::List { agent } => cmd_session_list(&store, agent.as_deref())?,
        SessionAction::Search { query, limit } => {
            cmd_session_search(&store, &query, limit)?
        }
        SessionAction::Export { session_id, output } => {
            cmd_session_export(&store, &session_id, output.as_deref())?
        }
    }

    Ok(())
}

/// List all sessions.
fn cmd_session_list(
    store: &SessionStore,
    agent_id: Option<&str>,
) -> Result<()> {
    let sessions = store
        .list_sessions(agent_id)
        .context("Failed to list sessions")?;

    if sessions.is_empty() {
        println!("No sessions found.");
        return Ok(());
    }

    println!(
        "{:<38} {:<30} {:<15} {:<8} UPDATED",
        "ID", "TITLE", "AGENT", "MSGS"
    );
    println!("{}", "-".repeat(110));

    for s in &sessions {
        let title = s.title.as_deref().unwrap_or("(untitled)");
        let agent = s.agent_id.as_deref().unwrap_or("-");
        let updated = s.updated_at.format("%Y-%m-%d %H:%M");
        println!(
            "{:<38} {:<30} {:<15} {:<8} {}",
            s.id,
            truncate(title, 28),
            truncate(agent, 13),
            s.message_count,
            updated,
        );
    }

    println!("\n{} session(s) total.", sessions.len());
    Ok(())
}

/// Search message content.
fn cmd_session_search(
    store: &SessionStore,
    query: &str,
    limit: usize,
) -> Result<()> {
    let results = store
        .search_messages(query, limit)
        .context("Search failed")?;

    if results.is_empty() {
        println!("No results found for: {query}");
        return Ok(());
    }

    println!("Search results for: \"{query}\" ({})", results.len());
    println!("{}", "-".repeat(80));

    for r in &results {
        let session_title = r.session_title.as_deref().unwrap_or("(untitled)");
        let role = match r.message.role {
            mc_core::Role::System => "system",
            mc_core::Role::User => "user",
            mc_core::Role::Assistant => "assistant",
            mc_core::Role::Tool => "tool",
        };
        let content = r
            .message
            .content
            .as_deref()
            .unwrap_or("(no content)");
        let preview = truncate(content, 100);
        let time = r.created_at.format("%Y-%m-%d %H:%M");

        println!(
            "[{}] {} ({}) — session: {} ({})",
            time, role, preview, session_title, r.session_id,
        );
    }

    Ok(())
}

/// Export a session as JSON.
fn cmd_session_export(
    store: &SessionStore,
    session_id: &str,
    output: Option<&Path>,
) -> Result<()> {
    let conversation = store
        .export_session(session_id)
        .context("Export failed")?;

    let conversation = match conversation {
        Some(c) => c,
        None => {
            bail!("Session not found: {session_id}");
        }
    };

    let json = serde_json::to_string_pretty(&conversation)
        .context("Failed to serialize session")?;

    match output {
        Some(path) => {
            std::fs::write(path, &json)
                .with_context(|| format!("Failed to write {}", path.display()))?;
            println!("Session exported to {}", path.display());
        }
        None => {
            println!("{json}");
        }
    }

    Ok(())
}

/// Truncate a string to max_len, appending "..." if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len - 3])
    }
}

// ─── Gateway command ────────────────────────────────────────────────────────────

/// Start the HTTP gateway with channel integrations.
async fn cmd_gateway(
    config_path: Option<&Path>,
    agent_id: Option<&str>,
) -> Result<()> {
    // 1. Load configuration
    let config = AppConfig::load(config_path).context("Failed to load configuration")?;

    if config.agents.is_empty() {
        bail!("No agents defined in configuration. Add at least one [[agents]] section.");
    }

    // 2. Resolve agent config
    let agent_cfg = resolve_agent_config(&config, agent_id)?;
    info!(agent = %agent_cfg.name, model = %agent_cfg.model, "Initializing gateway");

    // 3. Build agent
    let agent = build_agent(&config, agent_cfg)?;
    let agent = Arc::new(Mutex::new(agent));

    // 4. Set up channel integrations from config
    let mut webhook_channels: Vec<Arc<dyn mc_gateway::WebhookChannel>> = Vec::new();

    for ch_cfg in &config.channels {
        match ch_cfg.kind {
            ChannelKind::Feishu => {
                let feishu_config = FeishuConfig::from_settings(
                    &ch_cfg.settings,
                )
                .context("Failed to parse Feishu channel config")?;

                let feishu_channel = Arc::new(FeishuChannel::new(feishu_config));

                // Create the message handler that bridges Feishu ↔ Agent
                let handler: Arc<dyn MessageHandler> = Arc::new(AgentMessageHandler {
                    agent: Arc::clone(&agent),
                    channel: Arc::clone(&feishu_channel),
                });

                // Re-create channel with handler
                let feishu_config2 = FeishuConfig::from_settings(&ch_cfg.settings)
                    .context("Failed to parse Feishu channel config")?;
                let feishu_channel = Arc::new(
                    FeishuChannel::new(feishu_config2)
                        .with_handler(handler),
                );

                webhook_channels.push(feishu_channel);
                info!(channel_id = %ch_cfg.id, "Feishu channel registered");
            }
            _ => {
                warn!(
                    channel_id = %ch_cfg.id,
                    kind = ?ch_cfg.kind,
                    "Channel kind not yet supported, skipping"
                );
            }
        }
    }

    // 5. Create the ChatHandler for the OpenAI-compatible API
    let chat_handler: Arc<dyn ChatHandler> = Arc::new(AgentChatHandler {
        agent: Arc::clone(&agent),
    });

    // 6. Build and start the gateway
    let gateway = Gateway::new(config.gateway.clone())
        .with_channels(webhook_channels)
        .with_chat_handler(chat_handler);

    info!(
        bind = %gateway.config().bind,
        channels = gateway.channels().len(),
        "Starting gateway"
    );

    gateway.start().await.context("Gateway server error")?;

    Ok(())
}

// ─── Chat command ───────────────────────────────────────────────────────────────

/// Interactive chat session with an agent.
async fn cmd_chat(
    config_path: Option<&Path>,
    agent_id: Option<&str>,
    initial_message: Option<String>,
) -> Result<()> {
    // 1. Load configuration
    let config = AppConfig::load(config_path).context("Failed to load configuration")?;

    if config.providers.is_empty() {
        bail!("No providers defined in configuration. Add at least one [[providers]] section.");
    }
    if config.agents.is_empty() {
        bail!("No agents defined in configuration. Add at least one [[agents]] section.");
    }

    // 2. Resolve agent config
    let agent_cfg = resolve_agent_config(&config, agent_id)?;
    info!(agent = %agent_cfg.name, model = %agent_cfg.model, "Starting chat session");

    // 3. Build agent
    let mut agent = build_agent(&config, agent_cfg)?;

    // 4. Chat loop
    match initial_message {
        Some(msg) => {
            // Single-shot mode
            single_shot(&mut agent, &msg).await?;
        }
        None => {
            // Interactive mode
            interactive_loop(&mut agent).await?;
        }
    }

    Ok(())
}

/// Collect a stream into (content, tool_calls, finish_reason).
///
/// Prints content deltas to stdout as they arrive and accumulates
/// tool call deltas into complete ToolCall structs. After the stream
/// completes, prints a summary line with character count and estimated
/// token usage.
async fn collect_stream(
    agent: &mut Agent,
    user_input: &str,
) -> Result<(String, Option<Vec<ToolCall>>, Option<mc_llm::FinishReason>)> {
    use std::collections::BTreeMap;

    let mut stream = agent
        .handle_message_stream(user_input)
        .await
        .context("LLM request failed")?;

    let mut stdout = tokio::io::stdout();
    let mut full_content = String::new();
    // Accumulate tool call by index: (id, name, arguments_json)
    let mut tool_call_acc: BTreeMap<u32, (String, String, String)> = BTreeMap::new();
    let mut finish_reason: Option<mc_llm::FinishReason> = None;
    let mut char_count: usize = 0;

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.context("Stream error")?;

        // Accumulate content
        if !chunk.delta.is_empty() {
            stdout.write_all(chunk.delta.as_bytes()).await?;
            stdout.flush().await?;
            full_content.push_str(&chunk.delta);
            char_count += chunk.delta.len();
        }

        // Accumulate tool call deltas
        if let Some(tc_delta) = &chunk.tool_call_delta {
            let entry = tool_call_acc.entry(tc_delta.index).or_insert_with(|| {
                (String::new(), String::new(), String::new())
            });
            if let Some(id) = &tc_delta.id {
                entry.0 = id.clone();
            }
            if let Some(name) = &tc_delta.name {
                entry.1 = name.clone();
            }
            if let Some(args) = &tc_delta.arguments_delta {
                entry.2.push_str(args);
            }
        }

        if chunk.finish_reason.is_some() {
            finish_reason = chunk.finish_reason;
        }
    }
    drop(stream);

    stdout.write_all(b"\n").await?;
    stdout.flush().await?;

    // Print streaming summary: character count and estimated tokens
    let est_tokens = char_count / 4; // rough estimate: ~4 chars per token
    let summary = format!("  [{char_count} chars, ~{est_tokens} tokens]\n");
    stdout.write_all(summary.as_bytes()).await?;
    stdout.flush().await?;

    // Convert accumulated tool call deltas into ToolCall structs
    let tool_calls = if tool_call_acc.is_empty() {
        None
    } else {
        Some(
            tool_call_acc
                .into_values()
                .filter(|(id, name, _)| !id.is_empty() && !name.is_empty())
                .map(|(id, name, arguments)| ToolCall {
                    id,
                    function: mc_core::FunctionCall { name, arguments },
                })
                .collect::<Vec<_>>(),
        )
    };

    Ok((full_content, tool_calls, finish_reason))
}

/// Send a single message and print the streaming response.
async fn single_shot(agent: &mut Agent, user_input: &str) -> Result<()> {
    print!("🤖 ");

    let (content, tool_calls, _finish_reason) = collect_stream(agent, user_input).await?;

    // Finalize the stream response
    agent.finalize_stream(&content, tool_calls);

    // Run tool loop if there were tool calls
    if let Some(final_msg) = agent.execute_tool_loop().await? {
        // Print the final response after tool execution
        if let Some(text) = &final_msg.content {
            if !text.is_empty() {
                print!("🤖 ");
                let mut stdout = tokio::io::stdout();
                stdout.write_all(text.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
        }
    }

    Ok(())
}

/// Interactive REPL loop — read user input, stream assistant response.
async fn interactive_loop(agent: &mut Agent) -> Result<()> {
    let stdin = BufReader::new(tokio::io::stdin());
    let mut lines = stdin.lines();
    let mut stdout = tokio::io::stdout();

    stdout.write_all(b"mavis-claw chat (type 'exit' or 'quit' to leave)\n").await?;
    stdout.flush().await?;

    loop {
        stdout.write_all("\n🧑 ".as_bytes()).await?;
        stdout.flush().await?;

        let input = match lines.next_line().await? {
            Some(line) => line,
            None => break, // EOF
        };

        let trimmed = input.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed == "exit" || trimmed == "quit" {
            stdout.write_all(b"Goodbye!\n").await?;
            stdout.flush().await?;
            break;
        }

        stdout.write_all("\n🤖 ".as_bytes()).await?;
        stdout.flush().await?;

        // Stream the response
        match collect_stream(agent, trimmed).await {
            Ok((content, tool_calls, _finish_reason)) => {
                // Finalize the streamed response
                agent.finalize_stream(&content, tool_calls);

                // Run tool loop if needed
                match agent.execute_tool_loop().await {
                    Ok(Some(final_msg)) => {
                        if let Some(text) = &final_msg.content {
                            if !text.is_empty() {
                                print!("🤖 ");
                                let mut stdout = tokio::io::stdout();
                                stdout.write_all(text.as_bytes()).await?;
                                stdout.write_all(b"\n").await?;
                                stdout.flush().await?;
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let err_msg = format!("\n[tool loop error: {e}]\n");
                        stdout.write_all(err_msg.as_bytes()).await?;
                        stdout.flush().await?;
                    }
                }
            }
            Err(e) => {
                let err_msg = format!("\n[error: {e}]\n");
                stdout.write_all(err_msg.as_bytes()).await?;
                stdout.flush().await?;
            }
        }
    }

    Ok(())
}
