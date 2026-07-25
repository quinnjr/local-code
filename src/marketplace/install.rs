use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::config::paths::Paths;
use crate::marketplace::catalog::{
    CatalogError, CatalogPlugin, MarketplaceCatalog, PluginSource, RemotePluginSource,
    join_repo_path, resolve_relative_source,
};
use crate::marketplace::registry::{RegisteredMarketplace, RegistryError};
use crate::marketplace::source::{AddSpecError, GitUrlError, MarketplaceSource, parse_git_url};
use crate::skills::client::SkillClient;
use crate::skills::install::{
    InstallError, install_resolved_files, remove_skill, swap_skill_files,
};
use crate::skills::types::{
    FetchedFile, Host, InstalledSkillManifest, PluginProvenance, Scope, SkillHostError, SkillSource,
};

#[derive(Debug, thiserror::Error)]
pub enum MarketplaceError {
    #[error(transparent)]
    Host(#[from] SkillHostError),
    #[error(transparent)]
    Catalog(#[from] CatalogError),
    #[error(transparent)]
    Install(#[from] InstallError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    AddSpec(#[from] AddSpecError),
    #[error(transparent)]
    GitUrl(#[from] GitUrlError),
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("no .claude-plugin/marketplace.json found in {0}")]
    CatalogMissing(String),
    #[error("marketplace '{marketplace}' has no plugin named '{plugin}'")]
    UnknownPlugin { marketplace: String, plugin: String },
    #[error(
        "plugin '{plugin}' uses a {kind} source, which LocalCode does not support (git-hosted and local-path sources only)"
    )]
    UnsupportedSource { plugin: String, kind: &'static str },
    #[error(
        "plugin '{0}' contains no skills (LocalCode installs plugin skills only — commands, agents, hooks, and MCP servers in plugins are not supported)"
    )]
    NoSkills(String),
    #[error("plugin '{plugin}' directory '{path}' not found in the marketplace")]
    PluginDirMissing { plugin: String, path: String },
    #[error("invalid skill directory name '{0}' in plugin (no spaces, '@', '/', '\\\\', or '..')")]
    InvalidSkillName(String),
    #[error("no plugin named '{0}' is installed in this scope")]
    PluginNotInstalled(String),
    #[error("invalid github repo '{0}' (expected owner/repo)")]
    InvalidGithubRepo(String),
    #[error("failed to build host client: {0}")]
    ClientBuild(String),
}

fn io_err(path: &Path, source: std::io::Error) -> MarketplaceError {
    MarketplaceError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Builds a `SkillClient` for a host on demand. A plugin's source host can
/// differ from its marketplace's host (a GitLab marketplace can list
/// GitHub-sourced plugins), so clients are constructed per fetch target
/// rather than once per command. Injected so tests can point clients at a
/// mock server.
pub type ClientFor<'a> = dyn Fn(Host) -> Result<SkillClient, MarketplaceError> + 'a;

/// The pin a fetched catalog was resolved at, carried into plugin-content
/// fetches so a plugin's files and the catalog that listed them come from
/// the same commit (remote marketplaces only — local marketplaces are read
/// live from disk).
pub struct CatalogPin {
    pub commit_sha: Option<String>,
}

/// Fetches, parses, and validates a registered marketplace's
/// `.claude-plugin/marketplace.json`. Catalogs are fetched fresh on every
/// operation (no persistent clone/cache the way Claude Code keeps one) —
/// the registry only records *where* the catalog lives.
pub async fn fetch_catalog(
    source: &MarketplaceSource,
    client_for: &ClientFor<'_>,
) -> Result<(MarketplaceCatalog, CatalogPin), MarketplaceError> {
    match source {
        MarketplaceSource::Remote {
            host,
            owner,
            repo,
            path,
            git_ref,
        } => {
            let client = client_for(*host)?;
            let commit_sha = client.resolve_commit_sha(owner, repo, git_ref).await?;
            let dir = join_repo_path(path, ".claude-plugin");
            let files = client
                .fetch_directory_files(owner, repo, &dir, &commit_sha)
                .await?;
            let catalog_file = files
                .iter()
                .find(|f| f.relative_path == Path::new("marketplace.json"))
                .ok_or_else(|| MarketplaceError::CatalogMissing(source.summary()))?;
            let text = String::from_utf8_lossy(&catalog_file.bytes);
            let catalog = crate::marketplace::catalog::parse_catalog(&text)?;
            Ok((
                catalog,
                CatalogPin {
                    commit_sha: Some(commit_sha),
                },
            ))
        }
        MarketplaceSource::Local { path } => {
            let catalog_path = path.join(".claude-plugin").join("marketplace.json");
            let text = std::fs::read_to_string(&catalog_path).map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    MarketplaceError::CatalogMissing(source.summary())
                } else {
                    io_err(&catalog_path, e)
                }
            })?;
            let catalog = crate::marketplace::catalog::parse_catalog(&text)?;
            Ok((catalog, CatalogPin { commit_sha: None }))
        }
    }
}

/// Where an installed skill's manifest points: the coordinates of the repo
/// (or local directory) the plugin's content was fetched from, so later
/// operations can re-resolve it. For local marketplaces the host fields are
/// placeholders — `skills update` skips plugin-managed skills entirely, and
/// `plugin update` re-reads the marketplace rather than these coordinates.
struct ManifestCoords {
    host: Host,
    owner: String,
    repo: String,
    /// In-repo path of the plugin root (local marketplaces: the absolute
    /// filesystem path, purely informational).
    plugin_path: String,
    git_ref: String,
    commit_sha: String,
    is_local: bool,
}

