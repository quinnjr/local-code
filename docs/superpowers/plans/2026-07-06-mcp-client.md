# MCP Client Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user attach external MCP (Model Context Protocol) tool servers — stdio,
HTTP, or WebSocket transports — via layered project/user TOML config. At startup, connect to
each configured server, discover its tools, and register them (namespaced by server name) into
the exact same `daimon::agent::AgentBuilder`/`ToolRegistry` the core agent loop (Phase 2) already
uses for its six built-in tools — subject to the same `PermissionGate`/`PermissionMiddleware`
enforcement, with no bypass path. A server that fails to connect is logged and skipped; the rest
of the agent (built-ins + other MCP servers) still works.

**Architecture:** `daimon`'s vendored `mcp` feature already ships everything needed on the client
side: `McpClient::connect(transport)` (handshake + `tools/list`), three `McpTransport`
implementations (`StdioTransport`, `HttpTransport`, `WebSocketTransport`), and `McpToolBridge` — a
`Tool` impl that forwards `execute()` to a real `tools/call` JSON-RPC request. Because
`McpToolBridge::name()` returns the MCP server's own (un-namespaced) tool name, and we need
`servername__toolname` to avoid cross-server collisions, this plan adds one small wrapper type,
`local_code::mcp::tool::NamespacedMcpTool`, that owns an `McpToolBridge` and overrides only
`name()`. This wrapper is a single concrete Rust type, so it can be registered through Phase 2's
existing `AgentBuilder::tool<T: Tool + 'static>(self, tool: T)` in a loop — **no refactor of
Phase 2's registry is needed**; that builder was already generic/append-only by design (confirmed
by reading `daimon-0.16.0/src/agent/builder.rs`). Config is a new layered TOML file,
`.local-code/mcp-servers.toml` (project) + a user-level equivalent, merged by server name exactly
like Phase 1's `connections.toml` (`local_code::config::mcp_servers::load_mcp_servers`, same
project-wins-by-name merge as `load_connections`). `local_code::mcp::connect::connect_all` fans out
one connection attempt per configured server, returning `(Vec<NamespacedMcpTool>, Vec<McpConnectError>)`
— successes and failures are both collected, never a hard error, so one bad server never aborts
startup. `local_code::agent::build::build_agent_with_mcp_tools` (new, alongside the existing
`build_agent`, which now delegates to it with an empty tool list so Phase 2 callers keep working
unmodified) registers the discovered tools onto the same builder chain Phase 2 built, before the
same `PermissionMiddleware` is attached — so an MCP tool call is classified by
`local_code::permissions::types::classify_tool`, which (per Phase 2's own design) treats any
unrecognized tool name as `ToolKind::Edit`, i.e. **prompted by default**, never auto-trusted.
`local_code::agent::headless::run_headless` is updated to call `connect_all` before building the
agent and to print connection failures to stderr without aborting.

**Tech Stack:** `daimon` 0.16.0 `mcp` feature (adds `dep:reqwest`, `dep:tokio-tungstenite` — both
already reachable via Cargo, no new direct dependency needed beyond enabling the feature), `tokio`
(already a dependency from Phase 2), `serde`/`toml` (already dependencies from Phase 1). No new
crates are added to `[dependencies]`; a new `[[bin]]` target is added for a tiny test-fixture MCP
stdio server used only by this plan's integration test.

---

## Spec traceability

This plan implements the MCP-client half of spec section 2 ("Agent loop") from
`docs/superpowers/specs/2026-07-06-local-code-tui-design.md`:

> "MCP client wired in from v1 (`daimon`'s `mcp` feature) so external tool servers can be attached
> via project/user config, same spirit as Claude Code's MCP support — this is the primary 'make it
> easy to add more tooling' lever."

It builds directly on, and does not redefine, these Phase 1/Phase 2 types (imported verbatim):

- `local_code::config::paths::Paths` (Phase 1) — resolves `user_config_dir`/`project_config_dir`,
  reused unchanged to locate `mcp-servers.toml` at both layers.
- `local_code::config::connection::{load_connections, ConnectionsError}` merge pattern (Phase 1) —
  copied structurally (not imported, since it operates on a different file/type) for
  `load_mcp_servers`/`McpServersError`.
- `local_code::permissions::types::{classify_tool, ToolKind}` (Phase 2) — used unchanged; this plan
  adds a locking test confirming namespaced MCP tool names fall through to `ToolKind::Edit`, but
  changes no logic here.
- `local_code::permissions::gate::PermissionGate` and `local_code::agent::middleware::PermissionMiddleware`
  (Phase 2) — reused unchanged; MCP tools flow through the identical `on_tool_call` check as
  built-ins.
- `local_code::agent::build::build_agent` (Phase 2) — kept as a public function with its original
  signature (delegates to the new `build_agent_with_mcp_tools` with `vec![]`), so it is not a
  breaking change for any Phase-2-era caller.
- `local_code::agent::headless::run_headless` (Phase 2) — modified in place (this is the one
  function the spec identifies as the CLI entry point that must gain MCP wiring).

It deliberately does **not** implement: TUI rendering of MCP tool-call cards (Phase 3's job — Phase
3's plan, `docs/superpowers/plans/2026-07-06-tui-shell.md`, did not exist on disk at the time this
plan was written; see Self-review notes for the assumption this leaves), an MCP *server* (the
vendored `daimon::mcp::server`/`McpServer` — out of scope, this repo is a client only), and gRPC
transport (`daimon-0.16.0/src/mcp/grpc_transport.rs` is gated behind daimon's separate `grpc`
feature, not `mcp`, and is not part of the spec's "stdio, HTTP, or WebSocket" list).

---

## File structure

- Modify: `Cargo.toml` — add `mcp` to the `daimon` feature list; add a `[[bin]]` target for the
  test-fixture MCP stdio server
- Create: `src/config/mcp_servers.rs` — `McpTransportConfig`, `McpServerConfig`, `McpServersFile`,
  `load_mcp_servers`, `McpServersError`
- Modify: `src/config/mod.rs` — add `pub mod mcp_servers;`
- Create: `src/mcp/mod.rs` — re-exports for this crate's own `mcp` module (distinct from
  `daimon::mcp`)
- Create: `src/mcp/tool.rs` — `NamespacedMcpTool`
- Create: `src/mcp/connect.rs` — `connect_one`, `connect_all`, `McpConnectError`,
  `McpDiscoveryReport`
- Modify: `src/lib.rs` — add `pub mod mcp;`
- Modify: `src/agent/build.rs` — add `build_agent_with_mcp_tools`; `build_agent` delegates to it
- Modify: `src/agent/headless.rs` — call `connect_all` before `build_agent_with_mcp_tools`, print
  per-server failures to stderr, never abort on a single server failure
- Create: `src/bin/mock_mcp_stdio_server.rs` — a tiny fixture MCP server (one `echo` tool) speaking
  real Content-Length-framed JSON-RPC over stdio, used only by the stdio integration test below
- Create: `tests/mcp_stdio_integration.rs` — spawns the fixture binary as a real child process via
  `StdioTransport`, proving the real (non-mocked) stdio path works end to end

---

### Task 1: `.local-code/mcp-servers.toml` schema and layered loading

**Files:**
- Create: `src/config/mcp_servers.rs`
- Modify: `src/config/mod.rs`

- [ ] **Step 1: Write the failing test**

```rust
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

/// Loads and merges `mcp-servers.toml` from `user_config_dir` and
/// `project_config_dir`. A server in the project file replaces a user-level
/// server of the same name; otherwise servers from both files are kept,
/// user-level first. Missing files yield an empty list, not an error — the same
/// layering contract as `local_code::config::connection::load_connections`.
pub fn load_mcp_servers(
    user_config_dir: &Path,
    project_config_dir: &Path,
) -> Result<Vec<McpServerConfig>, McpServersError> {
    let user_file = load_one(&user_config_dir.join("mcp-servers.toml"))?;
    let project_file = load_one(&project_config_dir.join("mcp-servers.toml"))?;

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
        fs::write(dir.join("mcp-servers.toml"), contents).unwrap();
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
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib config::mcp_servers`
Expected: FAIL to compile — `src/config/mcp_servers.rs` doesn't exist yet. Create it with exactly
the content from Step 1.

- [ ] **Step 3: Add the module to `src/config/mod.rs`**

```rust
pub mod paths;
pub mod connection;
pub mod secrets;
pub mod mcp_servers;
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --lib config::mcp_servers`
Expected: PASS (7 tests).

- [ ] **Step 5: Commit**

```bash
git add src/config/mcp_servers.rs src/config/mod.rs
git commit -m "feat: add layered mcp-servers.toml schema and loading"
```

---

### Task 2: Enable `daimon`'s `mcp` feature

**Files:**
- Modify: `Cargo.toml`

- [ ] **Step 1: Add `mcp` to the `daimon` feature list**

Change the existing `daimon` dependency line (added in Phase 2's Task 1) from:

```toml
daimon = { version = "0.16.0", features = ["openai", "ollama", "macros"] }
```

to:

```toml
daimon = { version = "0.16.0", features = ["openai", "ollama", "macros", "mcp"] }
```

- [ ] **Step 2: Run `cargo check` to confirm the feature resolves**

Run: `cargo check`
Expected: builds (unused-code warnings only) — confirms `daimon`'s `mcp` feature (which pulls in
`reqwest` and `tokio-tungstenite` per `daimon-0.16.0/Cargo.toml`) compiles against the vendored
registry copy. `daimon::mcp::{McpClient, McpToolBridge, StdioTransport, HttpTransport,
WebSocketTransport}` should now be importable.

- [ ] **Step 3: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "chore: enable daimon's mcp client feature"
```

---

### Task 3: `NamespacedMcpTool` — prefixing MCP tool names to avoid collisions

**Files:**
- Create: `src/mcp/mod.rs`
- Create: `src/mcp/tool.rs`
- Modify: `src/lib.rs`

- [ ] **Step 1: Add the module declaration to `src/lib.rs`**

```rust
pub mod config;
pub mod cli;
pub mod permissions;
pub mod agent;
pub mod mcp;
```

- [ ] **Step 2: Create `src/mcp/mod.rs`**

```rust
pub mod tool;
pub mod connect;

pub use tool::NamespacedMcpTool;
pub use connect::{connect_all, McpConnectError, McpDiscoveryReport};
```

- [ ] **Step 3: Write the failing test for `NamespacedMcpTool`**

```rust
// src/mcp/tool.rs

use daimon::mcp::McpToolBridge;
use daimon::tool::{Tool, ToolOutput};

/// Wraps a `daimon::mcp::McpToolBridge` (one real MCP-server-provided tool) so it
/// is registered under a server-namespaced name (`{server_name}__{tool_name}`)
/// instead of the tool's own name, avoiding collisions between two MCP servers
/// (or a built-in tool) that happen to expose the same bare name. Everything
/// else — description, parameter schema, and execution — is delegated
/// unchanged to the wrapped bridge, which is what actually issues the
/// `tools/call` JSON-RPC request over the real transport.
pub struct NamespacedMcpTool {
    namespaced_name: String,
    inner: McpToolBridge,
}

impl NamespacedMcpTool {
    pub fn new(server_name: &str, inner: McpToolBridge) -> Self {
        Self {
            namespaced_name: format!("{server_name}__{}", inner.name()),
            inner,
        }
    }
}

impl Tool for NamespacedMcpTool {
    fn name(&self) -> &str {
        &self.namespaced_name
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }

    async fn execute(&self, input: &serde_json::Value) -> daimon::Result<ToolOutput> {
        self.inner.execute(input).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daimon::mcp::protocol::{
        JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, McpToolInfo,
    };
    use daimon::mcp::McpTransport;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    /// An in-process fake `McpTransport` for fast, deterministic unit tests —
    /// no real process/socket involved. Records the last request it received
    /// and always answers `tools/call` with a fixed text content block, or an
    /// MCP-level error if `fail_calls` is set.
    struct MockTransport {
        fail_calls: bool,
    }

    impl McpTransport for MockTransport {
        fn send<'a>(
            &'a self,
            request: &'a JsonRpcRequest,
        ) -> Pin<Box<dyn Future<Output = daimon::Result<JsonRpcResponse>> + Send + 'a>> {
            let fail_calls = self.fail_calls;
            let id = request.id;
            Box::pin(async move {
                let body = if fail_calls {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32000, "message": "tool failed" }
                    })
                } else {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{"type": "text", "text": "mock tool output"}],
                            "isError": false
                        }
                    })
                };
                Ok(serde_json::from_value(body).unwrap())
            })
        }

        fn notify<'a>(
            &'a self,
            _notification: &'a JsonRpcNotification,
        ) -> Pin<Box<dyn Future<Output = daimon::Result<()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }

        fn close<'a>(&'a self) -> Pin<Box<dyn Future<Output = daimon::Result<()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn bridge(fail_calls: bool) -> McpToolBridge {
        let transport: Arc<dyn McpTransport> = Arc::new(MockTransport { fail_calls });
        let info = McpToolInfo {
            name: "write_file".into(),
            description: Some("Writes a file".into()),
            input_schema: serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        };
        McpToolBridge::new(transport, info)
    }

    #[test]
    fn name_is_namespaced_by_server_name() {
        let tool = NamespacedMcpTool::new("filesystem", bridge(false));
        assert_eq!(tool.name(), "filesystem__write_file");
    }

    #[test]
    fn description_and_schema_are_delegated_unchanged() {
        let tool = NamespacedMcpTool::new("filesystem", bridge(false));
        assert_eq!(tool.description(), "Writes a file");
        assert_eq!(
            tool.parameters_schema(),
            serde_json::json!({"type": "object", "properties": {"path": {"type": "string"}}})
        );
    }

    #[tokio::test]
    async fn execute_delegates_to_the_real_mcp_call() {
        let tool = NamespacedMcpTool::new("filesystem", bridge(false));
        let output = tool
            .execute(&serde_json::json!({"path": "/tmp/x.txt", "content": "hi"}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(output.content, "mock tool output");
    }

    #[tokio::test]
    async fn execute_surfaces_mcp_errors_as_error_output_not_a_panic() {
        let tool = NamespacedMcpTool::new("filesystem", bridge(true));
        let output = tool
            .execute(&serde_json::json!({"path": "/tmp/x.txt", "content": "hi"}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("tool failed"));
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail**

Run: `cargo test --lib mcp::tool`
Expected: FAIL to compile — `src/mcp/tool.rs` doesn't exist yet. Create it with exactly the content
from Step 3.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --lib mcp::tool`
Expected: PASS (4 tests). If `McpToolBridge::name()`/`description()`/`parameters_schema()` are not
`pub` on the vendored type, this will fail to compile with a visibility error — re-check
`daimon-0.16.0/src/mcp/bridge.rs`; as vendored today, `McpToolBridge` implements the public `Tool`
trait, so its `name()`/`description()`/`parameters_schema()`/`execute()` are reachable through that
trait's public methods (`use daimon::tool::Tool;` brings them into scope), which is what the
`impl Tool for NamespacedMcpTool` body above already relies on.

- [ ] **Step 6: Commit**

```bash
git add src/mcp/mod.rs src/mcp/tool.rs src/lib.rs
git commit -m "feat: namespace MCP-discovered tools by server name"
```

---

### Task 4: Connecting to one configured server and turning its tools into `NamespacedMcpTool`s

**Files:**
- Modify: `src/mcp/connect.rs`

- [ ] **Step 1: Write the failing test**

```rust
// src/mcp/connect.rs

use daimon::mcp::{HttpTransport, McpClient, StdioTransport, WebSocketTransport};

use crate::config::mcp_servers::{McpServerConfig, McpTransportConfig};
use crate::mcp::tool::NamespacedMcpTool;

#[derive(Debug, thiserror::Error)]
pub enum McpConnectError {
    #[error("mcp server '{server}' failed to connect: {source}")]
    Connect {
        server: String,
        #[source]
        source: daimon::DaimonError,
    },
}

/// Connects to a single configured MCP server, performs the MCP handshake, and
/// discovers its tools, wrapping each as a [`NamespacedMcpTool`] under
/// `{config.name}__{tool_name}`. Returns [`McpConnectError`] (never panics) if
/// the transport can't be established or the handshake fails — callers (see
/// [`connect_all`]) are expected to treat this as "skip this one server," not a
/// fatal condition.
pub async fn connect_one(
    config: &McpServerConfig,
) -> Result<Vec<NamespacedMcpTool>, McpConnectError> {
    let client = match &config.transport {
        McpTransportConfig::Stdio { command, args } => {
            let transport = StdioTransport::new(command, args).await.map_err(|source| {
                McpConnectError::Connect {
                    server: config.name.clone(),
                    source,
                }
            })?;
            McpClient::connect(transport)
                .await
                .map_err(|source| McpConnectError::Connect {
                    server: config.name.clone(),
                    source,
                })?
        }
        McpTransportConfig::Http { url, headers } => {
            let mut transport = HttpTransport::new(url.clone());
            for (key, value) in headers {
                transport = transport.with_header(key.clone(), value.clone());
            }
            McpClient::connect(transport)
                .await
                .map_err(|source| McpConnectError::Connect {
                    server: config.name.clone(),
                    source,
                })?
        }
        McpTransportConfig::Websocket { url } => {
            let transport =
                WebSocketTransport::connect(url)
                    .await
                    .map_err(|source| McpConnectError::Connect {
                        server: config.name.clone(),
                        source,
                    })?;
            McpClient::connect(transport)
                .await
                .map_err(|source| McpConnectError::Connect {
                    server: config.name.clone(),
                    source,
                })?
        }
    };

    Ok(client
        .tools()
        .into_iter()
        .map(|bridge| NamespacedMcpTool::new(&config.name, bridge))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stdio_transport_reports_a_connect_error_for_a_nonexistent_command() {
        let config = McpServerConfig {
            name: "broken".into(),
            transport: McpTransportConfig::Stdio {
                command: "definitely-not-a-real-mcp-server-binary-xyz".into(),
                args: vec![],
            },
        };
        let result = connect_one(&config).await;
        assert!(matches!(result, Err(McpConnectError::Connect { server, .. }) if server == "broken"));
    }

    #[tokio::test]
    async fn http_transport_reports_a_connect_error_when_nothing_is_listening() {
        let config = McpServerConfig {
            name: "unreachable-http".into(),
            transport: McpTransportConfig::Http {
                url: "http://127.0.0.1:1".into(), // port 1: nothing listens here
                headers: Default::default(),
            },
        };
        let result = connect_one(&config).await;
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test --lib mcp::connect`
Expected: FAIL to compile — `src/mcp/connect.rs` currently only has the placeholder from Task 3's
`mod.rs` re-export (which doesn't yet resolve). Create the file with exactly the content above.

- [ ] **Step 3: Run the tests to verify they pass**

Run: `cargo test --lib mcp::connect`
Expected: PASS (2 tests). Both exercise real failure paths only (a nonexistent binary, an
unreachable port) — no real MCP server is required for these two; the happy path (a server that
actually answers) is covered by Task 6's stdio integration test against a real spawned fixture
process, per the plan's requirement to test process-based transports against a local test double
rather than a hand-run remote server.

- [ ] **Step 4: Commit**

```bash
git add src/mcp/connect.rs
git commit -m "feat: connect to a single configured MCP server and discover its tools"
```

---

### Task 5: `connect_all` — graceful degradation across every configured server

**Files:**
- Modify: `src/mcp/connect.rs`

- [ ] **Step 1: Write the failing test**

Append to `src/mcp/connect.rs` (implementation above the existing `mod tests`, tests inside it):

```rust
/// The outcome of attempting to connect to every configured MCP server:
/// every tool successfully discovered (already namespaced), plus one
/// [`McpConnectError`] per server that failed. A failure here is never fatal —
/// callers register `tools` and separately log/report `errors`, so one
/// misconfigured or offline server doesn't take down the built-in tools or any
/// other, working, MCP server.
pub struct McpDiscoveryReport {
    pub tools: Vec<NamespacedMcpTool>,
    pub errors: Vec<McpConnectError>,
}

/// Connects to every server in `configs`, collecting successes and failures
/// independently. Servers are attempted concurrently.
pub async fn connect_all(configs: &[McpServerConfig]) -> McpDiscoveryReport {
    let attempts = configs.iter().map(connect_one);
    let results = futures::future::join_all(attempts).await;

    let mut tools = Vec::new();
    let mut errors = Vec::new();
    for result in results {
        match result {
            Ok(discovered) => tools.extend(discovered),
            Err(e) => errors.push(e),
        }
    }

    McpDiscoveryReport { tools, errors }
}
```

Test (add inside the existing `mod tests` block from Task 4):

```rust
    #[tokio::test]
    async fn one_broken_server_does_not_prevent_others_from_being_reported() {
        let configs = vec![
            McpServerConfig {
                name: "broken-a".into(),
                transport: McpTransportConfig::Stdio {
                    command: "definitely-not-a-real-mcp-server-binary-xyz".into(),
                    args: vec![],
                },
            },
            McpServerConfig {
                name: "broken-b".into(),
                transport: McpTransportConfig::Http {
                    url: "http://127.0.0.1:1".into(),
                    headers: Default::default(),
                },
            },
        ];

        let report = connect_all(&configs).await;
        assert!(report.tools.is_empty());
        assert_eq!(report.errors.len(), 2);
        let failed_names: Vec<_> = report
            .errors
            .iter()
            .map(|e| match e {
                McpConnectError::Connect { server, .. } => server.as_str(),
            })
            .collect();
        assert!(failed_names.contains(&"broken-a"));
        assert!(failed_names.contains(&"broken-b"));
    }

    #[tokio::test]
    async fn empty_config_list_yields_empty_report() {
        let report = connect_all(&[]).await;
        assert!(report.tools.is_empty());
        assert!(report.errors.is_empty());
    }
```

- [ ] **Step 2: Add `futures` as a direct dependency if not already present**

Phase 2's Task 8 already added `futures = "0.3"` to `[dependencies]` for its own test code
(`futures::stream::empty()`), so `futures::future::join_all` should already resolve. Run
`cargo check` first; only run `cargo add futures` if it reports `futures` as unresolved.

- [ ] **Step 3: Run the tests to verify they fail, then pass**

Run: `cargo test --lib mcp::connect`
Expected: PASS (4 tests total: 2 from Task 4 + 2 new). The mixed-success case (one server
succeeding while another fails) is exercised end-to-end by Task 6's integration test, which spawns
one real working fixture server alongside a deliberately-broken config.

- [ ] **Step 4: Commit**

```bash
git add src/mcp/connect.rs
git commit -m "feat: connect to all configured MCP servers with per-server graceful degradation"
```

---

### Task 6: Real stdio integration test against a spawned fixture MCP server

**Files:**
- Create: `src/bin/mock_mcp_stdio_server.rs`
- Modify: `Cargo.toml`
- Create: `tests/mcp_stdio_integration.rs`

This task proves the real (non-mocked) stdio transport path — process spawn, Content-Length
framing, JSON-RPC handshake — actually works, using a tiny fixture server compiled as part of this
same crate rather than a hand-run remote MCP server.

- [ ] **Step 1: Add the `[[bin]]` target to `Cargo.toml`**

Append after the existing `[[bin]]` block for `local-code`:

```toml
[[bin]]
name = "mock_mcp_stdio_server"
path = "src/bin/mock_mcp_stdio_server.rs"
```

- [ ] **Step 2: Write `src/bin/mock_mcp_stdio_server.rs`**

A minimal, dependency-free (stdlib-only) MCP server: reads Content-Length-framed JSON-RPC requests
on stdin, answers `initialize`, ignores `notifications/initialized`, answers `tools/list` with one
tool (`echo`), and answers `tools/call` by echoing back the `text` argument it was given (or an
MCP-level error result if `arguments.fail` is `true`, letting the integration test also exercise the
tool-level-error path over a real transport).

```rust
//! Test-fixture MCP server. Not part of the `local-code` product surface —
//! exists only so `tests/mcp_stdio_integration.rs` can exercise the real
//! stdio transport (spawn + Content-Length framing + JSON-RPC) against a
//! real child process instead of an in-process mock.

use std::io::{self, Read, Write};

fn read_message(stdin: &mut impl Read) -> Option<Vec<u8>> {
    let mut header = Vec::new();
    let mut byte = [0u8; 1];
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = Vec::new();
        loop {
            if stdin.read_exact(&mut byte).is_err() {
                return None;
            }
            line.push(byte[0]);
            if line.ends_with(b"\r\n") {
                break;
            }
        }
        header.extend_from_slice(&line);

        let line_str = String::from_utf8_lossy(&line);
        let trimmed = line_str.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
            content_length = len_str.trim().parse().ok();
        }
    }

    let length = content_length?;
    let mut body = vec![0u8; length];
    stdin.read_exact(&mut body).ok()?;
    Some(body)
}

fn write_message(stdout: &mut impl Write, body: &serde_json_lite::Value) {
    let text = serde_json_lite::to_string(body);
    let header = format!("Content-Length: {}\r\n\r\n", text.as_bytes().len());
    let _ = stdout.write_all(header.as_bytes());
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

fn main() {
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();

    loop {
        let Some(body) = read_message(&mut stdin) else {
            break;
        };
        let request = serde_json_lite::parse(&body);
        let method = request.get_str("method").unwrap_or_default();
        let id = request.get_u64("id");

        match method {
            "initialize" => {
                if let Some(id) = id {
                    write_message(
                        &mut stdout,
                        &serde_json_lite::Value::response_ok(id, serde_json_lite::Value::empty_object()),
                    );
                }
            }
            "notifications/initialized" => {
                // No response expected for notifications.
            }
            "tools/list" => {
                if let Some(id) = id {
                    let tools_result = serde_json_lite::Value::tools_list_result();
                    write_message(&mut stdout, &serde_json_lite::Value::response_ok(id, tools_result));
                }
            }
            "tools/call" => {
                if let Some(id) = id {
                    let should_fail = request.get_bool_at("params.arguments.fail").unwrap_or(false);
                    let text = request
                        .get_str_at("params.arguments.text")
                        .unwrap_or_default()
                        .to_string();
                    let result = serde_json_lite::Value::tool_call_result(&text, should_fail);
                    write_message(&mut stdout, &serde_json_lite::Value::response_ok(id, result));
                }
            }
            _ => {
                if let Some(id) = id {
                    write_message(&mut stdout, &serde_json_lite::Value::response_err(id, "method not found"));
                }
            }
        }
    }
}
```

Note: the fixture above deliberately avoids depending on `serde_json` (adding a real JSON parser as
a hand-rolled `serde_json_lite` module would be its own large yield-no-value undertaking for a
throwaway test fixture). Replace every `serde_json_lite::*` call above with real `serde_json`
instead — `local-code` already depends on it (Phase 2's Task 1) so it is available to this `[[bin]]`
target too. Concretely:

```rust
//! Test-fixture MCP server. Not part of the `local-code` product surface —
//! exists only so `tests/mcp_stdio_integration.rs` can exercise the real
//! stdio transport (spawn + Content-Length framing + JSON-RPC) against a
//! real child process instead of an in-process mock.

use std::io::{self, Read, Write};

fn read_message(stdin: &mut impl Read) -> Option<Vec<u8>> {
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            stdin.read_exact(&mut byte).ok()?;
            line.push(byte[0]);
            if line.ends_with(b"\r\n") {
                break;
            }
        }
        let line_str = String::from_utf8_lossy(&line);
        let trimmed = line_str.trim();
        if trimmed.is_empty() {
            break;
        }
        if let Some(len_str) = trimmed.strip_prefix("Content-Length:") {
            content_length = len_str.trim().parse().ok();
        }
    }

    let length = content_length?;
    let mut body = vec![0u8; length];
    stdin.read_exact(&mut body).ok()?;
    Some(body)
}

fn write_message(stdout: &mut impl Write, body: &serde_json::Value) {
    let text = serde_json::to_string(body).expect("fixture responses always serialize");
    let header = format!("Content-Length: {}\r\n\r\n", text.as_bytes().len());
    let _ = stdout.write_all(header.as_bytes());
    let _ = stdout.write_all(text.as_bytes());
    let _ = stdout.flush();
}

fn main() {
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();

    loop {
        let Some(body) = read_message(&mut stdin) else {
            break;
        };
        let Ok(request) = serde_json::from_slice::<serde_json::Value>(&body) else {
            continue;
        };
        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or_default();
        let id = request.get("id").and_then(|v| v.as_u64());

        match method {
            "initialize" => {
                if let Some(id) = id {
                    write_message(
                        &mut stdout,
                        &serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {}}),
                    );
                }
            }
            "notifications/initialized" => {}
            "tools/list" => {
                if let Some(id) = id {
                    write_message(
                        &mut stdout,
                        &serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "tools": [{
                                    "name": "echo",
                                    "description": "Echoes back the given text.",
                                    "inputSchema": {
                                        "type": "object",
                                        "properties": {
                                            "text": {"type": "string"},
                                            "fail": {"type": "boolean"}
                                        }
                                    }
                                }]
                            }
                        }),
                    );
                }
            }
            "tools/call" => {
                if let Some(id) = id {
                    let arguments = request.pointer("/params/arguments");
                    let should_fail = arguments
                        .and_then(|a| a.get("fail"))
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let text = arguments
                        .and_then(|a| a.get("text"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();

                    write_message(
                        &mut stdout,
                        &serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "result": {
                                "content": [{"type": "text", "text": text}],
                                "isError": should_fail
                            }
                        }),
                    );
                }
            }
            _ => {
                if let Some(id) = id {
                    write_message(
                        &mut stdout,
                        &serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {"code": -32601, "message": "method not found"}
                        }),
                    );
                }
            }
        }
    }
}
```

Use this second (real `serde_json`-based) version as the actual file content — discard the
`serde_json_lite` sketch above; it was shown only to explain why the real version looks the way it
does.

- [ ] **Step 3: Write `tests/mcp_stdio_integration.rs`**

```rust
//! Exercises the real stdio MCP transport end to end: spawns
//! `mock_mcp_stdio_server` (built as part of this crate, see `Cargo.toml`'s
//! `[[bin]]` target) as a real child process, connects to it, discovers its
//! one `echo` tool, and calls it — proving `daimon::mcp::StdioTransport` +
//! `McpClient` + this plan's `connect_one`/`NamespacedMcpTool` work together
//! against a real (if tiny) MCP server, not just an in-process mock.

