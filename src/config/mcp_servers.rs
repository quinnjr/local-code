// src/config/mcp_servers.rs

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
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

/// Overwrites the project-level `mcp.toml` with exactly `servers`. Mirrors
/// `config::connection::save_connections` exactly: callers (the `/mcp add`
/// wizard, `local-code mcp remove`) load the merged (user+project) list,
/// replace-or-push by name, and pass the whole merged result back in here —
/// so the project file ends up holding a full copy, same convention
/// `connections add`/`remove` already use.
pub fn save_mcp_servers(dir: &Path, servers: &[McpServerConfig]) -> Result<(), McpServersError> {
    fs::create_dir_all(dir).map_err(|source| McpServersError::Read {
        path: dir.to_path_buf(),
        source,
    })?;
    let file = McpServersFile {
        servers: servers.to_vec(),
    };
    let text = toml::to_string_pretty(&file).expect("McpServerConfig serializes without error");
    fs::write(dir.join("mcp.toml"), text).map_err(|source| McpServersError::Read {
        path: dir.to_path_buf(),
        source,
    })
}

static ENV_VAR_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}").unwrap());

/// Replaces every `${VAR_NAME}` occurrence in `value` with that environment
/// variable's value, or an empty string if it's unset — a misconfigured
/// secret then fails at the point of use (e.g. an empty Bearer token gets a
/// 401 from the server) rather than at config-load time.
fn interpolate_env(value: &str) -> String {
    ENV_VAR_PATTERN
        .replace_all(value, |caps: &regex::Captures| std::env::var(&caps[1]).unwrap_or_default())
        .into_owned()
}

/// Applies `interpolate_env` to every string field of a transport config:
/// `command`, each `args` entry, `url`, and every header key/value.
fn interpolate_transport(transport: McpTransportConfig) -> McpTransportConfig {
    match transport {
        McpTransportConfig::Stdio { command, args } => McpTransportConfig::Stdio {
            command: interpolate_env(&command),
            args: args.iter().map(|a| interpolate_env(a)).collect(),
        },
        McpTransportConfig::Http { url, headers } => McpTransportConfig::Http {
            url: interpolate_env(&url),
            headers: headers
                .into_iter()
                .map(|(k, v)| (interpolate_env(&k), interpolate_env(&v)))
                .collect(),
        },
        McpTransportConfig::Websocket { url } => {
            McpTransportConfig::Websocket { url: interpolate_env(&url) }
        }
    }
}

fn load_one(path: &Path) -> Result<McpServersFile, McpServersError> {
    if !path.exists() {
        return Ok(McpServersFile::default());
    }
    let text = fs::read_to_string(path).map_err(|source| McpServersError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut file: McpServersFile = toml::from_str(&text).map_err(|source| McpServersError::Parse {
        path: path.to_path_buf(),
        source,
    })?;
    for server in &mut file.servers {
        server.transport = interpolate_transport(std::mem::replace(
            &mut server.transport,
            McpTransportConfig::Websocket { url: String::new() },
        ));
    }
    Ok(file)
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

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let server = McpServerConfig {
            name: "roundtrip".into(),
            transport: McpTransportConfig::Stdio {
                command: "npx".into(),
                args: vec!["-y".into(), "@modelcontextprotocol/server-filesystem".into()],
            },
        };
        save_mcp_servers(dir.path(), &[server.clone()]).unwrap();
        let loaded = load_mcp_servers(Path::new("/nonexistent"), dir.path()).unwrap();
        assert_eq!(loaded, vec![server]);
    }

    #[test]
    fn save_overwrites_existing_file_with_exactly_the_given_list() {
        let dir = tempdir().unwrap();
        let first = McpServerConfig {
            name: "a".into(),
            transport: McpTransportConfig::Http { url: "http://a".into(), headers: HashMap::new() },
        };
        let second = McpServerConfig {
            name: "b".into(),
            transport: McpTransportConfig::Http { url: "http://b".into(), headers: HashMap::new() },
        };
        save_mcp_servers(dir.path(), &[first]).unwrap();
        save_mcp_servers(dir.path(), &[second.clone()]).unwrap();
        let loaded = load_mcp_servers(Path::new("/nonexistent"), dir.path()).unwrap();
        assert_eq!(loaded, vec![second]);
    }

    #[test]
    fn interpolates_env_var_in_header_value() {
        // SAFETY: test-only, single-threaded within this process's test harness
        // for this specific var name; no other test reads or writes it.
        unsafe { std::env::set_var("MCP_TEST_TOKEN", "secret-abc") };
        let toml_text = r#"
[[server]]
name = "remote-tools"
transport = "http"
url = "http://localhost:9000/mcp"

[server.headers]
Authorization = "Bearer ${MCP_TEST_TOKEN}"
"#;
        let dir = tempdir().unwrap();
        write(dir.path(), toml_text);
        let servers = load_mcp_servers(Path::new("/nonexistent"), dir.path()).unwrap();
        assert_eq!(
            servers[0].transport,
            McpTransportConfig::Http {
                url: "http://localhost:9000/mcp".into(),
                headers: HashMap::from([("Authorization".to_string(), "Bearer secret-abc".to_string())]),
            }
        );
        unsafe { std::env::remove_var("MCP_TEST_TOKEN") };
    }

    #[test]
    fn missing_env_var_interpolates_to_empty_string() {
        unsafe { std::env::remove_var("MCP_TEST_DEFINITELY_UNSET") };
        let toml_text = r#"
[[server]]
name = "remote-tools"
transport = "http"
url = "http://localhost:9000/mcp"

[server.headers]
Authorization = "Bearer ${MCP_TEST_DEFINITELY_UNSET}"
"#;
        let dir = tempdir().unwrap();
        write(dir.path(), toml_text);
        let servers = load_mcp_servers(Path::new("/nonexistent"), dir.path()).unwrap();
        assert_eq!(
            servers[0].transport,
            McpTransportConfig::Http {
                url: "http://localhost:9000/mcp".into(),
                headers: HashMap::from([("Authorization".to_string(), "Bearer ".to_string())]),
            }
        );
    }

    #[test]
    fn interpolates_multiple_vars_in_one_string_and_in_command_and_args() {
        unsafe {
            std::env::set_var("MCP_TEST_BIN", "my-server");
            std::env::set_var("MCP_TEST_ROOT", "/srv/data");
        }
        let toml_text = r#"
[[server]]
name = "fs"
transport = "stdio"
command = "${MCP_TEST_BIN}"
args = ["--root=${MCP_TEST_ROOT}/sub"]
"#;
        let dir = tempdir().unwrap();
        write(dir.path(), toml_text);
        let servers = load_mcp_servers(Path::new("/nonexistent"), dir.path()).unwrap();
        assert_eq!(
            servers[0].transport,
            McpTransportConfig::Stdio {
                command: "my-server".into(),
                args: vec!["--root=/srv/data/sub".into()],
            }
        );
        unsafe {
            std::env::remove_var("MCP_TEST_BIN");
            std::env::remove_var("MCP_TEST_ROOT");
        }
    }
}
