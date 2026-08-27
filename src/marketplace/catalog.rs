use serde::Deserialize;

/// The parsed form of a Claude Code `.claude-plugin/marketplace.json`
/// catalog, scoped to the fields LocalCode acts on. Unknown fields (`hooks`,
/// `mcpServers`, `category`, `renames`, ...) are ignored on purpose:
/// LocalCode installs plugin *skills* only, and serde drops unknown fields
/// by default, so catalogs written for full Claude Code keep parsing here.
/// A top-level `version` is one of those deliberately ignored fields;
/// `metadata.version` is parsed but currently unused (see
/// `CatalogMetadata::version`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MarketplaceCatalog {
    pub name: String,
    pub owner: CatalogOwner,
    /// Top-level `description`; `metadata.description` is the
    /// backward-compatible fallback Claude Code also accepts.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub metadata: CatalogMetadata,
    #[serde(default)]
    pub plugins: Vec<CatalogPlugin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CatalogOwner {
    pub name: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CatalogMetadata {
    #[serde(default)]
    pub description: Option<String>,
    /// Parsed for schema completeness but currently unused: LocalCode acts
    /// on plugin sources, not catalog versioning. Unlike `description`,
    /// there is no top-level counterpart or accessor fallback.
    #[serde(default)]
    pub version: Option<String>,
    /// Base directory prepended to relative plugin source paths (Claude
    /// Code's `metadata.pluginRoot`): with `"./plugins"` set, an entry may
    /// write `"source": "formatter"` instead of `"source":
    /// "./plugins/formatter"`.
    #[serde(rename = "pluginRoot", default)]
    pub plugin_root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct CatalogPlugin {
    pub name: String,
    pub source: PluginSource,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub author: Option<CatalogOwner>,
    /// Custom skill-directory paths, supplementing/replacing the default
    /// `skills/` scan of the plugin root (Claude Code's `skills` field —
    /// a single path string or an array of them).
    #[serde(default)]
    pub skills: Option<OneOrManyStrings>,
}

/// Claude Code accepts several catalog fields as either one string or a list
/// of strings; `skills` is the one LocalCode consumes.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum OneOrManyStrings {
    One(String),
    Many(Vec<String>),
}

impl OneOrManyStrings {
    pub fn into_vec(self) -> Vec<String> {
        match self {
            OneOrManyStrings::One(s) => vec![s],
            OneOrManyStrings::Many(v) => v,
        }
    }
}

/// Where to fetch a plugin from: a `./relative` path inside the marketplace
/// itself, or a remote source object (`{"source": "github" | "url" |
/// "git-subdir" | "npm", ...}`). Serde's untagged dispatch tries `Relative`
/// (a plain string) first, then the tagged object form.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum PluginSource {
    Relative(String),
    Remote(RemotePluginSource),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case")]
pub enum RemotePluginSource {
    Github {
        repo: String,
        #[serde(rename = "ref", default)]
        git_ref: Option<String>,
        #[serde(default)]
        sha: Option<String>,
    },
    Url {
        url: String,
        #[serde(rename = "ref", default)]
        git_ref: Option<String>,
        #[serde(default)]
        sha: Option<String>,
    },
    GitSubdir {
        url: String,
        path: String,
        #[serde(rename = "ref", default)]
        git_ref: Option<String>,
        #[serde(default)]
        sha: Option<String>,
    },
    Npm {
        #[allow(dead_code)] // parsed so catalogs stay readable; install rejects it
        package: String,
        #[allow(dead_code)]
        #[serde(default)]
        version: Option<String>,
        #[allow(dead_code)]
        #[serde(default)]
        registry: Option<String>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("failed to parse marketplace.json: {0}")]
    Parse(#[from] serde_json::Error),
    #[error(
        "invalid marketplace name '{0}' (it becomes the `plugin@name` install suffix — no spaces, '@', '/', '\\\\', or '..')"
    )]
    InvalidMarketplaceName(String),
    #[error("invalid plugin name '{0}' (no spaces, '@', '/', '\\\\', or '..')")]
    InvalidPluginName(String),
    #[error(
        "relative plugin source '{0}' must start with \"./\" (or set metadata.pluginRoot in the marketplace)"
    )]
    InvalidRelativeSource(String),
    #[error("plugin source path '{0}' must not contain '..' segments or be absolute")]
    UnsafeSourcePath(String),
}

/// True for names usable as marketplace/plugin/skill identifiers: non-empty,
/// no whitespace, no `@` (the `plugin@marketplace` separator), no path
/// separators or `..` (defense-in-depth against path-shaped names).
pub fn valid_component_name(name: &str) -> bool {
    !name.is_empty()
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c == '@' || c == '/' || c == '\\')
        && !name.contains("..")
}

