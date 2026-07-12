use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::{Deserialize, Serialize};

const SERVICE_NAME: &str = "local-code";

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("keyring operation failed: {0}")]
    Keyring(#[from] keyring::Error),
    #[error("invalid secret name '{0}': allowed characters are A-Z, a-z, 0-9, '-' and '_'")]
    InvalidName(String),
    #[error("failed to read/write secret index {path}: {source}")]
    Index {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse secret index {path}: {source}")]
    IndexParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
}

pub struct SecretStore;

/// Process-global cache of `keyring::Entry` handles, keyed by connection name.
///
/// A real `Entry` is a stateless handle to external OS-managed storage, so caching
/// it changes nothing about correctness there. Under `keyring::mock`, however, each
/// `Entry` carries its own in-memory storage (the mock deliberately has no
/// persistence keyed by service/user), so a fresh `Entry::new(...)` per call would
/// never see data set by an earlier call. Reusing the same `Entry` per connection
/// name fixes that without changing behavior against real backends.
fn entry_cache() -> &'static Mutex<HashMap<String, keyring::Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, keyring::Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Fetches the cached `Entry` for `connection_name`, creating and caching one if
/// absent.
fn get_or_insert_entry<'a>(
    cache: &'a mut HashMap<String, keyring::Entry>,
    connection_name: &str,
) -> Result<&'a keyring::Entry, SecretsError> {
    if !cache.contains_key(connection_name) {
        let entry = keyring::Entry::new(SERVICE_NAME, connection_name)?;
        cache.insert(connection_name.to_string(), entry);
    }
    Ok(cache.get(connection_name).expect("entry was just inserted"))
}

/// Keyring `user` prefix for generic named secrets, keeping them in a
/// namespace that can never collide with connection-key entries
/// (`user = <connection-name>`) or skill host tokens (`user = "github"`…).
const SECRET_PREFIX: &str = "secret:";

/// Valid names for generic named secrets: non-empty, ASCII alphanumerics
/// plus `-` and `_`. This is also the exact charset the `${keyring:<name>}`
/// reference pattern in `config::mcp_servers` matches.
pub fn is_valid_secret_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Maps every character outside the secret-name charset to `-`, so a name
/// derived from arbitrary user input (e.g. an MCP server name) is always a
/// valid secret name and a valid `${keyring:...}` reference.
pub fn sanitize_secret_name(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Names-only index of stored secrets, needed because no keyring backend can
/// enumerate its entries portably. Values never appear here.
#[derive(Debug, Default, Serialize, Deserialize)]
struct SecretNamesFile {
    #[serde(default)]
    names: Vec<String>,
}

fn index_path(user_config_dir: &Path) -> PathBuf {
    user_config_dir.join("secret-names.toml")
}

/// Names of all secrets stored via [`store_secret`], sorted. A secret set by
/// external tools directly in the OS keyring won't appear here until it is
/// re-`set` through local-code.
pub fn list_secret_names(user_config_dir: &Path) -> Result<Vec<String>, SecretsError> {
    let path = index_path(user_config_dir);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path).map_err(|source| SecretsError::Index {
        path: path.clone(),
        source,
    })?;
    let file: SecretNamesFile =
        toml::from_str(&text).map_err(|source| SecretsError::IndexParse { path, source })?;
    Ok(file.names)
}

fn write_index(user_config_dir: &Path, names: &[String]) -> Result<(), SecretsError> {
    std::fs::create_dir_all(user_config_dir).map_err(|source| SecretsError::Index {
        path: user_config_dir.to_path_buf(),
        source,
    })?;
    let file = SecretNamesFile {
        names: names.to_vec(),
    };
    let text = toml::to_string_pretty(&file).expect("SecretNamesFile serializes without error");
    let path = index_path(user_config_dir);
    std::fs::write(&path, text).map_err(|source| SecretsError::Index { path, source })
}

