//! The on-disk peer registry: handle derivation, `meta.json`, heartbeats and
//! liveness, discovery, and reaping.
//!
//! Layout (under `Paths::user_state_dir`):
//!
//! ```text
//! peers/<handle>/meta.json      # PeerMeta
//! peers/<handle>/heartbeat      # mtime is the liveness signal
//! peers/<handle>/inbox/         # PeerMessage files
//! ```
//!
//! All functions here are synchronous filesystem work; callers in async
//! contexts must wrap them in `spawn_blocking`.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use super::message::MAX_HANDLE_BYTES;

/// Bump only for an incompatible `PeerMeta` change.
pub const META_VERSION: u32 = 1;

/// A peer whose heartbeat is older than this is a reaping candidate (subject
/// to the pid-liveness check).
pub const HEARTBEAT_STALE: Duration = Duration::from_secs(30);

/// Cap on handle-collision suffixes tried by [`create_peer_dir`].
const MAX_SUFFIX: u32 = 100;

/// Largest `meta.json` accepted by [`read_meta`], in bytes. Bounds the read a
/// hostile or corrupt peer directory can force.
const MAX_META_BYTES: u64 = 64 * 1024;

/// `<user_state_dir>/peers` — the machine-wide peer registry root.
pub fn peers_root(user_state_dir: &Path) -> PathBuf {
    user_state_dir.join("peers")
}

/// Human-readable prefix of a handle: the project directory's basename,
/// lowercased, non-alphanumerics collapsed to `-`, trimmed, capped at 24.
pub fn project_prefix(project_root: &Path) -> String {
    let base = project_root
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("project");
    let mut slug: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    while slug.contains("--") {
        slug = slug.replace("--", "-");
    }
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "project" } else { slug };
    slug.chars().take(24).collect()
}

/// Low 32 bits of a `DefaultHasher` over the canonical session path, as 8 hex
/// digits. Same technique as `session::paths::project_slug`, different input.
pub fn session_short_hash(session_path: &Path) -> String {
    format!("{:08x}", canonical_path_hash(session_path) as u32)
}

/// Canonicalizes `path` (falling back to the literal path when it does not
/// exist) and returns a `DefaultHasher` over the result. Same technique as
/// `session::paths::project_slug`, which appends its own readable tail; like
/// that function, the hash is not stable across Rust releases.
pub(crate) fn canonical_path_hash(path: &Path) -> u64 {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    canonical.hash(&mut hasher);
    hasher.finish()
}

/// `<project-prefix>-<8hex>` — deterministic for one session's lifetime.
pub fn handle_for(project_root: &Path, session_path: &Path) -> String {
    format!(
        "{}-{}",
        project_prefix(project_root),
        session_short_hash(session_path)
    )
}

/// The handle for a send-only (headless `-p`) runtime: `"{prefix}-{pid:08x}"`.
///
/// Deliberately pid-based and *not* registered in the peer directory: a
/// send-only runtime cannot receive, so it needs an identity only to stamp
/// outgoing messages, not a discoverable mailbox. Distinct from the
/// session-scoped [`handle_for`] shape, which is what lets a recipient keep the
/// two apart.
pub fn send_only_handle(project_root: &Path, pid: u32) -> String {
    format!("{}-{pid:08x}", project_prefix(project_root))
}

/// Everything `list_peers` reports about a running session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeerMeta {
    #[serde(default = "default_meta_version")]
    pub version: u32,
    pub handle: String,
    pub pid: u32,
    pub session_path: PathBuf,
    pub project_root: PathBuf,
    pub connection: String,
    pub model: String,
    pub preview: String,
    pub streaming: bool,
    pub can_reply: bool,
    pub updated_at: String,
}

fn default_meta_version() -> u32 {
    META_VERSION
}

/// Owned inputs for [`PeerMeta::new`], grouped so callers cannot transpose the
/// adjacent same-typed `handle`/`connection`/`model`/`preview`/`project_root`
/// strings.
pub struct PeerMetaInit {
    pub handle: String,
    pub session_path: PathBuf,
    pub project_root: PathBuf,
    pub connection: String,
    pub model: String,
    pub preview: String,
    pub streaming: bool,
    pub can_reply: bool,
}

