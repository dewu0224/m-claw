use async_trait::async_trait;
use mc_core::{McError, Tool, ToolDefinition};
use serde_json::{json, Value};

use crate::matches_glob;
use crate::security::SecurityConfig;

/// Search for plain-text matches in files.
///
/// Search path is validated against path-traversal policy before access.
pub struct GrepTool {
    security: SecurityConfig,
}

impl GrepTool {
    pub fn new(security: SecurityConfig) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "grep".into(),
            description: "Search for a text pattern in files (simple substring match). \
                         Returns matching lines with file path and line number."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Text to search for (substring match)"
                    },
                    "path": {
                        "type": "string",
                        "description": "File or directory to search in"
                    },
                    "include": {
                        "type": "string",
                        "description": "Glob to filter filenames, e.g. '*.rs'"
                    }
                },
                "required": ["pattern", "path"]
            }),
        }
    }

    async fn execute(&self, args: Value) -> Result<String, McError> {
        let pattern = args["pattern"]
            .as_str()
            .ok_or_else(|| McError::Tool("Missing required parameter: pattern".into()))?;

        let path = args["path"]
            .as_str()
            .ok_or_else(|| McError::Tool("Missing required parameter: path".into()))?;

        let include = args["include"].as_str();

        // ── Security: validate search path ───────────────────────────
        let path_buf = std::path::PathBuf::from(path);
        if path_buf.is_dir() {
            self.security.validate_dir_path(path)?;
        } else {
            self.security.validate_path(path)?;
        }

        let mut results: Vec<String> = Vec::new();

        if path_buf.is_file() {
            search_file(&path_buf, pattern, &mut results).await;
        } else if path_buf.is_dir() {
            let mut stack = vec![path_buf];
            while let Some(dir) = stack.pop() {
                let mut entries = match tokio::fs::read_dir(&dir).await {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                while let Some(entry) = entries.next_entry().await? {
                    let p = entry.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else {
                        let should = match include {
                            Some(inc) => {
                                let name = p
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy();
                                matches_glob(inc, &name)
                            }
                            None => true,
                        };
                        if should {
                            search_file(&p, pattern, &mut results).await;
                        }
                    }
                }
            }
        } else {
            return Err(McError::Tool(format!("Path does not exist: {path}")));
        }

        if results.is_empty() {
            Ok("No matches found.".into())
        } else {
            Ok(results.join("\n"))
        }
    }
}

/// Search a single file for `pattern`, pushing matching lines into `results`.
async fn search_file(path: &std::path::Path, pattern: &str, results: &mut Vec<String>) {
    let content = match tokio::fs::read_to_string(path).await {
        Ok(c) => c,
        Err(_) => return, // skip unreadable files
    };
    let display = path.to_string_lossy();
    for (i, line) in content.lines().enumerate() {
        if line.contains(pattern) {
            results.push(format!("{display}:{}: {line}", i + 1));
        }
    }
}
