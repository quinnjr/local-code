# Flat-File Cross-Session Memory Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a pure file-I/O, grep-searchable, human-readable/git-diffable cross-session memory
store for a project: a short-term buffer, dated daily files, a rolling 7-day recent window, an
archive of older entries, and an always-loaded core-memories file. Expose this as a
`local_code::memory` module (zero dependency on `daimon`/`ntui`, fully unit-testable with
`tempfile`) plus a small `local-code memory` CLI for manual inspection. This plan does **not** wire
memory into the agent loop or register a `#[tool_fn]` — that is the core-agent-loop phase's job; it
will consume the functions and types defined here.

**Architecture:** Memory files live under `<project_root>/.local-code/memory/` — i.e.
`paths.project_config_dir.join("memory")`, reusing `local_code::config::paths::Paths` from the
Phase 1 foundation plan (`docs/superpowers/plans/2026-07-06-foundation-config-connections.md`).
This is a **deliberate choice over `user_state_dir`**: the spec explicitly requires memory to be
"git-diffable" (section 9), and `user_state_dir` (XDG state dir under the user's home) is
per-machine, not part of the project repo, and would never be committed — it cannot satisfy
"git-diffable" by construction. `project_config_dir` (`.local-code/`) is the project-local,
repo-committable directory the spec's own example path (`.local-code/memory/`) points at, so that's
where it goes. There is no separate cross-project/user-level memory store in this plan — the spec
describes exactly one flat-file memory area per project, and a second cross-project store is not
requested; it can be added later as its own plan if a need emerges. Dates and timestamps use the
`chrono` crate (UTC) rather than hand-rolled string parsing, since daily-file naming and rollover
comparisons need real calendar-date arithmetic (adding days, comparing dates), which `std` does not
provide. Each file type is plain Markdown with a small, fixed structure so a human (or the model,
reading `core-memories.md` as raw context) can read it without any tooling. Recall is a linear
substring scan (`str::contains`, case-insensitive) over the buffer, daily files, `recent.md`, and
`archive.md` — no index, no embeddings, consistent with the spec's "grep/keyword search on
request, not embedding similarity." `core-memories.md` is excluded from search because it is not
searched on demand — it is always loaded in full by callers (the future agent-loop phase will
prepend it to context every session, the same way `AGENTS.md`/`CLAUDE.md` are loaded per section 4
of the design spec).

**Tech Stack:** Rust 2024 edition, `chrono` 0.4 (`clock` feature) for dates/timestamps,
`thiserror` for typed `MemoryError`, `tempfile` (dev-dependency) for filesystem test fixtures,
`clap` derive (already a dependency from Phase 1) for the `local-code memory` subcommand, `anyhow`
in the CLI glue.

---

## Spec traceability

This plan implements spec section 9 ("Cross-session memory") from
`docs/superpowers/specs/2026-07-06-local-code-tui-design.md`:

- "short-term buffer file" → `now.md`, written/read via `local_code::memory::buffer`.
- "dated daily files" → `today-YYYY-MM-DD.md`, produced by
  `local_code::memory::buffer::maybe_rollover`.
- "a rolled-up recent-window file" → `recent.md`, produced by
  `local_code::memory::rollup::rollup_and_archive`.
- "an archive" → `archive.md`, produced by the same `rollup_and_archive` call.
- "recalled by grep/keyword search on request" → `local_code::memory::search::search`, returning
  `Vec<MemoryHit>`.
- "core-memories.md for key moments" (named explicitly in the spec's `.remember/`-convention
  description) → `local_code::memory::core::{read_core_memories, append_core_memory}`.

**Types/functions the core-agent-loop phase will depend on** (import verbatim, do not redefine):

- `local_code::memory::MemoryPaths::new(memory_dir: &Path) -> MemoryPaths` — resolves all memory
  file paths from a single directory.
- `local_code::memory::buffer::append_buffer_entry(memory_dir: &Path, now: DateTime<Utc>, text: &str) -> Result<(), MemoryError>`
  — the primitive a future `#[tool_fn]`-wrapped "remember this" tool will call.
- `local_code::memory::buffer::maybe_rollover(memory_dir: &Path, now: DateTime<Utc>) -> Result<bool, MemoryError>`
  — the agent loop must call this once at session start (per this plan's design) before doing
  anything else with memory, so a stale buffer from a prior day is folded into its daily file
  before new entries are appended.
- `local_code::memory::rollup::rollup_and_archive(memory_dir: &Path, today: NaiveDate) -> Result<(), MemoryError>`
  — the agent loop should call this at session start too (after `maybe_rollover`), so `recent.md`
  stays a true 7-day window and old daily files get archived.
- `local_code::memory::search::search(memory_dir: &Path, query: &str) -> Result<Vec<MemoryHit>, MemoryError>`
  — the exact function the future memory-recall `#[tool_fn]` will call; `MemoryHit { file, line_number, line }`
  is the shape that tool will return to the model.
- `local_code::memory::core::read_core_memories(memory_dir: &Path) -> Result<String, MemoryError>`
  — the agent loop calls this once at session start and prepends the (possibly empty) result to
  the system prompt, the same way it prepends `AGENTS.md`/`CLAUDE.md`.

None of this plan's code imports `daimon` or `ntui`; every function takes a `&Path` and returns a
`Result`, making it directly unit-testable without a running agent or TUI.

---

## File structure

- Modify: `Cargo.toml` — add `chrono` dependency.
- Create: `src/memory/mod.rs` — `MemoryError`, `pub mod` declarations, `ensure_dir` helper.
- Create: `src/memory/paths.rs` — `MemoryPaths`.
- Create: `src/memory/buffer.rs` — `append_buffer_entry`, `maybe_rollover`.
- Create: `src/memory/rollup.rs` — `rollup_and_archive` (recent-window rebuild + archiving).
- Create: `src/memory/core.rs` — `read_core_memories`, `append_core_memory`.
- Create: `src/memory/search.rs` — `MemoryHit`, `search`.
- Modify: `src/lib.rs` — add `pub mod memory;`.
- Create: `src/cli/memory.rs` — `search_command`, `core_command`, `add_command`.
- Modify: `src/cli/mod.rs` — add `Command::Memory { action: MemoryAction }` and dispatch.

---

## Markdown file formats (concrete, not vague)

**`now.md` (buffer)** — first line is a machine-readable date header, then one `##`-headed section
per entry, timestamped to the second:

```markdown
<!-- buffer-date: 2026-07-06 -->

## 14:32:05Z
Fixed the flaky test in connection loader by switching to a fresh tempdir per test case.

## 15:10:22Z
Discovered that keyring's mock builder must be set once per process, not per test.
```

**`today-2026-07-06.md` (daily)** — the buffer's entry sections, verbatim, once rolled over (no
date header line — the date is in the filename):

```markdown
## 14:32:05Z
Fixed the flaky test in connection loader by switching to a fresh tempdir per test case.

## 15:10:22Z
Discovered that keyring's mock builder must be set once per process, not per test.
```

**`recent.md`** — rebuilt (overwritten) on every rollup call, concatenating in-window daily files
oldest-first under a `#`-level date header:

```markdown
# 2026-06-30

## 09:12:40Z
Set up the initial CI workflow for local-code.

# 2026-07-06

## 14:32:05Z
Fixed the flaky test in connection loader by switching to a fresh tempdir per test case.
```

**`archive.md`** — append-only, one `#`-level date header per archived day, oldest entries at the
top over time:

```markdown
# 2026-06-01

## 10:00:00Z
Initial project scaffold committed.
```

**`core-memories.md`** — append-only, `##`-headed by date (not time — these are meant to be sparse,
important, human-curated-or-agent-flagged facts, not a full log):

```markdown
## 2026-06-15
This project never uses `unwrap()` outside of tests — always propagate errors with `?`.

## 2026-07-01
The user's default connection is `local-vllm`; only switch models when explicitly asked.
```

---

### Task 1: `chrono` dependency + memory module skeleton

**Files:**
- Modify: `Cargo.toml`
- Create: `src/memory/mod.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Add `chrono` to `Cargo.toml`**

Add to the `[dependencies]` section (alongside the Phase 1 dependencies already present there):

```toml
chrono = { version = "0.4", features = ["clock"] }
```

- [ ] **Step 2: Run `cargo check` to confirm it resolves**

Run: `cargo check`
Expected: builds (warnings about unused code are fine), no errors.

- [ ] **Step 3: Write the failing test for `MemoryError` and `ensure_dir`**

```rust
// src/memory/mod.rs

pub mod buffer;
pub mod core;
pub mod paths;
pub mod rollup;
pub mod search;

pub use paths::MemoryPaths;

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to create directory {path}: {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

pub(crate) fn ensure_dir(dir: &Path) -> Result<(), MemoryError> {
    fs::create_dir_all(dir).map_err(|source| MemoryError::CreateDir {
        path: dir.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn ensure_dir_creates_nested_directories() {
        let root = tempdir().unwrap();
        let nested = root.path().join("a").join("b").join("memory");
        assert!(!nested.exists());
        ensure_dir(&nested).unwrap();
        assert!(nested.is_dir());
    }

    #[test]
    fn ensure_dir_is_idempotent_on_existing_directory() {
        let root = tempdir().unwrap();
        ensure_dir(root.path()).unwrap();
        ensure_dir(root.path()).unwrap();
        assert!(root.path().is_dir());
    }
}
```

This references `pub mod buffer; pub mod core; pub mod paths; pub mod rollup; pub mod search;`
which don't exist as files yet — create empty placeholder files so the crate compiles before
running the test:

Create `src/memory/paths.rs` with just:

```rust
// placeholder — implemented in Task 2
```

Create `src/memory/buffer.rs`, `src/memory/rollup.rs`, `src/memory/core.rs`, `src/memory/search.rs`
each with just:

```rust
// placeholder — implemented in a later task
```

Add to `src/lib.rs`:

```rust
pub mod config;
pub mod cli;
pub mod memory;
```

- [ ] **Step 4: Run the test to verify it fails**

Run: `cargo test --lib memory::tests`
Expected: FAIL to compile initially if `src/memory/mod.rs` didn't exist before this step — create
it with the exact content from Step 3, then re-run. It should compile and PASS immediately since
`ensure_dir` is a real (non-`todo!()`) implementation from the start — there is no partial-failure
step here because directory creation has no meaningful "wrong" intermediate implementation to
regress from.

- [ ] **Step 5: Run the test to verify it passes**

Run: `cargo test --lib memory::tests`
Expected: PASS (2 tests)

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/memory/mod.rs src/memory/paths.rs src/memory/buffer.rs src/memory/rollup.rs src/memory/core.rs src/memory/search.rs
git commit -m "chore: add memory module skeleton and chrono dependency"
```

---

### Task 2: `MemoryPaths`

**Files:**
- Create: `src/memory/paths.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/memory/paths.rs

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
        todo!()
    }

    pub fn daily(&self, date: NaiveDate) -> PathBuf {
        todo!()
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib memory::paths`
Expected: FAIL (`not yet implemented` panics from the two `todo!()`s).

- [ ] **Step 3: Implement `MemoryPaths`**

Replace both `todo!()` bodies:

```rust
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
        self.dir.join(format!("today-{}.md", date.format("%Y-%m-%d")))
    }
}
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib memory::paths`
Expected: PASS (2 tests)

- [ ] **Step 5: Commit**

```bash
git add src/memory/paths.rs
git commit -m "feat: resolve fixed memory file paths via MemoryPaths"
```

---

### Task 3: Buffer append + daily rollover

**Files:**
- Create: `src/memory/buffer.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/memory/buffer.rs

use crate::memory::{ensure_dir, MemoryError, MemoryPaths};
use chrono::{DateTime, NaiveDate, Utc};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;

pub fn append_buffer_entry(
    memory_dir: &Path,
    now: DateTime<Utc>,
    text: &str,
) -> Result<(), MemoryError> {
    todo!()
}

pub fn maybe_rollover(memory_dir: &Path, now: DateTime<Utc>) -> Result<bool, MemoryError> {
    todo!()
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
        assert!(!paths.daily(NaiveDate::from_ymd_opt(2026, 7, 6).unwrap()).exists());
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib memory::buffer`
Expected: FAIL (`not yet implemented` panics from the two `todo!()`s).

- [ ] **Step 3: Implement `append_buffer_entry` and `maybe_rollover`**

Replace both `todo!()` bodies:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib memory::buffer`
Expected: PASS (6 tests)

- [ ] **Step 5: Commit**

```bash
git add src/memory/buffer.rs
git commit -m "feat: append buffer entries and roll stale buffers into daily files"
```

---

### Task 4: Recent-window rollup + archiving

**Files:**
- Create: `src/memory/rollup.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/memory/rollup.rs

use crate::memory::{ensure_dir, MemoryError, MemoryPaths};
use chrono::{Duration, NaiveDate};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

const RECENT_WINDOW_DAYS: i64 = 7;

pub fn rollup_and_archive(memory_dir: &Path, today: NaiveDate) -> Result<(), MemoryError> {
    todo!()
}

fn list_daily_files(memory_dir: &Path) -> Result<Vec<(NaiveDate, PathBuf)>, MemoryError> {
    todo!()
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib memory::rollup`
Expected: FAIL (`not yet implemented` panics).

- [ ] **Step 3: Implement `rollup_and_archive` and `list_daily_files`**

Replace both `todo!()` bodies:

```rust
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib memory::rollup`
Expected: PASS (4 tests)

- [ ] **Step 5: Commit**

```bash
git add src/memory/rollup.rs
git commit -m "feat: rebuild recent.md 7-day window and archive older daily files"
```

---

### Task 5: Core memories (always-loaded, never searched)

**Files:**
- Create: `src/memory/core.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/memory/core.rs

use crate::memory::{ensure_dir, MemoryError, MemoryPaths};
use chrono::{DateTime, Utc};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::Path;

pub fn read_core_memories(memory_dir: &Path) -> Result<String, MemoryError> {
    todo!()
}

pub fn append_core_memory(memory_dir: &Path, now: DateTime<Utc>, text: &str) -> Result<(), MemoryError> {
    todo!()
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib memory::core`
Expected: FAIL (`not yet implemented` panics).

- [ ] **Step 3: Implement `read_core_memories` and `append_core_memory`**

Replace both `todo!()` bodies:

```rust
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

pub fn append_core_memory(memory_dir: &Path, now: DateTime<Utc>, text: &str) -> Result<(), MemoryError> {
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib memory::core`
Expected: PASS (3 tests)

- [ ] **Step 5: Commit**

```bash
git add src/memory/core.rs
git commit -m "feat: read and append always-loaded core memories"
```

---

### Task 6: Keyword search/recall across memory files

**Files:**
- Create: `src/memory/search.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/memory/search.rs

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
    todo!()
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
        write(&memory_dir, "now.md", "<!-- buffer-date: 2026-07-06 -->\n\n## 09:00:00Z\nFixed the Flaky test.\n");

        let hits = search(&memory_dir, "flaky").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, "Fixed the Flaky test.");
        assert!(hits[0].file.ends_with("now.md"));
    }

    #[test]
    fn finds_matches_across_daily_recent_and_archive_files() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        write(&memory_dir, "today-2026-07-05.md", "## 09:00:00Z\nDaily file mentions widgets.\n");
        write(&memory_dir, "recent.md", "# 2026-07-06\n\n## 09:00:00Z\nRecent file mentions widgets too.\n");
        write(&memory_dir, "archive.md", "# 2026-06-01\n\n## 09:00:00Z\nArchived widgets note.\n");

        let hits = search(&memory_dir, "widgets").unwrap();
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn does_not_search_core_memories_file() {
        let dir = tempdir().unwrap();
        let memory_dir = dir.path().join("memory");
        write(&memory_dir, "core-memories.md", "## 2026-06-15\nNever use unwrap() outside tests.\n");

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
        write(&memory_dir, "now.md", "line one\nline two matches HERE\nline three\n");

        let hits = search(&memory_dir, "here").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line_number, 2);
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib memory::search`
Expected: FAIL (`not yet implemented` panic).

- [ ] **Step 3: Implement `search`**

Replace the `todo!()` body:

```rust
pub fn search(memory_dir: &Path, query: &str) -> Result<Vec<MemoryHit>, MemoryError> {
    let mut hits = Vec::new();
    if !memory_dir.exists() || query.is_empty() {
        return Ok(hits);
    }

    let query_lower = query.to_lowercase();
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
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("today-") && name.ends_with(".md") {
                daily.push(path);
            }
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
            if line.to_lowercase().contains(&query_lower) {
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
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib memory::search`
Expected: PASS (5 tests)

- [ ] **Step 5: Commit**

```bash
git add src/memory/search.rs
git commit -m "feat: keyword search across buffer, daily, recent, and archive memory files"
```

---

### Task 7: `local-code memory` CLI (search / core / add)

**Files:**
- Create: `src/cli/memory.rs`
- Modify: `src/cli/mod.rs`

- [ ] **Step 1: Write `src/cli/memory.rs` with `search_command`, `core_command`, `add_command`**

```rust
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
```

- [ ] **Step 2: Run the tests to verify they fail, then pass**

Run: `cargo test --lib cli::memory`
Expected: first FAIL to compile if `src/cli/memory.rs` doesn't exist yet — create it with the
exact content above (implementation and tests are written together in this step, same pattern as
Phase 1's `connections.rs`). After creating it, re-run; expect PASS (4 tests).

- [ ] **Step 3: Wire `memory` into `src/cli/mod.rs`**

Modify `src/cli/mod.rs` (adding to the existing `Command`/`Cli`/`run` from the Phase 1 plan):

```rust
pub mod connections;
pub mod memory;

use crate::config::paths::Paths;
use clap::{Parser, Subcommand};
use std::io::{stdin, stdout};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "local-code")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage LLM connections (add/list/remove)
    Connections {
        #[command(subcommand)]
        action: ConnectionsAction,
    },
    /// Inspect cross-session memory (search/core/add)
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
}

#[derive(Subcommand)]
pub enum ConnectionsAction {
    Add,
    List,
    Remove { name: String },
}

#[derive(Subcommand)]
pub enum MemoryAction {
    /// Keyword-search the buffer, daily files, recent.md, and archive.md
    Search { query: String },
    /// Print the always-loaded core-memories.md file in full
    Core,
    /// Append a manual entry to the short-term buffer
    Add { text: String },
}

pub fn run(cli: Cli, project_root: PathBuf) -> anyhow::Result<()> {
    let paths = Paths::resolve(&project_root)?;
    match cli.command {
        Some(Command::Connections { action }) => match action {
            ConnectionsAction::Add => {
                connections::add(&paths, stdin().lock(), stdout())?;
            }
            ConnectionsAction::List => {
                connections::list(&paths, stdout())?;
            }
            ConnectionsAction::Remove { name } => {
                connections::remove(&paths, &name, stdout())?;
            }
        },
        Some(Command::Memory { action }) => match action {
            MemoryAction::Search { query } => {
                memory::search_command(&paths, &query, stdout())?;
            }
            MemoryAction::Core => {
                memory::core_command(&paths, stdout())?;
            }
            MemoryAction::Add { text } => {
                memory::add_command(&paths, &text, stdout())?;
            }
        },
        None => {
            println!("local-code: no command given. Try `local-code connections list`.");
        }
    }
    Ok(())
}
```

- [ ] **Step 4: Run the full build and test suite**

Run: `cargo build && cargo test`
Expected: build succeeds; all tests (Phase 1's plus this plan's) PASS.

- [ ] **Step 5: Manually verify the CLI end-to-end**

Run:
```bash
cargo run -- memory add "Manually recorded test memory about widgets."
cargo run -- memory search widgets
cargo run -- memory core
```
Expected: `add` prints `Recorded memory entry.`; `search` prints one line ending in
`Manually recorded test memory about widgets.` prefixed with the `now.md` path and line number;
`core` prints `No core memories recorded yet.` (nothing has been added to `core-memories.md` via
this manual flow, since there is no CLI command for it in this plan — `append_core_memory` is
exposed as a library function for the future agent loop / a curation flow to call).

- [ ] **Step 6: Commit**

```bash
git add src/cli/memory.rs src/cli/mod.rs
git commit -m "feat: add local-code memory search/core/add CLI"
```

---

## Self-review notes

- **Spec coverage:** every artifact named in spec section 9 — short-term buffer, dated daily
  files, rolled-up recent-window file, archive, and the `.remember/`-style `core-memories.md` — has
  a concrete file name, a concrete Markdown structure with a worked example, and a tested function
  that produces/consumes it. Recall is implemented as literal case-insensitive substring search
  (`search`), matching "grep/keyword search on request, not embedding similarity" precisely — no
  vector/embedding code, no `sqlite-vector-rs`, anywhere in this plan.
- **Placeholder scan:** the only `todo!()`s appear in each task's Step 1 (write-the-failing-test
  step) and every one is replaced with a real, complete implementation in that same task's Step 3.
  No "TODO", "TBD", "implement later", or "similar to Task N" shorthand appears anywhere — every
  code block in every step is complete, standalone Rust that compiles as shown.
- **Type/signature consistency:** `MemoryError`, `MemoryPaths`, `MemoryHit`, and every function
  signature (`append_buffer_entry`, `maybe_rollover`, `rollup_and_archive`, `read_core_memories`,
  `append_core_memory`, `search`) are defined exactly once (Tasks 1–6) and reused verbatim by the
  CLI (Task 7). The core-agent-loop phase must import these from `local_code::memory::{buffer,
  core, rollup, search}` (re-exported via `local_code::memory`) rather than redefining them, per
  the Spec Traceability section above.
- **Layering decision recorded:** the Architecture section states and justifies storing memory
  under `project_config_dir/memory` (git-diffable, project-committed) rather than `user_state_dir`
  (per-machine, not version-controlled) — this satisfies the spec's explicit "git-diffable"
  requirement, which `user_state_dir` cannot.
- **No agent-loop/tool wiring:** confirmed no `#[tool_fn]`, no `daimon` import, no `ntui` import,
  and no changes to any agent-loop-related file anywhere in this plan — `local_code::memory` is a
  standalone, pure-file-I/O module plus an optional manual-inspection CLI, exactly as scoped.
