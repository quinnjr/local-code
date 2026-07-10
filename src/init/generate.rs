// src/init/generate.rs

use std::path::Path;

use daimon::model::SharedModel;
use daimon::model::types::{ChatRequest, Message};

use crate::init::prompt::build_init_prompt;
use crate::init::survey::ProjectSurvey;

#[derive(Debug, thiserror::Error)]
pub enum InitError {
    #[error("model call failed: {0}")]
    Model(#[from] daimon::DaimonError),
    #[error("failed to write AGENTS.md: {0}")]
    Write(#[source] std::io::Error),
    #[error("model returned an empty response; AGENTS.md left unchanged")]
    EmptyResponse,
}

const INIT_SYSTEM_PROMPT: &str = "You are generating an AGENTS.md file for a coding project. \
Be concrete and specific to the project you're shown, not generic boilerplate.";

/// Makes the one real LLM call `/init` needs: survey → prompt → generated
/// Markdown. Uses `model.generate_erased` directly (not `Agent::prompt`) since
/// this is a single, tool-free completion, not a ReAct turn.
pub async fn generate_agents_md(
    model: &SharedModel,
    survey: &ProjectSurvey,
) -> Result<String, InitError> {
    let request = ChatRequest {
        messages: vec![
            Message::system(INIT_SYSTEM_PROMPT),
            Message::user(build_init_prompt(survey)),
        ],
        tools: Vec::new(),
        temperature: Some(0.2),
        max_tokens: Some(2048),
    };
    let response = model.generate_erased(&request).await?;
    let text = response.text().to_string();
    // A blank/whitespace-only response is treated the same way
    // `load_project_context` treats a blank AGENTS.md/CLAUDE.md: as meaningfully
    // absent. Reject it here rather than letting `write_agents_md` silently
    // overwrite a perfectly good existing AGENTS.md with nothing.
    if text.trim().is_empty() {
        return Err(InitError::EmptyResponse);
    }
    Ok(text)
}

/// Writes `content` to `<project_root>/AGENTS.md`, overwriting any existing
/// file. Never writes `CLAUDE.md` — that file is read for compatibility with
/// existing Claude Code projects, not owned by this tool, per spec section 4.
pub fn write_agents_md(project_root: &Path, content: &str) -> Result<(), InitError> {
    std::fs::write(project_root.join("AGENTS.md"), content).map_err(InitError::Write)
}

#[cfg(test)]
mod tests {
    use super::*;
    use daimon::model::types::{ChatResponse, StopReason, Usage};
    use daimon::stream::ResponseStream;
    use std::sync::Arc;
    use tempfile::tempdir;

    struct FixedResponseModel(String);
    impl daimon::model::Model for FixedResponseModel {
        async fn generate(&self, _request: &ChatRequest) -> daimon::Result<ChatResponse> {
            Ok(ChatResponse {
                message: Message::assistant(self.0.clone()),
                stop_reason: StopReason::EndTurn,
                usage: Some(Usage::default()),
            })
        }
        async fn generate_stream(&self, _request: &ChatRequest) -> daimon::Result<ResponseStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn generate_agents_md_returns_the_models_text() {
        let model: SharedModel = Arc::new(FixedResponseModel(
            "# AGENTS.md\n\nThis is a Rust crate.".into(),
        ));
        let survey = ProjectSurvey {
            file_paths: vec!["Cargo.toml".into()],
            manifests: vec![],
            ..Default::default()
        };
        let content = generate_agents_md(&model, &survey).await.unwrap();
        assert!(content.contains("This is a Rust crate."));
    }

    #[test]
    fn write_agents_md_creates_the_file_at_the_project_root() {
        let dir = tempdir().unwrap();
        write_agents_md(dir.path(), "# AGENTS.md\n\ncontent").unwrap();
        let written = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert!(written.contains("content"));
        assert!(!dir.path().join("CLAUDE.md").exists());
    }

    #[tokio::test]
    async fn generate_agents_md_rejects_a_blank_response() {
        let model: SharedModel = Arc::new(FixedResponseModel("   \n\t  ".into()));
        let survey = ProjectSurvey {
            file_paths: vec!["Cargo.toml".into()],
            ..Default::default()
        };
        let err = generate_agents_md(&model, &survey).await.unwrap_err();
        assert!(matches!(err, InitError::EmptyResponse));
    }

    #[test]
    fn write_agents_md_overwrites_an_existing_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "old content").unwrap();
        write_agents_md(dir.path(), "new content").unwrap();
        let written = std::fs::read_to_string(dir.path().join("AGENTS.md")).unwrap();
        assert_eq!(written, "new content");
    }
}
