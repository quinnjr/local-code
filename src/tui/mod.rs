// src/tui/mod.rs

pub mod app;
pub mod components;
pub mod gated_tool;
pub mod permission_prompter;
pub mod state;

pub use app::{App, AppProps};

use std::path::Path;

use crate::agent::provider::build_model;
use crate::config::connection::{load_connections, Connection};
use crate::config::paths::Paths;
use crate::config::secrets::SecretStore;
use crate::permissions::settings::load_settings;
use crate::permissions::types::PermissionTier;

#[derive(Debug, thiserror::Error)]
pub enum TuiSessionError {
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
    #[error("tui error: {0}")]
    Tui(#[from] ntui::Error),
}

/// Mirrors `local_code::agent::headless::run_headless`'s connection-selection
/// rule (exactly one configured connection, or an explicit `--connection`
/// name) — duplicated rather than shared because headless's `select_connection`
/// is a private fn in `src/agent/headless.rs`; both copies implement the same
/// one-paragraph rule from the spec ("`/model` ... lists all connections") and
/// are simple enough that a shared helper isn't worth the coupling it would
/// add between the headless and TUI entry points.
fn select_connection(
    connections: &[Connection],
    requested_name: Option<&str>,
) -> Result<Connection, TuiSessionError> {
    if let Some(name) = requested_name {
        return connections
            .iter()
            .find(|c| c.name == name)
            .cloned()
            .ok_or_else(|| TuiSessionError::ConnectionNotFound(name.to_string()));
    }
    match connections.len() {
        0 => Err(TuiSessionError::NoConnections),
        1 => Ok(connections[0].clone()),
        _ => Err(TuiSessionError::AmbiguousConnection(
            connections
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        )),
    }
}

/// Launches the interactive TUI: resolves the connection/model/settings
/// (any of which can fail before a single terminal cell is drawn — errors here
/// print a normal CLI error message, never a broken half-drawn screen), then
/// hands off to `ntui::render`. Defaults to `PermissionTier::Ask` (unlike
/// headless mode's `FullAuto` default) since an interactive TUI has a TTY to
/// answer prompts with, matching the spec's "ask (default)" permission tier.
pub async fn run_tui(
    paths: &Paths,
    _project_root: &Path,
    connection_name: Option<&str>,
    permission_mode_override: Option<PermissionTier>,
) -> Result<(), TuiSessionError> {
    let connections = load_connections(&paths.user_config_dir, &paths.project_config_dir)?;
    let connection = select_connection(&connections, connection_name)?;

    let api_key = SecretStore::get_api_key(&connection.name)?;
    let model = build_model(&connection, api_key)?;

    let settings = load_settings(&paths.user_config_dir, &paths.project_config_dir)?;
    let initial_tier = permission_mode_override.unwrap_or(PermissionTier::Ask);

    let props = AppProps {
        model: Some(model),
        connection_name: connection.name.clone(),
        model_name: connection.default_model.clone(),
        always_allow: settings.always_allow,
        always_deny: settings.always_deny,
        initial_tier,
    };

    ntui::render(ntui::element!(App(
        model: props.model,
        connection_name: props.connection_name,
        model_name: props.model_name,
        always_allow: props.always_allow,
        always_deny: props.always_deny,
        initial_tier: props.initial_tier
    )))
    .await?;
    Ok(())
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
        assert!(matches!(result, Err(TuiSessionError::NoConnections)));
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
        assert!(matches!(result, Err(TuiSessionError::AmbiguousConnection(_))));
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
        assert!(
            matches!(result, Err(TuiSessionError::ConnectionNotFound(name)) if name == "does-not-exist")
        );
    }
}