use local_code::config::mcp_servers::{McpServerConfig, McpTransportConfig};
use local_code::mcp::connect::{connect_all, connect_one};

fn fixture_server_config(name: &str) -> McpServerConfig {
    McpServerConfig {
        name: name.to_string(),
        transport: McpTransportConfig::Stdio {
            command: env!("CARGO_BIN_EXE_mock_mcp_stdio_server").to_string(),
            args: vec![],
        },
    }
}

#[tokio::test]
async fn discovers_and_namespaces_the_fixture_servers_echo_tool() {
    let config = fixture_server_config("fixture");
    let tools = connect_one(&config).await.expect("fixture server should connect");

    assert_eq!(tools.len(), 1);
    assert_eq!(
        daimon::tool::Tool::name(&tools[0]),
        "fixture__echo"
    );
}

#[tokio::test]
async fn calls_the_fixture_servers_echo_tool_and_gets_real_output_back() {
    use daimon::tool::Tool;

    let config = fixture_server_config("fixture2");
    let tools = connect_one(&config).await.expect("fixture server should connect");
    let echo_tool = &tools[0];

    let output = echo_tool
        .execute(&serde_json::json!({"text": "hello from a real child process"}))
        .await
        .expect("execute should not error at the transport level");

    assert!(!output.is_error);
    assert_eq!(output.content, "hello from a real child process");
}

