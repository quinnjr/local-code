use std::io::Write;

use crate::cli::marketplace::marketplace_host_client;
use crate::cli::skills::scope_label;
use crate::config::paths::Paths;
use crate::marketplace::install::{
    MarketplaceError, install_plugin, list_available_plugins, remove_plugin, update_plugin,
};
use crate::marketplace::registry;
use crate::skills::types::Scope;

/// Parses a `<plugin>@<marketplace>` install reference (Claude Code's
/// `/plugin install foo@marketplace` shape).
fn parse_plugin_spec(spec: &str) -> anyhow::Result<(String, String)> {
    let (plugin, marketplace) = spec.split_once('@').ok_or_else(|| {
        anyhow::anyhow!("expected <plugin>@<marketplace> (e.g. code-formatter@acme-tools)")
    })?;
    if plugin.is_empty() || marketplace.is_empty() || marketplace.contains('@') {
        anyhow::bail!("expected <plugin>@<marketplace> (e.g. code-formatter@acme-tools)");
    }
    Ok((plugin.to_string(), marketplace.to_string()))
}

pub async fn install<W: Write>(
    paths: &Paths,
    spec: &str,
    global: bool,
    mut out: W,
) -> anyhow::Result<()> {
    let (plugin, marketplace_name) = parse_plugin_spec(spec)?;
    let marketplace = registry::find_marketplace(paths, &marketplace_name)?;
    let scope = if global {
        Scope::Global
    } else {
        Scope::Project
    };

    let installed = install_plugin(
        paths,
        scope,
        &marketplace,
        &plugin,
        &marketplace_host_client,
    )
    .await
    .map_err(|e| augment_unknown_plugin(e, &marketplace_name))?;

    writeln!(
        out,
        "Installed plugin '{plugin}' from marketplace '{marketplace_name}' ({}):",
        scope_label(scope)
    )?;
    for skill in &installed {
        writeln!(out, "  skill '{skill}'")?;
    }
    Ok(())
}

/// An `UnknownPlugin` error on its own doesn't show what *is* available —
/// point at `plugin list` when that's what failed.
fn augment_unknown_plugin(e: MarketplaceError, marketplace: &str) -> anyhow::Error {
    match &e {
        MarketplaceError::UnknownPlugin { .. } => {
            anyhow::anyhow!("{e} (see `local-code plugin list` for what '{marketplace}' offers)")
        }
        _ => e.into(),
    }
}

pub async fn list<W: Write>(paths: &Paths, mut out: W) -> anyhow::Result<()> {
    let marketplaces = registry::load_registry(paths)?;
    if marketplaces.is_empty() {
        writeln!(
            out,
            "No marketplaces registered (add one with `local-code marketplace add`)."
        )?;
        return Ok(());
    }
    let available = list_available_plugins(paths, &marketplaces, &marketplace_host_client).await;
    if available.is_empty() {
        writeln!(out, "No plugins available.")?;
        return Ok(());
    }
    for plugin in available {
        let status = if plugin.installed_scopes.is_empty() {
            "not installed".to_string()
        } else {
            let scopes = plugin
                .installed_scopes
                .iter()
                .map(|s| scope_label(*s))
                .collect::<Vec<_>>()
                .join(", ");
            format!("installed ({scopes})")
        };
        writeln!(
            out,
            "{}@{} · {} · {}",
            plugin.name,
            plugin.marketplace,
            plugin.description.as_deref().unwrap_or("(no description)"),
            status
        )?;
    }
    Ok(())
}

pub fn remove<W: Write>(paths: &Paths, name: &str, global: bool, mut out: W) -> anyhow::Result<()> {
    let scope = if global {
        Scope::Global
    } else {
        Scope::Project
    };
    let removed = remove_plugin(paths, scope, name)?;
    writeln!(out, "Removed plugin '{name}' ({}):", scope_label(scope))?;
    for skill in &removed {
        writeln!(out, "  skill '{skill}'")?;
    }
    Ok(())
}

pub async fn update<W: Write>(
    paths: &Paths,
    name: &str,
    global: bool,
    mut out: W,
) -> anyhow::Result<()> {
    let scope = if global {
        Scope::Global
    } else {
        Scope::Project
    };
    let marketplace_names =
        crate::marketplace::install::installed_plugin_marketplaces(paths, scope, name);
    if marketplace_names.is_empty() {
        anyhow::bail!("no plugin named '{name}' is installed in this scope");
    }

    let mut failures = 0usize;
    for marketplace_name in &marketplace_names {
        let result = async {
            let marketplace = registry::find_marketplace(paths, marketplace_name)?;
            update_plugin(paths, scope, &marketplace, name, &marketplace_host_client).await
        }
        .await;
        match result {
            Ok(report) => {
                for skill in &report.updated {
                    writeln!(out, "Updated skill '{skill}'")?;
                }
                for skill in &report.added {
                    writeln!(out, "Installed new skill '{skill}'")?;
                }
                for skill in &report.removed {
                    writeln!(out, "Removed skill '{skill}' (no longer in the plugin)")?;
                }
                if report.updated.is_empty() && report.added.is_empty() && report.removed.is_empty()
                {
                    writeln!(out, "Plugin '{name}' is already up to date")?;
                }
            }
            Err(e) => {
                failures += 1;
                writeln!(out, "Failed to update plugin '{name}': {e}")?;
            }
        }
    }
    if failures > 0 {
        anyhow::bail!("{failures} plugin update(s) failed");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plugin_at_marketplace() {
        let (plugin, marketplace) = parse_plugin_spec("code-formatter@acme-tools").unwrap();
        assert_eq!(plugin, "code-formatter");
        assert_eq!(marketplace, "acme-tools");
    }

    #[test]
    fn rejects_specs_without_an_at_sign_or_with_empty_parts() {
        assert!(parse_plugin_spec("code-formatter").is_err());
        assert!(parse_plugin_spec("@acme-tools").is_err());
        assert!(parse_plugin_spec("code-formatter@").is_err());
        assert!(parse_plugin_spec("a@b@c").is_err());
    }
}
