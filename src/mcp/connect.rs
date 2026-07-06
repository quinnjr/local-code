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
