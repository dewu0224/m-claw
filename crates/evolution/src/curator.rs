//! Curator — skill lifecycle management with automatic state transitions,
//! backup, and consolidation suggestions.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use tracing::{debug, info, warn};

use mc_core::{Message, McError};
use mc_llm::{ChatRequest, LlmProvider};

use crate::types::{
    ConsolidationReport, MergeCandidate, SkillOrigin, SkillState, SkillUsage, Transition,
};

// ── CuratorConfig ─────────────────────────────────────────────────────────

/// Configuration for the Curator lifecycle manager.
#[derive(Debug, Clone)]
pub struct CuratorConfig {
    /// Days of inactivity before a skill transitions from Active to Stale.
    pub stale_after_days: u32,
    /// Days of inactivity before a skill transitions from Stale to Archived.
    pub archive_after_days: u32,
    /// How often the Curator should run (in hours). Default: 168 (7 days).
    pub run_interval_hours: u32,
}

impl Default for CuratorConfig {
    fn default() -> Self {
        Self {
            stale_after_days: 30,
            archive_after_days: 90,
            run_interval_hours: 168,
        }
    }
}

// ── Curator ───────────────────────────────────────────────────────────────

/// Periodic skill lifecycle manager.
///
/// Scans the skills directory, tracks usage telemetry in `.usage.json`,
/// applies automatic state transitions (Active → Stale → Archived),
/// creates backups before runs, and suggests merge candidates.
pub struct Curator {
    skills_dir: PathBuf,
    usage_path: PathBuf,
    config: CuratorConfig,
}

impl Curator {
    /// Create a new Curator for the given skills directory.
    ///
    /// Usage telemetry is stored at `skills_dir/.usage.json`.
    pub fn new(skills_dir: impl Into<PathBuf>, config: CuratorConfig) -> Self {
        let skills_dir = skills_dir.into();
        let usage_path = skills_dir.join(".usage.json");
        Self {
            skills_dir,
            usage_path,
            config,
        }
    }

    /// Create a new Curator with an explicit usage file path.
    pub fn with_usage_path(
        skills_dir: impl Into<PathBuf>,
        usage_path: impl Into<PathBuf>,
        config: CuratorConfig,
    ) -> Self {
        Self {
            skills_dir: skills_dir.into(),
            usage_path: usage_path.into(),
            config,
        }
    }

    /// Returns a reference to the skills directory.
    pub fn skills_dir(&self) -> &Path {
        &self.skills_dir
    }

    /// Returns a reference to the usage file path.
    pub fn usage_path(&self) -> &Path {
        &self.usage_path
    }

    /// Returns a reference to the configuration.
    pub fn config(&self) -> &CuratorConfig {
        &self.config
    }

    // ── Usage persistence ──────────────────────────────────────────────

    /// Load usage telemetry from `.usage.json`.
    ///
    /// Returns an empty map if the file doesn't exist.
    pub fn load_usage(&self) -> Result<HashMap<String, SkillUsage>, McError> {
        if !self.usage_path.exists() {
            return Ok(HashMap::new());
        }
        let data = fs::read_to_string(&self.usage_path)?;
        if data.trim().is_empty() {
            return Ok(HashMap::new());
        }
        let map: HashMap<String, SkillUsage> = serde_json::from_str(&data)?;
        Ok(map)
    }

    /// Save usage telemetry to `.usage.json`.
    pub fn save_usage(&self, usage: &HashMap<String, SkillUsage>) -> Result<(), McError> {
        if let Some(parent) = self.usage_path.parent() {
            if !parent.exists() {
                fs::create_dir_all(parent)?;
            }
        }
        let json = serde_json::to_string_pretty(usage)?;
        fs::write(&self.usage_path, json)?;
        Ok(())
    }

    // ── Automatic state transitions ────────────────────────────────────

