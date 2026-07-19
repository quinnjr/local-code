use crate::memory::{MemoryError, MemoryPaths, ensure_dir};
use chrono::{DateTime, NaiveDate, Utc};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;

pub fn append_buffer_entry(
    memory_dir: &Path,
    now: DateTime<Utc>,
    text: &str,
) -> Result<(), MemoryError> {
    ensure_dir(memory_dir)?;
    let paths = MemoryPaths::new(memory_dir);
    let is_new = !paths.buffer.exists();

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.buffer)
        .map_err(|source| MemoryError::Write {
            path: paths.buffer.clone(),
            source,
        })?;

    if is_new {
        writeln!(
            file,
            "<!-- buffer-date: {} -->\n",
            now.date_naive().format("%Y-%m-%d")
        )
        .map_err(|source| MemoryError::Write {
            path: paths.buffer.clone(),
            source,
        })?;
    }

    writeln!(file, "## {}\n{}\n", now.format("%H:%M:%SZ"), text.trim()).map_err(|source| {
        MemoryError::Write {
            path: paths.buffer.clone(),
            source,
        }
    })?;

    Ok(())
}

pub fn maybe_rollover(memory_dir: &Path, now: DateTime<Utc>) -> Result<bool, MemoryError> {
    let paths = MemoryPaths::new(memory_dir);
    if !paths.buffer.exists() {
        return Ok(false);
    }

    let contents = fs::read_to_string(&paths.buffer).map_err(|source| MemoryError::Read {
        path: paths.buffer.clone(),
        source,
    })?;

    let today = now.date_naive();
    let buffer_date = match parse_buffer_date(&contents) {
        Some(date) => date,
        None => return Ok(false),
    };

    if buffer_date >= today {
        return Ok(false);
    }

    let body = strip_buffer_header(&contents);
    let daily_path = paths.daily(buffer_date);
    let mut daily_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&daily_path)
        .map_err(|source| MemoryError::Write {
            path: daily_path.clone(),
            source,
        })?;
    write!(daily_file, "{body}").map_err(|source| MemoryError::Write {
        path: daily_path.clone(),
        source,
    })?;

    fs::remove_file(&paths.buffer).map_err(|source| MemoryError::Write {
        path: paths.buffer.clone(),
        source,
    })?;

    Ok(true)
}

fn parse_buffer_date(contents: &str) -> Option<NaiveDate> {
    let first_line = contents.lines().next()?;
    let date_str = first_line
        .strip_prefix("<!-- buffer-date: ")?
        .strip_suffix(" -->")?;
    NaiveDate::parse_from_str(date_str, "%Y-%m-%d").ok()
}

fn strip_buffer_header(contents: &str) -> String {
    let mut body: String = contents.lines().skip(1).collect::<Vec<_>>().join("\n");
    body.push('\n');
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::tempdir;

    fn dt(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, m, d, h, min, s).unwrap()
    }

    #[test]
    fn append_creates_buffer_with_date_header_and_entry() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        append_buffer_entry(&memory_dir, dt(2026, 7, 6, 14, 32, 5), "First memory.").unwrap();

        let paths = MemoryPaths::new(&memory_dir);
        let contents = fs::read_to_string(&paths.buffer).unwrap();
        assert!(contents.starts_with("<!-- buffer-date: 2026-07-06 -->"));
        assert!(contents.contains("## 14:32:05Z"));
        assert!(contents.contains("First memory."));
    }

    #[test]
    fn appending_twice_keeps_a_single_header_and_two_entries() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        append_buffer_entry(&memory_dir, dt(2026, 7, 6, 9, 0, 0), "Entry one.").unwrap();
        append_buffer_entry(&memory_dir, dt(2026, 7, 6, 10, 0, 0), "Entry two.").unwrap();

        let paths = MemoryPaths::new(&memory_dir);
        let contents = fs::read_to_string(&paths.buffer).unwrap();
        assert_eq!(contents.matches("<!-- buffer-date:").count(), 1);
        assert!(contents.contains("Entry one."));
        assert!(contents.contains("Entry two."));
    }

    #[test]
    fn maybe_rollover_is_noop_when_buffer_matches_today() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        append_buffer_entry(&memory_dir, dt(2026, 7, 6, 9, 0, 0), "Same-day entry.").unwrap();

        let rolled = maybe_rollover(&memory_dir, dt(2026, 7, 6, 23, 0, 0)).unwrap();
        assert!(!rolled);

        let paths = MemoryPaths::new(&memory_dir);
        assert!(paths.buffer.exists());
        assert!(
            !paths
                .daily(NaiveDate::from_ymd_opt(2026, 7, 6).unwrap())
                .exists()
        );
    }

    #[test]
    fn maybe_rollover_is_noop_when_buffer_missing() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        let rolled = maybe_rollover(&memory_dir, dt(2026, 7, 6, 9, 0, 0)).unwrap();
        assert!(!rolled);
    }

    #[test]
    fn maybe_rollover_moves_stale_buffer_into_its_daily_file_and_clears_buffer() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        append_buffer_entry(&memory_dir, dt(2026, 7, 5, 14, 32, 5), "Yesterday's entry.").unwrap();

        let rolled = maybe_rollover(&memory_dir, dt(2026, 7, 6, 8, 0, 0)).unwrap();
        assert!(rolled);

        let paths = MemoryPaths::new(&memory_dir);
        assert!(!paths.buffer.exists());

        let daily_path = paths.daily(NaiveDate::from_ymd_opt(2026, 7, 5).unwrap());
        let daily_contents = fs::read_to_string(&daily_path).unwrap();
        assert!(!daily_contents.contains("buffer-date"));
        assert!(daily_contents.contains("## 14:32:05Z"));
        assert!(daily_contents.contains("Yesterday's entry."));
    }

    #[test]
    fn append_after_rollover_starts_a_fresh_buffer_with_new_date() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        append_buffer_entry(&memory_dir, dt(2026, 7, 5, 14, 32, 5), "Yesterday's entry.").unwrap();
        maybe_rollover(&memory_dir, dt(2026, 7, 6, 8, 0, 0)).unwrap();
        append_buffer_entry(&memory_dir, dt(2026, 7, 6, 9, 0, 0), "Today's entry.").unwrap();

        let paths = MemoryPaths::new(&memory_dir);
        let contents = fs::read_to_string(&paths.buffer).unwrap();
        assert!(contents.starts_with("<!-- buffer-date: 2026-07-06 -->"));
        assert!(contents.contains("Today's entry."));
        assert!(!contents.contains("Yesterday's entry."));
    }
}
