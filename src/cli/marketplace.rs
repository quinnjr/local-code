use std::io::Write;

use crate::cli::skills::{resolve_skill_source, skill_client};
use crate::config::paths::Paths;
use crate::marketplace::install::{MarketplaceError, fetch_catalog};
use crate::marketplace::registry::{self, RegisteredMarketplace};
use crate::marketplace::source::{MarketplaceSource, ParsedAddSpec, parse_add_spec};
use crate::skills::client::SkillClient;
use crate::skills::types::Host;

/// The host-client factory every marketplace fetch goes through (a
/// plugin's source host can differ from its marketplace's host, so clients
/// are built per fetch target). Thin adapter over the skills CLI's
/// `skill_client`, which owns the keyring-credential wiring per host.
pub(crate) fn marketplace_host_client(host: Host) -> Result<SkillClient, MarketplaceError> {
    skill_client(host).map_err(|e| MarketplaceError::ClientBuild(e.to_string()))
}

/// Registers a marketplace: fetches and validates its
/// `.claude-plugin/marketplace.json`, then records it under the catalog's
/// own name. Re-adding an existing name replaces the previous registration
/// (Claude Code's one-marketplace-per-name semantics).
pub async fn add<W: Write>(paths: &Paths, spec: &str, mut out: W) -> anyhow::Result<()> {
    let source = match parse_add_spec(spec)? {
        ParsedAddSpec::Local(path) => MarketplaceSource::Local { path },
        ParsedAddSpec::Remote(parsed) => {
            let client = skill_client(parsed.source.host)?;
            let resolved = resolve_skill_source(&client, parsed).await?;
            let git_ref = match resolved.git_ref {
                Some(r) => r,
                None => {
                    client
                        .resolve_default_branch(&resolved.owner, &resolved.repo)
                        .await?
                }
            };
            MarketplaceSource::Remote {
                host: resolved.host,
                owner: resolved.owner,
                repo: resolved.repo,
                path: resolved.path,
                git_ref,
            }
        }
    };

    let (catalog, _) = fetch_catalog(&source, &marketplace_host_client).await?;
    let name = catalog.name.clone();
    let plugin_count = catalog.plugins.len();
    let summary = source.summary();
    let replaced = registry::add_marketplace(
        paths,
        RegisteredMarketplace {
            name: name.clone(),
            source,
        },
    )?;

    let verb = if replaced { "Replaced" } else { "Added" };
    writeln!(
        out,
        "{verb} marketplace '{name}' from {summary} ({plugin_count} plugin(s))"
    )?;
    Ok(())
}

pub fn list<W: Write>(paths: &Paths, mut out: W) -> anyhow::Result<()> {
    let marketplaces = registry::load_registry(paths)?;
    if marketplaces.is_empty() {
        writeln!(out, "No marketplaces registered.")?;
        return Ok(());
    }
    for marketplace in marketplaces {
        writeln!(
            out,
            "{} · {}",
            marketplace.name,
            marketplace.source.summary()
        )?;
    }
    Ok(())
}

pub fn remove<W: Write>(paths: &Paths, name: &str, mut out: W) -> anyhow::Result<()> {
    registry::remove_marketplace(paths, name)?;
    writeln!(
        out,
        "Removed marketplace '{name}' (skills already installed from its plugins were left in place)"
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::source::MarketplaceSource;
    use tempfile::tempdir;

    fn test_paths(root: &std::path::Path) -> Paths {
        Paths {
            user_config_dir: root.join("user-config"),
            project_config_dir: root.join("project/.local-code"),
            user_state_dir: root.join("user-state"),
        }
    }

    fn write_local_marketplace(root: &std::path::Path) -> std::path::PathBuf {
        let dir = root.join("my-marketplace");
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.join(".claude-plugin/marketplace.json"),
            r#"{"name": "local-dev", "owner": {"name": "me"}, "plugins": [
                {"name": "greeter", "source": "./plugins/greeter"}
            ]}"#,
        )
        .unwrap();
        dir
    }

    #[tokio::test]
    async fn add_registers_a_local_marketplace() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let marketplace_dir = write_local_marketplace(root.path());
        let mut out = Vec::new();
        add(&paths, marketplace_dir.to_str().unwrap(), &mut out)
            .await
            .unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("Added marketplace 'local-dev'"),
            "got: {text}"
        );
        let marketplaces = registry::load_registry(&paths).unwrap();
        assert_eq!(marketplaces.len(), 1);
        assert_eq!(marketplaces[0].name, "local-dev");
        assert!(matches!(
            marketplaces[0].source,
            MarketplaceSource::Local { .. }
        ));
    }

    #[tokio::test]
    async fn add_replaces_a_marketplace_of_the_same_name() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let marketplace_dir = write_local_marketplace(root.path());
        add(&paths, marketplace_dir.to_str().unwrap(), Vec::new())
            .await
            .unwrap();
        let mut out = Vec::new();
        add(&paths, marketplace_dir.to_str().unwrap(), &mut out)
            .await
            .unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("Replaced marketplace 'local-dev'")
        );
        assert_eq!(registry::load_registry(&paths).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn add_fails_for_a_directory_without_a_catalog() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let empty_dir = root.path().join("not-a-marketplace");
        std::fs::create_dir_all(&empty_dir).unwrap();
        let result = add(&paths, empty_dir.to_str().unwrap(), Vec::new()).await;
        assert!(result.is_err());
        assert!(registry::load_registry(&paths).unwrap().is_empty());
    }

    #[test]
    fn list_reports_registered_marketplaces() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        registry::add_marketplace(
            &paths,
            RegisteredMarketplace {
                name: "acme-tools".into(),
                source: MarketplaceSource::Remote {
                    host: Host::GitHub,
                    owner: "acme".into(),
                    repo: "plugins".into(),
                    path: String::new(),
                    git_ref: "main".into(),
                },
            },
        )
        .unwrap();

        let mut out = Vec::new();
        list(&paths, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("acme-tools · acme/plugins@main"),
            "got: {text}"
        );
    }

    #[test]
    fn list_reports_no_marketplaces() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let mut out = Vec::new();
        list(&paths, &mut out).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("No marketplaces registered")
        );
    }

    #[test]
    fn remove_unregisters_the_marketplace() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        registry::add_marketplace(
            &paths,
            RegisteredMarketplace {
                name: "acme-tools".into(),
                source: MarketplaceSource::Local {
                    path: std::path::PathBuf::from("/abs/dir"),
                },
            },
        )
        .unwrap();

        let mut out = Vec::new();
        remove(&paths, "acme-tools", &mut out).unwrap();
        assert!(
            String::from_utf8(out)
                .unwrap()
                .contains("Removed marketplace 'acme-tools'")
        );
        assert!(registry::load_registry(&paths).unwrap().is_empty());
    }
}
