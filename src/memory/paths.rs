use chrono::NaiveDate;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryPaths {
    pub dir: PathBuf,
    pub buffer: PathBuf,
    pub recent: PathBuf,
    pub archive: PathBuf,
    pub core_memories: PathBuf,
}

impl MemoryPaths {
    pub fn new(memory_dir: &Path) -> Self {
        MemoryPaths {
            dir: memory_dir.to_path_buf(),
            buffer: memory_dir.join("now.md"),
            recent: memory_dir.join("recent.md"),
            archive: memory_dir.join("archive.md"),
            core_memories: memory_dir.join("core-memories.md"),
        }
    }

    pub fn daily(&self, date: NaiveDate) -> PathBuf {
        self.dir
            .join(format!("today-{}.md", date.format("%Y-%m-%d")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn resolves_fixed_file_names_under_the_memory_dir() {
        let dir = Path::new("/project/.local-code/memory");
        let paths = MemoryPaths::new(dir);
        assert_eq!(paths.dir, dir);
        assert_eq!(paths.buffer, dir.join("now.md"));
        assert_eq!(paths.recent, dir.join("recent.md"));
        assert_eq!(paths.archive, dir.join("archive.md"));
        assert_eq!(paths.core_memories, dir.join("core-memories.md"));
    }

    #[test]
    fn daily_path_is_named_by_iso_date() {
        let dir = Path::new("/project/.local-code/memory");
        let paths = MemoryPaths::new(dir);
        let date = NaiveDate::from_ymd_opt(2026, 7, 6).unwrap();
        assert_eq!(paths.daily(date), dir.join("today-2026-07-06.md"));
    }
}
