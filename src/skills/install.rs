// src/skills/install.rs

use std::path::{Path, PathBuf};

use crate::config::paths::Paths;
use crate::skills::client::SkillClient;
use crate::skills::types::{FetchedFile, Host, InstalledSkillManifest, Scope, SkillHostError, SkillSource};

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Host(#[from] SkillHostError),
    #[error("io error at {path}: {source}")]
    Io { path: PathBuf, source: std::io::Error },
    #[error("failed to (de)serialize skill manifest: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("a skill named '{0}' is already installed in this scope; pass --name to choose a different name")]
    AlreadyInstalled(String),
    #[error("no skill named '{0}' is installed in this scope")]
    NotInstalled(String),
    #[error("fetched directory '{0}' contained no files")]
    EmptyDirectory(String),
    #[error("refusing to write file with unsafe path '{}' (contains '..' or is absolute)", .0.display())]
    UnsafePath(PathBuf),
}

/// Filename of the JSON manifest recorded alongside each installed skill's
/// fetched files, tracking its GitHub source and pinned commit.
const MANIFEST_FILENAME: &str = ".skill-manifest.json";

fn skills_dir(paths: &Paths, scope: Scope) -> PathBuf {
    match scope {
        Scope::Project => paths.project_config_dir.join("skills"),
        Scope::Global => paths.user_config_dir.join("skills"),
    }
}

/// Both scope directories paired with their `Scope`, in the fixed order
/// (project, then global) that callers rely on for shadowing semantics.
pub(crate) fn scope_dirs(paths: &Paths) -> [(PathBuf, Scope); 2] {
    [(skills_dir(paths, Scope::Project), Scope::Project), (skills_dir(paths, Scope::Global), Scope::Global)]
}

/// The temp directory a fresh fetch is written into before being renamed
/// into place, for both `install_skill` and `update_skill`.
fn installing_temp_dir(parent: &Path, name: &str) -> PathBuf {
    parent.join(format!(".{name}.installing"))
}

/// The backup path `update_skill` renames the pre-update directory to while
/// swapping in the newly-fetched content.
fn replaced_backup_dir(parent: &Path, name: &str) -> PathBuf {
    parent.join(format!(".{name}.replaced"))
}

/// Derives the default install name from a source spec: the last path
/// segment if a subpath was given, otherwise the repo name.
pub fn default_name(source: &SkillSource) -> String {
    source
        .path
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(&source.repo)
        .to_string()
}

fn io_err(path: &Path, source: std::io::Error) -> InstallError {
    InstallError::Io { path: path.to_path_buf(), source }
}

/// Fetches `source` from GitHub and installs it as `name` into `scope`.
/// Fetches fully into a temp directory first, then renames it into place —
/// a failed install never leaves a partially-written skill directory behind.
/// Errors with `AlreadyInstalled` if a skill with this name already exists
/// in this scope (use `update_skill` to refresh an existing install).
pub async fn install_skill(
    client: &SkillClient,
    paths: &Paths,
    scope: Scope,
    source: &SkillSource,
    name: &str,
) -> Result<(), InstallError> {
    let target_dir = skills_dir(paths, scope).join(name);
    if target_dir.exists() {
        return Err(InstallError::AlreadyInstalled(name.to_string()));
    }

    let git_ref = match &source.git_ref {
        Some(r) => r.clone(),
        None => client.resolve_default_branch(&source.owner, &source.repo).await?,
    };
    let commit_sha = client.resolve_commit_sha(&source.owner, &source.repo, &git_ref).await?;
    let files = client.fetch_directory_files(&source.owner, &source.repo, &source.path, &commit_sha).await?;
    if files.is_empty() {
        return Err(InstallError::EmptyDirectory(source.path.clone()));
    }

    let manifest = InstalledSkillManifest {
        host: source.host,
        owner: source.owner.clone(),
        repo: source.repo.clone(),
        path: source.path.clone(),
        git_ref,
        commit_sha,
    };

    let parent = target_dir.parent().expect("skills dir always has a parent");
    std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    let temp_dir = installing_temp_dir(parent, name);
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir).map_err(|e| io_err(&temp_dir, e))?;
    }
    write_files(&temp_dir, &files, &manifest)?;

    std::fs::rename(&temp_dir, &target_dir).map_err(|e| io_err(&target_dir, e))?;
    Ok(())
}

