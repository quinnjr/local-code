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