    /// Scan skills on disk and apply automatic state transitions.
    ///
    /// Rules:
    /// - **Pinned** skills are never transitioned.
    /// - **Active** → **Stale** after `stale_after_days` of inactivity.
    /// - **Stale** → **Archived** after `archive_after_days` of inactivity.
    /// - **Archived** stays archived (no auto-unarchive).
    ///
    /// Returns the list of transitions that were applied.
    pub fn apply_automatic_transitions(&self) -> Result<Vec<Transition>, McError> {
        let mut usage = self.load_usage()?;
        let now = Utc::now();
        let mut transitions = Vec::new();

        // Ensure every skill directory on disk has a usage entry
        for name in self.discover_skills() {
            usage
                .entry(name)
                .or_insert_with(|| SkillUsage::new(SkillOrigin::Agent));
        }

        for (name, entry) in &mut usage {
            if entry.pinned || entry.state == SkillState::Pinned {
                debug!("skipping pinned skill: {name}");
                continue;
            }

            let inactive_days = (now - entry.last_activity_at).num_days();
            let inactive_days = inactive_days.max(0) as u32;

            let new_state = match entry.state {
                SkillState::Active if inactive_days >= self.config.stale_after_days => {
                    Some(SkillState::Stale)
                }
                SkillState::Stale if inactive_days >= self.config.archive_after_days => {
                    Some(SkillState::Archived)
                }
                _ => None,
            };

            if let Some(to) = new_state {
                info!("skill '{name}': {} -> {to}", entry.state);
                transitions.push(Transition {
                    skill_name: name.clone(),
                    from: entry.state,
                    to,
                    applied_at: now,
                });
                entry.state = to;
            }
        }

        self.save_usage(&usage)?;
        Ok(transitions)
    }

    // ── Backup ─────────────────────────────────────────────────────────

    /// Create a timestamped snapshot of the skills directory into `.backups/`.
    ///
    /// Returns the path to the backup directory.
    pub fn backup(&self) -> Result<PathBuf, McError> {
        let timestamp = Utc::now().format("%Y%m%dT%H%M%S").to_string();
        let backup_dir = self.skills_dir.join(".backups").join(&timestamp);

        fs::create_dir_all(&backup_dir)?;

        // Copy each skill directory and the usage file
        for name in self.discover_skills() {
            let src = self.skills_dir.join(&name);
            let dst = backup_dir.join(&name);
            copy_dir_recursive(&src, &dst)?;
        }

        // Back up the usage file if it exists
        if self.usage_path.exists() {
            let dst = backup_dir.join(".usage.json");
            fs::copy(&self.usage_path, dst)?;
        }

        info!("backup created at {}", backup_dir.display());
        Ok(backup_dir)
    }

    // ── Consolidation ──────────────────────────────────────────────────

