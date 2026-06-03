//! Security guards for built-in tools.
//!
//! Centralizes path-traversal protection and dangerous-command detection,
//! driven by [`ToolsConfig`] so the policy is end-user configurable.

use std::path::PathBuf;

use mc_config::ToolsConfig;
use mc_core::McError;

/// Strip trailing path separators (`/` or `\`) from a `PathBuf`.
///
/// Windows `canonicalize()` on directories returns paths with trailing `\`,
/// which breaks `starts_with` checks. This normalizes them.
fn strip_trailing_sep(mut p: PathBuf) -> PathBuf {
    // Repeatedly strip trailing separator
    loop {
        let s = p.to_string_lossy();
        if s.ends_with('/') || s.ends_with('\\') {
            // Pop the last character
            let trimmed = &s[..s.len() - 1];
            p = PathBuf::from(trimmed);
        } else {
            break;
        }
    }
    p
}

/// Extracted, validated security policy derived from [`ToolsConfig`].
///
/// Stored inside each tool at construction time — immutable after creation.
#[derive(Debug, Clone)]
pub struct SecurityConfig {
    /// Pre-resolved allowed directory prefixes (canonicalized at init).
    allowed_dirs: Vec<PathBuf>,
    /// Whether to block `..` in raw user input before resolution.
    deny_path_traversal: bool,
    /// Lowercased dangerous command substrings.
    dangerous_commands: Vec<String>,
}

impl SecurityConfig {
    /// Build from the application's [`ToolsConfig`].
    ///
    /// Canonicalizes `allowed_paths` relative to `cwd`. If a path is `"."`,
    /// it resolves to `cwd` itself. Trailing path separators are stripped
    /// to ensure reliable `starts_with` checks on all platforms.
    pub fn from_tools_config(config: &ToolsConfig, cwd: &std::path::Path) -> Self {
        let allowed_dirs: Vec<PathBuf> = config
            .allowed_paths
            .iter()
            .map(|p| {
                let joined = if p == "." {
                    cwd.to_path_buf()
                } else {
                    cwd.join(p)
                };
                strip_trailing_sep(joined.canonicalize().unwrap_or(joined))
            })
            .collect();

        Self {
            allowed_dirs,
            deny_path_traversal: config.deny_path_traversal,
            dangerous_commands: config
                .bash_dangerous_commands
                .iter()
                .map(|c| c.to_lowercase())
                .collect(),
        }
    }

    /// Validate a filesystem path for read or write access.
    ///
    /// Returns `Ok(canonical_path)` on success, `Err(McError::Tool(_))` on
    /// policy violation.
    pub fn validate_path(&self, raw: &str) -> Result<PathBuf, McError> {
        // 1. Reject obvious traversal in raw input
        if self.deny_path_traversal && raw.contains("..") {
            return Err(McError::Tool(format!(
                "Path traversal rejected: '{raw}' contains '..'"
            )));
        }

        // 2. Canonicalize to resolve any `.` / symlinks
        let path = PathBuf::from(raw);
        let canonical = path
            .canonicalize()
            .map_err(|e| McError::Tool(format!("Path not found or inaccessible: {raw} ({e})")))?;

        // 3. Security check: must be a file (not a directory)
        if !canonical.is_file() {
            return Err(McError::Tool(format!(
                "Path is not a regular file: {raw}"
            )));
        }

        // 4. Check against allowed directories
        if !self.allowed_dirs.is_empty() {
            let canon_normalized = strip_trailing_sep(canonical.clone());
            let allowed = self
                .allowed_dirs
                .iter()
                .any(|dir| canon_normalized.starts_with(dir));
            if !allowed {
                return Err(McError::Tool(format!(
                    "Access denied: '{raw}' is outside allowed directories"
                )));
            }
        }

        Ok(canonical)
    }

    /// Validate a filesystem path for directory access (list_dir, glob, grep).
    ///
    /// The path must resolve to an existing directory within allowed bounds.
    pub fn validate_dir_path(&self, raw: &str) -> Result<PathBuf, McError> {
        // 1. Reject obvious traversal in raw input
        if self.deny_path_traversal && raw.contains("..") {
            return Err(McError::Tool(format!(
                "Path traversal rejected: '{raw}' contains '..'"
            )));
        }

        // 2. Canonicalize to resolve any `.` / symlinks
        let path = PathBuf::from(raw);
        let canonical = path
            .canonicalize()
            .map_err(|e| McError::Tool(format!("Path not found or inaccessible: {raw} ({e})")))?;

        // 3. Must be a directory
        if !canonical.is_dir() {
            return Err(McError::Tool(format!(
                "Path is not a directory: {raw}"
            )));
        }

        // 4. Check against allowed directories
        if !self.allowed_dirs.is_empty() {
            let canon_normalized = strip_trailing_sep(canonical.clone());
            let allowed = self
                .allowed_dirs
                .iter()
                .any(|dir| canon_normalized.starts_with(dir));
            if !allowed {
                return Err(McError::Tool(format!(
                    "Access denied: '{raw}' is outside allowed directories"
                )));
            }
        }

        Ok(canonical)
    }

