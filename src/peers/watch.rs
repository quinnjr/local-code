//! Inbox draining for the TUI watcher: picks valid message files out of an
//! inbox, marks each processed so a failed delete can never re-inject it every
//! tick, and enforces the count/age/size caps. Synchronous; wrap in
//! `spawn_blocking`.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::Read as _;
use std::path::Path;
use std::time::{Duration, SystemTime};

use super::message::{
    MAX_MESSAGE_BYTES, MESSAGE_VERSION, PeerMessage, is_acceptable_handle, is_message_filename,
};

/// At most this many messages are handled per tick.
pub const MAX_PER_TICK: usize = 8;
/// Above this many pending files, processing pauses and overflow is reported.
pub const MAX_INBOX: usize = 64;
/// Files older than this are discarded unread.
pub const MAX_MESSAGE_AGE: Duration = Duration::from_secs(5 * 60);
/// Newest entries retained in `inbox/processed/` before older ones are pruned.
pub const MAX_PROCESSED_ARCHIVE: usize = 64;

/// A valid `text` is capped at [`MAX_MESSAGE_BYTES`], but its JSON encoding is
/// larger (escaping plus the other fields). The stat guard allows this much
/// before reading, and `read_message` re-checks the decoded `text` length, so a
/// legitimate near-cap message is not dropped for being over the *text* cap.
const MAX_ENCODED_BYTES: u64 = MAX_MESSAGE_BYTES as u64 * 3 + 512;

#[derive(Debug, Default)]
pub struct DrainedInbox {
    pub messages: Vec<PeerMessage>,
    pub overflow: bool,
    /// Names skipped this tick (invalid, stale, oversize, unreadable).
    pub skipped: usize,
}

/// Reads up to [`MAX_PER_TICK`] valid messages from `inbox`, newest last.
///
/// Every candidate is added to `processed` (persisted across ticks by the
/// caller) so a file that cannot be renamed/removed is never handled twice.
/// Non-regular files (symlinks in particular) are never followed or read.
/// Returns the `read_dir` error so a caller can surface an unreadable inbox
/// rather than silently treating it as empty.
pub fn drain_inbox(
    inbox: &Path,
    processed: &mut HashSet<OsString>,
) -> std::io::Result<DrainedInbox> {
    let mut out = DrainedInbox::default();
    let entries = std::fs::read_dir(inbox)?;

    let mut candidates: Vec<OsString> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            let as_str = name.to_str()?;
            if !is_message_filename(as_str) || processed.contains(&name) {
                return None;
            }
            Some(name)
        })
        .collect();
    if candidates.len() > MAX_INBOX {
        out.overflow = true;
    }
    candidates.sort();

    let processed_dir = inbox.join("processed");
    let mut processed_dir_ready = false;

    for name in candidates.into_iter().take(MAX_PER_TICK) {
        let path = inbox.join(&name);
        // `symlink_metadata`: never follow a link planted in the inbox.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            tracing::warn!(file = %name.to_string_lossy(), "skipping peer message: cannot stat");
            processed.insert(name);
            out.skipped += 1;
            continue;
        };
        if !meta.is_file() {
            tracing::warn!(file = %name.to_string_lossy(), "skipping peer message: not a regular file");
            processed.insert(name);
            out.skipped += 1;
            continue;
        }
        let too_big = meta.len() > MAX_ENCODED_BYTES;
        let too_old = meta
            .modified()
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|age| age > MAX_MESSAGE_AGE);

        let message = if too_big || too_old {
            None
        } else {
            read_message(&path)
        };

        // Mark processed BEFORE handling: rename into `inbox/processed/`, else
        // delete; the in-memory set covers a failure of both.
        if !processed_dir_ready {
            processed_dir_ready = std::fs::create_dir_all(&processed_dir).is_ok();
        }
        let moved =
            processed_dir_ready && std::fs::rename(&path, processed_dir.join(&name)).is_ok();
        if !moved {
            let _ = std::fs::remove_file(&path);
        }
        processed.insert(name);

        match message {
            Some(msg) if msg.version == MESSAGE_VERSION => out.messages.push(msg),
            _ => {
                let reason = if too_big {
                    "oversize file"
                } else if too_old {
                    "stale"
                } else {
                    "malformed, invalid, or unknown version"
                };
                tracing::debug!(
                    file = %path.file_name().unwrap_or_default().to_string_lossy(),
                    reason,
                    "skipping peer message"
                );
                out.skipped += 1;
            }
        }
    }
    prune_processed(&processed_dir);
    Ok(out)
}