    /// Scan skill names for potential merge candidates using string similarity.
    ///
    /// This is the non-LLM version that uses name normalization and substring
    /// matching to find pairs of skills that may be redundant.
    pub fn run_consolidation_sync(&self) -> Result<ConsolidationReport, McError> {
        let names = self.discover_skills();
        let scanned = names.len();
        let mut candidates = Vec::new();

        for i in 0..names.len() {
            for j in (i + 1)..names.len() {
                if let Some(candidate) = evaluate_merge_candidate(&names[i], &names[j]) {
                    candidates.push(candidate);
                }
            }
        }

        candidates.sort_by(|a, b| {
            b.similarity
                .partial_cmp(&a.similarity)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(ConsolidationReport {
            merge_candidates: candidates,
            skills_scanned: scanned,
        })
    }

    /// LLM-enhanced consolidation pass.
    ///
    /// Runs the basic string-similarity scan first, then optionally refines
    /// results by asking the LLM to evaluate merge candidates.
    pub async fn run_consolidation(
        &self,
        provider: &dyn LlmProvider,
    ) -> Result<ConsolidationReport, McError> {
        let mut report = self.run_consolidation_sync()?;

        if report.merge_candidates.is_empty() {
            return Ok(report);
        }

        // Use LLM to re-evaluate the top candidates
        let skill_names: Vec<&str> = report
            .merge_candidates
            .iter()
            .flat_map(|c| [&*c.skill_a, &*c.skill_b])
            .collect();
        let prompt = format!(
            "Given these mavis-claw skill names, which pairs should be merged?\n\
             Skills: {skill_names:?}\n\
             Reply with JSON array: [{{\"a\": \"name1\", \"b\": \"name2\", \"merge\": true/false, \"reason\": \"...\"}}]"
        );

        let request = ChatRequest {
            messages: vec![Message::user(prompt)],
            model: String::new(),
            tools: None,
            max_tokens: Some(1024),
            temperature: Some(0.2),
            stream: false,
        };

        match provider.chat(request).await {
            Ok(response) => {
                let content = response.message.content.unwrap_or_default();
                debug!("LLM consolidation response: {content}");
                // Parse LLM response and filter candidates
                if let Ok(decisions) =
                    serde_json::from_str::<Vec<LlmMergeDecision>>(&content)
                {
                    report.merge_candidates.retain(|c| {
                        decisions.iter().any(|d| {
                            (d.a == c.skill_a && d.b == c.skill_b
                                || d.a == c.skill_b && d.b == c.skill_a)
                                && d.merge
                        })
                    });
                    // Update reasons from LLM
                    for candidate in &mut report.merge_candidates {
                        if let Some(decision) = decisions.iter().find(|d| {
                            d.a == candidate.skill_a && d.b == candidate.skill_b
                                || d.a == candidate.skill_b && d.b == candidate.skill_a
                        }) {
                            candidate.reason = decision.reason.clone();
                        }
                    }
                }
            }
            Err(e) => {
                warn!("LLM consolidation call failed, using basic results: {e}");
            }
        }

        Ok(report)
    }

    // ── Internal helpers ───────────────────────────────────────────────

    /// Discover skill directory names under `skills_dir`.
    ///
    /// A directory qualifies as a skill if it contains a `SKILL.md` file.
    pub(crate) fn discover_skills(&self) -> Vec<String> {
        let mut names = Vec::new();
        if !self.skills_dir.exists() {
            return names;
        }
        let entries = match fs::read_dir(&self.skills_dir) {
            Ok(e) => e,
            Err(_) => return names,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Skip hidden directories
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if name.starts_with('.') {
                        continue;
                    }
                }
                let skill_md = path.join("SKILL.md");
                if skill_md.exists() {
                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        names.push(name.to_string());
                    }
                }
            }
        }
        names.sort();
        names
    }
}

// ── Helper functions ──────────────────────────────────────────────────────

/// Evaluate whether two skill names are similar enough to be merge candidates.
///
/// Returns `None` if the names are too dissimilar.
fn evaluate_merge_candidate(a: &str, b: &str) -> Option<MergeCandidate> {
    let norm_a = normalize_name(a);
    let norm_b = normalize_name(b);

    // Exact normalized match (different directories, same effective name)
    if norm_a == norm_b {
        return Some(MergeCandidate {
            skill_a: a.to_string(),
            skill_b: b.to_string(),
            similarity: 1.0,
            reason: format!("identical normalized name: '{norm_a}'"),
        });
    }

    // One is a prefix of the other (e.g., "deploy" and "deploy-k8s")
    if norm_a.starts_with(&norm_b) || norm_b.starts_with(&norm_a) {
        let shorter = norm_a.len().min(norm_b.len());
        let longer = norm_a.len().max(norm_b.len());
        let sim = shorter as f64 / longer as f64;
        if sim >= 0.6 {
            return Some(MergeCandidate {
                skill_a: a.to_string(),
                skill_b: b.to_string(),
                similarity: sim,
                reason: format!("name prefix overlap: '{norm_a}' / '{norm_b}'"),
            });
        }
    }

    // Levenshtein-based similarity
    let sim = string_similarity(&norm_a, &norm_b);
    if sim >= 0.75 {
        return Some(MergeCandidate {
            skill_a: a.to_string(),
            skill_b: b.to_string(),
            similarity: sim,
            reason: format!(
                "name similarity ({:.0}%): '{norm_a}' / '{norm_b}'",
                sim * 100.0
            ),
        });
    }

    None
}

