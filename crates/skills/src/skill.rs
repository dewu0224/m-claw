//! Data types for a single loaded skill.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Metadata parsed from the YAML frontmatter of a SKILL.md file.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SkillMetadata {
    /// Words or phrases that trigger this skill.
    #[serde(default)]
    pub trigger_words: Vec<String>,

    /// Semantic version string (e.g. "1.0.0").
    #[serde(default)]
    pub version: String,

    /// Author or maintainer name.
    #[serde(default)]
    pub author: String,

    /// Names of other skills this skill depends on.
    #[serde(default)]
    pub dependencies: Vec<String>,
}

/// A fully loaded skill from a SKILL.md file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    /// Human-readable skill name (derived from the containing directory name).
    pub name: String,

    /// One-line description from frontmatter or first heading.
    pub description: String,

    /// The full Markdown body (everything after the frontmatter).
    pub content: String,

    /// Parsed YAML frontmatter metadata.
    pub metadata: SkillMetadata,

    /// Absolute path to the SKILL.md file on disk.
    pub path: PathBuf,
}

impl Skill {
    /// Return a short summary suitable for system-prompt injection.
    ///
    /// Format: `name — description`
    pub fn summary(&self) -> String {
        format!("{} — {}", self.name, self.description)
    }

    /// Check whether `query` matches any of this skill's trigger words.
    ///
    /// Matching is case-insensitive and supports partial substring hits.
    pub fn matches_trigger(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        self.metadata
            .trigger_words
            .iter()
            .any(|tw| q.contains(&tw.to_lowercase()) || tw.to_lowercase().contains(&q))
    }
}