impl PeerMeta {
    pub fn new(init: PeerMetaInit) -> Self {
        PeerMeta {
            version: META_VERSION,
            handle: init.handle,
            pid: std::process::id(),
            session_path: init.session_path,
            project_root: init.project_root,
            connection: init.connection,
            model: init.model,
            preview: init.preview,
            streaming: init.streaming,
            can_reply: init.can_reply,
            updated_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

fn create_dir_0700(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

#[cfg(unix)]
fn tighten_dir_mode(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn tighten_dir_mode(_path: &Path) {}

/// Creates the registry root (and tightens its mode) if absent.
pub fn ensure_peers_root(root: &Path) -> std::io::Result<()> {
    match std::fs::create_dir_all(root) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    tighten_dir_mode(root);
    Ok(())
}

/// Creates `peers_root/<handle>` (plus `inbox/` and a primed heartbeat).
///
/// The directory is fully built under a `TMP_MARKER` name and only then
/// renamed into place, so a concurrent [`reap_stale`]/[`list_live_peers`]
/// always sees either nothing or a complete peer dir — never a half-built one.
/// The build dir's name carries `TMP_MARKER`, so `has_inflight_temp` makes
/// reapers skip it even before the heartbeat exists. On a final-name collision,
/// retries with `-2`, `-3`, …; returns the handle actually created and its dir.
pub fn create_peer_dir(root: &Path, base_handle: &str) -> std::io::Result<(String, PathBuf)> {
    ensure_peers_root(root)?;
    for suffix in 0..MAX_SUFFIX {
        let handle = if suffix == 0 {
            base_handle.to_string()
        } else {
            format!("{base_handle}-{suffix}")
        };
        if handle.len() > MAX_HANDLE_BYTES {
            continue;
        }
        let final_dir = root.join(&handle);
        let build_dir = root.join(format!("{handle}.tmp-{}", std::process::id()));
        match create_dir_0700(&build_dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // A stale build dir from a crashed attempt; clear and reuse.
                let _ = std::fs::remove_dir_all(&build_dir);
                create_dir_0700(&build_dir)?;
            }
            Err(e) => return Err(e),
        }
        if let Err(e) = create_dir_0700(&build_dir.join("inbox")) {
            let _ = std::fs::remove_dir_all(&build_dir);
            return Err(e);
        }
        if let Err(e) = touch_heartbeat(&build_dir) {
            let _ = std::fs::remove_dir_all(&build_dir);
            return Err(e);
        }
        match std::fs::rename(&build_dir, &final_dir) {
            Ok(()) => return Ok((handle, final_dir)),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&build_dir);
                // The rename failed because the final name is taken (rename
                // onto an existing non-empty dir reports ENOTEMPTY, not EEXIST
                // on unix, so test for existence rather than an errno kind).
                if std::fs::symlink_metadata(&final_dir).is_ok() {
                    continue;
                }
                return Err(e);
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "no free peer handle suffix",
    ))
}

/// Removes a peer directory tree (best-effort; ignores absence).
pub fn remove_peer_dir(dir: &Path) {
    if let Err(e) = std::fs::remove_dir_all(dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::debug!("failed to remove peer dir {}: {e}", dir.display());
    }
}

/// Writes `meta` to `<dir>/meta.json` atomically.
pub fn write_meta(dir: &Path, meta: &PeerMeta) -> std::io::Result<()> {
    let path = dir.join("meta.json");
    crate::fsutil::write_atomically(
        &path,
        |writer| serde_json::to_writer(writer, meta).map_err(std::io::Error::other),
        |e| e,
    )
}

/// Reads `<dir>/meta.json`; `None` on any error, a non-regular/oversized file,
/// or an unrecognized version.
pub fn read_meta(dir: &Path) -> Option<PeerMeta> {
    let path = dir.join("meta.json");
    // `symlink_metadata` (not `metadata`) so a symlinked meta is rejected, and
    // `is_file` rejects FIFOs/devices/dirs that would block or misbehave.
    let file_meta = std::fs::symlink_metadata(&path).ok()?;
    if !file_meta.file_type().is_file() || file_meta.len() > MAX_META_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let meta: PeerMeta = serde_json::from_str(&text).ok()?;
    if meta.version != META_VERSION {
        return None;
    }
    Some(meta)
}

/// Rewrites `meta.json` through `edit` (used to refresh `streaming`/`preview`).
pub fn update_meta(dir: &Path, edit: impl FnOnce(&mut PeerMeta)) -> std::io::Result<()> {
    let mut meta = read_meta(dir).ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "peer meta.json missing")
    })?;
    edit(&mut meta);
    meta.updated_at = chrono::Utc::now().to_rfc3339();
    write_meta(dir, &meta)
}

/// Creates/truncates `<dir>/heartbeat`, updating its mtime.
pub fn touch_heartbeat(dir: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(dir.join("heartbeat"))
        .map(|_| ())
}

fn heartbeat_mtime(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir.join("heartbeat"))
        .and_then(|m| m.modified())
        .ok()
}

