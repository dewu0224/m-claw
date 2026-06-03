use async_trait::async_trait;
use mc_core::{McError, Tool, ToolDefinition};
use serde_json::{json, Value};

use crate::security::SecurityConfig;

/// Read file content with optional line-offset/limit.
///
/// Paths are validated against path-traversal policy before access.
pub struct ReadFileTool {
    security: SecurityConfig,
}

impl ReadFileTool {
    pub fn new(security: SecurityConfig) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read the content of a file. Returns lines prefixed with \
                         their 1-indexed line numbers. Supports offset and limit."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file (relative to cwd or absolute)"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Line number to start reading from (1-indexed, default 1)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of lines to read (default: all)"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, args: Value) -> Result<String, McError> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| McError::Tool("Missing required parameter: path".into()))?;

        // ── Security: validate path against traversal policy ─────────
        let canonical = self.security.validate_path(path)?;

        let content = tokio::fs::read_to_string(&canonical).await?;

        let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
        let limit = args["limit"].as_u64().map(|l| l as usize);

        let lines: Vec<&str> = content.lines().collect();
        let start = offset.saturating_sub(1);
        if start >= lines.len() {
            return Ok(String::new());
        }
        let end = match limit {
            Some(l) => (start + l).min(lines.len()),
            None => lines.len(),
        };

        let result: Vec<String> = lines[start..end]
            .iter()
            .enumerate()
            .map(|(i, line)| format!("{}: {}", start + i + 1, line))
            .collect();

        Ok(result.join("\n"))
    }
}
