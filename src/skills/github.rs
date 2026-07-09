// src/skills/github.rs

use crate::skills::types::{FetchedFile, Host, SkillHostError, SkillSource};

/// Parses an `owner/repo[/path][@ref]` skill source spec. The ref, if
/// present, is split off from the *last* `@` in the spec (GitHub owner/repo
/// names and paths cannot themselves contain `@`, so this is unambiguous in
/// practice). `path` is `""` when no subpath was given.
pub fn parse_spec(spec: &str) -> Result<SkillSource, SkillHostError> {
    let (rest, git_ref) = match spec.rsplit_once('@') {
        Some((rest, r)) if !r.is_empty() => (rest, Some(r.to_string())),
        _ => (spec, None),
    };

    let mut parts = rest.splitn(3, '/');
    let owner = parts.next().filter(|s| !s.is_empty());
    let repo = parts.next().filter(|s| !s.is_empty());
    let path = parts.next().unwrap_or("").to_string();

    match (owner, repo) {
        (Some(owner), Some(repo)) => Ok(SkillSource {
            host: Host::GitHub,
            owner: owner.to_string(),
            repo: repo.to_string(),
            path,
            git_ref,
        }),
        _ => Err(SkillHostError::InvalidSpec(spec.to_string())),
    }
}

#[cfg(test)]
mod parse_spec_tests {
    use super::*;

    #[test]
    fn parses_owner_repo_only() {
        let source = parse_spec("anthropics/skills").unwrap();
        assert_eq!(source.owner, "anthropics");
        assert_eq!(source.repo, "skills");
        assert_eq!(source.path, "");
        assert_eq!(source.git_ref, None);
    }

    #[test]
    fn parses_owner_repo_with_path() {
        let source = parse_spec("anthropics/skills/pdf").unwrap();
        assert_eq!(source.owner, "anthropics");
        assert_eq!(source.repo, "skills");
        assert_eq!(source.path, "pdf");
        assert_eq!(source.git_ref, None);
    }

    #[test]
    fn parses_owner_repo_with_ref() {
        let source = parse_spec("anthropics/skills@main").unwrap();
        assert_eq!(source.owner, "anthropics");
        assert_eq!(source.repo, "skills");
        assert_eq!(source.path, "");
        assert_eq!(source.git_ref, Some("main".to_string()));
    }

    #[test]
    fn parses_owner_repo_path_and_ref() {
        let source = parse_spec("anthropics/skills/pdf@v1.2.0").unwrap();
        assert_eq!(source.owner, "anthropics");
        assert_eq!(source.repo, "skills");
        assert_eq!(source.path, "pdf");
        assert_eq!(source.git_ref, Some("v1.2.0".to_string()));
    }

    #[test]
    fn parses_nested_path() {
        let source = parse_spec("anthropics/skills/nested/pdf").unwrap();
        assert_eq!(source.path, "nested/pdf");
    }

    #[test]
    fn rejects_missing_repo() {
        let result = parse_spec("anthropics");
        assert!(matches!(result, Err(SkillHostError::InvalidSpec(_))));
    }

    #[test]
    fn rejects_empty_spec() {
        let result = parse_spec("");
        assert!(matches!(result, Err(SkillHostError::InvalidSpec(_))));
    }

    #[test]
    fn parsed_source_defaults_to_github_host() {
        let source = parse_spec("anthropics/skills").unwrap();
        assert_eq!(source.host, Host::GitHub);
    }
}

use std::path::PathBuf;

/// A minimal GitHub REST API client. `api_base` defaults to
/// `https://api.github.com` but is overridable so tests can point it at a
/// local `wiremock` server instead of the real network.
pub struct GithubClient {
    http: reqwest::Client,
    api_base: String,
    token: Option<String>,
}

#[derive(serde::Deserialize)]
struct RepoInfo {
    default_branch: String,
}

#[derive(serde::Deserialize)]
struct CommitInfo {
    sha: String,
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ContentsResponse {
    Directory(Vec<ContentsEntry>),
    // The `File` case is only ever matched against with `_` (see
    // `fetch_directory_files` below) — its payload just needs to deserialize
    // successfully so `serde(untagged)` can distinguish it from `Directory`.
    #[allow(dead_code)]
    File(ContentsEntry),
}

#[derive(serde::Deserialize, Clone)]
struct ContentsEntry {
    // Present in GitHub's API response but never read here — `path` is what
    // callers key off of.
    #[allow(dead_code)]
    name: String,
    path: String,
    #[serde(rename = "type")]
    entry_type: String,
    download_url: Option<String>,
}

impl GithubClient {
    pub fn new(token: Option<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client with basic timeout config should always build"),
            api_base: "https://api.github.com".to_string(),
            token,
        }
    }