/// True iff the heartbeat is fresh, or has a future mtime (clock skew ⇒
/// assume alive rather than reap a possibly-live session).
pub fn heartbeat_fresh(dir: &Path) -> bool {
    match heartbeat_mtime(dir) {
        None => false,
        Some(t) => match SystemTime::now().duration_since(t) {
            Ok(age) => age < HEARTBEAT_STALE,
            Err(_) => true,
        },
    }
}

/// Warns once per process when the liveness probe cannot be run (no absolute
/// `kill` binary present, or spawning it failed).
#[cfg(unix)]
fn warn_pid_probe_failed(reason: &str) {
    use std::sync::Once;
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        tracing::warn!("cannot probe peer liveness ({reason}); assuming alive");
    });
}

/// True iff `pid` names a live process. On Linux this is the existence of
/// `/proc/<pid>`; on other unixes it is `kill -0` via an absolute binary path
/// so a hostile `PATH` cannot shadow it. When no probe is available it warns
/// once and conservatively returns `true`.
#[cfg(unix)]
pub fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        let proc_root = Path::new("/proc");
        if proc_root.exists() {
            return proc_root.join(pid.to_string()).exists();
        }
    }
    let Some(program) = ["/bin/kill", "/usr/bin/kill"]
        .into_iter()
        .find(|p| Path::new(p).exists())
    else {
        warn_pid_probe_failed("no absolute kill binary found");
        return true;
    };
    match std::process::Command::new(program)
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(e) => {
            warn_pid_probe_failed(&e.to_string());
            true
        }
    }
}

#[cfg(not(unix))]
pub fn pid_is_alive(_pid: u32) -> bool {
    true
}

/// True iff a portable pid-liveness probe exists on this platform. When it does
/// not, [`peer_is_alive`] falls back to heartbeat freshness alone.
#[cfg(unix)]
fn pid_check_available() -> bool {
    true
}

#[cfg(not(unix))]
fn pid_check_available() -> bool {
    false
}

/// True iff the peer in `dir` should be considered running.
///
/// A fresh heartbeat always means alive. Beyond that, a heartbeat-stale dir is
/// kept only when a pid-liveness probe exists and reports the pid alive (e.g.
/// SIGSTOPped/suspended). On non-unix there is no portable probe, so a stale
/// heartbeat means dead — the accepted trade-off is that a suspended peer may
/// be reaped early on macOS/Windows.
pub fn peer_is_alive(dir: &Path) -> bool {
    if heartbeat_fresh(dir) {
        return true;
    }
    if !pid_check_available() {
        return false;
    }
    read_meta(dir).map(|m| pid_is_alive(m.pid)).unwrap_or(false)
}

/// True iff `dir` is or contains an in-flight atomic-write temp — reaping such
/// a dir would race a concurrent deliverer mid-rename. This covers three cases:
/// a directory whose own name carries `TMP_MARKER` (a [`create_peer_dir`] build
/// dir), and a `TMP_MARKER` file sitting directly in `dir` or in `dir/inbox/`
/// (delivered messages land in the inbox).
fn has_inflight_temp(dir: &Path) -> bool {
    dir.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains(crate::fsutil::TMP_MARKER))
        || has_temp_in(dir)
        || has_temp_in(&dir.join("inbox"))
}

fn has_temp_in(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.flatten().any(|e| {
        e.file_name()
            .to_str()
            .is_some_and(|n| n.contains(crate::fsutil::TMP_MARKER))
    })
}

/// Removes every dead peer directory (stale heartbeat + dead pid, no in-flight
/// temp). Returns the number actually removed; per-dir removal failures are
/// logged and not counted.
pub fn reap_stale(root: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(root) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        if has_inflight_temp(&path) || peer_is_alive(&path) {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("failed to reap stale peer dir {}: {e}", path.display()),
        }
    }
    removed
}

