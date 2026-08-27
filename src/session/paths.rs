use std::path::{Path, PathBuf};

/// Derives a filesystem-safe, human-recognizable directory name for a
/// project so each project's sessions live in their own subdirectory under
/// `Paths::user_state_dir` (which is itself global-per-user, not
/// project-scoped — see this plan's Architecture section). Not
/// cryptographically strong or guaranteed collision-free across Rust
/// versions (`DefaultHasher` is not a stable hash across releases per its own
/// docs) — acceptable here since a collision only means two projects'
/// sessions land in the same listing directory, a cosmetic issue caught
/// immediately by `SessionFile::project_root` not matching, not silent data
/// loss.
pub fn project_slug(project_root: &Path) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let canonical = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());

    let mut hasher = DefaultHasher::new();
    canonical.hash(&mut hasher);
    let hash = hasher.finish();

    let readable: String = canonical
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let readable = readable.trim_matches('_');
    let tail: String = readable
        .chars()
        .rev()
        .take(40)
        .collect::<String>()
        .chars()
        .rev()
        .collect();

    format!("{tail}-{hash:016x}")
}

/// The directory holding every session file for `project_root`, under the
/// resolved user state dir.
pub fn session_dir_for_project(user_state_dir: &Path, project_root: &Path) -> PathBuf {
    user_state_dir
        .join("sessions")
        .join(project_slug(project_root))
}

/// Builds a fresh, not-yet-existing session file path for `project_root`,
/// timestamped to the second so concurrent sessions (unlikely, but possible
/// if two `local-code` processes start in the same second) still sort
/// distinctly enough for `list_sessions` — ties are broken by an incrementing
/// suffix.
pub fn new_session_path(
    user_state_dir: &Path,
    project_root: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> PathBuf {
    let dir = session_dir_for_project(user_state_dir, project_root);
    let base = now.format("%Y%m%dT%H%M%SZ").to_string();
    let mut candidate = dir.join(format!("{base}.json"));
    let mut suffix = 1u32;
    while candidate.exists() {
        candidate = dir.join(format!("{base}-{suffix}.json"));
        suffix += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn same_project_root_yields_the_same_slug() {
        let a = project_slug(Path::new("/home/user/myproject"));
        let b = project_slug(Path::new("/home/user/myproject"));
        assert_eq!(a, b);
    }

    #[test]
    fn different_project_roots_yield_different_slugs() {
        let a = project_slug(Path::new("/home/user/project-a"));
        let b = project_slug(Path::new("/home/user/project-b"));
        assert_ne!(a, b);
    }

    #[test]
    fn slug_contains_only_filesystem_safe_characters() {
        let slug = project_slug(Path::new("/home/user/my project (v2)"));
        assert!(
            slug.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        );
    }

    #[test]
    fn session_dir_is_nested_under_sessions_and_the_project_slug() {
        let dir = session_dir_for_project(Path::new("/state"), Path::new("/home/user/myproject"));
        assert!(dir.starts_with("/state/sessions"));
        assert!(dir.ends_with(project_slug(Path::new("/home/user/myproject"))));
    }

    #[test]
    fn new_session_path_avoids_colliding_with_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let user_state_dir = dir.path();
        let project_root = Path::new("/home/user/myproject");
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-06T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);

        let first = new_session_path(user_state_dir, project_root, now);
        std::fs::create_dir_all(first.parent().unwrap()).unwrap();
        std::fs::write(&first, "{}").unwrap();

        let second = new_session_path(user_state_dir, project_root, now);
        assert_ne!(first, second);
        assert!(second.to_string_lossy().contains("-1.json"));
    }
}
