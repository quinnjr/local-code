use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PermissionSettings {
    #[serde(default)]
    pub always_allow: Vec<String>,
    #[serde(default)]
    pub always_deny: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SettingsFile {
    #[serde(default)]
    pub permissions: PermissionSettings,
}

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: std::path::PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// Loads `settings.toml` from both `user_config_dir` and `project_config_dir` and
/// unions their `always_allow`/`always_deny` lists (both layers are additive safety
/// hints — a rule present at either level applies). Missing files yield empty lists,
/// not an error.
pub fn load_settings(
    user_config_dir: &Path,
    project_config_dir: &Path,
) -> Result<PermissionSettings, SettingsError> {
    let user = load_one(&user_config_dir.join("settings.toml"))?;
    let project = load_one(&project_config_dir.join("settings.toml"))?;

    let mut always_allow = user.permissions.always_allow;
    for rule in project.permissions.always_allow {
        if !always_allow.contains(&rule) {
            always_allow.push(rule);
        }
    }

    let mut always_deny = user.permissions.always_deny;
    for rule in project.permissions.always_deny {
        if !always_deny.contains(&rule) {
            always_deny.push(rule);
        }
    }

    Ok(PermissionSettings {
        always_allow,
        always_deny,
    })
}

fn load_one(path: &Path) -> Result<SettingsFile, SettingsError> {
    if !path.exists() {
        return Ok(SettingsFile::default());
    }
    let text = fs::read_to_string(path).map_err(|source| SettingsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| SettingsError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn write(dir: &Path, contents: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("settings.toml"), contents).unwrap();
    }

    #[test]
    fn missing_files_yield_empty_lists() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        let settings = load_settings(user_dir.path(), project_dir.path()).unwrap();
        assert!(settings.always_allow.is_empty());
        assert!(settings.always_deny.is_empty());
    }

    #[test]
    fn unions_rules_from_both_files() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        write(
            user_dir.path(),
            r#"
[permissions]
always_allow = ["cargo test"]
"#,
        );
        write(
            project_dir.path(),
            r#"
[permissions]
always_allow = ["cargo build"]
always_deny = ["rm -rf"]
"#,
        );

        let settings = load_settings(user_dir.path(), project_dir.path()).unwrap();
        assert_eq!(settings.always_allow, vec!["cargo test", "cargo build"]);
        assert_eq!(settings.always_deny, vec!["rm -rf"]);
    }

    #[test]
    fn deduplicates_rule_present_in_both_files() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        write(
            user_dir.path(),
            r#"
[permissions]
always_deny = ["rm -rf"]
"#,
        );
        write(
            project_dir.path(),
            r#"
[permissions]
always_deny = ["rm -rf"]
"#,
        );

        let settings = load_settings(user_dir.path(), project_dir.path()).unwrap();
        assert_eq!(settings.always_deny, vec!["rm -rf"]);
    }
}
