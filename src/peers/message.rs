//! Peer messages: the on-disk wire shape, handle/text validation, and atomic
//! delivery into another session's inbox.
//!
//! A "peer" is one running local-code session. Messages are plain JSON files
//! dropped into `<user_state_dir>/peers/<handle>/inbox/`; there is no server
//! or network transport (see the design spec). Everything in this module is
//! synchronous filesystem work and must be called via `spawn_blocking` from
//! async contexts.

use std::path::{Component, Path};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Bump only if `PeerMessage` changes incompatibly. Receivers treat a missing
/// `version` as `1` and skip (delete) an unrecognized one.
pub const MESSAGE_VERSION: u32 = 1;

/// Reply-chain budget. A fresh (initiator) message gets this; each reply
/// decrements. At zero, `send_message` refuses. Bounds ping-pong, not fresh
/// sends (the per-sender consent gate and inbox caps bound those).
pub const MAX_HOPS: u32 = 6;

/// Largest accepted message text, in bytes. Senders over this get an error
/// result; receivers stat-before-read and skip anything larger.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;

/// Longest accepted handle, in bytes.
pub const MAX_HANDLE_BYTES: usize = 64;

/// One message between sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerMessage {
    /// `#[serde(default)]` so a file without the key is read as v1.
    #[serde(default = "default_version")]
    pub version: u32,
    pub from: String,
    pub to: String,
    pub text: String,
    pub hops: u32,
    pub can_reply: bool,
    pub ts: String,
}

fn default_version() -> u32 {
    MESSAGE_VERSION
}

impl PeerMessage {
    pub fn new(
        from: String,
        to: String,
        text: String,
        hops: u32,
        can_reply: bool,
        ts: String,
    ) -> Self {
        PeerMessage {
            version: MESSAGE_VERSION,
            from,
            to,
            text,
            hops,
            can_reply,
            ts,
        }
    }
}

/// The conversation this session is currently answering: who to reply to and
/// the budget that message carried. Set by the App when a peer message is
/// injected; read by `send_message` to decide reply-vs-fresh and the hop count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyTarget {
    pub to: String,
    pub hops: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    #[error("invalid peer handle '{0}'")]
    InvalidHandle(String),
    #[error("peer '{0}' is not reachable: {1}")]
    Unreachable(String, #[source] std::io::Error),
    #[error("message text is {0} bytes, over the {1}-byte limit")]
    TextTooLarge(usize, usize),
    #[error("no target: pass `to`, or reply to an inbound peer message")]
    NoTarget,
    #[error("conversation budget exhausted; ask your user to continue")]
    Exhausted,
    #[error("peer '{0}' cannot receive replies")]
    NoReplyChannel(String),
    #[error("peer messaging io error: {0}")]
    Io(#[from] std::io::Error),
}

/// True iff `handle` is safe to join onto `peers_root`.
///
/// A charset check alone is not enough (it would admit `.` and `..`), so this
/// is *structural*: the path must parse to exactly one `Component::Normal`
/// equal to the input. Rejects empty, `.`/`..`, leading/trailing dots, path
/// separators, over-length input, and Windows reserved device names.
pub fn is_valid_handle(handle: &str) -> bool {
    if handle.is_empty() || handle.len() > MAX_HANDLE_BYTES {
        return false;
    }
    if !handle
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return false;
    }
    if handle.starts_with('.') || handle.ends_with('.') {
        return false;
    }
    // Exactly one Normal component, and it must be the whole string (this is
    // what defeats `.`/`..`/separators that a charset check would let past).
    let mut components = Path::new(handle).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(seg)), None) => seg.to_str() == Some(handle),
        _ => false,
    }
}

/// True iff `handle`'s stem (before any `.`) is a Windows reserved device name.
/// Checked on every platform so a registry created on Windows is never
/// addressed by a name Windows cannot create.
fn is_reserved_device_name(handle: &str) -> bool {
    let stem = handle.split('.').next().unwrap_or(handle);
    let upper = stem.to_ascii_uppercase();
    matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.as_bytes()[3].is_ascii_digit()
            && upper.as_bytes()[3] != b'0')
}

/// Full handle validity including the Windows-reserved check that
/// [`is_valid_handle`] deliberately keeps separate so the two rules can be
/// tested independently.
pub fn is_acceptable_handle(handle: &str) -> bool {
    is_valid_handle(handle) && !is_reserved_device_name(handle)
}

