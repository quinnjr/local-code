// src/skills/install.rs

use std::path::{Path, PathBuf};

use crate::config::paths::Paths;
use crate::skills::github::{FetchedFile, GithubClient, GithubError};
use crate::skills::types::{InstalledSkillManifest, Scope, SkillSource};

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Github(#[from] GithubError),
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
}

fn skills_dir(paths: &Paths, scope: Scope) -> PathBuf {
    match scope {
        Scope::Project => paths.project_config_dir.join("skills"),
        Scope::Global => paths.user_config_dir.join("skills"),
    }
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
    client: &GithubClient,
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
        owner: source.owner.clone(),
        repo: source.repo.clone(),
        path: source.path.clone(),
        git_ref,
        commit_sha,
    };

    let parent = target_dir.parent().expect("skills dir always has a parent");
    std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
    let temp_dir = parent.join(format!(".{name}.installing"));
    if temp_dir.exists() {
        std::fs::remove_dir_all(&temp_dir).map_err(|e| io_err(&temp_dir, e))?;
    }
    write_files(&temp_dir, &files, &manifest)?;

    std::fs::rename(&temp_dir, &target_dir).map_err(|e| io_err(&target_dir, e))?;
    Ok(())
}

fn write_files(dir: &Path, files: &[FetchedFile], manifest: &InstalledSkillManifest) -> Result<(), InstallError> {
    std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    for file in files {
        let dest = dir.join(&file.relative_path);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
        std::fs::write(&dest, &file.bytes).map_err(|e| io_err(&dest, e))?;
    }
    let manifest_json = serde_json::to_string_pretty(manifest)?;
    std::fs::write(dir.join(".skill-manifest.json"), manifest_json).map_err(|e| io_err(dir, e))?;
    Ok(())
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
        let client = crate::skills::github::GithubClient::new_for_test(None, server.uri());
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };

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
        let client = crate::skills::github::GithubClient::new_for_test(None, server.uri());
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };

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
        let client = crate::skills::github::GithubClient::new_for_test(None, server.uri());
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };

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
        let client = crate::skills::github::GithubClient::new_for_test(None, server.uri());
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };

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
        let client = crate::skills::github::GithubClient::new_for_test(None, server.uri());
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };

        let result = install_skill(&client, &paths, Scope::Project, &source, "pdf").await;
        assert!(result.is_err());

        let target_dir = paths.project_config_dir.join("skills/pdf");
        assert!(!target_dir.exists());
    }

    #[test]
    fn default_name_uses_last_path_segment() {
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "skills/pdf".into(), git_ref: None };
        assert_eq!(default_name(&source), "pdf");
    }

    #[test]
    fn default_name_falls_back_to_repo_when_no_path() {
        let source = SkillSource { owner: "acme".into(), repo: "widgets".into(), path: "".into(), git_ref: None };
        assert_eq!(default_name(&source), "widgets");
    }
}
