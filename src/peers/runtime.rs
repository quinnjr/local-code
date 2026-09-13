//! `PeerRuntime`: the per-session handle for sending, receiving, consent, and
//! heartbeat. One instance is shared (via `Arc`) by the session's App, its
//! watcher/heartbeat tasks, and every tool instance the agent is rebuilt with,
//! so reply context and per-sender approvals survive `/model`/`/resume`/MCP
//! rebuilds.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use super::message::{MAX_HOPS, PeerError, PeerMessage, ReplyTarget};
use super::registry;

/// A running session's peer identity.
pub struct PeerRuntime {
    pub handle: String,
    /// `peers_root/<handle>`; empty for a send-only runtime.
    pub dir: PathBuf,
    /// `dir/inbox`; empty for a send-only runtime.
    pub inbox: PathBuf,
    /// Send-only runtimes (`send_only`) create no directory and cannot receive.
    pub can_reply: bool,
    peers_root: PathBuf,
    /// Who a reply should go to (set on injection, read by `send`).
    reply: Mutex<Option<ReplyTarget>>,
    /// Senders the user approved this session.
    approved: Mutex<HashSet<String>>,
}

impl PeerRuntime {
    /// Registers a live peer: allocates an owned `peers/<handle>/`, writes
    /// `meta.json`, and primes the heartbeat.
    pub fn create(
        peers_root: PathBuf,
        project_root: &Path,
        session_path: &Path,
        connection: &str,
        model: &str,
        preview: &str,
    ) -> std::io::Result<std::sync::Arc<Self>> {
        registry::ensure_peers_root(&peers_root)?;
        let base = registry::handle_for(project_root, session_path);
        let (handle, dir) = registry::create_peer_dir(&peers_root, &base)?;
        let meta = registry::PeerMeta::new(registry::PeerMetaInit {
            handle: handle.clone(),
            session_path: session_path.to_path_buf(),
            project_root: project_root.to_path_buf(),
            connection: connection.to_string(),
            model: model.to_string(),
            preview: preview.to_string(),
            streaming: false,
            can_reply: true,
        });
        // If registration fails after the directory exists, remove it so a
        // half-registered peer is not left discoverable.
        if let Err(e) =
            registry::write_meta(&dir, &meta).and_then(|()| registry::touch_heartbeat(&dir))
        {
            registry::remove_peer_dir(&dir);
            return Err(e);
        }
        let inbox = dir.join("inbox");
        Ok(std::sync::Arc::new(PeerRuntime {
            handle,
            dir,
            inbox,
            can_reply: true,
            peers_root,
            reply: Mutex::new(None),
            approved: Mutex::new(HashSet::new()),
        }))
    }

    /// A send-only identity (headless `-p`): can resolve targets and send, but
    /// registers nothing and cannot receive replies.
    pub fn send_only(peers_root: PathBuf, project_root: &Path) -> std::sync::Arc<Self> {
        let handle = registry::send_only_handle(project_root, std::process::id());
        std::sync::Arc::new(PeerRuntime {
            handle,
            dir: PathBuf::new(),
            inbox: PathBuf::new(),
            can_reply: false,
            peers_root,
            reply: Mutex::new(None),
            approved: Mutex::new(HashSet::new()),
        })
    }

    pub fn peers_root(&self) -> &Path {
        &self.peers_root
    }

    /// True iff this runtime owns an on-disk peer directory (i.e. it is not
    /// send-only). Intentionally equal to `can_reply` today — both are derived
    /// from having a dir — but this predicate is the guard for directory
    /// operations while `can_reply` is the wire/semantic field stamped into
    /// outgoing messages.
    fn owns_dir(&self) -> bool {
        !self.dir.as_os_str().is_empty()
    }

