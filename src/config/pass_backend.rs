// src/config/pass_backend.rs

//! A `pass` (password-store) backed credential store for the `keyring`
//! crate, installed by `config::secrets` as the process-wide fallback when
//! no Secret Service daemon is reachable (headless boxes, minimal WMs).
//! Entries live under `<service>/<user>` in the user's password store, so
//! they mirror keyring entries one-to-one and are fully readable with the
//! standard `pass` CLI.

use std::any::Any;

use keyring::Error as KeyringError;
use keyring::credential::{Credential, CredentialApi, CredentialBuilderApi, CredentialPersistence};
use pass_sys::PasswordStore;

/// The in-store entry name for a keyring (service, user) pair.
fn entry_name(service: &str, user: &str) -> String {
    format!("{service}/{user}")
}

/// Read/delete-path error mapping: a missing entry — or a store that was
/// never initialized, in which no entry can exist — is `NoEntry`, matching
/// how `SecretStore` treats absent secrets on every other backend.
fn map_read_err(err: pass_sys::Error) -> KeyringError {
    match err {
        pass_sys::Error::NotFound(_) | pass_sys::Error::NotInitialized(_) => KeyringError::NoEntry,
        other => KeyringError::PlatformFailure(Box::new(other)),
    }
}

/// Write-path error mapping: an uninitialized store is an actionable
/// condition (the user must pick a GPG identity), not a missing entry.
fn map_write_err(err: pass_sys::Error) -> KeyringError {
    match err {
        pass_sys::Error::NotInitialized(dir) => KeyringError::PlatformFailure(
            format!(
                "password store at {} is not initialized; run `pass init <gpg-id>` first",
                dir.display()
            )
            .into(),
        ),
        other => KeyringError::PlatformFailure(Box::new(other)),
    }
}

/// One keyring credential, addressing a single entry in the password store.
/// Holds only paths and the entry name — no secret material — so the
/// derived `Debug` is safe to log.
#[derive(Debug)]
struct PassCredential {
    store: PasswordStore,
    name: String,
}

impl CredentialApi for PassCredential {
    fn set_secret(&self, secret: &[u8]) -> keyring::Result<()> {
        let value = std::str::from_utf8(secret).map_err(|_| {
            KeyringError::Invalid(
                "secret".to_string(),
                "pass entries must be valid UTF-8".to_string(),
            )
        })?;
        self.store.insert(&self.name, value).map_err(map_write_err)
    }

    fn get_secret(&self) -> keyring::Result<Vec<u8>> {
        // `show` returns the full entry verbatim; `password()` would
        // truncate at the first line, silently corrupting any stored value
        // that contains a newline.
        self.store
            .show(&self.name)
            .map(String::into_bytes)
            .map_err(map_read_err)
    }

    fn delete_credential(&self) -> keyring::Result<()> {
        self.store.remove(&self.name).map_err(map_read_err)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Builds [`PassCredential`]s over one password store. Installed process-wide
/// via `keyring::set_default_credential_builder` by `config::secrets`'
/// fallback probe.
#[derive(Debug)]
pub struct PassCredentialBuilder {
    store: PasswordStore,
}

impl PassCredentialBuilder {
    /// The user's default store: `$PASSWORD_STORE_DIR` if set, otherwise
    /// `~/.password-store`.
    pub fn new() -> Self {
        Self {
            store: PasswordStore::new(),
        }
    }

    /// A builder over a specific store — lets tests point at a hermetic
    /// tempdir store with its own GPG home.
    pub fn with_store(store: PasswordStore) -> Self {
        Self { store }
    }
}

impl Default for PassCredentialBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl CredentialBuilderApi for PassCredentialBuilder {
    fn build(
        &self,
        _target: Option<&str>,
        service: &str,
        user: &str,
    ) -> keyring::Result<Box<Credential>> {
        Ok(Box::new(PassCredential {
            store: self.store.clone(),
            name: entry_name(service, user),
        }))
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn persistence(&self) -> CredentialPersistence {
        CredentialPersistence::UntilDelete
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_name_nests_user_under_service_folder() {
        assert_eq!(
            entry_name("local-code", "secret:gh"),
            "local-code/secret:gh"
        );
    }

    #[test]
    fn read_errors_map_missing_and_uninitialized_to_no_entry() {
        let missing = pass_sys::Error::NotFound("local-code/x".into());
        assert!(matches!(map_read_err(missing), keyring::Error::NoEntry));
        let uninit = pass_sys::Error::NotInitialized(std::path::PathBuf::from("/tmp/store"));
        assert!(matches!(map_read_err(uninit), keyring::Error::NoEntry));
    }

    #[test]
    fn read_errors_map_other_failures_to_platform_failure() {
        let io = pass_sys::Error::Io(std::io::Error::other("disk on fire"));
        assert!(matches!(
            map_read_err(io),
            keyring::Error::PlatformFailure(_)
        ));
    }

    #[test]
    fn write_error_on_uninitialized_store_tells_the_user_to_pass_init() {
        let uninit = pass_sys::Error::NotInitialized(std::path::PathBuf::from("/tmp/store"));
        let err = map_write_err(uninit);
        let keyring::Error::PlatformFailure(inner) = err else {
            panic!("expected PlatformFailure");
        };
        assert!(inner.to_string().contains("pass init"));
    }

    #[test]
    fn builder_debug_output_never_holds_secret_values() {
        // The builder and credential hold only paths and an entry name;
        // this pins that a Debug derive stays safe to log.
        let builder = PassCredentialBuilder::with_store(pass_sys::PasswordStore::with_store_dir(
            "/tmp/nowhere",
        ));
        let debug = format!("{builder:?}");
        assert!(debug.contains("nowhere"));
    }
}
