//! MemoryStore — CRUD operations for MEMORY.md and USER.md.

use std::fs;
use std::path::{Path, PathBuf};

use mc_core::McError;

use crate::types::MemoryFile;

/// Persistent store for agent memory (`MEMORY.md`) and user memory (`USER.md`).
///
/// All files live under `base_path`. Reading a non-existent file returns
/// an empty string; appending creates the file if needed.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    base_path: PathBuf,
}

impl MemoryStore {
    /// Create a new store rooted at `base_path`.
    ///
    /// The directory is created automatically on first write if it doesn't exist.
    pub fn new(base_path: impl Into<PathBuf>) -> Self {
        Self {
            base_path: base_path.into(),
        }
    }

    /// Returns the base directory path.
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Resolve the full path for a given memory file type.
    fn path_for(&self, file: MemoryFile) -> PathBuf {
        self.base_path.join(file.filename())
    }

    // ── Agent memory (MEMORY.md) ────────────────────────────────────────

    /// Read agent memory. Returns empty string if file doesn't exist.
    pub fn read_agent_memory(&self) -> Result<String, McError> {
        self.read_file(MemoryFile::Agent)
    }

    /// Append content to agent memory. Creates the file if it doesn't exist.
    pub fn append_agent_memory(&self, content: &str) -> Result<(), McError> {
        self.append_file(MemoryFile::Agent, content)
    }

    // ── User memory (USER.md) ───────────────────────────────────────────

    /// Read user memory. Returns empty string if file doesn't exist.
    pub fn read_user_memory(&self) -> Result<String, McError> {
        self.read_file(MemoryFile::User)
    }

    /// Append content to user memory. Creates the file if it doesn't exist.
    pub fn append_user_memory(&self, content: &str) -> Result<(), McError> {
        self.append_file(MemoryFile::User, content)
    }

    // ── Generic read / write ────────────────────────────────────────────

    /// Read the contents of a memory file. Returns empty string if missing.
    pub fn read_file(&self, file: MemoryFile) -> Result<String, McError> {
        let path = self.path_for(file);
        match fs::read_to_string(&path) {
            Ok(content) => Ok(content),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(McError::Io(e)),
        }
    }

    /// Append content to a memory file. Creates the file (and parent dirs)
    /// if they don't exist.
    pub fn append_file(&self, file: MemoryFile, content: &str) -> Result<(), McError> {
        self.ensure_dir()?;
        let path = self.path_for(file);
        let mut existing = self.read_file(file)?;
        if !existing.is_empty() && !existing.ends_with('\n') {
            existing.push('\n');
        }
        existing.push_str(content);
        if !existing.ends_with('\n') {
            existing.push('\n');
        }
        fs::write(&path, existing)?;
        Ok(())
    }

    /// Write raw content to a memory file, overwriting existing content.
    pub fn write_file(&self, file: MemoryFile, content: &str) -> Result<(), McError> {
        self.ensure_dir()?;
        let path = self.path_for(file);
        fs::write(&path, content)?;
        Ok(())
    }

    // ── Section operations ──────────────────────────────────────────────

    /// Update a section identified by `## heading`.
    ///
    /// Finds the line starting with `## {heading}` and replaces everything
    /// from the next line until the next `## ` heading (or end of file)
    /// with `new_content`.
    ///
    /// Returns `true` if the section was found and updated, `false` if not found.
    pub fn update_section(
        &self,
        file: MemoryFile,
        heading: &str,
        new_content: &str,
    ) -> Result<bool, McError> {
        let original = self.read_file(file)?;
        let updated = match replace_section(&original, heading, new_content) {
            Some(content) => content,
            None => return Ok(false),
        };
        self.write_file(file, &updated)?;
        Ok(true)
    }

    /// Remove a section identified by `## heading`.
    ///
    /// Finds the line starting with `## {heading}` and removes everything
    /// from that line until the next `## ` heading (or end of file).
    ///
    /// Returns `true` if the section was found and removed, `false` if not found.
    pub fn remove_section(
        &self,
        file: MemoryFile,
        heading: &str,
    ) -> Result<bool, McError> {
        let original = self.read_file(file)?;
        let updated = match remove_section_from_text(&original, heading) {
            Some(content) => content,
            None => return Ok(false),
        };
        self.write_file(file, &updated)?;
        Ok(true)
    }

