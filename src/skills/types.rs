// src/skills/types.rs

use std::path::PathBuf;

/// Which git-hosting provider a skill source spec refers to. Determined by
/// `crate::skills::spec::parse_spec` from an explicit `gh:`/`gl:`/`bb:`
/// prefix, an `https://` URL's domain, or (no prefix, no URL) defaulting to
/// `GitHub` — the one and only backward-compatibility guarantee this feature
/// makes: every spec string that parsed before this change still resolves to
/// the same host and same `SkillSource` fields it did before.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Host {
    #[default]
    GitHub,
    GitLab,
    Bitbucket,
}

/// Errors shared by all three host clients (`github.rs`, `gitlab.rs`,
/// `bitbucket.rs`) and `spec.rs`. Replaces `github.rs`'s previously-private
/// `GithubError` — same shape, same variants, now shared instead of
/// duplicated per host.
#[derive(Debug, thiserror::Error)]
pub enum SkillHostError {
    #[error("invalid skill source '{0}'")]
    InvalidSpec(String),
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("API returned {status} for {url}: {body}")]
    Api { status: u16, url: String, body: String },
    #[error("'{0}' is a file, not a directory — skills must be installed from a directory")]
    NotADirectory(String),
}

/// One file fetched from a host directory listing, with its path relative to
/// the directory that was fetched (not the repo/project root). Shared by all
/// three host clients — was `github.rs::FetchedFile`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedFile {
    pub relative_path: PathBuf,
    pub bytes: Vec<u8>,
}

/// Which of the two config roots (`Paths::project_config_dir` /
/// `Paths::user_config_dir`) a skill was installed into. Project scope
/// shadows global scope for skills of the same name (see
/// `crate::skills::discovery::discover_skills`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Project,
    Global,
}

/// A parsed skill source spec, fully resolved (host + repo/project
/// coordinates + optional ref). GitHub/Bitbucket specs resolve to this
/// synchronously in `spec::parse_spec`; GitLab shorthand specs need an extra
/// async step first (`gitlab::resolve_project_path`) — see `spec.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSource {
    pub host: Host,
    /// GitHub: `owner`. Bitbucket: `workspace`. GitLab: the project's
    /// full namespace path (e.g. `group/subgroup/project`) — GitLab has no
    /// separate "owner" concept once nested groups are involved, so this
    /// field carries the whole project path and `repo` is left empty for
    /// GitLab sources (all three host clients take the full pair as
    /// constructed here; see `SkillClient` methods in `client.rs`).
    pub owner: String,
    pub repo: String,
    /// Empty string means "repo/project root".
    pub path: String,
    /// `None` means "use the repo's default branch".
    pub git_ref: Option<String>,
}

/// How a discovered skill should be loaded into the agent. Determined by
/// `crate::skills::frontmatter::parse_frontmatter` from the file extension
/// (`SKILL.md` vs `SKILL.mdc`) and frontmatter fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadMode {
    /// Plain `SKILL.md`, or `.mdc` with no `globs` and no `alwaysApply: true`.
    /// Listed for the model; body loaded on demand via the `skill` tool.
    ModelInvoked,
    /// `.mdc` with `alwaysApply: true`. Always injected into the system
    /// prompt at agent build time.
    AlwaysApply,
    /// `.mdc` with a non-empty `globs` list (and not `alwaysApply: true`).
    /// Injected into the system prompt only if the project tree contains at
    /// least one matching file, evaluated once at agent build time.
    Globs(Vec<String>),
}

/// One discovered skill: parsed frontmatter plus body, plus where it lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub scope: Scope,
    /// The skill's directory, e.g. `.local-code/skills/pdf-extraction/`.
    pub dir: PathBuf,
    /// The skill body (frontmatter stripped).
    pub body: String,
    pub load_mode: LoadMode,
}

/// Sidecar manifest written alongside a skill's files at
/// `<skill_dir>/.skill-manifest.json`, recording where it came from so
/// `crate::skills::install::update_skill` can re-resolve and detect changes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct InstalledSkillManifest {
    /// Missing in manifests written before this field existed (all installs
    /// on `develop` prior to this feature) — `#[serde(default)]` treats them
    /// as `Host::GitHub`, which is exactly what they always were.
    #[serde(default)]
    pub host: Host,
    pub owner: String,
    pub repo: String,
    pub path: String,
    /// The effective ref used at install time (either the user-supplied ref,
    /// or the repo's resolved default branch name).
    pub git_ref: String,
    /// The commit SHA that `git_ref` resolved to at install/update time.
    pub commit_sha: String,
}

#[cfg(test)]
mod host_backcompat_tests {
    use super::*;

    #[test]
    fn manifest_without_host_field_deserializes_as_github() {
        let json = r#"{"owner":"acme","repo":"widgets","path":"skills/pdf","git_ref":"main","commit_sha":"abc123"}"#;
        let manifest: InstalledSkillManifest = serde_json::from_str(json).unwrap();
        assert_eq!(manifest.host, Host::GitHub);
    }

    #[test]
    fn host_default_is_github() {
        assert_eq!(Host::default(), Host::GitHub);
    }
}
