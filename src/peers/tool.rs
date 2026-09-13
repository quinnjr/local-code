//! The two peer tools: `send_message` (agent-to-agent messaging) and
//! `list_peers` (discovery). Both are registered through the single
//! `agent::build::register_all_tools` path.

use std::sync::Arc;

use daimon::tool::{Tool, ToolOutput};

use super::registry;
use super::runtime::PeerRuntime;

/// `send_message { text, to? }`.
pub struct SendMessage {
    runtime: Arc<PeerRuntime>,
}

impl SendMessage {
    pub fn new(runtime: Arc<PeerRuntime>) -> Self {
        SendMessage { runtime }
    }
}

impl Tool for SendMessage {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a text message to another running local-code session (a peer) on this machine. \
         Use list_peers to discover handles. The recipient's user must approve your session \
         before your first message is delivered; once approved, your message interrupts their \
         current turn. To reply to a peer message you received, omit `to` — the reply goes back \
         to its sender. Replies are bounded by a hop budget; when it is exhausted, tell your \
         user instead of retrying. Messages are plain text, capped at 16 KiB."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "The message body." },
                "to": {
                    "type": "string",
                    "description": "Target peer handle from list_peers. Omit to reply to the \
                                    peer message currently being handled."
                }
            },
            "required": ["text"]
        })
    }

    async fn execute(&self, input: &serde_json::Value) -> daimon::Result<ToolOutput> {
        let text = input
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if text.trim().is_empty() {
            return Ok(ToolOutput::error("send_message requires non-empty `text`"));
        }
        let to = input.get("to").and_then(|v| v.as_str()).map(str::to_string);

        let runtime = self.runtime.clone();
        // `send` does synchronous filesystem work; keep it off the
        // current-thread runtime (matching the grep/artifacts convention).
        let result = tokio::task::spawn_blocking(move || runtime.send(to.as_deref(), &text)).await;
        match result {
            Ok(Ok(target)) => Ok(ToolOutput::text(format!("message sent to {target}"))),
            Ok(Err(e)) => Ok(ToolOutput::error(e.to_string())),
            Err(e) => Ok(ToolOutput::error(format!(
                "peer messaging task failed: {e}"
            ))),
        }
    }
}

/// `list_peers {}`.
pub struct ListPeers {
    peers_root: std::path::PathBuf,
    exclude_handle: String,
}

impl ListPeers {
    pub fn new(peers_root: std::path::PathBuf, exclude_handle: String) -> Self {
        ListPeers {
            peers_root,
            exclude_handle,
        }
    }
}

impl Tool for ListPeers {
    fn name(&self) -> &str {
        "list_peers"
    }

    fn description(&self) -> &str {
        "List other running local-code sessions (peers) on this machine: handle, project, model, \
         whether busy, and a preview of their first prompt. Use a handle from here as the `to` \
         argument of send_message."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }

    async fn execute(&self, _input: &serde_json::Value) -> daimon::Result<ToolOutput> {
        let root = self.peers_root.clone();
        let exclude = self.exclude_handle.clone();
        let peers = match tokio::task::spawn_blocking(move || {
            registry::list_live_peers(&root, Some(exclude.as_str()))
        })
        .await
        {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => {
                return Ok(ToolOutput::error(format!("could not list peers: {e}")));
            }
            Err(e) => {
                return Ok(ToolOutput::error(format!("peer list task failed: {e}")));
            }
        };

        Ok(ToolOutput::text(crate::peers::format_peer_list(
            &peers,
            &self.exclude_handle,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::peers::message::{MAX_HOPS, PeerMessage};

    fn runtime_in(dir: &std::path::Path, name: &str) -> Arc<PeerRuntime> {
        let project = dir.join("projects").join(name);
        std::fs::create_dir_all(&project).unwrap();
        PeerRuntime::create(
            dir.join("peers"),
            &project,
            &dir.join(format!("{name}.json")),
            "conn",
            "model",
            "first prompt",
        )
        .unwrap()
    }

    fn first_message(dir: &std::path::Path) -> PeerMessage {
        let path = std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .next()
            .unwrap();
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[tokio::test]
    async fn send_message_delivers_to_a_peer() {
        let dir = tempfile::tempdir().unwrap();
        let a = runtime_in(dir.path(), "alice");
        let b = runtime_in(dir.path(), "bob");

        let tool = SendMessage::new(a.clone());
        let out = tool
            .execute(&serde_json::json!({ "to": b.handle, "text": "ping" }))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.content);
        let msg = first_message(&b.dir.join("inbox"));
        assert_eq!(msg.text, "ping");
        assert_eq!(msg.hops, MAX_HOPS);
    }

    #[tokio::test]
    async fn send_message_errors_on_empty_text_and_unknown_target() {
        let dir = tempfile::tempdir().unwrap();
        let a = runtime_in(dir.path(), "alice");
        let tool = SendMessage::new(a);

        assert!(
            tool.execute(&serde_json::json!({ "to": "x", "text": "   " }))
                .await
                .unwrap()
                .is_error
        );
        assert!(
            tool.execute(&serde_json::json!({ "to": "nobody-0000", "text": "hi" }))
                .await
                .unwrap()
                .is_error
        );
        assert!(
            tool.execute(&serde_json::json!({ "text": "hi" }))
                .await
                .unwrap()
                .is_error
        );
    }

    #[tokio::test]
    async fn list_peers_reports_others_but_not_self() {
        let dir = tempfile::tempdir().unwrap();
        let a = runtime_in(dir.path(), "alice");
        let b = runtime_in(dir.path(), "bob");

        let tool = ListPeers::new(dir.path().join("peers"), a.handle.clone());
        let out = tool.execute(&serde_json::json!({})).await.unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains(&b.handle), "{}", out.content);
        assert!(
            out.content.contains(&format!("your handle: {}", a.handle)),
            "{}",
            out.content
        );
        // `a` appears only as the self handle, never as a listed peer line.
        assert!(
            !out.content.contains(&format!("  {} [", a.handle)),
            "{}",
            out.content
        );
        assert!(out.content.contains("first prompt"));
    }

    #[tokio::test]
    async fn list_peers_is_empty_when_the_only_peer_is_self() {
        let dir = tempfile::tempdir().unwrap();
        let a = runtime_in(dir.path(), "alice");
        let tool = ListPeers::new(dir.path().join("peers"), a.handle.clone());
        let out = tool.execute(&serde_json::json!({})).await.unwrap();
        assert!(!out.is_error);
        assert_eq!(out.content, "No other running sessions.");
    }
}
