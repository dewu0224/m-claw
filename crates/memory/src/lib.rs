//! Memory system (MEMORY.md / USER.md).
//!
//! This crate provides CRUD operations for agent knowledge
//! (MEMORY.md) and user profile (USER.md) files, with section-level
//! update and remove operations.
//!
//! # Example
//!
//! ```no_run
//! use std::path::Path;
//! use mc_memory::{MemoryStore, MemoryFile};
//!
//! let store = MemoryStore::new(Path::new("/data/memory"));
//!
//! // Append and read agent memory
//! store.append_agent_memory("## Preferences\nUser prefers concise replies.\n").unwrap();
//! let content = store.read_agent_memory().unwrap();
//!
//! // Update a section by heading
//! store.update_section(MemoryFile::Agent, "Preferences", "User prefers detailed replies.\n").unwrap();
//!
//! // Remove a section
//! store.remove_section(MemoryFile::Agent, "Preferences").unwrap();
//! ```

mod store;
mod types;

pub use store::MemoryStore;
pub use types::MemoryFile;
