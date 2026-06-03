//! SQLite session persistence with FTS5 full-text search for mavis-claw.
//!
//! Provides [`SessionStore`] which stores conversations (sessions + messages)
//! in a SQLite database and enables full-text search across all messages
//! using FTS5.
//!
//! # Database Schema
//!
//! - **sessions** — conversation metadata (id, title, agent_id, timestamps)
//! - **messages** — ordered messages within a session (role, content, tool_calls)
//! - **messages_fts** — FTS5 virtual table for full-text search on message content
//!
//! # Usage
//!
//! ```no_run
//! use mc_storage::SessionStore;
//!
//! let store = SessionStore::new("~/.mavis-claw/data/sessions.db").unwrap();
//! let sessions = store.list_sessions(None).unwrap();
//! let results = store.search_messages("rust async", 10).unwrap();
//! ```

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use mc_core::{Conversation, ConversationMeta, Message};
use rusqlite::{params, Connection};
use tracing::debug;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lightweight session info returned by [`SessionStore::list_sessions`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionSummary {
    /// Session UUID.
    pub id: String,
    /// Optional human-readable title.
    pub title: Option<String>,
    /// Agent ID that owns this session.
    pub agent_id: Option<String>,
    /// Number of messages in the session.
    pub message_count: usize,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// When the session was last updated.
    pub updated_at: DateTime<Utc>,
}

/// A search result from FTS5 full-text search.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SearchResult {
    /// The session ID containing the matched message.
    pub session_id: String,
    /// Optional session title.
    pub session_title: Option<String>,
    /// The matched message.
    pub message: Message,
    /// FTS5 relevance score (lower = more relevant).
    pub rank: f64,
    /// When the message was stored.
    pub created_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// SessionStore
// ---------------------------------------------------------------------------

/// SQLite-backed session store with FTS5 full-text search.
///
/// Stores conversations as sessions with ordered messages. Supports:
/// - Session CRUD (create, read, delete, list)
/// - Message append and full replacement
/// - Full-text search across all message content via FTS5
/// - Session export as a complete [`Conversation`]
pub struct SessionStore {
    db_path: PathBuf,
    conn: Mutex<Connection>,
}

impl SessionStore {
    /// Open or create a SQLite database at the given path.
    ///
    /// Creates parent directories if they don't exist. Initializes the
    /// schema (sessions, messages, FTS5 virtual table) on first open.
    pub fn new(db_path: impl Into<PathBuf>) -> Result<Self, mc_core::McError> {
        let db_path = db_path.into();

        // Create parent directory if needed
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                mc_core::McError::Storage(format!(
                    "failed to create database directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let conn = Connection::open(&db_path).map_err(|e| {
            mc_core::McError::Storage(format!(
                "failed to open database {}: {}",
                db_path.display(),
                e
            ))
        })?;

        // Enable WAL mode for better concurrent read performance
        conn.execute_batch("PRAGMA journal_mode=WAL;").map_err(|e| {
            mc_core::McError::Storage(format!("failed to set WAL mode: {e}"))
        })?;

        // Enable foreign keys
        conn.execute_batch("PRAGMA foreign_keys=ON;").map_err(|e| {
            mc_core::McError::Storage(format!("failed to enable foreign keys: {e}"))
        })?;

        let store = Self { db_path, conn: Mutex::new(conn) };
        store.init_schema()?;
        Ok(store)
    }

    /// Create a new in-memory database (for testing).
    #[cfg(test)]
    fn new_in_memory() -> Result<Self, mc_core::McError> {
        let conn = Connection::open_in_memory().map_err(|e| {
            mc_core::McError::Storage(format!("failed to create in-memory database: {e}"))
        })?;

        conn.execute_batch("PRAGMA foreign_keys=ON;").map_err(|e| {
            mc_core::McError::Storage(format!("failed to enable foreign keys: {e}"))
        })?;

        let store = Self {
            db_path: PathBuf::from(":memory:"),
            conn: Mutex::new(conn),
        };
        store.init_schema()?;
        Ok(store)
    }

    /// Get the database file path.
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    // -----------------------------------------------------------------------
    // Schema
    // -----------------------------------------------------------------------

    /// Initialize the database schema.
    fn init_schema(&self) -> Result<(), mc_core::McError> {
        let conn = self.conn.lock().unwrap();
        conn
            .execute_batch(
                "
                CREATE TABLE IF NOT EXISTS sessions (
                    id          TEXT PRIMARY KEY,
                    title       TEXT,
                    agent_id    TEXT,
                    created_at  TEXT NOT NULL,
                    updated_at  TEXT NOT NULL
                );

                CREATE TABLE IF NOT EXISTS messages (
                    id              INTEGER PRIMARY KEY AUTOINCREMENT,
                    session_id      TEXT NOT NULL
                        REFERENCES sessions(id) ON DELETE CASCADE,
                    idx             INTEGER NOT NULL,
                    role            TEXT NOT NULL,
                    content         TEXT,
                    tool_calls_json TEXT,
                    tool_call_id    TEXT,
                    name            TEXT,
                    created_at      TEXT NOT NULL
                );

                CREATE INDEX IF NOT EXISTS idx_messages_session
                    ON messages(session_id);

                CREATE INDEX IF NOT EXISTS idx_messages_session_idx
                    ON messages(session_id, idx);

                CREATE INDEX IF NOT EXISTS idx_sessions_agent
                    ON sessions(agent_id);
                ",
            )
            .map_err(|e| mc_core::McError::Storage(format!("schema creation failed: {e}")))?;

        // FTS5 virtual table — external content mode
        // Uses content=messages so FTS stores only the index, not a copy of content.
        // Triggers keep the index in sync.
        let fts_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master
                 WHERE type='table' AND name='messages_fts'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| mc_core::McError::Storage(format!("FTS check failed: {e}")))?;

        if !fts_exists {
            conn
                .execute_batch(
                    "
                    CREATE VIRTUAL TABLE messages_fts USING fts5(
                        content,
                        content=messages,
                        content_rowid=id
                    );

                    -- Triggers to keep FTS index in sync
                    CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
                        INSERT INTO messages_fts(rowid, content)
                            VALUES (new.id, new.content);
                    END;

                    CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
                        INSERT INTO messages_fts(messages_fts, rowid, content)
                            VALUES ('delete', old.id, old.content);
                    END;

                    CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
                        INSERT INTO messages_fts(messages_fts, rowid, content)
                            VALUES ('delete', old.id, old.content);
                        INSERT INTO messages_fts(rowid, content)
                            VALUES (new.id, new.content);
                    END;
                    ",
                )
                .map_err(|e| {
                    mc_core::McError::Storage(format!("FTS5 creation failed: {e}"))
                })?;
        }

        debug!(db_path = %self.db_path.display(), "Database schema initialized");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Session CRUD
    // -----------------------------------------------------------------------

    /// Save a conversation as a session (upsert semantics).
    ///
    /// If a session with the same ID exists, replaces its metadata and all
    /// messages. If not, creates a new session. This is the primary method
    /// for persisting a conversation.
    pub fn save_session(&self, conversation: &Conversation) -> Result<(), mc_core::McError> {
        let now = Utc::now();
        let created_at = conversation
            .metadata
            .created_at
            .to_rfc3339();
        let updated_at = now.to_rfc3339();

        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction().map_err(|e| {
            mc_core::McError::Storage(format!("transaction begin failed: {e}"))
        })?;

        // Upsert session
        tx.execute(
            "INSERT INTO sessions (id, title, agent_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                 title = excluded.title,
                 agent_id = excluded.agent_id,
                 updated_at = excluded.updated_at",
            params![
                conversation.id,
                conversation.metadata.title,
                conversation.metadata.agent_id,
                created_at,
                updated_at,
            ],
        )
        .map_err(|e| mc_core::McError::Storage(format!("session upsert failed: {e}")))?;

        // Replace all messages (delete + insert)
        tx.execute(
            "DELETE FROM messages WHERE session_id = ?1",
            params![conversation.id],
        )
        .map_err(|e| {
            mc_core::McError::Storage(format!("message delete failed: {e}"))
        })?;

        for (idx, msg) in conversation.messages.iter().enumerate() {
            let tool_calls_json = msg
                .tool_calls
                .as_ref()
                .map(|tc| serde_json::to_string(tc).unwrap_or_default());

            tx.execute(
                "INSERT INTO messages
                     (session_id, idx, role, content, tool_calls_json, tool_call_id, name, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    conversation.id,
                    idx,
                    role_to_str(msg.role),
                    msg.content,
                    tool_calls_json,
                    msg.tool_call_id,
                    msg.name,
                    now.to_rfc3339(),
                ],
            )
            .map_err(|e| {
                mc_core::McError::Storage(format!("message insert failed: {e}"))
            })?;
        }

        tx.commit().map_err(|e| {
            mc_core::McError::Storage(format!("transaction commit failed: {e}"))
        })?;

        debug!(
            session_id = %conversation.id,
            message_count = conversation.messages.len(),
            "Session saved"
        );
        Ok(())
    }

    /// Check if a session exists.
    pub fn session_exists(&self, id: &str) -> Result<bool, mc_core::McError> {
        let conn = self.conn.lock().unwrap();
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sessions WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .map_err(|e| mc_core::McError::Storage(format!("exists check failed: {e}")))?;
        Ok(exists)
    }

    /// Load a full conversation by session ID.
    ///
    /// Returns `None` if the session doesn't exist.
    pub fn get_session(&self, id: &str) -> Result<Option<Conversation>, mc_core::McError> {
        let conn = self.conn.lock().unwrap();

        // Fetch session metadata
        let meta = match conn.query_row(
            "SELECT id, title, agent_id, created_at, updated_at
             FROM sessions WHERE id = ?1",
            params![id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        ) {
            Ok(m) => m,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
            Err(e) => {
                return Err(mc_core::McError::Storage(format!(
                    "session fetch failed: {e}"
                )))
            }
        };

        // Fetch messages
        let mut stmt = conn
            .prepare(
                "SELECT role, content, tool_calls_json, tool_call_id, name
                 FROM messages
                 WHERE session_id = ?1
                 ORDER BY idx ASC",
            )
            .map_err(|e| mc_core::McError::Storage(format!("message query failed: {e}")))?;

        let messages: Vec<Message> = stmt
            .query_map(params![id], |row| {
                let role_str: String = row.get(0)?;
                let content: Option<String> = row.get(1)?;
                let tool_calls_json: Option<String> = row.get(2)?;
                let tool_call_id: Option<String> = row.get(3)?;
                let name: Option<String> = row.get(4)?;

                let tool_calls = tool_calls_json
                    .and_then(|json| serde_json::from_str(json.as_str()).ok());

                Ok(Message {
                    role: str_to_role(&role_str),
                    content,
                    tool_calls,
                    tool_call_id,
                    name,
                })
            })
            .map_err(|e| mc_core::McError::Storage(format!("message map failed: {e}")))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| mc_core::McError::Storage(format!("message collect failed: {e}")))?;

        let created_at = parse_datetime(&meta.3)?;
        let updated_at = parse_datetime(&meta.4)?;

        Ok(Some(Conversation {
            id: meta.0,
            messages,
            metadata: ConversationMeta {
                title: meta.1,
                created_at,
                updated_at,
                agent_id: meta.2,
            },
        }))
    }

    /// Delete a session and all its messages (cascading).
    ///
    /// Returns `true` if the session existed and was deleted.
    pub fn delete_session(&self, id: &str) -> Result<bool, mc_core::McError> {
        let conn = self.conn.lock().unwrap();
        let rows = conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])
            .map_err(|e| mc_core::McError::Storage(format!("session delete failed: {e}")))?;
        Ok(rows > 0)
    }

    /// Update the title of a session.
    pub fn update_session_title(
        &self,
        id: &str,
        title: &str,
    ) -> Result<(), mc_core::McError> {
        let conn = self.conn.lock().unwrap();
        let rows = conn
            .execute(
                "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
                params![title, Utc::now().to_rfc3339(), id],
            )
            .map_err(|e| {
                mc_core::McError::Storage(format!("title update failed: {e}"))
            })?;
        if rows == 0 {
            return Err(mc_core::McError::Storage(format!(
                "session not found: {id}"
            )));
        }
        Ok(())
    }

    /// List all sessions, optionally filtered by agent_id.
    ///
    /// Returns lightweight [`SessionSummary`] objects (no message content).
    /// Ordered by most recently updated first.
    pub fn list_sessions(
        &self,
        agent_id: Option<&str>,
    ) -> Result<Vec<SessionSummary>, mc_core::McError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = if agent_id.is_some() {
            conn
                .prepare(
                    "SELECT s.id, s.title, s.agent_id, s.created_at, s.updated_at,
                            COUNT(m.id) as msg_count
                     FROM sessions s
                     LEFT JOIN messages m ON m.session_id = s.id
                     WHERE s.agent_id = ?1
                     GROUP BY s.id
                     ORDER BY s.updated_at DESC",
                )
                .map_err(|e| {
                    mc_core::McError::Storage(format!("list query failed: {e}"))
                })?
        } else {
            conn
                .prepare(
                    "SELECT s.id, s.title, s.agent_id, s.created_at, s.updated_at,
                            COUNT(m.id) as msg_count
                     FROM sessions s
                     LEFT JOIN messages m ON m.session_id = s.id
                     GROUP BY s.id
                     ORDER BY s.updated_at DESC",
                )
                .map_err(|e| {
                    mc_core::McError::Storage(format!("list query failed: {e}"))
                })?
        };

        let rows: Vec<SessionSummary> = if let Some(aid) = agent_id {
            stmt.query_map(params![aid], session_summary_from_row)
                .map_err(|e| {
                    mc_core::McError::Storage(format!("list map failed: {e}"))
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| {
                    mc_core::McError::Storage(format!("list collect failed: {e}"))
                })?
        } else {
            stmt.query_map([], session_summary_from_row)
                .map_err(|e| {
                    mc_core::McError::Storage(format!("list map failed: {e}"))
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| {
                    mc_core::McError::Storage(format!("list collect failed: {e}"))
                })?
        };

        Ok(rows)
    }

    // -----------------------------------------------------------------------
    // Message operations
    // -----------------------------------------------------------------------

    /// Append messages to an existing session.
    ///
    /// New messages are added after the current last message. Updates the
    /// session's `updated_at` timestamp.
    pub fn append_messages(
        &self,
        session_id: &str,
        messages: &[Message],
    ) -> Result<(), mc_core::McError> {
        let conn = self.conn.lock().unwrap();

        // Find current max idx
        let max_idx: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(idx), -1) FROM messages WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|e| {
                mc_core::McError::Storage(format!("max idx query failed: {e}"))
            })?;

        // Verify session exists
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|e| mc_core::McError::Storage(format!("exists check failed: {e}")))?;
        if !exists {
            return Err(mc_core::McError::Storage(format!(
                "session not found: {session_id}"
            )));
        }

        let now = Utc::now();
        let tx = conn.unchecked_transaction().map_err(|e| {
            mc_core::McError::Storage(format!("transaction begin failed: {e}"))
        })?;

        for (i, msg) in messages.iter().enumerate() {
            let idx = max_idx + 1 + i as i64;
            let tool_calls_json = msg
                .tool_calls
                .as_ref()
                .map(|tc| serde_json::to_string(tc).unwrap_or_default());

            tx.execute(
                "INSERT INTO messages
                     (session_id, idx, role, content, tool_calls_json, tool_call_id, name, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    session_id,
                    idx,
                    role_to_str(msg.role),
                    msg.content,
                    tool_calls_json,
                    msg.tool_call_id,
                    msg.name,
                    now.to_rfc3339(),
                ],
            )
            .map_err(|e| {
                mc_core::McError::Storage(format!("message insert failed: {e}"))
            })?;
        }

        // Update session timestamp
        tx.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![now.to_rfc3339(), session_id],
        )
        .map_err(|e| {
            mc_core::McError::Storage(format!("session timestamp update failed: {e}"))
        })?;

        tx.commit().map_err(|e| {
            mc_core::McError::Storage(format!("transaction commit failed: {e}"))
        })?;

        debug!(
            session_id = %session_id,
            appended = messages.len(),
            "Messages appended"
        );
        Ok(())
    }

    /// Search message content using FTS5 full-text search.
    ///
    /// Returns up to `limit` results ordered by relevance (most relevant first).
    /// The query supports FTS5 query syntax: phrase queries (`"exact phrase"`),
    /// boolean operators (`AND`, `OR`, `NOT`), prefix queries (`rust*`).
    pub fn search_messages(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SearchResult>, mc_core::McError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT
                     m.session_id,
                     s.title,
                     m.role,
                     m.content,
                     m.tool_calls_json,
                     m.tool_call_id,
                     m.name,
                     m.created_at,
                     bm25(messages_fts) as rank
                 FROM messages_fts
                 JOIN messages m ON m.id = messages_fts.rowid
                 JOIN sessions s ON s.id = m.session_id
                 WHERE messages_fts MATCH ?1
                 ORDER BY rank
                 LIMIT ?2",
            )
            .map_err(|e| {
                mc_core::McError::Storage(format!("search query prepare failed: {e}"))
            })?;

        let results: Vec<SearchResult> = stmt
            .query_map(params![query, limit as i64], |row| {
                let session_id: String = row.get(0)?;
                let session_title: Option<String> = row.get(1)?;
                let role_str: String = row.get(2)?;
                let content: Option<String> = row.get(3)?;
                let tool_calls_json: Option<String> = row.get(4)?;
                let tool_call_id: Option<String> = row.get(5)?;
                let name: Option<String> = row.get(6)?;
                let created_at_str: String = row.get(7)?;
                let rank: f64 = row.get(8)?;

                let tool_calls = tool_calls_json
                    .and_then(|json| serde_json::from_str(json.as_str()).ok());

                let message = Message {
                    role: str_to_role(&role_str),
                    content,
                    tool_calls,
                    tool_call_id,
                    name,
                };

                Ok(SearchResult {
                    session_id,
                    session_title,
                    message,
                    rank,
                    created_at: parse_datetime_inner(&created_at_str),
                })
            })
            .map_err(|e| {
                mc_core::McError::Storage(format!("search map failed: {e}"))
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                mc_core::McError::Storage(format!("search collect failed: {e}"))
            })?;

        Ok(results)
    }

    /// Export a session as a complete [`Conversation`] (alias for `get_session`).
    ///
    /// Provided for CLI convenience — functionally identical to `get_session`.
    pub fn export_session(&self, id: &str) -> Result<Option<Conversation>, mc_core::McError> {
        self.get_session(id)
    }

    /// Get the total number of stored sessions.
    pub fn session_count(&self) -> Result<usize, mc_core::McError> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .map_err(|e| {
                mc_core::McError::Storage(format!("count query failed: {e}"))
            })?;
        Ok(count as usize)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a [`mc_core::Role`] to its SQLite storage string.
