use async_trait::async_trait;
use mc_core::{McError, Tool, ToolDefinition};
use serde_json::{json, Value};
use tokio::time::{timeout, Duration};

use crate::security::SecurityConfig;

const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Execute shell commands — PowerShell on Windows, `/bin/sh` on Unix.
///
/// Commands are checked against a configurable dangerous-command blacklist
/// before execution.
pub struct BashTool {
    security: SecurityConfig,
}

impl BashTool {
    pub fn new(security: SecurityConfig) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".into(),
            description: "Execute a shell command and return its output. \
                         Uses PowerShell on Windows, /bin/sh on Unix."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to execute"
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Timeout in milliseconds (default 30000)"
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, args: Value) -> Result<String, McError> {
        let command = args["command"]
            .as_str()
            .ok_or_else(|| McError::Tool("Missing required parameter: command".into()))?;

        // ── Security: check against dangerous command blacklist ──────
        self.security.check_command(command)?;

        let timeout_ms = args["timeout_ms"].as_u64().unwrap_or(DEFAULT_TIMEOUT_MS);

        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = tokio::process::Command::new("powershell.exe");
            c.args(["-NoProfile", "-NonInteractive", "-Command", command]);
            c
        } else {
            let mut c = tokio::process::Command::new("/bin/sh");
            c.args(["-c", command]);
            c
        };

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        let child = cmd
            .spawn()
            .map_err(|e| McError::Tool(format!("Failed to spawn process: {e}")))?;

        let result = timeout(Duration::from_millis(timeout_ms), child.wait_with_output()).await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                if output.status.success() {
                    Ok(stdout.to_string())
                } else {
                    let code = output.status.code().unwrap_or(-1);
                    Ok(format!("Exit code: {code}\n{stdout}{stderr}"))
                }
            }
            Ok(Err(e)) => Err(McError::Tool(format!("Process error: {e}"))),
            Err(_) => Err(McError::Tool(format!(
                "Command timed out after {timeout_ms}ms"
            ))),
        }
    }
}
