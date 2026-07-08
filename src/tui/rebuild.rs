// src/tui/rebuild.rs

use std::sync::{Arc, Mutex};

use daimon::agent::Agent;
use daimon::model::types::Message;
use daimon::model::SharedModel;
use tokio::sync::oneshot;

use crate::mcp::tool::NamespacedMcpTool;
use crate::permissions::gate::PermissionGate;
use crate::permissions::settings::PermissionSettings;
use crate::permissions::types::{PermissionDecision, PermissionRequest, PermissionTier};
use crate::skills::types::Skill;
use crate::tui::gated_tool::build_streaming_agent_with_history;
use crate::tui::permission_prompter::NtuiPermissionPrompter;

pub type ResponderHandle = Arc<Mutex<Option<oneshot::Sender<PermissionDecision>>>>;

/// Builds a fresh `(Agent, PermissionGate, ResponderHandle)` triple: a new
/// `NtuiPermissionPrompter` bound to `pending_permission`, a `PermissionGate`
/// at `initial_tier` with `permission_settings`, and an `Agent` seeded
/// with `initial_messages`, `extra_system_context`, and `mcp_tools` (already
/// `connect_all`-discovered `NamespacedMcpTool`s — `NamespacedMcpTool` is
/// `Clone`, wrapping an `Arc<McpToolBridge>`, so the *same* live MCP
/// connections can be handed to a freshly-rebuilt agent without reconnecting
/// to every server again on `/model`/`/resume`). This is the single place
/// that logic lives — `App`'s mount, `/model`, `/resume`, and `/mcp add` all
/// call it instead of duplicating the construction sequence.
///
/// Takes `permission_settings: PermissionSettings` rather than separate
/// `always_allow`/`always_deny: Vec<String>` parameters deliberately: two
/// adjacent same-typed `Vec<String>` params are a real footgun a caller
/// could transpose without a compile error, and `PermissionSettings` is the
/// type this function builds internally anyway.
pub fn rebuild_agent(
    model: SharedModel,
    initial_tier: PermissionTier,
    permission_settings: PermissionSettings,
    initial_messages: Vec<Message>,
    extra_system_context: &str,
    mcp_tools: Vec<NamespacedMcpTool>,
    skills: Vec<Skill>,
    pending_permission: ntui::State<Option<PermissionRequest>>,
) -> (Arc<Agent>, Arc<PermissionGate>, ResponderHandle) {
    let prompter = NtuiPermissionPrompter::new(pending_permission);
    let responder = prompter.responder_handle();
    let gate = Arc::new(PermissionGate::new(initial_tier, permission_settings, Arc::new(prompter)));
    let agent = Arc::new(
        build_streaming_agent_with_history(
            model,
            gate.clone(),
            initial_messages,
            extra_system_context,
            mcp_tools,
            skills,
        )
        .expect("agent construction should not fail"),
    );
    (agent, gate, responder)
}

/// Reloads `old_agent`'s conversation history and calls [`rebuild_agent`]
/// with it — the shared core of both `/model`'s and `/mcp add`'s rebuild
/// flow (they differ only in what they do with the result: `/model` swaps
/// the active model, `/mcp add` merges in newly-discovered tools). `/resume`
/// does NOT use this — it rebuilds from a *loaded session's* messages, not
/// the live agent's current history, so reloading would be wrong there.
pub async fn rebuild_agent_from_history(
    old_agent: &Agent,
    model: SharedModel,
    initial_tier: PermissionTier,
    permission_settings: PermissionSettings,
    extra_system_context: &str,
    mcp_tools: Vec<NamespacedMcpTool>,
    skills: Vec<Skill>,
    pending_permission: ntui::State<Option<PermissionRequest>>,
) -> (Arc<Agent>, Arc<PermissionGate>, ResponderHandle) {
    let history = old_agent.memory().get_messages_erased().await.unwrap_or_default();
    rebuild_agent(
        model,
        initial_tier,
        permission_settings,
        history,
        extra_system_context,
        mcp_tools,
        skills,
        pending_permission,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use daimon::model::types::{ChatRequest, ChatResponse, StopReason, Usage};
    use daimon::stream::ResponseStream;
    use ntui::testing::TestTerminal;
    use ntui::{component, element, Element};

    struct EchoModel;
    impl daimon::model::Model for EchoModel {
        async fn generate(&self, request: &ChatRequest) -> daimon::Result<ChatResponse> {
            Ok(ChatResponse {
                message: Message::assistant(format!("messages={}", request.messages.len())),
                stop_reason: StopReason::EndTurn,
                usage: Some(Usage::default()),
            })
        }
        async fn generate_stream(&self, _request: &ChatRequest) -> daimon::Result<ResponseStream> {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    #[derive(Clone, PartialEq, Default)]
    struct HarnessProps;

    #[component]
    fn Harness(_props: &HarnessProps, hooks: &mut ntui::Hooks) -> ntui::Element {
        let pending = hooks.use_state(|| Option::<PermissionRequest>::None);
        let result_text = hooks.use_state(|| "not built yet".to_string());

        hooks.use_effect((), {
            let pending = pending.clone();
            let result_text = result_text.clone();
            move || {
                let model: SharedModel = Arc::new(EchoModel);
                let (agent, _gate, _responder) = rebuild_agent(
                    model,
                    PermissionTier::FullAuto,
                    PermissionSettings::default(),
                    vec![Message::user("seeded turn")],
                    "",
                    Vec::new(),
                    Vec::new(),
                    pending,
                );
                tokio::spawn(async move {
                    let response = agent.prompt("hi").await.unwrap();
                    result_text.set(response.text().to_string());
                });
            }
        });

        element! {
            View { Text(content: result_text.get()) }
        }
    }

    #[tokio::test]
    async fn rebuild_agent_produces_a_working_agent_seeded_with_history() {
        let mut t = TestTerminal::new(60, 1, Element::component::<Harness>(HarnessProps)).unwrap();
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            t.tick().await.unwrap();
        }
        // system prompt + 1 seeded message + "hi" = 3
        assert!(t.frame_text().contains("messages=3"), "{}", t.frame_text());
    }
}
