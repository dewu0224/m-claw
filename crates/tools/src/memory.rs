//! MemoryTool — CRUD operations on agent/user memory via the Tool trait.

use async_trait::async_trait;
use mc_core::{McError, Tool, ToolDefinition};
use mc_memory::{MemoryFile, MemoryStore};
use serde_json::{json, Value};

/// Maximum size of a single content payload (1 MiB).
const MAX_CONTENT_SIZE: usize = 1024 * 1024;

/// Maximum total file size for memory files (10 MiB).
const MAX_FILE_SIZE: usize = 10 * 1024 * 1024;

/// Tool that exposes memory CRUD operations to the LLM.
///
/// Supports four operations:
/// - `read` — read memory file contents
/// - `write` — overwrite memory file contents
/// - `append` — append content to a memory file
/// - `update_section` — update a section by `## heading`
pub struct MemoryTool {
    store: MemoryStore,
}

impl MemoryTool {
    /// Create a new `MemoryTool` rooted at the given path.
    pub fn new(store: MemoryStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for MemoryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "memory".to_string(),
            description: "Read, write, append, or update memory files (MEMORY.md for agent knowledge, USER.md for user profile). \
                Operations: 'read' (read file), 'write' (overwrite file), 'append' (add to end), \
                'update_section' (replace section under a ## heading)."
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["read", "write", "append", "update_section"],
                        "description": "The memory operation to perform."
                    },
                    "file": {
                        "type": "string",
                        "enum": ["agent", "user"],
                        "description": "Which memory file: 'agent' for MEMORY.md (agent knowledge), 'user' for USER.md (user profile)."
                    },
                    "content": {
                        "type": "string",
                        "description": "Content for write/append/update_section operations. Required for write and append; for update_section this is the new section body."
                    },
                    "heading": {
                        "type": "string",
                        "description": "Section heading (without ##) for update_section operation. Required for update_section."
                    }
                },
                "required": ["operation", "file"]
            }),
        }
    }

    async fn execute(&self, args: Value) -> Result<String, McError> {
        let operation = args["operation"]
            .as_str()
            .ok_or_else(|| McError::Tool("missing required field 'operation'".to_string()))?;

        let file_str = args["file"]
            .as_str()
            .ok_or_else(|| McError::Tool("missing required field 'file'".to_string()))?;

        let memory_file = match file_str {
            "agent" => MemoryFile::Agent,
            "user" => MemoryFile::User,
            other => {
                return Err(McError::Tool(format!(
                    "invalid file '{other}', must be 'agent' or 'user'"
                )));
            }
        };

        match operation {
            "read" => {
                let content = self.store.read_file(memory_file)?;
                if content.is_empty() {
                    Ok(format!("{} memory is empty.", capitalize(file_str)))
                } else {
                    Ok(content)
                }
            }
            "write" => {
                let content = args["content"]
                    .as_str()
                    .ok_or_else(|| McError::Tool("write requires 'content' field".to_string()))?;
                check_content_size(content)?;
                self.store.write_file(memory_file, content)?;
                Ok(format!("{} memory updated ({} bytes).", capitalize(file_str), content.len()))
            }
            "append" => {
                let content = args["content"]
                    .as_str()
                    .ok_or_else(|| McError::Tool("append requires 'content' field".to_string()))?;
                check_content_size(content)?;
                check_file_size_after_append(&self.store, memory_file, content)?;
                self.store.append_file(memory_file, content)?;
                Ok(format!(
                    "Appended to {} memory ({} bytes).",
                    file_str,
                    content.len()
                ))
            }
            "update_section" => {
                let heading = args["heading"]
                    .as_str()
                    .ok_or_else(|| {
                        McError::Tool("update_section requires 'heading' field".to_string())
                    })?;
                let content = args["content"]
                    .as_str()
                    .ok_or_else(|| {
                        McError::Tool("update_section requires 'content' field".to_string())
                    })?;
                check_content_size(content)?;
                let found = self.store.update_section(memory_file, heading, content)?;
                if found {
                    Ok(format!(
                        "Section '## {heading}' in {} memory updated.",
                        file_str
                    ))
                } else {
                    Ok(format!(
                        "Section '## {heading}' not found in {} memory. No changes made.",
                        file_str
                    ))
                }
            }
            other => Err(McError::Tool(format!(
                "unknown memory operation '{other}', expected: read, write, append, update_section"
            ))),
        }
    }
}

