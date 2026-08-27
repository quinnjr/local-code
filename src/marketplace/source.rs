use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::skills::types::{Host, SkillHostError, SkillSource};

/// Where a registered marketplace's catalog lives, persisted in
/// `marketplaces.toml`. Kebab-case tagged enum, matching this codebase's
/// enum-dispatch convention (`McpTransportConfig`, `SkillClient`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case")]
pub enum MarketplaceSource {
    /// A git-hosted repository fetched on demand via the skill host clients.
    Remote {
        host: Host,
        owner: String,
        repo: String,
        /// Marketplace root within the repo; `""` means repo root (the
        /// catalog then lives at `<path>/.claude-plugin/marketplace.json`).
        #[serde(default)]
        path: String,
        /// The effective ref recorded at add time (the user-supplied `@ref`
        /// or the repo's then-default branch).
        #[serde(rename = "ref")]
        git_ref: String,
    },
    /// A marketplace directory on the local filesystem, read live (nothing
    /// is copied — edits to the directory are visible immediately).
    Local { path: PathBuf },
}

impl MarketplaceSource {
    /// One-line rendering for `marketplace list`, e.g.
    /// `acme/plugins@main`, `gl:group/plugins@main`, or `local:/abs/dir`.
    /// Follows the same convention as `skills list`: GitHub renders bare,
    /// the other hosts get their `gl:`/`bb:` prefix.
    pub fn summary(&self) -> String {
        match self {
            MarketplaceSource::Remote {
                host,
                owner,
                repo,
                path,
                git_ref,
            } => {
                let coords = [owner.as_str(), repo.as_str()]
                    .into_iter()
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("/");
                let path_suffix = if path.is_empty() {
                    String::new()
                } else {
                    format!("/{path}")
                };
                let bare = format!("{coords}{path_suffix}@{git_ref}");
                match host {
                    Host::GitHub => bare,
                    Host::GitLab => format!("gl:{bare}"),
                    Host::Bitbucket => format!("bb:{bare}"),
                }
            }
            MarketplaceSource::Local { path } => format!("local:{}", path.display()),
        }
    }
}

/// The result of parsing a `marketplace add` spec, as far as it can be
/// parsed synchronously (same contract as `skills::spec::parse_spec` —
/// GitLab shorthand specs still need their async project-path resolution
/// before the source is usable).
#[derive(Debug)]
pub enum ParsedAddSpec {
    /// An existing local directory (canonicalized to an absolute path).
    Local(PathBuf),
    /// A `skills::spec`-shaped remote spec (`owner/repo[/path][@ref]`,
    /// `gh:`/`gl:`/`bb:` prefixes, or a full URL of a known host).
    Remote(crate::skills::spec::ParsedSpec),
}

#[derive(Debug, thiserror::Error)]
pub enum AddSpecError {
    #[error(
        "local marketplace directory '{0}' not found (a remote spec like owner/repo is assumed otherwise)"
    )]
    LocalDirNotFound(String),
    #[error("failed to canonicalize marketplace directory '{spec}': {source}")]
    Canonicalize {
        spec: String,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Host(#[from] SkillHostError),
}

/// Parses a `marketplace add` spec. An existing directory on disk is a local
/// marketplace; a spec that *looks* local (`./`, `../`, `/` prefix) but
/// doesn't exist is an error rather than silently treated as a (nonsensical)
/// remote spec; anything else goes through `skills::spec::parse_spec`
/// (`owner/repo[/path][@ref]`, `gh:`/`gl:`/`bb:`, known-host URLs).
pub fn parse_add_spec(spec: &str) -> Result<ParsedAddSpec, AddSpecError> {
    let candidate = Path::new(spec);
    if candidate.is_dir() {
        let absolute = candidate
            .canonicalize()
            .map_err(|e| AddSpecError::Canonicalize {
                spec: spec.to_string(),
                source: e,
            })?;
        return Ok(ParsedAddSpec::Local(absolute));
    }
    if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') {
        return Err(AddSpecError::LocalDirNotFound(spec.to_string()));
    }
    Ok(ParsedAddSpec::Remote(crate::skills::spec::parse_spec(
        spec,
    )?))
}

