//! Tool registration, discovery, and execution.
//!
//! Provides [`ToolRegistry`] for managing tools and built-in tools
//! for common agent operations: shell commands, file I/O, directory
//! listing, glob matching, and text search.

mod bash;
mod glob;
mod grep;
mod list_dir;
mod memory;
mod read_file;
mod registry;
pub mod security;
mod write_file;

pub use bash::BashTool;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use list_dir::ListDirTool;
pub use memory::MemoryTool;
pub use read_file::ReadFileTool;
pub use registry::ToolRegistry;
pub use security::SecurityConfig;
pub use write_file::WriteFileTool;

use std::sync::Arc;

use mc_config::ToolsConfig;
use mc_memory::MemoryStore;

/// Create a registry pre-loaded with all built-in tools.
///
/// Tools are configured with the security policy from `config` (e.g.,
/// dangerous-command blacklist, path-traversal restrictions).
///
/// If `memory_store` is provided, a [`MemoryTool`] is also registered
/// for LLM-accessible memory CRUD operations.
pub fn builtin_registry(config: &ToolsConfig, memory_store: Option<MemoryStore>) -> ToolRegistry {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let security = SecurityConfig::from_tools_config(config, &cwd);

    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(BashTool::new(security.clone())));
    registry.register(Arc::new(ReadFileTool::new(security.clone())));
    registry.register(Arc::new(WriteFileTool::new(security.clone())));
    registry.register(Arc::new(ListDirTool::new(security.clone())));
    registry.register(Arc::new(GlobTool::new(security.clone())));
    registry.register(Arc::new(GrepTool::new(security)));

    if let Some(store) = memory_store {
        registry.register(Arc::new(MemoryTool::new(store)));
    }

    registry
}

// ── Shared glob-matching utilities ────────────────────────────────────

/// Normalize path separators to `/` for consistent glob matching.
pub(crate) fn normalize_sep(path: &str) -> String {
    path.replace('\\', "/")
}

/// Match `path` against a glob `pattern`.
///
/// Supports `*` (any chars except `/`), `?` (single char), and
/// `**` (any number of path components).
pub(crate) fn matches_glob(pattern: &str, path: &str) -> bool {
    let p = normalize_sep(pattern);
    let s = normalize_sep(path);
    let p_segs: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    let s_segs: Vec<&str> = s.split('/').filter(|s| !s.is_empty()).collect();
    match_segments(&p_segs, &s_segs)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    if pattern.is_empty() {
        return path.is_empty();
    }
    if pattern[0] == "**" {
        // `**` matches zero or more components
        if match_segments(&pattern[1..], path) {
            return true;
        }
        return !path.is_empty() && match_segments(pattern, &path[1..]);
    }
    if path.is_empty() {
        return false;
    }
    if match_segment(pattern[0], path[0]) {
        return match_segments(&pattern[1..], &path[1..]);
    }
    false
}

/// Match a single path component against a single pattern component.
/// Supports `*` (any chars) and `?` (single char).
fn match_segment(pattern: &str, segment: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = segment.chars().collect();
    let (mut pi, mut si) = (0usize, 0usize);
    let mut star_pi: Option<usize> = None;
    let mut star_si = 0usize;

    while si < s.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == s[si]) {
            pi += 1;
            si += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star_pi = Some(pi);
            star_si = si;
            pi += 1;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_si += 1;
            si = star_si;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_exact() {
        assert!(matches_glob("foo.rs", "foo.rs"));
        assert!(!matches_glob("foo.rs", "bar.rs"));
    }

    #[test]
    fn glob_star() {
        assert!(matches_glob("*.rs", "foo.rs"));
        assert!(!matches_glob("*.rs", "foo.ts"));
        assert!(!matches_glob("*.rs", "dir/foo.rs")); // * doesn't cross /
    }

    #[test]
    fn glob_question() {
        assert!(matches_glob("?.rs", "a.rs"));
        assert!(!matches_glob("?.rs", "ab.rs"));
    }

    #[test]
    fn glob_double_star() {
        assert!(matches_glob("**/*.rs", "foo/bar.rs"));
        assert!(matches_glob("**/*.rs", "bar.rs"));
        assert!(matches_glob("**", "a/b/c"));
    }

    #[test]
    fn glob_double_star_prefix() {
        assert!(matches_glob("src/**", "src/foo/bar.rs"));
        assert!(!matches_glob("src/**", "lib/foo.rs"));
    }

    // ── builtin_registry integration tests ──────────────────────────

    #[test]
    fn builtin_registry_without_memory_has_no_memory_tool() {
        let config = mc_config::ToolsConfig::default();
        let registry = builtin_registry(&config, None);
        let defs = registry.definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(
            !names.contains(&"memory"),
            "should NOT have memory tool when no store passed, got: {names:?}"
        );
        // Should still have the 6 built-in tools
        assert!(names.contains(&"bash"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"list_dir"));
        assert!(names.contains(&"glob"));
        assert!(names.contains(&"grep"));
    }

    #[test]
    fn builtin_registry_with_memory_registers_memory_tool() {
        let config = mc_config::ToolsConfig::default();
        let tmp = tempfile::TempDir::new().unwrap();
        let store = mc_memory::MemoryStore::new(tmp.path());
        let registry = builtin_registry(&config, Some(store));
        let defs = registry.definitions();
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        assert!(
            names.contains(&"memory"),
            "should have memory tool when store passed, got: {names:?}"
        );
        // Should have 7 tools total (6 built-in + memory)
        assert_eq!(defs.len(), 7, "expected 7 tools, got {}", defs.len());
    }

    #[tokio::test]
    async fn builtin_registry_memory_tool_is_executable() {
        let config = mc_config::ToolsConfig::default();
        let tmp = tempfile::TempDir::new().unwrap();
        let store = mc_memory::MemoryStore::new(tmp.path());
        let registry = builtin_registry(&config, Some(store));

        // Execute a write then read through the registry
        let write_result = registry
            .execute(
                "memory",
                serde_json::json!({"operation": "write", "file": "agent", "content": "hello integration"}),
            )
            .await
            .unwrap();
        assert!(write_result.contains("updated"));

        let read_result = registry
            .execute(
                "memory",
                serde_json::json!({"operation": "read", "file": "agent"}),
            )
            .await
            .unwrap();
        assert_eq!(read_result, "hello integration");
    }
}
