use daimon::mcp::{HttpTransport, McpClient, SseTransport, StdioTransport, WebSocketTransport};

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
    #[error("mcp server '{server}' did not finish connecting within {seconds}s; skipping it")]
    Timeout { server: String, seconds: u64 },
}

/// Upper bound on one server's spawn + handshake + `tools/list`. Without it a
/// stdio server that starts but never speaks the protocol blocks forever —
/// daimon's `StdioTransport` reads have no timeout of their own, and
/// `connect_all` runs *before* the first TUI frame renders, so one hung
/// server previously meant the UI never drew at all. HTTP/SSE/WS transports
/// self-bound at 30s inside daimon; this tighter budget also caps their
/// contribution to startup latency.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Connects to a single configured MCP server, performs the MCP handshake, and
/// discovers its tools, wrapping each as a [`NamespacedMcpTool`] under
/// `{config.name}__{tool_name}`. Returns [`McpConnectError`] (never panics) if
/// the transport can't be established, the handshake fails, or the whole
/// attempt exceeds [`CONNECT_TIMEOUT`] — callers (see [`connect_all`]) are
/// expected to treat this as "skip this one server," not a fatal condition.
pub async fn connect_one(
    config: &McpServerConfig,
) -> Result<Vec<NamespacedMcpTool>, McpConnectError> {
    connect_one_with_timeout(config, CONNECT_TIMEOUT).await
}

async fn connect_one_with_timeout(
    config: &McpServerConfig,
    timeout: std::time::Duration,
) -> Result<Vec<NamespacedMcpTool>, McpConnectError> {
    match tokio::time::timeout(timeout, connect_one_inner(config)).await {
        Ok(result) => result,
        Err(_) => Err(McpConnectError::Timeout {
            server: config.name.clone(),
            seconds: timeout.as_secs(),
        }),
    }
}

