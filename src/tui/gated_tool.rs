// src/tui/gated_tool.rs

use std::sync::Arc;

use daimon::agent::{Agent, AgentBuilder};
use daimon::model::SharedModel;

use crate::agent::build::register_all_tools;
use crate::mcp::tool::NamespacedMcpTool;
use crate::permissions::gate::PermissionGate;
use crate::tui::memory_seed::SeededMemory;
use daimon::model::types::Message;
#[cfg(test)]
use crate::agent::gated_tool::GatedTool;

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
    register_all_tools(builder, gate, Vec::new(), Vec::new()).build()
}

/// Identical to [`build_streaming_agent`] but (a) seeds the agent's memory
/// with `initial_messages` via [`SeededMemory`] instead of starting empty,
/// (b) appends `extra_system_context` (AGENTS.md/CLAUDE.md content, or an
/// empty string if none was found) to the system prompt, and (c) registers
/// `mcp_tools` (already-discovered `NamespacedMcpTool`s — see
/// `local_code::mcp::connect::connect_all`, Phase 5, called once at `run_tui`
/// startup, Task 6) alongside the built-ins. Tool registration itself goes
/// through `local_code::agent::build::register_all_tools` — the exact same
/// function headless mode's `build_agent_with_mcp_tools` (Phase 5) calls —
/// rather than re-listing `GatedTool`-wrapped built-ins by hand, so the TUI
/// and headless paths can never register a different tool set from each
/// other. Used by every call site added in this plan (`App`'s mount, `/model`,
/// `/resume`); `build_streaming_agent` itself remains unchanged and is still
/// exercised by Phase 3's own tests.
pub fn build_streaming_agent_with_history(
    model: SharedModel,
    gate: Arc<PermissionGate>,
    initial_messages: Vec<Message>,
    extra_system_context: &str,
    mcp_tools: Vec<NamespacedMcpTool>,
) -> daimon::Result<Agent> {
    let system_prompt = if extra_system_context.trim().is_empty() {
        SYSTEM_PROMPT.to_string()
    } else {
        format!("{SYSTEM_PROMPT}\n\n{extra_system_context}")
    };

    let builder = AgentBuilder::new()
        .shared_model(model)
        .system_prompt(system_prompt)
        .memory(SeededMemory::new(initial_messages));
    register_all_tools(builder, gate, mcp_tools, Vec::new()).build()
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

#[cfg(test)]
mod with_history_tests {
    use super::*;
    use crate::permissions::settings::PermissionSettings;
    use crate::permissions::types::{PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier};
    use std::future::Future;
    use std::pin::Pin;

    struct AlwaysAllow;
    impl PermissionPrompter for AlwaysAllow {
        fn prompt<'a>(
            &'a self,
            _request: &'a PermissionRequest,
        ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
            Box::pin(async { PermissionDecision::Allow })
        }
    }

    fn gate() -> Arc<PermissionGate> {
        Arc::new(PermissionGate::new(
            PermissionTier::FullAuto,
            PermissionSettings::default(),
            Arc::new(AlwaysAllow),
        ))
    }

    struct EchoModel;
    impl daimon::model::Model for EchoModel {
        async fn generate(&self, request: &daimon::model::types::ChatRequest) -> daimon::Result<daimon::model::types::ChatResponse> {
            Ok(daimon::model::types::ChatResponse {
                message: Message::assistant(format!("saw {} messages", request.messages.len())),
                stop_reason: daimon::model::types::StopReason::EndTurn,
                usage: Some(daimon::model::types::Usage::default()),
            })
        }
        async fn generate_stream(&self, _request: &daimon::model::types::ChatRequest) -> daimon::Result<daimon::stream::ResponseStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn seeded_history_is_visible_to_the_next_turn() {
        let model: SharedModel = Arc::new(EchoModel);
        let initial = vec![Message::user("earlier turn"), Message::assistant("earlier reply")];
        let agent = build_streaming_agent_with_history(model, gate(), initial, "", Vec::new()).unwrap();

        let response = agent.prompt("new turn").await.unwrap();
        // system prompt + 2 seeded + new user turn = 4 messages sent to the model
        assert!(response.text().contains("saw 4 messages"), "{}", response.text());
    }

    #[tokio::test]
    async fn extra_system_context_is_appended_to_the_prompt() {
        struct CapturingModel;
        impl daimon::model::Model for CapturingModel {
            async fn generate(&self, request: &daimon::model::types::ChatRequest) -> daimon::Result<daimon::model::types::ChatResponse> {
                let system_text = request
                    .messages
                    .first()
                    .and_then(|m| m.content.clone())
                    .unwrap_or_default();
                Ok(daimon::model::types::ChatResponse {
                    message: Message::assistant(system_text),
                    stop_reason: daimon::model::types::StopReason::EndTurn,
                    usage: Some(daimon::model::types::Usage::default()),
                })
            }
            async fn generate_stream(&self, _request: &daimon::model::types::ChatRequest) -> daimon::Result<daimon::stream::ResponseStream> {
                Ok(Box::pin(futures::stream::empty()))
            }
        }

        let model: SharedModel = Arc::new(CapturingModel);
        let agent = build_streaming_agent_with_history(
            model,
            gate(),
            vec![],
            "Project rule: never use unwrap().",
            Vec::new(),
        )
        .unwrap();
        let response = agent.prompt("hi").await.unwrap();
        assert!(response.text().contains("Project rule: never use unwrap()."), "{}", response.text());
    }
}