/// Parses and validates a `marketplace.json` document. Validation covers
/// what LocalCode's own behavior depends on (registry key and
/// `plugin@marketplace` parsing), not the full Claude Code schema.
pub fn parse_catalog(json: &str) -> Result<MarketplaceCatalog, CatalogError> {
    let catalog: MarketplaceCatalog = serde_json::from_str(json)?;
    if !valid_component_name(&catalog.name) {
        return Err(CatalogError::InvalidMarketplaceName(catalog.name.clone()));
    }
    for plugin in &catalog.plugins {
        if !valid_component_name(&plugin.name) {
            return Err(CatalogError::InvalidPluginName(plugin.name.clone()));
        }
    }
    Ok(catalog)
}

impl MarketplaceCatalog {
    /// The effective description: top-level wins, `metadata.description` is
    /// the backward-compatible fallback (mirroring Claude Code).
    pub fn description(&self) -> Option<&str> {
        self.description
            .as_deref()
            .or(self.metadata.description.as_deref())
    }

    pub fn plugin(&self, name: &str) -> Option<&CatalogPlugin> {
        self.plugins.iter().find(|p| p.name == name)
    }
}

/// Resolves a relative plugin source string to a normalized path relative to
/// the marketplace root. Sources starting with `./` resolve directly; bare
/// paths require `metadata.pluginRoot` (joined as its base), matching Claude
/// Code. The result is rejected if it would escape the marketplace root.
pub fn resolve_relative_source(
    source: &str,
    plugin_root: Option<&str>,
) -> Result<String, CatalogError> {
    let joined = if let Some(rest) = source.strip_prefix("./") {
        rest.to_string()
    } else if let Some(root) = plugin_root {
        let root = root.strip_prefix("./").unwrap_or(root);
        format!("{}/{source}", root.trim_end_matches('/'))
    } else {
        return Err(CatalogError::InvalidRelativeSource(source.to_string()));
    };
    validate_in_repo_path(&joined).map_err(|_| CatalogError::UnsafeSourcePath(source.to_string()))
}

/// Validates and normalizes an in-repo path (`plugins/foo`): no empty
/// segments, no `..`, not absolute. Rejected wherever a catalog-supplied
/// path is joined onto a repo path or a local filesystem path, so a
/// malicious or buggy marketplace can't escape its root.
pub fn validate_in_repo_path(path: &str) -> Result<String, CatalogError> {
    let normalized = path.trim_matches('/').to_string();
    let unsafe_path = normalized.is_empty()
        || normalized
            .split('/')
            .any(|segment| segment == ".." || segment.is_empty());
    if unsafe_path {
        return Err(CatalogError::UnsafeSourcePath(path.to_string()));
    }
    Ok(normalized)
}

