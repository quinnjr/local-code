use crate::config::paths::Paths;
use crate::memory::buffer::{append_buffer_entry, maybe_rollover};
use crate::memory::core::{append_core_memory, read_core_memories};
use crate::memory::rollup::rollup_and_archive;
use crate::memory::search::search;
use chrono::{DateTime, Utc};
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
        writeln!(
            out,
            "{}:{}: {}",
            hit.file.display(),
            hit.line_number,
            hit.line
        )?;
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

pub fn add_command<W: Write>(paths: &Paths, text: &str, out: W) -> anyhow::Result<()> {
    add_command_at(paths, Utc::now(), text, out)
}

/// `add_command` with an injectable clock so tests can drive day boundaries.
/// This is the maintenance moment for the whole flat-file pipeline: a stale
/// (previous-day) buffer is rolled into its daily file BEFORE appending — so
/// today's entry lands in a fresh buffer stamped with today's date instead of
/// under the old header — and a rollover also ages daily files past the
/// recent window into `recent.md`/`archive.md`.
fn add_command_at<W: Write>(
    paths: &Paths,
    now: DateTime<Utc>,
    text: &str,
    mut out: W,
) -> anyhow::Result<()> {
    let dir = memory_dir(paths);
    if maybe_rollover(&dir, now)? {
        rollup_and_archive(&dir, now.date_naive())?;
    }
    append_buffer_entry(&dir, now, text)?;
    writeln!(out, "Recorded memory entry.")?;
    Ok(())
}

pub fn core_add_command<W: Write>(paths: &Paths, text: &str, mut out: W) -> anyhow::Result<()> {
    append_core_memory(&memory_dir(paths), Utc::now(), text)?;
    writeln!(out, "Recorded core memory.")?;
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

        add_command(
            &paths,
            "Remember this fact about the build.",
            &mut Vec::new(),
        )
        .unwrap();

        let mut out = Vec::new();
        search_command(&paths, "build", &mut out).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("Remember this fact about the build.")
        );
    }

    #[test]
    fn search_reports_no_matches() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());

        let mut out = Vec::new();
        search_command(&paths, "nonexistent-term", &mut out).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("No memory entries matched")
        );
    }

    #[test]
    fn add_rolls_a_stale_buffer_into_daily_files_and_archives_old_days() {
        use crate::memory::MemoryPaths;
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        let memory = memory_dir(&paths);

        // An entry recorded 40 days ago sits in the buffer...
        let long_ago = Utc.with_ymd_and_hms(2026, 6, 1, 9, 0, 0).unwrap();
        add_command_at(&paths, long_ago, "Ancient fact.", &mut Vec::new()).unwrap();

        // ...until the next add on a later day triggers the maintenance pass.
        let today = Utc.with_ymd_and_hms(2026, 7, 11, 9, 0, 0).unwrap();
        add_command_at(&paths, today, "Fresh fact.", &mut Vec::new()).unwrap();

        let mp = MemoryPaths::new(&memory);
        let buffer = std::fs::read_to_string(&mp.buffer).unwrap();
        assert!(buffer.starts_with("<!-- buffer-date: 2026-07-11 -->"));
        assert!(buffer.contains("Fresh fact."));
        assert!(!buffer.contains("Ancient fact."));
        // 40 days is past the recent window, so the rolled daily file was
        // immediately archived.
        let archive = std::fs::read_to_string(&mp.archive).unwrap();
        assert!(archive.contains("# 2026-06-01"));
        assert!(archive.contains("Ancient fact."));
    }

    #[test]
    fn core_add_command_appends_a_readable_core_memory() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());

        let mut out = Vec::new();
        core_add_command(&paths, "Ship small diffs.", &mut out).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("Recorded core memory.")
        );

        let mut printed = Vec::new();
        core_command(&paths, &mut printed).unwrap();
        assert!(
            String::from_utf8(printed)
                .unwrap()
                .contains("Ship small diffs.")
        );
    }

    #[test]
    fn core_command_reports_none_recorded_when_empty() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());

        let mut out = Vec::new();
        core_command(&paths, &mut out).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("No core memories recorded yet.")
        );
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