    /// Validate a filesystem path for write access (allows non-existent files
    /// whose parent directory exists or can be created).
    pub fn validate_write_path(&self, raw: &str) -> Result<PathBuf, McError> {
        // 1. Reject obvious traversal in raw input
        if self.deny_path_traversal && raw.contains("..") {
            return Err(McError::Tool(format!(
                "Path traversal rejected: '{raw}' contains '..'"
            )));
        }

        // 2. Canonicalize — try the file first, then the parent
        let path = PathBuf::from(raw);
        let canonical = if path.exists() {
            path.canonicalize()
                .map_err(|e| McError::Tool(format!("Cannot canonicalize '{raw}': {e}")))?
        } else {
            // File doesn't exist yet — canonicalize the parent
            let parent = match path.parent() {
                Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
                _ => PathBuf::from("."),
            };
            let canon_parent = parent
                .canonicalize()
                .map_err(|e| McError::Tool(format!("Parent directory not found: {raw} ({e})")))?;
            let file_name = path
                .file_name()
                .ok_or_else(|| McError::Tool(format!("Invalid path: '{raw}'")))?;
            canon_parent.join(file_name)
        };

        // 3. Check against allowed directories
        if !self.allowed_dirs.is_empty() {
            let canon_normalized = strip_trailing_sep(canonical.clone());
            let allowed = self
                .allowed_dirs
                .iter()
                .any(|dir| canon_normalized.starts_with(dir));
            if !allowed {
                return Err(McError::Tool(format!(
                    "Access denied: '{raw}' is outside allowed directories"
                )));
            }
        }

        Ok(canonical)
    }

    /// Check whether a shell command matches any dangerous pattern.
    ///
    /// Returns `Ok(())` if safe, `Err(McError::Tool(_))` if blocked.
    pub fn check_command(&self, command: &str) -> Result<(), McError> {
        if self.dangerous_commands.is_empty() {
            return Ok(());
        }

        let lower = command.to_lowercase();

        for pattern in &self.dangerous_commands {
            if lower.contains(pattern) {
                return Err(McError::Tool(format!(
                    "Blocked dangerous command: matches pattern '{pattern}'"
                )));
            }
        }

        Ok(())
    }

