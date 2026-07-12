// src/config/mcp_servers.rs

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::secrets::SecretStore;

/// How to reach an MCP server: the four client transports `daimon`'s vendored
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
    /// The MCP "HTTP+SSE" transport: a persistent `GET` receives server
    /// responses/notifications as SSE frames, while requests are sent via
    /// separate `POST`s. `headers` are attached to both. Requires
    /// `daimon::mcp::SseTransport` (added in daimon 0.19.0) — see
    /// `src/mcp/connect.rs`.
    Sse {
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
    #[error("failed to write {path}: {source}")]
    Write {
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
/// `project_config_dir`, resolving `${VAR}` and `${keyring:<name>}` references along the way. A
/// server in the project file replaces a user-level server of the same
/// name; otherwise servers from both files are kept, user-level first.
/// Missing files yield an empty list, not an error — the same layering
/// contract as `local_code::config::connection::load_connections`.
///
/// Use this for callers that need real, connectable config (headless/TUI
/// startup, `connect_one`). Callers that will write the result back to disk
/// (`/mcp add`, `mcp remove`, `mcp list`) must use [`load_mcp_servers_raw`]
/// instead — interpolating here and saving the result would permanently
/// bake resolved secrets into `mcp.toml`, defeating the point of `${VAR}`.
pub fn load_mcp_servers(
    user_config_dir: &Path,
    project_config_dir: &Path,
) -> Result<Vec<McpServerConfig>, McpServersError> {
    let mut servers = load_mcp_servers_raw(user_config_dir, project_config_dir)?;
    for server in &mut servers {
        server.transport = interpolate_transport(std::mem::replace(
            &mut server.transport,
            McpTransportConfig::Websocket { url: String::new() },
        ));
    }
    Ok(servers)
}

/// Like [`load_mcp_servers`] but does not resolve `${VAR}` references —
/// `${VAR}` placeholders are returned as literal text. For callers that will
/// pass the result to [`save_mcp_servers`] (read-modify-write flows), so a
/// secret referenced via `${VAR}` is never resolved and re-persisted as a
/// plaintext literal.
pub fn load_mcp_servers_raw(
    user_config_dir: &Path,
    project_config_dir: &Path,
) -> Result<Vec<McpServerConfig>, McpServersError> {
    let user_file = load_one_with_legacy_fallback(user_config_dir)?;
    let project_file = load_one_with_legacy_fallback(project_config_dir)?;

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
/// wizard, `local-code mcp remove`) load the merged (user+project) list via
/// [`load_mcp_servers_raw`] (never [`load_mcp_servers`] — see its doc
/// comment), replace-or-push by name, and pass the whole merged result back
/// in here — so the project file ends up holding a full copy, same
/// convention `connections add`/`remove` already use.
pub fn save_mcp_servers(dir: &Path, servers: &[McpServerConfig]) -> Result<(), McpServersError> {
    fs::create_dir_all(dir).map_err(|source| McpServersError::Write {
        path: dir.to_path_buf(),
        source,
    })?;
    let file = McpServersFile {
        servers: servers.to_vec(),
    };
    let text = toml::to_string_pretty(&file).expect("McpServerConfig serializes without error");
    let path = dir.join("mcp.toml");
    fs::write(&path, text).map_err(|source| McpServersError::Write { path, source })
}

/// One combined pattern for both reference forms, resolved in a single pass
/// so a substituted value is never itself re-scanned for references:
/// capture 1 = `${keyring:<name>}` (secret-name charset), capture 2 =
/// `${VAR_NAME}` (env-var charset, unchanged from the original pattern).
static REF_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{(?:keyring:([A-Za-z0-9_-]+)|([A-Za-z_][A-Za-z0-9_]*))\}").unwrap()
});

/// Replaces every `${keyring:<name>}` occurrence with that secret from the OS
/// keyring, and every `${VAR_NAME}` occurrence with that environment
/// variable's value. A missing secret/variable (or a keyring backend error)
/// becomes an empty string — a misconfigured secret then fails at the point
/// of use (e.g. an empty Bearer token gets a 401 from the server) rather
/// than at config-load time.
fn interpolate_refs(value: &str) -> String {
    REF_PATTERN
        .replace_all(value, |caps: &regex::Captures| {
            if let Some(name) = caps.get(1) {
                SecretStore::get_secret(name.as_str())
                    .ok()
                    .flatten()
                    .unwrap_or_default()
            } else {
                std::env::var(&caps[2]).unwrap_or_default()
            }
        })
        .into_owned()
}

/// Resolves `${VAR}` references in a single, already-in-hand server config —
/// for a caller (the `/mcp add` wizard's live-connect step) that built the
/// config directly from user input rather than loading it from disk via
/// [`load_mcp_servers`], but still needs the same env resolution before
/// attempting to connect.
pub fn resolve_server_env(server: McpServerConfig) -> McpServerConfig {
    McpServerConfig {
        name: server.name,
        transport: interpolate_transport(server.transport),
    }
}

/// Applies `interpolate_refs` to every string field of a transport config:
/// `command`, each `args` entry, `url`, and every header key/value, resolving
/// `${VAR}` and `${keyring:<name>}` references.
fn interpolate_transport(transport: McpTransportConfig) -> McpTransportConfig {
    match transport {
        McpTransportConfig::Stdio { command, args } => McpTransportConfig::Stdio {
            command: interpolate_refs(&command),
            args: args.iter().map(|a| interpolate_refs(a)).collect(),
        },
        McpTransportConfig::Http { url, headers } => McpTransportConfig::Http {
            url: interpolate_refs(&url),
            headers: headers
                .into_iter()
                .map(|(k, v)| (interpolate_refs(&k), interpolate_refs(&v)))
                .collect(),
        },
        McpTransportConfig::Sse { url, headers } => McpTransportConfig::Sse {
            url: interpolate_refs(&url),
            headers: headers
                .into_iter()
                .map(|(k, v)| (interpolate_refs(&k), interpolate_refs(&v)))
                .collect(),
        },
        McpTransportConfig::Websocket { url } => McpTransportConfig::Websocket {
            url: interpolate_refs(&url),
        },
    }
}

/// Reads `mcp.toml` out of `dir`, falling back to the pre-rename
/// `mcp-servers.toml` if `mcp.toml` doesn't exist there — without this, a
/// user upgrading from a version that used the old filename would have
/// every configured MCP server silently vanish (a missing file loads as
/// "no servers configured", not an error). Does not migrate the file on
/// disk; the next [`save_mcp_servers`] call for that directory naturally
/// writes it out under the new name.
fn load_one_with_legacy_fallback(dir: &Path) -> Result<McpServersFile, McpServersError> {
    let current = dir.join("mcp.toml");
    if current.exists() {
        return load_one(&current);
    }
    let legacy = dir.join("mcp-servers.toml");
    if legacy.exists() {
        return load_one(&legacy);
    }
    Ok(McpServersFile::default())
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
    use crate::config::secrets::SecretStore;
    use std::sync::Once;
    use tempfile::tempdir;

    static KEYRING_INIT: Once = Once::new();
    fn use_mock_keyring() {
        KEYRING_INIT.call_once(|| {
            keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        });
    }

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
                headers: HashMap::from([(
                    "Authorization".to_string(),
                    "Bearer abc123".to_string()
                )]),
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
    fn falls_back_to_legacy_mcp_servers_toml_when_mcp_toml_is_absent() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        fs::create_dir_all(project_dir.path()).unwrap();
        fs::write(
            project_dir.path().join("mcp-servers.toml"),
            r#"
[[server]]
name = "legacy"
transport = "stdio"
command = "old-binary"
"#,
        )
        .unwrap();

        let servers = load_mcp_servers(user_dir.path(), project_dir.path()).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "legacy");
    }

    #[test]
    fn mcp_toml_takes_precedence_over_legacy_filename_when_both_exist() {
        let user_dir = tempdir().unwrap();
        let project_dir = tempdir().unwrap();
        fs::create_dir_all(project_dir.path()).unwrap();
        fs::write(
            project_dir.path().join("mcp-servers.toml"),
            r#"
[[server]]
name = "legacy"
transport = "stdio"
command = "old-binary"
"#,
        )
        .unwrap();
        write(
            project_dir.path(),
            r#"
[[server]]
name = "current"
transport = "stdio"
command = "new-binary"
"#,
        );

        let servers = load_mcp_servers(user_dir.path(), project_dir.path()).unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "current");
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempdir().unwrap();
        let server = McpServerConfig {
            name: "roundtrip".into(),
            transport: McpTransportConfig::Stdio {
                command: "npx".into(),
                args: vec![
                    "-y".into(),
                    "@modelcontextprotocol/server-filesystem".into(),
                ],
            },
        };
        save_mcp_servers(dir.path(), std::slice::from_ref(&server)).unwrap();
        let loaded = load_mcp_servers(Path::new("/nonexistent"), dir.path()).unwrap();
        assert_eq!(loaded, vec![server]);
    }

    #[test]
    fn save_overwrites_existing_file_with_exactly_the_given_list() {
        let dir = tempdir().unwrap();
        let first = McpServerConfig {
            name: "a".into(),
            transport: McpTransportConfig::Http {
                url: "http://a".into(),
                headers: HashMap::new(),
            },
        };
        let second = McpServerConfig {
            name: "b".into(),
            transport: McpTransportConfig::Http {
                url: "http://b".into(),
                headers: HashMap::new(),
            },
        };
        save_mcp_servers(dir.path(), &[first]).unwrap();
        save_mcp_servers(dir.path(), std::slice::from_ref(&second)).unwrap();
        let loaded = load_mcp_servers(Path::new("/nonexistent"), dir.path()).unwrap();
        assert_eq!(loaded, vec![second]);
    }

    #[test]
    fn save_then_load_round_trips_sse_and_websocket() {
        let dir = tempdir().unwrap();
        let servers = vec![
            McpServerConfig {
                name: "sse-rt".into(),
                transport: McpTransportConfig::Sse {
                    url: "http://localhost:9002/sse".into(),
                    headers: HashMap::from([(
                        "Authorization".to_string(),
                        "Bearer abc123".to_string(),
                    )]),
                },
            },
            McpServerConfig {
                name: "ws-rt".into(),
                transport: McpTransportConfig::Websocket {
                    url: "ws://localhost:9001/mcp".into(),
                },
            },
        ];
        save_mcp_servers(dir.path(), &servers).unwrap();
        let loaded = load_mcp_servers(Path::new("/nonexistent"), dir.path()).unwrap();
        assert_eq!(loaded, servers);
    }

    #[test]
    fn load_mcp_servers_raw_does_not_resolve_env_vars() {
        // Proves the fix for a real bug: `remove`/`/mcp add` used to load
        // via the *interpolating* loader before saving, which permanently
        // baked resolved secrets into mcp.toml. `load_mcp_servers_raw` must
        // return the literal `${VAR}` text unchanged so read-modify-write
        // flows never do that.
        unsafe { std::env::set_var("MCP_TEST_RAW_TOKEN", "secret-should-not-be-baked-in") };
        let toml_text = r#"
[[server]]
name = "remote-tools"
transport = "http"
url = "http://localhost:9000/mcp"

[server.headers]
Authorization = "Bearer ${MCP_TEST_RAW_TOKEN}"
"#;
        let dir = tempdir().unwrap();
        write(dir.path(), toml_text);

        let raw = load_mcp_servers_raw(Path::new("/nonexistent"), dir.path()).unwrap();
        assert_eq!(
            raw[0].transport,
            McpTransportConfig::Http {
                url: "http://localhost:9000/mcp".into(),
                headers: HashMap::from([(
                    "Authorization".to_string(),
                    "Bearer ${MCP_TEST_RAW_TOKEN}".to_string()
                )]),
            }
        );

        // Sanity check: the interpolating loader still resolves it, so the
        // distinction is real and intentional, not load_one being broken.
        let resolved = load_mcp_servers(Path::new("/nonexistent"), dir.path()).unwrap();
        assert_eq!(
            resolved[0].transport,
            McpTransportConfig::Http {
                url: "http://localhost:9000/mcp".into(),
                headers: HashMap::from([(
                    "Authorization".to_string(),
                    "Bearer secret-should-not-be-baked-in".to_string()
                )]),
            }
        );

        unsafe { std::env::remove_var("MCP_TEST_RAW_TOKEN") };
    }

    #[test]
    fn saving_a_raw_load_never_bakes_in_resolved_secrets() {
        // End-to-end version of the fix: load raw, mutate, save — the
        // ${VAR} reference must survive on disk, not get replaced with the
        // resolved literal.
        unsafe { std::env::set_var("MCP_TEST_E2E_TOKEN", "should-never-hit-disk") };
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            r#"
[[server]]
name = "kept"
transport = "http"
url = "http://localhost:9000/mcp"

[server.headers]
Authorization = "Bearer ${MCP_TEST_E2E_TOKEN}"
"#,
        );

        let mut servers = load_mcp_servers_raw(Path::new("/nonexistent"), dir.path()).unwrap();
        servers.push(McpServerConfig {
            name: "added".into(),
            transport: McpTransportConfig::Http {
                url: "http://other".into(),
                headers: HashMap::new(),
            },
        });
        save_mcp_servers(dir.path(), &servers).unwrap();

        let on_disk = fs::read_to_string(dir.path().join("mcp.toml")).unwrap();
        assert!(
            on_disk.contains("${MCP_TEST_E2E_TOKEN}"),
            "expected the literal ${{VAR}} reference to survive the round trip, got:\n{on_disk}"
        );
        assert!(!on_disk.contains("should-never-hit-disk"));

        unsafe { std::env::remove_var("MCP_TEST_E2E_TOKEN") };
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
                headers: HashMap::from([(
                    "Authorization".to_string(),
                    "Bearer secret-abc".to_string()
                )]),
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

    #[test]
    fn parses_sse_transport_with_headers() {
        let toml_text = r#"
[[server]]
name = "sse-tools"
transport = "sse"
url = "http://localhost:9002/sse"

[server.headers]
Authorization = "Bearer abc123"
"#;
        let file: McpServersFile = toml::from_str(toml_text).expect("valid toml");
        assert_eq!(
            file.servers[0].transport,
            McpTransportConfig::Sse {
                url: "http://localhost:9002/sse".into(),
                headers: HashMap::from([(
                    "Authorization".to_string(),
                    "Bearer abc123".to_string()
                )]),
            }
        );
    }

    #[test]
    fn sse_headers_default_to_empty_when_omitted() {
        let toml_text = r#"
[[server]]
name = "sse-tools"
transport = "sse"
url = "http://localhost:9002/sse"
"#;
        let file: McpServersFile = toml::from_str(toml_text).expect("valid toml");
        assert_eq!(
            file.servers[0].transport,
            McpTransportConfig::Sse {
                url: "http://localhost:9002/sse".into(),
                headers: HashMap::new()
            }
        );
    }

    #[test]
    fn keyring_reference_resolves_from_the_secret_store() {
        use_mock_keyring();
        SecretStore::set_secret("mcp-github", "tok-abc").unwrap();
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("mcp.toml"),
            r#"
[[server]]
name = "github"
transport = "http"
url = "http://localhost:9000/mcp"

[server.headers]
Authorization = "Bearer ${keyring:mcp-github}"
"#,
        )
        .unwrap();
        let servers = load_mcp_servers(dir.path(), std::path::Path::new("/nonexistent")).unwrap();
        let McpTransportConfig::Http { headers, .. } = &servers[0].transport else {
            panic!("expected Http transport");
        };
        assert_eq!(headers["Authorization"], "Bearer tok-abc");
    }

    #[test]
    fn missing_keyring_reference_resolves_to_empty_string() {
        use_mock_keyring();
        let resolved = super::interpolate_refs("Bearer ${keyring:not-stored-anywhere}");
        assert_eq!(resolved, "Bearer ");
    }

    #[test]
    fn keyring_and_env_references_coexist_in_one_value() {
        use_mock_keyring();
        SecretStore::set_secret("mix-secret", "S").unwrap();
        // SAFETY-of-test: set_var is fine in a test that owns this var name.
        unsafe { std::env::set_var("MCP_MIX_ENV_VAR", "E") };
        let resolved = super::interpolate_refs("${MCP_MIX_ENV_VAR}/${keyring:mix-secret}");
        assert_eq!(resolved, "E/S");
    }

    #[test]
    fn raw_load_keeps_keyring_references_literal() {
        use_mock_keyring();
        SecretStore::set_secret("mcp-raw", "should-not-appear").unwrap();
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("mcp.toml"),
            r#"
[[server]]
name = "raw"
transport = "http"
url = "http://localhost:9000/mcp"

[server.headers]
Authorization = "Bearer ${keyring:mcp-raw}"
"#,
        )
        .unwrap();
        let servers =
            load_mcp_servers_raw(dir.path(), std::path::Path::new("/nonexistent")).unwrap();
        let McpTransportConfig::Http { headers, .. } = &servers[0].transport else {
            panic!("expected Http transport");
        };
        assert_eq!(headers["Authorization"], "Bearer ${keyring:mcp-raw}");
    }
}
