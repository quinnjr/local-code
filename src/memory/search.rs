use crate::memory::{MemoryError, MemoryPaths};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryHit {
    pub file: PathBuf,
    pub line_number: usize,
    pub line: String,
}

pub fn search(memory_dir: &Path, query: &str) -> Result<Vec<MemoryHit>, MemoryError> {
    let mut hits = Vec::new();
    if !memory_dir.exists() || query.is_empty() {
        return Ok(hits);
    }

    // A case-insensitive matcher compiled once, instead of allocating a
    // lowercased copy of every scanned line (the previous
    // `line.to_lowercase().contains(...)` paid one heap allocation per line
    // across every memory file). `(?i)` uses Unicode simple case folding,
    // matching the old `to_lowercase` semantics for practical inputs.
    let matcher = regex::RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
        .expect("escaped literal query always compiles");
    let paths = MemoryPaths::new(memory_dir);

    let mut files: Vec<PathBuf> = Vec::new();
    if paths.buffer.exists() {
        files.push(paths.buffer.clone());
    }

    let entries = fs::read_dir(memory_dir).map_err(|source| MemoryError::Read {
        path: memory_dir.to_path_buf(),
        source,
    })?;
    let mut daily: Vec<PathBuf> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| MemoryError::Read {
            path: memory_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if let Some(name) = path.file_name().and_then(|n| n.to_str())
            && name.starts_with("today-")
            && name.ends_with(".md")
        {
            daily.push(path);
        }
    }
    daily.sort();
    files.extend(daily);

    if paths.recent.exists() {
        files.push(paths.recent.clone());
    }
    if paths.archive.exists() {
        files.push(paths.archive.clone());
    }
    // Deliberately excludes paths.core_memories: core memories are always loaded in
    // full by callers (see read_core_memories), never searched on demand.

    for file in files {
        let contents = fs::read_to_string(&file).map_err(|source| MemoryError::Read {
            path: file.clone(),
            source,
        })?;
        for (idx, line) in contents.lines().enumerate() {
            if matcher.is_match(line) {
                hits.push(MemoryHit {
                    file: file.clone(),
                    line_number: idx + 1,
                    line: line.to_string(),
                });
            }
        }
    }

    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(memory_dir: &Path, name: &str, contents: &str) {
        fs::create_dir_all(memory_dir).unwrap();
        fs::write(memory_dir.join(name), contents).unwrap();
    }

    #[test]
    fn finds_case_insensitive_matches_in_buffer() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        write(
            &memory_dir,
            "now.md",
            "<!-- buffer-date: 2026-07-06 -->\n\n## 09:00:00Z\nFixed the Flaky test.\n",
        );

        let hits = search(&memory_dir, "flaky").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, "Fixed the Flaky test.");
        assert!(hits[0].file.ends_with("now.md"));
    }

    #[test]
    fn finds_matches_across_daily_recent_and_archive_files() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        write(
            &memory_dir,
            "today-2026-07-05.md",
            "## 09:00:00Z\nDaily file mentions widgets.\n",
        );
        write(
            &memory_dir,
            "recent.md",
            "# 2026-07-06\n\n## 09:00:00Z\nRecent file mentions widgets too.\n",
        );
        write(
            &memory_dir,
            "archive.md",
            "# 2026-06-01\n\n## 09:00:00Z\nArchived widgets note.\n",
        );

        let hits = search(&memory_dir, "widgets").unwrap();
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn does_not_search_core_memories_file() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        write(
            &memory_dir,
            "core-memories.md",
            "## 2026-06-15\nNever use unwrap() outside tests.\n",
        );

        let hits = search(&memory_dir, "unwrap").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn returns_empty_when_memory_dir_does_not_exist() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("does-not-exist");
        let hits = search(&memory_dir, "anything").unwrap();
        assert!(hits.is_empty());
    }

    #[test]
    fn line_numbers_are_one_indexed() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        write(
            &memory_dir,
            "now.md",
            "line one\nline two matches HERE\nline three\n",
        );

        let hits = search(&memory_dir, "here").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line_number, 2);
    }
}
