//! SkillManager — lifecycle operations for skills on disk.

use std::fs;
use std::path::{Path, PathBuf};

use mc_core::McError;
use mc_memory::MemoryStore;

use crate::types::{SkillPatch, SkillProvenance, UsageOperation};
use crate::usage::UsageTracker;

/// Manages skill CRUD operations on disk with provenance and usage tracking.
///
/// Each skill lives at `<skills_dir>/<name>/SKILL.md`.
/// All mutations are recorded to `<skills_dir>/.usage.json`.
#[derive(Debug, Clone)]
pub struct SkillManager {
    skills_dir: PathBuf,
    memory_store: MemoryStore,
}

impl SkillManager {
    /// Create a new SkillManager.
    ///
    /// - `skills_dir`: root directory where each skill lives in a subdirectory.
    /// - `memory_store`: for potential future integration with memory nudges.
    pub fn new(skills_dir: impl Into<PathBuf>, memory_store: MemoryStore) -> Self {
        Self {
            skills_dir: skills_dir.into(),
            memory_store,
        }
    }

    /// Return the skills directory path.
    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }

    /// Return a reference to the underlying memory store.
    pub fn memory_store(&self) -> &MemoryStore {
        &self.memory_store
    }

    // ── CRUD operations ─────────────────────────────────────────────

    /// Create a new skill at `<skills_dir>/<name>/SKILL.md`.
    ///
    /// Returns `Err` if the skill directory already exists.
    pub fn create_skill(
        &self,
        name: &str,
        content: &str,
        provenance: SkillProvenance,
    ) -> Result<PathBuf, McError> {
        let skill_dir = self.skill_dir(name);
        if skill_dir.exists() {
            return Err(McError::Skill(format!(
                "skill '{name}' already exists at {}",
                skill_dir.display()
            )));
        }
        fs::create_dir_all(&skill_dir)?;
        let path = skill_dir.join("SKILL.md");
        fs::write(&path, content)?;

        self.record_usage(name, UsageOperation::Create, provenance)?;
        Ok(path)
    }

    /// Replace the entire content of an existing skill's SKILL.md.
    ///
    /// Returns `Err` if the skill does not exist.
    pub fn edit_skill(
        &self,
        name: &str,
        content: &str,
        provenance: SkillProvenance,
    ) -> Result<PathBuf, McError> {
        let path = self.require_skill(name)?;
        fs::write(&path, content)?;

        self.record_usage(name, UsageOperation::Edit, provenance)?;
        Ok(path)
    }

    /// Apply a partial patch to an existing skill.
    ///
    /// The patch can selectively update trigger words, description, or content
    /// without replacing the entire file. Fields set to `None` are left unchanged.
    ///
    /// Returns `Err` if the skill does not exist.
    pub fn patch_skill(
        &self,
        name: &str,
        patch: &SkillPatch,
        provenance: SkillProvenance,
    ) -> Result<PathBuf, McError> {
        let path = self.require_skill(name)?;
        let raw = fs::read_to_string(&path)?;
        let patched = apply_patch(&raw, patch)?;
        fs::write(&path, patched)?;

        self.record_usage(name, UsageOperation::Patch, provenance)?;
        Ok(path)
    }

    /// Delete a skill by removing its directory and all contents.
    ///
    /// Returns `Err` if the skill does not exist.
    pub fn delete_skill(
        &self,
        name: &str,
        provenance: SkillProvenance,
    ) -> Result<(), McError> {
        let skill_dir = self.skill_dir(name);
        if !skill_dir.exists() {
            return Err(McError::Skill(format!(
                "skill '{name}' not found at {}",
                skill_dir.display()
            )));
        }
        fs::remove_dir_all(&skill_dir)?;

        self.record_usage(name, UsageOperation::Delete, provenance)?;
        Ok(())
    }

    /// Check whether a skill exists on disk.
    pub fn skill_exists(&self, name: &str) -> bool {
        self.skill_dir(name).join("SKILL.md").exists()
    }

    /// Read the raw content of a skill's SKILL.md.
    pub fn read_skill(&self, name: &str) -> Result<String, McError> {
        let path = self.require_skill(name)?;
        Ok(fs::read_to_string(path)?)
    }

    /// List all skill names (subdirectory names that contain a SKILL.md).
    pub fn list_skills(&self) -> Result<Vec<String>, McError> {
        if !self.skills_dir.exists() {
            return Ok(Vec::new());
        }
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.skills_dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() && entry.path().join("SKILL.md").exists() {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(name.to_string());
                }
            }
        }
        names.sort();
        Ok(names)
    }

    // ── Internal helpers ─────────────────────────────────────────────

    /// Resolve the directory path for a skill.
    fn skill_dir(&self, name: &str) -> PathBuf {
        self.skills_dir.join(name)
    }

    /// Resolve the SKILL.md path and verify it exists.
    fn require_skill(&self, name: &str) -> Result<PathBuf, McError> {
        let path = self.skill_dir(name).join("SKILL.md");
        if !path.exists() {
            return Err(McError::Skill(format!("skill '{name}' not found")));
        }
        Ok(path)
    }

    /// Record a usage event.
    fn record_usage(
        &self,
        name: &str,
        operation: UsageOperation,
        provenance: SkillProvenance,
    ) -> Result<(), McError> {
        let tracker = UsageTracker::new(&self.skills_dir);
        tracker.record(name, operation, provenance)
    }
}

