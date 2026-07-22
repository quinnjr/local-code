use std::path::PathBuf;

use crate::skills::client::urlencoding_ref;
use crate::skills::types::{FetchedFile, SkillHostError};

/// A minimal Bitbucket Cloud REST (2.0) API client. `api_base` defaults to
/// `https://api.bitbucket.org/2.0`. Auth is HTTP Basic
/// (`username:app_password`) — Bitbucket Cloud's REST API has no
/// bearer-token concept for app passwords.
pub struct BitbucketClient {
    http: reqwest::Client,
    api_base: String,
    /// `(username, app_password)`, pre-split by the caller (`cli/skills.rs`)
    /// from the single `SecretStore`-stored `username:app_password` string.
    credentials: Option<(String, String)>,
}

#[derive(serde::Deserialize)]
struct RepoInfo {
    mainbranch: MainBranch,
}

#[derive(serde::Deserialize)]
struct MainBranch {
    name: String,
}

#[derive(serde::Deserialize)]
struct CommitInfo {
    hash: String,
}

#[derive(serde::Deserialize)]
struct SrcListing {
    values: Vec<SrcEntry>,
    next: Option<String>,
}

#[derive(serde::Deserialize, Clone)]
struct SrcEntry {
    path: String,
    #[serde(rename = "type")]
    entry_type: String, // "commit_file" | "commit_directory"
    links: SrcLinks,
}

#[derive(serde::Deserialize, Clone)]
struct SrcLinks {
    #[serde(rename = "self")]
    self_link: SelfLink,
}

#[derive(serde::Deserialize, Clone)]
struct SelfLink {
    href: String,
}

impl BitbucketClient {
    pub fn new(credentials: Option<(String, String)>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client with basic timeout config should always build"),
            api_base: "https://api.bitbucket.org/2.0".to_string(),
            credentials,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_for_test(credentials: Option<(String, String)>, api_base: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client with basic timeout config should always build"),
            api_base,
            credentials,
        }
    }

    fn request(&self, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.get(url).header("User-Agent", "local-code");
        if let Some((user, pass)) = &self.credentials {
            req = req.basic_auth(user, Some(pass));
        }
        req
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<T, SkillHostError> {
        crate::skills::client::get_json(self.request(url), url).await
    }

    pub async fn resolve_default_branch(
        &self,
        workspace: &str,
        repo_slug: &str,
    ) -> Result<String, SkillHostError> {
        let url = format!("{}/repositories/{workspace}/{repo_slug}", self.api_base);
        let info: RepoInfo = self.get_json(&url).await?;
        Ok(info.mainbranch.name)
    }

    pub async fn resolve_commit_sha(
        &self,
        workspace: &str,
        repo_slug: &str,
        revision: &str,
    ) -> Result<String, SkillHostError> {
        let url = format!(
            "{}/repositories/{workspace}/{repo_slug}/commit/{}",
            self.api_base,
            urlencoding_ref(revision)
        );
        let info: CommitInfo = self.get_json(&url).await?;
        Ok(info.hash)
    }

    /// Fetches every file under `path` at `revision` (normally a resolved
    /// commit hash). Recurses into `commit_directory` entries the way
    /// `github.rs` does, following Bitbucket's `next` pagination link for
    /// each directory listing, and downloads file bytes via each entry's
    /// `links.self.href`.
    pub async fn fetch_directory_files(
        &self,
        workspace: &str,
        repo_slug: &str,
        path: &str,
        revision: &str,
    ) -> Result<Vec<FetchedFile>, SkillHostError> {
        self.fetch_into(workspace, repo_slug, path, path, revision)
            .await
    }

    fn fetch_into<'a>(
        &'a self,
        workspace: &'a str,
        repo_slug: &'a str,
        base_path: &'a str,
        current_path: &'a str,
        revision: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Vec<FetchedFile>, SkillHostError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut files = Vec::new();
            let mut url = Some(format!(
                "{}/repositories/{workspace}/{repo_slug}/src/{revision}/{current_path}",
                self.api_base
            ));
            while let Some(page_url) = url {
                let listing: SrcListing = self.get_json(&page_url).await?;
                let mut dir_paths = Vec::new();
                let mut file_entries = Vec::new();
                for entry in listing.values {
                    match entry.entry_type.as_str() {
                        "commit_directory" => dir_paths.push(entry.path),
                        "commit_file" => file_entries.push(entry),
                        _ => {} // symlinks/submodules: skip
                    }
                }

                // Sibling subtree recursions and this page's file downloads
                // are all independent — run them as one concurrent wave
                // rather than a serial depth-first chain. Pages themselves
                // stay serial: each `next` URL comes from the previous page's
                // response.
                let subtrees =
                    futures::future::join_all(dir_paths.into_iter().map(|dir_path| async move {
                        self.fetch_into(workspace, repo_slug, base_path, &dir_path, revision)
                            .await
                    }));
                let downloads =
                    futures::future::join_all(file_entries.into_iter().map(|entry| async move {
                        let response = self.request(&entry.links.self_link.href).send().await?;
                        let status = response.status();
                        if !status.is_success() {
                            let body = crate::skills::client::sanitize_body(
                                response.text().await.unwrap_or_default(),
                            );
                            return Err(SkillHostError::Api {
                                status: status.as_u16(),
                                url: entry.links.self_link.href.clone(),
                                body,
                            });
                        }
                        let bytes = response
                            .bytes()
                            .await
                            .map_err(SkillHostError::Request)?
                            .to_vec();
                        let relative = entry
                            .path
                            .strip_prefix(base_path)
                            .unwrap_or(&entry.path)
                            .trim_start_matches('/');
                        Ok(FetchedFile {
                            relative_path: PathBuf::from(relative),
                            bytes,
                        })
                    }));

                let (subtree_results, download_results) =
                    futures::future::join(subtrees, downloads).await;
                // Subtrees first, matching the old depth-first accumulation
                // order within each page.
                for result in subtree_results {
                    files.extend(result?);
                }
                for result in download_results {
                    files.push(result?);
                }
                url = listing.next;
            }
            Ok(files)
        })
    }
}

