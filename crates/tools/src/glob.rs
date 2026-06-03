use async_trait::async_trait;
use mc_core::{McError, Tool, ToolDefinition};
use serde_json::{json, Value};

use crate::matches_glob;
use crate::security::SecurityConfig;

/// Find files matching a glob pattern.
///
/// Root directory and glob pattern are validated against security policy.
pub struct GlobTool {
    security: SecurityConfig,
}

impl GlobTool {
    pub fn new(security: SecurityConfig) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "glob".into(),
            description: "Find files matching a glob pattern. Supports *, ?, and **. \
                         Paths are returned relative to the root."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Glob pattern (e.g. '**/*.rs', 'src/**/*.ts')"
                    },
                    "root": {
                        "type": "string",
                        "description": "Root directory to search from (default: cwd)"
                    }
                },
                "required": ["pattern"]
            }),
        }
    }

    async fn execute(&self, args: Value) -> Result<String, McError> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| McError::Tool("Missing required parameter: pattern".into()))?;

        // ── Security: reject traversal in glob pattern ───────────────
        self.security.check_glob_pattern(pattern)?;

        let root = match args["root"].as_str() {
            Some(r) => {
                // ── Security: validate root directory ────────────────
                self.security.validate_dir_path(r)?
            }
            None => std::env::current_dir()
                .map_err(|e| McError::Tool(format!("Cannot determine cwd: {e}")))?,
        };

        let mut results = Vec::new();
        let mut stack = vec![root.clone()];

        while let Some(dir) = stack.pop() {
            let mut entries = match tokio::fs::read_dir(&dir).await {
                Ok(e) => e,
                Err(_) => continue,
            };
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let rel = path
                    .strip_prefix(&root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                if matches_glob(pattern, &rel) {
                    results.push(path.to_string_lossy().into_owned());
                }
                if path.is_dir() {
                    stack.push(path);
                }
            }
        }

        results.sort();
        Ok(results.join("\n"))
    }
}