    /// Test-only: builds a client pointed at a fake API base (e.g. a
    /// `wiremock::MockServer`'s URI) instead of the real GitHub API.
    #[cfg(test)]
    pub(crate) fn new_for_test(token: Option<String>, api_base: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client with basic timeout config should always build"),
            api_base,
            token,
        }
    }

    fn request(&self, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.get(url).header("User-Agent", "local-code");
        if let Some(token) = &self.token {
            let is_github_host = url.starts_with("https://api.github.com")
                || url.starts_with("https://raw.githubusercontent.com")
                || url.starts_with(&self.api_base); // covers the test-mock api_base override
            if is_github_host {
                req = req.header("Authorization", format!("Bearer {token}"));
            }
        }
        req
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, url: &str) -> Result<T, SkillHostError> {
        let response = self.request(url).send().await.map_err(SkillHostError::Request)?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(SkillHostError::Api { status: status.as_u16(), url: url.to_string(), body });
        }
        response.json::<T>().await.map_err(SkillHostError::Request)
    }

    /// Resolves the repo's default branch name (used when the user didn't
    /// supply an explicit `@ref`).
    pub async fn resolve_default_branch(&self, owner: &str, repo: &str) -> Result<String, SkillHostError> {
        let url = format!("{}/repos/{owner}/{repo}", self.api_base);
        let info: RepoInfo = self.get_json(&url).await?;
        Ok(info.default_branch)
    }

    /// Resolves a branch/tag/SHA `git_ref` to a concrete commit SHA, so the
    /// subsequent directory fetch is a stable snapshot even if the branch
    /// moves mid-fetch, and so `update_skill` has something to compare
    /// against later.
    pub async fn resolve_commit_sha(&self, owner: &str, repo: &str, git_ref: &str) -> Result<String, SkillHostError> {
        let url = format!("{}/repos/{owner}/{repo}/commits/{}", self.api_base, urlencoding_ref(git_ref));
        let info: CommitInfo = self.get_json(&url).await?;
        Ok(info.sha)
    }

    /// Recursively fetches every file under `path` (repo-relative) at
    /// `commit_sha`, returning each with a path relative to `path` itself.
    /// Errors with `SkillHostError::NotADirectory` if `path` points at a single
    /// file rather than a directory (skills must be installed from a
    /// directory per the design spec).
    pub async fn fetch_directory_files(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        commit_sha: &str,
    ) -> Result<Vec<FetchedFile>, SkillHostError> {
        let mut files = Vec::new();
        self.fetch_directory_files_into(owner, repo, path, path, commit_sha, &mut files).await?;
        Ok(files)
    }

    fn fetch_directory_files_into<'a>(
        &'a self,
        owner: &'a str,
        repo: &'a str,
        base_path: &'a str,
        current_path: &'a str,
        commit_sha: &'a str,
        out: &'a mut Vec<FetchedFile>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), SkillHostError>> + Send + 'a>> {
        Box::pin(async move {
            let url = format!(
                "{}/repos/{owner}/{repo}/contents/{current_path}?ref={commit_sha}",
                self.api_base
            );
            let response: ContentsResponse = self.get_json(&url).await?;
            let entries = match response {
                ContentsResponse::Directory(entries) => entries,
                ContentsResponse::File(_) => {
                    return Err(SkillHostError::NotADirectory(current_path.to_string()));
                }
            };

            for entry in entries {
                match entry.entry_type.as_str() {
                    "dir" => {
                        self.fetch_directory_files_into(owner, repo, base_path, &entry.path, commit_sha, out).await?;
                    }
                    "file" => {
                        let download_url = entry.download_url.ok_or_else(|| SkillHostError::Api {
                            status: 0,
                            url: entry.path.clone(),
                            body: "file entry missing download_url".to_string(),
                        })?;
                        let response = self.request(&download_url).send().await.map_err(SkillHostError::Request)?;
                        let status = response.status();
                        if !status.is_success() {
                            let body = response.text().await.unwrap_or_default();
                            return Err(SkillHostError::Api { status: status.as_u16(), url: download_url, body });
                        }
                        let bytes = response.bytes().await.map_err(SkillHostError::Request)?.to_vec();
                        let relative = entry.path.strip_prefix(base_path).unwrap_or(&entry.path).trim_start_matches('/');
                        out.push(FetchedFile { relative_path: PathBuf::from(relative), bytes });
                    }
                    _ => {} // symlinks/submodules: skip, not relevant to skill content
                }
            }
            Ok(())
        })
    }
}

/// GitHub ref names can contain `/` (e.g. `feature/x`); percent-encode just
/// that character so the commits-endpoint path segment stays well-formed.
fn urlencoding_ref(git_ref: &str) -> String {
    git_ref.replace('/', "%2F")
}

