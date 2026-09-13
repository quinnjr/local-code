//! Inter-session peer messaging.
//!
//! Every running local-code session can register itself (`registry`), discover
//! other live sessions, and exchange messages through per-session filesystem
//! mailboxes (`message`). `runtime::PeerRuntime` is the per-session handle
//! shared by the App, the watcher/heartbeat tasks, and the agent's tools; the
//! TUI drains its inbox (`watch`) and injects approved messages as interrupting
//! turns. `tool` exposes `send_message`/`list_peers` to the model.

pub mod message;
pub mod registry;
pub mod runtime;
pub mod tool;
pub mod watch;

/// Longest rendered handle in the peer list, in characters.
const MAX_HANDLE_CHARS: usize = 64;
/// Longest rendered model name in the peer list, in characters.
const MAX_MODEL_CHARS: usize = 120;
/// Longest rendered prompt preview in the peer list, in characters.
const MAX_PREVIEW_CHARS: usize = 200;

/// One-line-per-peer rendering shared by the `list_peers` tool and `/sessions`.
///
/// Every peer-supplied field is untrusted input: each goes through
/// [`message::sanitize_peer_text`] (dropping ANSI/OSC/bidi escapes) and is
/// capped to a bounded width before it is painted, so a hostile peer cannot
/// spoof or flood the list.
pub fn format_peer_list(peers: &[registry::PeerMeta], self_handle: &str) -> String {
    if peers.is_empty() {
        return "No other running sessions.".to_string();
    }
    let self_handle = cap(message::sanitize_peer_text(self_handle), MAX_HANDLE_CHARS);
    let mut out = format!("Running sessions (your handle: {self_handle}):");
    for p in peers {
        let handle = cap(message::sanitize_peer_text(&p.handle), MAX_HANDLE_CHARS);
        let project = message::sanitize_peer_text(&p.project_root.display().to_string());
        let model = cap(message::sanitize_peer_text(&p.model), MAX_MODEL_CHARS);
        let busy = if p.streaming { " (busy)" } else { "" };
        let send_only = if p.can_reply { "" } else { " (send-only)" };
        let preview = cap(message::sanitize_peer_text(&p.preview), MAX_PREVIEW_CHARS);
        let preview = if preview.is_empty() {
            String::new()
        } else {
            format!(" — {preview}")
        };
        out.push_str(&format!(
            "\n  {handle} [{project}] {model}{busy}{send_only}{preview}"
        ));
    }
    out
}

/// Truncates `s` to at most `max` characters (not bytes). Shorter input is
/// returned unchanged; callers sanitize first so the cap counts safe text.
fn cap(s: String, max: usize) -> String {
    if s.chars().count() <= max {
        s
    } else {
        s.chars().take(max).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn meta(handle: &str) -> registry::PeerMeta {
        registry::PeerMeta::new(registry::PeerMetaInit {
            handle: handle.into(),
            session_path: PathBuf::from("/state/s.json"),
            project_root: PathBuf::from("/proj"),
            connection: "conn".into(),
            model: "gpt-4o".into(),
            preview: String::new(),
            streaming: false,
            can_reply: true,
        })
    }

    #[test]
    fn format_peer_list_empty_is_a_clear_message() {
        assert_eq!(
            format_peer_list(&[], "me-1234"),
            "No other running sessions."
        );
    }

    #[test]
    fn format_peer_list_annotates_busy_send_only_and_preview() {
        let mut busy = meta("bob-1");
        busy.streaming = true;
        busy.preview = "first prompt".into();
        let mut send_only = meta("carol-1");
        send_only.can_reply = false;

        let out = format_peer_list(&[busy, send_only], "me-1234");
        assert_eq!(
            out,
            "Running sessions (your handle: me-1234):\n  \
             bob-1 [/proj] gpt-4o (busy) — first prompt\n  \
             carol-1 [/proj] gpt-4o (send-only)"
        );
    }

    #[test]
    fn format_peer_list_sanitizes_and_caps_untrusted_fields() {
        let mut evil = meta("bob-1");
        evil.project_root = PathBuf::from("/proj\x1b[2J");
        evil.model = "m".repeat(MAX_MODEL_CHARS + 50);
        evil.preview = "p".repeat(MAX_PREVIEW_CHARS + 50);

        let out = format_peer_list(&[evil], "me\x1b[2J");
        assert!(!out.contains('\x1b'), "escape survived: {out:?}");
        assert!(out.contains("/proj[2J"), "{out}");
        assert!(
            !out.contains(&"m".repeat(MAX_MODEL_CHARS + 1)),
            "model not capped"
        );
        assert!(
            !out.contains(&"p".repeat(MAX_PREVIEW_CHARS + 1)),
            "preview not capped"
        );
        assert!(out.contains(&"m".repeat(MAX_MODEL_CHARS)), "{out}");
        assert!(out.contains(&"p".repeat(MAX_PREVIEW_CHARS)), "{out}");
    }
}
