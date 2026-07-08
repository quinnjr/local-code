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

pub async fn install<W: Write>(
    paths: &Paths,
    spec: &str,
    global: bool,
    name_override: Option<&str>,
    mut out: W,
) -> anyhow::Result<()> {
    let source = crate::skills::github::parse_spec(spec)?;
    let name = name_override.map(str::to_string).unwrap_or_else(|| default_name(&source));
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
    let scope = if global { Scope::Global } else { Scope::Project };
    remove_skill(paths, scope, name)?;
    writeln!(out, "Removed skill '{name}' ({})", scope_label(scope))?;
    Ok(())
}

pub async fn update<W: Write>(paths: &Paths, name: Option<&str>, global: bool, mut out: W) -> anyhow::Result<()> {
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
}