struct PluginContent {
    /// Every file under the plugin root, paths relative to that root.
    files: Vec<FetchedFile>,
    coords: ManifestCoords,
}

async fn fetch_hosted_plugin(
    mut source: SkillSource,
    subdir: &str,
    sha: Option<&str>,
    client_for: &ClientFor<'_>,
) -> Result<PluginContent, MarketplaceError> {
    let client = client_for(source.host)?;
    // An explicit `sha` is the effective pin (Claude Code: when both `ref`
    // and `sha` are set, `sha` wins), and it becomes the manifest's ref so
    // the pin is stable across updates.
    let git_ref = match (sha, source.git_ref.take()) {
        (Some(sha), _) => sha.to_string(),
        (None, Some(r)) => r,
        (None, None) => {
            client
                .resolve_default_branch(&source.owner, &source.repo)
                .await?
        }
    };
    let commit_sha = match sha {
        Some(s) => s.to_string(),
        None => {
            client
                .resolve_commit_sha(&source.owner, &source.repo, &git_ref)
                .await?
        }
    };
    let files = client
        .fetch_directory_files(&source.owner, &source.repo, subdir, &commit_sha)
        .await?;
    Ok(PluginContent {
        files,
        coords: ManifestCoords {
            host: source.host,
            owner: source.owner,
            repo: source.repo,
            plugin_path: subdir.to_string(),
            git_ref,
            commit_sha,
            is_local: false,
        },
    })
}

fn github_repo_coords(plugin_name: &str, repo: &str) -> Result<(String, String), MarketplaceError> {
    repo.split_once('/')
        .filter(|(owner, name)| !owner.is_empty() && !name.is_empty() && !name.contains('/'))
        .map(|(owner, name)| (owner.to_string(), name.to_string()))
        .ok_or_else(|| MarketplaceError::InvalidGithubRepo(format!("{plugin_name}: {repo}")))
}

async fn fetch_remote_plugin(
    plugin_name: &str,
    remote: &RemotePluginSource,
    client_for: &ClientFor<'_>,
) -> Result<PluginContent, MarketplaceError> {
    match remote {
        RemotePluginSource::Github { repo, git_ref, sha } => {
            let (owner, name) = github_repo_coords(plugin_name, repo)?;
            let source = SkillSource {
                host: Host::GitHub,
                owner,
                repo: name,
                path: String::new(),
                git_ref: git_ref.clone(),
            };
            fetch_hosted_plugin(source, "", sha.as_deref(), client_for).await
        }
        RemotePluginSource::Url { url, git_ref, sha } => {
            let mut source = parse_git_url(url)?;
            source.git_ref = git_ref.clone();
            fetch_hosted_plugin(source, "", sha.as_deref(), client_for).await
        }
        RemotePluginSource::GitSubdir {
            url,
            path,
            git_ref,
            sha,
        } => {
            let mut source = parse_git_url(url)?;
            source.git_ref = git_ref.clone();
            let subdir = crate::marketplace::catalog::validate_in_repo_path(path)?;
            fetch_hosted_plugin(source, &subdir, sha.as_deref(), client_for).await
        }
        RemotePluginSource::Npm { .. } => Err(MarketplaceError::UnsupportedSource {
            plugin: plugin_name.to_string(),
            kind: "npm",
        }),
    }
}

async fn fetch_plugin_content(
    marketplace: &RegisteredMarketplace,
    catalog: &MarketplaceCatalog,
    entry: &CatalogPlugin,
    pin: &CatalogPin,
    client_for: &ClientFor<'_>,
) -> Result<PluginContent, MarketplaceError> {
    match &entry.source {
        PluginSource::Relative(src) => {
            let rel = resolve_relative_source(src, catalog.metadata.plugin_root.as_deref())?;
            match &marketplace.source {
                MarketplaceSource::Remote {
                    host,
                    owner,
                    repo,
                    path,
                    git_ref,
                } => {
                    let repo_path = join_repo_path(path, &rel);
                    let client = client_for(*host)?;
                    let commit_sha = pin
                        .commit_sha
                        .clone()
                        .expect("remote marketplaces always carry a pin");
                    let files = client
                        .fetch_directory_files(owner, repo, &repo_path, &commit_sha)
                        .await?;
                    Ok(PluginContent {
                        files,
                        coords: ManifestCoords {
                            host: *host,
                            owner: owner.clone(),
                            repo: repo.clone(),
                            plugin_path: repo_path,
                            git_ref: git_ref.clone(),
                            commit_sha,
                            is_local: false,
                        },
                    })
                }
                MarketplaceSource::Local { path } => {
                    let dir = rel.split('/').fold(path.clone(), |acc, seg| acc.join(seg));
                    if !dir.is_dir() {
                        return Err(MarketplaceError::PluginDirMissing {
                            plugin: entry.name.clone(),
                            path: dir.display().to_string(),
                        });
                    }
                    Ok(PluginContent {
                        files: walk_local_dir(&dir)?,
                        coords: ManifestCoords {
                            host: Host::default(),
                            owner: String::new(),
                            repo: String::new(),
                            plugin_path: dir.display().to_string(),
                            git_ref: String::new(),
                            commit_sha: String::new(),
                            is_local: true,
                        },
                    })
                }
            }
        }
        PluginSource::Remote(remote) => fetch_remote_plugin(&entry.name, remote, client_for).await,
    }
}