/// Validate that content does not exceed the single-write limit.
fn check_content_size(content: &str) -> Result<(), McError> {
    if content.len() > MAX_CONTENT_SIZE {
        return Err(McError::Tool(format!(
            "content exceeds maximum size ({} bytes > {} bytes limit)",
            content.len(),
            MAX_CONTENT_SIZE,
        )));
    }
    Ok(())
}

/// Validate that appending `content` won't push the file over the total limit.
fn check_file_size_after_append(
    store: &MemoryStore,
    file: MemoryFile,
    content: &str,
) -> Result<(), McError> {
    let existing = store.read_file(file)?;
    let new_total = existing.len() + content.len();
    if new_total > MAX_FILE_SIZE {
        return Err(McError::Tool(format!(
            "memory file would exceed maximum size after append ({} bytes > {} bytes limit)",
            new_total, MAX_FILE_SIZE,
        )));
    }
    Ok(())
}

/// Capitalize the first character of a string.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tool_with_temp() -> (TempDir, MemoryTool) {
        let dir = TempDir::new().unwrap();
        let store = MemoryStore::new(dir.path());
        let tool = MemoryTool::new(store);
        (dir, tool)
    }

    #[tokio::test]
    async fn read_empty_memory() {
        let (_dir, tool) = tool_with_temp();
        let result = tool
            .execute(json!({"operation": "read", "file": "agent"}))
            .await
            .unwrap();
        assert!(result.contains("empty"));
    }

    #[tokio::test]
    async fn write_and_read_agent_memory() {
        let (_dir, tool) = tool_with_temp();
        let write_result = tool
            .execute(json!({"operation": "write", "file": "agent", "content": "hello world"}))
            .await
            .unwrap();
        assert!(write_result.contains("updated"));

        let read_result = tool
            .execute(json!({"operation": "read", "file": "agent"}))
            .await
            .unwrap();
        assert_eq!(read_result, "hello world");
    }

    #[tokio::test]
    async fn append_to_memory() {
        let (_dir, tool) = tool_with_temp();
        tool.execute(json!({"operation": "write", "file": "user", "content": "line 1\n"}))
            .await
            .unwrap();

        let append_result = tool
            .execute(json!({"operation": "append", "file": "user", "content": "line 2"}))
            .await
            .unwrap();
        assert!(append_result.contains("Appended"));

        let content = tool
            .execute(json!({"operation": "read", "file": "user"}))
            .await
            .unwrap();
        assert!(content.contains("line 1"));
        assert!(content.contains("line 2"));
    }

    #[tokio::test]
    async fn update_section_in_memory() {
        let (_dir, tool) = tool_with_temp();
        tool.execute(json!({
            "operation": "write",
            "file": "agent",
            "content": "## Preferences\nold prefs\n## Notes\nnotes here\n"
        }))
        .await
        .unwrap();

        let update_result = tool
            .execute(json!({
                "operation": "update_section",
                "file": "agent",
                "heading": "Preferences",
                "content": "new preferences\n"
            }))
            .await
            .unwrap();
        assert!(update_result.contains("updated"));

        let content = tool
            .execute(json!({"operation": "read", "file": "agent"}))
            .await
            .unwrap();
        assert!(content.contains("new preferences"));
        assert!(content.contains("## Notes\nnotes here\n"));
    }

    #[tokio::test]
    async fn update_section_not_found() {
        let (_dir, tool) = tool_with_temp();
        tool.execute(json!({
            "operation": "write",
            "file": "agent",
            "content": "## Intro\nhello\n"
        }))
        .await
        .unwrap();

        let result = tool
            .execute(json!({
                "operation": "update_section",
                "file": "agent",
                "heading": "Missing",
                "content": "new\n"
            }))
            .await
            .unwrap();
        assert!(result.contains("not found"));
    }

    #[tokio::test]
    async fn invalid_file_rejected() {
        let (_dir, tool) = tool_with_temp();
        let result = tool
            .execute(json!({"operation": "read", "file": "invalid"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("invalid file"));
    }

    #[tokio::test]
    async fn invalid_operation_rejected() {
        let (_dir, tool) = tool_with_temp();
        let result = tool
            .execute(json!({"operation": "delete", "file": "agent"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unknown memory operation"));
    }

    #[tokio::test]
    async fn write_requires_content() {
        let (_dir, tool) = tool_with_temp();
        let result = tool
            .execute(json!({"operation": "write", "file": "agent"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("content"));
    }

    #[tokio::test]
    async fn update_section_requires_heading() {
        let (_dir, tool) = tool_with_temp();
        let result = tool
            .execute(json!({"operation": "update_section", "file": "agent", "content": "new"}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("heading"));
    }

    #[tokio::test]
    async fn tool_definition_is_valid() {
        let dir = TempDir::new().unwrap();
        let store = MemoryStore::new(dir.path());
        let tool = MemoryTool::new(store);
        let def = tool.definition();
        assert_eq!(def.name, "memory");
        assert!(!def.description.is_empty());
        assert!(def.parameters.is_object());
    }

    // ── Size limit tests ────────────────────────────────────────────

    #[tokio::test]
    async fn write_content_exceeds_max_size() {
        let (_dir, tool) = tool_with_temp();
        let big_content = "x".repeat(MAX_CONTENT_SIZE + 1);
        let result = tool
            .execute(json!({"operation": "write", "file": "agent", "content": big_content}))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("exceeds maximum size"), "unexpected error: {err_msg}");
    }

    #[tokio::test]
    async fn append_content_exceeds_max_size() {
        let (_dir, tool) = tool_with_temp();
        let big_content = "y".repeat(MAX_CONTENT_SIZE + 1);
        let result = tool
            .execute(json!({"operation": "append", "file": "agent", "content": big_content}))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exceeds maximum size"));
    }

    #[tokio::test]
    async fn append_file_total_exceeds_max_size() {
        let (_dir, tool) = tool_with_temp();
        // Build up a file near the limit using multiple appends (each under MAX_CONTENT_SIZE)
        let chunk = "a".repeat(MAX_CONTENT_SIZE);
        // Write first chunk
        tool.execute(json!({"operation": "write", "file": "agent", "content": &chunk}))
            .await
            .unwrap();
        // Append more chunks to approach the file size limit
        // We need ~10 chunks to approach MAX_FILE_SIZE (10 * 1MB = 10MB)
        for _ in 0..9 {
            let result = tool
                .execute(json!({"operation": "append", "file": "agent", "content": &chunk}))
                .await;
            // If we hit the file size limit, that's expected
            if result.is_err() {
                let err_msg = result.unwrap_err().to_string();
                assert!(
                    err_msg.contains("would exceed maximum size"),
                    "unexpected error: {err_msg}"
                );
                return; // test passed — we hit the file size limit
            }
        }
        // If we got here, the next append should trigger the limit
        let extra = "b".repeat(1024);
        let result = tool
            .execute(json!({"operation": "append", "file": "agent", "content": &extra}))
            .await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("would exceed maximum size"),
            "unexpected error: {err_msg}"
        );
    }

    #[tokio::test]
    async fn write_content_at_exact_limit_succeeds() {
        let (_dir, tool) = tool_with_temp();
        let exact_content = "z".repeat(MAX_CONTENT_SIZE);
        let result = tool
            .execute(json!({"operation": "write", "file": "agent", "content": &exact_content}))
            .await;
        assert!(result.is_ok());
        assert!(result.unwrap().contains("updated"));
    }
}
