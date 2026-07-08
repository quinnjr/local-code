// src/agent/headless.rs

use std::path::Path;
use std::sync::Arc;

use crate::agent::build::build_agent_with_mcp_tools;
use crate::agent::provider::build_model;
use crate::config::connection::{load_connections, Connection};
use crate::config::mcp_servers::load_mcp_servers;
use crate::config::paths::Paths;
use crate::config::secrets::SecretStore;
use crate::mcp::connect::connect_all;
use crate::permissions::gate::PermissionGate;
use crate::permissions::settings::load_settings;
use crate::permissions::stdio::StdioPrompter;
use crate::permissions::types::PermissionTier;
use crate::skills::discovery::{discover_skills, render_skill_context, resolve_skill_context};

#[derive(Debug, thiserror::Error)]
pub enum HeadlessError {
    #[error("no connections configured; run `local-code connections add` first")]
    NoConnections,
    #[error("connection '{0}' not found")]
    ConnectionNotFound(String),
    #[error("multiple connections configured ({0}); pass --connection <name> to choose one")]
    AmbiguousConnection(String),
    #[error("failed to load connections: {0}")]
    LoadConnections(#[from] crate::config::connection::ConnectionsError),
    #[error("failed to load settings: {0}")]
    LoadSettings(#[from] crate::permissions::settings::SettingsError),
    #[error("failed to read API key: {0}")]
    Secrets(#[from] crate::config::secrets::SecretsError),
    #[error("failed to construct model: {0}")]
    Provider(#[from] crate::agent::provider::ProviderError),
    #[error("agent error: {0}")]
    Agent(#[from] daimon::DaimonError),
    #[error("failed to load mcp-servers.toml: {0}")]
    LoadMcpServers(crate::config::mcp_servers::McpServersError),
}

fn select_connection(
    connections: &[Connection],
    requested_name: Option<&str>,
) -> Result<Connection, HeadlessError> {
    if let Some(name) = requested_name {
        return connections
            .iter()
            .find(|c| c.name == name)
            .cloned()
            .ok_or_else(|| HeadlessError::ConnectionNotFound(name.to_string()));
    }
    match connections.len() {
        0 => Err(HeadlessError::NoConnections),
        1 => Ok(connections[0].clone()),
        _ => Err(HeadlessError::AmbiguousConnection(
            connections
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        )),
    }
}

/// Runs one full ReAct-loop turn headlessly and returns the final text response.
/// Headless invocations default to `PermissionTier::FullAuto` (there is no TTY to
/// answer an inline prompt); pass `permission_mode_override` to force a different
/// tier (the project/user allow/deny list still applies as a hard boundary
/// regardless of tier).
pub async fn run_headless(
    paths: &Paths,
    project_root: &Path,
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

    let discovered_skills = discover_skills(paths);
    let skill_context = resolve_skill_context(&discovered_skills, project_root);
    let system_context = render_skill_context(&skill_context);

    let agent = build_agent_with_mcp_tools(
        model,
        gate,
        mcp_report.tools,
        discovered_skills,
        &system_context,
    )?;
    let response = agent.prompt(prompt).await?;
    Ok(response.text().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::connection::ProviderKind;

    fn conn(name: &str) -> Connection {
        Connection {
            name: name.to_string(),
            provider: ProviderKind::OpenAiCompatible,
            base_url: "http://localhost:8000/v1".into(),
            default_model: "m".into(),
            models: vec![],
        }
    }

    #[test]
    fn select_connection_errors_when_none_configured() {
        let result = select_connection(&[], None);
        assert!(matches!(result, Err(HeadlessError::NoConnections)));
    }

    #[test]
    fn select_connection_picks_the_only_one_when_unambiguous() {
        let connections = vec![conn("only-one")];
        let result = select_connection(&connections, None).unwrap();
        assert_eq!(result.name, "only-one");
    }

    #[test]
    fn select_connection_errors_when_ambiguous_without_a_name() {
        let connections = vec![conn("a"), conn("b")];
        let result = select_connection(&connections, None);
        assert!(matches!(result, Err(HeadlessError::AmbiguousConnection(_))));
    }

    #[test]
    fn select_connection_finds_by_explicit_name() {
        let connections = vec![conn("a"), conn("b")];
        let result = select_connection(&connections, Some("b")).unwrap();
        assert_eq!(result.name, "b");
    }

    #[test]
    fn select_connection_errors_when_named_connection_missing() {
        let connections = vec![conn("a")];
        let result = select_connection(&connections, Some("does-not-exist"));
        assert!(matches!(result, Err(HeadlessError::ConnectionNotFound(name)) if name == "does-not-exist"));
    }

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
        let agent = build_agent_with_mcp_tools(model, gate, report.tools, Vec::new(), "");
        assert!(agent.is_ok());
    }
}
