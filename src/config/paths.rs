use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq)]
pub struct Paths {
    pub user_config_dir: PathBuf,
    pub project_config_dir: PathBuf,
    pub user_state_dir: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum PathsError {
    #[error("could not determine the OS config/state directories for this platform")]
    NoProjectDirs,
}

impl Paths {
    pub fn resolve(project_root: &Path) -> Result<Paths, PathsError> {
        let project_dirs = directories::ProjectDirs::from("dev", "local-code", "local-code")
            .ok_or(PathsError::NoProjectDirs)?;

        let user_state_dir = project_dirs
            .state_dir()
            .unwrap_or_else(|| project_dirs.data_dir())
            .to_path_buf();

        Ok(Paths {
            user_config_dir: project_dirs.config_dir().to_path_buf(),
            project_config_dir: project_root.join(".local-code"),
            user_state_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_config_dir_is_dot_local_code_under_project_root() {
        let project_root = Path::new("/home/user/myproject");
        let paths = Paths::resolve(project_root).expect("resolve should succeed");
        assert_eq!(
            paths.project_config_dir,
            Path::new("/home/user/myproject/.local-code")
        );
    }

    #[test]
    fn user_config_dir_ends_with_local_code() {
        let project_root = Path::new("/home/user/myproject");
        let paths = Paths::resolve(project_root).expect("resolve should succeed");
        assert!(paths.user_config_dir.ends_with("local-code"));
    }

    #[test]
    fn user_state_dir_ends_with_local_code() {
        let project_root = Path::new("/home/user/myproject");
        let paths = Paths::resolve(project_root).expect("resolve should succeed");
        assert!(paths.user_state_dir.ends_with("local-code"));
    }
}