    /// Resolves the target and hop count: a reply when there is a reply context
    /// and `to` is absent or names it, otherwise a fresh send. Then delivers.
    /// Returns the handle the message was sent to.
    pub fn send(&self, to: Option<&str>, text: &str) -> Result<String, PeerError> {
        let reply = self
            .reply
            .lock()
            .expect("peer reply mutex poisoned")
            .clone();
        let (target, inbound_hops, is_reply) = match (&reply, to) {
            (Some(ctx), None) => (ctx.to.clone(), ctx.hops, true),
            (Some(ctx), Some(t)) if t == ctx.to => (ctx.to.clone(), ctx.hops, true),
            (_, Some(t)) => (t.to_string(), MAX_HOPS, false),
            (None, None) => return Err(PeerError::NoTarget),
        };
        // The inbound message's hop count is this turn's budget: a reply spends
        // one, so a message that arrived with 0 left cannot be answered.
        let hops = if is_reply {
            inbound_hops.checked_sub(1).ok_or(PeerError::Exhausted)?
        } else {
            inbound_hops
        };
        let msg = PeerMessage::new(
            self.handle.clone(),
            target.clone(),
            text.to_string(),
            hops,
            self.can_reply,
            chrono::Utc::now().to_rfc3339(),
        );
        super::message::deliver(&self.peers_root, &target, &msg)?;
        Ok(target)
    }

    /// Sets (or clears) the conversation this session is answering.
    pub fn set_reply(&self, target: Option<ReplyTarget>) {
        *self.reply.lock().expect("peer reply mutex poisoned") = target;
    }

    /// Marks `handle`'s owner approved for this session (idempotent).
    pub fn approve(&self, handle: &str) {
        self.approved
            .lock()
            .expect("peer approved mutex poisoned")
            .insert(handle.to_string());
    }

    pub fn is_approved(&self, handle: &str) -> bool {
        self.approved
            .lock()
            .expect("peer approved mutex poisoned")
            .contains(handle)
    }

    /// Refreshes `streaming`/`preview` in `meta.json`, if this runtime owns one.
    pub fn refresh_meta(&self, streaming: bool, preview: &str) {
        if !self.owns_dir() {
            return;
        }
        if let Err(e) = registry::update_meta(&self.dir, |m| {
            m.streaming = streaming;
            m.preview = preview.to_string();
        }) {
            tracing::warn!("failed to refresh peer meta: {e}");
        }
    }

    /// Rewrites the heartbeat (and is a no-op for send-only runtimes).
    pub fn heartbeat(&self) {
        if !self.owns_dir() {
            return;
        }
        if let Err(e) = registry::touch_heartbeat(&self.dir) {
            tracing::warn!("failed to refresh peer heartbeat: {e}");
        }
    }