/// Serializes `msg` and atomically writes it into `peers_root/<target>/inbox/`.
///
/// Rejects an invalid `target`, a target whose directory is missing or is a
/// symlink, and an inbox that canonicalizes outside `peers_root` (symlink
/// escape). Filename uniqueness across concurrent writers in one process comes
/// from the process-global sequence counter in [`message_filename`], so panes
/// sharing a process cannot collide.
pub fn deliver(peers_root: &Path, target: &str, msg: &PeerMessage) -> Result<(), PeerError> {
    if !is_acceptable_handle(target) {
        return Err(PeerError::InvalidHandle(target.to_string()));
    }
    if msg.text.len() > MAX_MESSAGE_BYTES {
        return Err(PeerError::TextTooLarge(msg.text.len(), MAX_MESSAGE_BYTES));
    }

    // `peers_root` itself is trusted (we created it); canonicalize so the
    // containment check below is against a stable absolute root.
    let root = std::fs::canonicalize(peers_root)
        .map_err(|e| PeerError::Unreachable(target.to_string(), e))?;
    let dir = root.join(target);
    let dir_meta = std::fs::symlink_metadata(&dir)
        .map_err(|e| PeerError::Unreachable(target.to_string(), e))?;
    if dir_meta.file_type().is_symlink() || !dir_meta.is_dir() {
        return Err(PeerError::Unreachable(
            target.to_string(),
            std::io::Error::other("target is not a real directory"),
        ));
    }
    let inbox = dir.join("inbox");
    let inbox_canon =
        std::fs::canonicalize(&inbox).map_err(|e| PeerError::Unreachable(target.to_string(), e))?;
    if !inbox_canon.starts_with(&root) {
        return Err(PeerError::Unreachable(
            target.to_string(),
            std::io::Error::other("inbox canonicalizes outside the peer registry"),
        ));
    }

    let path = inbox_canon.join(message_filename());
    crate::fsutil::write_atomically(
        &path,
        |writer| {
            serde_json::to_writer(writer, msg).map_err(|e| PeerError::Io(std::io::Error::other(e)))
        },
        PeerError::Io,
    )
}

/// Process-global message counter. Kept here (not on `PeerRuntime`) so every
/// writer in one process shares a single namespace; `fetch_add` is monotone, so
/// same-millisecond writes still order chronologically.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// `<unix_millis>-<pid>-<seq>.json` — lexicographically chronological and
/// collision-free between concurrent writers (pid + process-global seq). Each
/// numeric field is zero-padded so `seq` 10 sorts after `seq` 9.
pub fn message_filename() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    filename_for(millis, std::process::id(), seq)
}

/// Fixed-width name for explicit components; split out so tests can assert on
/// ordering/padding deterministically without racing the process-global [`SEQ`].
fn filename_for(millis: u128, pid: u32, seq: u64) -> String {
    format!("{millis:013}-{pid:010}-{seq:08}.json")
}

/// True iff `name` looks like a message file this module wrote (and not a
/// `TMP_MARKER` atomic-write stray, which shares the `.json` extension).
pub fn is_message_filename(name: &str) -> bool {
    name.ends_with(".json") && !name.contains(crate::fsutil::TMP_MARKER)
}

