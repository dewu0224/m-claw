use async_trait::async_trait;
use mc_core::{McError, Tool, ToolDefinition};
use serde_json::{json, Value};

use crate::security::SecurityConfig;

/// Write content to a file — creates or overwrites.
///
/// Paths are validated against path-traversal policy before access.
pub struct WriteFileTool {
    security: SecurityConfig,
}

impl WriteFileTool {
    pub fn new(security: SecurityConfig) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_file".into(),
            description: "Write content to a file. Creates the file if it doesn't exist, \
                         overwrites if it does. Parent directories are created automatically."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file (relative to cwd or absolute)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write to the file"
                    }
                },
                "required": ["path", "content"]
            }),
        }
    }

    async fn execute(&self, args: Value) -> Result<String, McError> {
        let path = args["path"]
            .as_str()
            .ok_or_else(|| McError::Tool("Missing required parameter: path".into()))?;

        let content = args["content"]
            .as_str()
            .ok_or_else(|| McError::Tool("Missing required parameter: content".into()))?;

        // ── Security: validate path against traversal policy ─────────
        let canonical = self.security.validate_write_path(path)?;

        if let Some(parent) = canonical.parent() {
            if !parent.as_os_str().is_empty() {
                tokio::fs::create_dir_all(parent).await?;
            }
        }

        tokio::fs::write(&canonical, content).await?;
        Ok(format!("Wrote {} bytes to {}", content.len(), canonical.display()))
    }
}
