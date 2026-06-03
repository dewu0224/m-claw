//! Nudge configuration and review trigger types.
//!
//! The nudge system tracks conversation activity and triggers background
//! reviews when configurable thresholds are reached.

use serde::{Deserialize, Serialize};

/// Configuration for when to trigger background reviews.
///
/// Two independent counters track conversation progress:
/// - `memory_interval`: trigger a memory review every N conversation turns
/// - `skill_interval`: trigger a skill review every N tool invocations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NudgeConfig {
    /// Number of conversation turns between memory reviews. Default: 10.
    pub memory_interval: u32,
    /// Number of tool invocations between skill reviews. Default: 10.
    pub skill_interval: u32,
}

impl Default for NudgeConfig {
    fn default() -> Self {
        Self {
            memory_interval: 10,
            skill_interval: 10,
        }
    }
}

impl NudgeConfig {
    /// Create a new config with custom intervals.
    pub fn new(memory_interval: u32, skill_interval: u32) -> Self {
        Self {
            memory_interval,
            skill_interval,
        }
    }
}

/// Identifies which type of review was triggered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewTrigger {
    /// Memory review triggered — analyze conversation for knowledge updates.
    Memory,
    /// Skill review triggered — analyze tool usage for skill improvements.
    Skill,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nudge_config_defaults() {
        let cfg = NudgeConfig::default();
        assert_eq!(cfg.memory_interval, 10);
        assert_eq!(cfg.skill_interval, 10);
    }

    #[test]
    fn test_nudge_config_custom() {
        let cfg = NudgeConfig::new(5, 20);
        assert_eq!(cfg.memory_interval, 5);
        assert_eq!(cfg.skill_interval, 20);
    }

    #[test]
    fn test_review_trigger_serde_roundtrip() {
        let memory = ReviewTrigger::Memory;
        let json = serde_json::to_string(&memory).unwrap();
        let back: ReviewTrigger = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ReviewTrigger::Memory);

        let skill = ReviewTrigger::Skill;
        let json = serde_json::to_string(&skill).unwrap();
        let back: ReviewTrigger = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ReviewTrigger::Skill);
    }

    #[test]
    fn test_nudge_config_serde_roundtrip() {
        let cfg = NudgeConfig::new(7, 15);
        let json = serde_json::to_string(&cfg).unwrap();
        let back: NudgeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.memory_interval, 7);
        assert_eq!(back.skill_interval, 15);
    }
}
