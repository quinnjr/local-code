// src/session/store.rs

use std::fs;
use std::path::Path;

use crate::session::paths::session_dir_for_project;
use crate::session::types::{SessionFile, SessionSummary, SESSION_FILE_VERSION};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("failed to read {path}: {source}")]
    Read { path: std::path::PathBuf, #[source] source: std::io::Error },
    #[error("failed to write {path}: {source}")]
    Write { path: std::path::PathBuf, #[source] source: std::io::Error },
    #[error("failed to parse session file {path}: {source}")]
    Parse { path: std::path::PathBuf, #[source] source: serde_json::Error },
    #[error("failed to serialize session: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("session file {path} has unsupported version {found} (expected {expected})")]
    UnsupportedVersion { path: std::path::PathBuf, found: u32, expected: u32 },
}

/// Overwrites `path` with `session`'s current contents, creating parent
/// directories as needed. Called after every completed turn, after
/// `/compact`, and after `/clear` starts a fresh session — see `src/tui/app.rs`.
pub fn save_session(path: &Path, session: &SessionFile) -> Result<(), SessionError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| SessionError::Write {
            path: path.to_path_buf(),
            source,
        })?;
    }
    let text = serde_json::to_string_pretty(session).map_err(SessionError::Serialize)?;
    fs::write(path, text).map_err(|source| SessionError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// Loads and validates one session file.
pub fn load_session(path: &Path) -> Result<SessionFile, SessionError> {
    let text = fs::read_to_string(path).map_err(|source| SessionError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let session: SessionFile = serde_json::from_str(&text).map_err(|source| SessionError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    if session.version != SESSION_FILE_VERSION {
        return Err(SessionError::UnsupportedVersion {
            path: path.to_path_buf(),
            found: session.version,
            expected: SESSION_FILE_VERSION,
        });
    }
    Ok(session)
}

/// Lists every session recorded for `project_root`, most-recently-updated
/// first. Unreadable/corrupt files are skipped rather than failing the whole
/// listing (a hand-edited or partially-written file shouldn't block
/// `/resume` from finding everything else).
pub fn list_sessions(user_state_dir: &Path, project_root: &Path) -> Result<Vec<SessionSummary>, SessionError> {
    let dir = session_dir_for_project(user_state_dir, project_root);
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut summaries = Vec::new();
    let read_dir = fs::read_dir(&dir).map_err(|source| SessionError::Read {
        path: dir.clone(),
        source,
    })?;
    for entry in read_dir.filter_map(|e| e.ok()) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(session) = load_session(&path) else { continue };
        let preview = session
            .entries
            .iter()
            .find_map(|e| match e {
                crate::tui::state::TranscriptEntry::UserTurn { text } => {
                    Some(text.chars().take(60).collect::<String>())
                }
                _ => None,
            });
        summaries.push(SessionSummary {
            path,
            connection_name: session.connection_name,
            model_name: session.model_name,
            updated_at: session.updated_at,
            first_user_turn_preview: preview,
        });
    }
    summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::types::PermissionTier;
    use tempfile::tempdir;

    fn sample(connection: &str, updated_at: &str) -> SessionFile {
        let mut s = SessionFile::new(
            "/proj".into(),
            connection.into(),
            "model".into(),
            PermissionTier::Ask,
            updated_at.into(),
        );
        s.updated_at = updated_at.into();
        s
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session.json");
        let session = sample("local-vllm", "2026-07-06T10:00:00Z");
        save_session(&path, &session).unwrap();
        let loaded = load_session(&path).unwrap();
        assert_eq!(loaded, session);
    }

    #[test]
    fn load_rejects_unsupported_version() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session.json");
        let mut session = sample("conn", "2026-07-06T10:00:00Z");
        session.version = 999;
        save_session(&path, &session).unwrap();
        let result = load_session(&path);
        assert!(matches!(result, Err(SessionError::UnsupportedVersion { found: 999, .. })));
    }

    #[test]
    fn list_sessions_returns_empty_when_no_directory_exists() {
        let dir = tempdir().unwrap();
        let sessions = list_sessions(dir.path(), Path::new("/nonexistent-project")).unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn list_sessions_sorts_most_recently_updated_first_and_skips_corrupt_files() {
        let user_state_dir = tempdir().unwrap();
        let project_root = Path::new("/proj");
        let dir = session_dir_for_project(user_state_dir.path(), project_root);
        fs::create_dir_all(&dir).unwrap();

        save_session(&dir.join("a.json"), &sample("older", "2026-07-01T00:00:00Z")).unwrap();
        save_session(&dir.join("b.json"), &sample("newer", "2026-07-06T00:00:00Z")).unwrap();
        fs::write(dir.join("corrupt.json"), "not json").unwrap();

        let sessions = list_sessions(user_state_dir.path(), project_root).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].connection_name, "newer");
        assert_eq!(sessions[1].connection_name, "older");
    }

    #[test]
    fn list_sessions_extracts_first_user_turn_preview() {
        let user_state_dir = tempdir().unwrap();
        let project_root = Path::new("/proj");
        let dir = session_dir_for_project(user_state_dir.path(), project_root);
        fs::create_dir_all(&dir).unwrap();

        let mut session = sample("conn", "2026-07-06T00:00:00Z");
        session.entries.push(crate::tui::state::TranscriptEntry::UserTurn {
            text: "fix the flaky test".into(),
        });
        save_session(&dir.join("s.json"), &session).unwrap();

        let sessions = list_sessions(user_state_dir.path(), project_root).unwrap();
        assert_eq!(sessions[0].first_user_turn_preview.as_deref(), Some("fix the flaky test"));
    }
}
