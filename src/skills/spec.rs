// src/skills/spec.rs

use crate::skills::types::{Host, SkillHostError, SkillSource};

/// A spec, parsed as far as it can be parsed *synchronously*. Fully resolved
/// for GitHub, Bitbucket, and GitLab-full-URL specs (`source` below is a
/// complete `SkillSource`, `needs_project_path_resolution` is `false`). For
/// GitLab shorthand specs (`gl:group/sub/project/skills/pdf@main`),
/// `source.repo` temporarily holds the *unsplit* `raw_path` (everything
/// between the `gl:` prefix and the optional `@ref`) and
/// `needs_project_path_resolution` is `true` — the caller must run
/// `gitlab::GitlabClient::resolve_project_path` on `source.repo` before the
/// `SkillSource` is usable, then overwrite `source.repo`/`source.path` with
/// the resolved pair. See design spec §2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSpec {
    pub source: SkillSource,
    pub needs_project_path_resolution: bool,
}

pub fn parse_spec(spec: &str) -> Result<ParsedSpec, SkillHostError> {
    if let Some(rest) = spec.strip_prefix("https://") {
        return parse_url(rest, spec);
    }

    if let Some(rest) = spec.strip_prefix("gh:") {
        return parse_github_or_bitbucket_shaped(rest, Host::GitHub, spec);
    }
    if let Some(rest) = spec.strip_prefix("bb:") {
        return parse_github_or_bitbucket_shaped(rest, Host::Bitbucket, spec);
    }
    if let Some(rest) = spec.strip_prefix("gl:") {
        return parse_gitlab_shorthand(rest);
    }

    // No prefix, no URL: defaults to GitHub, using exactly today's
    // owner/repo[/path][@ref] parser — the one backward-compatibility
    // guarantee this feature needs to uphold.
    let source = crate::skills::github::parse_spec(spec)?;
    Ok(ParsedSpec { source, needs_project_path_resolution: false })
}

/// GitHub (`owner/repo[/path][@ref]`) and Bitbucket (`workspace/repo_slug[/path][@ref]`)
/// share an identical flat shape, differing only in the resulting `Host`.
fn parse_github_or_bitbucket_shaped(rest: &str, host: Host, original_spec: &str) -> Result<ParsedSpec, SkillHostError> {
    let parsed = crate::skills::github::parse_spec(rest).map_err(|_| SkillHostError::InvalidSpec(original_spec.to_string()))?;
    Ok(ParsedSpec { source: SkillSource { host, ..parsed }, needs_project_path_resolution: false })
}

/// `gl:group/subgroup/project/skills/pdf@main` — split off `@ref` only;
/// project-path-vs-in-repo-path is genuinely ambiguous without an API call
/// (see design spec §2), so `raw_path` is carried unsplit in `source.repo`
/// pending `gitlab::resolve_project_path`.
fn parse_gitlab_shorthand(rest: &str) -> Result<ParsedSpec, SkillHostError> {
    let (raw_path, git_ref) = match rest.rsplit_once('@') {
        Some((rest, r)) if !r.is_empty() => (rest, Some(r.to_string())),
        _ => (rest, None),
    };
    if raw_path.is_empty() {
        return Err(SkillHostError::InvalidSpec(format!("gl:{rest}")));
    }
    Ok(ParsedSpec {
        source: SkillSource {
            host: Host::GitLab,
            owner: String::new(),
            repo: raw_path.to_string(), // unsplit; resolved later
            path: String::new(),        // unresolved; resolved later
            git_ref,
        },
        needs_project_path_resolution: true,
    })
}

fn parse_url(rest: &str, original_spec: &str) -> Result<ParsedSpec, SkillHostError> {
    let (domain, after_domain) = rest.split_once('/').unwrap_or((rest, ""));
    match domain {
        "github.com" => {
            let parsed = crate::skills::github::parse_spec(after_domain)
                .map_err(|_| SkillHostError::InvalidSpec(original_spec.to_string()))?;
            Ok(ParsedSpec { source: SkillSource { host: Host::GitHub, ..parsed }, needs_project_path_resolution: false })
        }
        "bitbucket.org" => {
            let parsed = crate::skills::github::parse_spec(after_domain)
                .map_err(|_| SkillHostError::InvalidSpec(original_spec.to_string()))?;
            Ok(ParsedSpec { source: SkillSource { host: Host::Bitbucket, ..parsed }, needs_project_path_resolution: false })
        }
        "gitlab.com" => parse_gitlab_url(after_domain, original_spec),
        _ => Err(SkillHostError::InvalidSpec(original_spec.to_string())),
    }
}