/// Strips terminal-dangerous control characters from peer-supplied text before
/// it is painted. Keeps `\n`/`\t` (readable multi-line messages) and drops all
/// other C0/C1 controls — notably ESC, so a peer cannot inject ANSI/OSC
/// sequences (UI spoofing, clipboard writes) into the user's terminal. Also
/// drops Unicode bidi controls and zero-width characters, which can reorder or
/// hide the painted text without being C0/C1 controls.
pub fn sanitize_peer_text(input: &str) -> String {
    input
        .chars()
        .filter(|c| {
            if *c == '\n' || *c == '\t' {
                return true;
            }
            if c.is_control() {
                return false;
            }
            !matches!(
                *c,
                '\u{061C}'                  // arabic letter mark
                    | '\u{200B}'..='\u{200F}' // zero-width space/joiners, LRM/RLM
                    | '\u{202A}'..='\u{202E}' // bidi embeddings/overrides
                    | '\u{2060}'..='\u{2069}' // word joiner, bidi isolates
                    | '\u{FEFF}'              // zero-width no-break space (BOM)
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use tempfile::tempdir;

    fn sample(from: &str, to: &str) -> PeerMessage {
        PeerMessage::new(
            from.into(),
            to.into(),
            "hello".into(),
            MAX_HOPS,
            true,
            "2026-09-13T00:00:00Z".into(),
        )
    }

    fn make_target(root: &Path, handle: &str) {
        std::fs::create_dir_all(root.join(handle).join("inbox")).unwrap();
    }

    #[test]
    fn accepts_normal_handles() {
        for h in ["local-code-3f9a2b7c", "a", "a_b.c-d", "x9"] {
            assert!(is_acceptable_handle(h), "{h} should be valid");
        }
    }

    #[test]
    fn rejects_traversal_and_malformed_handles() {
        for h in [
            "", ".", "..", ".hidden", "trail.", "a/b", "a\\b", "/abs", "a b", "a:b", "C O N",
        ] {
            assert!(!is_valid_handle(h), "{h:?} must be rejected");
        }
        let long = "a".repeat(MAX_HANDLE_BYTES + 1);
        assert!(!is_valid_handle(&long));
    }

    #[test]
    fn rejects_windows_reserved_device_names() {
        for h in [
            "CON", "con", "PRN", "AUX", "NUL", "COM1", "LPT9", "con.json",
        ] {
            assert!(!is_acceptable_handle(h), "{h:?} is a reserved name");
        }
        assert!(is_acceptable_handle("COM0")); // 0 is not a reserved port
        assert!(is_acceptable_handle("console-1a2b"));
    }

    #[test]
    fn peer_message_round_trips_and_missing_version_reads_as_v1() {
        let msg = sample("a", "b");
        let json = serde_json::to_string(&msg).unwrap();
        let back: PeerMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);

        let no_version = serde_json::json!({
            "from": "a", "to": "b", "text": "x", "hops": 2,
            "can_reply": true, "ts": "2026-09-13T00:00:00Z"
        });
        let back: PeerMessage = serde_json::from_value(no_version).unwrap();
        assert_eq!(back.version, MESSAGE_VERSION);
    }

    #[test]
    fn message_filename_is_chronological_and_unique() {
        // Two calls in the same process (same millisecond or not) must never
        // yield the same name — the global seq disambiguates.
        let a = message_filename();
        let b = message_filename();
        assert_ne!(a, b, "process-global seq must disambiguate writers");
        assert!(a < b, "{a} should sort before {b}");
        assert!(is_message_filename(&a));
        assert!(!is_message_filename(&format!(
            "{a}{}123-0",
            crate::fsutil::TMP_MARKER
        )));

        // Fixed-width fields make lexicographic order == chronological order.
        let t = 1_700_000_000_000_u128;
        assert_eq!(
            filename_for(t, 42, 1),
            "1700000000000-0000000042-00000001.json"
        );
        // Same millisecond, same pid, different seq => distinct and ordered.
        assert_ne!(filename_for(t, 42, 1), filename_for(t, 42, 2));
        assert!(filename_for(t, 42, 9) < filename_for(t, 42, 10));
        assert!(filename_for(t, 42, 10) < filename_for(t + 1, 42, 10));
    }

    #[test]
    fn message_filename_from_two_panes_sharing_a_process_do_not_collide() {
        let names: HashSet<String> = (0..1000).map(|_| message_filename()).collect();
        assert_eq!(names.len(), 1000, "same-process writers collided");
    }

    #[test]
    fn sanitize_strips_escapes_but_keeps_newlines_and_tabs() {
        assert_eq!(
            sanitize_peer_text("a\x1b[2Jb\x07\nc\td"),
            "a[2Jb\nc\td",
            "ESC and BEL are dropped; newline/tab survive"
        );
    }

    #[test]
    fn sanitize_strips_bidi_and_zero_width_characters() {
        let input = "safe\u{202E}gnp.exe\u{202C}\u{200B}x\u{FEFF}\u{2066}y";
        assert_eq!(sanitize_peer_text(input), "safegnp.exexy");
    }

    #[test]
    fn deliver_writes_a_valid_message_into_the_inbox() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("peers");
        make_target(&root, "bob-1234abcd");
        deliver(&root, "bob-1234abcd", &sample("alice-1", "bob-1234abcd")).unwrap();

        let inbox = root.join("bob-1234abcd").join("inbox");
        let entries: Vec<_> = std::fs::read_dir(&inbox)
            .unwrap()
            .map(|e| e.unwrap())
            .collect();
        assert_eq!(entries.len(), 1);
        let text = std::fs::read_to_string(entries[0].path()).unwrap();
        let msg: PeerMessage = serde_json::from_str(&text).unwrap();
        assert_eq!(msg.from, "alice-1");
        assert_eq!(msg.text, "hello");
    }

    #[test]
    fn deliver_rejects_invalid_target_unknown_target_and_oversize_text() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("peers");
        std::fs::create_dir_all(&root).unwrap();

        assert!(matches!(
            deliver(&root, "../escape", &sample("a", "b")),
            Err(PeerError::InvalidHandle(_))
        ));
        assert!(matches!(
            deliver(&root, "nobody-0000", &sample("a", "b")),
            Err(PeerError::Unreachable(_, _))
        ));

        make_target(&root, "bob-1234abcd");
        let mut big = sample("a", "bob-1234abcd");
        big.text = "x".repeat(MAX_MESSAGE_BYTES + 1);
        assert!(matches!(
            deliver(&root, "bob-1234abcd", &big),
            Err(PeerError::TextTooLarge(_, _))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn deliver_refuses_a_symlinked_target_dir() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("peers");
        std::fs::create_dir_all(&root).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(outside.join("inbox")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("evil-0000")).unwrap();

        assert!(matches!(
            deliver(&root, "evil-0000", &sample("a", "evil-0000")),
            Err(PeerError::Unreachable(_, _))
        ));
        // Nothing landed outside.
        assert_eq!(std::fs::read_dir(outside.join("inbox")).unwrap().count(), 0);
    }

    /// The containment check, not just the target-dir symlink check: a real
    /// peer dir whose `inbox/` is a symlink pointing outside the registry must
    /// be refused, and nothing may be written through it.
    #[cfg(unix)]
    #[test]
    fn deliver_refuses_an_inbox_symlinked_outside_the_root() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("peers");
        let peer = root.join("evil-0000");
        std::fs::create_dir_all(&peer).unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, peer.join("inbox")).unwrap();

        assert!(matches!(
            deliver(&root, "evil-0000", &sample("a", "evil-0000")),
            Err(PeerError::Unreachable(_, _))
        ));
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }
}
