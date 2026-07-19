use std::sync::Arc;

use daimon::mcp::McpToolBridge;
use daimon::tool::{Tool, ToolOutput};

/// Wraps a `daimon::mcp::McpToolBridge` (one real MCP-server-provided tool) so it
/// is registered under a server-namespaced name (`{server_name}__{tool_name}`)
/// instead of the tool's own name, avoiding collisions between two MCP servers
/// (or a built-in tool) that happen to expose the same bare name. Everything
/// else — description, parameter schema, and execution — is delegated
/// unchanged to the wrapped bridge, which is what actually issues the
/// `tools/call` JSON-RPC request over the real transport. The bridge is held
/// behind an `Arc` (and `NamespacedMcpTool` derives `Clone`) so the *same*
/// live MCP connection can be registered onto more than one `daimon::agent::Agent`
/// across a TUI agent rebuild (`/model`, `/resume`) without reconnecting to the
/// server every time — see Phase 4's `rebuild_agent`.
#[derive(Clone)]
pub struct NamespacedMcpTool {
    namespaced_name: String,
    inner: Arc<McpToolBridge>,
}

impl NamespacedMcpTool {
    pub fn new(server_name: &str, inner: McpToolBridge) -> Self {
        Self {
            namespaced_name: format!("{server_name}__{}", inner.name()),
            inner: Arc::new(inner),
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

/// Test-only bridge factory shared with sibling modules' tests (e.g.
/// `tui::rebuild`'s duplicate-tool-name failure test): a minimal in-process
/// `McpTransport` that never answers (none of its consumers execute the
/// tool), wrapped in a real `McpToolBridge` carrying the given tool name.
#[cfg(test)]
pub(crate) mod test_support {
    use daimon::mcp::protocol::{JsonRpcNotification, JsonRpcResponse, McpToolInfo};
    use daimon::mcp::{McpToolBridge, McpTransport};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    struct InertTransport;

    impl McpTransport for InertTransport {
        fn request<'a>(
            &'a self,
            _method: &'a str,
            _params: Option<serde_json::Value>,
        ) -> Pin<Box<dyn Future<Output = daimon::Result<JsonRpcResponse>> + Send + 'a>> {
            Box::pin(async {
                Ok(serde_json::from_value(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 0,
                    "result": { "content": [], "isError": false }
                }))
                .unwrap())
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

    pub(crate) fn bridge_named(tool_name: &str) -> McpToolBridge {
        let transport: Arc<dyn McpTransport> = Arc::new(InertTransport);
        let info = McpToolInfo {
            name: tool_name.to_string(),
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
        };
        McpToolBridge::new(transport, info)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daimon::mcp::McpTransport;
    use daimon::mcp::protocol::{JsonRpcNotification, JsonRpcResponse, McpToolInfo};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    /// An in-process fake `McpTransport` for fast, deterministic unit tests —
    /// no real process/socket involved. Always answers `tools/call` with a
    /// fixed text content block, or an MCP-level error if `fail_calls` is
    /// set. Request-id allocation is the transport's job (per
    /// `McpTransport::request`'s contract); these tests never inspect the
    /// response id, so a fixed `0` stands in for whatever a real transport
    /// would allocate.
    struct MockTransport {
        fail_calls: bool,
    }

    impl McpTransport for MockTransport {
        fn request<'a>(
            &'a self,
            _method: &'a str,
            _params: Option<serde_json::Value>,
        ) -> Pin<Box<dyn Future<Output = daimon::Result<JsonRpcResponse>> + Send + 'a>> {
            let fail_calls = self.fail_calls;
            Box::pin(async move {
                let body = if fail_calls {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 0,
                        "error": { "code": -32000, "message": "tool failed" }
                    })
                } else {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": 0,
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
