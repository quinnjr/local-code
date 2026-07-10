use crate::config::connection::{Connection, ProviderKind, load_connections, save_connections};
use crate::config::paths::Paths;
use crate::config::secrets::SecretStore;
use std::io::{BufRead, Write};

pub fn list<W: Write>(paths: &Paths, mut out: W) -> anyhow::Result<()> {
    let connections = load_connections(&paths.user_config_dir, &paths.project_config_dir)?;
    if connections.is_empty() {
        writeln!(
            out,
            "No connections configured. Run `local-code connections add`."
        )?;
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
    let mut connections = load_connections(&paths.user_config_dir, &paths.project_config_dir)?;
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

pub fn add<R: BufRead, W: Write>(
    paths: &Paths,
    mut input: R,
    mut out: W,
) -> anyhow::Result<Connection> {
    write!(out, "Connection name: ")?;
    out.flush()?;
    let name = read_line(&mut input)?;

    write!(out, "Provider type (1=openai-compatible, 2=ollama): ")?;
    out.flush()?;
    let provider = match read_line(&mut input)?.trim() {
        "2" => ProviderKind::Ollama,
        _ => ProviderKind::OpenAiCompatible,
    };

    write!(out, "Base URL: ")?;
    out.flush()?;
    let base_url = read_line(&mut input)?;

    write!(out, "Default model: ")?;
    out.flush()?;
    let default_model = read_line(&mut input)?;

    write!(out, "API key (leave blank if none): ")?;
    out.flush()?;
    let api_key = read_line(&mut input)?;

    let connection = Connection {
        name,
        provider,
        base_url,
        default_model,
        models: vec![],
    };

    let mut connections = load_connections(&paths.user_config_dir, &paths.project_config_dir)?;
    connections.retain(|c| c.name != connection.name);
    connections.push(connection.clone());
    save_connections(&paths.project_config_dir, &connections)?;

    if !api_key.is_empty() {
        SecretStore::set_api_key(&connection.name, &api_key)?;
    }

    writeln!(out, "Saved connection '{}'.", connection.name)?;
    Ok(connection)
}

fn read_line<R: BufRead>(input: &mut R) -> anyhow::Result<String> {
    let mut line = String::new();
    input.read_line(&mut line)?;
    Ok(line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("No connections configured")
        );
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

        let remaining =
            load_connections(&paths.user_config_dir, &paths.project_config_dir).unwrap();
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
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("No connection named")
        );
    }

    #[test]
    fn add_writes_connection_and_key_from_transcript() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());

        let transcript =
            "local-vllm\n1\nhttp://localhost:8000/v1\nqwen2.5-coder-32b\nsk-test-789\n";
        let mut out = Vec::new();
        let connection = add(&paths, transcript.as_bytes(), &mut out).unwrap();

        assert_eq!(connection.name, "local-vllm");
        assert_eq!(connection.provider, ProviderKind::OpenAiCompatible);
        assert_eq!(connection.base_url, "http://localhost:8000/v1");
        assert_eq!(connection.default_model, "qwen2.5-coder-32b");

        let saved = load_connections(&paths.user_config_dir, &paths.project_config_dir).unwrap();
        assert_eq!(saved, vec![connection.clone()]);
        assert_eq!(
            SecretStore::get_api_key(&connection.name).unwrap(),
            Some("sk-test-789".to_string())
        );
    }

    #[test]
    fn add_with_blank_api_key_stores_no_secret() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());

        let transcript = "home-ollama\n2\nhttp://localhost:11434\nllama3.1\n\n";
        let mut out = Vec::new();
        let connection = add(&paths, transcript.as_bytes(), &mut out).unwrap();

        assert_eq!(connection.provider, ProviderKind::Ollama);
        assert_eq!(SecretStore::get_api_key(&connection.name).unwrap(), None);
    }
}
