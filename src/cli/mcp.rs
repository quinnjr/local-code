use crate::config::mcp_servers::{load_mcp_servers_raw, save_mcp_servers, McpTransportConfig};
use crate::config::paths::Paths;
use std::io::Write;

pub fn list<W: Write>(paths: &Paths, mut out: W) -> anyhow::Result<()> {
    // Raw (un-interpolated): display shows a `${VAR}` reference as typed
    // rather than resolving and printing the real secret to the screen.
    let servers = load_mcp_servers_raw(&paths.user_config_dir, &paths.project_config_dir)?;
    if servers.is_empty() {
        writeln!(out, "No MCP servers configured. Run `/mcp add` inside the TUI.")?;
        return Ok(());
    }
    for server in &servers {
        let summary = match &server.transport {
            McpTransportConfig::Stdio { command, args } => {
                format!("stdio: {command} {}", args.join(" "))
            }
            McpTransportConfig::Http { url, .. } => format!("http: {url}"),
            McpTransportConfig::Sse { url, .. } => format!("sse: {url}"),
            McpTransportConfig::Websocket { url } => format!("websocket: {url}"),
        };
        writeln!(out, "{}  [{}]", server.name, summary)?;
    }
    Ok(())
}

pub fn remove<W: Write>(paths: &Paths, name: &str, mut out: W) -> anyhow::Result<()> {
    // Raw (un-interpolated): this list gets written straight back to disk
    // below, and resolving ${VAR} here would permanently bake every other
    // server's secret into mcp.toml as a plaintext literal.
    let mut servers = load_mcp_servers_raw(&paths.user_config_dir, &paths.project_config_dir)?;
    let before = servers.len();
    servers.retain(|s| s.name != name);
    if servers.len() == before {
        writeln!(out, "No MCP server named '{name}' found.")?;
        return Ok(());
    }
    save_mcp_servers(&paths.project_config_dir, &servers)?;
    writeln!(out, "Removed MCP server '{name}'.")?;
    Ok(())
}

pub fn add_unsupported<W: Write>(mut out: W) -> anyhow::Result<()> {
    writeln!(
        out,
        "adding an MCP server interactively isn't supported outside the TUI.\n\
         Run `local-code` and use `/mcp add`."
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::mcp_servers::McpServerConfig;
    use std::collections::HashMap;
    use tempfile::tempdir;

    fn test_paths(project_dir: &std::path::Path) -> Paths {
        Paths {
            user_config_dir: project_dir.join("user-config-unused"),
            project_config_dir: project_dir.to_path_buf(),
            user_state_dir: project_dir.join("state-unused"),
        }
    }

    #[test]
    fn list_reports_no_servers_when_empty() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        let mut out = Vec::new();
        list(&paths, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("No MCP servers configured"));
    }

    #[test]
    fn list_prints_each_server_with_a_transport_summary() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        save_mcp_servers(
            &paths.project_config_dir,
            &[McpServerConfig {
                name: "fs".into(),
                transport: McpTransportConfig::Stdio {
                    command: "npx".into(),
                    args: vec!["-y".into(), "@modelcontextprotocol/server-filesystem".into()],
                },
            }],
        )
        .unwrap();

        let mut out = Vec::new();
        list(&paths, &mut out).unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("fs"));
        assert!(printed.contains("npx -y @modelcontextprotocol/server-filesystem"));
    }

    #[test]
    fn list_shows_the_literal_var_reference_not_the_resolved_secret() {
        unsafe { std::env::set_var("MCP_CLI_TEST_TOKEN", "should-not-appear-on-screen") };
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        save_mcp_servers(
            &paths.project_config_dir,
            &[McpServerConfig {
                name: "remote".into(),
                transport: McpTransportConfig::Http {
                    url: "http://x/${MCP_CLI_TEST_TOKEN}".into(),
                    headers: HashMap::new(),
                },
            }],
        )
        .unwrap();

        let mut out = Vec::new();
        list(&paths, &mut out).unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("${MCP_CLI_TEST_TOKEN}"));
        assert!(!printed.contains("should-not-appear-on-screen"));

        unsafe { std::env::remove_var("MCP_CLI_TEST_TOKEN") };
    }

    #[test]
    fn remove_does_not_bake_a_sibling_servers_secret_into_mcp_toml() {
        unsafe { std::env::set_var("MCP_CLI_TEST_SIBLING_TOKEN", "should-never-hit-disk") };
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        save_mcp_servers(
            &paths.project_config_dir,
            &[
                McpServerConfig {
                    name: "gone".into(),
                    transport: McpTransportConfig::Http { url: "http://x".into(), headers: HashMap::new() },
                },
                McpServerConfig {
                    name: "kept".into(),
                    transport: McpTransportConfig::Http {
                        url: "http://y".into(),
                        headers: HashMap::from([(
                            "Authorization".to_string(),
                            "Bearer ${MCP_CLI_TEST_SIBLING_TOKEN}".to_string(),
                        )]),
                    },
                },
            ],
        )
        .unwrap();

        let mut out = Vec::new();
        remove(&paths, "gone", &mut out).unwrap();

        let on_disk = std::fs::read_to_string(paths.project_config_dir.join("mcp.toml")).unwrap();
        assert!(on_disk.contains("${MCP_CLI_TEST_SIBLING_TOKEN}"));
        assert!(!on_disk.contains("should-never-hit-disk"));

        unsafe { std::env::remove_var("MCP_CLI_TEST_SIBLING_TOKEN") };
    }

    #[test]
    fn remove_deletes_matching_server() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        save_mcp_servers(
            &paths.project_config_dir,
            &[McpServerConfig {
                name: "gone".into(),
                transport: McpTransportConfig::Http { url: "http://x".into(), headers: HashMap::new() },
            }],
        )
        .unwrap();

        let mut out = Vec::new();
        remove(&paths, "gone", &mut out).unwrap();

        let remaining = load_mcp_servers_raw(&paths.user_config_dir, &paths.project_config_dir).unwrap();
        assert!(remaining.is_empty());
    }

    #[test]
    fn remove_reports_when_name_not_found() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        let mut out = Vec::new();
        remove(&paths, "does-not-exist", &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("No MCP server named"));
    }

    #[test]
    fn add_unsupported_explains_to_use_the_tui() {
        let mut out = Vec::new();
        add_unsupported(&mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("/mcp add"));
    }
}
