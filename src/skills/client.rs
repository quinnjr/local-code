use crate::skills::bitbucket::BitbucketClient;
use crate::skills::github::GithubClient;
use crate::skills::gitlab::GitlabClient;
use crate::skills::types::{FetchedFile, SkillHostError};

/// Wraps the three concrete host clients behind one set of methods, matching
/// this codebase's existing enum-dispatch convention (see
/// `McpTransportConfig` in `src/config/mcp_servers.rs`) rather than a
/// trait/`dyn` object — the smallest change to `install.rs`'s call sites,
/// which previously took `&GithubClient` directly.
///
/// All three inherent methods take `owner`/`repo` as GitHub does (Bitbucket's
/// `workspace`/`repo_slug` map onto the same two positional strings — the
/// terminology differs but the shape is identical). For GitLab, `owner` is
/// ignored and `repo` carries the full project path (see
/// `SkillSource::owner`'s doc comment) — GitLab has no separate owner
/// component once the project path is fully resolved.
pub enum SkillClient {
    GitHub(GithubClient),
    GitLab(GitlabClient),
    Bitbucket(BitbucketClient),
}

/// Escapes `/` in a git ref for use inside a URL path segment. Shared by all
/// three host clients (each previously hand-copied a byte-identical version).
pub(crate) fn urlencoding_ref(git_ref: &str) -> String {
    git_ref.replace('/', "%2F")
}

/// Shared "send, check status, decode JSON" scaffolding for the three host
/// clients: any non-2xx becomes `SkillHostError::Api` with the body preserved
/// for diagnostics. Each client supplies its own authenticated
/// `RequestBuilder` (auth headers are the only genuinely host-specific part
/// of the HTTP layer). Extracted after review caught the three hand-copied
/// versions drifting in their error mapping.
pub(crate) async fn get_json<T: serde::de::DeserializeOwned>(
    request: reqwest::RequestBuilder,
    url: &str,
) -> Result<T, SkillHostError> {
    let response = request.send().await.map_err(SkillHostError::Request)?;
    let status = response.status();
    if !status.is_success() {
        let body = sanitize_body(response.text().await.unwrap_or_default());
        return Err(SkillHostError::Api {
            status: status.as_u16(),
            url: url.to_string(),
            body,
        });
    }
    response.json::<T>().await.map_err(SkillHostError::Request)
}

/// Strips control bytes (keeping `\n`/`\t`) from an untrusted HTTP response
/// body before it is embedded in a user-facing error: `SkillHostError::Api`'s
/// Display prints the body to the terminal, and a malicious or MITM'd host
/// could otherwise smuggle ANSI escape sequences into that output (cursor
/// moves, line rewrites, fake banners).
pub(crate) fn sanitize_body(body: String) -> String {
    if body
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        body.chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect()
    } else {
        body
    }
}

impl SkillClient {
    pub async fn resolve_default_branch(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<String, SkillHostError> {
        match self {
            SkillClient::GitHub(c) => c.resolve_default_branch(owner, repo).await,
            SkillClient::GitLab(c) => c.resolve_default_branch(repo).await,
            SkillClient::Bitbucket(c) => c.resolve_default_branch(owner, repo).await,
        }
    }

    pub async fn resolve_commit_sha(
        &self,
        owner: &str,
        repo: &str,
        git_ref: &str,
    ) -> Result<String, SkillHostError> {
        match self {
            SkillClient::GitHub(c) => c.resolve_commit_sha(owner, repo, git_ref).await,
            SkillClient::GitLab(c) => c.resolve_commit_sha(repo, git_ref).await,
            SkillClient::Bitbucket(c) => c.resolve_commit_sha(owner, repo, git_ref).await,
        }
    }

    pub async fn fetch_directory_files(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        commit_sha: &str,
    ) -> Result<Vec<FetchedFile>, SkillHostError> {
        match self {
            SkillClient::GitHub(c) => c.fetch_directory_files(owner, repo, path, commit_sha).await,
            SkillClient::GitLab(c) => c.fetch_directory_files(repo, path, commit_sha).await,
            SkillClient::Bitbucket(c) => {
                c.fetch_directory_files(owner, repo, path, commit_sha).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::skills::types::{Host, SkillSource};

    #[test]
    fn source_construction_smoke_test() {
        let source = SkillSource {
            host: Host::GitLab,
            owner: "".into(),
            repo: "group/sub/project".into(),
            path: "skills/pdf".into(),
            git_ref: None,
        };
        assert_eq!(source.owner, "");
        assert_eq!(source.repo, "group/sub/project");
    }
}

#[cfg(test)]
mod scaffolding_tests {
    use super::*;

    #[test]
    fn sanitize_body_strips_c0_and_c1_controls_but_keeps_newlines_and_tabs() {
        // `char::is_control` is Unicode Cc — C0 (incl. ESC, DEL) AND C1
        // (incl. the raw single-byte CSI U+009B), so both escape-sequence
        // introducers are stripped before a body reaches the terminal.
        let hostile = "ok line\n\u{1b}[2Jcleared\u{9b}31mred\ttab\u{7f}";
        let clean = sanitize_body(hostile.to_string());
        // Only the control bytes themselves are stripped; the now-inert
        // printable remainder of a sequence ("[2J", "31m") stays.
        assert_eq!(clean, "ok line\n[2Jcleared31mred\ttab");
        assert_eq!(sanitize_body("plain body".into()), "plain body");
    }
}
