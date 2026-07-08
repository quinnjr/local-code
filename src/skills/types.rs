// src/skills/types.rs

use std::path::PathBuf;

/// Which of the two config roots (`Paths::project_config_dir` /
/// `Paths::user_config_dir`) a skill was installed into. Project scope
/// shadows global scope for skills of the same name (see
/// `crate::skills::discovery::discover_skills`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Project,
    Global,
}

/// A parsed `owner/repo[/path][@ref]` skill source spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillSource {
    pub owner: String,
    pub repo: String,
    /// Empty string means "repo root".
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
    pub owner: String,
    pub repo: String,
    pub path: String,
    /// The effective ref used at install time (either the user-supplied ref,
    /// or the repo's resolved default branch name).
    pub git_ref: String,
    /// The commit SHA that `git_ref` resolved to at install/update time.
    pub commit_sha: String,
}
