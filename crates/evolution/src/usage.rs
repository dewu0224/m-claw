//! Usage tracking — reads and writes `.usage-log.json` in the skills directory.
//!
//! Note: The Curator uses `.usage.json` for per-skill state tracking.
//! SkillManager uses `.usage-log.json` for event logging to avoid conflicts.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use mc_core::McError;

use crate::types::{SkillProvenance, SkillUsageLog, UsageEntry, UsageOperation};

const USAGE_FILENAME: &str = ".usage-log.json";

/// Manages the `.usage-log.json` file that records SkillManager lifecycle events.
pub struct UsageTracker {
    path: PathBuf,
}

impl UsageTracker {
    /// Create a tracker that writes to `<skills_dir>/.usage-log.json`.
    pub fn new(skills_dir: &Path) -> Self {
        Self {
            path: skills_dir.join(USAGE_FILENAME),
        }
    }

    /// Record a skill operation.
    pub fn record(
        &self,
        skill_name: &str,
        operation: UsageOperation,
        provenance: SkillProvenance,
    ) -> Result<(), McError> {
        let mut log = self.load()?;
        log.entries.push(UsageEntry {
            skill_name: skill_name.to_string(),
            operation,
            provenance,
            timestamp: Utc::now(),
        });
        self.save(&log)?;
        Ok(())
    }

    /// Load the current usage log. Returns an empty log if the file doesn't exist.
    pub fn load(&self) -> Result<SkillUsageLog, McError> {
        match fs::read_to_string(&self.path) {
            Ok(raw) => {
                let log: SkillUsageLog =
                    serde_json::from_str(&raw).unwrap_or_default();
                Ok(log)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(SkillUsageLog::default()),
            Err(e) => Err(McError::Io(e)),
        }
    }

    /// Persist the usage log to disk.
    fn save(&self, log: &SkillUsageLog) -> Result<(), McError> {
        if let Some(parent) = self.path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        let json = serde_json::to_string_pretty(log)?;
        fs::write(&self.path, json)?;
        Ok(())
    }

    /// Return the path to the usage file.
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn temp_tracker() -> (TempDir, UsageTracker) {
        let dir = TempDir::new().unwrap();
        let tracker = UsageTracker::new(dir.path());
        (dir, tracker)
    }

    #[test]
    fn load_empty_when_no_file() {
        let (_dir, tracker) = temp_tracker();
        let log = tracker.load().unwrap();
        assert!(log.entries.is_empty());
    }

    #[test]
    fn record_and_reload() {
        let (_dir, tracker) = temp_tracker();
        tracker
            .record("test-skill", UsageOperation::Create, SkillProvenance::User)
            .unwrap();

        let log = tracker.load().unwrap();
        assert_eq!(log.entries.len(), 1);
        assert_eq!(log.entries[0].skill_name, "test-skill");
        assert_eq!(log.entries[0].operation, UsageOperation::Create);
        assert_eq!(log.entries[0].provenance, SkillProvenance::User);
    }

    #[test]
    fn multiple_records_accumulate() {
        let (_dir, tracker) = temp_tracker();
        tracker
            .record("a", UsageOperation::Create, SkillProvenance::User)
            .unwrap();
        tracker
            .record("a", UsageOperation::Edit, SkillProvenance::User)
            .unwrap();
        tracker
            .record("b", UsageOperation::Create, SkillProvenance::Agent)
            .unwrap();

        let log = tracker.load().unwrap();
        assert_eq!(log.entries.len(), 3);
    }

    #[test]
    fn corrupt_file_returns_empty_log() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(USAGE_FILENAME);
        fs::write(&path, "not valid json {{{").unwrap();

        let tracker = UsageTracker::new(dir.path());
        let log = tracker.load().unwrap();
        assert!(log.entries.is_empty());
    }
}