async fn connect_one_inner(
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
            let transport = WebSocketTransport::connect(url).await.map_err(|source| {
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
        McpTransportConfig::Sse { url, headers } => {
            let transport = SseTransport::connect(url.clone(), headers.clone())
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

/// Closes every distinct MCP server connection behind `tools`, best-effort.
/// Called at app exit so well-behaved stdio servers get an orderly shutdown
/// (children reaped promptly) instead of waiting for process teardown to
/// break their pipes. Tools from the same server share one connection, which
/// is closed exactly once; failures are logged, never propagated — this runs
/// on the way out.
pub async fn close_all(tools: &[NamespacedMcpTool]) {
    use daimon::tool::Tool as _;
    let mut representatives: Vec<&NamespacedMcpTool> = Vec::new();
    for tool in tools {
        if !representatives
            .iter()
            .any(|r| r.shares_connection_with(tool))
        {
            representatives.push(tool);
        }
    }
    for tool in representatives {
        if let Err(e) = tool.close_connection().await {
            eprintln!(
                "warning: failed to close MCP connection behind {}: {e}",
                tool.name()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use daimon::mcp::protocol::{JsonRpcNotification, JsonRpcResponse, McpToolInfo};
    use daimon::mcp::{McpToolBridge, McpTransport};
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        assert!(
            matches!(result, Err(McpConnectError::Connect { server, .. }) if server == "broken")
        );
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

    #[tokio::test]
    async fn sse_transport_reports_a_connect_error_when_nothing_is_listening() {
        let config = McpServerConfig {
            name: "unreachable-sse".into(),
            transport: McpTransportConfig::Sse {
                url: "http://127.0.0.1:1".into(), // port 1: nothing listens here
                headers: Default::default(),
            },
        };
        let result = connect_one(&config).await;
        assert!(
            matches!(result, Err(McpConnectError::Connect { server, .. }) if server == "unreachable-sse")
        );
    }

    #[tokio::test]
    async fn websocket_transport_reports_a_connect_error_when_nothing_is_listening() {
        let config = McpServerConfig {
            name: "unreachable-ws".into(),
            transport: McpTransportConfig::Websocket {
                url: "ws://127.0.0.1:1".into(),
            },
        };
        let result = connect_one(&config).await;
        assert!(
            matches!(result, Err(McpConnectError::Connect { server, .. }) if server == "unreachable-ws")
        );
    }

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
                McpConnectError::Connect { server, .. }
                | McpConnectError::Timeout { server, .. } => server.as_str(),
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

    #[cfg(unix)]
    #[tokio::test]
    async fn stdio_server_that_never_speaks_mcp_times_out_instead_of_hanging() {
        // `sleep` starts fine but never answers the MCP handshake — exactly
        // the shape that used to block `connect_all` (and thus TUI startup)
        // forever.
        let config = McpServerConfig {
            name: "hung".into(),
            transport: McpTransportConfig::Stdio {
                command: "sleep".into(),
                args: vec!["30".into()],
            },
        };
        let result = connect_one_with_timeout(&config, std::time::Duration::from_millis(200)).await;
        assert!(
            matches!(result, Err(McpConnectError::Timeout { server, seconds: 0 }) if server == "hung")
        );
    }

    /// An in-process `McpTransport` that counts `close()` calls (and can be
    /// made to fail them) — the observation point for `close_all`'s
    /// exactly-once and failure-tolerance guarantees without a live server.
    /// `request` is never exercised by these tests, so it simply errors.
    #[derive(Default)]
    struct CountingCloseTransport {
        closes: AtomicUsize,
        fail_close: bool,
    }

    impl McpTransport for CountingCloseTransport {
        fn request<'a>(
            &'a self,
            _method: &'a str,
            _params: Option<serde_json::Value>,
        ) -> Pin<Box<dyn Future<Output = daimon::Result<JsonRpcResponse>> + Send + 'a>> {
            Box::pin(async { Err(daimon::DaimonError::Mcp("counting transport".into())) })
        }

        fn notify<'a>(
            &'a self,
            _notification: &'a JsonRpcNotification,
        ) -> Pin<Box<dyn Future<Output = daimon::Result<()>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }

        fn close<'a>(&'a self) -> Pin<Box<dyn Future<Output = daimon::Result<()>> + Send + 'a>> {
            self.closes.fetch_add(1, Ordering::Relaxed);
            let fail_close = self.fail_close;
            Box::pin(async move {
                if fail_close {
                    Err(daimon::DaimonError::Mcp("close failed".into()))
                } else {
                    Ok(())
                }
            })
        }
    }

    /// Builds a tool over `transport` the way `McpClient::tools()` does:
    /// every tool from one server is a separate bridge over a clone of the
    /// *same* transport `Arc`, which is what makes them compare as sharing
    /// one connection.
    fn tool_on(
        transport: &Arc<CountingCloseTransport>,
        server: &str,
        tool_name: &str,
    ) -> NamespacedMcpTool {
        let erased: Arc<dyn McpTransport> = transport.clone();
        let info = McpToolInfo {
            name: tool_name.into(),
            description: None,
            input_schema: serde_json::json!({"type": "object"}),
        };
        NamespacedMcpTool::new(server, McpToolBridge::new(erased, info))
    }

    #[tokio::test]
    async fn close_all_closes_each_distinct_connection_exactly_once() {
        let alpha = Arc::new(CountingCloseTransport::default());
        let beta = Arc::new(CountingCloseTransport::default());

        // Two tools from server `alpha` (bridges over clones of one
        // transport Arc), a rebuild-style clone of one of them, and one
        // tool from a second server `beta`.
        let alpha_read = tool_on(&alpha, "alpha", "read_file");
        let tools = vec![
            alpha_read.clone(),
            alpha_read,
            tool_on(&alpha, "alpha", "write_file"),
            tool_on(&beta, "beta", "list_things"),
        ];

        close_all(&tools).await;

        assert_eq!(alpha.closes.load(Ordering::Relaxed), 1);
        assert_eq!(beta.closes.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn close_all_continues_past_a_failing_close() {
        let flaky = Arc::new(CountingCloseTransport {
            fail_close: true,
            ..CountingCloseTransport::default()
        });
        let steady = Arc::new(CountingCloseTransport::default());

        // The failing connection comes first: `close_all` must return
        // normally (failures are logged, never propagated) and still
        // attempt the remaining connection.
        let tools = vec![
            tool_on(&flaky, "flaky", "tool_a"),
            tool_on(&steady, "steady", "tool_b"),
        ];

        close_all(&tools).await;

        assert_eq!(flaky.closes.load(Ordering::Relaxed), 1);
        assert_eq!(steady.closes.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn close_all_with_no_tools_is_a_no_op() {
        close_all(&[]).await;
    }
}