    /// Reject glob patterns containing `..` path traversal.
    ///
    /// Even though the root is validated separately, patterns like
    /// `../../*.conf` are a defense-in-depth concern.
    pub fn check_glob_pattern(&self, pattern: &str) -> Result<(), McError> {
        if self.deny_path_traversal && pattern.contains("..") {
            return Err(McError::Tool(format!(
                "Path traversal rejected: glob pattern '{pattern}' contains '..'"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn test_config() -> ToolsConfig {
        ToolsConfig::default()
    }

    fn security() -> SecurityConfig {
        let cwd = env::current_dir().expect("cwd");
        SecurityConfig::from_tools_config(&test_config(), &cwd)
    }

    // ── Path traversal tests ─────────────────────────────────────────

    #[test]
    fn reject_dotdot_in_raw_input() {
        let sec = security();
        let result = sec.validate_path("../etc/passwd");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("traversal"), "unexpected error: {err}");
    }

    #[test]
    fn reject_dotdot_in_middle_of_path() {
        let sec = security();
        let result = sec.validate_path("src/../../etc/passwd");
        assert!(result.is_err());
    }

    #[test]
    fn reject_dotdot_write_path() {
        let sec = security();
        let result = sec.validate_write_path("../evil.txt");
        assert!(result.is_err());
    }

    #[test]
    fn allow_relative_path_within_cwd() {
        let sec = security();
        // This file exists and is within cwd
        let result = sec.validate_path("Cargo.toml");
        assert!(result.is_ok(), "should allow Cargo.toml in cwd");
    }

    #[test]
    fn allow_relative_write_path_within_cwd() {
        let sec = security();
        // Non-existent file but parent is cwd
        let result = sec.validate_write_path("test_output.txt");
        assert!(result.is_ok(), "should allow writing to cwd/test_output.txt");
    }

    // ── Dangerous command tests ──────────────────────────────────────

    #[test]
    fn block_rm_rf_root() {
        let sec = security();
        assert!(sec.check_command("rm -rf /").is_err());
        assert!(sec.check_command("rm -rf /*").is_err());
        assert!(sec.check_command("sudo rm -rf / --no-preserve-root").is_err());
    }

    #[test]
    fn block_rm_rf_home() {
        let sec = security();
        assert!(sec.check_command("rm -rf ~").is_err());
    }

    #[test]
    fn block_fork_bomb() {
        let sec = security();
        assert!(sec.check_command(":(){ :|:& };:").is_err());
    }

    #[test]
    fn block_format_disk() {
        let sec = security();
        assert!(sec.check_command("format c: /y").is_err());
        assert!(sec.check_command("FORMAT D:").is_err());
    }

    #[test]
    fn block_dd_zero() {
        let sec = security();
        assert!(sec.check_command("dd if=/dev/zero of=/dev/sda").is_err());
    }

    #[test]
    fn block_del_force() {
        let sec = security();
        assert!(sec.check_command("del /f /s /q C:\\*.*").is_err());
    }

    #[test]
    fn block_rmdir_s() {
        let sec = security();
        assert!(sec.check_command("rmdir /s /q C:\\Windows").is_err());
    }

    #[test]
    fn block_format_slash() {
        let sec = security();
        assert!(sec.check_command("format /").is_err());
    }

    #[test]
    fn block_reg_delete() {
        let sec = security();
        assert!(sec.check_command("reg delete HKLM\\SOFTWARE").is_err());
    }

    #[test]
    fn block_diskpart() {
        let sec = security();
        assert!(sec.check_command("diskpart").is_err());
    }

    #[test]
    fn block_vssadmin_delete() {
        let sec = security();
        assert!(sec
            .check_command("vssadmin delete shadows /all /quiet")
            .is_err());
    }

    #[test]
    fn allow_safe_commands() {
        let sec = security();
        assert!(sec.check_command("ls -la").is_ok());
        assert!(sec.check_command("echo hello").is_ok());
        assert!(sec.check_command("cargo build").is_ok());
        assert!(sec.check_command("git status").is_ok());
        assert!(sec.check_command("cat file.txt").is_ok());
    }

    #[test]
    fn allow_rm_non_root() {
        let sec = security();
        // "rm -rf" without "/" target is fine
        assert!(sec.check_command("rm -rf ./build").is_ok());
        assert!(sec.check_command("rm -rf my_dir").is_ok());
    }

    // ── Edge-case patterns ───────────────────────────────────────────

    #[test]
    fn block_case_insensitive_format() {
        let sec = security();
        assert!(sec.check_command("FORMAT C:").is_err());
        assert!(sec.check_command("Format C:").is_err());
    }

    #[test]
    fn block_chained_dangerous() {
        let sec = security();
        // Command chaining: if any part matches, it's blocked
        assert!(sec.check_command("echo ok; rm -rf /").is_err());
    }

    #[test]
    fn deny_path_traversal_toggle() {
        let cwd = env::current_dir().unwrap();
        let mut config = test_config();
        config.deny_path_traversal = false;
        // When traversal check is off, ".." in raw input is not rejected
        // (canonicalize may still fail, but the explicit check is skipped)
        let sec = SecurityConfig::from_tools_config(&config, &cwd);
        // This should NOT fail with "traversal" error — it may fail for
        // other reasons (file not found) but the traversal guard is off.
        let result = sec.validate_path("../nonexistent.txt");
        match result {
            Err(McError::Tool(msg)) => assert!(
                !msg.contains("traversal"),
                "traversal check should be off: {msg}"
            ),
            _ => { /* ok — either succeeded or failed for other reasons */ }
        }
    }

    // ── Directory path validation tests ─────────────────────────────

    #[test]
    fn reject_dir_dotdot() {
        let sec = security();
        let result = sec.validate_dir_path("../../etc");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("traversal"));
    }

    #[test]
    fn allow_cwd_dir() {
        let sec = security();
        // "." resolves to cwd, which is a directory
        let result = sec.validate_dir_path(".");
        assert!(result.is_ok(), "should allow listing cwd");
    }

    #[test]
    fn allow_subdir_within_cwd() {
        let sec = security();
        // "src" is a subdirectory of the crate's cwd
        let result = sec.validate_dir_path("src");
        assert!(result.is_ok(), "should allow listing src/ subdir");
    }

    // ── Glob pattern validation tests ───────────────────────────────

    #[test]
    fn reject_glob_with_dotdot() {
        let sec = security();
        let result = sec.check_glob_pattern("../../*.conf");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("traversal"));
    }

    #[test]
    fn allow_normal_glob() {
        let sec = security();
        assert!(sec.check_glob_pattern("**/*.rs").is_ok());
        assert!(sec.check_glob_pattern("src/**/*.ts").is_ok());
        assert!(sec.check_glob_pattern("*.toml").is_ok());
    }

    #[test]
    fn reject_glob_dotdot_in_middle() {
        let sec = security();
        assert!(sec.check_glob_pattern("src/../../secret/*.key").is_err());
    }
}