/// Live peers under `root`, excluding `exclude_handle` (the caller's own).
/// Sorted by handle for stable output. Propagates a `read_dir` error on `root`
/// (e.g. the registry does not exist yet); a live-looking dir whose `meta.json`
/// is missing/unreadable is logged and skipped rather than failing the listing.
pub fn list_live_peers(
    root: &Path,
    exclude_handle: Option<&str>,
) -> std::io::Result<Vec<PeerMeta>> {
    let entries = std::fs::read_dir(root)?;
    let mut peers: Vec<PeerMeta> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter(|e| !has_inflight_temp(&e.path()))
        .filter(|e| peer_is_alive(&e.path()))
        .filter_map(|e| {
            let path = e.path();
            match read_meta(&path) {
                Some(meta) => Some(meta),
                None => {
                    tracing::warn!(
                        "ignoring live-looking peer with unreadable meta: {}",
                        path.display()
                    );
                    None
                }
            }
        })
        .filter(|m| exclude_handle != Some(m.handle.as_str()))
        .collect();
    peers.sort_by(|a, b| a.handle.cmp(&b.handle));
    Ok(peers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn meta(handle: &str, pid: u32) -> PeerMeta {
        let mut m = PeerMeta::new(PeerMetaInit {
            handle: handle.into(),
            session_path: PathBuf::from("/s.json"),
            project_root: PathBuf::from("/proj"),
            connection: "conn".into(),
            model: "model".into(),
            preview: "preview".into(),
            streaming: false,
            can_reply: true,
        });
        m.pid = pid;
        m
    }

    #[test]
    fn handle_prefix_is_filesystem_safe_and_bounded() {
        assert_eq!(
            project_prefix(Path::new("/home/u/My Project (v2)")),
            "my-project-v2"
        );
        assert_eq!(project_prefix(Path::new("/")), "project");
        assert!(project_prefix(Path::new(&format!("/home/{}", "x".repeat(100)))).len() <= 24);
    }

    #[test]
    fn handle_is_deterministic_and_well_formed() {
        let a = handle_for(Path::new("/proj"), Path::new("/state/s.json"));
        let b = handle_for(Path::new("/proj"), Path::new("/state/s.json"));
        assert_eq!(a, b);
        assert!(super::super::message::is_acceptable_handle(&a), "{a}");
        assert!(a.starts_with("proj-"), "{a}");
        let c = handle_for(Path::new("/proj"), Path::new("/state/other.json"));
        assert_ne!(a, c);
    }

    #[cfg(unix)]
    #[test]
    fn create_peer_dir_is_0700_and_avoids_collisions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let root = dir.path().join("peers");
        let (h1, d1) = create_peer_dir(&root, "proj-1234abcd").unwrap();
        let (h2, d2) = create_peer_dir(&root, "proj-1234abcd").unwrap();
        assert_eq!(h1, "proj-1234abcd");
        assert_eq!(h2, "proj-1234abcd-1");
        assert_ne!(d1, d2);
        for d in [&root, &d1, &d2, &d1.join("inbox")] {
            let mode = std::fs::metadata(d).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{} mode {:o}", d.display(), mode);
        }
    }

    #[test]
    fn create_peer_dir_primes_a_heartbeat_before_returning() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("peers");
        let (_handle, peer_dir) = create_peer_dir(&root, "proj-1234abcd").unwrap();
        assert!(peer_dir.join("heartbeat").is_file());
        // The build dir was renamed into place, leaving no temp stray.
        let names: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("proj-1234abcd")]);
    }

    #[test]
    fn meta_round_trips_and_update_refreshes_fields() {
        let dir = tempdir().unwrap();
        let d = dir.path().join("p");
        std::fs::create_dir_all(&d).unwrap();
        write_meta(&d, &meta("proj-1234", 42)).unwrap();
        assert_eq!(read_meta(&d).unwrap().pid, 42);

        update_meta(&d, |m| {
            m.streaming = true;
            m.preview = "working".into();
        })
        .unwrap();
        let m = read_meta(&d).unwrap();
        assert!(m.streaming);
        assert_eq!(m.preview, "working");
    }

    #[test]
    fn unknown_meta_version_reads_as_none() {
        let dir = tempdir().unwrap();
        let d = dir.path().join("p");
        std::fs::create_dir_all(&d).unwrap();
        let mut m = meta("proj-1234", 42);
        m.version = 999;
        std::fs::write(d.join("meta.json"), serde_json::to_string(&m).unwrap()).unwrap();
        assert!(read_meta(&d).is_none());
    }

    #[test]
    fn read_meta_rejects_an_oversized_file() {
        let dir = tempdir().unwrap();
        let d = dir.path().join("p");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("meta.json"), "x".repeat(MAX_META_BYTES as usize + 1)).unwrap();
        assert!(read_meta(&d).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn read_meta_rejects_a_symlinked_file() {
        let dir = tempdir().unwrap();
        let d = dir.path().join("p");
        std::fs::create_dir_all(&d).unwrap();
        let real = dir.path().join("real.json");
        std::fs::write(
            &real,
            serde_json::to_string(&meta("proj-1234", 42)).unwrap(),
        )
        .unwrap();
        std::os::unix::fs::symlink(&real, d.join("meta.json")).unwrap();
        assert!(read_meta(&d).is_none());
    }

    #[test]
    fn liveness_tracks_heartbeat_and_pid() {
        let dir = tempdir().unwrap();

        // Fresh heartbeat => alive regardless of pid.
        let fresh = dir.path().join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        write_meta(&fresh, &meta("fresh", 999_999)).unwrap();
        touch_heartbeat(&fresh).unwrap();
        assert!(peer_is_alive(&fresh));

        // No heartbeat but our own pid => alive only where a pid probe exists
        // (suspended-but-running); elsewhere a stale heartbeat means dead.
        let pid_alive = dir.path().join("pid-alive");
        std::fs::create_dir_all(&pid_alive).unwrap();
        write_meta(&pid_alive, &meta("pid-alive", std::process::id())).unwrap();
        #[cfg(unix)]
        assert!(peer_is_alive(&pid_alive));
        #[cfg(not(unix))]
        assert!(!peer_is_alive(&pid_alive));

        // No heartbeat and a dead pid => not alive on every platform.
        let dead = dir.path().join("dead");
        std::fs::create_dir_all(&dead).unwrap();
        write_meta(&dead, &meta("dead", 999_999)).unwrap();
        assert!(!peer_is_alive(&dead));
    }

    #[test]
    fn inflight_temp_in_inbox_blocks_reaping_and_listing() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("peers");
        std::fs::create_dir_all(&root).unwrap();

        // A stale, dead peer with a concurrent deliverer mid-write in its inbox.
        let busy = root.join("busy-1");
        std::fs::create_dir_all(busy.join("inbox")).unwrap();
        write_meta(&busy, &meta("busy-1", 999_999)).unwrap();
        let tmp = busy
            .join("inbox")
            .join(format!("{}inbox-write", crate::fsutil::TMP_MARKER));
        std::fs::write(&tmp, "x").unwrap();

        assert!(!heartbeat_fresh(&busy));
        assert_eq!(reap_stale(&root), 0);
        assert!(busy.exists());
        assert!(list_live_peers(&root, None).unwrap().is_empty());
    }

    #[test]
    fn inflight_temp_at_peer_root_blocks_reaping_and_listing() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("peers");
        std::fs::create_dir_all(&root).unwrap();

        // A stale, dead peer with a concurrent write landing in its root
        // (meta.json), not its inbox.
        let busy = root.join("busy-2");
        std::fs::create_dir_all(busy.join("inbox")).unwrap();
        write_meta(&busy, &meta("busy-2", 999_999)).unwrap();
        let tmp = busy.join(format!("{}meta-write", crate::fsutil::TMP_MARKER));
        std::fs::write(&tmp, "x").unwrap();

        assert!(!heartbeat_fresh(&busy));
        assert_eq!(reap_stale(&root), 0);
        assert!(busy.exists());
        assert!(list_live_peers(&root, None).unwrap().is_empty());
    }

    #[test]
    fn list_live_peers_propagates_a_missing_root() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("no-such-peers-root");
        assert!(list_live_peers(&missing, None).is_err());
    }

    #[test]
    fn send_only_handle_is_pid_suffixed_and_well_formed() {
        let h = send_only_handle(Path::new("/home/u/My Proj"), 0x1a2b);
        assert_eq!(h, "my-proj-00001a2b");
        assert!(super::super::message::is_acceptable_handle(&h), "{h}");
    }

    #[test]
    fn canonical_path_hash_is_deterministic_and_distinguishes_paths() {
        let dir = tempdir().unwrap();
        assert_eq!(
            canonical_path_hash(dir.path()),
            canonical_path_hash(dir.path())
        );
        assert_ne!(
            canonical_path_hash(dir.path()),
            canonical_path_hash(&dir.path().join("other"))
        );
    }

    #[test]
    fn list_filters_dead_and_excluded_and_reap_removes_dead() {
        let dir = tempdir().unwrap();
        let root = dir.path().join("peers");
        std::fs::create_dir_all(&root).unwrap();

        let live = root.join("live-1");
        std::fs::create_dir_all(&live).unwrap();
        write_meta(&live, &meta("live-1", std::process::id())).unwrap();
        touch_heartbeat(&live).unwrap();

        let dead = root.join("dead-1");
        std::fs::create_dir_all(&dead).unwrap();
        write_meta(&dead, &meta("dead-1", 999_999)).unwrap();

        #[cfg(unix)]
        {
            let peers = list_live_peers(&root, None).unwrap();
            assert_eq!(peers.len(), 1);
            assert_eq!(peers[0].handle, "live-1");
            assert!(list_live_peers(&root, Some("live-1")).unwrap().is_empty());

            assert_eq!(reap_stale(&root), 1);
            assert!(!dead.exists());
            assert!(live.exists());
        }
    }
}