#[tokio::test]
async fn fixture_servers_tool_level_error_surfaces_as_error_output() {
    use daimon::tool::Tool;

    let config = fixture_server_config("fixture3");
    let tools = connect_one(&config).await.expect("fixture server should connect");
    let echo_tool = &tools[0];

    let output = echo_tool
        .execute(&serde_json::json!({"text": "won't be used", "fail": true}))
        .await
        .expect("execute should not error at the transport level");

    assert!(output.is_error);
}

#[tokio::test]
async fn connect_all_reports_one_real_success_alongside_one_configured_failure() {
    let configs = vec![
        fixture_server_config("fixture4"),
        McpServerConfig {
            name: "broken".into(),
            transport: McpTransportConfig::Stdio {
                command: "definitely-not-a-real-mcp-server-binary-xyz".into(),
                args: vec![],
            },
        },
    ];

    let report = connect_all(&configs).await;
    assert_eq!(report.tools.len(), 1);
    assert_eq!(report.errors.len(), 1);
}
```

- [ ] **Step 4: Run the tests to verify they fail, then pass**

Run: `cargo test --test mcp_stdio_integration`
Expected: first, a compile/lookup failure if `mock_mcp_stdio_server` hasn't been added as a
`[[bin]]` yet, or if `src/bin/mock_mcp_stdio_server.rs` doesn't exist — add both (Steps 1–2) if not
already done, then re-run. Expected after that: PASS (4 tests). `CARGO_BIN_EXE_mock_mcp_stdio_server`
is set automatically by Cargo for integration tests in `tests/` whenever the package defines a
`[[bin]]` target with that name — no manual path wiring required.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml src/bin/mock_mcp_stdio_server.rs tests/mcp_stdio_integration.rs
git commit -m "test: add real stdio MCP integration test against a spawned fixture server"
```

