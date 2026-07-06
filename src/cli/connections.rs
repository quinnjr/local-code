use crate::config::connection::{load_connections, save_connections, Connection};
use crate::config::paths::Paths;
use crate::config::secrets::SecretStore;
use std::io::Write;

pub fn list<W: Write>(paths: &Paths, mut out: W) -> anyhow::Result<()> {
    let connections =
        load_connections(&paths.user_config_dir, &paths.project_config_dir)?;
    if connections.is_empty() {
        writeln!(out, "No connections configured. Run `local-code connections add`.")?;
        return Ok(());
    }
    for conn in &connections {
        let has_key = SecretStore::get_api_key(&conn.name)?.is_some();
        writeln!(
            out,
            "{}  [{:?}]  {}  (default model: {}){}",
            conn.name,
            conn.provider,
            conn.base_url,
            conn.default_model,
            if has_key { "  [key stored]" } else { "" }
        )?;
    }
    Ok(())
}

pub fn remove<W: Write>(paths: &Paths, name: &str, mut out: W) -> anyhow::Result<()> {
    let mut connections =
        load_connections(&paths.user_config_dir, &paths.project_config_dir)?;
    let before = connections.len();
    connections.retain(|c| c.name != name);
    if connections.len() == before {
        writeln!(out, "No connection named '{name}' found.")?;
        return Ok(());
    }
    save_connections(&paths.project_config_dir, &connections)?;
    SecretStore::delete_api_key(name)?;
    writeln!(out, "Removed connection '{name}'.")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::connection::ProviderKind;
    use std::sync::Once;
    use tempfile::tempdir;

    static INIT: Once = Once::new();
    fn use_mock_keyring() {
        INIT.call_once(|| {
            keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        });
    }

    fn test_paths(project_dir: &std::path::Path) -> Paths {
        Paths {
            user_config_dir: project_dir.join("user-config-unused"),
            project_config_dir: project_dir.to_path_buf(),
            user_state_dir: project_dir.join("state-unused"),
        }
    }

    #[test]
    fn list_reports_no_connections_when_empty() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        let mut out = Vec::new();
        list(&paths, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("No connections configured"));
    }

    #[test]
    fn list_prints_each_connection() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        save_connections(
            &paths.project_config_dir,
            &[Connection {
                name: "conn-x".into(),
                provider: ProviderKind::OpenAiCompatible,
                base_url: "http://localhost:8000/v1".into(),
                default_model: "m".into(),
                models: vec![],
            }],
        )
        .unwrap();

        let mut out = Vec::new();
        list(&paths, &mut out).unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("conn-x"));
        assert!(printed.contains("http://localhost:8000/v1"));
    }

    #[test]
    fn remove_deletes_matching_connection_and_its_key() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        save_connections(
            &paths.project_config_dir,
            &[Connection {
                name: "conn-y".into(),
                provider: ProviderKind::Ollama,
                base_url: "http://localhost:11434".into(),
                default_model: "llama3.1".into(),
                models: vec![],
            }],
        )
        .unwrap();
        SecretStore::set_api_key("conn-y", "unused-key").unwrap();

        let mut out = Vec::new();
        remove(&paths, "conn-y", &mut out).unwrap();

        let remaining = load_connections(&paths.user_config_dir, &paths.project_config_dir).unwrap();
        assert!(remaining.is_empty());
        assert_eq!(SecretStore::get_api_key("conn-y").unwrap(), None);
    }

    #[test]
    fn remove_reports_when_name_not_found() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        let mut out = Vec::new();
        remove(&paths, "does-not-exist", &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("No connection named"));
    }
}
