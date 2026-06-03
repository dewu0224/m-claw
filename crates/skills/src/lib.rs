//! SKILL.md parsing and skill management.
//!
//! This crate provides skill loading from the filesystem, YAML frontmatter
//! parsing, trigger word matching, and summary generation for system prompts.
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//! use mc_skills::SkillLoader;
//!
//! let mut loader = SkillLoader::new();
//! loader.load_dir(Path::new("/path/to/skills")).unwrap();
//!
//! // Fuzzy lookup
//! if let Some(skill) = loader.load("deploy") {
//!     println!("{}", skill.summary());
//! }
//!
//! // Trigger word matching
//! let matched = loader.match_trigger("CI/CD");
//! for s in matched {
//!     println!("triggered: {}", s.name);
//! }
//!
//! // System prompt injection
//! println!("{}", loader.summary());
//! ```

mod loader;
mod skill;

pub use loader::SkillLoader;
pub use skill::{Skill, SkillMetadata};
