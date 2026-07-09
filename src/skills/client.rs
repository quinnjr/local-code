// src/skills/client.rs

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

impl SkillClient {
    pub async fn resolve_default_branch(&self, owner: &str, repo: &str) -> Result<String, SkillHostError> {
        match self {
            SkillClient::GitHub(c) => c.resolve_default_branch(owner, repo).await,
            SkillClient::GitLab(c) => c.resolve_default_branch(repo).await,
            SkillClient::Bitbucket(c) => c.resolve_default_branch(owner, repo).await,
        }
    }

    pub async fn resolve_commit_sha(&self, owner: &str, repo: &str, git_ref: &str) -> Result<String, SkillHostError> {
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
            SkillClient::Bitbucket(c) => c.fetch_directory_files(owner, repo, path, commit_sha).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
