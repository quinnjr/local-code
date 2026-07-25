use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::paths::Paths;
use crate::marketplace::source::MarketplaceSource;

/// One registered marketplace: the catalog's own name (the
/// `plugin@<name>` install suffix) plus where to fetch the catalog from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredMarketplace {
    pub name: String,
    #[serde(flatten)]
    pub source: MarketplaceSource,
}

#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
struct MarketplacesFile {
    #[serde(rename = "marketplace", default)]
    marketplaces: Vec<RegisteredMarketplace>,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize marketplaces.toml: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("no marketplace named '{0}' is registered (see `local-code marketplace list`)")]
    NotRegistered(String),
}

/// The registry file. Marketplaces are user-level only (like Claude Code's
/// `~/.claude/plugins/known_marketplaces.json`): they're a discovery
/// mechanism, while the *installed* skills land in the usual project/global
/// skill scopes.
fn registry_path(paths: &Paths) -> PathBuf {
    paths.user_config_dir.join("marketplaces.toml")
}

/// Loads every registered marketplace. A missing registry file yields an
/// empty list, not an error — the same contract as the connections and
/// `mcp.toml` loaders.
pub fn load_registry(paths: &Paths) -> Result<Vec<RegisteredMarketplace>, RegistryError> {
    let path = registry_path(paths);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(RegistryError::Read { path, source: e }),
    };
    let file: MarketplacesFile =
        toml::from_str(&text).map_err(|e| RegistryError::Parse { path, source: e })?;
    Ok(file.marketplaces)
}

fn save_registry(
    paths: &Paths,
    marketplaces: &[RegisteredMarketplace],
) -> Result<(), RegistryError> {
    let path = registry_path(paths);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| RegistryError::Write {
            path: parent.to_path_buf(),
            source: e,
        })?;
    }
    let file = MarketplacesFile {
        marketplaces: marketplaces.to_vec(),
    };
    let text = toml::to_string_pretty(&file)?;
    std::fs::write(&path, text).map_err(|e| RegistryError::Write { path, source: e })
}

/// Registers a marketplace, replacing any existing registration of the same
/// name (Claude Code semantics: one marketplace per name; re-adding under an
/// existing name updates it). Returns `true` if an existing entry was
/// replaced.
pub fn add_marketplace(paths: &Paths, entry: RegisteredMarketplace) -> Result<bool, RegistryError> {
    let mut marketplaces = load_registry(paths)?;
    let replaced = marketplaces.iter().any(|m| m.name == entry.name);
    marketplaces.retain(|m| m.name != entry.name);
    marketplaces.push(entry);
    save_registry(paths, &marketplaces)?;
    Ok(replaced)
}

/// Unregisters a marketplace by name. Errors with `NotRegistered` if no such
/// marketplace exists. Skills already installed from its plugins are left in
/// place — they're ordinary installed skills with their own manifests.
pub fn remove_marketplace(paths: &Paths, name: &str) -> Result<(), RegistryError> {
    let mut marketplaces = load_registry(paths)?;
    let before = marketplaces.len();
    marketplaces.retain(|m| m.name != name);
    if marketplaces.len() == before {
        return Err(RegistryError::NotRegistered(name.to_string()));
    }
    save_registry(paths, &marketplaces)
}

/// Looks up one registered marketplace by name.
pub fn find_marketplace(paths: &Paths, name: &str) -> Result<RegisteredMarketplace, RegistryError> {
    load_registry(paths)?
        .into_iter()
        .find(|m| m.name == name)
        .ok_or_else(|| RegistryError::NotRegistered(name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::types::Host;
    use std::path::Path;
    use tempfile::tempdir;

    fn test_paths(root: &Path) -> Paths {
        Paths {
            user_config_dir: root.join("user-config"),
            project_config_dir: root.join("project/.local-code"),
            user_state_dir: root.join("user-state"),
        }
    }

    fn remote_entry(name: &str) -> RegisteredMarketplace {
        RegisteredMarketplace {
            name: name.to_string(),
            source: MarketplaceSource::Remote {
                host: Host::GitHub,
                owner: "acme".into(),
                repo: "plugins".into(),
                path: String::new(),
                git_ref: "main".into(),
            },
        }
    }

    #[test]
    fn missing_registry_file_loads_as_empty() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        assert!(load_registry(&paths).unwrap().is_empty());
    }

    #[test]
    fn add_then_list_round_trips_both_source_kinds() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        add_marketplace(&paths, remote_entry("acme-tools")).unwrap();
        add_marketplace(
            &paths,
            RegisteredMarketplace {
                name: "local-dev".into(),
                source: MarketplaceSource::Local {
                    path: PathBuf::from("/abs/marketplace"),
                },
            },
        )
        .unwrap();

        let marketplaces = load_registry(&paths).unwrap();
        assert_eq!(marketplaces.len(), 2);
        assert_eq!(marketplaces[0], remote_entry("acme-tools"));
        assert_eq!(
            marketplaces[1].source,
            MarketplaceSource::Local {
                path: PathBuf::from("/abs/marketplace")
            }
        );
    }

    #[test]
    fn adding_the_same_name_replaces_and_reports_it() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        assert!(!add_marketplace(&paths, remote_entry("acme-tools")).unwrap());

        let mut updated = remote_entry("acme-tools");
        updated.source = MarketplaceSource::Local {
            path: PathBuf::from("/elsewhere"),
        };
        assert!(add_marketplace(&paths, updated).unwrap());

        let marketplaces = load_registry(&paths).unwrap();
        assert_eq!(marketplaces.len(), 1);
        assert_eq!(
            marketplaces[0].source,
            MarketplaceSource::Local {
                path: PathBuf::from("/elsewhere")
            }
        );
    }

    #[test]
    fn remove_deletes_the_entry() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        add_marketplace(&paths, remote_entry("acme-tools")).unwrap();
        remove_marketplace(&paths, "acme-tools").unwrap();
        assert!(load_registry(&paths).unwrap().is_empty());
    }

    #[test]
    fn removing_an_unknown_marketplace_errors() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let result = remove_marketplace(&paths, "nope");
        assert!(matches!(
            result,
            Err(RegistryError::NotRegistered(name)) if name == "nope"
        ));
    }

    #[test]
    fn find_returns_the_named_entry() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        add_marketplace(&paths, remote_entry("acme-tools")).unwrap();
        assert_eq!(
            find_marketplace(&paths, "acme-tools").unwrap(),
            remote_entry("acme-tools")
        );
        assert!(matches!(
            find_marketplace(&paths, "nope"),
            Err(RegistryError::NotRegistered(_))
        ));
    }
}