    // ── Internal helpers ────────────────────────────────────────────────

    /// Ensure the base directory exists.
    fn ensure_dir(&self) -> Result<(), McError> {
        if !self.base_path.exists() {
            fs::create_dir_all(&self.base_path)?;
        }
        Ok(())
    }
}

// ── Pure section-parsing functions ──────────────────────────────────────

/// Find the line index where `## heading` starts (exact match after `## `).
fn find_section_start(lines: &[&str], heading: &str) -> Option<usize> {
    let prefix = format!("## {heading}");
    lines.iter().position(|line| line.trim_end() == prefix)
}

/// Find the line index where the next `## ` section starts after `start`.
fn find_section_end(lines: &[&str], start: usize) -> usize {
    // Skip the heading line itself
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim_end().starts_with("## ") {
            return i;
        }
    }
    lines.len()
}

/// Replace a section's body (everything between the heading and the next `## `).
///
/// Returns `None` if the heading is not found.
fn replace_section(text: &str, heading: &str, new_content: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = find_section_start(&lines, heading)?;
    let end = find_section_end(&lines, start);

    let mut result = String::new();
    // Keep everything before the heading
    for line in &lines[..start] {
        result.push_str(line);
        result.push('\n');
    }
    // Write the heading + new content
    result.push_str(&format!("## {heading}\n"));
    result.push_str(new_content);
    if !new_content.ends_with('\n') {
        result.push('\n');
    }
    // Keep everything after the section
    for line in &lines[end..] {
        result.push_str(line);
        result.push('\n');
    }
    Some(result)
}

