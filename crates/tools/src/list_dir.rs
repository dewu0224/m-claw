use async_trait::async_trait;
use mc_core::{McError, Tool, ToolDefinition};
use serde_json::{json, Value};

use crate::security::SecurityConfig;

/// List directory contents.
///
/// Path is validated against path-traversal policy before access.
pub struct ListDirTool {
    security: SecurityConfig,
}

impl ListDirTool {
    pub fn new(security: SecurityConfig) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_dir".into(),
            description: "List the contents of a directory. Directories are shown \
                         with a trailing slash."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path to list"
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
        let canonical = self.security.validate_dir_path(path)?;

        let mut entries = tokio::fs::read_dir(&canonical).await?;
        let mut items: Vec<String> = Vec::new();

        while let Some(entry) = entries.next_entry().await? {
            let ft = entry.file_type().await?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if ft.is_dir() {
                items.push(format!("{name}/"));
            } else {
                items.push(name);
            }
        }

        items.sort();
        Ok(items.join("\n"))
    }
}