fn role_to_str(role: mc_core::Role) -> &'static str {
    match role {
        mc_core::Role::System => "system",
        mc_core::Role::User => "user",
        mc_core::Role::Assistant => "assistant",
        mc_core::Role::Tool => "tool",
    }
}

/// Convert a storage string back to a [`mc_core::Role`].
fn str_to_role(s: &str) -> mc_core::Role {
    match s {
        "system" => mc_core::Role::System,
        "user" => mc_core::Role::User,
        "assistant" => mc_core::Role::Assistant,
        "tool" => mc_core::Role::Tool,
        _ => mc_core::Role::User, // fallback
    }
}

/// Parse an ISO 8601 datetime string, returning an error on failure.
fn parse_datetime(s: &str) -> Result<DateTime<Utc>, mc_core::McError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| {
            mc_core::McError::Storage(format!("datetime parse failed for '{s}': {e}"))
        })
}

/// Parse an ISO 8601 datetime string, falling back to epoch on failure.
fn parse_datetime_inner(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
}

/// Extract a `SessionSummary` from a SQL row.
fn session_summary_from_row(
    row: &rusqlite::Row,
) -> Result<SessionSummary, rusqlite::Error> {
    let id: String = row.get(0)?;
    let title: Option<String> = row.get(1)?;
    let agent_id: Option<String> = row.get(2)?;
    let created_at_str: String = row.get(3)?;
    let updated_at_str: String = row.get(4)?;
    let message_count: i64 = row.get(5)?;

    Ok(SessionSummary {
        id,
        title,
        agent_id,
        message_count: message_count as usize,
        created_at: parse_datetime_inner(&created_at_str),
        updated_at: parse_datetime_inner(&updated_at_str),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use mc_core::{FunctionCall, Message, Role, ToolCall};

    fn make_test_conversation(id: &str) -> Conversation {
        let mut conv = Conversation::new();
        conv.id = id.to_string();
        conv.metadata.title = Some("Test Session".to_string());
        conv.metadata.agent_id = Some("test-agent".to_string());
        conv.push(Message::user("Hello, how are you?"));
        conv.push(Message::assistant("I'm doing well, thanks!"));
        conv
    }

    #[test]
    fn create_in_memory_store() {
        let store = SessionStore::new_in_memory().unwrap();
        assert_eq!(store.session_count().unwrap(), 0);
    }

    #[test]
    fn save_and_get_session() {
        let store = SessionStore::new_in_memory().unwrap();
        let conv = make_test_conversation("sess-1");

        store.save_session(&conv).unwrap();
        let loaded = store.get_session("sess-1").unwrap().unwrap();

        assert_eq!(loaded.id, "sess-1");
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].role, Role::User);
        assert_eq!(
            loaded.messages[0].content.as_deref(),
            Some("Hello, how are you?")
        );
        assert_eq!(loaded.messages[1].role, Role::Assistant);
        assert_eq!(
            loaded.metadata.title.as_deref(),
            Some("Test Session")
        );
        assert_eq!(
            loaded.metadata.agent_id.as_deref(),
            Some("test-agent")
        );
    }

    #[test]
    fn get_nonexistent_session_returns_none() {
        let store = SessionStore::new_in_memory().unwrap();
        let result = store.get_session("nonexistent").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn save_session_upsert() {
        let store = SessionStore::new_in_memory().unwrap();
        let mut conv = make_test_conversation("sess-1");
        store.save_session(&conv).unwrap();

        // Modify and save again (upsert)
        conv.push(Message::user("Another message"));
        conv.metadata.title = Some("Updated Title".to_string());
        store.save_session(&conv).unwrap();

        let loaded = store.get_session("sess-1").unwrap().unwrap();
        assert_eq!(loaded.messages.len(), 3);
        assert_eq!(loaded.metadata.title.as_deref(), Some("Updated Title"));
    }

    #[test]
    fn session_exists() {
        let store = SessionStore::new_in_memory().unwrap();
        assert!(!store.session_exists("sess-1").unwrap());

        store
            .save_session(&make_test_conversation("sess-1"))
            .unwrap();
        assert!(store.session_exists("sess-1").unwrap());
        assert!(!store.session_exists("sess-2").unwrap());
    }

    #[test]
    fn delete_session() {
        let store = SessionStore::new_in_memory().unwrap();
        store
            .save_session(&make_test_conversation("sess-1"))
            .unwrap();
        assert!(store.session_exists("sess-1").unwrap());

        assert!(store.delete_session("sess-1").unwrap());
        assert!(!store.session_exists("sess-1").unwrap());

        // Deleting non-existent returns false
        assert!(!store.delete_session("sess-1").unwrap());
    }

    #[test]
    fn update_session_title() {
        let store = SessionStore::new_in_memory().unwrap();
        store
            .save_session(&make_test_conversation("sess-1"))
            .unwrap();

        store
            .update_session_title("sess-1", "New Title")
            .unwrap();

        let loaded = store.get_session("sess-1").unwrap().unwrap();
        assert_eq!(loaded.metadata.title.as_deref(), Some("New Title"));
    }

    #[test]
    fn update_title_nonexistent_errors() {
        let store = SessionStore::new_in_memory().unwrap();
        let result = store.update_session_title("nope", "title");
        assert!(result.is_err());
    }

    #[test]
    fn list_sessions_all() {
        let store = SessionStore::new_in_memory().unwrap();
        store
            .save_session(&make_test_conversation("sess-1"))
            .unwrap();
        store
            .save_session(&make_test_conversation("sess-2"))
            .unwrap();

        let list = store.list_sessions(None).unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().any(|s| s.id == "sess-1"));
        assert!(list.iter().any(|s| s.id == "sess-2"));
    }

    #[test]
    fn list_sessions_by_agent() {
        let store = SessionStore::new_in_memory().unwrap();
        let mut conv1 = make_test_conversation("sess-1");
        conv1.metadata.agent_id = Some("agent-a".to_string());
        store.save_session(&conv1).unwrap();

        let mut conv2 = make_test_conversation("sess-2");
        conv2.metadata.agent_id = Some("agent-b".to_string());
        store.save_session(&conv2).unwrap();

        let list_a = store.list_sessions(Some("agent-a")).unwrap();
        assert_eq!(list_a.len(), 1);
        assert_eq!(list_a[0].id, "sess-1");

        let list_b = store.list_sessions(Some("agent-b")).unwrap();
        assert_eq!(list_b.len(), 1);
        assert_eq!(list_b[0].id, "sess-2");

        let list_all = store.list_sessions(None).unwrap();
        assert_eq!(list_all.len(), 2);
    }

    #[test]
    fn list_sessions_message_count() {
        let store = SessionStore::new_in_memory().unwrap();
        store
            .save_session(&make_test_conversation("sess-1"))
            .unwrap();

        let list = store.list_sessions(None).unwrap();
        assert_eq!(list[0].message_count, 2);
    }

    #[test]
    fn append_messages() {
        let store = SessionStore::new_in_memory().unwrap();
        store
            .save_session(&make_test_conversation("sess-1"))
            .unwrap();

        store
            .append_messages(
                "sess-1",
                &[Message::user("Third message"), Message::assistant("Reply")],
            )
            .unwrap();

        let loaded = store.get_session("sess-1").unwrap().unwrap();
        assert_eq!(loaded.messages.len(), 4);
        assert_eq!(
            loaded.messages[2].content.as_deref(),
            Some("Third message")
        );
        assert_eq!(loaded.messages[3].content.as_deref(), Some("Reply"));
    }

    #[test]
    fn append_to_nonexistent_session_errors() {
        let store = SessionStore::new_in_memory().unwrap();
        let result = store.append_messages("nope", &[Message::user("hi")]);
        assert!(result.is_err());
    }

    #[test]
    fn save_and_load_tool_calls() {
        let store = SessionStore::new_in_memory().unwrap();
        let mut conv = Conversation::new();
        conv.id = "tc-test".to_string();
        conv.push(Message::user("use a tool"));
        conv.push(Message {
            role: Role::Assistant,
            content: Some("Let me call a tool".to_string()),
            tool_calls: Some(vec![ToolCall {
                id: "call_1".to_string(),
                function: FunctionCall {
                    name: "bash".to_string(),
                    arguments: r#"{"command":"ls"}"#.to_string(),
                },
            }]),
            tool_call_id: None,
            name: None,
        });
        conv.push(Message::tool("call_1", "file1.txt\nfile2.txt"));

        store.save_session(&conv).unwrap();
        let loaded = store.get_session("tc-test").unwrap().unwrap();

        assert_eq!(loaded.messages.len(), 3);

        let assistant_msg = &loaded.messages[1];
        assert!(assistant_msg.tool_calls.is_some());
        let tc = assistant_msg.tool_calls.as_ref().unwrap();
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0].id, "call_1");
        assert_eq!(tc[0].function.name, "bash");
        assert_eq!(tc[0].function.arguments, r#"{"command":"ls"}"#);

        let tool_msg = &loaded.messages[2];
        assert_eq!(tool_msg.role, Role::Tool);
        assert_eq!(tool_msg.tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn search_messages_basic() {
        let store = SessionStore::new_in_memory().unwrap();
        let mut conv = make_test_conversation("sess-1");
        conv.push(Message::user("What is Rust programming?"));
        conv.push(Message::assistant(
            "Rust is a systems programming language focused on safety.",
        ));
        store.save_session(&conv).unwrap();

        let results = store.search_messages("Rust", 10).unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r
            .message
            .content
            .as_ref()
            .map_or(false, |c| c.contains("Rust"))));
    }

    #[test]
    fn search_messages_no_results() {
        let store = SessionStore::new_in_memory().unwrap();
        store
            .save_session(&make_test_conversation("sess-1"))
            .unwrap();

        let results = store.search_messages("nonexistent_xyz", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn search_messages_across_sessions() {
        let store = SessionStore::new_in_memory().unwrap();

        let mut conv1 = make_test_conversation("sess-1");
        conv1.push(Message::user("Tell me about Python"));
        store.save_session(&conv1).unwrap();

        let mut conv2 = make_test_conversation("sess-2");
        conv2.push(Message::user("Tell me about Rust"));
        store.save_session(&conv2).unwrap();

        // Search for "Rust" — should find only sess-2
        let results = store.search_messages("Rust", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "sess-2");

        // Search for "Python" — should find only sess-1
        let results = store.search_messages("Python", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].session_id, "sess-1");
    }

    #[test]
    fn search_messages_limit() {
        let store = SessionStore::new_in_memory().unwrap();
        let mut conv = make_test_conversation("sess-1");
        for i in 0..20 {
            conv.push(Message::user(format!("Rust message number {i}")));
        }
        store.save_session(&conv).unwrap();

        let results = store.search_messages("Rust", 5).unwrap();
        assert!(results.len() <= 5);
    }

    #[test]
    fn export_session_matches_get_session() {
        let store = SessionStore::new_in_memory().unwrap();
        store
            .save_session(&make_test_conversation("sess-1"))
            .unwrap();

        let from_get = store.get_session("sess-1").unwrap().unwrap();
        let from_export = store.export_session("sess-1").unwrap().unwrap();

        assert_eq!(from_get.id, from_export.id);
        assert_eq!(from_get.messages.len(), from_export.messages.len());
        assert_eq!(from_get.metadata.title, from_export.metadata.title);
    }

    #[test]
    fn save_empty_conversation() {
        let store = SessionStore::new_in_memory().unwrap();
        let conv = Conversation::new();
        store.save_session(&conv).unwrap();

        let loaded = store.get_session(&conv.id).unwrap().unwrap();
        assert_eq!(loaded.messages.len(), 0);
    }

    #[test]
    fn save_file_based_store() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db_path = tmp.path().join("test_sessions.db");

        let store = SessionStore::new(&db_path).unwrap();
        store
            .save_session(&make_test_conversation("sess-1"))
            .unwrap();
        drop(store);

        // Re-open and verify
        let store2 = SessionStore::new(&db_path).unwrap();
        let loaded = store2.get_session("sess-1").unwrap().unwrap();
        assert_eq!(loaded.messages.len(), 2);
    }

    #[test]
    fn list_sessions_summary_fields() {
        let store = SessionStore::new_in_memory().unwrap();
        store
            .save_session(&make_test_conversation("sess-1"))
            .unwrap();

        let list = store.list_sessions(None).unwrap();
        assert_eq!(list.len(), 1);

        let summary = &list[0];
        assert_eq!(summary.id, "sess-1");
        assert_eq!(summary.title.as_deref(), Some("Test Session"));
        assert_eq!(summary.agent_id.as_deref(), Some("test-agent"));
        assert_eq!(summary.message_count, 2);
    }

    #[test]
    fn append_messages_maintains_order() {
        let store = SessionStore::new_in_memory().unwrap();
        store
            .save_session(&make_test_conversation("sess-1"))
            .unwrap();

        store
            .append_messages(
                "sess-1",
                &[
                    Message::user("msg3"),
                    Message::assistant("msg4"),
                    Message::user("msg5"),
                ],
            )
            .unwrap();

        let loaded = store.get_session("sess-1").unwrap().unwrap();
        assert_eq!(loaded.messages.len(), 5);
        assert_eq!(
            loaded.messages[0].content.as_deref(),
            Some("Hello, how are you?")
        );
        assert_eq!(loaded.messages[2].content.as_deref(), Some("msg3"));
        assert_eq!(loaded.messages[4].content.as_deref(), Some("msg5"));
    }

    #[test]
    fn session_count() {
        let store = SessionStore::new_in_memory().unwrap();
        assert_eq!(store.session_count().unwrap(), 0);

        store
            .save_session(&make_test_conversation("s1"))
            .unwrap();
        assert_eq!(store.session_count().unwrap(), 1);

        store
            .save_session(&make_test_conversation("s2"))
            .unwrap();
        assert_eq!(store.session_count().unwrap(), 2);

        store.delete_session("s1").unwrap();
        assert_eq!(store.session_count().unwrap(), 1);
    }
}
