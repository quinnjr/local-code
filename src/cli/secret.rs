// src/cli/secret.rs

use crate::config::paths::Paths;
use crate::config::secrets;
use std::io::{BufRead, Write};

/// Stores a named secret in the OS keyring. The value is read from `input`
/// (one line, trimmed) rather than taken as a CLI argument, so it never
/// appears in shell history or `ps` output.
pub fn set<R: BufRead, W: Write>(
    paths: &Paths,
    name: &str,
    mut input: R,
    mut out: W,
) -> anyhow::Result<()> {
    write!(out, "Value for '{name}': ")?;
    out.flush()?;
    let mut line = String::new();
    input.read_line(&mut line)?;
    let value = line.trim();
    store_value(paths, name, value, out)
}

/// Stores `value` under `name` and writes the confirmation line to `out`.
/// Shared by `set` (after reading from `input`) and the interactive
/// non-echoing prompt path in `cli::run`, which reads the value via
/// `rpassword` instead of a `BufRead`.
pub fn store_value<W: Write>(
    paths: &Paths,
    name: &str,
    value: &str,
    mut out: W,
) -> anyhow::Result<()> {
    if value.is_empty() {
        anyhow::bail!("secret value cannot be empty");
    }
    secrets::store_secret(&paths.user_config_dir, name, value)?;
    writeln!(out, "Stored secret '{name}'.")?;
    Ok(())
}

pub fn rm<W: Write>(paths: &Paths, name: &str, mut out: W) -> anyhow::Result<()> {
    secrets::remove_secret(&paths.user_config_dir, name)?;
    writeln!(out, "Removed secret '{name}'.")?;
    Ok(())
}

pub fn ls<W: Write>(paths: &Paths, mut out: W) -> anyhow::Result<()> {
    let names = secrets::list_secret_names(&paths.user_config_dir)?;
    if names.is_empty() {
        writeln!(
            out,
            "No secrets stored. Run `local-code secret set <name>`."
        )?;
        return Ok(());
    }
    for name in names {
        writeln!(out, "{name}")?;
    }
    Ok(())
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

    fn paths_in(dir: &std::path::Path) -> Paths {
        Paths {
            user_config_dir: dir.join("user-config"),
            project_config_dir: dir.join("project-config"),
            user_state_dir: dir.join("user-state"),
        }
    }

    #[test]
    fn set_prompts_stores_and_confirms() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = paths_in(dir.path());
        let mut out = Vec::new();
        set(&paths, "gh-token", &b"tok-999\n"[..], &mut out).unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("Value for 'gh-token':"));
        assert!(printed.contains("Stored secret 'gh-token'."));
        assert!(!printed.contains("tok-999"), "value must never be echoed");
        assert_eq!(
            crate::config::secrets::SecretStore::get_secret("gh-token").unwrap(),
            Some("tok-999".to_string())
        );
        assert_eq!(
            secrets::list_secret_names(&paths.user_config_dir).unwrap(),
            vec!["gh-token".to_string()]
        );
    }

    #[test]
    fn store_value_stores_confirms_and_rejects_empty() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = paths_in(dir.path());
        let mut out = Vec::new();
        store_value(&paths, "gh-token", "tok-999", &mut out).unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("Stored secret 'gh-token'."));
        assert!(!printed.contains("tok-999"), "value must never be echoed");
        assert_eq!(
            crate::config::secrets::SecretStore::get_secret("gh-token").unwrap(),
            Some("tok-999".to_string())
        );

        let mut out = Vec::new();
        let err = store_value(&paths, "empty-val", "", &mut out).unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn set_rejects_empty_value() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = paths_in(dir.path());
        let mut out = Vec::new();
        let err = set(&paths, "empty-val", &b"\n"[..], &mut out).unwrap_err();
        assert!(err.to_string().contains("cannot be empty"));
    }

    #[test]
    fn set_rejects_invalid_name() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = paths_in(dir.path());
        let mut out = Vec::new();
        let err = set(&paths, "bad name", &b"v\n"[..], &mut out).unwrap_err();
        assert!(err.to_string().contains("invalid secret name"));
    }

    #[test]
    fn rm_removes_and_ls_lists_names_only() {
        use_mock_keyring();
        let dir = tempdir().unwrap();
        let paths = paths_in(dir.path());
        set(&paths, "keep", &b"v1\n"[..], Vec::new()).unwrap();
        set(&paths, "drop", &b"v2\n"[..], Vec::new()).unwrap();
        let mut out = Vec::new();
        rm(&paths, "drop", &mut out).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("Removed secret 'drop'.")
        );

        let mut out = Vec::new();
        ls(&paths, &mut out).unwrap();
        let printed = String::from_utf8(out).unwrap();
        assert!(printed.contains("keep"));
        assert!(!printed.contains("drop"));
        assert!(!printed.contains("v1"), "values must never be printed");
    }

    #[test]
    fn ls_with_no_secrets_hints_at_set() {
        let dir = tempdir().unwrap();
        let paths = paths_in(dir.path());
        let mut out = Vec::new();
        ls(&paths, &mut out).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("No secrets stored")
        );
    }
}
