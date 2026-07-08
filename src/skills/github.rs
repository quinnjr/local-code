// src/skills/github.rs

use crate::skills::types::SkillSource;

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("invalid skill source '{0}': expected owner/repo[/path][@ref]")]
    InvalidSpec(String),
}

/// Parses an `owner/repo[/path][@ref]` skill source spec. The ref, if
/// present, is split off from the *last* `@` in the spec (GitHub owner/repo
/// names and paths cannot themselves contain `@`, so this is unambiguous in
/// practice). `path` is `""` when no subpath was given.
pub fn parse_spec(spec: &str) -> Result<SkillSource, GithubError> {
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
            owner: owner.to_string(),
            repo: repo.to_string(),
            path,
            git_ref,
        }),
        _ => Err(GithubError::InvalidSpec(spec.to_string())),
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
        assert!(matches!(result, Err(GithubError::InvalidSpec(_))));
    }

    #[test]
    fn rejects_empty_spec() {
        let result = parse_spec("");
        assert!(matches!(result, Err(GithubError::InvalidSpec(_))));
    }
}
