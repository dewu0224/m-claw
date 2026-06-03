//! Skill loading, scanning, and lookup.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::skill::{Skill, SkillMetadata};

/// Scan directories for SKILL.md files and load them.
pub struct SkillLoader {
    /// Map from skill name → loaded Skill.
    skills: HashMap<String, Skill>,
}

impl SkillLoader {
    /// Create an empty loader with no skills.
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
        }
    }

    /// Scan `dir` recursively for `SKILL.md` files and load them.
    ///
    /// Each `SKILL.md` is expected to live inside a directory whose name
    /// becomes the skill's `name`. Returns the number of skills loaded.
    pub fn load_dir(&mut self, dir: &Path) -> Result<usize, String> {
        let pattern = format!("{}/**/SKILL.md", dir.display());
        let paths: Vec<PathBuf> = glob::glob(&pattern)
            .map_err(|e| format!("invalid glob pattern: {e}"))?
            .filter_map(Result::ok)
            .collect();

        let mut count = 0;
        for path in paths {
            self.load_file(&path)?;
            count += 1;
        }
        Ok(count)
    }

    /// Load a single SKILL.md file. The skill name is derived from the
    /// parent directory name.
    pub fn load_file(&mut self, path: &Path) -> Result<&Skill, String> {
        let raw = fs::read_to_string(path)
            .map_err(|e| format!("failed to read {}: {e}", path.display()))?;

        let (metadata, content, description) = parse_skill_md(&raw)?;

        let name = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".to_string());

        let skill = Skill {
            name: name.clone(),
            description,
            content,
            metadata,
            path: path.to_path_buf(),
        };

        self.skills.insert(name, skill);
        // SAFETY: we just inserted it.
        Ok(self.skills.values().next().unwrap())
    }

    /// Load a skill by name with fuzzy matching.
    ///
    /// Lookup order:
    /// 1. Exact match on skill name
    /// 2. Case-insensitive match
    /// 3. Substring match (name contains query or query contains name)
    pub fn load(&self, name: &str) -> Option<&Skill> {
        // Exact match
        if let Some(skill) = self.skills.get(name) {
            return Some(skill);
        }

        let lower = name.to_lowercase();

        // Case-insensitive exact
        if let Some(skill) = self.skills.values().find(|s| s.name.to_lowercase() == lower) {
            return Some(skill);
        }

        // Substring match
        self.skills
            .values()
            .find(|s| {
                let name_lower = s.name.to_lowercase();
                name_lower.contains(&lower) || lower.contains(&name_lower)
            })
    }

    /// Find all skills whose trigger words match `query`.
    pub fn match_trigger(&self, query: &str) -> Vec<&Skill> {
        self.skills
            .values()
            .filter(|s| s.matches_trigger(query))
            .collect()
    }

    /// Return a summary string of all loaded skills for system-prompt injection.
    pub fn summary(&self) -> String {
        let mut lines: Vec<String> = self.skills.values().map(|s| s.summary()).collect();
        lines.sort();
        lines.join("\n")
    }

    /// Get all loaded skills.
    pub fn all(&self) -> &HashMap<String, Skill> {
        &self.skills
    }

    /// Get a reference to a skill by exact name.
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// Number of loaded skills.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    /// Whether no skills are loaded.
    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}

impl Default for SkillLoader {
    fn default() -> Self {
        Self::new()
    }
}

/// Parse a SKILL.md string into (metadata, body, description).
///
/// Expected format:
/// ```markdown
/// ---
/// trigger_words: [foo, bar]
/// version: "1.0"
/// ---
/// Description line
///
/// Rest of content...
/// ```
fn parse_skill_md(raw: &str) -> Result<(SkillMetadata, String, String), String> {
    let trimmed = raw.trim_start();

    if !trimmed.starts_with("---") {
        return Err("missing YAML frontmatter (must start with '---')".to_string());
    }

    // Find the closing '---'
    let after_first = &trimmed[3..];
    let end = after_first
        .find("\n---")
        .ok_or("unterminated YAML frontmatter (missing closing '---')")?;

    let yaml_str = &after_first[..end];
    let metadata: SkillMetadata =
        serde_yaml::from_str(yaml_str).map_err(|e| format!("invalid YAML frontmatter: {e}"))?;

    let body_start = end + 3 + 1; // skip past "\n---\n"
    let body = after_first
        .get(body_start..)
        .unwrap_or("")
        .trim()
        .to_string();

    // Extract description from the first non-empty line of the body.
    let description = body
        .lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| {
            l.trim()
                .trim_start_matches('#')
                .trim()
                .to_string()
        })
        .unwrap_or_default();

    Ok((metadata, body, description))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: create a SKILL.md inside `<dir>/<name>/SKILL.md`.
    fn create_skill(dir: &Path, name: &str, content: &str) -> PathBuf {
        let skill_dir = dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();
        let path = skill_dir.join("SKILL.md");
        fs::write(&path, content).unwrap();
        path
    }

    const SAMPLE_SKILL: &str = r#"---
trigger_words:
  - deploy
  - deployment
  - CI/CD
version: "1.0.0"
author: "test-bot"
dependencies:
  - "lark-tools"
---

# Deploy Skill