#[derive(Debug, thiserror::Error)]
pub enum GitUrlError {
    #[error(
        "unsupported git URL '{0}' — url/git-subdir plugin sources support https:// and SSH (git@host: or ssh://) URLs on github.com, gitlab.com, and bitbucket.org, or GitHub owner/repo shorthand"
    )]
    Unsupported(String),
}

/// Parses the `url` field of `url`/`git-subdir` plugin sources into skill
/// host coordinates. Accepts `https://` and SSH (`git@host:…` scp-style or
/// `ssh://…`) URLs on the three known hosts, plus GitHub `owner/repo`
/// shorthand, all with an optional `.git` suffix. The transport scheme only
/// names how git would clone the repo — LocalCode fetches content through
/// the host APIs, so SSH URLs resolve to the same coordinates as their
/// https equivalents. GitLab URLs need no `-/tree/` marker here: unlike
/// skill specs, the in-repo path comes from the source object's own `path`
/// field, so the whole URL path is the project path and there is nothing
/// ambiguous to split.
pub fn parse_git_url(url: &str) -> Result<SkillSource, GitUrlError> {
    let trimmed = url.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);

    // SSH forms first: scp-style (`git@host:owner/repo`) has no `://` and
    // would otherwise be misread as a malformed shorthand.
    if let Some((host, path)) = ssh_host_and_path(trimmed) {
        return source_for_host_path(host, path, url);
    }

    // GitHub `owner/repo` shorthand: no scheme at all.
    if !trimmed.contains("://") && !trimmed.contains(':') {
        let mut parts = trimmed.split('/');
        if let (Some(owner), Some(repo), None) = (parts.next(), parts.next(), parts.next())
            && !owner.is_empty()
            && !repo.is_empty()
        {
            return Ok(SkillSource {
                host: Host::GitHub,
                owner: owner.to_string(),
                repo: repo.to_string(),
                path: String::new(),
                git_ref: None,
            });
        }
        return Err(GitUrlError::Unsupported(url.to_string()));
    }

    let after_scheme = trimmed
        .strip_prefix("https://")
        .ok_or_else(|| GitUrlError::Unsupported(url.to_string()))?;
    let (domain, path) = after_scheme.split_once('/').unwrap_or((after_scheme, ""));
    source_for_host_path(domain, path, url)
}

/// Extracts `(host, path)` from SSH git URLs: `ssh://[user@]host/path` and
/// scp-style `user@host:path`. Returns `None` for non-SSH input (https
/// URLs, shorthand) so the caller's other arms get their turn.
fn ssh_host_and_path(s: &str) -> Option<(&str, &str)> {
    if let Some(rest) = s.strip_prefix("ssh://") {
        let after_user = rest.rsplit('@').next().unwrap_or(rest);
        let (host, path) = after_user.split_once('/')?;
        return Some((host, path));
    }
    let (user_host, path) = s.split_once(':')?;
    let (_, host) = user_host.split_once('@')?;
    Some((host, path))
}