    /// Removes this peer's directory (best-effort; no-op for send-only).
    pub fn remove_dir(&self) {
        if self.owns_dir() {
            registry::remove_peer_dir(&self.dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peers::message::MESSAGE_VERSION;

    fn runtime_in(dir: &Path, name: &str) -> std::sync::Arc<PeerRuntime> {
        let peers = dir.join("peers");
        let project = dir.join("projects").join(name);
        let session = dir.join(format!("{name}.json"));
        std::fs::create_dir_all(&project).unwrap();
        PeerRuntime::create(peers, &project, &session, "conn", "model", "preview").unwrap()
    }

    fn inbox_files(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|it| it.flatten().map(|e| e.path()).collect())
            .unwrap_or_default()
    }

    fn first_message(dir: &Path) -> PeerMessage {
        let files = inbox_files(dir);
        let text = std::fs::read_to_string(&files[0]).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn create_registers_a_directory_with_meta_and_heartbeat() {
        let dir = tempfile::tempdir().unwrap();
        let peers = dir.path().join("peers");
        let project = dir.path().join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let rt = PeerRuntime::create(
            peers.clone(),
            &project,
            &dir.path().join("s.json"),
            "conn",
            "model",
            "preview",
        )
        .unwrap();

        assert!(rt.dir.join("meta.json").is_file());
        assert!(rt.dir.join("heartbeat").is_file());
        assert!(rt.dir.join("inbox").is_dir());
        let meta = registry::read_meta(&rt.dir).unwrap();
        assert_eq!(meta.handle, rt.handle);
        assert!(meta.can_reply);
    }

    #[test]
    fn fresh_send_uses_max_hops_and_reply_decrements_without_to() {
        let dir = tempfile::tempdir().unwrap();
        let a = runtime_in(dir.path(), "alice");
        let b = runtime_in(dir.path(), "bob");
        let b_inbox = b.dir.join("inbox");

        a.send(Some(&b.handle), "hi").unwrap();
        assert_eq!(first_message(&b_inbox).hops, MAX_HOPS);
        assert_eq!(first_message(&b_inbox).from, a.handle);

        // A reply (no `to`) targets the reply context and spends one hop.
        a.set_reply(Some(ReplyTarget {
            to: b.handle.clone(),
            hops: MAX_HOPS,
        }));
        a.send(None, "reply").unwrap();
        let msgs = inbox_files(&b_inbox);
        assert_eq!(msgs.len(), 2);
        let latest = first_message(&b_inbox);
        assert_eq!(latest.hops, MAX_HOPS - 1);
    }

    #[test]
    fn explicit_to_naming_the_context_is_a_reply_but_a_third_party_is_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let a = runtime_in(dir.path(), "alice");
        let b = runtime_in(dir.path(), "bob");
        let c = runtime_in(dir.path(), "carol");

        a.set_reply(Some(ReplyTarget {
            to: b.handle.clone(),
            hops: 3,
        }));
        a.send(Some(&b.handle), "reply to bob").unwrap();
        assert_eq!(first_message(&b.dir.join("inbox")).hops, 2);

        a.send(Some(&c.handle), "new to carol").unwrap();
        assert_eq!(first_message(&c.dir.join("inbox")).hops, MAX_HOPS);
    }

    #[test]
    fn an_exhausted_context_refuses_a_reply_but_not_a_fresh_send() {
        let dir = tempfile::tempdir().unwrap();
        let a = runtime_in(dir.path(), "alice");
        let b = runtime_in(dir.path(), "bob");
        let c = runtime_in(dir.path(), "carol");

        a.set_reply(Some(ReplyTarget {
            to: b.handle.clone(),
            hops: 0,
        }));
        assert!(matches!(a.send(None, "x"), Err(PeerError::Exhausted)));
        assert!(matches!(
            a.send(Some(&b.handle), "x"),
            Err(PeerError::Exhausted)
        ));
        // A fresh send to someone else is unaffected.
        a.send(Some(&c.handle), "hi").unwrap();
    }

    #[test]
    fn no_context_and_no_to_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let a = runtime_in(dir.path(), "alice");
        assert!(matches!(a.send(None, "x"), Err(PeerError::NoTarget)));
    }

    #[test]
    fn approvals_are_per_sender() {
        let dir = tempfile::tempdir().unwrap();
        let a = runtime_in(dir.path(), "alice");
        assert!(!a.is_approved("bob-1"));
        a.approve("bob-1");
        assert!(a.is_approved("bob-1"));
        assert!(!a.is_approved("carol-1"));
    }

    #[test]
    fn send_only_has_no_dir_marks_messages_unrepliable_and_still_sends() {
        let dir = tempfile::tempdir().unwrap();
        let b = runtime_in(dir.path(), "bob");
        let sender = PeerRuntime::send_only(dir.path().join("peers"), dir.path());

        assert!(!sender.can_reply);
        assert!(!sender.dir.exists());
        sender.send(Some(&b.handle), "from headless").unwrap();

        let msg = first_message(&b.dir.join("inbox"));
        assert_eq!(msg.version, MESSAGE_VERSION);
        assert!(!msg.can_reply);
    }

    #[test]
    fn refresh_meta_and_heartbeat_are_noops_for_send_only() {
        let dir = tempfile::tempdir().unwrap();
        let sender = PeerRuntime::send_only(dir.path().join("peers"), dir.path());
        sender.refresh_meta(true, "x"); // must not panic
        sender.heartbeat();
        sender.remove_dir();
    }
}
