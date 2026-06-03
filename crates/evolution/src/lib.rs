//! Evolution system (Background Review + Curator).
//!
//! This crate provides the self-improvement loop: background conversation
//! review after N turns, memory/skill nudges, and the Curator for skill
//! lifecycle management (Active → Stale → Archived).
//!
//! # Background Review
//!
//! The [`BackgroundReviewer`] monitors conversation activity and spawns a
//! restricted LLM agent when nudge thresholds are reached. The agent
//! analyzes the conversation for user corrections, technical discoveries,
//! and preference changes, then proposes memory and skill updates.
//!
//! # Curator
//!
//! The [`Curator`] manages skill lifecycle through:
//! - **Automatic state transitions** based on inactivity
//! - **Backup snapshots** before each run
//! - **Consolidation scanning** to find merge candidates
//!
//! # Configuration
//!
//! [`NudgeConfig`] controls the review frequency:
//! - `memory_interval` — review memory every N turns (default: 10)
//! - `skill_interval` — review skills every N tool invocations (default: 10)

mod curator;
mod manager;
mod nudge;
mod reviewer;
mod types;
mod usage;

pub use curator::{Curator, CuratorConfig};
pub use manager::SkillManager;
pub use nudge::{NudgeConfig, ReviewTrigger};
pub use reviewer::{BackgroundReviewer, MemoryUpdate, ReviewResult, SkillUpdate, run_review};
pub use types::{
    ConsolidationReport, MergeCandidate, SkillOrigin, SkillPatch, SkillProvenance, SkillState,
    SkillUsage, SkillUsageLog, Transition, UsageEntry, UsageOperation,
};
pub use usage::UsageTracker;
