// src/skills/discovery.rs

use std::collections::HashSet;
use std::path::Path;

use crate::config::paths::Paths;
use crate::skills::frontmatter::{classify, parse_frontmatter};
use crate::skills::types::{Scope, Skill};

/// Scans both scope directories (`<project_config_dir>/skills/`,
/// `<user_config_dir>/skills/`) for installed skills. Each immediate
/// subdirectory containing a `SKILL.mdc` or `SKILL.md` (`.mdc` wins if both
/// are present) is parsed into a `Skill`. Project-scope skills shadow
/// global-scope skills of the same name — a global skill is skipped
/// entirely if a project skill with the same name was already found.
/// Malformed skills (unparseable frontmatter) are skipped with a warning
/// printed to stderr rather than failing discovery for the rest.
pub fn discover_skills(paths: &Paths, project_root: &Path) -> Vec<Skill> {
    let _ = project_root; // reserved for glob-matching call sites (Task 7)
    let mut seen_names: HashSet<String> = HashSet::new();
    let mut skills = Vec::new();

    for (dir, scope) in [
        (paths.project_config_dir.join("skills"), Scope::Project),
        (paths.user_config_dir.join("skills"), Scope::Global),
    ] {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let skill_dir = entry.path();
            if !skill_dir.is_dir() {
                continue;
            }
            match load_skill_dir(&skill_dir, scope) {
                Ok(skill) => {
                    if seen_names.contains(&skill.name) {
                        continue; // shadowed by a project-scope skill already found
                    }
                    seen_names.insert(skill.name.clone());
                    skills.push(skill);
                }
                Err(SkillLoadError::NoSkillFile) => {} // not a skill directory, ignore silently
                Err(SkillLoadError::Malformed(reason)) => {
                    eprintln!("warning: skipping skill at {}: {reason}", skill_dir.display());
                }
            }
        }
    }

    skills
}

enum SkillLoadError {
    NoSkillFile,
    Malformed(String),
}

fn load_skill_dir(dir: &Path, scope: Scope) -> Result<Skill, SkillLoadError> {
    let mdc_path = dir.join("SKILL.mdc");
    let md_path = dir.join("SKILL.md");
    let (path, is_mdc) = if mdc_path.is_file() {
        (mdc_path, true)
    } else if md_path.is_file() {
        (md_path, false)
    } else {
        return Err(SkillLoadError::NoSkillFile);
    };

    let content = std::fs::read_to_string(&path)
        .map_err(|e| SkillLoadError::Malformed(format!("failed to read {}: {e}", path.display())))?;
    let (frontmatter, body) = parse_frontmatter(&content)
        .map_err(|e| SkillLoadError::Malformed(e.to_string()))?;
    let load_mode = classify(&frontmatter, is_mdc);

    Ok(Skill {
        name: frontmatter.name,
        description: frontmatter.description,
        scope,
        dir: dir.to_path_buf(),
        body,
        load_mode,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_paths(root: &Path) -> Paths {
        Paths {
            user_config_dir: root.join("user-config"),
            project_config_dir: root.join("project/.local-code"),
            user_state_dir: root.join("user-state"),
        }
    }

    fn write_skill(dir: &Path, filename: &str, name: &str, description: &str, extra_frontmatter: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(filename),
            format!("---\nname: {name}\ndescription: {description}\n{extra_frontmatter}---\nbody for {name}"),
        )
        .unwrap();
    }

    #[test]
    fn discovers_no_skills_when_no_scope_dirs_exist() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let skills = discover_skills(&paths, root.path());
        assert!(skills.is_empty());
    }

    #[test]
    fn discovers_a_project_scope_skill() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        write_skill(&paths.project_config_dir.join("skills/pdf"), "SKILL.md", "pdf", "Extract PDFs", "");

        let skills = discover_skills(&paths, root.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "pdf");
        assert_eq!(skills[0].description, "Extract PDFs");
        assert_eq!(skills[0].scope, Scope::Project);
        assert_eq!(skills[0].body.trim(), "body for pdf");
    }

    #[test]
    fn discovers_a_global_scope_skill() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        write_skill(&paths.user_config_dir.join("skills/pdf"), "SKILL.md", "pdf", "Extract PDFs", "");

        let skills = discover_skills(&paths, root.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].scope, Scope::Global);
    }

    #[test]
    fn project_scope_shadows_global_scope_by_name() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        write_skill(&paths.project_config_dir.join("skills/pdf"), "SKILL.md", "pdf", "Project version", "");
        write_skill(&paths.user_config_dir.join("skills/pdf"), "SKILL.md", "pdf", "Global version", "");

        let skills = discover_skills(&paths, root.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "Project version");
        assert_eq!(skills[0].scope, Scope::Project);
    }

    #[test]
    fn mdc_takes_precedence_over_md_in_the_same_directory() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        let dir = paths.project_config_dir.join("skills/pdf");
        write_skill(&dir, "SKILL.md", "pdf", "From md", "");
        write_skill(&dir, "SKILL.mdc", "pdf", "From mdc", "");

        let skills = discover_skills(&paths, root.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "From mdc");
    }

    #[test]
    fn malformed_skill_is_skipped_not_fatal() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        std::fs::create_dir_all(paths.project_config_dir.join("skills/broken")).unwrap();
        std::fs::write(paths.project_config_dir.join("skills/broken/SKILL.md"), "no frontmatter here").unwrap();
        write_skill(&paths.project_config_dir.join("skills/ok"), "SKILL.md", "ok", "Fine", "");

        let skills = discover_skills(&paths, root.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "ok");
    }

    #[test]
    fn directories_without_a_skill_file_are_ignored() {
        let root = tempdir().unwrap();
        let paths = test_paths(root.path());
        std::fs::create_dir_all(paths.project_config_dir.join("skills/not-a-skill")).unwrap();
        std::fs::write(paths.project_config_dir.join("skills/not-a-skill/README.md"), "hi").unwrap();

        let skills = discover_skills(&paths, root.path());
        assert!(skills.is_empty());
    }
}
