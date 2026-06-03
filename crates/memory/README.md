# mc-memory

Memory system for mavis-claw — CRUD operations for agent knowledge (`MEMORY.md`) and user profile (`USER.md`).

## Features

- **MemoryStore** — file-backed store rooted at a configurable `base_path`
- **MemoryFile enum** — `Agent` (MEMORY.md) / `User` (USER.md)
- **read / append / write** — full CRUD for both file types
- **update_section()** — locate `## heading` and replace body
- **remove_section()** — locate `## heading` and delete section
- UTF-8 safe, handles Chinese/multilingual content

## Usage

```rust
use std::path::Path;
use mc_memory::{MemoryStore, MemoryFile};

let store = MemoryStore::new(Path::new("/data/memory"));

// Append and read
store.append_agent_memory("## Preferences\nUser prefers concise replies.\n").unwrap();
let content = store.read_agent_memory().unwrap();

// Section operations
store.update_section(MemoryFile::Agent, "Preferences", "Detailed replies.\n").unwrap();
store.remove_section(MemoryFile::Agent, "Preferences").unwrap();
```

## Section Format

Sections are identified by `## heading` lines. A section's body extends from the line after the heading to the next `## ` heading or end of file.

```markdown
## Intro
Some introduction text.

## Notes
Notes go here.
```

## Dependencies

- `mc-core` — error types (`McError`)
- `tempfile` (dev) — test fixtures

## Tests

```bash
cargo test -p mc-memory
```

Covers: read/write roundtrip, UTF-8 content, section update/remove, missing file handling, pure section-parsing functions.
