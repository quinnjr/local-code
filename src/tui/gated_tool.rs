use std::sync::Arc;

use daimon::agent::{Agent, AgentBuilder};
use daimon::model::SharedModel;

use crate::agent::build::register_all_tools;
#[cfg(test)]
use crate::agent::gated_tool::GatedTool;
use crate::mcp::tool::NamespacedMcpTool;
use crate::permissions::gate::PermissionGate;
use crate::skills::types::Skill;
use crate::tui::memory_seed::SeededMemory;
use daimon::model::types::Message;

const SYSTEM_PROMPT: &str = "You are local-code, a coding assistant that talks only to \
local/local-network LLM backends. You can read, write, and edit files, run shell commands, and \
search the codebase via your tools. Prefer edit_file for targeted changes over rewriting whole \
files with write_file. Always explain what you're about to do before calling a tool that changes \
the filesystem or runs a command.";

/// Builds the `daimon::agent::Agent` used by the interactive TUI: (a) seeds the agent's memory
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
/// `/resume`, `/mcp add`).
///
/// `GatedTool` (Phase 2) works correctly under both `Agent::prompt` and
/// `Agent::prompt_stream` because it checks the permission gate inside
/// `execute()` itself, which both call unconditionally. (A no-history
/// `build_streaming_agent` variant used to sit alongside this fn; it had no
/// production callers — passing empty defaults here is identical.)
pub fn build_streaming_agent_with_history(
    model: SharedModel,
    gate: Arc<PermissionGate>,
    initial_messages: Vec<Message>,
    extra_system_context: &str,
    mcp_tools: Vec<NamespacedMcpTool>,
    skills: Vec<Skill>,
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
    register_all_tools(builder, gate, mcp_tools, skills).build()
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
        let agent =
            build_streaming_agent_with_history(model, gate, Vec::new(), "", Vec::new(), Vec::new());
        assert!(agent.is_ok());
    }

    #[tokio::test]
    async fn built_streaming_agent_streams_a_response() {
        use futures::StreamExt;

        let model: SharedModel = Arc::new(EchoModel);
        let gate = gate_with(PermissionTier::FullAuto, PermissionDecision::Allow);
        let agent =
            build_streaming_agent_with_history(model, gate, Vec::new(), "", Vec::new(), Vec::new())
                .unwrap();
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
    use crate::permissions::types::{
        PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier,
    };
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
        async fn generate(
            &self,
            request: &daimon::model::types::ChatRequest,
        ) -> daimon::Result<daimon::model::types::ChatResponse> {
            Ok(daimon::model::types::ChatResponse {
                message: Message::assistant(format!("saw {} messages", request.messages.len())),
                stop_reason: daimon::model::types::StopReason::EndTurn,
                usage: Some(daimon::model::types::Usage::default()),
            })
        }
        async fn generate_stream(
            &self,
            _request: &daimon::model::types::ChatRequest,
        ) -> daimon::Result<daimon::stream::ResponseStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[tokio::test]
    async fn seeded_history_is_visible_to_the_next_turn() {
        let model: SharedModel = Arc::new(EchoModel);
        let initial = vec![
            Message::user("earlier turn"),
            Message::assistant("earlier reply"),
        ];
        let agent =
            build_streaming_agent_with_history(model, gate(), initial, "", Vec::new(), Vec::new())
                .unwrap();

        let response = agent.prompt("new turn").await.unwrap();
        // system prompt + 2 seeded + new user turn = 4 messages sent to the model
        assert!(
            response.text().contains("saw 4 messages"),
            "{}",
            response.text()
        );
    }

    #[tokio::test]
    async fn extra_system_context_is_appended_to_the_prompt() {
        struct CapturingModel;
        impl daimon::model::Model for CapturingModel {
            async fn generate(
                &self,
                request: &daimon::model::types::ChatRequest,
            ) -> daimon::Result<daimon::model::types::ChatResponse> {
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
            async fn generate_stream(
                &self,
                _request: &daimon::model::types::ChatRequest,
            ) -> daimon::Result<daimon::stream::ResponseStream> {
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
            Vec::new(),
        )
        .unwrap();
        let response = agent.prompt("hi").await.unwrap();
        assert!(
            response
                .text()
                .contains("Project rule: never use unwrap()."),
            "{}",
            response.text()
        );
    }

    #[tokio::test]
    async fn a_skill_threaded_through_build_streaming_agent_with_history_is_callable() {
        use crate::skills::types::{LoadMode, Scope};
        use std::sync::atomic::{AtomicUsize, Ordering};

        // A model stub that, on its first turn, requests the `skill` tool for
        // "test-skill" (proving the tool was actually registered under that
        // name), then on its second turn echoes back whatever content the
        // tool result message carried (proving the skill's real body — not
        // just some placeholder — made it through the round trip).
        struct ToolCallingModel {
            call_count: AtomicUsize,
        }

        impl daimon::model::Model for ToolCallingModel {
            async fn generate(
                &self,
                request: &daimon::model::types::ChatRequest,
            ) -> daimon::Result<daimon::model::types::ChatResponse> {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);
                if count == 0 {
                    Ok(daimon::model::types::ChatResponse {
                        message: Message::assistant_with_tool_calls(vec![daimon::tool::ToolCall {
                            id: "call_1".into(),
                            name: "skill".into(),
                            arguments: serde_json::json!({"name": "test-skill"}),
                        }]),
                        stop_reason: daimon::model::types::StopReason::ToolUse,
                        usage: Some(daimon::model::types::Usage::default()),
                    })
                } else {
                    let tool_result = request
                        .messages
                        .last()
                        .and_then(|m| m.content.clone())
                        .unwrap_or_default();
                    Ok(daimon::model::types::ChatResponse {
                        message: Message::assistant(format!("skill said: {tool_result}")),
                        stop_reason: daimon::model::types::StopReason::EndTurn,
                        usage: Some(daimon::model::types::Usage::default()),
                    })
                }
            }
            async fn generate_stream(
                &self,
                _request: &daimon::model::types::ChatRequest,
            ) -> daimon::Result<daimon::stream::ResponseStream> {
                Ok(Box::pin(futures::stream::empty()))
            }
        }

        let skill = Skill {
            name: "test-skill".to_string(),
            description: "a fixture skill for wiring tests".to_string(),
            scope: Scope::Project,
            dir: std::path::PathBuf::from("/unused"),
            body: "test skill body content".to_string(),
            load_mode: LoadMode::ModelInvoked,
        };

        let model: SharedModel = Arc::new(ToolCallingModel {
            call_count: AtomicUsize::new(0),
        });
        let agent =
            build_streaming_agent_with_history(model, gate(), vec![], "", Vec::new(), vec![skill])
                .unwrap();

        let response = agent.prompt("please use the test skill").await.unwrap();
        assert!(
            response.text().contains("test skill body content"),
            "expected the skill tool's real body to flow through the agent, got: {}",
            response.text()
        );
    }
}