/// Validates `name`, stores the value in the OS keyring, and records the name
/// in the index. The single write path for named secrets — the `secret set`
/// CLI and the `/mcp add` wizard both go through here so the index stays
/// consistent.
pub fn store_secret(user_config_dir: &Path, name: &str, value: &str) -> Result<(), SecretsError> {
    if !is_valid_secret_name(name) {
        return Err(SecretsError::InvalidName(name.to_string()));
    }
    SecretStore::set_secret(name, value)?;
    let mut names = list_secret_names(user_config_dir)?;
    if !names.iter().any(|n| n == name) {
        names.push(name.to_string());
        names.sort();
    }
    write_index(user_config_dir, &names)
}

/// Deletes the keyring entry (a missing entry is not an error) and removes
/// the name from the index.
pub fn remove_secret(user_config_dir: &Path, name: &str) -> Result<(), SecretsError> {
    if !is_valid_secret_name(name) {
        return Err(SecretsError::InvalidName(name.to_string()));
    }
    SecretStore::delete_secret(name)?;
    let mut names = list_secret_names(user_config_dir)?;
    names.retain(|n| n != name);
    write_index(user_config_dir, &names)
}

impl SecretStore {
    /// Returns `Ok(None)` if no key is stored for this connection (e.g. the local
    /// server needs no auth), `Ok(Some(key))` if one is, and `Err` on a genuine
    /// backend failure.
    pub fn get_api_key(connection_name: &str) -> Result<Option<String>, SecretsError> {
        let mut cache = entry_cache().lock().expect("secret entry cache poisoned");
        let entry = get_or_insert_entry(&mut cache, connection_name)?;
        match entry.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(other) => Err(other.into()),
        }
    }

    pub fn set_api_key(connection_name: &str, api_key: &str) -> Result<(), SecretsError> {
        let mut cache = entry_cache().lock().expect("secret entry cache poisoned");
        let entry = get_or_insert_entry(&mut cache, connection_name)?;
        entry.set_password(api_key)?;
        Ok(())
    }

    pub fn delete_api_key(connection_name: &str) -> Result<(), SecretsError> {
        let mut cache = entry_cache().lock().expect("secret entry cache poisoned");
        let entry = get_or_insert_entry(&mut cache, connection_name)?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(other) => Err(other.into()),
        }
    }

    /// Generic named secret, stored under `user = "secret:<name>"`. Returns
    /// `Ok(None)` when no such secret exists.
    pub fn get_secret(name: &str) -> Result<Option<String>, SecretsError> {
        let mut cache = entry_cache().lock().expect("secret entry cache poisoned");
        let entry = get_or_insert_entry(&mut cache, &format!("{SECRET_PREFIX}{name}"))?;
        match entry.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(other) => Err(other.into()),
        }
    }

    pub fn set_secret(name: &str, value: &str) -> Result<(), SecretsError> {
        let mut cache = entry_cache().lock().expect("secret entry cache poisoned");
        let entry = get_or_insert_entry(&mut cache, &format!("{SECRET_PREFIX}{name}"))?;
        entry.set_password(value)?;
        Ok(())
    }

    pub fn delete_secret(name: &str) -> Result<(), SecretsError> {
        let mut cache = entry_cache().lock().expect("secret entry cache poisoned");
        let entry = get_or_insert_entry(&mut cache, &format!("{SECRET_PREFIX}{name}"))?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(other) => Err(other.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    static INIT: Once = Once::new();

    /// Switches keyring to its platform-independent in-memory mock store so tests
    /// don't touch the real OS secret manager and can run in CI without a display
    /// server / Keychain prompt.
    fn use_mock_keyring() {
        INIT.call_once(|| {
            keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
        });
    }

    #[test]
    fn missing_key_returns_none_not_error() {
        use_mock_keyring();
        let result = SecretStore::get_api_key("conn-with-no-key-set-yet").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn set_then_get_round_trips() {
        use_mock_keyring();
        SecretStore::set_api_key("conn-a", "sk-test-123").unwrap();
        let result = SecretStore::get_api_key("conn-a").unwrap();
        assert_eq!(result, Some("sk-test-123".to_string()));
    }

    #[test]
    fn delete_removes_key() {
        use_mock_keyring();
        SecretStore::set_api_key("conn-b", "sk-test-456").unwrap();
        SecretStore::delete_api_key("conn-b").unwrap();
        let result = SecretStore::get_api_key("conn-b").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn delete_on_missing_key_is_not_an_error() {
        use_mock_keyring();
        SecretStore::delete_api_key("conn-never-existed").unwrap();
    }

    #[test]
    fn named_secret_round_trips() {
        use_mock_keyring();
        SecretStore::set_secret("github-mcp", "tok-123").unwrap();
        assert_eq!(
            SecretStore::get_secret("github-mcp").unwrap(),
            Some("tok-123".to_string())
        );
    }

    #[test]
    fn missing_named_secret_returns_none() {
        use_mock_keyring();
        assert_eq!(SecretStore::get_secret("never-set").unwrap(), None);
    }

    #[test]
    fn delete_named_secret_removes_it_and_is_idempotent() {
        use_mock_keyring();
        SecretStore::set_secret("to-delete", "v").unwrap();
        SecretStore::delete_secret("to-delete").unwrap();
        assert_eq!(SecretStore::get_secret("to-delete").unwrap(), None);
        SecretStore::delete_secret("to-delete").unwrap(); // missing is not an error
    }

    #[test]
    fn named_secrets_do_not_collide_with_connection_keys() {
        use_mock_keyring();
        SecretStore::set_api_key("same-name", "connection-key").unwrap();
        SecretStore::set_secret("same-name", "named-secret").unwrap();
        assert_eq!(
            SecretStore::get_api_key("same-name").unwrap(),
            Some("connection-key".to_string())
        );
        assert_eq!(
            SecretStore::get_secret("same-name").unwrap(),
            Some("named-secret".to_string())
        );
    }

    #[test]
    fn secret_name_validation_accepts_the_documented_charset_only() {
        assert!(is_valid_secret_name("github-mcp"));
        assert!(is_valid_secret_name("A1_b-2"));
        assert!(!is_valid_secret_name(""));
        assert!(!is_valid_secret_name("has space"));
        assert!(!is_valid_secret_name("has:colon"));
        assert!(!is_valid_secret_name("hàs-unicode"));
    }

    #[test]
    fn store_secret_writes_keyring_and_index_sorted() {
        use_mock_keyring();
        let dir = tempfile::tempdir().unwrap();
        store_secret(dir.path(), "zeta", "v1").unwrap();
        store_secret(dir.path(), "alpha", "v2").unwrap();
        store_secret(dir.path(), "alpha", "v3").unwrap(); // overwrite, no dup in index
        assert_eq!(
            list_secret_names(dir.path()).unwrap(),
            vec!["alpha".to_string(), "zeta".to_string()]
        );
        assert_eq!(
            SecretStore::get_secret("alpha").unwrap(),
            Some("v3".to_string())
        );
        let index_text = std::fs::read_to_string(dir.path().join("secret-names.toml")).unwrap();
        assert!(
            !index_text.contains("v3"),
            "index must never contain values"
        );
    }

    #[test]
    fn remove_secret_deletes_keyring_entry_and_index_line() {
        use_mock_keyring();
        let dir = tempfile::tempdir().unwrap();
        store_secret(dir.path(), "gone", "v").unwrap();
        remove_secret(dir.path(), "gone").unwrap();
        assert_eq!(SecretStore::get_secret("gone").unwrap(), None);
        assert!(list_secret_names(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn store_secret_rejects_invalid_names() {
        use_mock_keyring();
        let dir = tempfile::tempdir().unwrap();
        let err = store_secret(dir.path(), "bad name", "v").unwrap_err();
        assert!(matches!(err, SecretsError::InvalidName(n) if n == "bad name"));
    }

    #[test]
    fn remove_secret_rejects_invalid_names() {
        use_mock_keyring();
        let dir = tempfile::tempdir().unwrap();
        let err = remove_secret(dir.path(), "bad name").unwrap_err();
        assert!(matches!(err, SecretsError::InvalidName(n) if n == "bad name"));
    }

    #[test]
    fn list_secret_names_with_no_index_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_secret_names(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn sanitize_secret_name_maps_out_of_charset_chars_to_dashes() {
        assert_eq!(sanitize_secret_name("my tools"), "my-tools");
        assert_eq!(sanitize_secret_name("ok-name_1"), "ok-name_1");
        assert_eq!(sanitize_secret_name("a:b/c"), "a-b-c");
    }
}