/// Recursively collects a local plugin directory's files, paths relative to
/// `root`. Symlinks are skipped (not followed), matching the host clients'
/// treatment of non-file/non-dir entries.
fn walk_local_dir(root: &Path) -> Result<Vec<FetchedFile>, MarketplaceError> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir).map_err(|e| io_err(&dir, e))?;
        for entry in entries {
            let entry = entry.map_err(|e| io_err(&dir, e))?;
            let path = entry.path();
            let file_type = entry.file_type().map_err(|e| io_err(&path, e))?;
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() {
                let relative_path = path
                    .strip_prefix(root)
                    .expect("walked entries are always under root")
                    .to_path_buf();
                let bytes = std::fs::read(&path).map_err(|e| io_err(&path, e))?;
                files.push(FetchedFile {
                    relative_path,
                    bytes,
                });
            }
        }
    }
    files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(files)
}

/// The plugin-root-relative directories that contain a skill (`SKILL.md` or
/// `SKILL.mdc` directly inside). Default: the immediate subdirectories of
/// the plugin's `skills/` dir. When the catalog entry lists custom `skills`
/// paths, each listed path is either taken as a skill itself (if it holds a
/// skill file) or scanned for immediate skill subdirectories; if none of
/// the listed paths yield anything, the default scan runs instead (the
/// fallback Claude Code documents).
fn find_skill_dirs(files: &[FetchedFile], entry: &CatalogPlugin) -> Vec<String> {
    let paths: BTreeSet<String> = files
        .iter()
        .filter_map(|f| f.relative_path.to_str().map(str::to_string))
        .collect();
    let has_skill_file = |dir: &str| {
        paths.contains(&format!("{dir}/SKILL.md")) || paths.contains(&format!("{dir}/SKILL.mdc"))
    };
    // Immediate subdirectories of `root` that contain a skill file.
    let skill_subdirs_of = |root: &str| -> Vec<String> {
        let prefix = format!("{root}/");
        paths
            .iter()
            .filter_map(|p| {
                let rest = p.strip_prefix(&prefix)?;
                let (first, _) = rest.split_once('/')?;
                let candidate = format!("{prefix}{first}");
                has_skill_file(&candidate).then_some(candidate)
            })
            .collect()
    };

    let mut dirs = Vec::new();
    if let Some(skills_field) = &entry.skills {
        for raw in skills_field.clone().into_vec() {
            let listed = raw.trim_start_matches("./").trim_matches('/');
            // "./" (the plugin root) and "./skills/" both mean the default
            // scan (Claude Code: listing them "keeps the full scan").
            if listed.is_empty() || listed == "skills" {
                dirs.extend(skill_subdirs_of("skills"));
            } else if has_skill_file(listed) {
                dirs.push(listed.to_string());
            } else {
                dirs.extend(skill_subdirs_of(listed));
            }
        }
    }
    if dirs.is_empty() {
        dirs = skill_subdirs_of("skills");
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

fn skill_name_of(dir: &str) -> Result<String, MarketplaceError> {
    let name = dir
        .rsplit('/')
        .next()
        .expect("a non-empty dir always has a last segment");
    if !crate::marketplace::catalog::valid_component_name(name) {
        return Err(MarketplaceError::InvalidSkillName(name.to_string()));
    }
    Ok(name.to_string())
}

/// The files of one skill directory, re-relativized to the skill dir itself
/// (the shape `write_files` expects).
fn files_under(files: &[FetchedFile], dir: &str) -> Vec<FetchedFile> {
    let prefix = format!("{dir}/");
    files
        .iter()
        .filter_map(|f| {
            let path = f.relative_path.to_str()?;
            Some(FetchedFile {
                relative_path: PathBuf::from(path.strip_prefix(&prefix)?),
                bytes: f.bytes.clone(),
            })
        })
        .collect()
}

fn skill_manifest(
    content: &PluginContent,
    skill_dir: &str,
    marketplace: &str,
    plugin: &str,
) -> InstalledSkillManifest {
    InstalledSkillManifest {
        host: content.coords.host,
        owner: content.coords.owner.clone(),
        repo: content.coords.repo.clone(),
        path: join_repo_path(&content.coords.plugin_path, skill_dir),
        git_ref: content.coords.git_ref.clone(),
        commit_sha: content.coords.commit_sha.clone(),
        plugin: Some(PluginProvenance {
            marketplace: marketplace.to_string(),
            plugin: plugin.to_string(),
        }),
    }
}

/// One skill dir of a plugin, ready to install: its install name, its files
/// (re-relativized), and the manifest to record.
struct PlannedSkill {
    name: String,
    files: Vec<FetchedFile>,
    manifest: InstalledSkillManifest,
}

fn plan_plugin_skills(
    content: &PluginContent,
    entry: &CatalogPlugin,
    marketplace: &str,
) -> Result<Vec<PlannedSkill>, MarketplaceError> {
    let skill_dirs = find_skill_dirs(&content.files, entry);
    if skill_dirs.is_empty() {
        return Err(MarketplaceError::NoSkills(entry.name.clone()));
    }
    skill_dirs
        .iter()
        .map(|dir| {
            Ok(PlannedSkill {
                name: skill_name_of(dir)?,
                files: files_under(&content.files, dir),
                manifest: skill_manifest(content, dir, marketplace, &entry.name),
            })
        })
        .collect()
}

/// Installs every skill of `plugin_name` from `marketplace` into `scope`,
/// as ordinary skill directories whose manifests record the plugin they
/// came from. Returns the installed skill names. Validates all skill names
/// and target directories up front so a conflicting or malformed plugin
/// fails before anything is written.
pub async fn install_plugin(
    paths: &Paths,
    scope: Scope,
    marketplace: &RegisteredMarketplace,
    plugin_name: &str,
    client_for: &ClientFor<'_>,
) -> Result<Vec<String>, MarketplaceError> {
    let (catalog, pin) = fetch_catalog(&marketplace.source, client_for).await?;
    let entry = catalog
        .plugin(plugin_name)
        .ok_or_else(|| MarketplaceError::UnknownPlugin {
            marketplace: marketplace.name.clone(),
            plugin: plugin_name.to_string(),
        })?;
    let content = fetch_plugin_content(marketplace, &catalog, entry, &pin, client_for).await?;
    let planned = plan_plugin_skills(&content, entry, &marketplace.name)?;

    for skill in &planned {
        if crate::skills::install::skill_is_installed(paths, scope, &skill.name) {
            return Err(InstallError::AlreadyInstalled(skill.name.clone()).into());
        }
    }

    let mut installed = Vec::with_capacity(planned.len());
    for skill in &planned {
        install_resolved_files(paths, scope, &skill.name, &skill.files, &skill.manifest)?;
        installed.push(skill.name.clone());
    }
    Ok(installed)
}

/// What `update_plugin` did, per skill.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PluginUpdateReport {
    /// Existing skills whose files were swapped for the fresh fetch.
    pub updated: Vec<String>,
    /// Skill dirs the plugin gained since install, now installed.
    pub added: Vec<String>,
    /// Installed skills whose dirs no longer exist in the plugin, removed.
    pub removed: Vec<String>,
    /// Skills already at the resolved commit (remote plugins only — local
    /// marketplaces are always re-copied).
    pub up_to_date: Vec<String>,
}

/// Re-resolves a plugin through its (freshly fetched) marketplace catalog
/// and converges the installed skills on the plugin's current skill set:
/// swaps changed skills, installs newly-added ones, removes vanished ones.
pub async fn update_plugin(
    paths: &Paths,
    scope: Scope,
    marketplace: &RegisteredMarketplace,
    plugin_name: &str,
    client_for: &ClientFor<'_>,
) -> Result<PluginUpdateReport, MarketplaceError> {
    let installed = installed_skill_manifests(paths, scope)?
        .into_iter()
        .filter(|(_, manifest)| {
            manifest
                .plugin
                .as_ref()
                .is_some_and(|p| p.marketplace == marketplace.name && p.plugin == plugin_name)
        })
        .collect::<HashMap<_, _>>();
    if installed.is_empty() {
        return Err(MarketplaceError::PluginNotInstalled(
            plugin_name.to_string(),
        ));
    }

    let (catalog, pin) = fetch_catalog(&marketplace.source, client_for).await?;
    let entry = catalog
        .plugin(plugin_name)
        .ok_or_else(|| MarketplaceError::UnknownPlugin {
            marketplace: marketplace.name.clone(),
            plugin: plugin_name.to_string(),
        })?;
    let content = fetch_plugin_content(marketplace, &catalog, entry, &pin, client_for).await?;
    let planned = plan_plugin_skills(&content, entry, &marketplace.name)?;

    let mut report = PluginUpdateReport::default();
    let mut fresh_names = BTreeSet::new();
    for skill in &planned {
        fresh_names.insert(skill.name.clone());
        match installed.get(&skill.name) {
            Some(existing)
                if !content.coords.is_local
                    && existing.commit_sha == content.coords.commit_sha
                    && existing.git_ref == content.coords.git_ref =>
            {
                report.up_to_date.push(skill.name.clone());
            }
            Some(_) => {
                swap_skill_files(paths, scope, &skill.name, &skill.files, &skill.manifest)?;
                report.updated.push(skill.name.clone());
            }
            None => {
                install_resolved_files(paths, scope, &skill.name, &skill.files, &skill.manifest)?;
                report.added.push(skill.name.clone());
            }
        }
    }
    for name in installed.keys() {
        if !fresh_names.contains(name) {
            remove_skill(paths, scope, name)?;
            report.removed.push(name.clone());
        }
    }
    Ok(report)
}

/// Removes every skill installed from `plugin_name` (any marketplace) in
/// `scope`. Returns the removed skill names.
pub fn remove_plugin(
    paths: &Paths,
    scope: Scope,
    plugin_name: &str,
) -> Result<Vec<String>, MarketplaceError> {
    let mut removed = Vec::new();
    for (name, manifest) in installed_skill_manifests(paths, scope)? {
        if manifest
            .plugin
            .as_ref()
            .is_some_and(|p| p.plugin == plugin_name)
        {
            remove_skill(paths, scope, &name)?;
            removed.push(name);
        }
    }
    if removed.is_empty() {
        return Err(MarketplaceError::PluginNotInstalled(
            plugin_name.to_string(),
        ));
    }
    removed.sort();
    Ok(removed)
}

/// One row of `plugin list` output: a plugin offered by a registered
/// marketplace, with the scopes it's currently installed in (if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailablePlugin {
    pub marketplace: String,
    pub name: String,
    pub description: Option<String>,
    pub installed_scopes: Vec<Scope>,
}

