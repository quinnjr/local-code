// src/agent/build.rs

use std::sync::Arc;

use daimon::agent::{Agent, AgentBuilder};
use daimon::model::SharedModel;

use crate::agent::gated_tool::GatedTool;
use crate::agent::tools::{Bash, EditFile, Glob, Grep, ReadFile, WriteFile};
use crate::permissions::gate::PermissionGate;

const DEFAULT_SYSTEM_PROMPT: &str = "You are local-code, a coding assistant that talks only to \
local/local-network LLM backends. You can read, write, and edit files, run shell commands, and \
search the codebase via your tools. Prefer edit_file for targeted changes over rewriting whole \
files with write_file. Always explain what you're about to do before calling a tool that changes \
the filesystem or runs a command.";

/// Registers every available tool onto `builder`, each wrapped in
/// [`GatedTool`] so permission enforcement works identically whether the
/// resulting `Agent` is later driven via `prompt` or `prompt_stream`. This
/// phase's version registers only the six built-ins; a later MCP-client phase
/// extends this function's signature in place to add MCP-discovered tools
/// (each also `GatedTool`-wrapped). Both the headless path (`build_agent`,
/// below) and a later TUI path call this one function, so they never drift
/// apart.
pub fn register_all_tools(builder: AgentBuilder, gate: Arc<PermissionGate>) -> AgentBuilder {
    builder
        .tool(GatedTool::new(ReadFile, gate.clone()))
        .tool(GatedTool::new(WriteFile, gate.clone()))
        .tool(GatedTool::new(EditFile, gate.clone()))
        .tool(GatedTool::new(Bash, gate.clone()))
        .tool(GatedTool::new(Grep, gate.clone()))
        .tool(GatedTool::new(Glob, gate))
}

/// Builds a `daimon::agent::Agent` wired with the six `GatedTool`-wrapped
/// built-in tools via [`register_all_tools`]. No `daimon::middleware::Middleware`
/// is used anywhere — see `src/agent/gated_tool.rs` for why.
pub fn build_agent(model: SharedModel, gate: Arc<PermissionGate>) -> daimon::Result<Agent> {
    let builder = AgentBuilder::new()
        .shared_model(model)
        .system_prompt(DEFAULT_SYSTEM_PROMPT);
    register_all_tools(builder, gate).build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::settings::PermissionSettings;
    use crate::permissions::types::{PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier};
    use daimon::model::types::{ChatRequest, ChatResponse, Message, StopReason, Usage};
    use daimon::stream::ResponseStream;
    use std::future::Future;
    use std::pin::Pin;

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
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    struct AlwaysAllowPrompter;

    impl PermissionPrompter for AlwaysAllowPrompter {
        fn prompt<'a>(
            &'a self,
            _request: &'a PermissionRequest,
        ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
            Box::pin(async { PermissionDecision::Allow })
        }
    }

    fn test_gate() -> Arc<PermissionGate> {
        Arc::new(PermissionGate::new(
            PermissionTier::FullAuto,
            PermissionSettings::default(),
            Arc::new(AlwaysAllowPrompter),
        ))
    }

    #[test]
    fn builds_successfully_with_all_six_tools_registered() {
        let model: SharedModel = Arc::new(EchoModel);
        let agent = build_agent(model, test_gate());
        assert!(agent.is_ok());
    }

    #[tokio::test]
    async fn built_agent_responds_to_a_simple_prompt() {
        let model: SharedModel = Arc::new(EchoModel);
        let agent = build_agent(model, test_gate()).unwrap();
        let response = agent.prompt("hello").await.unwrap();
        assert!(response.text().contains("echo: hello"));
    }
}
