use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

const SERVICE_NAME: &str = "local-code";

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    #[error("keyring operation failed: {0}")]
    Keyring(#[from] keyring::Error),
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
}