// ── Patch application (pure functions) ──────────────────────────────

/// Apply a `SkillPatch` to the raw SKILL.md content.
///
/// Returns the patched content. Only non-`None` fields are modified.
fn apply_patch(raw: &str, patch: &SkillPatch) -> Result<String, McError> {
    let mut result = raw.to_string();

    if let Some(ref trigger_words) = patch.trigger_words {
        result = replace_frontmatter_field(&result, "trigger_words", &format_yaml_list(trigger_words))?;
    }

    if let Some(ref description) = patch.description {
        result = replace_description_line(&result, description);
    }

    if let Some(ref content) = patch.content {
        result = replace_body(&result, content)?;
    }

    Ok(result)
}

/// Format a list of strings as a YAML block scalar list.
///
/// ```yaml
/// trigger_words:
///   - foo
///   - bar
/// ```
fn format_yaml_list(items: &[String]) -> String {
    if items.is_empty() {
        return "[]".to_string();
    }
    let mut lines = String::new();
    for item in items {
        lines.push_str(&format!("\n  - {item}"));
    }
    lines
}

/// Replace a field's value in the YAML frontmatter.
///
/// Finds the line starting with `{key}:` (or `{key}:\n`) inside the `---` fences
/// and replaces everything from that line until the next top-level key (or closing `---`).
fn replace_frontmatter_field(raw: &str, key: &str, new_value: &str) -> Result<String, McError> {
    let lines: Vec<&str> = raw.lines().collect();

    // Find opening ---
    let start = lines
        .iter()
        .position(|l| l.trim() == "---")
        .ok_or_else(|| McError::Skill("missing YAML frontmatter opening".to_string()))?;

    // Find closing ---
    let end = lines
        .iter()
        .skip(start + 1)
        .position(|l| l.trim() == "---")
        .map(|i| i + start + 1)
        .ok_or_else(|| McError::Skill("missing YAML frontmatter closing".to_string()))?;

    // Find the key line within frontmatter
    let key_prefix = format!("{key}:");
    let key_line = lines[start + 1..end]
        .iter()
        .position(|l| l.trim_start().starts_with(&key_prefix))
        .map(|i| i + start + 1);

    let Some(key_idx) = key_line else {
        // Key not found — append it before closing ---
        let mut result = String::new();
        for (i, line) in lines.iter().enumerate() {
            if i == end {
                result.push_str(&format!("{key}:{new_value}\n"));
            }
            result.push_str(line);
            result.push('\n');
        }
        return Ok(result);
    };

    // Find where this field's value ends (next top-level key or closing ---)
    let field_end = find_field_end(&lines, key_idx, end);

    // Reconstruct
    let mut result = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i == key_idx {
            result.push_str(&format!("{key}:{new_value}"));
            result.push('\n');
        } else if i > key_idx && i < field_end {
            // Skip old continuation lines
            continue;
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }
    Ok(result)
}

/// Find the line index where a YAML field's value ends.
///
/// A field's value continues on indented lines or list items (`  - `).
/// It ends when we hit a non-indented line or the closing `---`.
fn find_field_end(lines: &[&str], key_idx: usize, frontmatter_end: usize) -> usize {
    for i in (key_idx + 1)..frontmatter_end {
        let line = lines[i];
        let trimmed = line.trim_start();
        // If not indented and not a list item continuation, field ends here
        if !line.is_empty()
            && !line.starts_with("  ")
            && !line.starts_with("\t")
            && !trimmed.starts_with("- ")
        {
            return i;
        }
        // Empty lines within multi-line values are OK
        if line.is_empty() {
            // Check if next non-empty line is still part of this field
            let next_continues = lines[i + 1..frontmatter_end]
                .iter()
                .find(|l| !l.is_empty())
                .map(|l| l.starts_with("  ") || l.starts_with("\t") || l.trim_start().starts_with("- "))
                .unwrap_or(false);
            if !next_continues {
                return i;
            }
        }
    }
    frontmatter_end
}