/// Lists every plugin offered by every registered marketplace. A marketplace
/// whose catalog fails to fetch is skipped with a warning (mirroring
/// `mcp::connect::connect_all`'s failure tolerance) so one broken source
/// doesn't hide the rest.
pub async fn list_available_plugins(
    paths: &Paths,
    marketplaces: &[RegisteredMarketplace],
    client_for: &ClientFor<'_>,
) -> Vec<AvailablePlugin> {
    let installed = installed_plugin_scopes(paths);
    let mut available = Vec::new();
    for marketplace in marketplaces {
        let (catalog, _) = match fetch_catalog(&marketplace.source, client_for).await {
            Ok(result) => result,
            Err(e) => {
                eprintln!("warning: skipping marketplace '{}': {e}", marketplace.name);
                continue;
            }
        };
        for plugin in &catalog.plugins {
            available.push(AvailablePlugin {
                marketplace: marketplace.name.clone(),
                name: plugin.name.clone(),
                description: plugin.description.clone(),
                installed_scopes: installed
                    .get(&(marketplace.name.clone(), plugin.name.clone()))
                    .cloned()
                    .unwrap_or_default(),
            });
        }
    }
    available
}

/// The distinct marketplace names recorded in the provenance of this
/// scope's installed skills for `plugin_name` (almost always one;
/// `plugin update <name>` routes through it to re-fetch from the right
/// marketplace rather than asking the user for `<plugin>@<marketplace>`).
pub fn installed_plugin_marketplaces(
    paths: &Paths,
    scope: Scope,
    plugin_name: &str,
) -> Vec<String> {
    let mut names = Vec::new();
    for (_, manifest) in installed_skill_manifests(paths, scope).unwrap_or_default() {
        if let Some(p) = manifest.plugin
            && p.plugin == plugin_name
            && !names.contains(&p.marketplace)
        {
            names.push(p.marketplace);
        }
    }
    names
}