/// Maps a git host + in-URL path to skill host coordinates: github.com and
/// bitbucket.org URLs are `owner/repo` (exactly two segments); gitlab.com's
/// whole path is the project path (nested groups included). Unknown or
/// self-hosted domains are unsupported.
fn source_for_host_path(
    host: &str,
    path: &str,
    original: &str,
) -> Result<SkillSource, GitUrlError> {
    if path.is_empty() {
        return Err(GitUrlError::Unsupported(original.to_string()));
    }
    match host {
        "github.com" | "bitbucket.org" => {
            let host_kind = if host == "github.com" {
                Host::GitHub
            } else {
                Host::Bitbucket
            };
            let mut parts = path.splitn(3, '/');
            let (Some(owner), Some(repo)) = (parts.next(), parts.next()) else {
                return Err(GitUrlError::Unsupported(original.to_string()));
            };
            if owner.is_empty() || repo.is_empty() || !parts.next().unwrap_or("").is_empty() {
                // A github/bitbucket URL with extra path segments isn't a
                // bare repo URL — reject rather than guess at the split.
                return Err(GitUrlError::Unsupported(original.to_string()));
            }
            Ok(SkillSource {
                host: host_kind,
                owner: owner.to_string(),
                repo: repo.to_string(),
                path: String::new(),
                git_ref: None,
            })
        }
        "gitlab.com" => Ok(SkillSource {
            host: Host::GitLab,
            owner: String::new(),
            repo: path.to_string(),
            path: String::new(),
            git_ref: None,
        }),
        _ => Err(GitUrlError::Unsupported(original.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn existing_directory_is_a_local_marketplace() {
        let root = tempdir().unwrap();
        let dir = root.path().join("my-marketplace");
        std::fs::create_dir_all(&dir).unwrap();
        let result = parse_add_spec(dir.to_str().unwrap()).unwrap();
        let ParsedAddSpec::Local(path) = result else {
            panic!("expected Local, got {result:?}");
        };
        assert!(path.is_absolute());
        assert!(path.ends_with("my-marketplace"));
    }

    #[test]
    fn dot_slash_path_that_does_not_exist_is_a_local_error_not_a_remote_spec() {
        let result = parse_add_spec("./definitely-not-here-xyz");
        assert!(matches!(result, Err(AddSpecError::LocalDirNotFound(_))));
    }

    #[test]
    fn absolute_path_that_does_not_exist_is_a_local_error() {
        let result = parse_add_spec("/definitely/not/here-xyz");
        assert!(matches!(result, Err(AddSpecError::LocalDirNotFound(_))));
    }

    #[test]
    fn bare_owner_repo_is_a_remote_github_spec() {
        let result = parse_add_spec("acme/plugins").unwrap();
        let ParsedAddSpec::Remote(parsed) = result else {
            panic!("expected Remote, got {result:?}");
        };
        assert_eq!(parsed.source.host, Host::GitHub);
        assert_eq!(parsed.source.owner, "acme");
        assert_eq!(parsed.source.repo, "plugins");
        assert!(!parsed.needs_project_path_resolution);
    }

    #[test]
    fn gitlab_shorthand_is_a_remote_spec_pending_resolution() {
        let result = parse_add_spec("gl:group/sub/plugins@main").unwrap();
        let ParsedAddSpec::Remote(parsed) = result else {
            panic!("expected Remote, got {result:?}");
        };
        assert_eq!(parsed.source.host, Host::GitLab);
        assert!(parsed.needs_project_path_resolution);
    }

    #[test]
    fn git_url_parses_github_https_with_git_suffix() {
        let source = parse_git_url("https://github.com/acme/plugin.git").unwrap();
        assert_eq!(source.host, Host::GitHub);
        assert_eq!(source.owner, "acme");
        assert_eq!(source.repo, "plugin");
    }

    #[test]
    fn git_url_parses_bitbucket_https() {
        let source = parse_git_url("https://bitbucket.org/acme/plugin").unwrap();
        assert_eq!(source.host, Host::Bitbucket);
        assert_eq!(source.owner, "acme");
        assert_eq!(source.repo, "plugin");
    }

    #[test]
    fn git_url_treats_whole_gitlab_path_as_the_project_path() {
        let source = parse_git_url("https://gitlab.com/group/sub/project.git").unwrap();
        assert_eq!(source.host, Host::GitLab);
        assert_eq!(source.owner, "");
        assert_eq!(source.repo, "group/sub/project");
        assert_eq!(source.path, "");
    }

    #[test]
    fn git_url_parses_github_owner_repo_shorthand() {
        let source = parse_git_url("acme/plugin").unwrap();
        assert_eq!(source.host, Host::GitHub);
        assert_eq!(source.owner, "acme");
        assert_eq!(source.repo, "plugin");
        assert_eq!(source.path, "");
    }

    #[test]
    fn git_url_parses_shorthand_with_git_suffix() {
        let source = parse_git_url("acme/plugin.git").unwrap();
        assert_eq!(source.host, Host::GitHub);
        assert_eq!(source.owner, "acme");
        assert_eq!(source.repo, "plugin");
    }

    #[test]
    fn git_url_parses_scp_style_ssh_urls() {
        let source = parse_git_url("git@github.com:acme/plugin.git").unwrap();
        assert_eq!(source.host, Host::GitHub);
        assert_eq!(source.owner, "acme");
        assert_eq!(source.repo, "plugin");

        let source = parse_git_url("git@gitlab.com:group/sub/project.git").unwrap();
        assert_eq!(source.host, Host::GitLab);
        assert_eq!(source.repo, "group/sub/project");

        let source = parse_git_url("git@bitbucket.org:acme/plugin").unwrap();
        assert_eq!(source.host, Host::Bitbucket);
        assert_eq!(source.owner, "acme");
        assert_eq!(source.repo, "plugin");
    }

    #[test]
    fn git_url_parses_ssh_scheme_urls_with_and_without_a_user() {
        let source = parse_git_url("ssh://git@github.com/acme/plugin.git").unwrap();
        assert_eq!(source.host, Host::GitHub);
        assert_eq!(source.owner, "acme");
        assert_eq!(source.repo, "plugin");

        let source = parse_git_url("ssh://github.com/acme/plugin").unwrap();
        assert_eq!(source.host, Host::GitHub);
        assert_eq!(source.owner, "acme");
        assert_eq!(source.repo, "plugin");
    }

    #[test]
    fn git_url_rejects_unknown_and_self_hosted_hosts() {
        assert!(matches!(
            parse_git_url("https://git.example.com/acme/plugin.git"),
            Err(GitUrlError::Unsupported(_))
        ));
        assert!(matches!(
            parse_git_url("git@git.example.com:acme/plugin.git"),
            Err(GitUrlError::Unsupported(_))
        ));
    }

    #[test]
    fn git_url_rejects_malformed_ssh_urls() {
        // scp-style with no path, and ssh:// pointing at a repo-less host.
        assert!(matches!(
            parse_git_url("git@github.com:"),
            Err(GitUrlError::Unsupported(_))
        ));
        assert!(matches!(
            parse_git_url("ssh://git@github.com/acme"),
            Err(GitUrlError::Unsupported(_))
        ));
    }

    #[test]
    fn git_url_rejects_shorthand_with_wrong_segment_count() {
        assert!(matches!(
            parse_git_url("acme"),
            Err(GitUrlError::Unsupported(_))
        ));
        assert!(matches!(
            parse_git_url("acme/plugin/tree"),
            Err(GitUrlError::Unsupported(_))
        ));
    }

    #[test]
    fn git_url_rejects_extra_path_segments_on_flat_hosts() {
        assert!(matches!(
            parse_git_url("https://github.com/acme/plugin/tree/main"),
            Err(GitUrlError::Unsupported(_))
        ));
    }

    #[test]
    fn source_summary_renders_each_variant() {
        let remote = MarketplaceSource::Remote {
            host: Host::GitHub,
            owner: "acme".into(),
            repo: "plugins".into(),
            path: String::new(),
            git_ref: "main".into(),
        };
        assert_eq!(remote.summary(), "acme/plugins@main");

        let gitlab = MarketplaceSource::Remote {
            host: Host::GitLab,
            owner: String::new(),
            repo: "group/plugins".into(),
            path: "catalog".into(),
            git_ref: "main".into(),
        };
        assert_eq!(gitlab.summary(), "gl:group/plugins/catalog@main");

        let local = MarketplaceSource::Local {
            path: PathBuf::from("/abs/dir"),
        };
        assert_eq!(local.summary(), "local:/abs/dir");
    }

    #[test]
    fn source_round_trips_through_toml() {
        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct Wrapper {
            #[serde(flatten)]
            source: MarketplaceSource,
        }

        let remote = Wrapper {
            source: MarketplaceSource::Remote {
                host: Host::GitHub,
                owner: "acme".into(),
                repo: "plugins".into(),
                path: String::new(),
                git_ref: "main".into(),
            },
        };
        let text = toml::to_string(&remote).unwrap();
        let parsed: Wrapper = toml::from_str(&text).unwrap();
        assert_eq!(parsed, remote);

        let local = Wrapper {
            source: MarketplaceSource::Local {
                path: PathBuf::from("/abs/dir"),
            },
        };
        let text = toml::to_string(&local).unwrap();
        let parsed: Wrapper = toml::from_str(&text).unwrap();
        assert_eq!(parsed, local);
    }
}
