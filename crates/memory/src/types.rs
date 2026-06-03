//! Memory file types.

/// Identifies which memory file to operate on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryFile {
    /// Agent knowledge base — `MEMORY.md`.
    Agent,
    /// User profile — `USER.md`.
    User,
}

impl MemoryFile {
    /// Returns the filename for this memory file type.
    pub fn filename(&self) -> &'static str {
        match self {
            MemoryFile::Agent => "MEMORY.md",
            MemoryFile::User => "USER.md",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filenames() {
        assert_eq!(MemoryFile::Agent.filename(), "MEMORY.md");
        assert_eq!(MemoryFile::User.filename(), "USER.md");
    }

    #[test]
    fn test_equality() {
        assert_eq!(MemoryFile::Agent, MemoryFile::Agent);
        assert_eq!(MemoryFile::User, MemoryFile::User);
        assert_ne!(MemoryFile::Agent, MemoryFile::User);
    }
}