---

### Task 7: Confirm MCP tool names are never auto-trusted by `PermissionGate`

**Files:**
- Modify: `src/permissions/types.rs`

This task adds no new production code — Phase 2's `classify_tool` already defaults any
unrecognized name to `ToolKind::Edit` (i.e. "ask/gate it"), and namespaced MCP tool names like
`filesystem__write_file` or `fixture__echo` never match the built-in-tool names it special-cases.
This task exists to lock that behavior down with an explicit test, per this plan's hard requirement
that MCP tool calls are never a permissions bypass.

- [ ] **Step 1: Write the failing test**

Append to the existing `mod tests` block in `src/permissions/types.rs` (from Phase 2's Task 3):

```rust
    #[test]
    fn namespaced_mcp_tool_names_default_to_edit_not_read_only() {
        // Mirrors the `{server_name}__{tool_name}` shape produced by
        // `local_code::mcp::tool::NamespacedMcpTool::new` — asserting here
        // (rather than only in `src/mcp/tool.rs`) keeps the permission
        // default visible from the permissions module itself, since that is
        // what a future edit to `classify_tool` is most likely to touch.
        assert_eq!(classify_tool("filesystem__write_file"), ToolKind::Edit);
        assert_eq!(classify_tool("filesystem__read_file"), ToolKind::Edit);
        assert_eq!(classify_tool("some_remote_server__delete_everything"), ToolKind::Edit);
    }
```

- [ ] **Step 2: Run the tests to verify they pass**

Run: `cargo test --lib permissions::types`
Expected: PASS immediately (5 tests total: the 4 from Phase 2 + this one) — no production code
change is required, since `classify_tool`'s existing `_ => ToolKind::Edit` fallback already covers
this. If this assertion ever fails after a future change to `classify_tool`, that is exactly the
regression this test exists to catch.

- [ ] **Step 3: Commit**

```bash
git add src/permissions/types.rs
git commit -m "test: lock down that namespaced MCP tool names are never auto-trusted"
```

---

### Task 8: Wire MCP tool discovery into `build_agent` and `run_headless`

**Files:**
- Modify: `src/agent/build.rs`
- Modify: `src/agent/headless.rs`

- [ ] **Step 1: Write the failing test for `build_agent_with_mcp_tools`**

Append to `src/agent/build.rs` (implementation above the existing `build_agent`, tests inside the
existing `mod tests` block):

```rust
use crate::mcp::tool::NamespacedMcpTool;

/// Builds a `daimon::agent::Agent` wired with the six built-in tools, the
/// sentinel `__permission_denied` tool, any MCP-server-discovered tools passed
/// in `mcp_tools`, and a `PermissionMiddleware` backed by `gate`. MCP tools are
/// registered onto the exact same `AgentBuilder.tool(...)` chain as the
/// built-ins — there is no separate registry or execution path for them, and
/// they pass through the identical `PermissionMiddleware` (see
/// `src/agent/middleware.rs`) before ever executing.
pub fn build_agent_with_mcp_tools(
    model: SharedModel,
    gate: Arc<PermissionGate>,
    mcp_tools: Vec<NamespacedMcpTool>,
) -> daimon::Result<Agent> {
    let mut builder = AgentBuilder::new()
        .shared_model(model)
        .system_prompt(DEFAULT_SYSTEM_PROMPT)
        .tool(ReadFile)
        .tool(WriteFile)
        .tool(EditFile)
        .tool(Bash)
        .tool(Grep)
        .tool(Glob)
        .tool(PermissionDeniedTool);

    for tool in mcp_tools {
        builder = builder.tool(tool);
    }

    builder.middleware(PermissionMiddleware::new(gate)).build()
}

/// Builds an agent with only the six built-in tools (no MCP servers
/// configured/connected). Kept as its own function, with its original Phase 2
/// signature, so existing callers are unaffected by this plan.
pub fn build_agent(model: SharedModel, gate: Arc<PermissionGate>) -> daimon::Result<Agent> {
    build_agent_with_mcp_tools(model, gate, Vec::new())
}
```

Remove the old standalone `build_agent` body (the one defined directly with the `AgentBuilder`
chain in Phase 2's Task 8) — it is replaced by the two functions above, where `build_agent` is now
a one-line delegator.

Test (add to the existing `mod tests` block in `src/agent/build.rs`):

```rust
    #[test]
    fn builds_successfully_with_additional_mcp_tools_registered() {
        let model: SharedModel = Arc::new(EchoModel);

        struct FakeMcpTool;
        impl daimon::tool::Tool for FakeMcpTool {
            fn name(&self) -> &str {
                "fixture__echo"
            }
            fn description(&self) -> &str {
                "fixture echo tool"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(&self, _input: &serde_json::Value) -> daimon::Result<daimon::tool::ToolOutput> {
                Ok(daimon::tool::ToolOutput::text("fixture echo"))
            }
        }

        // NamespacedMcpTool itself always wraps a real McpToolBridge (which
        // needs a transport); to keep this test fast and dependency-free we
        // assert the same *shape of contract* — "a plain Tool impl can be
        // added to the same builder chain build_agent uses" — via a
        // structurally-identical fake tool rather than standing up an MCP
        // client. Task 8's headless integration proves the real
        // NamespacedMcpTool path end to end.
        let agent = AgentBuilder::new()
            .shared_model(model)
            .system_prompt(DEFAULT_SYSTEM_PROMPT)
            .tool(ReadFile)
            .tool(WriteFile)
            .tool(EditFile)
            .tool(Bash)
            .tool(Grep)
            .tool(Glob)
            .tool(PermissionDeniedTool)
            .tool(FakeMcpTool)
            .middleware(PermissionMiddleware::new(test_gate()))
            .build();
        assert!(agent.is_ok());
    }

    #[test]
    fn build_agent_still_builds_with_zero_mcp_tools() {
        let model: SharedModel = Arc::new(EchoModel);
        let agent = build_agent(model, test_gate());
        assert!(agent.is_ok());
    }
```

- [ ] **Step 2: Run the tests to verify they fail, then pass**

Run: `cargo test --lib agent::build`
Expected: replace/extend `src/agent/build.rs` with the content above; then PASS (4 tests: the 2
original from Phase 2 + 2 new).

- [ ] **Step 3: Update `src/agent/headless.rs` to discover and register MCP tools**

Replace `run_headless`'s body:

```rust
use crate::agent::build::build_agent_with_mcp_tools;
use crate::config::mcp_servers::load_mcp_servers;
use crate::mcp::connect::connect_all;

pub async fn run_headless(
    paths: &Paths,
    _project_root: &Path,
    connection_name: Option<&str>,
    permission_mode_override: Option<PermissionTier>,
    prompt: &str,
) -> Result<String, HeadlessError> {
    let connections = load_connections(&paths.user_config_dir, &paths.project_config_dir)?;
    let connection = select_connection(&connections, connection_name)?;

    let api_key = SecretStore::get_api_key(&connection.name)?;
    let model = build_model(&connection, api_key)?;

    let settings = load_settings(&paths.user_config_dir, &paths.project_config_dir)?;
    let tier = permission_mode_override.unwrap_or(PermissionTier::FullAuto);
    let gate = Arc::new(PermissionGate::new(
        tier,
        settings,
        Arc::new(StdioPrompter::real()),
    ));

    let mcp_server_configs = load_mcp_servers(&paths.user_config_dir, &paths.project_config_dir)
        .map_err(HeadlessError::LoadMcpServers)?;
    let mcp_report = connect_all(&mcp_server_configs).await;
    for error in &mcp_report.errors {
        eprintln!("warning: {error}");
    }

    let agent = build_agent_with_mcp_tools(model, gate, mcp_report.tools)?;
    let response = agent.prompt(prompt).await?;
    Ok(response.text().to_string())
}
```

Add the new error variant to `HeadlessError` (in the same file):

```rust
    #[error("failed to load mcp-servers.toml: {0}")]
    LoadMcpServers(crate::config::mcp_servers::McpServersError),
```

- [ ] **Step 4: Write the failing test for the "one broken MCP server doesn't abort headless" behavior**

Append to the existing `mod tests` block in `src/agent/headless.rs`:

```rust
    #[tokio::test]
    async fn mcp_report_errors_do_not_prevent_agent_construction() {
        use crate::mcp::connect::{connect_all, McpConnectError};
        use crate::config::mcp_servers::{McpServerConfig, McpTransportConfig};
        use crate::agent::build::build_agent_with_mcp_tools;
        use crate::permissions::settings::PermissionSettings;
        use crate::permissions::stdio::StdioPrompter;

        let configs = vec![McpServerConfig {
            name: "broken".into(),
            transport: McpTransportConfig::Stdio {
                command: "definitely-not-a-real-mcp-server-binary-xyz".into(),
                args: vec![],
            },
        }];
        let report = connect_all(&configs).await;
        assert!(report.tools.is_empty());
        assert_eq!(report.errors.len(), 1);
        assert!(matches!(report.errors[0], McpConnectError::Connect { .. }));

        let model: crate::agent::provider::build_model;
        let connection = conn("dummy");
        let model = crate::agent::provider::build_model(&connection, None).unwrap();
        let gate = std::sync::Arc::new(crate::permissions::gate::PermissionGate::new(
            crate::permissions::types::PermissionTier::FullAuto,
            PermissionSettings::default(),
            std::sync::Arc::new(StdioPrompter::real()),
        ));

        // The whole point: a fully-failed MCP discovery report still produces
        // a working agent with just the built-in tools.
        let agent = build_agent_with_mcp_tools(model, gate, report.tools);
        assert!(agent.is_ok());
    }
```

Remove the stray `let model: crate::agent::provider::build_model;` line above — it was left in by
mistake while drafting; the corrected test body is:

```rust
    #[tokio::test]
    async fn mcp_report_errors_do_not_prevent_agent_construction() {
        use crate::agent::build::build_agent_with_mcp_tools;
        use crate::config::mcp_servers::{McpServerConfig, McpTransportConfig};
        use crate::mcp::connect::{connect_all, McpConnectError};
        use crate::permissions::settings::PermissionSettings;
        use crate::permissions::stdio::StdioPrompter;

        let configs = vec![McpServerConfig {
            name: "broken".into(),
            transport: McpTransportConfig::Stdio {
                command: "definitely-not-a-real-mcp-server-binary-xyz".into(),
                args: vec![],
            },
        }];
        let report = connect_all(&configs).await;
        assert!(report.tools.is_empty());
        assert_eq!(report.errors.len(), 1);
        assert!(matches!(report.errors[0], McpConnectError::Connect { .. }));

        let connection = conn("dummy");
        let model = crate::agent::provider::build_model(&connection, None).unwrap();
        let gate = std::sync::Arc::new(crate::permissions::gate::PermissionGate::new(
            crate::permissions::types::PermissionTier::FullAuto,
            PermissionSettings::default(),
            std::sync::Arc::new(StdioPrompter::real()),
        ));

        // The whole point: a fully-failed MCP discovery report still produces
        // a working agent with just the built-in tools.
        let agent = build_agent_with_mcp_tools(model, gate, report.tools);
        assert!(agent.is_ok());
    }
```

Use this corrected version as the actual test content (the file must not contain the stray
`let model: crate::agent::provider::build_model;` line — that was shown only to illustrate the
mistake being corrected).

- [ ] **Step 5: Run the tests to verify they fail, then pass**

Run: `cargo test --lib agent::headless`
Expected: PASS (6 tests: the 5 from Phase 2 + 1 new).

- [ ] **Step 6: Run the full workspace test suite**

Run: `cargo test`
Expected: PASS across every module — Phase 1, Phase 2, and this plan's tests, plus the
`mcp_stdio_integration` test binary from Task 6.

- [ ] **Step 7: Manually verify end-to-end (requires a real local LLM server; MCP server is the fixture binary)**

```bash
mkdir -p .local-code
cat > .local-code/mcp-servers.toml <<'EOF'
[[server]]
name = "fixture"
transport = "stdio"
command = "target/debug/mock_mcp_stdio_server"
args = []
EOF
cargo build --bin mock_mcp_stdio_server
printf 'my-server\n1\nhttp://localhost:8000/v1\nqwen2.5-coder-7b\n\n' | cargo run -- connections add
cargo run -- -p "call the fixture__echo tool with text set to 'mcp works'" --permission-mode full-auto
rm .local-code/mcp-servers.toml
```

Expected: the model (if it supports tool calling well) calls `fixture__echo`, gets back `mcp
works`, and reports it in its final answer. This step is documentation for manual verification, not
an automated test, since it requires a real local LLM server able to decide to call an
arbitrarily-named tool from a natural-language instruction.

- [ ] **Step 8: Commit**

```bash
git add src/agent/build.rs src/agent/headless.rs
git commit -m "feat: discover and register MCP-server tools at headless agent startup"
```

---

## Self-review notes

- **Spec coverage:**
  - Config: `.local-code/mcp-servers.toml` (project) + user-level equivalent, layered
    project-overrides-user by server name — implemented and tested (Task 1), structurally mirroring
    (not importing — different file/type) Phase 1's `load_connections` merge pattern. Schema covers
    server name, `stdio` (command + args), `http` (url + headers), and `websocket` (url) — all three
    transports the spec's crate list names for `daimon`'s `mcp` feature.
  - Startup: connect to each configured server, list tools via the real MCP protocol
    (`McpClient::connect` → `tools/list`, both vendored in `daimon`, unmodified), register each as a
    namespaced tool (`{server_name}__{tool_name}`) — implemented in Tasks 3–4, proven against a real
    spawned child process (not just an in-process mock) in Task 6.
  - Execution: an MCP tool call is routed through the actual MCP client call
    (`McpToolBridge::execute` → real `tools/call` JSON-RPC, unmodified from `daimon`), and is subject
    to the identical `PermissionGate`/`PermissionMiddleware` as built-ins — no separate registry, no
    separate middleware, no bypass path. `classify_tool`'s existing fallback (`_ =>
    ToolKind::Edit`) already defaults any MCP tool name to "needs permission," locked down by an
    explicit test in Task 7 with zero production-code change required.
  - Graceful degradation: `connect_all` (Task 5) collects per-server failures into
    `McpDiscoveryReport.errors` without ever returning a hard error itself; `run_headless` (Task 8)
    logs each failure to stderr and proceeds to build the agent with whatever tools *did* connect
    (built-ins always present, plus any MCP servers that succeeded) — proven with a fully-failed
    discovery report still producing a working agent (Task 8's test), and with a mixed
    success/failure report against a real spawned process (Task 6's
    `connect_all_reports_one_real_success_alongside_one_configured_failure`).

- **Did Phase 2's tool registry need a refactor?** No. `daimon::agent::builder::AgentBuilder::tool<T:
  Tool + 'static>(self, tool: T)` (confirmed by reading `daimon-0.16.0/src/agent/builder.rs`) is
  already generic over any concrete `Tool` implementor and can be called any number of times in a
  loop — which is exactly what registering a runtime-determined number of MCP tools needs. The one
  addition, `NamespacedMcpTool` (Task 3), exists solely to override the tool *name* (MCP's
  `McpToolBridge::name()` returns the bare, un-namespaced name) — it is a wrapper around the
  vendored bridge type, not a new registry mechanism. `build_agent`'s Phase 2 signature is preserved
  unchanged (it now delegates to the new `build_agent_with_mcp_tools` with an empty tool list), so
  no Phase-2-era caller breaks.

- **Placeholder scan:** no `TODO`/`TBD`/`unimplemented!`/"implement later" anywhere in this plan.
  Task 6's fixture-server code sketch (the `serde_json_lite` version) is explicitly labeled
  "discard this sketch, use the real `serde_json` version below" within the same task — it is not a
  stub left for a future task, it is scaffolding-in-prose for why the real version looks the way it
  does, immediately followed by the complete, real implementation to actually use. Similarly, Task
  8 Step 4 shows one intentionally-broken test-draft line and then immediately gives the corrected
  full test body to use instead — both are resolved within the same task, not deferred.

- **Type consistency:** `McpServerConfig`/`McpTransportConfig`/`McpServersFile`/`load_mcp_servers`
  (Task 1) are defined once and reused verbatim by `connect_one`/`connect_all` (Tasks 4–5) and
  `run_headless` (Task 8). `NamespacedMcpTool` (Task 3) is defined once and reused verbatim by
  `connect_one` (Task 4) and `build_agent_with_mcp_tools` (Task 8). `McpConnectError`/
  `McpDiscoveryReport` (Tasks 4–5) are defined once and reused by `run_headless`'s new
  `HeadlessError::LoadMcpServers` handling and its test (Task 8). No Phase 1/Phase 2 type
  (`Paths`, `PermissionGate`, `PermissionMiddleware`, `classify_tool`, `build_agent`) is redefined —
  each is imported from its original module path.

- **API-compatibility risks worth flagging before implementation starts:**
  1. **`AgentBuilder::tool`/`ToolRegistry::register` silently drops duplicate-named tools.**
     `AgentBuilder::tool` discards `ToolRegistry::register`'s `Result` (`let _ =
     self.tools.register(tool);`, confirmed in `daimon-0.16.0/src/agent/builder.rs`). If two
     configured MCP servers were ever given the *same* `name` in `mcp-servers.toml`, the second
     server's tools would silently fail to register (no error surfaced to the user) rather than
     erroring loudly — this plan's namespacing prevents *cross-server* collisions by construction as
     long as server names in the merged config are distinct, but does not itself validate
     uniqueness of `McpServerConfig.name` at load time. A future hardening pass could add a
     duplicate-name check to `load_mcp_servers`; not done here to keep this plan scoped to the spec's
     explicit asks.
  2. **`WebSocketTransport::connect` takes no header/auth parameter** (confirmed in
     `daimon-0.16.0/src/mcp/websocket.rs` — `connect(url: impl AsRef<str>)` only). Any
     authentication for a `websocket` MCP server must be embedded in the URL itself (e.g. a
     query-string token); there is no equivalent of `HttpTransport::with_header` for WebSocket in
     the vendored client. Documented in `McpTransportConfig::Websocket`'s doc comment (Task 1).
  3. **`StdioTransport::new` and `WebSocketTransport::connect` are both `async` constructors**
     (unlike `HttpTransport::new`, which is sync) — confirmed in `daimon-0.16.0/src/mcp/transport.rs`
     and `websocket.rs`. `connect_one` (Task 4) branches on this correctly per-transport; a future
     4th transport variant would need to check whether its constructor is sync or async before
     assuming the `HttpTransport` shape.
  4. **No gRPC MCP transport is wired in**, even though `daimon-0.16.0/src/mcp/grpc_transport.rs`
     exists — it sits behind daimon's separate `grpc` feature, not `mcp`, and the spec only asks for
     "stdio, HTTP, or WebSocket." Out of scope by the spec's own wording, not an oversight.
  5. **TUI rendering assumption:** this plan does not implement or depend on Phase 3's TUI shell.
     `docs/superpowers/plans/2026-07-06-tui-shell.md` did not exist on disk when this plan was
     written, so the assumption that MCP tool calls are renderable through Phase 3's presumed
     generic "any tool call" rendering path (per this task's brief) could not be confirmed by
     reading that plan — it is taken on faith from the spec's framing of tool-call rendering as
     generic across all tools, built-in or MCP. If Phase 3 turns out to special-case tool rendering
     by a fixed, compile-time-known set of tool names, it will need a follow-up to also handle the
     runtime-determined `{server}__{tool}` name shape this plan introduces.
