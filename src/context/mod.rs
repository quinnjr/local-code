use std::path::Path;

use crate::config::paths::Paths;

/// Loads and concatenates, in the order spec section 4 specifies (project
/// AGENTS.md, project CLAUDE.md, user-level AGENTS.md, user-level CLAUDE.md),
/// whichever of these four files exist. Missing files are silently skipped —
/// this is optional context, not a hard requirement. Each present file is
/// wrapped in a small header identifying its source, so the model can tell
/// project-level guidance apart from user-level defaults if they conflict.
pub fn load_project_context(paths: &Paths, project_root: &Path) -> String {
    let candidates = [
        (project_root.join("AGENTS.md"), "Project AGENTS.md"),
        (project_root.join("CLAUDE.md"), "Project CLAUDE.md"),
        (
            paths.user_config_dir.join("AGENTS.md"),
            "User-level AGENTS.md",
        ),
        (
            paths.user_config_dir.join("CLAUDE.md"),
            "User-level CLAUDE.md",
        ),
    ];

    let mut sections = Vec::new();
    for (path, label) in candidates {
        if let Ok(content) = std::fs::read_to_string(&path)
            && !content.trim().is_empty()
        {
            sections.push(format!("## {label}\n\n{content}"));
        }
    }
    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn test_paths(user_config_dir: &Path) -> Paths {
        Paths {
            user_config_dir: user_config_dir.to_path_buf(),
            project_config_dir: user_config_dir.join("unused-project-config"),
            user_state_dir: user_config_dir.join("unused-state"),
        }
    }

    #[test]
    fn returns_empty_string_when_no_context_files_exist() {
        let dir = tempdir().unwrap();
        let paths = test_paths(dir.path());
        let context = load_project_context(&paths, dir.path());
        assert!(context.is_empty());
    }

    #[test]
    fn loads_project_agents_md_when_present() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("AGENTS.md"),
            "Always run tests before committing.",
        )
        .unwrap();
        let paths = test_paths(&dir.path().join("user-config"));
        let context = load_project_context(&paths, dir.path());
        assert!(context.contains("Project AGENTS.md"));
        assert!(context.contains("Always run tests before committing."));
    }

    #[test]
    fn loads_all_four_files_in_the_documented_order() {
        let dir = tempdir().unwrap();
        let user_config_dir = dir.path().join("user-config");
        std::fs::create_dir_all(&user_config_dir).unwrap();

        std::fs::write(dir.path().join("AGENTS.md"), "project agents").unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "project claude").unwrap();
        std::fs::write(user_config_dir.join("AGENTS.md"), "user agents").unwrap();
        std::fs::write(user_config_dir.join("CLAUDE.md"), "user claude").unwrap();

        let paths = test_paths(&user_config_dir);
        let context = load_project_context(&paths, dir.path());

        let project_agents_pos = context.find("project agents").unwrap();
        let project_claude_pos = context.find("project claude").unwrap();
        let user_agents_pos = context.find("user agents").unwrap();
        let user_claude_pos = context.find("user claude").unwrap();
        assert!(project_agents_pos < project_claude_pos);
        assert!(project_claude_pos < user_agents_pos);
        assert!(user_agents_pos < user_claude_pos);
    }

    #[test]
    fn blank_file_is_treated_as_absent() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "   \n  ").unwrap();
        let paths = test_paths(&dir.path().join("user-config"));
        let context = load_project_context(&paths, dir.path());
        assert!(context.is_empty());
    }
}
