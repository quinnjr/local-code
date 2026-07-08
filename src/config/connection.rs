use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    #[serde(rename = "openai-compatible")]
    OpenAiCompatible,
    Ollama,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub name: String,
    pub provider: ProviderKind,
    pub base_url: String,
    pub default_model: String,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConnectionsFile {
    #[serde(rename = "connection", default)]
    pub connections: Vec<Connection>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOML_FIXTURE: &str = r#"
[[connection]]
name = "local-vllm"
provider = "openai-compatible"
base_url = "http://localhost:8000/v1"
default_model = "qwen2.5-coder-32b"
models = ["qwen2.5-coder-32b", "llama-3.1-70b"]

[[connection]]
name = "home-ollama"
provider = "ollama"
base_url = "http://localhost:11434"
default_model = "llama3.1"
"#;

    #[test]
    fn parses_multiple_connections_from_toml() {
        let file: ConnectionsFile = toml::from_str(TOML_FIXTURE).expect("valid toml");
        assert_eq!(file.connections.len(), 2);
        assert_eq!(file.connections[0].name, "local-vllm");
        assert_eq!(file.connections[0].provider, ProviderKind::OpenAiCompatible);
        assert_eq!(
            file.connections[0].models,
            vec!["qwen2.5-coder-32b", "llama-3.1-70b"]
        );
    }

    #[test]
    fn models_field_defaults_to_empty_when_omitted() {
        let file: ConnectionsFile = toml::from_str(TOML_FIXTURE).expect("valid toml");
        assert_eq!(file.connections[1].name, "home-ollama");
        assert_eq!(file.connections[1].provider, ProviderKind::Ollama);
        assert!(file.connections[1].models.is_empty());
    }

    #[test]
    fn round_trips_through_serialization() {
        let file: ConnectionsFile = toml::from_str(TOML_FIXTURE).expect("valid toml");
        let serialized = toml::to_string(&file).expect("serializes");
        let reparsed: ConnectionsFile = toml::from_str(&serialized).expect("reparses");
        assert_eq!(file, reparsed);
    }
}

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConnectionsError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

/// Loads and merges connections.toml from `user_config_dir` and `project_config_dir`.
/// A connection in the project file replaces a user-level connection of the same name;
/// otherwise entries from both files are kept, user-level first.
pub fn load_connections(
    user_config_dir: &Path,
    project_config_dir: &Path,
) -> Result<Vec<Connection>, ConnectionsError> {
    let user_file = load_one(&user_config_dir.join("connections.toml"))?;
    let project_file = load_one(&project_config_dir.join("connections.toml"))?;

    let mut merged: Vec<Connection> = user_file.connections;
    for project_conn in project_file.connections {
        if let Some(existing) = merged.iter_mut().find(|c| c.name == project_conn.name) {
            *existing = project_conn;
        } else {
            merged.push(project_conn);
        }
    }
    Ok(merged)
}

fn load_one(path: &Path) -> Result<ConnectionsFile, ConnectionsError> {
    if !path.exists() {
        return Ok(ConnectionsFile::default());
    }
    let text = fs::read_to_string(path).map_err(|source| ConnectionsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| ConnectionsError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

/// Overwrites the project-level connections.toml with exactly `connections`.
/// Used by `connections remove` — removal always targets the project-level file
/// since that's the file this CLI writes to (user-level file is hand-edited or
/// written by `connections add` when the user chooses to save it there).
pub fn save_connections(
    dir: &Path,
    connections: &[Connection],
) -> Result<(), ConnectionsError> {
    fs::create_dir_all(dir).map_err(|source| ConnectionsError::Read {
        path: dir.to_path_buf(),
        source,
    })?;
    let file = ConnectionsFile {
        connections: connections.to_vec(),
    };
    let text = toml::to_string_pretty(&file).expect("Connection serializes without error");
    fs::write(dir.join("connections.toml"), text).map_err(|source| ConnectionsError::Read {
        path: dir.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod merge_tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, contents: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("connections.toml"), contents).unwrap();
    }

    #[test]
    fn project_connection_overrides_user_connection_of_same_name() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();

        write(
            user_dir.path(),
            r#"
[[connection]]
name = "shared"
provider = "openai-compatible"
base_url = "http://user-host:8000/v1"
default_model = "model-a"
"#,
        );
        write(
            project_dir.path(),
            r#"
[[connection]]
name = "shared"
provider = "openai-compatible"
base_url = "http://project-host:8000/v1"
default_model = "model-b"
"#,
        );

        let connections = load_connections(user_dir.path(), project_dir.path()).unwrap();
        assert_eq!(connections.len(), 1);
        assert_eq!(connections[0].base_url, "http://project-host:8000/v1");
        assert_eq!(connections[0].default_model, "model-b");
    }

    #[test]
    fn distinct_names_from_both_files_are_kept() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();

        write(
            user_dir.path(),
            r#"
[[connection]]
name = "user-conn"
provider = "openai-compatible"
base_url = "http://a/v1"
default_model = "m"
"#,
        );
        write(
            project_dir.path(),
            r#"
[[connection]]
name = "project-conn"
provider = "ollama"
base_url = "http://b"
default_model = "m2"
"#,
        );

        let connections = load_connections(user_dir.path(), project_dir.path()).unwrap();
        let names: Vec<_> = connections.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec!["user-conn", "project-conn"]);
    }

    #[test]
    fn missing_files_yield_empty_list_not_error() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        let connections = load_connections(user_dir.path(), project_dir.path()).unwrap();
        assert!(connections.is_empty());
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let conn = Connection {
            name: "roundtrip".into(),
            provider: ProviderKind::OpenAiCompatible,
            base_url: "http://localhost:8000/v1".into(),
            default_model: "m".into(),
            models: vec![],
        };
        save_connections(dir.path(), std::slice::from_ref(&conn)).unwrap();
        let loaded = load_connections(Path::new("/nonexistent"), dir.path()).unwrap();
        assert_eq!(loaded, vec![conn]);
    }
}
