use std::fs;
use std::path::{Path, PathBuf};

use crate::permissions::types::PermissionTier;
use crate::session::paths::{new_session_path, session_dir_for_project};
use crate::session::types::{SESSION_FILE_VERSION, SessionFile, SessionSummary};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse session file {path}: {source}")]
    Parse {
        path: std::path::PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to serialize session: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("session file {path} has unsupported version {found} (expected {expected})")]
    UnsupportedVersion {
        path: std::path::PathBuf,
        found: u32,
        expected: u32,
    },
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

/// Allocates a fresh session file on disk and returns `(path, created_at)`.
/// The single shared "birth a session" recipe — used by `run_tui` for the
/// startup session and by `Workspace` for every new tab/pane, so the two can
/// never drift field-for-field (they previously open-coded identical
/// `new_session_path` → `SessionFile::new` → `save_session` sequences).
pub fn create_fresh_session(
    user_state_dir: &Path,
    project_root: &Path,
    connection_name: &str,
    model_name: &str,
    tier: PermissionTier,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(PathBuf, String), SessionError> {
    let path = new_session_path(user_state_dir, project_root, now);
    let created_at = now.to_rfc3339();
    let session = SessionFile::new(
        project_root.to_path_buf(),
        connection_name.to_string(),
        model_name.to_string(),
        tier,
        created_at.clone(),
    );
    save_session(&path, &session)?;
    Ok((path, created_at))
}

/// Loads and validates one session file.
pub fn load_session(path: &Path) -> Result<SessionFile, SessionError> {
    let text = fs::read_to_string(path).map_err(|source| SessionError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let session: SessionFile =
        serde_json::from_str(&text).map_err(|source| SessionError::Parse {
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
pub fn list_sessions(
    user_state_dir: &Path,
    project_root: &Path,
) -> Result<Vec<SessionSummary>, SessionError> {
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
        let Ok(session) = load_session(&path) else {
            continue;
        };
        // Sessions with no transcript at all are skipped: every launch and
        // every new workspace tab/pane eagerly writes an (empty) session
        // file, so without this filter a user who opens a few tabs and types
        // in one would see the resume list fill up with permanently empty,
        // previewless entries. The file itself is kept — it gains entries
        // (and appears here) after its first turn.
        if session.entries.is_empty() {
            continue;
        }
        let preview = session.entries.iter().find_map(|e| match e {
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
        // A non-empty transcript so `list_sessions` (which skips
        // never-used sessions) includes it.
        s.entries
            .push(crate::tui::state::TranscriptEntry::UserTurn {
                text: "hello".into(),
            });
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
        assert!(matches!(
            result,
            Err(SessionError::UnsupportedVersion { found: 999, .. })
        ));
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

        save_session(
            &dir.join("a.json"),
            &sample("older", "2026-07-01T00:00:00Z"),
        )
        .unwrap();
        save_session(
            &dir.join("b.json"),
            &sample("newer", "2026-07-06T00:00:00Z"),
        )
        .unwrap();
        fs::write(dir.join("corrupt.json"), "not json").unwrap();

        let sessions = list_sessions(user_state_dir.path(), project_root).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].connection_name, "newer");
        assert_eq!(sessions[1].connection_name, "older");
    }

    #[test]
    fn list_sessions_skips_sessions_with_no_entries() {
        let user_state_dir = tempdir().unwrap();
        let project_root = Path::new("/proj");
        let dir = session_dir_for_project(user_state_dir.path(), project_root);
        fs::create_dir_all(&dir).unwrap();

        let mut empty = sample("untouched-tab", "2026-07-06T00:00:00Z");
        empty.entries.clear();
        save_session(&dir.join("empty.json"), &empty).unwrap();
        save_session(
            &dir.join("used.json"),
            &sample("used", "2026-07-05T00:00:00Z"),
        )
        .unwrap();

        let sessions = list_sessions(user_state_dir.path(), project_root).unwrap();
        assert_eq!(sessions.len(), 1, "the never-used session is hidden");
        assert_eq!(sessions[0].connection_name, "used");
    }

    #[test]
    fn create_fresh_session_writes_the_file_and_returns_its_path() {
        let user_state_dir = tempdir().unwrap();
        let project_root = Path::new("/proj");
        let now = chrono::Utc::now();
        let (path, created_at) = create_fresh_session(
            user_state_dir.path(),
            project_root,
            "conn",
            "model",
            PermissionTier::Ask,
            now,
        )
        .unwrap();
        assert_eq!(created_at, now.to_rfc3339());
        let loaded = load_session(&path).unwrap();
        assert_eq!(loaded.connection_name, "conn");
        assert_eq!(loaded.model_name, "model");
        assert!(loaded.entries.is_empty());
    }

    #[test]
    fn list_sessions_extracts_first_user_turn_preview() {
        let user_state_dir = tempdir().unwrap();
        let project_root = Path::new("/proj");
        let dir = session_dir_for_project(user_state_dir.path(), project_root);
        fs::create_dir_all(&dir).unwrap();

        let mut session = sample("conn", "2026-07-06T00:00:00Z");
        session.entries.clear();
        session
            .entries
            .push(crate::tui::state::TranscriptEntry::UserTurn {
                text: "fix the flaky test".into(),
            });
        save_session(&dir.join("s.json"), &session).unwrap();

        let sessions = list_sessions(user_state_dir.path(), project_root).unwrap();
        assert_eq!(
            sessions[0].first_user_turn_preview.as_deref(),
            Some("fix the flaky test")
        );
    }
}