/// Joins a marketplace-root-relative path onto a base in-repo path (`""`
/// means repo root), producing the repo-relative path the host clients take.
pub fn join_repo_path(base: &str, rel: &str) -> String {
    let base = base.trim_matches('/');
    if base.is_empty() {
        rel.to_string()
    } else {
        format!("{base}/{rel}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_CATALOG: &str = r#"{
        "name": "company-tools",
        "owner": { "name": "DevTools Team", "email": "devtools@example.com" },
        "metadata": { "description": "meta desc", "version": "1.2.3", "pluginRoot": "./plugins" },
        "plugins": [
            {
                "name": "code-formatter",
                "source": "./plugins/formatter",
                "description": "Automatic code formatting on save",
                "version": "2.1.0",
                "author": { "name": "DevTools Team" }
            },
            {
                "name": "deployment-tools",
                "source": { "source": "github", "repo": "company/deploy-plugin" },
                "description": "Deployment automation tools"
            },
            {
                "name": "pinned",
                "source": { "source": "github", "repo": "company/pinned", "ref": "v2", "sha": "abc123" }
            },
            {
                "name": "gitlab-plugin",
                "source": { "source": "url", "url": "https://gitlab.com/team/plugin.git" }
            },
            {
                "name": "monorepo-plugin",
                "source": { "source": "git-subdir", "url": "https://github.com/acme/monorepo.git", "path": "tools/plugin" }
            },
            {
                "name": "npm-plugin",
                "source": { "source": "npm", "package": "@acme/claude-plugin", "version": "2.1.0" }
            },
            {
                "name": "custom-skills",
                "source": "./plugins/custom",
                "skills": ["./skills/code-review", "./skills/docs"]
            },
            {
                "name": "single-skill-path",
                "source": "./plugins/single",
                "skills": "./extra-skills/",
                "hooks": { "ignored": true }
            }
        ]
    }"#;

    #[test]
    fn parses_a_full_claude_code_catalog() {
        let catalog = parse_catalog(FULL_CATALOG).unwrap();
        assert_eq!(catalog.name, "company-tools");
        assert_eq!(catalog.owner.name, "DevTools Team");
        assert_eq!(catalog.owner.email.as_deref(), Some("devtools@example.com"));
        assert_eq!(catalog.description(), Some("meta desc"));
        assert_eq!(catalog.metadata.plugin_root.as_deref(), Some("./plugins"));
        assert_eq!(catalog.plugins.len(), 8);

        let formatter = catalog.plugin("code-formatter").unwrap();
        assert_eq!(
            formatter.source,
            PluginSource::Relative("./plugins/formatter".to_string())
        );
        assert_eq!(
            formatter.description.as_deref(),
            Some("Automatic code formatting on save")
        );

        let deploy = catalog.plugin("deployment-tools").unwrap();
        assert_eq!(
            deploy.source,
            PluginSource::Remote(RemotePluginSource::Github {
                repo: "company/deploy-plugin".to_string(),
                git_ref: None,
                sha: None,
            })
        );

        let pinned = catalog.plugin("pinned").unwrap();
        assert_eq!(
            pinned.source,
            PluginSource::Remote(RemotePluginSource::Github {
                repo: "company/pinned".to_string(),
                git_ref: Some("v2".to_string()),
                sha: Some("abc123".to_string()),
            })
        );

        let gitlab = catalog.plugin("gitlab-plugin").unwrap();
        assert!(matches!(
            gitlab.source,
            PluginSource::Remote(RemotePluginSource::Url { .. })
        ));

        let monorepo = catalog.plugin("monorepo-plugin").unwrap();
        assert!(matches!(
            &monorepo.source,
            PluginSource::Remote(RemotePluginSource::GitSubdir { path, .. })
                if path == "tools/plugin"
        ));

        let npm = catalog.plugin("npm-plugin").unwrap();
        assert!(matches!(
            npm.source,
            PluginSource::Remote(RemotePluginSource::Npm { .. })
        ));

        let custom = catalog.plugin("custom-skills").unwrap();
        let paths = custom.skills.clone().unwrap().into_vec();
        assert_eq!(paths, vec!["./skills/code-review", "./skills/docs"]);

        let single = catalog.plugin("single-skill-path").unwrap();
        assert_eq!(
            single.skills.clone().unwrap().into_vec(),
            vec!["./extra-skills/"]
        );
    }

    #[test]
    fn top_level_description_wins_over_metadata_description() {
        let catalog = parse_catalog(
            r#"{
                "name": "m", "owner": {"name": "o"}, "plugins": [],
                "description": "top", "metadata": {"description": "meta"}
            }"#,
        )
        .unwrap();
        assert_eq!(catalog.description(), Some("top"));
    }

    #[test]
    fn minimal_catalog_with_empty_plugins_is_valid() {
        let catalog =
            parse_catalog(r#"{"name": "m", "owner": {"name": "o"}, "plugins": []}"#).unwrap();
        assert!(catalog.plugins.is_empty());
        assert_eq!(catalog.description(), None);
    }

    #[test]
    fn missing_owner_is_a_parse_error() {
        let result = parse_catalog(r#"{"name": "m", "plugins": []}"#);
        assert!(matches!(result, Err(CatalogError::Parse(_))));
    }

    #[test]
    fn marketplace_name_with_at_sign_is_rejected() {
        let result =
            parse_catalog(r#"{"name": "bad@name", "owner": {"name": "o"}, "plugins": []}"#);
        assert!(matches!(
            result,
            Err(CatalogError::InvalidMarketplaceName(_))
        ));
    }

    #[test]
    fn marketplace_name_with_space_is_rejected() {
        let result =
            parse_catalog(r#"{"name": "bad name", "owner": {"name": "o"}, "plugins": []}"#);
        assert!(matches!(
            result,
            Err(CatalogError::InvalidMarketplaceName(_))
        ));
    }

    #[test]
    fn plugin_name_with_separator_is_rejected() {
        let result = parse_catalog(
            r#"{"name": "m", "owner": {"name": "o"}, "plugins": [{"name": "a/b", "source": "./x"}]}"#,
        );
        assert!(matches!(result, Err(CatalogError::InvalidPluginName(_))));
    }

    #[test]
    fn relative_source_strips_dot_slash() {
        assert_eq!(
            resolve_relative_source("./plugins/formatter", None).unwrap(),
            "plugins/formatter"
        );
    }

    #[test]
    fn bare_source_joins_plugin_root() {
        assert_eq!(
            resolve_relative_source("formatter", Some("./plugins")).unwrap(),
            "plugins/formatter"
        );
    }

    #[test]
    fn bare_source_without_plugin_root_is_rejected() {
        let result = resolve_relative_source("formatter", None);
        assert!(matches!(
            result,
            Err(CatalogError::InvalidRelativeSource(_))
        ));
    }

    #[test]
    fn source_path_escaping_the_marketplace_root_is_rejected() {
        assert!(matches!(
            resolve_relative_source("./../outside", None),
            Err(CatalogError::UnsafeSourcePath(_))
        ));
        assert!(matches!(
            resolve_relative_source("../../outside", Some("./plugins")),
            Err(CatalogError::UnsafeSourcePath(_))
        ));
    }

    #[test]
    fn join_repo_path_handles_empty_base() {
        assert_eq!(join_repo_path("", "plugins/foo"), "plugins/foo");
        assert_eq!(join_repo_path("mkt", "plugins/foo"), "mkt/plugins/foo");
        assert_eq!(join_repo_path("mkt/", "plugins/foo"), "mkt/plugins/foo");
    }
}
