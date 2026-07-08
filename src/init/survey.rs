// src/init/survey.rs

use std::path::Path;

use ignore::WalkBuilder;

/// A `.gitignore`-respecting survey of a project directory: every
/// non-ignored file path (capped, so a huge repo doesn't blow up the prompt)
/// plus the full contents of any recognized build-manifest file found, used
/// to build the LLM prompt `/init` sends to generate AGENTS.md.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectSurvey {
    pub file_paths: Vec<String>,
    /// (relative path, file contents) pairs for recognized manifests.
    pub manifests: Vec<(String, String)>,
    /// The true number of files encountered by the walk, regardless of
    /// whether they made it into `file_paths` (which is capped at
    /// `MAX_FILES_LISTED`). Lets callers detect and honestly disclose
    /// truncation instead of reporting the capped `file_paths.len()` as if
    /// it were the project's real file count.
    pub total_files_seen: usize,
}

const RECOGNIZED_MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "requirements.txt",
    "go.mod",
    "Gemfile",
    "pom.xml",
    "build.gradle",
];

const MAX_FILES_LISTED: usize = 500;

/// Walks `project_root`, respecting `.gitignore` (via `ignore::WalkBuilder`,
/// the same traversal semantics ripgrep uses), collecting up to
/// `MAX_FILES_LISTED` relative file paths and the full contents of any
/// top-level file matching `RECOGNIZED_MANIFESTS`.
pub fn survey_project(project_root: &Path) -> ProjectSurvey {
    let mut survey = ProjectSurvey::default();

    // `require_git(false)`: by default `ignore::WalkBuilder` only honors
    // `.gitignore` files when `project_root` is inside an actual Git
    // repository (has a `.git` directory) — plenty of real projects (and all
    // of this module's tests, which use bare `tempdir()`s) have a
    // `.gitignore` without `git init` ever having been run, so that default
    // would silently stop respecting it.
    let mut builder = WalkBuilder::new(project_root);
    builder.require_git(false);
    for entry in builder.build().filter_map(|e| e.ok()) {
        if !entry.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let Ok(relative) = entry.path().strip_prefix(project_root) else { continue };
        let relative_str = relative.to_string_lossy().to_string();

        survey.total_files_seen += 1;
        if survey.file_paths.len() < MAX_FILES_LISTED {
            survey.file_paths.push(relative_str.clone());
        }

        if let Some(name) = entry.path().file_name().and_then(|n| n.to_str())
            && RECOGNIZED_MANIFESTS.contains(&name)
            && let Ok(content) = std::fs::read_to_string(entry.path())
        {
            survey.manifests.push((relative_str, content));
        }
    }

    survey
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn collects_file_paths_and_recognized_manifests() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let survey = survey_project(dir.path());
        assert!(survey.file_paths.iter().any(|p| p == "Cargo.toml"));
        assert!(survey.file_paths.iter().any(|p| p == "main.rs"));
        assert_eq!(survey.manifests.len(), 1);
        assert_eq!(survey.manifests[0].0, "Cargo.toml");
        assert!(survey.manifests[0].1.contains("name = \"x\""));
    }

    #[test]
    fn respects_gitignore() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "should not appear").unwrap();
        std::fs::write(dir.path().join("kept.txt"), "should appear").unwrap();

        let survey = survey_project(dir.path());
        assert!(!survey.file_paths.iter().any(|p| p == "ignored.txt"));
        assert!(survey.file_paths.iter().any(|p| p == "kept.txt"));
    }

    #[test]
    fn non_manifest_files_are_listed_but_not_read() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "secret notes").unwrap();

        let survey = survey_project(dir.path());
        assert!(survey.file_paths.iter().any(|p| p == "notes.txt"));
        assert!(survey.manifests.is_empty());
    }

    #[test]
    fn total_files_seen_counts_every_file_even_past_the_listed_cap() {
        let dir = tempdir().unwrap();
        for i in 0..(MAX_FILES_LISTED + 5) {
            std::fs::write(dir.path().join(format!("f{i}.txt")), "x").unwrap();
        }

        let survey = survey_project(dir.path());
        assert_eq!(survey.file_paths.len(), MAX_FILES_LISTED);
        assert_eq!(survey.total_files_seen, MAX_FILES_LISTED + 5);
    }
}
