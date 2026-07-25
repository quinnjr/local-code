use crate::init::survey::ProjectSurvey;

/// Maximum number of surveyed file paths included in the prompt body. The
/// header's disclosed sample size is clamped to this so the number it names
/// always matches how many paths actually appear below it.
const MAX_PATHS_IN_PROMPT: usize = 200;

/// Builds the user-message text for the `/init` generation call from a
/// `ProjectSurvey` — a pure function so its output is deterministically
/// testable without a live model.
pub fn build_init_prompt(survey: &ProjectSurvey) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "Generate the contents of an AGENTS.md file for this project. AGENTS.md is read at the \
         start of every coding-agent session and folded into the system prompt — it should \
         describe the project's structure, build/test commands, and any conventions a coding \
         agent should follow. Output only the Markdown content of the file, no preamble.\n\n",
    );

    if !survey.manifests.is_empty() {
        prompt.push_str("Detected build manifests:\n\n");
        for (path, content) in &survey.manifests {
            let truncated: String = content.chars().take(2000).collect();
            prompt.push_str(&format!("### {path}\n```\n{truncated}\n```\n\n"));
        }
    }

    let listed = survey.file_paths.len().min(MAX_PATHS_IN_PROMPT);
    let header = if survey.total_files_seen > listed {
        format!(
            "Project contains {} files (showing a sample of the first {listed}). Paths:\n",
            survey.total_files_seen
        )
    } else {
        format!("Project contains {listed} files. A sample of paths:\n")
    };
    prompt.push_str(&format!(
        "{header}{}\n",
        survey
            .file_paths
            .iter()
            .take(MAX_PATHS_IN_PROMPT)
            .map(|p| format!("- {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    ));

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_manifest_contents_when_present() {
        let survey = ProjectSurvey {
            file_paths: vec!["Cargo.toml".into(), "src/main.rs".into()],
            manifests: vec![(
                "Cargo.toml".into(),
                "[package]\nname = \"local-code\"".into(),
            )],
            ..Default::default()
        };
        let prompt = build_init_prompt(&survey);
        assert!(prompt.contains("Cargo.toml"));
        assert!(prompt.contains("name = \"local-code\""));
        assert!(prompt.contains("src/main.rs"));
    }

    #[test]
    fn handles_a_survey_with_no_manifests() {
        let survey = ProjectSurvey {
            file_paths: vec!["README.md".into()],
            manifests: vec![],
            ..Default::default()
        };
        let prompt = build_init_prompt(&survey);
        assert!(prompt.contains("README.md"));
        assert!(!prompt.contains("Detected build manifests"));
    }

    #[test]
    fn truncates_very_long_manifest_contents() {
        let long_content = "x".repeat(5000);
        let survey = ProjectSurvey {
            file_paths: vec![],
            manifests: vec![("Cargo.toml".into(), long_content)],
            ..Default::default()
        };
        let prompt = build_init_prompt(&survey);
        assert!(prompt.len() < 5000 + 1000);
    }

    #[test]
    fn discloses_truncation_when_total_files_seen_exceeds_the_listed_sample() {
        let survey = ProjectSurvey {
            file_paths: vec!["a.rs".into(), "b.rs".into()],
            manifests: vec![],
            total_files_seen: 10_000,
        };
        let prompt = build_init_prompt(&survey);
        assert!(prompt.contains("Project contains 10000 files"));
        assert!(prompt.contains("showing a sample of the first 2"));
    }

    #[test]
    fn disclosed_sample_size_matches_the_paths_actually_emitted() {
        let survey = ProjectSurvey {
            file_paths: (0..500).map(|i| format!("src/file_{i}.rs")).collect(),
            manifests: vec![],
            total_files_seen: 10_000,
        };
        let prompt = build_init_prompt(&survey);
        let emitted = prompt.lines().filter(|line| line.starts_with("- ")).count();
        assert_eq!(emitted, MAX_PATHS_IN_PROMPT);
        assert!(prompt.contains(&format!(
            "showing a sample of the first {MAX_PATHS_IN_PROMPT}"
        )));
        assert!(prompt.contains("- src/file_199.rs"));
        assert!(!prompt.contains("- src/file_200.rs"));
    }

    #[test]
    fn does_not_mention_truncation_when_nothing_was_dropped() {
        let survey = ProjectSurvey {
            file_paths: vec!["a.rs".into(), "b.rs".into()],
            manifests: vec![],
            total_files_seen: 2,
        };
        let prompt = build_init_prompt(&survey);
        assert!(prompt.contains("Project contains 2 files"));
        assert!(!prompt.contains("showing a sample"));
    }
}
