# mc-skills

SKILL.md parsing and skill management for mavis-claw.

## Overview

This crate loads skill definitions from `SKILL.md` files on disk. Each skill
lives in its own directory and consists of a YAML frontmatter block followed
by Markdown content.

## SKILL.md Format

```markdown
---
trigger_words:
  - deploy
  - deployment
  - CI/CD
version: "1.0.0"
author: "team-name"
dependencies:
  - "lark-tools"
---

# Deploy Skill

Automate deployment workflows with one command.
```

**Frontmatter fields** (all optional):

| Field          | Type       | Description                              |
|----------------|------------|------------------------------------------|
| `trigger_words`| `string[]` | Keywords that activate this skill        |
| `version`      | `string`   | Semantic version                         |
| `author`       | `string`   | Skill author or maintainer               |
| `dependencies` | `string[]` | Names of other skills this one requires  |

## API

```rust
use std::path::Path;
use mc_skills::SkillLoader;

// Scan a directory tree for SKILL.md files
let mut loader = SkillLoader::new();
loader.load_dir(Path::new("/path/to/skills"))?;

// Fuzzy lookup by name (exact → case-insensitive → substring)
let skill = loader.load("deploy");

// Match by trigger words
let matches = loader.match_trigger("CI/CD");

// Generate summary for system prompt injection
println!("{}", loader.summary());
```

## Tests

```bash
cargo test -p mc-skills
```

13 unit tests covering:
- Normal skill loading (single + multiple)
- Missing directory / missing file
- Malformed YAML frontmatter
- Missing frontmatter
- Trigger word matching (exact + case-insensitive)
- Fuzzy name lookup (case-insensitive + substring)
- Summary generation (single, multiple, empty)
