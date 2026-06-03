# mc-tools

Tool registration, discovery, and execution for mavis-claw.

## Overview

- **ToolRegistry** — `HashMap<String, Arc<dyn Tool>>` with `register()`, `definitions()`, `execute()`
- **Built-in tools** — `bash`, `read_file`, `write_file`, `list_dir`, `glob`, `grep`

## Built-in Tools

| Struct | Name | Description |
|--------|------|-------------|
| `BashTool` | `bash` | Shell commands (PowerShell on Windows, `/bin/sh` on Unix) |
| `ReadFileTool` | `read_file` | Read file content with offset/limit |
| `WriteFileTool` | `write_file` | Write content (create/overwrite) |
| `ListDirTool` | `list_dir` | List directory contents |
| `GlobTool` | `glob` | Find files by glob pattern |
| `GrepTool` | `grep` | Substring search across files |

## Quick Start

```rust
use std::sync::Arc;
use mc_tools::{ToolRegistry, BashTool, ReadFileTool};
use serde_json::json;

// Register specific tools
let mut registry = ToolRegistry::new();
registry.register(Arc::new(BashTool::new()));
registry.register(Arc::new(ReadFileTool::new()));

// Or load all built-in tools at once
let registry = mc_tools::builtin_registry();

// Get OpenAI function-calling definitions
let defs = registry.definitions();

// Execute a tool
let output = registry.execute("bash", json!({"command": "echo hello"})).await?;
```

## Tool Parameters

### `bash`
- `command` (string, required) — shell command to execute
- `timeout_ms` (integer, optional) — timeout in ms, default 30 000

### `read_file`
- `path` (string, required) — absolute file path
- `offset` (integer, optional) — start line (1-indexed, default 1)
- `limit` (integer, optional) — max lines to read

### `write_file`
- `path` (string, required) — absolute file path
- `content` (string, required) — content to write

### `list_dir`
- `path` (string, required) — directory path

### `glob`
- `pattern` (string, required) — glob pattern, e.g. `**/*.rs`
- `root` (string, optional) — search root, default cwd

### `grep`
- `pattern` (string, required) — text to search for
- `path` (string, required) — file or directory
- `include` (string, optional) — filename glob filter, e.g. `*.rs`

## Glob Syntax

- `*` — any characters except `/`
- `?` — exactly one character
- `**` — any number of path components
