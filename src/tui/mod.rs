// src/tui/mod.rs

pub mod app;
pub mod components;
pub mod gated_tool;
pub mod mcp_wizard;
pub mod memory_seed;
pub mod permission_prompter;
pub mod rebuild;
pub mod slash;
pub mod state;
pub mod workspace;

pub use app::{App, AppProps};

use std::path::Path;

use crate::agent::provider::build_model;
use crate::config::connection::{Connection, load_connections};
use crate::config::mcp_servers::load_mcp_servers;
use crate::config::paths::Paths;
use crate::config::secrets::SecretStore;
use crate::context::load_project_context;
use crate::mcp::connect::connect_all;
use crate::permissions::settings::load_settings;
use crate::permissions::types::PermissionTier;
use crate::session::paths::new_session_path;
use crate::session::types::SessionFile;
use crate::skills::discovery::{discover_skills, render_skill_context, resolve_skill_context};
use daimon::model::types::Message;

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
    #[error("failed to persist session: {0}")]
    Session(#[from] crate::session::store::SessionError),
    #[error("failed to load mcp.toml: {0}")]
    LoadMcpServers(crate::config::mcp_servers::McpServersError),
}

/// The subset of a loaded `SessionFile` `run_tui` needs to seed a resumed
/// session — the file's own `path` is threaded through separately so the
/// resumed session keeps appending to the same file rather than starting a
/// new one.
pub struct ResumedSession {
    pub session_path: std::path::PathBuf,
    pub entries: Vec<crate::tui::state::TranscriptEntry>,
    pub messages: Vec<Message>,
    pub tier: PermissionTier,
    /// The connection/model the session was originally created under
    /// (`SessionFile::connection_name`/`model_name`). Used by `run_tui` to
    /// pick the *same* connection the transcript was generated against,
    /// mirroring the in-TUI `/resume` command (`src/tui/app.rs`), instead of
    /// falling back to `--connection`/the single-connection default.
    pub connection_name: String,
    pub model_name: String,
    /// The session's original creation timestamp (`SessionFile::created_at`),
    /// carried through so `run_tui` can seed `App`'s `created_at` state
    /// without re-reading the session file it already loaded here.
    pub created_at: String,
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

/// Resolves which connection/model `run_tui` should build the model against
/// when resuming a previous session. A resumed session's own
/// `connection_name`/`model_name` (the connection it was actually created
/// under) always wins over `--connection`/the single-connection default —
/// mirroring the in-TUI `/resume` command (`src/tui/app.rs`), and matching
/// the plan's stated expectation that resuming reproduces "the same
/// connection/model as before". A session's transcript is tied to a specific
/// provider/model; silently replaying it against a different one (e.g.
/// because `--connection` was also passed) risks confusing or broken
/// behavior, so the explicit flag is intentionally ignored in this case
/// rather than preferred.
///
/// Returns the resolved `Connection` with `default_model` overridden to the
/// resumed session's `model_name`, or `ConnectionNotFound` if that connection
/// no longer exists in config (e.g. it was removed since the session was
/// created).
fn resolve_connection_for_resume(
    connections: &[Connection],
    resumed_connection_name: &str,
    resumed_model_name: &str,
) -> Result<Connection, TuiSessionError> {
    let mut connection = connections
        .iter()
        .find(|c| c.name == resumed_connection_name)
        .cloned()
        .ok_or_else(|| TuiSessionError::ConnectionNotFound(resumed_connection_name.to_string()))?;
    connection.default_model = resumed_model_name.to_string();
    Ok(connection)
}

/// Launches the interactive TUI: resolves the connection/model/settings
/// (any of which can fail before a single terminal cell is drawn — errors here
/// print a normal CLI error message, never a broken half-drawn screen), then
/// hands off to `ntui::render`. Defaults to `PermissionTier::Ask` (unlike
/// headless mode's `FullAuto` default) since an interactive TUI has a TTY to
/// answer prompts with, matching the spec's "ask (default)" permission tier.
pub async fn run_tui(
    paths: &Paths,
    project_root: &Path,
    connection_name: Option<&str>,
    permission_mode_override: Option<PermissionTier>,
    resume: Option<ResumedSession>,
) -> Result<(), TuiSessionError> {
    let connections = load_connections(&paths.user_config_dir, &paths.project_config_dir)?;
    let connection = match &resume {
        Some(resumed) => resolve_connection_for_resume(
            &connections,
            &resumed.connection_name,
            &resumed.model_name,
        )?,
        None => select_connection(&connections, connection_name)?,
    };

    let api_key = SecretStore::get_api_key(&connection.name)?;
    let model = build_model(&connection, api_key)?;

    let settings = load_settings(&paths.user_config_dir, &paths.project_config_dir)?;
    let system_context = load_project_context(paths, project_root);
    let discovered_skills = discover_skills(paths);
    let skill_context = resolve_skill_context(&discovered_skills, project_root);
    let rendered_skill_context = render_skill_context(&skill_context);
    let system_context = if rendered_skill_context.is_empty() {
        system_context
    } else if system_context.is_empty() {
        rendered_skill_context
    } else {
        format!("{system_context}\n\n{rendered_skill_context}")
    };

    // Discover MCP-server tools once at startup, exactly like run_headless
    // (Phase 5) does — a broken server is logged and skipped, never fatal,
    // and the resulting tools are threaded through every later agent rebuild
    // (`/model`, `/resume`) via `AppProps::mcp_tools` so they're never only
    // present in headless mode.
    let mcp_server_configs = load_mcp_servers(&paths.user_config_dir, &paths.project_config_dir)
        .map_err(TuiSessionError::LoadMcpServers)?;
    let mcp_report = connect_all(&mcp_server_configs).await;
    for error in &mcp_report.errors {
        eprintln!("warning: {error}");
    }
    let mcp_tools = mcp_report.tools;

    let (initial_tier, initial_entries, initial_messages, session_path, created_at) = match resume {
        Some(resumed) => (
            permission_mode_override.unwrap_or(resumed.tier),
            resumed.entries,
            resumed.messages,
            resumed.session_path,
            resumed.created_at,
        ),
        None => {
            let now = chrono::Utc::now();
            let path = new_session_path(&paths.user_state_dir, project_root, now);
            let tier = permission_mode_override.unwrap_or(PermissionTier::Ask);
            let created_at = now.to_rfc3339();
            let session = SessionFile::new(
                project_root.to_path_buf(),
                connection.name.clone(),
                connection.default_model.clone(),
                tier,
                created_at.clone(),
            );
            crate::session::store::save_session(&path, &session)
                .map_err(TuiSessionError::Session)?;
            (tier, Vec::new(), Vec::new(), path, created_at)
        }
    };

    let props = AppProps {
        model: Some(model),
        connection_name: connection.name.clone(),
        model_name: connection.default_model.clone(),
        always_allow: settings.always_allow,
        always_deny: settings.always_deny,
        initial_tier,
        initial_entries,
        initial_messages,
        system_context,
        mcp_tools,
        skills: discovered_skills,
        session_path,
        user_state_dir: paths.user_state_dir.clone(),
        user_config_dir: paths.user_config_dir.clone(),
        project_config_dir: paths.project_config_dir.clone(),
        project_root: project_root.to_path_buf(),
        created_at,
    };

    ntui::render(ntui::element!(App(
        model: props.model,
        connection_name: props.connection_name,
        model_name: props.model_name,
        always_allow: props.always_allow,
        always_deny: props.always_deny,
        initial_tier: props.initial_tier,
        initial_entries: props.initial_entries,
        initial_messages: props.initial_messages,
        system_context: props.system_context,
        mcp_tools: props.mcp_tools,
        skills: props.skills,
        session_path: props.session_path,
        user_state_dir: props.user_state_dir,
        user_config_dir: props.user_config_dir,
        project_config_dir: props.project_config_dir,
        project_root: props.project_root,
        created_at: props.created_at
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
        assert!(matches!(
            result,
            Err(TuiSessionError::AmbiguousConnection(_))
        ));
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

    #[test]
    fn resolve_connection_for_resume_picks_the_sessions_own_connection_among_several() {
        // Regression test for the Task 16 code-review bug: with 2+ configured
        // connections, resuming a session must select the connection the
        // session was actually created under, not the first/default one.
        let connections = vec![conn("a"), conn("b")];
        let result = resolve_connection_for_resume(&connections, "b", "session-model").unwrap();
        assert_eq!(result.name, "b");
        assert_eq!(result.default_model, "session-model");
    }

    #[test]
    fn resolve_connection_for_resume_errors_when_sessions_connection_no_longer_exists() {
        let connections = vec![conn("a")];
        let result = resolve_connection_for_resume(&connections, "removed-connection", "m");
        assert!(
            matches!(result, Err(TuiSessionError::ConnectionNotFound(name)) if name == "removed-connection")
        );
    }
}