/// Keeps `processed/` bounded: after each drain, retain only the
/// lexicographically newest [`MAX_PROCESSED_ARCHIVE`] entries (the names are
/// millisecond-prefixed, so lexical order is chronological) and delete the
/// rest. Best-effort — a missing directory or unremovable entry is ignored.
fn prune_processed(processed_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(processed_dir) else {
        return;
    };
    let mut names: Vec<OsString> = entries.flatten().map(|e| e.file_name()).collect();
    if names.len() <= MAX_PROCESSED_ARCHIVE {
        return;
    }
    names.sort();
    let remove = names.len() - MAX_PROCESSED_ARCHIVE;
    for name in names.into_iter().take(remove) {
        let path = processed_dir.join(&name);
        if std::fs::symlink_metadata(&path)
            .map(|m| m.is_file())
            .unwrap_or(false)
        {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Reads one message file through a `MAX_ENCODED_BYTES + 1` bound, so a file
/// that grew between the stat and the read still cannot exceed the encoding
/// allowance. The decoded `text` is then re-checked against the real cap, and a
/// `from` that is not an acceptable handle is refused, before either reaches
/// the app.
fn read_message(path: &Path) -> Option<PeerMessage> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    file.take(MAX_ENCODED_BYTES + 1)
        .read_to_end(&mut buf)
        .ok()?;
    if buf.len() as u64 > MAX_ENCODED_BYTES {
        return None;
    }
    let msg: PeerMessage = serde_json::from_slice(&buf).ok()?;
    if msg.text.len() > MAX_MESSAGE_BYTES || !is_acceptable_handle(&msg.from) {
        return None;
    }
    Some(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peers::message::{MAX_HOPS, message_filename};
    use std::path::PathBuf;

    fn msg(from: &str) -> PeerMessage {
        PeerMessage::new(
            from.into(),
            "me".into(),
            "hello".into(),
            MAX_HOPS,
            true,
            "2026-09-13T00:00:00Z".into(),
        )
    }

    fn write_msg(inbox: &Path, message: &PeerMessage) -> PathBuf {
        let path = inbox.join(message_filename());
        std::fs::write(&path, serde_json::to_vec(message).unwrap()).unwrap();
        path
    }

    #[test]
    fn drains_valid_messages_and_moves_them_out_of_the_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        write_msg(&inbox, &msg("a"));
        write_msg(&inbox, &msg("b"));

        let mut processed = HashSet::new();
        let out = drain_inbox(&inbox, &mut processed).unwrap();
        assert_eq!(out.messages.len(), 2);
        assert_eq!(out.messages[0].from, "a");
        assert!(!out.overflow);
        // Nothing left to re-read next tick.
        let out2 = drain_inbox(&inbox, &mut processed).unwrap();
        assert!(out2.messages.is_empty());
    }

    #[test]
    fn skips_symlinks_malformed_unknown_version_and_tmp_files() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();

        // Malformed JSON.
        std::fs::write(inbox.join(message_filename()), b"{not json").unwrap();
        // Unknown version.
        let mut bad = msg("bad");
        bad.version = 999;
        write_msg(&inbox, &bad);
        // Atomic-write stray: shares .json but carries TMP_MARKER.
        std::fs::write(
            inbox.join(format!(
                "{}{}1-0",
                message_filename(),
                crate::fsutil::TMP_MARKER
            )),
            b"{}",
        )
        .unwrap();

        #[cfg(unix)]
        {
            let secret = dir.path().join("secret.json");
            std::fs::write(&secret, serde_json::to_vec(&msg("evil")).unwrap()).unwrap();
            std::os::unix::fs::symlink(&secret, inbox.join(message_filename())).unwrap();
        }

        let mut processed = HashSet::new();
        let out = drain_inbox(&inbox, &mut processed).unwrap();
        assert!(out.messages.is_empty(), "{:?}", out.messages);
        assert!(out.skipped >= 2);
        let _ = processed;
    }

    #[test]
    fn overflow_is_reported_past_the_cap_and_only_a_batch_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        for _ in 0..(MAX_INBOX + 2) {
            write_msg(&inbox, &msg("a"));
        }
        let mut processed = HashSet::new();
        let out = drain_inbox(&inbox, &mut processed).unwrap();
        assert!(out.overflow);
        assert_eq!(out.messages.len(), MAX_PER_TICK);
    }

    #[test]
    fn previously_processed_names_are_never_reinjected() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        write_msg(&inbox, &msg("a"));

        // Simulate a prior tick whose rename/delete failed: the name is in the
        // set even though the file is still on disk.
        let mut processed = HashSet::new();
        let name = std::fs::read_dir(&inbox)
            .unwrap()
            .flatten()
            .next()
            .unwrap()
            .file_name();
        processed.insert(name);
        let out = drain_inbox(&inbox, &mut processed).unwrap();
        assert!(out.messages.is_empty());
    }

    #[test]
    fn near_cap_text_whose_encoding_exceeds_the_text_cap_is_delivered() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();

        // Text just under the *text* cap, but the JSON encoding of the whole
        // message is comfortably over it. This must still be delivered.
        let mut m = msg("a");
        m.text = "x".repeat(MAX_MESSAGE_BYTES - 32);
        let path = write_msg(&inbox, &m);
        let encoded = std::fs::metadata(&path).unwrap().len();
        assert!(
            encoded > MAX_MESSAGE_BYTES as u64,
            "test setup: encoding {encoded} not over the text cap"
        );

        let mut processed = HashSet::new();
        let out = drain_inbox(&inbox, &mut processed).unwrap();
        assert_eq!(out.skipped, 0, "near-cap message was wrongly skipped");
        assert_eq!(out.messages.len(), 1);
        assert_eq!(out.messages[0].text.len(), MAX_MESSAGE_BYTES - 32);
    }

    #[test]
    fn stale_message_is_skipped_and_moved_to_processed() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let path = write_msg(&inbox, &msg("a"));
        let name = path.file_name().unwrap().to_os_string();

        let old = SystemTime::now() - (MAX_MESSAGE_AGE + Duration::from_secs(60));
        let times = std::fs::FileTimes::new().set_modified(old);
        std::fs::File::open(&path)
            .unwrap()
            .set_times(times)
            .unwrap();

        let mut processed = HashSet::new();
        let out = drain_inbox(&inbox, &mut processed).unwrap();
        assert!(out.messages.is_empty());
        assert_eq!(out.skipped, 1);
        assert!(!path.exists(), "stale file should leave the inbox");
        assert!(inbox.join("processed").join(&name).exists());
    }

    #[test]
    fn oversize_file_is_skipped_and_moved_to_processed() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        let name = message_filename();
        std::fs::write(
            inbox.join(&name),
            vec![b'a'; MAX_ENCODED_BYTES as usize + 1],
        )
        .unwrap();

        let mut processed = HashSet::new();
        let out = drain_inbox(&inbox, &mut processed).unwrap();
        assert!(out.messages.is_empty());
        assert_eq!(out.skipped, 1);
        assert!(inbox.join("processed").join(&name).exists());
    }

    #[test]
    fn message_from_an_unacceptable_handle_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();
        write_msg(&inbox, &msg("../evil"));

        let mut processed = HashSet::new();
        let out = drain_inbox(&inbox, &mut processed).unwrap();
        assert!(out.messages.is_empty());
        assert_eq!(out.skipped, 1);
    }

    #[test]
    fn decoded_text_over_the_cap_is_skipped_and_archived() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        std::fs::create_dir_all(&inbox).unwrap();

        // Valid JSON, under the *encoded* read guard, but the decoded `text`
        // exceeds MAX_MESSAGE_BYTES. `read_message` must reject it.
        let mut m = msg("a");
        m.text = "x".repeat(MAX_MESSAGE_BYTES + 1);
        let path = write_msg(&inbox, &m);
        let encoded = std::fs::metadata(&path).unwrap().len();
        assert!(
            encoded <= MAX_ENCODED_BYTES,
            "test setup: encoding {encoded} exceeds the read guard"
        );
        let name = path.file_name().unwrap().to_os_string();

        let mut processed = HashSet::new();
        let out = drain_inbox(&inbox, &mut processed).unwrap();
        assert!(out.messages.is_empty());
        assert_eq!(out.skipped, 1);
        assert!(inbox.join("processed").join(&name).exists());
    }

    #[test]
    fn processed_archive_is_pruned_to_the_newest_entries() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let processed_dir = inbox.join("processed");
        std::fs::create_dir_all(&processed_dir).unwrap();
        let total = MAX_PROCESSED_ARCHIVE + 5;
        for i in 0..total {
            std::fs::write(processed_dir.join(format!("{i:08}.json")), b"{}").unwrap();
        }

        let mut processed = HashSet::new();
        drain_inbox(&inbox, &mut processed).unwrap();

        let remaining = std::fs::read_dir(&processed_dir).unwrap().count();
        assert_eq!(remaining, MAX_PROCESSED_ARCHIVE);
        let newest = processed_dir.join(format!("{:08}.json", total - 1));
        assert!(newest.exists());
        assert!(!processed_dir.join("00000000.json").exists());
    }

    #[test]
    fn drain_inbox_propagates_a_missing_inbox() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("no-such-inbox");
        let mut processed = HashSet::new();
        assert!(drain_inbox(&missing, &mut processed).is_err());
    }
}