fn write_files(dir: &Path, files: &[FetchedFile], manifest: &InstalledSkillManifest) -> Result<(), InstallError> {
    // Defense-in-depth: reject any fetched file whose relative path is absolute
    // or escapes the target directory via a `..` component, *before* writing
    // anything. GitHub's real API shouldn't produce such paths, but nothing
    // upstream guarantees it, so refuse to trust `relative_path` blindly.
    for file in files {
        if file.relative_path.is_absolute()
            || file.relative_path.components().any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(InstallError::UnsafePath(file.relative_path.clone()));
        }
    }

    std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    for file in files {
        let dest = dir.join(&file.relative_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
        std::fs::write(&dest, &file.bytes).map_err(|e| io_err(&dest, e))?;
    }
    let manifest_json = serde_json::to_string_pretty(manifest)?;
    std::fs::write(dir.join(MANIFEST_FILENAME), manifest_json).map_err(|e| io_err(dir, e))?;
    Ok(())
}

/// Re-resolves `name`'s pinned ref to a commit SHA; if it has changed since
/// the manifest's recorded `commit_sha`, re-fetches and replaces the skill's
/// files (manifest included), the same way `install_skill` does but with a
/// weaker atomicity guarantee: the new files are written to a temp directory
/// first (which fails safe, same as install), then the old directory is
/// renamed aside to a backup path, then the temp directory is renamed into
/// place, then the backup is removed. A crash after the second rename leaves
/// the new skill fully installed, with only an orphaned backup directory
/// left to clean up; a crash before it leaves the old skill untouched (either
/// still in place, or recoverable from the backup).
///
/// There IS a window, between the two renames, where the skill's directory
/// doesn't exist at all. This function is self-healing across that window,
/// though: if a prior call crashed there, `dir` is missing but the backup
/// (`.{name}.replaced`) still holds the pre-update content, and the next
/// call to `update_skill` restores it from the backup before doing anything
/// else, rather than reporting `NotInstalled`.
/// No-op (returns `Ok(false)`) if the ref hasn't moved. Returns `Ok(true)` if
/// an update was applied.
pub async fn update_skill(client: &SkillClient, paths: &Paths, scope: Scope, name: &str) -> Result<bool, InstallError> {
    let dir = skills_dir(paths, scope).join(name);
    let parent = dir.parent().expect("skills dir always has a parent").to_path_buf();
    let backup_dir = replaced_backup_dir(&parent, name);

    // Self-heal: a prior `update_skill` call may have crashed between renaming
    // `dir` aside to `backup_dir` and renaming the newly-fetched content into
    // `dir`. If so, `dir` is missing but `backup_dir` (the pre-update content)
    // still exists — restore it so this call sees the skill as still
    // installed, rather than erroring `NotInstalled`.
    if !dir.exists() && backup_dir.exists() {
        std::fs::rename(&backup_dir, &dir).map_err(|e| io_err(&dir, e))?;
    }

    if !dir.exists() {
        return Err(InstallError::NotInstalled(name.to_string()));
    }
    let manifest_text = std::fs::read_to_string(dir.join(MANIFEST_FILENAME)).map_err(|e| io_err(&dir, e))?;
    let manifest: InstalledSkillManifest = serde_json::from_str(&manifest_text)?;

    let latest_sha = client.resolve_commit_sha(&manifest.owner, &manifest.repo, &manifest.git_ref).await?;
    if latest_sha == manifest.commit_sha {
        return Ok(false);
    }

    let files = client.fetch_directory_files(&manifest.owner, &manifest.repo, &manifest.path, &latest_sha).await?;
    if files.is_empty() {
        return Err(InstallError::EmptyDirectory(manifest.path.clone()));
    }
    let new_manifest = InstalledSkillManifest { commit_sha: latest_sha, ..manifest };

    let temp_dir = installing_temp_dir(&parent, name);
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir).map_err(|e| io_err(&temp_dir, e))?;
    }
    write_files(&temp_dir, &files, &new_manifest)?;

    if backup_dir.exists() {
        std::fs::remove_dir_all(&backup_dir).map_err(|e| io_err(&backup_dir, e))?;
    }
    std::fs::rename(&dir, &backup_dir).map_err(|e| io_err(&dir, e))?;
    std::fs::rename(&temp_dir, &dir).map_err(|e| io_err(&dir, e))?;
    std::fs::remove_dir_all(&backup_dir).map_err(|e| io_err(&backup_dir, e))?;
    Ok(true)
}

