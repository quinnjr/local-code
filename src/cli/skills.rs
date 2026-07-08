// src/cli/skills.rs

use std::io::Write;

use crate::config::paths::Paths;
use crate::config::secrets::SecretStore;
use crate::skills::github::GithubClient;
use crate::skills::install::{default_name, install_skill, list_skills, remove_skill, update_skill};
use crate::skills::types::Scope;

fn github_client() -> anyhow::Result<GithubClient> {
    let token = SecretStore::get_api_key("github")?;
    Ok(GithubClient::new(token))
}

/// Rejects a skill name that looks like it's trying to escape the skills
/// directory (path separators or a `..` segment), rather than silently
/// sanitizing it. Used at the CLI layer as defense-in-depth on top of the
/// `write_files` guard in `skills::install`.
fn validate_skill_name(name: &str) -> anyhow::Result<()> {
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        anyhow::bail!("skill name '{name}' must not contain path separators or '..'");
    }
    Ok(())
}

pub async fn install<W: Write>(
    paths: &Paths,
    spec: &str,
    global: bool,
    name_override: Option<&str>,
    mut out: W,
) -> anyhow::Result<()> {
    let source = crate::skills::github::parse_spec(spec)?;
    let name = name_override.map(str::to_string).unwrap_or_else(|| default_name(&source));
    validate_skill_name(&name)?;
    let scope = if global { Scope::Global } else { Scope::Project };
    let client = github_client()?;

    install_skill(&client, paths, scope, &source, &name).await?;
    writeln!(out, "Installed skill '{name}' from {spec} ({})", scope_label(scope))?;
    Ok(())
}

pub fn list<W: Write>(paths: &Paths, mut out: W) -> anyhow::Result<()> {
    let summaries = list_skills(paths)?;
    if summaries.is_empty() {
        writeln!(out, "No skills installed.")?;
        return Ok(());
    }
    for summary in summaries {
        writeln!(out, "{} · {} · {}", summary.name, scope_label(summary.scope), summary.source)?;
    }
    Ok(())
}

pub fn remove<W: Write>(paths: &Paths, name: &str, global: bool, mut out: W) -> anyhow::Result<()> {
    validate_skill_name(name)?;
    let scope = if global { Scope::Global } else { Scope::Project };
    remove_skill(paths, scope, name)?;
    writeln!(out, "Removed skill '{name}' ({})", scope_label(scope))?;
    Ok(())
}

pub async fn update<W: Write>(paths: &Paths, name: Option<&str>, global: bool, mut out: W) -> anyhow::Result<()> {
    if let Some(n) = name {
        validate_skill_name(n)?;
    }
    let scope = if global { Scope::Global } else { Scope::Project };
    let client = github_client()?;

    let names: Vec<String> = match name {
        Some(n) => vec![n.to_string()],
        None => list_skills(paths)?.into_iter().filter(|s| s.scope == scope).map(|s| s.name).collect(),
    };

    if names.is_empty() {
        writeln!(out, "No skills installed in this scope.")?;
        return Ok(());
    }

    for name in names {
        let updated = update_skill(&client, paths, scope, &name).await?;
        if updated {
            writeln!(out, "Updated skill '{name}'")?;
        } else {
            writeln!(out, "Skill '{name}' is already up to date")?;
        }
    }
    Ok(())
}

fn scope_label(scope: Scope) -> &'static str {
    match scope {
        Scope::Project => "project",
        Scope::Global => "global",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_paths(root: &std::path::Path) -> Paths {
        Paths {
            user_config_dir: root.join("user-config"),
            project_config_dir: root.join("project/.local-code"),
            user_state_dir: root.join("user-state"),
        }
    }

    #[test]
    fn list_reports_no_skills_installed() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let mut out = Vec::new();
        list(&paths, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("No skills installed"));
    }

    #[test]
    fn remove_reports_not_installed_error() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let out: Vec<u8> = Vec::new();
        let result = remove(&paths, "nope", false, out);
        assert!(result.is_err());
    }

    #[test]
    fn remove_reports_success() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        std::fs::create_dir_all(paths.project_config_dir.join("skills/pdf")).unwrap();
        let mut out = Vec::new();
        remove(&paths, "pdf", false, &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("Removed skill 'pdf'"));
    }

    #[test]
    fn validate_skill_name_rejects_traversal_shapes() {
        assert!(validate_skill_name("../escape").is_err());
        assert!(validate_skill_name("foo/../bar").is_err());
        assert!(validate_skill_name("foo/bar").is_err());
        assert!(validate_skill_name("foo\\bar").is_err());
        assert!(validate_skill_name("pdf").is_ok());
    }

    #[test]
    fn remove_rejects_a_path_traversal_shaped_name() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        // Create a real skill outside the (nonexistent) `project/.local-code`
        // root, at the location `../escape` would resolve to if traversal
        // weren't blocked, so a would-be escape has something to hit.
        let escape_target = root.path().join("escape");
        std::fs::create_dir_all(&escape_target).unwrap();

        let out: Vec<u8> = Vec::new();
        let result = remove(&paths, "../escape", false, out);
        assert!(result.is_err());
        // The directory a successful traversal would have deleted must still exist.
        assert!(escape_target.exists());
    }

    #[tokio::test]
    async fn install_rejects_a_path_traversal_shaped_name_override() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let out: Vec<u8> = Vec::new();
        // Validation happens before any GitHub client/network call is made, so
        // this fails fast on the bad `--name` regardless of `spec` validity.
        let result = install(&paths, "acme/widgets", false, Some("../escape"), out).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must not contain path separators"));
    }

    #[tokio::test]
    async fn update_rejects_a_path_traversal_shaped_name() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let out: Vec<u8> = Vec::new();
        let result = update(&paths, Some("../escape"), false, out).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must not contain path separators"));
    }
}