/// Remove a section entirely (heading + body).
///
/// Returns `None` if the heading is not found.
fn remove_section_from_text(text: &str, heading: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let start = find_section_start(&lines, heading)?;
    let end = find_section_end(&lines, start);

    let mut result = String::new();
    for line in lines.iter().take(start).chain(lines.iter().skip(end)) {
        result.push_str(line);
        result.push('\n');
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_store() -> (TempDir, MemoryStore) {
        let dir = TempDir::new().unwrap();
        let store = MemoryStore::new(dir.path());
        (dir, store)
    }

    // ── Read / Write basics ─────────────────────────────────────────

    #[test]
    fn test_read_nonexistent_returns_empty() {
        let (_dir, store) = temp_store();
        assert_eq!(store.read_agent_memory().unwrap(), "");
        assert_eq!(store.read_user_memory().unwrap(), "");
    }

    #[test]
    fn test_append_creates_file_and_reads_back() {
        let (_dir, store) = temp_store();
        store.append_agent_memory("hello world").unwrap();
        assert_eq!(store.read_agent_memory().unwrap(), "hello world\n");

        store.append_user_memory("user info").unwrap();
        assert_eq!(store.read_user_memory().unwrap(), "user info\n");
    }

    #[test]
    fn test_append_multiple_times_accumulates() {
        let (_dir, store) = temp_store();
        store.append_agent_memory("line 1").unwrap();
        store.append_agent_memory("line 2").unwrap();
        let content = store.read_agent_memory().unwrap();
        assert!(content.contains("line 1"));
        assert!(content.contains("line 2"));
    }

    #[test]
    fn test_write_overwrites() {
        let (_dir, store) = temp_store();
        store.append_agent_memory("old content").unwrap();
        store.write_file(MemoryFile::Agent, "new content\n").unwrap();
        assert_eq!(store.read_agent_memory().unwrap(), "new content\n");
    }

    // ── UTF-8 / Chinese content ────────────────────────────────────

    #[test]
    fn test_chinese_content_roundtrip() {
        let (_dir, store) = temp_store();
        store.append_agent_memory("用户偏好：中文交流").unwrap();
        let content = store.read_agent_memory().unwrap();
        assert!(content.contains("用户偏好：中文交流"));
    }

    // ── Section update ──────────────────────────────────────────────

    #[test]
    fn test_update_section_existing() {
        let (_dir, store) = temp_store();
        store
            .write_file(
                MemoryFile::Agent,
                "## Intro\nold intro text\n## Notes\nsome notes\n",
            )
            .unwrap();

        let found = store
            .update_section(MemoryFile::Agent, "Intro", "new intro text")
            .unwrap();
        assert!(found);

        let content = store.read_agent_memory().unwrap();
        assert!(content.contains("## Intro\nnew intro text\n"));
        assert!(content.contains("## Notes\nsome notes\n"));
    }

    #[test]
    fn test_update_section_not_found() {
        let (_dir, store) = temp_store();
        store
            .write_file(MemoryFile::Agent, "## Intro\ntext\n")
            .unwrap();

        let found = store
            .update_section(MemoryFile::Agent, "Missing", "new")
            .unwrap();
        assert!(!found);
    }

    #[test]
    fn test_update_section_preserves_surrounding() {
        let (_dir, store) = temp_store();
        store
            .write_file(
                MemoryFile::Agent,
                "header line\n## A\naaa\n## B\nbbb\nfooter\n",
            )
            .unwrap();

        store
            .update_section(MemoryFile::Agent, "A", "AAA\n")
            .unwrap();
        let content = store.read_agent_memory().unwrap();
        assert!(content.starts_with("header line\n"));
        assert!(content.contains("## A\nAAA\n"));
        assert!(content.contains("## B\nbbb\n"));
        assert!(content.contains("footer\n"));
    }

    // ── Section remove ─────────────────────────────────────────────

    #[test]
    fn test_remove_section_existing() {
        let (_dir, store) = temp_store();
        store
            .write_file(
                MemoryFile::Agent,
                "## Keep\nkeep this\n## Remove\nremove this\n## Also Keep\nkeep\n",
            )
            .unwrap();

        let found = store
            .remove_section(MemoryFile::Agent, "Remove")
            .unwrap();
        assert!(found);

        let content = store.read_agent_memory().unwrap();
        assert!(content.contains("## Keep\nkeep this\n"));
        assert!(content.contains("## Also Keep\nkeep\n"));
        assert!(!content.contains("Remove this"));
    }

    #[test]
    fn test_remove_section_not_found() {
        let (_dir, store) = temp_store();
        store
            .write_file(MemoryFile::Agent, "## Intro\ntext\n")
            .unwrap();

        let found = store
            .remove_section(MemoryFile::Agent, "Ghost")
            .unwrap();
        assert!(!found);
    }

    #[test]
    fn test_remove_last_section() {
        let (_dir, store) = temp_store();
        store
            .write_file(MemoryFile::Agent, "## First\naaa\n## Last\nbbb\n")
            .unwrap();

        store
            .remove_section(MemoryFile::Agent, "Last")
            .unwrap();
        let content = store.read_agent_memory().unwrap();
        assert!(content.contains("## First\naaa\n"));
        assert!(!content.contains("bbb"));
    }

    // ── Path helpers ───────────────────────────────────────────────

    #[test]
    fn test_path_for() {
        let store = MemoryStore::new("/tmp/test");
        assert_eq!(
            store.path_for(MemoryFile::Agent),
            PathBuf::from("/tmp/test/MEMORY.md")
        );
        assert_eq!(
            store.path_for(MemoryFile::User),
            PathBuf::from("/tmp/test/USER.md")
        );
    }

    // ── Pure function tests ────────────────────────────────────────

    #[test]
    fn test_replace_section_multiline_body() {
        let text = "## Config\nkey1: val1\nkey2: val2\n## Other\nstuff\n";
        let result = replace_section(text, "Config", "key1: new\n").unwrap();
        assert_eq!(
            result,
            "## Config\nkey1: new\n## Other\nstuff\n"
        );
    }

    #[test]
    fn test_remove_section_from_text() {
        let text = "## A\naaa\n## B\nbbb\n";
        let result = remove_section_from_text(text, "A").unwrap();
        assert_eq!(result, "## B\nbbb\n");
    }
}