/// Removes an installed skill's directory entirely.
pub fn remove_skill(paths: &Paths, scope: Scope, name: &str) -> Result<(), InstallError> {
    let dir = skills_dir(paths, scope).join(name);
    if !dir.exists() {
        return Err(InstallError::NotInstalled(name.to_string()));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| io_err(&dir, e))
}

/// One row of `local-code skills list` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledSkillSummary {
    pub name: String,
    pub scope: Scope,
    pub source: String, // "owner/repo/path@ref", prefixed "gl:"/"bb:" for non-GitHub
}

/// Lists every installed skill across both scopes (no shadowing applied here
/// — `list` shows everything installed, including a global skill that's
/// currently shadowed by a project skill of the same name, so the user can
/// see what `remove --global` would affect).
pub fn list_skills(paths: &Paths) -> Result<Vec<InstalledSkillSummary>, InstallError> {
    let mut summaries = Vec::new();
    for (dir, scope) in scope_dirs(paths) {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let skill_dir = entry.path();
            if !skill_dir.is_dir() {
                continue;
            }
            let manifest_path = skill_dir.join(MANIFEST_FILENAME);
            let manifest_text = match std::fs::read_to_string(&manifest_path) {
                Ok(text) => text,
                Err(e) => {
                    eprintln!("warning: skipping skill at {}: no manifest: {e}", skill_dir.display());
                    continue;
                }
            };
            let manifest = match serde_json::from_str::<InstalledSkillManifest>(&manifest_text) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("warning: skipping skill at {}: invalid manifest: {e}", skill_dir.display());
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().to_string();
            let path_suffix = if manifest.path.is_empty() { String::new() } else { format!("/{}", manifest.path) };
            let bare = format!("{}/{}{}@{}", manifest.owner, manifest.repo, path_suffix, manifest.git_ref);
            let source = match manifest.host {
                Host::GitHub => bare,
                Host::GitLab => format!("gl:{bare}"),
                Host::Bitbucket => format!("bb:{bare}"),
            };
            summaries.push(InstalledSkillSummary { name, scope, source });
        }
    }
    Ok(summaries)
}

/// Reads just the `host` field from an installed skill's manifest, without
/// the rest of `list_skills`' full-scan/format work — used by
/// `cli::skills::update` to pick the right `SkillClient` per skill before
/// calling `update_skill`.
pub fn skill_manifest_host(paths: &Paths, scope: Scope, name: &str) -> Result<Host, InstallError> {
    let manifest_path = skills_dir(paths, scope).join(name).join(MANIFEST_FILENAME);
    let manifest_text = std::fs::read_to_string(&manifest_path).map_err(|e| io_err(&manifest_path, e))?;
    let manifest: InstalledSkillManifest = serde_json::from_str(&manifest_text)?;
    Ok(manifest.host)
}

#[cfg(test)]
mod tests {
    use super::*;
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