/// Normalize a skill name for comparison: lowercase, replace hyphens/underscores with spaces.
fn normalize_name(name: &str) -> String {
    name.to_lowercase()
        .replace(['-', '_'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compute string similarity using normalized Levenshtein distance.
///
/// Returns a value between 0.0 (completely different) and 1.0 (identical).
fn string_similarity(a: &str, b: &str) -> f64 {
    if a == b {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let distance = levenshtein_distance(a, b);
    let max_len = a.len().max(b.len());
    1.0 - (distance as f64 / max_len as f64)
}

/// Compute the Levenshtein edit distance between two strings.
fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = a_chars.len();
    let m = b_chars.len();

    let mut prev = (0..=m).collect::<Vec<_>>();
    let mut curr = vec![0usize; m + 1];

    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = usize::from(a_chars[i - 1] != b_chars[j - 1]);
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[m]
}

/// Recursively copy a directory.
fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), McError> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)?.flatten() {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// LLM merge decision response shape.
#[derive(serde::Deserialize)]
struct LlmMergeDecision {
    a: String,
    b: String,
    merge: bool,
    #[allow(dead_code)]
    reason: String,
}

// ── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::fs;
    use tempfile::TempDir;

    fn make_skill_dir(base: &Path, name: &str) {
        let dir = base.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), format!("# {name}\nA skill.\n")).unwrap();
    }

    fn temp_curator() -> (TempDir, Curator) {
        let dir = TempDir::new().unwrap();
        let config = CuratorConfig::default();
        let curator = Curator::new(dir.path(), config);
        (dir, curator)
    }

    // ── CuratorConfig defaults ─────────────────────────────────────

    #[test]
    fn config_defaults() {
        let config = CuratorConfig::default();
        assert_eq!(config.stale_after_days, 30);
        assert_eq!(config.archive_after_days, 90);
        assert_eq!(config.run_interval_hours, 168);
    }

    // ── Usage persistence ──────────────────────────────────────────

    #[test]
    fn load_usage_empty_when_missing() {
        let (_dir, curator) = temp_curator();
        let usage = curator.load_usage().unwrap();
        assert!(usage.is_empty());
    }

    #[test]
    fn save_and_load_usage_roundtrip() {
        let (_dir, curator) = temp_curator();
        let mut usage = HashMap::new();
        usage.insert("test-skill".into(), SkillUsage::new(SkillOrigin::Hub));

        curator.save_usage(&usage).unwrap();
        let loaded = curator.load_usage().unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["test-skill"].state, SkillState::Active);
        assert_eq!(loaded["test-skill"].created_by, SkillOrigin::Hub);
    }

    #[test]
    fn load_usage_handles_empty_file() {
        let dir = TempDir::new().unwrap();
        let usage_path = dir.path().join(".usage.json");
        fs::write(&usage_path, "  ").unwrap();

        let curator = Curator::with_usage_path(dir.path(), &usage_path, CuratorConfig::default());
        let usage = curator.load_usage().unwrap();
        assert!(usage.is_empty());
    }

    // ── Skill discovery ────────────────────────────────────────────

    #[test]
    fn discover_skills_finds_skill_dirs() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "deploy");
        make_skill_dir(dir.path(), "test-runner");
        // Non-skill directory (no SKILL.md)
        fs::create_dir_all(dir.path().join("not-a-skill")).unwrap();

        let curator = Curator::new(dir.path(), CuratorConfig::default());
        let names = curator.discover_skills();
        assert_eq!(names, vec!["deploy", "test-runner"]);
    }

    #[test]
    fn discover_skills_empty_dir() {
        let (_dir, curator) = temp_curator();
        let names = curator.discover_skills();
        assert!(names.is_empty());
    }

    #[test]
    fn discover_skills_skips_hidden_dirs() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "real-skill");
        make_skill_dir(dir.path(), ".backups");

        let curator = Curator::new(dir.path(), CuratorConfig::default());
        let names = curator.discover_skills();
        assert_eq!(names, vec!["real-skill"]);
    }

    // ── Automatic transitions ──────────────────────────────────────

    #[test]
    fn transition_active_to_stale() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "old-skill");

        let config = CuratorConfig {
            stale_after_days: 30,
            archive_after_days: 90,
            run_interval_hours: 168,
        };
        let curator = Curator::new(dir.path(), config);

        // Pre-populate usage with old activity
        let mut usage = HashMap::new();
        let mut entry = SkillUsage::new(SkillOrigin::Agent);
        entry.last_activity_at = Utc::now() - Duration::days(45);
        usage.insert("old-skill".into(), entry);
        curator.save_usage(&usage).unwrap();

        let transitions = curator.apply_automatic_transitions().unwrap();
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].from, SkillState::Active);
        assert_eq!(transitions[0].to, SkillState::Stale);
        assert_eq!(transitions[0].skill_name, "old-skill");

        let loaded = curator.load_usage().unwrap();
        assert_eq!(loaded["old-skill"].state, SkillState::Stale);
    }

    #[test]
    fn transition_stale_to_archived() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "ancient-skill");

        let config = CuratorConfig {
            stale_after_days: 30,
            archive_after_days: 90,
            run_interval_hours: 168,
        };
        let curator = Curator::new(dir.path(), config);

        let mut usage = HashMap::new();
        let mut entry = SkillUsage::new(SkillOrigin::Agent);
        entry.state = SkillState::Stale;
        entry.last_activity_at = Utc::now() - Duration::days(100);
        usage.insert("ancient-skill".into(), entry);
        curator.save_usage(&usage).unwrap();

        let transitions = curator.apply_automatic_transitions().unwrap();
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].from, SkillState::Stale);
        assert_eq!(transitions[0].to, SkillState::Archived);

        let loaded = curator.load_usage().unwrap();
        assert_eq!(loaded["ancient-skill"].state, SkillState::Archived);
    }

    #[test]
    fn pinned_skills_never_transition() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "pinned-skill");

        let config = CuratorConfig {
            stale_after_days: 30,
            archive_after_days: 90,
            run_interval_hours: 168,
        };
        let curator = Curator::new(dir.path(), config);

        let mut usage = HashMap::new();
        let mut entry = SkillUsage::new(SkillOrigin::Agent);
        entry.pinned = true;
        entry.last_activity_at = Utc::now() - Duration::days(200);
        usage.insert("pinned-skill".into(), entry);
        curator.save_usage(&usage).unwrap();

        let transitions = curator.apply_automatic_transitions().unwrap();
        assert!(transitions.is_empty());

        let loaded = curator.load_usage().unwrap();
        assert_eq!(loaded["pinned-skill"].state, SkillState::Active);
    }

    #[test]
    fn pinned_state_never_transitions() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "pinned-state-skill");

        let curator = Curator::new(dir.path(), CuratorConfig::default());

        let mut usage = HashMap::new();
        let mut entry = SkillUsage::new(SkillOrigin::Agent);
        entry.state = SkillState::Pinned;
        entry.last_activity_at = Utc::now() - Duration::days(200);
        usage.insert("pinned-state-skill".into(), entry);
        curator.save_usage(&usage).unwrap();

        let transitions = curator.apply_automatic_transitions().unwrap();
        assert!(transitions.is_empty());

        let loaded = curator.load_usage().unwrap();
        assert_eq!(loaded["pinned-state-skill"].state, SkillState::Pinned);
    }

    #[test]
    fn active_skill_within_threshold_stays_active() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "fresh-skill");

        let curator = Curator::new(dir.path(), CuratorConfig::default());

        let mut usage = HashMap::new();
        let mut entry = SkillUsage::new(SkillOrigin::Agent);
        entry.last_activity_at = Utc::now() - Duration::days(5);
        usage.insert("fresh-skill".into(), entry);
        curator.save_usage(&usage).unwrap();

        let transitions = curator.apply_automatic_transitions().unwrap();
        assert!(transitions.is_empty());

        let loaded = curator.load_usage().unwrap();
        assert_eq!(loaded["fresh-skill"].state, SkillState::Active);
    }

    #[test]
    fn new_skill_dir_gets_usage_entry() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "brand-new");

        let curator = Curator::new(dir.path(), CuratorConfig::default());
        let transitions = curator.apply_automatic_transitions().unwrap();
        assert!(transitions.is_empty()); // fresh, so no transitions

        let loaded = curator.load_usage().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded["brand-new"].state, SkillState::Active);
        assert_eq!(loaded["brand-new"].created_by, SkillOrigin::Agent);
    }

    #[test]
    fn archived_skill_stays_archived() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "archived-one");

        let curator = Curator::new(dir.path(), CuratorConfig::default());

        let mut usage = HashMap::new();
        let mut entry = SkillUsage::new(SkillOrigin::Agent);
        entry.state = SkillState::Archived;
        entry.last_activity_at = Utc::now() - Duration::days(500);
        usage.insert("archived-one".into(), entry);
        curator.save_usage(&usage).unwrap();

        let transitions = curator.apply_automatic_transitions().unwrap();
        assert!(transitions.is_empty());
    }

    #[test]
    fn multiple_transitions_in_one_run() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "stale-one");
        make_skill_dir(dir.path(), "archive-one");

        let curator = Curator::new(dir.path(), CuratorConfig::default());

        let mut usage = HashMap::new();

        let mut entry_active = SkillUsage::new(SkillOrigin::Agent);
        entry_active.last_activity_at = Utc::now() - Duration::days(35);
        usage.insert("stale-one".into(), entry_active);

        let mut entry_stale = SkillUsage::new(SkillOrigin::Agent);
        entry_stale.state = SkillState::Stale;
        entry_stale.last_activity_at = Utc::now() - Duration::days(100);
        usage.insert("archive-one".into(), entry_stale);

        curator.save_usage(&usage).unwrap();

        let transitions = curator.apply_automatic_transitions().unwrap();
        assert_eq!(transitions.len(), 2);
    }

    // ── Backup ─────────────────────────────────────────────────────

    #[test]
    fn backup_creates_snapshot() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "skill-a");
        make_skill_dir(dir.path(), "skill-b");

        let curator = Curator::new(dir.path(), CuratorConfig::default());

        // Pre-populate usage
        let mut usage = HashMap::new();
        usage.insert("skill-a".into(), SkillUsage::new(SkillOrigin::Agent));
        curator.save_usage(&usage).unwrap();

        let backup_path = curator.backup().unwrap();
        assert!(backup_path.exists());
        assert!(backup_path.join("skill-a/SKILL.md").exists());
        assert!(backup_path.join("skill-b/SKILL.md").exists());
        assert!(backup_path.join(".usage.json").exists());
    }

    #[test]
    fn backup_preserves_file_contents() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "my-skill");

        let curator = Curator::new(dir.path(), CuratorConfig::default());
        let backup_path = curator.backup().unwrap();

        let original = fs::read_to_string(dir.path().join("my-skill/SKILL.md")).unwrap();
        let backed_up =
            fs::read_to_string(backup_path.join("my-skill/SKILL.md")).unwrap();
        assert_eq!(original, backed_up);
    }

    #[test]
    fn backup_on_empty_skills_dir() {
        let dir = TempDir::new().unwrap();
        let curator = Curator::new(dir.path(), CuratorConfig::default());

        let backup_path = curator.backup().unwrap();
        assert!(backup_path.exists());
    }

    // ── Consolidation ──────────────────────────────────────────────

    #[test]
    fn consolidation_finds_similar_names() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "deploy");
        make_skill_dir(dir.path(), "deploy-k8s");
        make_skill_dir(dir.path(), "unrelated");

        let curator = Curator::new(dir.path(), CuratorConfig::default());
        let report = curator.run_consolidation_sync().unwrap();

        assert_eq!(report.skills_scanned, 3);
        assert!(!report.merge_candidates.is_empty());

        // deploy + deploy-k8s should be a candidate
        let found = report.merge_candidates.iter().any(|c| {
            (c.skill_a == "deploy" && c.skill_b == "deploy-k8s")
                || (c.skill_a == "deploy-k8s" && c.skill_b == "deploy")
        });
        assert!(found, "expected deploy + deploy-k8s as merge candidate");
    }

    #[test]
    fn consolidation_identical_normalized_names() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "my-skill");
        make_skill_dir(dir.path(), "my_skill");

        let curator = Curator::new(dir.path(), CuratorConfig::default());
        let report = curator.run_consolidation_sync().unwrap();

        assert_eq!(report.merge_candidates.len(), 1);
        assert!((report.merge_candidates[0].similarity - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn consolidation_clean_when_dissimilar() {
        let dir = TempDir::new().unwrap();
        make_skill_dir(dir.path(), "alpha");
        make_skill_dir(dir.path(), "beta");
        make_skill_dir(dir.path(), "gamma");

        let curator = Curator::new(dir.path(), CuratorConfig::default());
        let report = curator.run_consolidation_sync().unwrap();

        assert!(report.is_clean());
    }

    #[test]
    fn consolidation_empty_skills_dir() {
        let (_dir, curator) = temp_curator();
        let report = curator.run_consolidation_sync().unwrap();
        assert_eq!(report.skills_scanned, 0);
        assert!(report.is_clean());
    }

    // ── String helpers ─────────────────────────────────────────────

    #[test]
    fn normalize_name_removes_special_chars() {
        assert_eq!(normalize_name("my-cool_skill"), "my cool skill");
        assert_eq!(normalize_name("UPPER_case"), "upper case");
        assert_eq!(normalize_name("  extra  spaces  "), "extra spaces");
    }

    #[test]
    fn levenshtein_identical() {
        assert_eq!(levenshtein_distance("abc", "abc"), 0);
    }

    #[test]
    fn levenshtein_empty() {
        assert_eq!(levenshtein_distance("", "abc"), 3);
        assert_eq!(levenshtein_distance("abc", ""), 3);
    }

    #[test]
    fn levenshtein_single_edit() {
        assert_eq!(levenshtein_distance("kitten", "sitten"), 1);
        assert_eq!(levenshtein_distance("kitten", "kittens"), 1);
    }

    #[test]
    fn string_similarity_identical() {
        assert!((string_similarity("hello", "hello") - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn string_similarity_different() {
        let sim = string_similarity("abc", "xyz");
        assert!(sim < 0.5);
    }

    // ── Copy dir recursive ─────────────────────────────────────────

    #[test]
    fn copy_dir_recursive_copies_files() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();

        fs::create_dir_all(src.path().join("sub")).unwrap();
        fs::write(src.path().join("root.txt"), "root").unwrap();
        fs::write(src.path().join("sub/nested.txt"), "nested").unwrap();

        copy_dir_recursive(src.path(), dst.path()).unwrap();

        assert_eq!(
            fs::read_to_string(dst.path().join("root.txt")).unwrap(),
            "root"
        );
        assert_eq!(
            fs::read_to_string(dst.path().join("sub/nested.txt")).unwrap(),
            "nested"
        );
    }
}
