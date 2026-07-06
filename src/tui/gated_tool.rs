// src/tui/gated_tool.rs

use std::sync::Arc;

use daimon::agent::{Agent, AgentBuilder};
use daimon::model::SharedModel;

use crate::agent::build::register_all_tools;
use crate::agent::gated_tool::GatedTool;
use crate::permissions::gate::PermissionGate;

const SYSTEM_PROMPT: &str = "You are local-code, a coding assistant that talks only to \
local/local-network LLM backends. You can read, write, and edit files, run shell commands, and \
search the codebase via your tools. Prefer edit_file for targeted changes over rewriting whole \
files with write_file. Always explain what you're about to do before calling a tool that changes \
the filesystem or runs a command.";

/// Builds the `daimon::agent::Agent` used by the interactive TUI: the same
/// `GatedTool`-wrapped built-in tools as headless mode's
/// `local_code::agent::build::build_agent`, registered via the identical
/// `local_code::agent::build::register_all_tools` function — not a
/// locally-redefined tool list. `GatedTool` (Phase 2) works correctly under
/// both `Agent::prompt` and `Agent::prompt_stream` because it checks the
/// permission gate inside `execute()` itself, which both call unconditionally.
pub fn build_streaming_agent(model: SharedModel, gate: Arc<PermissionGate>) -> daimon::Result<Agent> {
    let builder = AgentBuilder::new()
        .shared_model(model)
        .system_prompt(SYSTEM_PROMPT);
    register_all_tools(builder, gate).build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::{Bash, ReadFile, WriteFile};
    use crate::permissions::settings::PermissionSettings;
    use crate::permissions::types::{
        PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier,
    };
    use daimon::model::types::{ChatRequest, ChatResponse, Message, StopReason, Usage};
    use daimon::stream::{ResponseStream, StreamEvent};
    use daimon::tool::Tool;
    use std::future::Future;
    use std::pin::Pin;

    struct StubPrompter {
        decision: PermissionDecision,
    }
    impl PermissionPrompter for StubPrompter {
        fn prompt<'a>(
            &'a self,
            _request: &'a PermissionRequest,
        ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
            let decision = self.decision.clone();
            Box::pin(async move { decision })
        }
    }

    fn gate_with(tier: PermissionTier, decision: PermissionDecision) -> Arc<PermissionGate> {
        Arc::new(PermissionGate::new(
            tier,
            PermissionSettings::default(),
            Arc::new(StubPrompter { decision }),
        ))
    }

    #[tokio::test]
    async fn read_only_gated_tool_never_prompts_and_executes() {
        let gate = gate_with(
            PermissionTier::Ask,
            PermissionDecision::Deny {
                feedback: "should never be reached".into(),
            },
        );
        let tool = GatedTool::new(ReadFile, gate);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.txt");
        std::fs::write(&file, "hello").unwrap();
        let output = tool
            .execute(&serde_json::json!({"path": file.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(output.content, "hello");
    }

    #[tokio::test]
    async fn denied_edit_tool_never_touches_the_filesystem() {
        let gate = gate_with(
            PermissionTier::Ask,
            PermissionDecision::Deny {
                feedback: "no thanks".into(),
            },
        );
        let tool = GatedTool::new(WriteFile, gate);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("out.txt");
        let output = tool
            .execute(&serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "should not be written"
            }))
            .await
            .unwrap();
        assert!(output.is_error);
        assert_eq!(output.content, "no thanks");
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn allowed_bash_tool_executes() {
        let gate = gate_with(PermissionTier::FullAuto, PermissionDecision::Allow);
        let tool = GatedTool::new(Bash, gate);
        let output = tool
            .execute(&serde_json::json!({"command": "echo gated_ok"}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("gated_ok"));
    }

    struct EchoModel;
    impl daimon::model::Model for EchoModel {
        async fn generate(&self, request: &ChatRequest) -> daimon::Result<ChatResponse> {
            let last = request
                .messages
                .last()
                .and_then(|m| m.content.as_deref())
                .unwrap_or("");
            Ok(ChatResponse {
                message: Message::assistant(format!("echo: {last}")),
                stop_reason: StopReason::EndTurn,
                usage: Some(Usage::default()),
            })
        }
        async fn generate_stream(&self, _request: &ChatRequest) -> daimon::Result<ResponseStream> {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(StreamEvent::TextDelta("echo".into())),
                Ok(StreamEvent::Done),
            ])))
        }
    }

    #[test]
    fn build_streaming_agent_succeeds_with_all_six_tools() {
        let model: SharedModel = Arc::new(EchoModel);
        let gate = gate_with(PermissionTier::FullAuto, PermissionDecision::Allow);
        let agent = build_streaming_agent(model, gate);
        assert!(agent.is_ok());
    }

    #[tokio::test]
    async fn built_streaming_agent_streams_a_response() {
        use futures::StreamExt;

        let model: SharedModel = Arc::new(EchoModel);
        let gate = gate_with(PermissionTier::FullAuto, PermissionDecision::Allow);
        let agent = build_streaming_agent(model, gate).unwrap();
        let mut stream = agent.prompt_stream("hello").await.unwrap();
        let mut texts = Vec::new();
        while let Some(event) = stream.next().await {
            if let Ok(StreamEvent::TextDelta(t)) = event {
                texts.push(t);
            }
        }
        assert_eq!(texts, vec!["echo".to_string()]);
    }
}
