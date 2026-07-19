use std::path::PathBuf;

use crate::skills::client::urlencoding_ref;
use crate::skills::types::{FetchedFile, SkillHostError};

/// A minimal GitLab REST (v4) API client. `api_base` defaults to
/// `https://gitlab.com/api/v4` but is overridable so tests can point it at a
/// local `wiremock` server instead of the real network. Auth uses GitLab's
/// `PRIVATE-TOKEN` header, not `Authorization: Bearer` — GitLab personal
/// access tokens are not bearer tokens in the OAuth sense here.
pub struct GitlabClient {
    http: reqwest::Client,
    api_base: String,
    token: Option<String>,
}

#[derive(serde::Deserialize)]
struct ProjectInfo {
    default_branch: String,
}

#[derive(serde::Deserialize)]
struct CommitInfo {
    id: String, // GitLab's field name for the commit SHA (not `sha`)
}

#[derive(serde::Deserialize, Clone)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    entry_type: String, // "blob" | "tree"
}

impl GitlabClient {
    pub fn new(token: Option<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .connect_timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("reqwest client with basic timeout config should always build"),
            api_base: "https://gitlab.com/api/v4".to_string(),
            token,
        }
    }

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
            req = req.header("PRIVATE-TOKEN", token);
        }
        req
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<T, SkillHostError> {
        crate::skills::client::get_json(self.request(url), url).await
    }

    /// URL-encodes a GitLab project path (`group/subgroup/project`) for use
    /// as the `:id` path segment GitLab's API accepts in place of a numeric
    /// project ID.
    fn encoded_id(project_path: &str) -> String {
        urlencoding_path(project_path)
    }

    pub async fn resolve_default_branch(
        &self,
        project_path: &str,
    ) -> Result<String, SkillHostError> {
        let url = format!(
            "{}/projects/{}",
            self.api_base,
            Self::encoded_id(project_path)
        );
        let info: ProjectInfo = self.get_json(&url).await?;
        Ok(info.default_branch)
    }

    pub async fn resolve_commit_sha(
        &self,
        project_path: &str,
        git_ref: &str,
    ) -> Result<String, SkillHostError> {
        let url = format!(
            "{}/projects/{}/repository/commits/{}",
            self.api_base,
            Self::encoded_id(project_path),
            urlencoding_ref(git_ref)
        );
        let info: CommitInfo = self.get_json(&url).await?;
        Ok(info.id)
    }

    /// Fetches every file under `path` (project-relative) at `commit_sha`,
    /// via GitLab's recursive tree endpoint (one call per page, `Link`-header
    /// paginated) plus one raw-content call per blob. `path == ""` means the
    /// project root.
    pub async fn fetch_directory_files(
        &self,
        project_path: &str,
        path: &str,
        commit_sha: &str,
    ) -> Result<Vec<FetchedFile>, SkillHostError> {
        let id = Self::encoded_id(project_path);
        let mut entries: Vec<TreeEntry> = Vec::new();
        let mut page_url = Some(format!(
            "{}/projects/{id}/repository/tree?path={}&ref={commit_sha}&recursive=true&per_page=100",
            self.api_base,
            urlencoding_path(path)
        ));

        while let Some(url) = page_url {
            let response = self.request(&url).send().await?;
            let status = response.status();
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(SkillHostError::Api {
                    status: status.as_u16(),
                    url,
                    body,
                });
            }
            let next = next_link_from_header(response.headers());
            let page: Vec<TreeEntry> = response.json().await.map_err(SkillHostError::Request)?;
            entries.extend(page);
            page_url = next;
        }

        // An empty tree listing plus a request that *would* have hit a file
        // (not a directory) can't be distinguished by the tree endpoint alone
        // (GitLab 404s on a nonexistent tree path the same as on a real file
        // path passed to the tree endpoint); a single "does this look like a
        // directory" check isn't available cheaply, so — mirroring the design
        // spec's scope — a `path` that resolves to a blob returns an empty
        // `entries` list here rather than a hard `NotADirectory` error, and
        // callers (`install_skill`/`update_skill`) already treat "no files
        // fetched" as `InstallError::EmptyDirectory`, which is the correct
        // user-facing outcome either way.
        // Every blob's raw-content fetch is independent — download them
        // concurrently instead of one round trip at a time, since this is
        // the dominant cost for skills with more than a couple of files.
        let downloads = entries
            .iter()
            .filter(|e| e.entry_type == "blob")
            .map(|entry| {
                let id = id.clone();
                async move {
                    let raw_url = format!(
                        "{}/projects/{id}/repository/files/{}/raw?ref={commit_sha}",
                        self.api_base,
                        urlencoding_path(&entry.path)
                    );
                    let response = self.request(&raw_url).send().await?;
                    let status = response.status();
                    if !status.is_success() {
                        let body = response.text().await.unwrap_or_default();
                        return Err(SkillHostError::Api {
                            status: status.as_u16(),
                            url: raw_url,
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
                        .strip_prefix(path)
                        .unwrap_or(&entry.path)
                        .trim_start_matches('/');
                    Ok(FetchedFile {
                        relative_path: PathBuf::from(relative),
                        bytes,
                    })
                }
            });
        futures::future::join_all(downloads)
            .await
            .into_iter()
            .collect()
    }

    /// Walks prefixes of `raw_path` (split on `/`), longest first, calling
    /// `GET /projects/:encoded_prefix` for each until one resolves (200) —
    /// see design spec §2 "Deferred GitLab project-path resolution". Capped
    /// at 10 path segments.
    pub async fn resolve_project_path(
        &self,
        raw_path: &str,
    ) -> Result<(String, String), SkillHostError> {
        let segments: Vec<&str> = raw_path.split('/').filter(|s| !s.is_empty()).collect();
        if segments.is_empty() || segments.len() > 10 {
            return Err(SkillHostError::InvalidSpec(raw_path.to_string()));
        }

        for split_at in (1..=segments.len()).rev() {
            let candidate = segments[..split_at].join("/");
            let url = format!(
                "{}/projects/{}",
                self.api_base,
                Self::encoded_id(&candidate)
            );
            let response = self.request(&url).send().await?;
            let status = response.status();
            if status.is_success() {
                let in_repo_path = segments[split_at..].join("/");
                return Ok((candidate, in_repo_path));
            }
            // Only 404 means "not this prefix, keep walking". An auth or
            // rate-limit failure (401/403/429) or a server error would
            // previously be swallowed and misreported as an invalid spec,
            // sending the user to fix a spec that was fine all along.
            if status != reqwest::StatusCode::NOT_FOUND {
                let body = response.text().await.unwrap_or_default();
                return Err(SkillHostError::Api {
                    status: status.as_u16(),
                    url,
                    body,
                });
            }
        }
        Err(SkillHostError::InvalidSpec(raw_path.to_string()))
    }
}

/// GitLab requires the full `:id` path segment percent-encoded (including
/// `/`), unlike the ref-encoding helper below which only needs to escape `/`.
fn urlencoding_path(path: &str) -> String {
    path.split('/')
        .map(percent_encode_segment)
        .collect::<Vec<_>>()
        .join("%2F")
}

fn percent_encode_segment(segment: &str) -> String {
    // Minimal percent-encoding sufficient for path segments used here (no
    // spaces or unicode expected in practice, but encode defensively).
    let mut out = String::new();
    for byte in segment.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Parses the `rel="next"` URL out of GitLab's `Link` response header, if
/// present (RFC 5988 format: `<url>; rel="next", <url2>; rel="last"`).
fn next_link_from_header(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let link = headers.get("Link")?.to_str().ok()?;
    link.split(',').find_map(|part| {
        let part = part.trim();
        if !part.contains("rel=\"next\"") {
            return None;
        }
        let start = part.find('<')? + 1;
        let end = part.find('>')?;
        Some(part[start..end].to_string())
    })
}

#[cfg(test)]
mod gitlab_client_tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn resolves_default_branch() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fwidgets"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"default_branch": "main"})),
            )
            .mount(&server)
            .await;
        let client = GitlabClient::new_for_test(None, server.uri());
        assert_eq!(
            client.resolve_default_branch("acme/widgets").await.unwrap(),
            "main"
        );
    }

    #[tokio::test]
    async fn resolves_commit_sha_using_gitlabs_id_field() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fwidgets/repository/commits/main"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"id": "abc123"})),
            )
            .mount(&server)
            .await;
        let client = GitlabClient::new_for_test(None, server.uri());
        assert_eq!(
            client
                .resolve_commit_sha("acme/widgets", "main")
                .await
                .unwrap(),
            "abc123"
        );
    }

    #[tokio::test]
    async fn sends_private_token_header_when_present() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fwidgets"))
            .and(header("PRIVATE-TOKEN", "test-token"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"default_branch": "main"})),
            )
            .mount(&server)
            .await;
        let client = GitlabClient::new_for_test(Some("test-token".to_string()), server.uri());
        assert_eq!(
            client.resolve_default_branch("acme/widgets").await.unwrap(),
            "main"
        );
    }

    #[tokio::test]
    async fn fetches_files_from_a_flat_directory() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fwidgets/repository/tree"))
            .and(query_param("path", "skills/pdf"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "skills/pdf/SKILL.md", "type": "blob"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/projects/acme%2Fwidgets/repository/files/skills%2Fpdf%2FSKILL.md/raw",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("---\nname: pdf\n---\nbody"))
            .mount(&server)
            .await;

        let client = GitlabClient::new_for_test(None, server.uri());
        let files = client
            .fetch_directory_files("acme/widgets", "skills/pdf", "abc123")
            .await
            .unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].relative_path, std::path::PathBuf::from("SKILL.md"));
    }

    #[tokio::test]
    async fn fetches_files_from_nested_subdirectories_in_one_recursive_call() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fwidgets/repository/tree"))
            .and(query_param("recursive", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {"path": "skills/pdf/SKILL.md", "type": "blob"},
                {"path": "skills/pdf/reference", "type": "tree"},
                {"path": "skills/pdf/reference/notes.md", "type": "blob"}
            ])))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/projects/acme%2Fwidgets/repository/files/skills%2Fpdf%2FSKILL.md/raw",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("skill"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/projects/acme%2Fwidgets/repository/files/skills%2Fpdf%2Freference%2Fnotes.md/raw",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("notes"))
            .mount(&server)
            .await;

        let client = GitlabClient::new_for_test(None, server.uri());
        let mut files = client
            .fetch_directory_files("acme/widgets", "skills/pdf", "abc123")
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
    async fn follows_link_header_pagination() {
        let server = MockServer::start().await;
        let page2_url = format!(
            "{}/projects/acme%2Fwidgets/repository/tree?page=2",
            server.uri()
        );
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fwidgets/repository/tree"))
            .and(query_param("path", "skills/pdf"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!([{"path": "skills/pdf/a.md", "type": "blob"}]))
                    .insert_header("Link", format!("<{page2_url}>; rel=\"next\"")),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fwidgets/repository/tree"))
            .and(query_param("page", "2"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(
                    serde_json::json!([{"path": "skills/pdf/b.md", "type": "blob"}]),
                ),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/projects/acme%2Fwidgets/repository/files/skills%2Fpdf%2Fa.md/raw",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("a"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(
                "/projects/acme%2Fwidgets/repository/files/skills%2Fpdf%2Fb.md/raw",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string("b"))
            .mount(&server)
            .await;

        let client = GitlabClient::new_for_test(None, server.uri());
        let files = client
            .fetch_directory_files("acme/widgets", "skills/pdf", "abc123")
            .await
            .unwrap();
        assert_eq!(files.len(), 2);
    }

    #[tokio::test]
    async fn surfaces_api_errors_with_status_and_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fwidgets"))
            .respond_with(ResponseTemplate::new(404).set_body_string("Not Found"))
            .mount(&server)
            .await;
        let client = GitlabClient::new_for_test(None, server.uri());
        let result = client.resolve_default_branch("acme/widgets").await;
        assert!(matches!(
            result,
            Err(SkillHostError::Api { status: 404, .. })
        ));
    }

    // resolve_project_path

    #[tokio::test]
    async fn resolve_project_path_finds_a_top_level_project() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/acme%2Fwidgets"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"default_branch": "main"})),
            )
            .mount(&server)
            .await;
        let client = GitlabClient::new_for_test(None, server.uri());
        let (project, in_repo) = client
            .resolve_project_path("acme/widgets/skills/pdf")
            .await
            .unwrap();
        assert_eq!(project, "acme/widgets");
        assert_eq!(in_repo, "skills/pdf");
    }

    #[tokio::test]
    async fn resolve_project_path_finds_a_one_level_nested_project() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/group%2Fsub%2Fproj%2Fskills"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/projects/group%2Fsub%2Fproj"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"default_branch": "main"})),
            )
            .mount(&server)
            .await;
        let client = GitlabClient::new_for_test(None, server.uri());
        let (project, in_repo) = client
            .resolve_project_path("group/sub/proj/skills")
            .await
            .unwrap();
        assert_eq!(project, "group/sub/proj");
        assert_eq!(in_repo, "skills");
    }

    #[tokio::test]
    async fn resolve_project_path_finds_a_two_level_nested_project() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/projects/a%2Fb%2Fc%2Fd"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/projects/a%2Fb%2Fc"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"default_branch": "main"})),
            )
            .mount(&server)
            .await;
        let client = GitlabClient::new_for_test(None, server.uri());
        let (project, in_repo) = client.resolve_project_path("a/b/c/d").await.unwrap();
        assert_eq!(project, "a/b/c");
        assert_eq!(in_repo, "d");
    }

    #[tokio::test]
    async fn resolve_project_path_errors_when_no_prefix_resolves() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let client = GitlabClient::new_for_test(None, server.uri());
        let result = client.resolve_project_path("a/b/c").await;
        assert!(matches!(result, Err(SkillHostError::InvalidSpec(_))));
    }

    #[tokio::test]
    async fn resolve_project_path_surfaces_auth_failures_instead_of_invalid_spec() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403).set_body_string("forbidden"))
            .mount(&server)
            .await;
        let client = GitlabClient::new_for_test(None, server.uri());
        let result = client.resolve_project_path("acme/widgets").await;
        assert!(
            matches!(result, Err(SkillHostError::Api { status: 403, .. })),
            "a 403 must not be misreported as an invalid spec: {result:?}"
        );
    }

    #[tokio::test]
    async fn resolve_project_path_enforces_the_depth_cap() {
        let client = GitlabClient::new_for_test(None, "http://127.0.0.1:1".to_string());
        let too_deep = (0..11)
            .map(|i| format!("seg{i}"))
            .collect::<Vec<_>>()
            .join("/");
        let result = client.resolve_project_path(&too_deep).await;
        assert!(matches!(result, Err(SkillHostError::InvalidSpec(_))));
    }
}
