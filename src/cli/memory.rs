// src/cli/memory.rs

use crate::config::paths::Paths;
use crate::memory::buffer::append_buffer_entry;
use crate::memory::core::read_core_memories;
use crate::memory::search::search;
use chrono::Utc;
use std::io::Write;

fn memory_dir(paths: &Paths) -> std::path::PathBuf {
    paths.project_config_dir.join("memory")
}

pub fn search_command<W: Write>(paths: &Paths, query: &str, mut out: W) -> anyhow::Result<()> {
    let hits = search(&memory_dir(paths), query)?;
    if hits.is_empty() {
        writeln!(out, "No memory entries matched '{query}'.")?;
        return Ok(());
    }
    for hit in hits {
        writeln!(out, "{}:{}: {}", hit.file.display(), hit.line_number, hit.line)?;
    }
    Ok(())
}

pub fn core_command<W: Write>(paths: &Paths, mut out: W) -> anyhow::Result<()> {
    let content = read_core_memories(&memory_dir(paths))?;
    if content.is_empty() {
        writeln!(out, "No core memories recorded yet.")?;
    } else {
        write!(out, "{content}")?;
    }
    Ok(())
}

pub fn add_command<W: Write>(paths: &Paths, text: &str, mut out: W) -> anyhow::Result<()> {
    append_buffer_entry(&memory_dir(paths), Utc::now(), text)?;
    writeln!(out, "Recorded memory entry.")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::core::append_core_memory;
    use chrono::TimeZone;
    use tempfile::tempdir;

    fn test_paths(project_dir: &std::path::Path) -> Paths {
        Paths {
            user_config_dir: project_dir.join("user-config-unused"),
            project_config_dir: project_dir.to_path_buf(),
            user_state_dir: project_dir.join("state-unused"),
        }
    }

    #[test]
    fn add_then_search_finds_the_new_entry() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());

        add_command(&paths, "Remember this fact about the build.", &mut Vec::new()).unwrap();

        let mut out = Vec::new();
        search_command(&paths, "build", &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("Remember this fact about the build."));
    }

    #[test]
    fn search_reports_no_matches() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());

        let mut out = Vec::new();
        search_command(&paths, "nonexistent-term", &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("No memory entries matched"));
    }

    #[test]
    fn core_command_reports_none_recorded_when_empty() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());

        let mut out = Vec::new();
        core_command(&paths, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("No core memories recorded yet."));
    }

    #[test]
    fn core_command_prints_full_core_memories_file() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        append_core_memory(
            &memory_dir(&paths),
            Utc.with_ymd_and_hms(2026, 6, 15, 0, 0, 0).unwrap(),
            "Never use unwrap() outside tests.",
        )
        .unwrap();

        let mut out = Vec::new();
        core_command(&paths, &mut out).unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("## 2026-06-15"));
        assert!(printed.contains("Never use unwrap() outside tests."));
    }
}