/// `group/subgroup/project/-/tree/main/skills/pdf` or `.../-/blob/main/skills/pdf`
/// — GitLab's own web UI convention marks exactly where the project path
/// ends and the ref+in-repo-path begins, so this is unambiguous without an
/// API call, unlike the shorthand form.
fn parse_gitlab_url(after_domain: &str, original_spec: &str) -> Result<ParsedSpec, SkillHostError> {
    let marker = "/-/tree/";
    let (project_path, after_marker) = after_domain
        .split_once(marker)
        .or_else(|| after_domain.split_once("/-/blob/"))
        .ok_or_else(|| SkillHostError::InvalidSpec(original_spec.to_string()))?;
    if project_path.is_empty() {
        return Err(SkillHostError::InvalidSpec(original_spec.to_string()));
    }
    let (git_ref, in_repo_path) = after_marker.split_once('/').unwrap_or((after_marker, ""));
    if git_ref.is_empty() {
        return Err(SkillHostError::InvalidSpec(original_spec.to_string()));
    }
    Ok(ParsedSpec {
        source: SkillSource {
            host: Host::GitLab,
            owner: String::new(),
            repo: project_path.to_string(),
            path: in_repo_path.to_string(),
            git_ref: Some(git_ref.to_string()),
        },
        needs_project_path_resolution: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_prefix_defaults_to_github_and_matches_todays_bare_spec_shape() {
        let parsed = parse_spec("anthropics/skills/pdf@main").unwrap();
        assert_eq!(parsed.source.host, Host::GitHub);
        assert_eq!(parsed.source.owner, "anthropics");
        assert_eq!(parsed.source.repo, "skills");
        assert_eq!(parsed.source.path, "pdf");
        assert_eq!(parsed.source.git_ref, Some("main".to_string()));
        assert!(!parsed.needs_project_path_resolution);
    }

    #[test]
    fn gh_prefix_is_explicit_github() {
        let parsed = parse_spec("gh:anthropics/skills").unwrap();
        assert_eq!(parsed.source.host, Host::GitHub);
        assert_eq!(parsed.source.owner, "anthropics");
    }

    #[test]
    fn bb_prefix_parses_workspace_repo_slug_shape() {
        let parsed = parse_spec("bb:acme/widgets/skills/pdf@main").unwrap();
        assert_eq!(parsed.source.host, Host::Bitbucket);
        assert_eq!(parsed.source.owner, "acme");
        assert_eq!(parsed.source.repo, "widgets");
        assert_eq!(parsed.source.path, "skills/pdf");
        assert!(!parsed.needs_project_path_resolution);
    }

    #[test]
    fn gl_prefix_shorthand_defers_project_path_resolution() {
        let parsed = parse_spec("gl:group/subgroup/project/skills/pdf@main").unwrap();
        assert_eq!(parsed.source.host, Host::GitLab);
        assert_eq!(parsed.source.repo, "group/subgroup/project/skills/pdf");
        assert_eq!(parsed.source.git_ref, Some("main".to_string()));
        assert!(parsed.needs_project_path_resolution);
    }

    #[test]
    fn gl_prefix_shorthand_without_ref() {
        let parsed = parse_spec("gl:group/project/skills/pdf").unwrap();
        assert_eq!(parsed.source.git_ref, None);
        assert_eq!(parsed.source.repo, "group/project/skills/pdf");
    }

    #[test]
    fn github_url_is_parsed() {
        let parsed = parse_spec("https://github.com/anthropics/skills/pdf@main").unwrap();
        assert_eq!(parsed.source.host, Host::GitHub);
        assert_eq!(parsed.source.owner, "anthropics");
        assert_eq!(parsed.source.repo, "skills");
        assert_eq!(parsed.source.path, "pdf");
    }

    #[test]
    fn bitbucket_url_is_parsed() {
        let parsed = parse_spec("https://bitbucket.org/acme/widgets/skills/pdf").unwrap();
        assert_eq!(parsed.source.host, Host::Bitbucket);
        assert_eq!(parsed.source.owner, "acme");
        assert_eq!(parsed.source.repo, "widgets");
    }

    #[test]
    fn gitlab_url_with_tree_is_parsed_unambiguously_without_an_api_call() {
        let parsed = parse_spec("https://gitlab.com/group/subgroup/project/-/tree/main/skills/pdf").unwrap();
        assert_eq!(parsed.source.host, Host::GitLab);
        assert_eq!(parsed.source.repo, "group/subgroup/project");
        assert_eq!(parsed.source.path, "skills/pdf");
        assert_eq!(parsed.source.git_ref, Some("main".to_string()));
        assert!(!parsed.needs_project_path_resolution);
    }

    #[test]
    fn gitlab_url_with_blob_is_parsed() {
        let parsed = parse_spec("https://gitlab.com/group/project/-/blob/main/skills/pdf/SKILL.md").unwrap();
        assert_eq!(parsed.source.repo, "group/project");
        assert_eq!(parsed.source.path, "skills/pdf/SKILL.md");
    }

    #[test]
    fn gitlab_url_at_project_root_has_empty_in_repo_path() {
        let parsed = parse_spec("https://gitlab.com/group/project/-/tree/main").unwrap();
        assert_eq!(parsed.source.repo, "group/project");
        assert_eq!(parsed.source.path, "");
    }

    #[test]
    fn unrecognized_domain_is_rejected() {
        let result = parse_spec("https://example.invalid/foo/bar");
        assert!(matches!(result, Err(SkillHostError::InvalidSpec(_))));
    }

    #[test]
    fn self_hosted_gitlab_style_domain_is_rejected_out_of_scope() {
        let result = parse_spec("https://gitlab.mycompany.com/group/project");
        assert!(matches!(result, Err(SkillHostError::InvalidSpec(_))));
    }

    #[test]
    fn empty_gitlab_shorthand_path_is_rejected() {
        let result = parse_spec("gl:@main");
        assert!(matches!(result, Err(SkillHostError::InvalidSpec(_))));
    }
}