#[cfg(test)]
mod bitbucket_client_tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn resolves_default_branch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repositories/acme/widgets"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"mainbranch": {"name": "main"}})),
            )
            .mount(&server)
            .await;
        let client = BitbucketClient::new_for_test(None, server.uri());
        assert_eq!(
            client
                .resolve_default_branch("acme", "widgets")
                .await
                .unwrap(),
            "main"
        );
    }

    #[tokio::test]
    async fn resolves_commit_sha_using_bitbuckets_hash_field() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repositories/acme/widgets/commit/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"hash": "abc123"})),
            )
            .mount(&server)
            .await;
        let client = BitbucketClient::new_for_test(None, server.uri());
        assert_eq!(
            client
                .resolve_commit_sha("acme", "widgets", "main")
                .await
                .unwrap(),
            "abc123"
        );
    }

    #[tokio::test]
    async fn sends_basic_auth_header_when_credentials_present() {
        let server = MockServer::start().await;
        // "user:pass" base64 == "dXNlcjpwYXNz"
        Mock::given(method("GET"))
            .and(path("/repositories/acme/widgets"))
            .and(header("Authorization", "Basic dXNlcjpwYXNz"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"mainbranch": {"name": "main"}})),
            )
            .mount(&server)
            .await;
        let client = BitbucketClient::new_for_test(
            Some(("user".to_string(), "pass".to_string())),
            server.uri(),
        );
        assert_eq!(
            client
                .resolve_default_branch("acme", "widgets")
                .await
                .unwrap(),
            "main"
        );
    }

    #[tokio::test]
    async fn fetches_files_from_a_flat_directory() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repositories/acme/widgets/src/abc123/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "values": [{
                    "path": "skills/pdf/SKILL.md",
                    "type": "commit_file",
                    "links": {"self": {"href": format!("{}/raw/SKILL.md", server.uri())}}
                }],
                "next": null
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/raw/SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("---\nname: pdf\n---\nbody"))
            .mount(&server)
            .await;

        let client = BitbucketClient::new_for_test(None, server.uri());
        let files = client
            .fetch_directory_files("acme", "widgets", "skills/pdf", "abc123")
            .await
            .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, std::path::PathBuf::from("SKILL.md"));
    }

    #[tokio::test]
    async fn fetches_files_from_nested_subdirectories() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/repositories/acme/widgets/src/abc123/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "values": [
                    {"path": "skills/pdf/SKILL.md", "type": "commit_file",
                     "links": {"self": {"href": format!("{}/raw/SKILL.md", server.uri())}}},
                    {"path": "skills/pdf/reference", "type": "commit_directory",
                     "links": {"self": {"href": format!("{}/repositories/acme/widgets/src/abc123/skills/pdf/reference", server.uri())}}}
                ],
                "next": null
            })))
            .mount(&server).await;
        Mock::given(method("GET"))
            .and(path(
                "/repositories/acme/widgets/src/abc123/skills/pdf/reference",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "values": [{"path": "skills/pdf/reference/notes.md", "type": "commit_file",
                            "links": {"self": {"href": format!("{}/raw/notes.md", server.uri())}}}],
                "next": null
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/raw/SKILL.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("skill"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/raw/notes.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("notes"))
            .mount(&server)
            .await;

        let client = BitbucketClient::new_for_test(None, server.uri());
        let mut files = client
            .fetch_directory_files("acme", "widgets", "skills/pdf", "abc123")
            .await
            .unwrap();
        files.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
        assert_eq!(files.len(), 2);
        assert_eq!(
            files[1].relative_path,
            std::path::PathBuf::from("reference/notes.md")
        );
    }

    #[tokio::test]
    async fn follows_next_link_pagination() {
        let server = MockServer::start().await;
        let page2 = format!(
            "{}/repositories/acme/widgets/src/abc123/skills/pdf?page=2",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path("/repositories/acme/widgets/src/abc123/skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "values": [{"path": "skills/pdf/a.md", "type": "commit_file",
                            "links": {"self": {"href": format!("{}/raw/a.md", server.uri())}}}],
                "next": page2
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/repositories/acme/widgets/src/abc123/skills/pdf"))
            .and(wiremock::matchers::query_param("page", "2"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "values": [{"path": "skills/pdf/b.md", "type": "commit_file",
                            "links": {"self": {"href": format!("{}/raw/b.md", server.uri())}}}],
                "next": null
            })))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/raw/a.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("a"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/raw/b.md"))
            .respond_with(ResponseTemplate::new(200).set_body_string("b"))
            .mount(&server)
            .await;

        let client = BitbucketClient::new_for_test(None, server.uri());
        let files = client
            .fetch_directory_files("acme", "widgets", "skills/pdf", "abc123")
            .await
            .unwrap();
        assert_eq!(files.len(), 2);
    }

    #[tokio::test]
    async fn surfaces_api_errors_with_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/repositories/acme/widgets"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;
        let client = BitbucketClient::new_for_test(None, server.uri());
        let result = client.resolve_default_branch("acme", "widgets").await;
        assert!(matches!(
            result,
            Err(SkillHostError::Api { status: 404, .. })
        ));
    }
}
