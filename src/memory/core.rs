use crate::memory::{MemoryError, MemoryPaths, ensure_dir};
use chrono::{DateTime, Utc};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;

pub fn read_core_memories(memory_dir: &Path) -> Result<String, MemoryError> {
    let paths = MemoryPaths::new(memory_dir);
    if !paths.core_memories.exists() {
        return Ok(String::new());
    }
    fs::read_to_string(&paths.core_memories).map_err(|source| MemoryError::Read {
        path: paths.core_memories.clone(),
        source,
    })
}

pub fn append_core_memory(
    memory_dir: &Path,
    now: DateTime<Utc>,
    text: &str,
) -> Result<(), MemoryError> {
    ensure_dir(memory_dir)?;
    let paths = MemoryPaths::new(memory_dir);
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.core_memories)
        .map_err(|source| MemoryError::Write {
            path: paths.core_memories.clone(),
            source,
        })?;
    writeln!(file, "## {}\n{}\n", now.format("%Y-%m-%d"), text.trim()).map_err(|source| {
        MemoryError::Write {
            path: paths.core_memories.clone(),
            source,
        }
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::tempdir;

    fn dt(y: i32, m: u32, d: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, 0, 0, 0).unwrap()
    }

    #[test]
    fn read_core_memories_returns_empty_string_when_file_missing() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        let content = read_core_memories(&memory_dir).unwrap();
        assert_eq!(content, "");
    }

    #[test]
    fn append_then_read_round_trips() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        append_core_memory(
            &memory_dir,
            dt(2026, 6, 15),
            "This project never uses unwrap() outside of tests.",
        )
        .unwrap();

        let content = read_core_memories(&memory_dir).unwrap();
        assert!(content.contains("## 2026-06-15"));
        assert!(content.contains("This project never uses unwrap() outside of tests."));
    }

    #[test]
    fn appending_twice_keeps_both_entries() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        append_core_memory(&memory_dir, dt(2026, 6, 15), "First core memory.").unwrap();
        append_core_memory(&memory_dir, dt(2026, 7, 1), "Second core memory.").unwrap();

        let content = read_core_memories(&memory_dir).unwrap();
        assert!(content.contains("First core memory."));
        assert!(content.contains("Second core memory."));
    }
}
