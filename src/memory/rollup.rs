// src/memory/rollup.rs

use crate::memory::{ensure_dir, MemoryError, MemoryPaths};
use chrono::{Duration, NaiveDate};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

const RECENT_WINDOW_DAYS: i64 = 7;

pub fn rollup_and_archive(memory_dir: &Path, today: NaiveDate) -> Result<(), MemoryError> {
    ensure_dir(memory_dir)?;
    let paths = MemoryPaths::new(memory_dir);

    let mut daily_files = list_daily_files(memory_dir)?;
    daily_files.sort_by_key(|(date, _)| *date);

    let cutoff = today - Duration::days(RECENT_WINDOW_DAYS - 1);

    let mut recent_sections: Vec<(NaiveDate, String)> = Vec::new();
    let mut to_archive: Vec<(NaiveDate, String, PathBuf)> = Vec::new();

    for (date, path) in &daily_files {
        let contents = fs::read_to_string(path).map_err(|source| MemoryError::Read {
            path: path.clone(),
            source,
        })?;
        if *date < cutoff {
            to_archive.push((*date, contents, path.clone()));
        } else if *date <= today {
            recent_sections.push((*date, contents));
        }
    }

    if !to_archive.is_empty() {
        let mut archive_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&paths.archive)
            .map_err(|source| MemoryError::Write {
                path: paths.archive.clone(),
                source,
            })?;
        for (date, contents, daily_path) in &to_archive {
            writeln!(archive_file, "# {}\n", date.format("%Y-%m-%d")).map_err(|source| {
                MemoryError::Write {
                    path: paths.archive.clone(),
                    source,
                }
            })?;
            write!(archive_file, "{contents}").map_err(|source| MemoryError::Write {
                path: paths.archive.clone(),
                source,
            })?;
            writeln!(archive_file).map_err(|source| MemoryError::Write {
                path: paths.archive.clone(),
                source,
            })?;
            fs::remove_file(daily_path).map_err(|source| MemoryError::Write {
                path: daily_path.clone(),
                source,
            })?;
        }
    }

    let mut recent_content = String::new();
    for (date, contents) in &recent_sections {
        recent_content.push_str(&format!("# {}\n\n", date.format("%Y-%m-%d")));
        recent_content.push_str(contents);
        recent_content.push('\n');
    }
    fs::write(&paths.recent, recent_content).map_err(|source| MemoryError::Write {
        path: paths.recent.clone(),
        source,
    })?;

    Ok(())
}

fn list_daily_files(memory_dir: &Path) -> Result<Vec<(NaiveDate, PathBuf)>, MemoryError> {
    let mut result = Vec::new();
    if !memory_dir.exists() {
        return Ok(result);
    }
    let entries = fs::read_dir(memory_dir).map_err(|source| MemoryError::Read {
        path: memory_dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| MemoryError::Read {
            path: memory_dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if let Some(date_str) = file_name
            .strip_prefix("today-")
            .and_then(|s| s.strip_suffix(".md"))
        {
            if let Ok(date) = NaiveDate::parse_from_str(date_str, "%Y-%m-%d") {
                result.push((date, path));
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write_daily(memory_dir: &Path, date: NaiveDate, body: &str) {
        fs::create_dir_all(memory_dir).unwrap();
        let paths = MemoryPaths::new(memory_dir);
        fs::write(paths.daily(date), body).unwrap();
    }

    #[test]
    fn recent_md_contains_only_files_within_the_seven_day_window_oldest_first() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        let today = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();

        write_daily(&memory_dir, today - Duration::days(10), "## 09:00:00Z\nToo old.\n");
        write_daily(&memory_dir, today - Duration::days(6), "## 09:00:00Z\nOldest in window.\n");
        write_daily(&memory_dir, today, "## 09:00:00Z\nToday's entry.\n");

        rollup_and_archive(&memory_dir, today).unwrap();

        let paths = MemoryPaths::new(&memory_dir);
        let recent = fs::read_to_string(&paths.recent).unwrap();

        assert!(recent.contains("# 2026-07-04"));
        assert!(recent.contains("Oldest in window."));
        assert!(recent.contains("# 2026-07-10"));
        assert!(recent.contains("Today's entry."));
        assert!(!recent.contains("Too old."));

        let oldest_pos = recent.find("Oldest in window.").unwrap();
        let today_pos = recent.find("Today's entry.").unwrap();
        assert!(oldest_pos < today_pos, "expected oldest-first ordering in recent.md");
    }

    #[test]
    fn files_older_than_the_window_are_moved_into_archive_and_deleted() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        let today = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();
        let old_date = today - Duration::days(10);

        write_daily(&memory_dir, old_date, "## 09:00:00Z\nToo old.\n");

        rollup_and_archive(&memory_dir, today).unwrap();

        let paths = MemoryPaths::new(&memory_dir);
        assert!(!paths.daily(old_date).exists());

        let archive = fs::read_to_string(&paths.archive).unwrap();
        assert!(archive.contains("# 2026-06-30"));
        assert!(archive.contains("Too old."));
    }

    #[test]
    fn running_rollup_twice_does_not_duplicate_archive_content() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        let today = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();
        let old_date = today - Duration::days(10);
        write_daily(&memory_dir, old_date, "## 09:00:00Z\nToo old.\n");

        rollup_and_archive(&memory_dir, today).unwrap();
        rollup_and_archive(&memory_dir, today).unwrap();

        let paths = MemoryPaths::new(&memory_dir);
        let archive = fs::read_to_string(&paths.archive).unwrap();
        assert_eq!(archive.matches("Too old.").count(), 1);
    }

    #[test]
    fn no_daily_files_yields_empty_recent_and_no_archive_file() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        let today = NaiveDate::from_ymd_opt(2026, 7, 10).unwrap();

        rollup_and_archive(&memory_dir, today).unwrap();

        let paths = MemoryPaths::new(&memory_dir);
        let recent = fs::read_to_string(&paths.recent).unwrap();
        assert_eq!(recent, "");
        assert!(!paths.archive.exists());
    }
}