/// Maps `(marketplace, plugin)` to the scopes that have at least one of the
/// plugin's skills installed, for `plugin list`'s installed markers.
fn installed_plugin_scopes(paths: &Paths) -> HashMap<(String, String), Vec<Scope>> {
    let mut map: HashMap<(String, String), Vec<Scope>> = HashMap::new();
    for scope in [Scope::Project, Scope::Global] {
        for (_, manifest) in installed_skill_manifests(paths, scope).unwrap_or_default() {
            if let Some(p) = manifest.plugin {
                let scopes = map.entry((p.marketplace, p.plugin)).or_default();
                if !scopes.contains(&scope) {
                    scopes.push(scope);
                }
            }
        }
    }
    map
}

/// Reads the manifest of every skill in `scope` that has a parseable one
/// (dirs with missing/corrupt manifests are skipped with a warning, the
/// same tolerance `list_skills` shows). Keyed by skill directory name.
fn installed_skill_manifests(
    paths: &Paths,
    scope: Scope,
) -> Result<HashMap<String, InstalledSkillManifest>, MarketplaceError> {
    let dir = match scope {
        Scope::Project => paths.project_config_dir.join("skills"),
        Scope::Global => paths.user_config_dir.join("skills"),
    };
    let mut manifests = HashMap::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(manifests);
    };
    for entry in entries.flatten() {
        let skill_dir = entry.path();
        if !skill_dir.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        match crate::skills::install::read_skill_manifest(paths, scope, &name) {
            Ok(manifest) => {
                manifests.insert(name, manifest);
            }
            Err(e) => {
                eprintln!("warning: skipping skill at {}: {e}", skill_dir.display());
            }
        }
    }
    Ok(manifests)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::source::MarketplaceSource;
    use crate::skills::github::GithubClient;
    use tempfile::tempdir;
    use wiremock::matchers::{method, path as wpath};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_paths(root: &Path) -> Paths {
        Paths {
            user_config_dir: root.join("user-config"),
            project_config_dir: root.join("project/.local-code"),
            user_state_dir: root.join("user-state"),
        }
    }

    fn github_marketplace(name: &str) -> RegisteredMarketplace {
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

    fn client_for(
        server: &MockServer,
    ) -> impl Fn(Host) -> Result<SkillClient, MarketplaceError> + '_ {
        move |host| {
            assert_eq!(host, Host::GitHub);
            Ok(SkillClient::GitHub(GithubClient::new_for_test(
                None,
                server.uri(),
            )))
        }
    }

    const CATALOG_JSON: &str = r#"{
        "name": "acme-tools",
        "owner": {"name": "Acme"},
        "plugins": [
            {"name": "foo", "source": "./plugins/foo", "description": "Foo plugin"},
            {"name": "gadget", "source": {"source": "github", "repo": "acme/gadgets"}},
            {"name": "pinned", "source": {"source": "github", "repo": "acme/gadgets", "sha": "deadbeef"}},
            {"name": "npm-thing", "source": {"source": "npm", "package": "@acme/x"}},
            {"name": "no-skills", "source": "./plugins/empty"},
            {"name": "custom", "source": "./plugins/custom", "skills": ["./extra/review"]}
        ]
    }"#;

    /// Mounts the catalog endpoints for the `acme/plugins` marketplace repo.
    async fn mount_catalog(server: &MockServer, sha: &str) {
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/plugins/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"sha": sha})))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/plugins/contents/.claude-plugin"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "marketplace.json", "path": ".claude-plugin/marketplace.json", "type": "file",
                 "download_url": format!("{}/raw/marketplace.json", server.uri())}
            ])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/raw/marketplace.json"))
            .respond_with(ResponseTemplate::new(200).set_body_string(CATALOG_JSON))
            .mount(server)
            .await;
    }

    /// Mounts the `plugins/foo` plugin content: one skill `bar` with two files.
    async fn mount_foo_plugin(server: &MockServer) {
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/plugins/contents/plugins/foo"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "plugins/foo/skills", "type": "dir"}
            ])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/plugins/contents/plugins/foo/skills"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "plugins/foo/skills/bar", "type": "dir"}
            ])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/plugins/contents/plugins/foo/skills/bar"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "plugins/foo/skills/bar/SKILL.md", "type": "file",
                 "download_url": format!("{}/raw/bar-SKILL.md", server.uri())},
                {"name": "notes.txt", "path": "plugins/foo/skills/bar/notes.txt", "type": "file",
                 "download_url": format!("{}/raw/bar-notes.txt", server.uri())}
            ])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/raw/bar-SKILL.md"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("---\nname: bar\ndescription: d\n---\nbody"),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/raw/bar-notes.txt"))
            .respond_with(ResponseTemplate::new(200).set_body_string("notes"))
            .mount(server)
            .await;
    }

    /// Mounts the `acme/gadgets` repo (plugin source `github`/`url`): one
    /// skill `wid` at the repo root's `skills/` dir.
    async fn mount_gadgets_repo(server: &MockServer) {
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/gadgets"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"default_branch": "main"})),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/gadgets/commits/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"sha": "g1"})),
            )
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/gadgets/contents/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "skills", "type": "dir"}
            ])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/gadgets/contents/skills"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "skills/wid", "type": "dir"}
            ])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/gadgets/contents/skills/wid"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "skills/wid/SKILL.md", "type": "file",
                 "download_url": format!("{}/raw/wid-SKILL.md", server.uri())}
            ])))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/raw/wid-SKILL.md"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("---\nname: wid\ndescription: d\n---\nbody"),
            )
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn installs_a_relative_source_plugin_from_a_remote_marketplace() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        mount_catalog(&server, "mkt1").await;
        mount_foo_plugin(&server).await;
        let marketplace = github_marketplace("acme-tools");

        let installed = install_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "foo",
            &client_for(&server),
        )
        .await
        .unwrap();

        assert_eq!(installed, vec!["bar"]);
        let skill_dir = paths.project_config_dir.join("skills/bar");
        assert!(skill_dir.join("SKILL.md").exists());
        assert!(skill_dir.join("notes.txt").exists());
        let manifest =
            crate::skills::install::read_skill_manifest(&paths, Scope::Project, "bar").unwrap();
        assert_eq!(manifest.host, Host::GitHub);
        assert_eq!(manifest.owner, "acme");
        assert_eq!(manifest.repo, "plugins");
        assert_eq!(manifest.path, "plugins/foo/skills/bar");
        assert_eq!(manifest.git_ref, "main");
        assert_eq!(manifest.commit_sha, "mkt1");
        assert_eq!(
            manifest.plugin,
            Some(PluginProvenance {
                marketplace: "acme-tools".into(),
                plugin: "foo".into(),
            })
        );
    }

    #[tokio::test]
    async fn installs_a_github_source_plugin_resolving_its_default_branch() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        mount_catalog(&server, "mkt1").await;
        mount_gadgets_repo(&server).await;
        let marketplace = github_marketplace("acme-tools");

        let installed = install_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "gadget",
            &client_for(&server),
        )
        .await
        .unwrap();

        assert_eq!(installed, vec!["wid"]);
        let manifest =
            crate::skills::install::read_skill_manifest(&paths, Scope::Project, "wid").unwrap();
        assert_eq!(manifest.owner, "acme");
        assert_eq!(manifest.repo, "gadgets");
        assert_eq!(manifest.path, "skills/wid");
        assert_eq!(manifest.git_ref, "main");
        assert_eq!(manifest.commit_sha, "g1");
    }

    #[tokio::test]
    async fn a_sha_pinned_plugin_source_skips_ref_resolution() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        mount_catalog(&server, "mkt1").await;
        // Only the catalog endpoints plus the gadgets *content* endpoints are
        // mounted — no `/repos/acme/gadgets` (default branch) or `commits/*`
        // mocks, so the pinned sha must be used as-is. Mount the content
        // endpoints without the commit-resolution ones.
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/gadgets/contents/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "skills", "type": "dir"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/gadgets/contents/skills"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "skills/wid", "type": "dir"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/gadgets/contents/skills/wid"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "skills/wid/SKILL.md", "type": "file",
                 "download_url": format!("{}/raw/wid-SKILL.md", server.uri())}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/raw/wid-SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("pinned"))
            .mount(&server)
            .await;
        let marketplace = github_marketplace("acme-tools");

        let installed = install_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "pinned",
            &client_for(&server),
        )
        .await
        .unwrap();

        assert_eq!(installed, vec!["wid"]);
        let manifest =
            crate::skills::install::read_skill_manifest(&paths, Scope::Project, "wid").unwrap();
        assert_eq!(manifest.git_ref, "deadbeef");
        assert_eq!(manifest.commit_sha, "deadbeef");
    }

    #[tokio::test]
    async fn npm_sources_are_rejected_as_unsupported() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        mount_catalog(&server, "mkt1").await;
        let marketplace = github_marketplace("acme-tools");

        let result = install_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "npm-thing",
            &client_for(&server),
        )
        .await;
        assert!(matches!(
            result,
            Err(MarketplaceError::UnsupportedSource { kind: "npm", .. })
        ));
    }

    #[tokio::test]
    async fn a_plugin_with_no_skills_is_rejected() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        mount_catalog(&server, "mkt1").await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/plugins/contents/plugins/empty"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "README.md", "path": "plugins/empty/README.md", "type": "file",
                 "download_url": format!("{}/raw/empty-readme", server.uri())}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/raw/empty-readme"))
            .respond_with(ResponseTemplate::new(200).set_body_string("no skills here"))
            .mount(&server)
            .await;
        let marketplace = github_marketplace("acme-tools");

        let result = install_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "no-skills",
            &client_for(&server),
        )
        .await;
        assert!(matches!(result, Err(MarketplaceError::NoSkills(_))));
    }

    #[tokio::test]
    async fn unknown_plugins_report_the_marketplace() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        mount_catalog(&server, "mkt1").await;
        let marketplace = github_marketplace("acme-tools");

        let result = install_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "nope",
            &client_for(&server),
        )
        .await;
        assert!(matches!(
            result,
            Err(MarketplaceError::UnknownPlugin { marketplace, plugin })
                if marketplace == "acme-tools" && plugin == "nope"
        ));
    }

    #[tokio::test]
    async fn a_conflicting_skill_name_fails_before_anything_is_written() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        // Pre-install an unrelated skill named `bar` in the target scope.
        std::fs::create_dir_all(paths.project_config_dir.join("skills/bar")).unwrap();
        let server = MockServer::start().await;
        mount_catalog(&server, "mkt1").await;
        mount_foo_plugin(&server).await;
        let marketplace = github_marketplace("acme-tools");

        let result = install_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "foo",
            &client_for(&server),
        )
        .await;
        assert!(matches!(
            result,
            Err(MarketplaceError::Install(InstallError::AlreadyInstalled(name))) if name == "bar"
        ));
    }

    #[tokio::test]
    async fn entry_skills_paths_point_directly_at_a_skill_dir() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        mount_catalog(&server, "mkt1").await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/plugins/contents/plugins/custom"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "plugins/custom/extra", "type": "dir"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/plugins/contents/plugins/custom/extra"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "plugins/custom/extra/review", "type": "dir"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath(
                "/repos/acme/plugins/contents/plugins/custom/extra/review",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "plugins/custom/extra/review/SKILL.md", "type": "file",
                 "download_url": format!("{}/raw/review-SKILL.md", server.uri())}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/raw/review-SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("review"))
            .mount(&server)
            .await;
        let marketplace = github_marketplace("acme-tools");

        let installed = install_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "custom",
            &client_for(&server),
        )
        .await
        .unwrap();

        assert_eq!(installed, vec!["review"]);
        let manifest =
            crate::skills::install::read_skill_manifest(&paths, Scope::Project, "review").unwrap();
        assert_eq!(manifest.path, "plugins/custom/extra/review");
    }

    #[tokio::test]
    async fn update_swaps_changed_skills_and_reports_noop_when_unmoved() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        mount_catalog(&server, "mkt1").await;
        mount_foo_plugin(&server).await;
        let marketplace = github_marketplace("acme-tools");
        install_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "foo",
            &client_for(&server),
        )
        .await
        .unwrap();

        // Move the marketplace ref: new sha, changed SKILL.md content.
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/plugins/commits/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"sha": "mkt2"})),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/plugins/contents/plugins/foo/skills/bar"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "plugins/foo/skills/bar/SKILL.md", "type": "file",
                 "download_url": format!("{}/raw/bar-SKILL2.md", server.uri())},
                {"name": "notes.txt", "path": "plugins/foo/skills/bar/notes.txt", "type": "file",
                 "download_url": format!("{}/raw/bar-notes.txt", server.uri())}
            ])))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/raw/bar-SKILL2.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("updated body"))
            .with_priority(1)
            .mount(&server)
            .await;

        let report = update_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "foo",
            &client_for(&server),
        )
        .await
        .unwrap();
        assert_eq!(report.updated, vec!["bar"]);
        assert!(
            report.added.is_empty() && report.removed.is_empty() && report.up_to_date.is_empty()
        );
        let content =
            std::fs::read_to_string(paths.project_config_dir.join("skills/bar/SKILL.md")).unwrap();
        assert_eq!(content, "updated body");
        let manifest =
            crate::skills::install::read_skill_manifest(&paths, Scope::Project, "bar").unwrap();
        assert_eq!(manifest.commit_sha, "mkt2");

        // Same sha now: the second update is a no-op.
        let report = update_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "foo",
            &client_for(&server),
        )
        .await
        .unwrap();
        assert_eq!(report.up_to_date, vec!["bar"]);
        assert!(report.updated.is_empty() && report.added.is_empty() && report.removed.is_empty());
    }

    #[tokio::test]
    async fn update_adds_new_skills_and_removes_vanished_ones() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        mount_catalog(&server, "mkt1").await;
        mount_foo_plugin(&server).await;
        let marketplace = github_marketplace("acme-tools");
        install_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "foo",
            &client_for(&server),
        )
        .await
        .unwrap();

        // Move the ref; the plugin now ships skill `baz` instead of `bar`.
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/plugins/commits/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"sha": "mkt2"})),
            )
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/plugins/contents/plugins/foo/skills"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "plugins/foo/skills/baz", "type": "dir"}
            ])))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/repos/acme/plugins/contents/plugins/foo/skills/baz"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "plugins/foo/skills/baz/SKILL.md", "type": "file",
                 "download_url": format!("{}/raw/baz-SKILL.md", server.uri())}
            ])))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(wpath("/raw/baz-SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("baz"))
            .with_priority(1)
            .mount(&server)
            .await;

        let report = update_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "foo",
            &client_for(&server),
        )
        .await
        .unwrap();
        assert_eq!(report.added, vec!["baz"]);
        assert_eq!(report.removed, vec!["bar"]);
        assert!(report.updated.is_empty() && report.up_to_date.is_empty());
        assert!(!paths.project_config_dir.join("skills/bar").exists());
        assert!(
            paths
                .project_config_dir
                .join("skills/baz/SKILL.md")
                .exists()
        );
    }

    #[tokio::test]
    async fn update_errors_when_the_plugin_is_not_installed() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        let marketplace = github_marketplace("acme-tools");
        let result = update_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "foo",
            &client_for(&server),
        )
        .await;
        assert!(matches!(
            result,
            Err(MarketplaceError::PluginNotInstalled(name)) if name == "foo"
        ));
    }

    #[tokio::test]
    async fn remove_plugin_deletes_every_skill_of_the_plugin() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        mount_catalog(&server, "mkt1").await;
        mount_foo_plugin(&server).await;
        let marketplace = github_marketplace("acme-tools");
        install_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "foo",
            &client_for(&server),
        )
        .await
        .unwrap();

        let removed = remove_plugin(&paths, Scope::Project, "foo").unwrap();
        assert_eq!(removed, vec!["bar"]);
        assert!(!paths.project_config_dir.join("skills/bar").exists());

        let result = remove_plugin(&paths, Scope::Project, "foo");
        assert!(matches!(
            result,
            Err(MarketplaceError::PluginNotInstalled(name)) if name == "foo"
        ));
    }

    #[tokio::test]
    async fn list_available_marks_installed_plugins_and_tolerates_a_broken_marketplace() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        mount_catalog(&server, "mkt1").await;
        mount_foo_plugin(&server).await;
        let marketplace = github_marketplace("acme-tools");
        install_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "foo",
            &client_for(&server),
        )
        .await
        .unwrap();

        let mut broken = github_marketplace("broken");
        broken.source = MarketplaceSource::Remote {
            host: Host::GitHub,
            owner: "no".into(),
            repo: "repo".into(),
            path: String::new(),
            git_ref: "main".into(),
        };
        let available =
            list_available_plugins(&paths, &[marketplace, broken], &client_for(&server)).await;

        let foo = available.iter().find(|p| p.name == "foo").unwrap();
        assert_eq!(foo.installed_scopes, vec![Scope::Project]);
        assert_eq!(foo.description.as_deref(), Some("Foo plugin"));
        let gadget = available.iter().find(|p| p.name == "gadget").unwrap();
        assert!(gadget.installed_scopes.is_empty());
        // The broken marketplace was skipped (with a warning), not fatal.
        assert!(available.iter().all(|p| p.marketplace == "acme-tools"));
    }

    #[tokio::test]
    async fn installed_plugin_marketplaces_routes_updates_by_provenance() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        mount_catalog(&server, "mkt1").await;
        mount_foo_plugin(&server).await;
        let marketplace = github_marketplace("acme-tools");
        install_plugin(
            &paths,
            Scope::Project,
            &marketplace,
            "foo",
            &client_for(&server),
        )
        .await
        .unwrap();

        assert_eq!(
            installed_plugin_marketplaces(&paths, Scope::Project, "foo"),
            vec!["acme-tools"]
        );
        assert!(installed_plugin_marketplaces(&paths, Scope::Project, "nope").is_empty());
        assert!(installed_plugin_marketplaces(&paths, Scope::Global, "foo").is_empty());
    }

    // --- Local marketplaces ---

    fn write_local_marketplace(root: &Path) -> PathBuf {
        let dir = root.join("my-marketplace");
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::write(
            dir.join(".claude-plugin/marketplace.json"),
            r#"{
                "name": "local-dev",
                "owner": {"name": "me"},
                "plugins": [
                    {"name": "hello", "source": "./plugins/hello"},
                    {"name": "escape", "source": "./../outside"}
                ]
            }"#,
        )
        .unwrap();
        let skill_dir = dir.join("plugins/hello/skills/greet");
        std::fs::create_dir_all(skill_dir.join("sub")).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: greet\ndescription: d\n---\nlocal body",
        )
        .unwrap();
        std::fs::write(skill_dir.join("sub/tool.py"), "print('hi')").unwrap();
        dir
    }

    fn local_marketplace(root: &Path) -> RegisteredMarketplace {
        RegisteredMarketplace {
            name: "local-dev".into(),
            source: MarketplaceSource::Local {
                path: write_local_marketplace(root),
            },
        }
    }

    fn no_client() -> impl Fn(Host) -> Result<SkillClient, MarketplaceError> {
        |_| panic!("a local marketplace must never build a host client")
    }

    #[tokio::test]
    async fn installs_a_plugin_from_a_local_marketplace() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let marketplace = local_marketplace(root.path());

        let installed = install_plugin(&paths, Scope::Project, &marketplace, "hello", &no_client())
            .await
            .unwrap();

        assert_eq!(installed, vec!["greet"]);
        let skill_dir = paths.project_config_dir.join("skills/greet");
        assert!(skill_dir.join("SKILL.md").exists());
        assert!(skill_dir.join("sub/tool.py").exists());
        let manifest =
            crate::skills::install::read_skill_manifest(&paths, Scope::Project, "greet").unwrap();
        assert_eq!(
            manifest.plugin,
            Some(PluginProvenance {
                marketplace: "local-dev".into(),
                plugin: "hello".into(),
            })
        );
    }

    #[tokio::test]
    async fn local_marketplace_edits_flow_through_update() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let marketplace = local_marketplace(root.path());
        install_plugin(&paths, Scope::Project, &marketplace, "hello", &no_client())
            .await
            .unwrap();

        // Local marketplaces are read live: edit the skill in place, then update.
        let MarketplaceSource::Local { path } = &marketplace.source else {
            panic!("expected a local source");
        };
        std::fs::write(
            path.join("plugins/hello/skills/greet/SKILL.md"),
            "edited local body",
        )
        .unwrap();

        let report = update_plugin(&paths, Scope::Project, &marketplace, "hello", &no_client())
            .await
            .unwrap();
        assert_eq!(report.updated, vec!["greet"]);
        let content =
            std::fs::read_to_string(paths.project_config_dir.join("skills/greet/SKILL.md"))
                .unwrap();
        assert_eq!(content, "edited local body");
    }

    #[tokio::test]
    async fn a_relative_source_escaping_the_marketplace_root_is_rejected() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let marketplace = local_marketplace(root.path());

        let result =
            install_plugin(&paths, Scope::Project, &marketplace, "escape", &no_client()).await;
        assert!(matches!(
            result,
            Err(MarketplaceError::Catalog(CatalogError::UnsafeSourcePath(_)))
        ));
    }
}