    async fn mock_server_with_one_file() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"default_branch": "main"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"sha": "abc123"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets/contents/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "skills/pdf/SKILL.md", "type": "file",
                 "download_url": format!("{}/raw/SKILL.md", server.uri())}
            ])))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/raw/SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("---\nname: pdf\ndescription: d\n---\nbody"))
            .mount(&server).await;
        server
    }

    #[tokio::test]
    async fn installs_a_skill_into_project_scope() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = mock_server_with_one_file().await;
        let client = SkillClient::GitHub(crate::skills::github::GithubClient::new_for_test(None, server.uri()));
        let source = SkillSource { host: Host::GitHub, owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };

        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        let skill_md = paths.project_config_dir.join("skills/pdf/SKILL.md");
        assert!(skill_md.exists());
        let manifest_path = paths.project_config_dir.join("skills/pdf/.skill-manifest.json");
        let manifest: InstalledSkillManifest = serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.commit_sha, "abc123");
        assert_eq!(manifest.git_ref, "main");
    }

    #[tokio::test]
    async fn refuses_to_overwrite_an_existing_install() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        std::fs::create_dir_all(paths.project_config_dir.join("skills/pdf")).unwrap();
        let server = mock_server_with_one_file().await;
        let client = SkillClient::GitHub(crate::skills::github::GithubClient::new_for_test(None, server.uri()));
        let source = SkillSource { host: Host::GitHub, owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };

        let result = install_skill(&client, &paths, Scope::Project, &source, "pdf").await;
        assert!(matches!(result, Err(InstallError::AlreadyInstalled(name)) if name == "pdf"));
    }

    #[tokio::test]
    async fn install_fails_with_empty_directory_error_when_fetch_returns_no_files() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"default_branch": "main"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"sha": "abc123"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets/contents/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server).await;
        let client = SkillClient::GitHub(crate::skills::github::GithubClient::new_for_test(None, server.uri()));
        let source = SkillSource { host: Host::GitHub, owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };

        let result = install_skill(&client, &paths, Scope::Project, &source, "pdf").await;
        assert!(matches!(result, Err(InstallError::EmptyDirectory(p)) if p == "skills/pdf"));

        let target_dir = paths.project_config_dir.join("skills/pdf");
        assert!(!target_dir.exists());
    }

    #[tokio::test]
    async fn installs_a_skill_into_global_scope() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = mock_server_with_one_file().await;
        let client = SkillClient::GitHub(crate::skills::github::GithubClient::new_for_test(None, server.uri()));
        let source = SkillSource { host: Host::GitHub, owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };

        install_skill(&client, &paths, Scope::Global, &source, "pdf").await.unwrap();

        let skill_md = paths.user_config_dir.join("skills/pdf/SKILL.md");
        assert!(skill_md.exists());
        let manifest_path = paths.user_config_dir.join("skills/pdf/.skill-manifest.json");
        let manifest: InstalledSkillManifest = serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.commit_sha, "abc123");
        assert_eq!(manifest.git_ref, "main");
    }

    #[tokio::test]
    async fn failed_fetch_leaves_no_partial_target_directory() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());

        // Point the raw-file download at a port nothing is listening on (port 1 is
        // a privileged port no unprivileged process can bind, so it's reliably
        // unreachable), so the download fails at the connection level (simulating
        // a mid-fetch failure) rather than merely returning a non-2xx status —
        // `fetch_directory_files` never checks the raw download's HTTP status, so
        // a mocked 500 response would be "successfully" downloaded as file content.
        let dead_download_url = "http://127.0.0.1:1/raw/SKILL.md".to_string();

        let server = MockServer::start().await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"default_branch": "main"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"sha": "abc123"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets/contents/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "skills/pdf/SKILL.md", "type": "file",
                 "download_url": dead_download_url}
            ])))
            .mount(&server).await;
        let client = SkillClient::GitHub(crate::skills::github::GithubClient::new_for_test(None, server.uri()));
        let source = SkillSource { host: Host::GitHub, owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };

        let result = install_skill(&client, &paths, Scope::Project, &source, "pdf").await;
        assert!(result.is_err());

        let target_dir = paths.project_config_dir.join("skills/pdf");
        assert!(!target_dir.exists());
    }

    #[test]
    fn default_name_uses_last_path_segment() {
        let source = SkillSource { host: Host::GitHub, owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };
        assert_eq!(default_name(&source), "pdf");
    }

    #[test]
    fn default_name_falls_back_to_repo_when_no_path() {
        let source = SkillSource { host: Host::GitHub, owner: "acme".into(), repo: "widgets".into(), path: "".into(), git_ref: None };
        assert_eq!(default_name(&source), "widgets");
    }

    #[tokio::test]
    async fn update_is_a_noop_when_ref_has_not_moved() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = mock_server_with_one_file().await;
        let client = SkillClient::GitHub(crate::skills::github::GithubClient::new_for_test(None, server.uri()));
        let source = SkillSource { host: Host::GitHub, owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };
        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        let updated = update_skill(&client, &paths, Scope::Project, "pdf").await.unwrap();
        assert!(!updated);
    }

    #[tokio::test]
    async fn update_refetches_when_ref_has_moved() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = mock_server_with_one_file().await;
        let client = SkillClient::GitHub(crate::skills::github::GithubClient::new_for_test(None, server.uri()));
        let source = SkillSource { host: Host::GitHub, owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };
        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        // Point the commit-resolution mock at a new sha and change the file content.
        // `with_priority(1)` is needed so these newly-mounted mocks take precedence
        // over the still-mounted mocks from `mock_server_with_one_file` above, since
        // wiremock falls back to insertion order when priority is tied (default: 5).
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"sha": "def456"})))
            .with_priority(1)
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repos/acme/widgets/contents/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"name": "SKILL.md", "path": "skills/pdf/SKILL.md", "type": "file",
                 "download_url": format!("{}/raw/SKILL2.md", server.uri())}
            ])))
            .with_priority(1)
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/raw/SKILL2.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("---\nname: pdf\ndescription: updated\n---\nnew body"))
            .with_priority(1)
            .mount(&server).await;

        let updated = update_skill(&client, &paths, Scope::Project, "pdf").await.unwrap();
        assert!(updated);
        let content = std::fs::read_to_string(paths.project_config_dir.join("skills/pdf/SKILL.md")).unwrap();
        assert!(content.contains("updated"));
    }

    #[tokio::test]
    async fn update_self_heals_from_a_crash_between_the_two_renames() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = mock_server_with_one_file().await;
        let client = SkillClient::GitHub(crate::skills::github::GithubClient::new_for_test(None, server.uri()));

        let parent = paths.project_config_dir.join("skills");
        let dir = parent.join("pdf");
        let backup_dir = parent.join(".pdf.replaced");

        // Simulate the on-disk state a crash between the two renames in
        // `update_skill` would leave behind: `dir` is missing, but the
        // pre-update content (including a valid manifest matching the "ref
        // hasn't moved" mock) still sits at the backup path.
        std::fs::create_dir_all(&backup_dir).unwrap();
        std::fs::write(backup_dir.join("SKILL.md"), "pre-update content").unwrap();
        let manifest = InstalledSkillManifest {
            host: Host::GitHub,
            owner: "acme".into(),
            repo: "widgets".into(),
            path: "skills/pdf".into(),
            git_ref: "main".into(),
            commit_sha: "abc123".into(),
        };
        std::fs::write(backup_dir.join(MANIFEST_FILENAME), serde_json::to_string(&manifest).unwrap()).unwrap();
        assert!(!dir.exists());

        // The mocked ref hasn't moved (still resolves to "abc123"), so this
        // call should self-heal (restore `dir` from `backup_dir`) and then
        // return `Ok(false)` (no-op) rather than `Err(NotInstalled)`.
        let updated = update_skill(&client, &paths, Scope::Project, "pdf").await.unwrap();
        assert!(!updated);

        assert!(dir.exists());
        assert!(!backup_dir.exists());
        let content = std::fs::read_to_string(dir.join("SKILL.md")).unwrap();
        assert_eq!(content, "pre-update content");
    }

    #[tokio::test]
    async fn update_errors_when_skill_not_installed() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        let client = SkillClient::GitHub(crate::skills::github::GithubClient::new_for_test(None, server.uri()));
        let result = update_skill(&client, &paths, Scope::Project, "nope").await;
        assert!(matches!(result, Err(InstallError::NotInstalled(name)) if name == "nope"));
    }

    #[test]
    fn remove_deletes_the_skill_directory() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let dir = paths.project_config_dir.join("skills/pdf");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), "content").unwrap();

        remove_skill(&paths, Scope::Project, "pdf").unwrap();
        assert!(!dir.exists());
    }

    #[test]
    fn remove_errors_when_not_installed() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let result = remove_skill(&paths, Scope::Project, "nope");
        assert!(matches!(result, Err(InstallError::NotInstalled(name)) if name == "nope"));
    }

    #[tokio::test]
    async fn list_reports_installed_skills_with_source_and_scope() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = mock_server_with_one_file().await;
        let client = SkillClient::GitHub(crate::skills::github::GithubClient::new_for_test(None, server.uri()));
        let source = SkillSource { host: Host::GitHub, owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };
        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        let summaries = list_skills(&paths).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].name, "pdf");
        assert_eq!(summaries[0].scope, Scope::Project);
        assert_eq!(summaries[0].source, "acme/widgets/skills/pdf@main");
    }

    #[tokio::test]
    async fn list_reports_both_scopes_without_shadowing_same_named_skill() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = mock_server_with_one_file().await;
        let client = SkillClient::GitHub(crate::skills::github::GithubClient::new_for_test(None, server.uri()));
        let source = SkillSource { host: Host::GitHub, owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };
        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();
        install_skill(&client, &paths, Scope::Global, &source, "pdf").await.unwrap();

        let summaries = list_skills(&paths).unwrap();
        assert_eq!(summaries.len(), 2);
        assert!(summaries.iter().any(|s| s.name == "pdf" && s.scope == Scope::Project));
        assert!(summaries.iter().any(|s| s.name == "pdf" && s.scope == Scope::Global));
    }

    #[test]
    fn list_is_empty_when_nothing_installed() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let summaries = list_skills(&paths).unwrap();
        assert!(summaries.is_empty());
    }

    #[test]
    fn write_files_rejects_a_path_traversal_relative_path() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let target_dir = paths.project_config_dir.join("skills/pdf");
        let manifest = InstalledSkillManifest {
            host: Host::GitHub,
            owner: "acme".into(),
            repo: "widgets".into(),
            path: "skills/pdf".into(),
            git_ref: "main".into(),
            commit_sha: "abc123".into(),
        };
        let files = vec![
            FetchedFile { relative_path: PathBuf::from("SKILL.md"), bytes: b"safe".to_vec() },
            FetchedFile { relative_path: PathBuf::from("../../etc/passwn"), bytes: b"evil".to_vec() },
        ];

        let result = write_files(&target_dir, &files, &manifest);
        assert!(matches!(result, Err(InstallError::UnsafePath(p)) if p == PathBuf::from("../../etc/passwn")));

        // Nothing should have been written outside (or even inside) the target
        // directory as a result of the rejected batch.
        assert!(!root.path().join("etc/passwn").exists());
        assert!(!target_dir.join("SKILL.md").exists());
    }

    #[test]
    fn write_files_rejects_an_absolute_relative_path() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let target_dir = paths.project_config_dir.join("skills/pdf");
        let manifest = InstalledSkillManifest {
            host: Host::GitHub,
            owner: "acme".into(),
            repo: "widgets".into(),
            path: "skills/pdf".into(),
            git_ref: "main".into(),
            commit_sha: "abc123".into(),
        };
        let files = vec![FetchedFile { relative_path: PathBuf::from("/etc/passwn"), bytes: b"evil".to_vec() }];

        let result = write_files(&target_dir, &files, &manifest);
        assert!(matches!(result, Err(InstallError::UnsafePath(p)) if p == PathBuf::from("/etc/passwn")));
    }

    #[test]
    fn list_skips_a_skill_with_a_corrupt_manifest() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let broken_dir = paths.project_config_dir.join("skills/broken");
        std::fs::create_dir_all(&broken_dir).unwrap();
        std::fs::write(broken_dir.join(".skill-manifest.json"), "not valid json").unwrap();

        let summaries = list_skills(&paths).unwrap();
        assert!(summaries.is_empty());
    }

    // --- GitLab ---

    async fn gitlab_mock_server_with_one_file() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(wpath("/projects/acme%2Fwidgets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"default_branch": "main"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/projects/acme%2Fwidgets/repository/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "abc123"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/projects/acme%2Fwidgets/repository/tree"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "skills/pdf/SKILL.md", "type": "blob"}
            ])))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/projects/acme%2Fwidgets/repository/files/skills%2Fpdf%2FSKILL.md/raw"))
            .respond_with(ResponseTemplate::new(200).set_body_string("---\nname: pdf\ndescription: d\n---\nbody"))
            .mount(&server).await;
        server
    }

    fn gitlab_source() -> SkillSource {
        SkillSource { host: Host::GitLab, owner: "".into(), repo: "acme/widgets".into(), path: "skills/pdf".into(), git_ref: None }
    }

    #[tokio::test]
    async fn installs_a_gitlab_skill_into_project_scope() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = gitlab_mock_server_with_one_file().await;
        let client = SkillClient::GitLab(crate::skills::gitlab::GitlabClient::new_for_test(None, server.uri()));
        let source = gitlab_source();

        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        let skill_md = paths.project_config_dir.join("skills/pdf/SKILL.md");
        assert!(skill_md.exists());
        let manifest_path = paths.project_config_dir.join("skills/pdf/.skill-manifest.json");
        let manifest: InstalledSkillManifest = serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.host, Host::GitLab);
        assert_eq!(manifest.commit_sha, "abc123");
        assert_eq!(manifest.git_ref, "main");
    }

    #[tokio::test]
    async fn refuses_to_overwrite_an_existing_gitlab_install() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        std::fs::create_dir_all(paths.project_config_dir.join("skills/pdf")).unwrap();
        let server = gitlab_mock_server_with_one_file().await;
        let client = SkillClient::GitLab(crate::skills::gitlab::GitlabClient::new_for_test(None, server.uri()));
        let source = gitlab_source();

        let result = install_skill(&client, &paths, Scope::Project, &source, "pdf").await;
        assert!(matches!(result, Err(InstallError::AlreadyInstalled(name)) if name == "pdf"));
    }

    #[tokio::test]
    async fn gitlab_install_fails_with_empty_directory_error_when_fetch_returns_no_files() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(wpath("/projects/acme%2Fwidgets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"default_branch": "main"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/projects/acme%2Fwidgets/repository/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "abc123"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/projects/acme%2Fwidgets/repository/tree"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server).await;
        let client = SkillClient::GitLab(crate::skills::gitlab::GitlabClient::new_for_test(None, server.uri()));
        let source = gitlab_source();

        let result = install_skill(&client, &paths, Scope::Project, &source, "pdf").await;
        assert!(matches!(result, Err(InstallError::EmptyDirectory(p)) if p == "skills/pdf"));

        let target_dir = paths.project_config_dir.join("skills/pdf");
        assert!(!target_dir.exists());
    }

    #[tokio::test]
    async fn gitlab_update_is_a_noop_when_ref_has_not_moved() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = gitlab_mock_server_with_one_file().await;
        let client = SkillClient::GitLab(crate::skills::gitlab::GitlabClient::new_for_test(None, server.uri()));
        let source = gitlab_source();
        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        let updated = update_skill(&client, &paths, Scope::Project, "pdf").await.unwrap();
        assert!(!updated);
    }

    #[tokio::test]
    async fn gitlab_update_refetches_when_ref_has_moved() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = gitlab_mock_server_with_one_file().await;
        let client = SkillClient::GitLab(crate::skills::gitlab::GitlabClient::new_for_test(None, server.uri()));
        let source = gitlab_source();
        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        Mock::given(method("GET")).and(wpath("/projects/acme%2Fwidgets/repository/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "def456"})))
            .with_priority(1)
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/projects/acme%2Fwidgets/repository/tree"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "skills/pdf/SKILL.md", "type": "blob"}
            ])))
            .with_priority(1)
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/projects/acme%2Fwidgets/repository/files/skills%2Fpdf%2FSKILL.md/raw"))
            .respond_with(ResponseTemplate::new(200).set_body_string("---\nname: pdf\ndescription: updated\n---\nnew body"))
            .with_priority(1)
            .mount(&server).await;

        let updated = update_skill(&client, &paths, Scope::Project, "pdf").await.unwrap();
        assert!(updated);
        let content = std::fs::read_to_string(paths.project_config_dir.join("skills/pdf/SKILL.md")).unwrap();
        assert!(content.contains("updated"));
    }

    // --- Bitbucket ---

    async fn bitbucket_mock_server_with_one_file() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(wpath("/repositories/acme/widgets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"mainbranch": {"name": "main"}})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repositories/acme/widgets/commit/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"hash": "abc123"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repositories/acme/widgets/src/abc123/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "values": [{
                    "path": "skills/pdf/SKILL.md",
                    "type": "commit_file",
                    "links": {"self": {"href": format!("{}/raw/SKILL.md", server.uri())}}
                }],
                "next": null
            })))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/raw/SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("---\nname: pdf\ndescription: d\n---\nbody"))
            .mount(&server).await;
        server
    }

    fn bitbucket_source() -> SkillSource {
        SkillSource { host: Host::Bitbucket, owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None }
    }

    #[tokio::test]
    async fn installs_a_bitbucket_skill_into_project_scope() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = bitbucket_mock_server_with_one_file().await;
        let client = SkillClient::Bitbucket(crate::skills::bitbucket::BitbucketClient::new_for_test(None, server.uri()));
        let source = bitbucket_source();

        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        let skill_md = paths.project_config_dir.join("skills/pdf/SKILL.md");
        assert!(skill_md.exists());
        let manifest_path = paths.project_config_dir.join("skills/pdf/.skill-manifest.json");
        let manifest: InstalledSkillManifest = serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.host, Host::Bitbucket);
        assert_eq!(manifest.commit_sha, "abc123");
        assert_eq!(manifest.git_ref, "main");
    }

    #[tokio::test]
    async fn refuses_to_overwrite_an_existing_bitbucket_install() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        std::fs::create_dir_all(paths.project_config_dir.join("skills/pdf")).unwrap();
        let server = bitbucket_mock_server_with_one_file().await;
        let client = SkillClient::Bitbucket(crate::skills::bitbucket::BitbucketClient::new_for_test(None, server.uri()));
        let source = bitbucket_source();

        let result = install_skill(&client, &paths, Scope::Project, &source, "pdf").await;
        assert!(matches!(result, Err(InstallError::AlreadyInstalled(name)) if name == "pdf"));
    }

    #[tokio::test]
    async fn bitbucket_install_fails_with_empty_directory_error_when_fetch_returns_no_files() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(wpath("/repositories/acme/widgets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"mainbranch": {"name": "main"}})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repositories/acme/widgets/commit/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"hash": "abc123"})))
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repositories/acme/widgets/src/abc123/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"values": [], "next": null})))
            .mount(&server).await;
        let client = SkillClient::Bitbucket(crate::skills::bitbucket::BitbucketClient::new_for_test(None, server.uri()));
        let source = bitbucket_source();

        let result = install_skill(&client, &paths, Scope::Project, &source, "pdf").await;
        assert!(matches!(result, Err(InstallError::EmptyDirectory(p)) if p == "skills/pdf"));

        let target_dir = paths.project_config_dir.join("skills/pdf");
        assert!(!target_dir.exists());
    }

    #[tokio::test]
    async fn bitbucket_update_is_a_noop_when_ref_has_not_moved() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = bitbucket_mock_server_with_one_file().await;
        let client = SkillClient::Bitbucket(crate::skills::bitbucket::BitbucketClient::new_for_test(None, server.uri()));
        let source = bitbucket_source();
        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        let updated = update_skill(&client, &paths, Scope::Project, "pdf").await.unwrap();
        assert!(!updated);
    }

    #[tokio::test]
    async fn bitbucket_update_refetches_when_ref_has_moved() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let server = bitbucket_mock_server_with_one_file().await;
        let client = SkillClient::Bitbucket(crate::skills::bitbucket::BitbucketClient::new_for_test(None, server.uri()));
        let source = bitbucket_source();
        install_skill(&client, &paths, Scope::Project, &source, "pdf").await.unwrap();

        Mock::given(method("GET")).and(wpath("/repositories/acme/widgets/commit/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"hash": "def456"})))
            .with_priority(1)
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/repositories/acme/widgets/src/def456/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "values": [{
                    "path": "skills/pdf/SKILL.md",
                    "type": "commit_file",
                    "links": {"self": {"href": format!("{}/raw/SKILL2.md", server.uri())}}
                }],
                "next": null
            })))
            .with_priority(1)
            .mount(&server).await;
        Mock::given(method("GET")).and(wpath("/raw/SKILL2.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("---\nname: pdf\ndescription: updated\n---\nnew body"))
            .with_priority(1)
            .mount(&server).await;

        let updated = update_skill(&client, &paths, Scope::Project, "pdf").await.unwrap();
        assert!(updated);
        let content = std::fs::read_to_string(paths.project_config_dir.join("skills/pdf/SKILL.md")).unwrap();
        assert!(content.contains("updated"));
    }
}
