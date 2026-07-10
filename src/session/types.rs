// src/session/types.rs

use serde::{Deserialize, Serialize};

use crate::permissions::types::PermissionTier;
use crate::tui::state::TranscriptEntry;
use daimon::model::types::Message;

/// The full on-disk shape of one session: the display transcript (for
/// immediate re-render) and the raw agent-facing message history (for
/// rebuilding the agent's memory) side by side, plus enough connection/tier
/// metadata to reconstruct the same `Model`/`PermissionGate` on resume. These
/// two message representations are not interconvertible (see this plan's
/// Architecture section), so both are stored rather than one being derived
/// from the other.
///
/// Note: `PartialEq` is implemented by hand rather than derived. `Message`
/// (from `daimon_core`, re-exported as `daimon::model::types::Message`) only
/// derives `Debug, Clone, Serialize, Deserialize` — it does not implement
/// `PartialEq` — so `messages: Vec<Message>` cannot participate in a derived
/// comparison. The hand-written impl compares every other field structurally
/// and falls back to comparing `messages` via their serialized JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionFile {
    /// Bumped only if this shape changes incompatibly; `load_session` refuses
    /// to load a file with an unrecognized version rather than guessing.
    pub version: u32,
    pub project_root: std::path::PathBuf,
    pub connection_name: String,
    pub model_name: String,
    pub tier: PermissionTier,
    pub created_at: String,
    pub updated_at: String,
    pub entries: Vec<TranscriptEntry>,
    pub messages: Vec<Message>,
}

pub const SESSION_FILE_VERSION: u32 = 1;

impl SessionFile {
    pub fn new(
        project_root: std::path::PathBuf,
        connection_name: String,
        model_name: String,
        tier: PermissionTier,
        created_at: String,
    ) -> Self {
        SessionFile {
            version: SESSION_FILE_VERSION,
            project_root,
            connection_name,
            model_name,
            tier,
            created_at: created_at.clone(),
            updated_at: created_at,
            entries: Vec::new(),
            messages: Vec::new(),
        }
    }
}

impl PartialEq for SessionFile {
    fn eq(&self, other: &Self) -> bool {
        self.version == other.version
            && self.project_root == other.project_root
            && self.connection_name == other.connection_name
            && self.model_name == other.model_name
            && self.tier == other.tier
            && self.created_at == other.created_at
            && self.updated_at == other.updated_at
            && self.entries == other.entries
            && messages_eq(&self.messages, &other.messages)
    }
}

/// `Message` has no `PartialEq`, so compare message histories structurally by
/// round-tripping each side through JSON (both sides already derive
/// `Serialize`, so this is cheap and exact for our purposes: two message
/// vectors are equal here iff they'd serialize identically).
fn messages_eq(a: &[Message], b: &[Message]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| serde_json::to_string(x).ok() == serde_json::to_string(y).ok())
}

/// One row in a `/resume` or `--resume` listing — everything needed to show
/// the user a human-readable choice without loading the full transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSummary {
    pub path: std::path::PathBuf,
    pub connection_name: String,
    pub model_name: String,
    pub updated_at: String,
    /// The text of the first `TranscriptEntry::UserTurn` in the session, if
    /// any, truncated to 60 chars — a human-recognizable label.
    pub first_user_turn_preview: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::types::PermissionTier;

    #[test]
    fn session_file_round_trips_through_json() {
        let mut session = SessionFile::new(
            "/home/user/proj".into(),
            "local-vllm".into(),
            "qwen2.5-coder-32b".into(),
            PermissionTier::Ask,
            "2026-07-06T10:00:00Z".into(),
        );
        session
            .entries
            .push(TranscriptEntry::UserTurn { text: "hi".into() });
        session.messages.push(Message::user("hi"));

        let json = serde_json::to_string_pretty(&session).unwrap();
        let back: SessionFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back, session);
    }

    #[test]
    fn new_session_has_matching_created_and_updated_timestamps_and_empty_history() {
        let session = SessionFile::new(
            "/proj".into(),
            "conn".into(),
            "model".into(),
            PermissionTier::FullAuto,
            "2026-07-06T10:00:00Z".into(),
        );
        assert_eq!(session.created_at, session.updated_at);
        assert!(session.entries.is_empty());
        assert!(session.messages.is_empty());
        assert_eq!(session.version, SESSION_FILE_VERSION);
    }
}