Automate deployment workflows."#;

    #[test]
    fn load_normal_skill() {
        let tmp = TempDir::new().unwrap();
        create_skill(tmp.path(), "deploy", SAMPLE_SKILL);

        let mut loader = SkillLoader::new();
        let count = loader.load_dir(tmp.path()).unwrap();
        assert_eq!(count, 1);

        let skill = loader.load("deploy").expect("skill 'deploy' not found");
        assert_eq!(skill.name, "deploy");
        assert_eq!(skill.description, "Deploy Skill");
        assert_eq!(skill.metadata.version, "1.0.0");
        assert_eq!(skill.metadata.author, "test-bot");
        assert_eq!(skill.metadata.dependencies, vec!["lark-tools"]);
        assert!(skill.content.contains("Automate deployment"));
    }

    #[test]
    fn load_multiple_skills() {
        let tmp = TempDir::new().unwrap();
        create_skill(tmp.path(), "alpha", SAMPLE_SKILL);
        create_skill(
            tmp.path(),
            "beta",
            "---\ntrigger_words: [test]\n---\n# Beta\nBeta skill.",
        );

        let mut loader = SkillLoader::new();
        let count = loader.load_dir(tmp.path()).unwrap();
        assert_eq!(count, 2);

        assert!(loader.load("alpha").is_some());
        assert!(loader.load("beta").is_some());
        assert_eq!(loader.len(), 2);
    }

    #[test]
    fn load_missing_dir() {
        let loader = SkillLoader::new();
        assert!(loader.load("nonexistent").is_none());
    }

    #[test]
    fn load_missing_file() {
        let tmp = TempDir::new().unwrap();
        // Empty dir — no SKILL.md files
        let mut loader = SkillLoader::new();
        let count = loader.load_dir(tmp.path()).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn load_bad_frontmatter() {
        let tmp = TempDir::new().unwrap();
        create_skill(
            tmp.path(),
            "bad",
            "---\n: invalid: yaml: [[[\n---\nContent",
        );

        let mut loader = SkillLoader::new();
        let result = loader.load_dir(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid YAML frontmatter"));
    }

    #[test]
    fn load_missing_frontmatter() {
        let tmp = TempDir::new().unwrap();
        create_skill(tmp.path(), "nofm", "# No Frontmatter\nJust content.");

        let mut loader = SkillLoader::new();
        let result = loader.load_dir(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("missing YAML frontmatter"));
    }

    #[test]
    fn trigger_match() {
        let tmp = TempDir::new().unwrap();
        create_skill(tmp.path(), "deploy", SAMPLE_SKILL);

        let mut loader = SkillLoader::new();
        loader.load_dir(tmp.path()).unwrap();

        assert!(!loader.match_trigger("deploy").is_empty());
        assert!(!loader.match_trigger("deployment").is_empty());
        assert!(!loader.match_trigger("CI/CD").is_empty());
        assert!(loader.match_trigger("unrelated").is_empty());
    }

    #[test]
    fn trigger_case_insensitive() {
        let tmp = TempDir::new().unwrap();
        create_skill(
            tmp.path(),
            "search",
            "---\ntrigger_words:\n  - Search\n  - FIND\n---\n# Search\nSearch things.",
        );

        let mut loader = SkillLoader::new();
        loader.load_dir(tmp.path()).unwrap();

        assert!(!loader.match_trigger("search").is_empty());
        assert!(!loader.match_trigger("SEARCH").is_empty());
        assert!(!loader.match_trigger("find").is_empty());
        assert!(!loader.match_trigger("FIND").is_empty());
    }

    #[test]
    fn fuzzy_load_case_insensitive() {
        let tmp = TempDir::new().unwrap();
        create_skill(tmp.path(), "MySkill", SAMPLE_SKILL);

        let mut loader = SkillLoader::new();
        loader.load_dir(tmp.path()).unwrap();

        assert!(loader.load("myskill").is_some());
        assert!(loader.load("MYSKILL").is_some());
        assert!(loader.load("MySkill").is_some());
    }

    #[test]
    fn fuzzy_load_substring() {
        let tmp = TempDir::new().unwrap();
        create_skill(tmp.path(), "lark-tools", SAMPLE_SKILL);

        let mut loader = SkillLoader::new();
        loader.load_dir(tmp.path()).unwrap();

        assert!(loader.load("lark").is_some());
        assert!(loader.load("tools").is_some());
    }

    #[test]
    fn summary_format() {
        let tmp = TempDir::new().unwrap();
        create_skill(tmp.path(), "alpha", SAMPLE_SKILL);
        create_skill(
            tmp.path(),
            "beta",
            "---\n---\n# Beta\nBeta skill.",
        );

        let mut loader = SkillLoader::new();
        loader.load_dir(tmp.path()).unwrap();

        let summary = loader.summary();
        assert!(summary.contains("alpha — Deploy Skill"));
        assert!(summary.contains("beta — Beta"));
    }

    #[test]
    fn summary_single_skill() {
        let tmp = TempDir::new().unwrap();
        create_skill(tmp.path(), "only", "---\n---\n# Only\nThe only skill.");

        let mut loader = SkillLoader::new();
        loader.load_dir(tmp.path()).unwrap();

        assert_eq!(loader.summary(), "only — Only");
    }

    #[test]
    fn summary_empty() {
        let loader = SkillLoader::new();
        assert_eq!(loader.summary(), "");
    }
}
