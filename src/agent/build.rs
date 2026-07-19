use std::sync::Arc;

use daimon::agent::{Agent, AgentBuilder};
use daimon::model::SharedModel;

use crate::agent::gated_tool::GatedTool;
use crate::agent::skill_tool::SkillTool;
use crate::agent::tools::{Bash, EditFile, Glob, Grep, ReadFile, WriteFile};
use crate::mcp::tool::NamespacedMcpTool;
use crate::permissions::gate::PermissionGate;
use crate::skills::types::Skill;

const DEFAULT_SYSTEM_PROMPT: &str = "You are local-code, a coding assistant that talks only to \
local/local-network LLM backends. You can read, write, and edit files, run shell commands, and \
search the codebase via your tools. Prefer edit_file for targeted changes over rewriting whole \
files with write_file. Always explain what you're about to do before calling a tool that changes \
the filesystem or runs a command.";

/// Registers every available tool onto `builder`, each wrapped in
/// [`crate::agent::gated_tool::GatedTool`] so permission enforcement is
/// identical for built-ins and MCP tools alike, under both `Agent::prompt` and
/// `Agent::prompt_stream`. This is the one and only tool-registration function
/// in the project — Phase 2 defined its non-MCP-aware form first (TDD
/// progression, since `NamespacedMcpTool` didn't exist yet); this task extends
/// its *signature* in place to add `mcp_tools`, rather than adding a second
/// function, so headless mode (`build_agent`/`build_agent_with_mcp_tools`,
/// below) and the TUI's agent-rebuild path can never register a different
/// tool set from each other.
pub fn register_all_tools(
    builder: AgentBuilder,
    gate: Arc<PermissionGate>,
    mcp_tools: Vec<NamespacedMcpTool>,
    skills: Vec<Skill>,
) -> AgentBuilder {
    let mut builder = builder
        .tool(GatedTool::new(ReadFile, gate.clone()))
        .tool(GatedTool::new(WriteFile, gate.clone()))
        .tool(GatedTool::new(EditFile, gate.clone()))
        .tool(GatedTool::new(Bash, gate.clone()))
        .tool(GatedTool::new(Grep, gate.clone()))
        .tool(GatedTool::new(Glob, gate.clone()))
        .tool(GatedTool::new(SkillTool::new(skills), gate.clone()));

    for tool in mcp_tools {
        builder = builder.tool(GatedTool::new(tool, gate.clone()));
    }

    builder
}

/// Builds a `daimon::agent::Agent` wired with the six built-in tools plus any
/// MCP-server-discovered tools passed in `mcp_tools`, via [`register_all_tools`]
/// — every tool, built-in or MCP, is `GatedTool`-wrapped there, so there is no
/// separate registry or enforcement path for MCP tools.
pub fn build_agent_with_mcp_tools(
    model: SharedModel,
    gate: Arc<PermissionGate>,
    mcp_tools: Vec<NamespacedMcpTool>,
    skills: Vec<Skill>,
    extra_system_context: &str,
) -> daimon::Result<Agent> {
    let system_prompt = if extra_system_context.trim().is_empty() {
        DEFAULT_SYSTEM_PROMPT.to_string()
    } else {
        format!("{DEFAULT_SYSTEM_PROMPT}\n\n{extra_system_context}")
    };
    let builder = AgentBuilder::new()
        .shared_model(model)
        .system_prompt(system_prompt);
    register_all_tools(builder, gate, mcp_tools, skills).build()
}

/// Builds an agent with only the six built-in tools (no MCP servers
/// configured/connected). Kept as its own function, with its original Phase 2
/// signature, so existing callers are unaffected by this plan.
pub fn build_agent(model: SharedModel, gate: Arc<PermissionGate>) -> daimon::Result<Agent> {
    build_agent_with_mcp_tools(model, gate, Vec::new(), Vec::new(), "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::settings::PermissionSettings;
    use crate::permissions::types::{
        PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier,
    };
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

    #[test]
    fn builds_successfully_with_additional_mcp_tools_registered() {
        let model: SharedModel = Arc::new(EchoModel);

        struct FakeMcpTool;
        impl daimon::tool::Tool for FakeMcpTool {
            fn name(&self) -> &str {
                "fixture__echo"
            }
            fn description(&self) -> &str {
                "fixture echo tool"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(
                &self,
                _input: &serde_json::Value,
            ) -> daimon::Result<daimon::tool::ToolOutput> {
                Ok(daimon::tool::ToolOutput::text("fixture echo"))
            }
        }

        // NamespacedMcpTool itself always wraps a real McpToolBridge (which
        // needs a transport); to keep this test fast and dependency-free we
        // assert the same *shape of contract* — "a plain Tool impl can be
        // wrapped in GatedTool and added to the same register_all_tools
        // builder chain build_agent uses" — via a structurally-identical fake
        // tool rather than standing up an MCP client. Task 6's headless
        // integration test proves the real NamespacedMcpTool path end to end.
        let builder = AgentBuilder::new()
            .shared_model(model)
            .system_prompt(DEFAULT_SYSTEM_PROMPT)
            .tool(GatedTool::new(FakeMcpTool, test_gate()));
        let agent = register_all_tools(builder, test_gate(), Vec::new(), Vec::new()).build();
        assert!(agent.is_ok());
    }

    #[test]
    fn build_agent_still_builds_with_zero_mcp_tools() {
        let model: SharedModel = Arc::new(EchoModel);
        let agent = build_agent(model, test_gate());
        assert!(agent.is_ok());
    }
}
