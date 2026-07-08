// src/config/mcp_servers.rs

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How to reach an MCP server: the three client transports `daimon`'s vendored
/// `mcp` feature supports (`daimon::mcp::{StdioTransport, HttpTransport, WebSocketTransport}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "transport", rename_all = "kebab-case")]
pub enum McpTransportConfig {
    /// Spawns `command args...` as a child process and speaks Content-Length-framed
    /// JSON-RPC over its stdin/stdout.
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
    /// Sends each JSON-RPC message as an HTTP POST to `url`. `headers` are attached
    /// to every request (e.g. `Authorization = "Bearer <token>"`).
    Http {
        url: String,
        #[serde(default)]
        headers: HashMap<String, String>,
    },
    /// Opens a persistent WebSocket connection to `url` and exchanges JSON-RPC as
    /// text frames. `daimon`'s `WebSocketTransport::connect` takes no headers, so
    /// any required auth must be encoded into `url` itself (e.g. a query-string
    /// token) — see Self-review notes.
    Websocket { url: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    #[serde(flatten)]
    pub transport: McpTransportConfig,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct McpServersFile {
    #[serde(rename = "server", default)]
    pub servers: Vec<McpServerConfig>,
}

#[derive(Debug, thiserror::Error)]
pub enum McpServersError {
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

/// Loads and merges `mcp.toml` from `user_config_dir` and
/// `project_config_dir`. A server in the project file replaces a user-level
/// server of the same name; otherwise servers from both files are kept,
/// user-level first. Missing files yield an empty list, not an error — the same
/// layering contract as `local_code::config::connection::load_connections`.
pub fn load_mcp_servers(
    user_config_dir: &Path,
    project_config_dir: &Path,
) -> Result<Vec<McpServerConfig>, McpServersError> {
    let user_file = load_one(&user_config_dir.join("mcp.toml"))?;
    let project_file = load_one(&project_config_dir.join("mcp.toml"))?;

    let mut merged: Vec<McpServerConfig> = user_file.servers;
    for project_server in project_file.servers {
        if let Some(existing) = merged.iter_mut().find(|s| s.name == project_server.name) {
            *existing = project_server;
        } else {
            merged.push(project_server);
        }
    }
    Ok(merged)
}

fn load_one(path: &Path) -> Result<McpServersFile, McpServersError> {
    if !path.exists() {
        return Ok(McpServersFile::default());
    }
    let text = fs::read_to_string(path).map_err(|source| McpServersError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    toml::from_str(&text).map_err(|source| McpServersError::Parse {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn parses_stdio_transport() {
        let toml_text = r#"
[[server]]
name = "filesystem"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
"#;
        let file: McpServersFile = toml::from_str(toml_text).expect("valid toml");
        assert_eq!(file.servers.len(), 1);
        assert_eq!(file.servers[0].name, "filesystem");
        assert_eq!(
            file.servers[0].transport,
            McpTransportConfig::Stdio {
                command: "npx".into(),
                args: vec![
                    "-y".into(),
                    "@modelcontextprotocol/server-filesystem".into(),
                    "/tmp".into()
                ],
            }
        );
    }

    #[test]
    fn parses_http_transport_with_headers() {
        let toml_text = r#"
[[server]]
name = "remote-tools"
transport = "http"
url = "http://localhost:9000/mcp"

[server.headers]
Authorization = "Bearer abc123"
"#;
        let file: McpServersFile = toml::from_str(toml_text).expect("valid toml");
        assert_eq!(
            file.servers[0].transport,
            McpTransportConfig::Http {
                url: "http://localhost:9000/mcp".into(),
                headers: HashMap::from([("Authorization".to_string(), "Bearer abc123".to_string())]),
            }
        );
    }

    #[test]
    fn parses_websocket_transport() {
        let toml_text = r#"
[[server]]
name = "ws-tools"
transport = "websocket"
url = "ws://localhost:9001/mcp"
"#;
        let file: McpServersFile = toml::from_str(toml_text).expect("valid toml");
        assert_eq!(
            file.servers[0].transport,
            McpTransportConfig::Websocket {
                url: "ws://localhost:9001/mcp".into(),
            }
        );
    }

    #[test]
    fn stdio_args_default_to_empty_when_omitted() {
        let toml_text = r#"
[[server]]
name = "no-args"
transport = "stdio"
command = "some-mcp-server"
"#;
        let file: McpServersFile = toml::from_str(toml_text).expect("valid toml");
        assert_eq!(
            file.servers[0].transport,
            McpTransportConfig::Stdio {
                command: "some-mcp-server".into(),
                args: vec![],
            }
        );
    }

    fn write(dir: &Path, contents: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("mcp.toml"), contents).unwrap();
    }

    #[test]
    fn project_server_overrides_user_server_of_same_name() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();

        write(
            user_dir.path(),
            r#"
[[server]]
name = "shared"
transport = "stdio"
command = "user-command"
"#,
        );
        write(
            project_dir.path(),
            r#"
[[server]]
name = "shared"
transport = "stdio"
command = "project-command"
"#,
        );

        let servers = load_mcp_servers(user_dir.path(), project_dir.path()).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(
            servers[0].transport,
            McpTransportConfig::Stdio {
                command: "project-command".into(),
                args: vec![],
            }
        );
    }

    #[test]
    fn distinct_names_from_both_files_are_kept() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();

        write(
            user_dir.path(),
            r#"
[[server]]
name = "user-server"
transport = "stdio"
command = "a"
"#,
        );
        write(
            project_dir.path(),
            r#"
[[server]]
name = "project-server"
transport = "http"
url = "http://b"
"#,
        );

        let servers = load_mcp_servers(user_dir.path(), project_dir.path()).unwrap();
        let names: Vec<_> = servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["user-server", "project-server"]);
    }

    #[test]
    fn missing_files_yield_empty_list_not_error() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        let servers = load_mcp_servers(user_dir.path(), project_dir.path()).unwrap();
        assert!(servers.is_empty());
    }
}
