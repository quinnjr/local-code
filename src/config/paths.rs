use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Paths {
    /// Per-user config dir, resolved per-OS by `directories` (XDG config dir
    /// on Linux & the BSDs, roaming `%APPDATA%` on Windows, `~/Library/Application
    /// Support` on macOS). See `Paths::resolve` for the concrete paths.
    pub user_config_dir: PathBuf,
    /// `<project_root>/.local-code` — always project-local by design, never
    /// OS-resolved.
    pub project_config_dir: PathBuf,
    /// Per-user, machine-local state dir: sessions live here, and sessions
    /// reference absolute project paths of this machine, so this must NOT be
    /// the roaming profile on Windows (`%LOCALAPPDATA%`, not `%APPDATA%`).
    pub user_state_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("could not determine the OS config/state directories for this platform")]
    NoProjectDirs,
}

impl Paths {
    /// Resolves the per-OS config/state dirs plus the project-local config
    /// dir under `project_root`.
    ///
    /// With `ProjectDirs::from("dev", "local-code", "local-code")` the
    /// concrete expansions are:
    /// - Linux & the BSDs: config `$XDG_CONFIG_HOME/local-code` (or
    ///   `~/.config/local-code`); state `$XDG_STATE_HOME/local-code` (or
    ///   `~/.local/state/local-code`).
    /// - Windows: config `%APPDATA%\local-code\local-code\config` (roaming);
    ///   state `%LOCALAPPDATA%\local-code\local-code\data` (machine-local).
    /// - macOS: `~/Library/Application Support/dev.local-code.local-code`
    ///   for both.
    pub fn resolve(project_root: &Path) -> Result<Paths, PathsError> {
        // `directories` implements the per-OS directory conventions for us:
        // the XDG Base Directory spec on unix-likes, Known Folders
        // (%APPDATA%/%LOCALAPPDATA%) on Windows, ~/Library on macOS.
        let project_dirs = directories::ProjectDirs::from("dev", "local-code", "local-code")
            .ok_or(PathsError::NoProjectDirs)?;

        // `state_dir` only exists on XDG systems (XDG_STATE_HOME); it is
        // `None` on Windows and macOS. Fall back to the *local* data dir
        // there — `%LOCALAPPDATA%` on Windows, never the roaming `%APPDATA%`
        // — because sessions are machine-local state (they reference
        // absolute project paths that only exist on this machine) and must
        // not roam across machines via a roaming profile. On macOS
        // `data_local_dir` == `data_dir`, so behavior there is unchanged.
        let user_state_dir = project_dirs
            .state_dir()
            .unwrap_or_else(|| project_dirs.data_local_dir())
            .to_path_buf();

        // One-time v0.1.0 compat: releases before the machine-local state
        // dir stored sessions under the roaming data dir; move them across.
        #[cfg(target_os = "windows")]
        migrate_legacy_sessions(project_dirs.data_dir(), &user_state_dir);

        Ok(Paths {
            user_config_dir: project_dirs.config_dir().to_path_buf(),
            project_config_dir: project_root.join(".local-code"),
            user_state_dir,
        })
    }
}