/// Replace the description line (first non-empty, non-frontmatter line after `---`).
fn replace_description_line(raw: &str, new_desc: &str) -> String {
    let lines: Vec<&str> = raw.lines().collect();

    // Find closing ---
    let Some(end_fence) = lines.iter().skip(1).position(|l| l.trim() == "---").map(|i| i + 1) else {
        return raw.to_string();
    };

    // Find first non-empty line after closing ---
    for (i, line) in lines.iter().enumerate().skip(end_fence + 1) {
        if !line.trim().is_empty() {
            let mut result = String::new();
            for (j, l) in lines.iter().enumerate() {
                if j == i {
                    result.push_str(new_desc);
                } else {
                    result.push_str(l);
                }
                result.push('\n');
            }
            return result;
        }
    }
    raw.to_string()
}

/// Replace everything after the YAML frontmatter with new body content.
fn replace_body(raw: &str, new_body: &str) -> Result<String, McError> {
    let lines: Vec<&str> = raw.lines().collect();

    // Find opening ---
    let start = lines
        .iter()
        .position(|l| l.trim() == "---")
        .ok_or_else(|| McError::Skill("missing YAML frontmatter opening".to_string()))?;

    // Find closing ---
    let end = lines
        .iter()
        .skip(start + 1)
        .position(|l| l.trim() == "---")
        .map(|i| i + start + 1)
        .ok_or_else(|| McError::Skill("missing YAML frontmatter closing".to_string()))?;

    let mut result = String::new();
    // Keep frontmatter (including closing ---)
    for line in &lines[..=end] {
        result.push_str(line);
        result.push('\n');
    }
    result.push('\n');
    result.push_str(new_body);
    if !new_body.ends_with('\n') {
        result.push('\n');
    }
    Ok(result)
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const SAMPLE_SKILL: &str = r#"---
trigger_words:
  - deploy
  - CI/CD
version: "1.0.0"
author: test-bot
---

# Deploy Skill

Automate deployment workflows."#;

    fn temp_manager() -> (TempDir, SkillManager) {
        let dir = TempDir::new().unwrap();
        let skills_dir = dir.path().join("skills");
        let memory_dir = dir.path().join("memory");
        let mem = MemoryStore::new(memory_dir);
        let mgr = SkillManager::new(skills_dir, mem);
        (dir, mgr)
    }

    // ── Create ──────────────────────────────────────────────────────

    #[test]
    fn create_skill_basic() {
        let (_dir, mgr) = temp_manager();
        let path = mgr
            .create_skill("deploy", SAMPLE_SKILL, SkillProvenance::User)
            .unwrap();

        assert!(path.exists());
        assert_eq!(path, mgr.skills_dir().join("deploy").join("SKILL.md"));
        assert!(mgr.skill_exists("deploy"));

        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("Deploy Skill"));
    }

    #[test]
    fn create_skill_duplicate_errors() {
        let (_dir, mgr) = temp_manager();
        mgr.create_skill("deploy", SAMPLE_SKILL, SkillProvenance::User)
            .unwrap();

        let result = mgr.create_skill("deploy", SAMPLE_SKILL, SkillProvenance::User);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn create_skill_records_usage() {
        let (_dir, mgr) = temp_manager();
        mgr.create_skill("deploy", SAMPLE_SKILL, SkillProvenance::Agent)
            .unwrap();

        let tracker = UsageTracker::new(mgr.skills_dir());
        let log = tracker.load().unwrap();
        assert_eq!(log.entries.len(), 1);
        assert_eq!(log.entries[0].skill_name, "deploy");
        assert_eq!(log.entries[0].operation, UsageOperation::Create);
        assert_eq!(log.entries[0].provenance, SkillProvenance::Agent);
    }

    // ── Edit ────────────────────────────────────────────────────────

    #[test]
    fn edit_skill_replaces_content() {
        let (_dir, mgr) = temp_manager();
        mgr.create_skill("deploy", SAMPLE_SKILL, SkillProvenance::User)
            .unwrap();

        let new_content = "---\n---\n# Updated\nNew content.";
        mgr.edit_skill("deploy", new_content, SkillProvenance::User)
            .unwrap();

        let content = mgr.read_skill("deploy").unwrap();
        assert_eq!(content, new_content);
    }

    #[test]
    fn edit_skill_not_found() {
        let (_dir, mgr) = temp_manager();
        let result = mgr.edit_skill("ghost", "content", SkillProvenance::User);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn edit_skill_records_usage() {
        let (_dir, mgr) = temp_manager();
        mgr.create_skill("deploy", SAMPLE_SKILL, SkillProvenance::User)
            .unwrap();
        mgr.edit_skill("deploy", "new", SkillProvenance::User)
            .unwrap();

        let tracker = UsageTracker::new(mgr.skills_dir());
        let log = tracker.load().unwrap();
        assert_eq!(log.entries.len(), 2);
        assert_eq!(log.entries[1].operation, UsageOperation::Edit);
    }

    // ── Patch ───────────────────────────────────────────────────────

    #[test]
    fn patch_trigger_words() {
        let (_dir, mgr) = temp_manager();
        mgr.create_skill("deploy", SAMPLE_SKILL, SkillProvenance::User)
            .unwrap();

        let patch = SkillPatch {
            trigger_words: Some(vec!["ship".to_string(), "release".to_string()]),
            ..Default::default()
        };
        mgr.patch_skill("deploy", &patch, SkillProvenance::User)
            .unwrap();

        let content = mgr.read_skill("deploy").unwrap();
        assert!(content.contains("ship"));
        assert!(content.contains("release"));
        // Old trigger replaced: "- deploy" no longer in trigger_words section
        assert!(!content.contains("- deploy"));
        assert!(!content.contains("- CI/CD"));
    }

    #[test]
    fn patch_description() {
        let (_dir, mgr) = temp_manager();
        mgr.create_skill("deploy", SAMPLE_SKILL, SkillProvenance::User)
            .unwrap();

        let patch = SkillPatch {
            description: Some("# New Deploy".to_string()),
            ..Default::default()
        };
        mgr.patch_skill("deploy", &patch, SkillProvenance::User)
            .unwrap();

        let content = mgr.read_skill("deploy").unwrap();
        assert!(content.contains("# New Deploy"));
        assert!(content.contains("trigger_words:")); // frontmatter preserved
    }

    #[test]
    fn patch_content_replaces_body() {
        let (_dir, mgr) = temp_manager();
        mgr.create_skill("deploy", SAMPLE_SKILL, SkillProvenance::User)
            .unwrap();

        let patch = SkillPatch {
            content: Some("# Entirely New Body\n\nDifferent content.".to_string()),
            ..Default::default()
        };
        mgr.patch_skill("deploy", &patch, SkillProvenance::User)
            .unwrap();

        let content = mgr.read_skill("deploy").unwrap();
        assert!(content.contains("Entirely New Body"));
        assert!(!content.contains("Automate deployment"));
        assert!(content.contains("trigger_words:")); // frontmatter preserved
    }

    #[test]
    fn patch_not_found() {
        let (_dir, mgr) = temp_manager();
        let patch = SkillPatch::default();
        let result = mgr.patch_skill("ghost", &patch, SkillProvenance::User);
        assert!(result.is_err());
    }

    #[test]
    fn patch_empty_is_noop() {
        let (_dir, mgr) = temp_manager();
        mgr.create_skill("deploy", SAMPLE_SKILL, SkillProvenance::User)
            .unwrap();

        let patch = SkillPatch::default();
        mgr.patch_skill("deploy", &patch, SkillProvenance::User)
            .unwrap();

        let content = mgr.read_skill("deploy").unwrap();
        assert_eq!(content, SAMPLE_SKILL);
    }

    #[test]
    fn patch_records_usage() {
        let (_dir, mgr) = temp_manager();
        mgr.create_skill("deploy", SAMPLE_SKILL, SkillProvenance::User)
            .unwrap();

        let patch = SkillPatch {
            trigger_words: Some(vec!["test".to_string()]),
            ..Default::default()
        };
        mgr.patch_skill("deploy", &patch, SkillProvenance::Agent)
            .unwrap();

        let tracker = UsageTracker::new(mgr.skills_dir());
        let log = tracker.load().unwrap();
        assert_eq!(log.entries.len(), 2);
        assert_eq!(log.entries[1].operation, UsageOperation::Patch);
        assert_eq!(log.entries[1].provenance, SkillProvenance::Agent);
    }

    // ── Delete ──────────────────────────────────────────────────────

    #[test]
    fn delete_skill_removes_directory() {
        let (_dir, mgr) = temp_manager();
        mgr.create_skill("deploy", SAMPLE_SKILL, SkillProvenance::User)
            .unwrap();
        assert!(mgr.skill_exists("deploy"));

        mgr.delete_skill("deploy", SkillProvenance::User).unwrap();
        assert!(!mgr.skill_exists("deploy"));
        assert!(!mgr.skills_dir().join("deploy").exists());
    }

    #[test]
    fn delete_skill_not_found() {
        let (_dir, mgr) = temp_manager();
        let result = mgr.delete_skill("ghost", SkillProvenance::User);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn delete_skill_records_usage() {
        let (_dir, mgr) = temp_manager();
        mgr.create_skill("deploy", SAMPLE_SKILL, SkillProvenance::User)
            .unwrap();
        mgr.delete_skill("deploy", SkillProvenance::User).unwrap();

        let tracker = UsageTracker::new(mgr.skills_dir());
        let log = tracker.load().unwrap();
        assert_eq!(log.entries.len(), 2);
        assert_eq!(log.entries[1].operation, UsageOperation::Delete);
    }

    // ── List / Read ─────────────────────────────────────────────────

    #[test]
    fn list_skills_sorted() {
        let (_dir, mgr) = temp_manager();
        mgr.create_skill("beta", "---\n---\nBeta.", SkillProvenance::User)
            .unwrap();
        mgr.create_skill("alpha", "---\n---\nAlpha.", SkillProvenance::User)
            .unwrap();

        let names = mgr.list_skills().unwrap();
        assert_eq!(names, vec!["alpha", "beta"]);
    }

    #[test]
    fn list_skills_empty_dir() {
        let (_dir, mgr) = temp_manager();
        let names = mgr.list_skills().unwrap();
        assert!(names.is_empty());
    }

    #[test]
    fn read_skill_not_found() {
        let (_dir, mgr) = temp_manager();
        let result = mgr.read_skill("ghost");
        assert!(result.is_err());
    }

    // ── Full lifecycle ──────────────────────────────────────────────

    #[test]
    fn full_lifecycle_tracks_all_operations() {
        let (_dir, mgr) = temp_manager();

        mgr.create_skill("my-skill", SAMPLE_SKILL, SkillProvenance::Agent)
            .unwrap();

        mgr.edit_skill("my-skill", "---\n---\n# Edited", SkillProvenance::Agent)
            .unwrap();

        let patch = SkillPatch {
            trigger_words: Some(vec!["foo".to_string()]),
            ..Default::default()
        };
        mgr.patch_skill("my-skill", &patch, SkillProvenance::Agent)
            .unwrap();

        mgr.delete_skill("my-skill", SkillProvenance::Agent)
            .unwrap();

        let tracker = UsageTracker::new(mgr.skills_dir());
        let log = tracker.load().unwrap();
        assert_eq!(log.entries.len(), 4);

        let ops: Vec<UsageOperation> = log.entries.iter().map(|e| e.operation).collect();
        assert_eq!(
            ops,
            vec![
                UsageOperation::Create,
                UsageOperation::Edit,
                UsageOperation::Patch,
                UsageOperation::Delete,
            ]
        );

        // All entries reference the same skill
        assert!(log.entries.iter().all(|e| e.skill_name == "my-skill"));
        assert!(log
            .entries
            .iter()
            .all(|e| e.provenance == SkillProvenance::Agent));
    }

    // ── Pure function tests ─────────────────────────────────────────

    #[test]
    fn replace_frontmatter_field_updates_existing() {
        let result = replace_frontmatter_field(
            "---\nversion: \"1.0\"\nauthor: bob\n---\nBody",
            "version",
            " \"2.0\"",
        )
        .unwrap();
        assert!(result.contains("version: \"2.0\""));
        assert!(result.contains("author: bob"));
    }

    #[test]
    fn replace_frontmatter_field_appends_missing() {
        let result = replace_frontmatter_field(
            "---\nversion: \"1.0\"\n---\nBody",
            "author",
            " alice",
        )
        .unwrap();
        assert!(result.contains("author: alice"));
        assert!(result.contains("version: \"1.0\""));
    }

    #[test]
    fn replace_body_preserves_frontmatter() {
        let result =
            replace_body("---\nversion: \"1\"\n---\nOld body", "# New\nBody").unwrap();
        assert!(result.contains("---\nversion: \"1\"\n---\n"));
        assert!(result.contains("# New\nBody"));
        assert!(!result.contains("Old body"));
    }

    #[test]
    fn format_yaml_list_empty() {
        assert_eq!(format_yaml_list(&[]), "[]");
    }

    #[test]
    fn format_yaml_list_items() {
        let items = vec!["foo".to_string(), "bar".to_string()];
        let result = format_yaml_list(&items);
        assert!(result.contains("\n  - foo"));
        assert!(result.contains("\n  - bar"));
    }
}
