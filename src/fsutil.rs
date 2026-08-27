//! Atomic file writes: sibling temp file + rename, the project's single
//! implementation of the convention documented in CLAUDE.md.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Marker appended to temp file names (kept public(crate) so session-store
/// sweeps/listings recognize strays).
pub(crate) const TMP_MARKER: &str = ".tmp-";

fn next_tmp_suffix() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// `<target>.tmp-<pid>-<n>` — pid disambiguates across processes, the
/// counter within one process (e.g. concurrent workspace panes saving).
fn tmp_path_for(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_os_string();
    name.push(format!(
        "{TMP_MARKER}{}-{}",
        std::process::id(),
        next_tmp_suffix()
    ));
    PathBuf::from(name)
}

/// Writes `target` atomically: exclusive-create a sibling temp file, let
/// `write` stream into it, flush + fsync, then rename over `target`.
/// `map_io` attaches the caller's error context (always the FINAL target
/// path) to any io failure; the temp file is removed best-effort on any
/// error. A rename within one filesystem is atomic on unix and Windows
/// alike (std uses MoveFileExW with MOVEFILE_REPLACE_EXISTING).
pub(crate) fn write_atomically<E>(
    target: &Path,
    write: impl FnOnce(&mut std::io::BufWriter<std::fs::File>) -> Result<(), E>,
    map_io: impl Fn(std::io::Error) -> E,
) -> Result<(), E> {
    // Exclusive-create the temp file (create_new): never follow a
    // pre-planted symlink nor clobber another writer's in-flight temp
    // file. A name collision (AlreadyExists) just retries with a fresh
    // suffix, up to three attempts in total.
    let opened = {
        let mut attempt = 0;
        loop {
            attempt += 1;
            let tmp = tmp_path_for(target);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)
            {
                Ok(file) => break Ok((tmp, file)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && attempt < 3 => {}
                // A third collision or any other io error is final:
                // `write` is never invoked without an open temp file.
                Err(e) => break Err(e),
            }
        }
    };
    let (tmp, file) = opened.map_err(&map_io)?;

    let mut writer = std::io::BufWriter::new(file);
    let result = write(&mut writer)
        .and_then(|()| std::io::Write::flush(&mut writer).map_err(&map_io))
        .and_then(|()| {
            // fsync before the rename so the file's data is durable
            // (power-loss safety), not just process-kill safe.
            writer.get_ref().sync_all().map_err(&map_io)
        });
    // Drop the writer before the rename so no platform renames a
    // still-open file.
    drop(writer);
    let result = result.and_then(|()| std::fs::rename(&tmp, target).map_err(&map_io));
    if result.is_err() {
        // Best-effort cleanup: don't accumulate temp files when the disk
        // is full, the write closure failed, or the rename failed.
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successive_tmp_paths_for_the_same_target_differ() {
        let target = Path::new("/some/dir/file.json");
        assert_ne!(tmp_path_for(target), tmp_path_for(target));
    }

    #[test]
    fn writes_a_new_file_and_overwrites_an_existing_one() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.txt");
        write_atomically(&target, |w| std::io::Write::write_all(w, b"first"), |e| e).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "first");
        write_atomically(&target, |w| std::io::Write::write_all(w, b"second"), |e| e).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "second");
        // No temp files left behind.
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("out.txt")]);
    }

    #[test]
    fn a_closure_error_leaves_no_tmp_file_and_the_target_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("out.txt");
        std::fs::write(&target, "original").unwrap();
        let result: Result<(), std::io::Error> =
            write_atomically(&target, |_| Err(std::io::Error::other("boom")), |e| e);
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("out.txt")]);
    }
}