// One-time v0.1.0 compat: releases before the machine-local state dir
// stored sessions under the roaming data dir. Move them across once —
// best-effort: if the rename fails (e.g. folder redirection putting the
// two on different volumes), the legacy files simply stay where they are.
// This deliberately couples `config::paths` to the knowledge that sessions
// live in a `sessions/` subdir of the state dir; that's acceptable and
// documented here rather than spread across modules.
#[cfg(target_os = "windows")]
fn migrate_legacy_sessions(legacy_data_dir: &Path, new_state_dir: &Path) {
    let legacy_sessions = legacy_data_dir.join("sessions");
    let new_sessions = new_state_dir.join("sessions");
    if legacy_sessions.exists() && !new_sessions.exists() {
        let _ = std::fs::rename(&legacy_sessions, &new_sessions);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_config_dir_is_dot_local_code_under_project_root() {
        // A plain join, so this holds identically on every platform.
        let project_root = Path::new("/home/user/myproject");
        let paths = Paths::resolve(project_root).expect("resolve should succeed");
        assert_eq!(
            paths.project_config_dir,
            Path::new("/home/user/myproject/.local-code")
        );
    }

    /// The XDG Base Directory spec: an absolute `$XDG_*_HOME` wins; a
    /// relative or unset one falls back to the `$HOME`-relative default.
    /// Derives the expectation straight from the environment (no env
    /// mutation, so no cross-test races) instead of restating
    /// `directories`' internals.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn xdg_dir(var: &str, home_fallback: &str) -> PathBuf {
        let from_var = std::env::var_os(var)
            .map(PathBuf::from)
            .filter(|p| p.is_absolute());
        from_var.unwrap_or_else(|| {
            let home = std::env::var_os("HOME").expect("HOME must be set on unix");
            Path::new(&home).join(home_fallback)
        })
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn user_dirs_follow_the_xdg_base_directory_spec() {
        let paths = Paths::resolve(Path::new("project")).expect("resolve should succeed");
        assert_eq!(
            paths.user_config_dir,
            xdg_dir("XDG_CONFIG_HOME", ".config").join("local-code")
        );
        assert_eq!(
            paths.user_state_dir,
            xdg_dir("XDG_STATE_HOME", ".local/state").join("local-code")
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn user_dirs_follow_windows_known_folder_conventions() {
        let paths = Paths::resolve(Path::new("project")).expect("resolve should succeed");
        let roaming = PathBuf::from(std::env::var_os("APPDATA").expect("APPDATA set on windows"));
        let local =
            PathBuf::from(std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA set on windows"));

        // Config may roam with the user profile...
        assert!(paths.user_config_dir.starts_with(&roaming));
        // ...but session state is machine-local and must NOT roam.
        assert!(paths.user_state_dir.starts_with(&local));
        assert!(
            !paths.user_state_dir.starts_with(&roaming),
            "sessions must land in %LOCALAPPDATA%, not the roaming %APPDATA%"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn user_dirs_live_under_library_application_support() {
        let paths = Paths::resolve(Path::new("project")).expect("resolve should succeed");
        let home = std::env::var_os("HOME").expect("HOME must be set on macos");
        let app_support = Path::new(&home).join("Library/Application Support");
        assert!(paths.user_config_dir.starts_with(&app_support));
        assert!(paths.user_state_dir.starts_with(&app_support));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn legacy_sessions_are_migrated_to_the_machine_local_dir() {
        // v0.1.0 layout present, nothing in the new location yet: the whole
        // sessions tree moves across.
        let legacy = tempfile::tempdir().expect("tempdir");
        let new = tempfile::tempdir().expect("tempdir");
        let legacy_session = legacy.path().join("sessions").join("some-slug");
        std::fs::create_dir_all(&legacy_session).expect("create legacy session dir");
        std::fs::write(legacy_session.join("s.json"), b"{}").expect("write legacy session");

        migrate_legacy_sessions(legacy.path(), new.path());

        assert!(
            new.path()
                .join("sessions")
                .join("some-slug")
                .join("s.json")
                .exists()
        );
        assert!(!legacy.path().join("sessions").exists());

        // New sessions dir already present: legacy files stay put rather
        // than clobbering or merging into the new location.
        let legacy = tempfile::tempdir().expect("tempdir");
        let new = tempfile::tempdir().expect("tempdir");
        let legacy_session = legacy.path().join("sessions").join("some-slug");
        std::fs::create_dir_all(&legacy_session).expect("create legacy session dir");
        std::fs::write(legacy_session.join("s.json"), b"{}").expect("write legacy session");
        std::fs::create_dir_all(new.path().join("sessions")).expect("create new sessions dir");

        migrate_legacy_sessions(legacy.path(), new.path());

        assert!(legacy_session.join("s.json").exists());
    }
}