#[cfg(test)]
mod github_client_tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn resolves_default_branch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "default_branch": "main"
            })))
            .mount(&server)
            .await;

        let client = GithubClient::new_for_test(None, server.uri());
        let branch = client.resolve_default_branch("acme", "widgets").await.unwrap();
        assert_eq!(branch, "main");
    }

    #[tokio::test]
    async fn resolves_commit_sha_for_a_ref() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/commits/main"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "sha": "abc123"
            })))
            .mount(&server)
            .await;

        let client = GithubClient::new_for_test(None, server.uri());
        let sha = client.resolve_commit_sha("acme", "widgets", "main").await.unwrap();
        assert_eq!(sha, "abc123");
    }

    #[tokio::test]
    async fn sends_bearer_token_when_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets"))
            .and(header("Authorization", "Bearer test-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "default_branch": "main"
            })))
            .mount(&server)
            .await;

        let client = GithubClient::new_for_test(Some("test-token".to_string()), server.uri());
        let branch = client.resolve_default_branch("acme", "widgets").await.unwrap();
        assert_eq!(branch, "main");
    }

    #[tokio::test]
    async fn fetches_files_from_a_flat_directory() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/contents/skills/pdf"))
            .and(query_param("ref", "abc123"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "SKILL.md",
                    "path": "skills/pdf/SKILL.md",
                    "type": "file",
                    "download_url": format!("{}/raw/SKILL.md", server.uri())
                }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/raw/SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("---\nname: pdf\n---\nbody"))
            .mount(&server)
            .await;

        let client = GithubClient::new_for_test(None, server.uri());
        let files = client.fetch_directory_files("acme", "widgets", "skills/pdf", "abc123").await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, PathBuf::from("SKILL.md"));
        assert_eq!(files[0].bytes, b"---\nname: pdf\n---\nbody");
    }

    #[tokio::test]
    async fn fetches_files_from_nested_subdirectories() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/contents/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "SKILL.md",
                    "path": "skills/pdf/SKILL.md",
                    "type": "file",
                    "download_url": format!("{}/raw/SKILL.md", server.uri())
                },
                {
                    "name": "reference",
                    "path": "skills/pdf/reference",
                    "type": "dir",
                    "download_url": null
                }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/contents/skills/pdf/reference"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "notes.md",
                    "path": "skills/pdf/reference/notes.md",
                    "type": "file",
                    "download_url": format!("{}/raw/notes.md", server.uri())
                }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET")).and(path("/raw/SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("skill body")).mount(&server).await;
        Mock::given(method("GET")).and(path("/raw/notes.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("notes")).mount(&server).await;

        let client = GithubClient::new_for_test(None, server.uri());
        let mut files = client.fetch_directory_files("acme", "widgets", "skills/pdf", "abc123").await.unwrap();
        files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].relative_path, PathBuf::from("SKILL.md"));
        assert_eq!(files[1].relative_path, PathBuf::from("reference/notes.md"));
    }

    #[tokio::test]
    async fn errors_when_path_points_at_a_single_file() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/contents/skills/pdf/SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "name": "SKILL.md",
                "path": "skills/pdf/SKILL.md",
                "type": "file",
                "download_url": "https://example.invalid/SKILL.md"
            })))
            .mount(&server)
            .await;

        let client = GithubClient::new_for_test(None, server.uri());
        let result = client.fetch_directory_files("acme", "widgets", "skills/pdf/SKILL.md", "abc123").await;
        assert!(matches!(result, Err(SkillHostError::NotADirectory(_))));
    }

    #[tokio::test]
    async fn surfaces_api_errors_with_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;

        let client = GithubClient::new_for_test(None, server.uri());
        let result = client.resolve_default_branch("acme", "widgets").await;
        assert!(matches!(result, Err(SkillHostError::Api { status: 404, .. })));
    }

    #[tokio::test]
    async fn fetch_directory_files_errors_when_raw_download_returns_non_2xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repos/acme/widgets/contents/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "name": "SKILL.md",
                    "path": "skills/pdf/SKILL.md",
                    "type": "file",
                    "download_url": format!("{}/raw/SKILL.md", server.uri())
                }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/raw/SKILL.md"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .mount(&server)
            .await;

        let client = GithubClient::new_for_test(None, server.uri());
        let result = client.fetch_directory_files("acme", "widgets", "skills/pdf", "abc123").await;
        assert!(matches!(result, Err(SkillHostError::Api { status: 500, .. })));
    }

    #[tokio::test]
    async fn does_not_send_bearer_token_to_non_github_host() {
        let client = GithubClient::new_for_test(Some("test-token".to_string()), "http://127.0.0.1:1".to_string());
        let request = client.request("https://example.invalid/SKILL.md");
        let built = request.build().unwrap();
        assert!(built.headers().get("Authorization").is_none());
    }
}
