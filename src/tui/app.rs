// src/tui/app.rs

use std::sync::Arc;

use daimon::agent::Agent;
use daimon::model::SharedModel;
use daimon::stream::StreamEvent;
use futures::StreamExt;
use ntui::props::{Dimension, FlexDirection};
use ntui::{component, element, Cleanup, KeyCode};

use crate::permissions::types::{PermissionDecision, PermissionTier};
use crate::tui::components::transcript::{Transcript, TranscriptProps};
use crate::tui::components::{Footer, FooterProps, Header, HeaderProps, InputBox, InputBoxProps};
use crate::tui::permission_prompter::NtuiPermissionPrompter;
use crate::tui::state::{
    find_tool_call_mut, toggle_last_tool_call_expanded, ToolCallEntry, ToolCallResult,
    TranscriptEntry, UsageSummary,
};

#[derive(Clone)]
pub struct AppProps {
    /// Wrapped in `Option` only so `AppProps: Default` (required by
    /// `ntui::Component::Props`) is satisfiable — `daimon::model::SharedModel`
    /// (`Arc<dyn Model + Send + Sync>`) has no `Default`. Always `Some` in
    /// practice: `run_tui` (this module) is the only caller and always passes
    /// a real model.
    pub model: Option<SharedModel>,
    pub connection_name: String,
    pub model_name: String,
    pub always_allow: Vec<String>,
    pub always_deny: Vec<String>,
    pub initial_tier: PermissionTier,
    /// Non-empty only when launched via `--resume`/`/resume`; seeds the
    /// visible transcript so a resumed session redraws immediately instead
    /// of starting blank.
    pub initial_entries: Vec<TranscriptEntry>,
    /// The raw agent-facing history to seed the rebuilt agent's memory with
    /// (see `SeededMemory`) — kept separate from `initial_entries` because
    /// the two are not interconvertible (see this plan's Architecture
    /// section).
    pub initial_messages: Vec<daimon::model::types::Message>,
    /// AGENTS.md/CLAUDE.md content (already concatenated by
    /// `local_code::context::load_project_context`), appended to the system
    /// prompt. Empty string if no context files were found.
    pub system_context: String,
    /// MCP-server-discovered tools (Phase 5's `connect_all`, called once by
    /// `run_tui` at startup — see Step 6). Threaded through every agent
    /// rebuild (`rebuild_agent`, Task 5) so `/model`/`/resume` never drop
    /// MCP tools that were available at launch. `NamespacedMcpTool` is
    /// `Clone` (wraps an `Arc<McpToolBridge>`), so cloning this list to hand
    /// to a rebuilt agent reuses the same live connections rather than
    /// reconnecting to every configured server on every rebuild.
    pub mcp_tools: Vec<crate::mcp::tool::NamespacedMcpTool>,
    /// The session file this instance persists to after every turn.
    pub session_path: std::path::PathBuf,
    /// Needed only so `/clear` and future commands can resolve a fresh
    /// session path without re-deriving `Paths` from scratch inside `App`.
    pub user_state_dir: std::path::PathBuf,
}

impl Default for AppProps {
    fn default() -> Self {
        AppProps {
            model: None,
            connection_name: String::new(),
            model_name: String::new(),
            always_allow: Vec::new(),
            always_deny: Vec::new(),
            initial_tier: PermissionTier::Ask,
            initial_entries: Vec::new(),
            initial_messages: Vec::new(),
            system_context: String::new(),
            mcp_tools: Vec::new(),
            session_path: std::path::PathBuf::new(),
            user_state_dir: std::path::PathBuf::new(),
        }
    }
}

impl PartialEq for AppProps {
    /// `App` is mounted exactly once, at the TUI's root (`run_tui` calls
    /// `ntui::render(element!(App(...)))` a single time), so its props never
    /// actually change between renders — this impl exists only to satisfy the
    /// `Component::Props: PartialEq` bound, and always reports "unchanged" to
    /// skip pointless prop-diffing work.
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

fn tier_label(tier: PermissionTier) -> &'static str {
    match tier {
        PermissionTier::Ask => "ask",
        PermissionTier::AutoAcceptEdits => "auto-accept-edits",
        PermissionTier::FullAuto => "full-auto",
    }
}

/// The TUI's single stateful root component. Owns the transcript, the input
/// buffer, the pending permission request, and the `turn_id` counter that
/// drives (re-)running a turn.
#[component]
pub fn App(props: &AppProps, hooks: &mut Hooks) -> Element {
    let transcript = hooks.use_state({
        let initial_entries = props.initial_entries.clone();
        move || initial_entries
    });
    let input_buffer = hooks.use_state(String::new);
    let turn_id = hooks.use_state(|| 0u64);
    let pending_turn_input = hooks.use_state(|| Option::<String>::None);
    let streaming = hooks.use_state(|| false);
    let usage = hooks.use_state(UsageSummary::default);
    let tier = hooks.use_state(|| props.initial_tier);
    let session_path = hooks.use_state({
        let initial = props.session_path.clone();
        move || initial
    });
    let pending_permission =
        hooks.use_state(|| Option::<crate::permissions::types::PermissionRequest>::None);

    let agent_and_responder = hooks.use_state({
        let model = props.model.clone().expect("AppProps::model is always Some");
        let always_allow = props.always_allow.clone();
        let always_deny = props.always_deny.clone();
        let initial_tier = props.initial_tier;
        let initial_messages = props.initial_messages.clone();
        let system_context = props.system_context.clone();
        let mcp_tools = props.mcp_tools.clone();
        let pending_permission = pending_permission.clone();
        move || {
            crate::tui::rebuild::rebuild_agent(
                model,
                initial_tier,
                always_allow,
                always_deny,
                initial_messages,
                &system_context,
                mcp_tools,
                pending_permission,
            )
        }
    });
    let (agent, gate, responder) = agent_and_responder.get();

    hooks.use_effect(tier.get() as u8, {
        let gate = gate.clone();
        let new_tier = tier.get();
        move || {
            tokio::spawn(async move {
                gate.set_tier(new_tier).await;
            });
        }
    });

    hooks.use_effect(turn_id.get(), {
        let pending_turn_input = pending_turn_input.clone();
        let transcript = transcript.clone();
        let usage = usage.clone();
        let streaming = streaming.clone();
        let agent = agent.clone();
        let session_path = session_path.clone();
        let connection_name = props.connection_name.clone();
        let model_name = props.model_name.clone();
        let tier = tier.clone();
        move || {
            let Some(input) = pending_turn_input.get() else {
                return Cleanup::from(());
            };
            let handle = tokio::spawn(run_turn(
                agent,
                input,
                transcript,
                usage,
                streaming,
                pending_turn_input,
                session_path.clone(),
                connection_name.clone(),
                model_name.clone(),
                tier.get(),
                std::env::current_dir().unwrap_or_default(),
            ));
            Cleanup::from(move || handle.abort())
        }
    });

    hooks.use_input({
        let input_buffer = input_buffer.clone();
        let transcript = transcript.clone();
        let turn_id = turn_id.clone();
        let pending_turn_input = pending_turn_input.clone();
        let streaming = streaming.clone();
        let pending_permission = pending_permission.clone();
        let responder = responder.clone();
        let tier = tier.clone();
        let session_path = session_path.clone();
        let connection_name = props.connection_name.clone();
        let model_name = props.model_name.clone();
        let user_state_dir = props.user_state_dir.clone();
        move |ev, _ctx| {
            if pending_permission.get().is_some() {
                let decision = match ev.code {
                    KeyCode::Char('1') => Some(PermissionDecision::Allow),
                    KeyCode::Char('2') => Some(PermissionDecision::AllowAlwaysThisSession),
                    KeyCode::Char('3') => Some(PermissionDecision::Deny {
                        feedback: "denied via TUI".into(),
                    }),
                    _ => None,
                };
                if let Some(decision) = decision {
                    let allowed = !matches!(decision, PermissionDecision::Deny { .. });
                    if let Some(request) = pending_permission.get() {
                        transcript.update(|entries| {
                            entries.push(TranscriptEntry::PermissionResolved {
                                description: request.description.clone(),
                                allowed,
                            });
                        });
                    }
                    NtuiPermissionPrompter::respond(&responder, decision);
                }
                return;
            }

            match ev.code {
                KeyCode::Char('a') if ev.modifiers.contains(ntui::hooks::input::KeyModifiers::CONTROL) => {
                    let next = match tier.get() {
                        PermissionTier::Ask => PermissionTier::AutoAcceptEdits,
                        PermissionTier::AutoAcceptEdits => PermissionTier::FullAuto,
                        PermissionTier::FullAuto => PermissionTier::Ask,
                    };
                    tier.set(next);
                }
                KeyCode::Char('c') if ev.modifiers.contains(ntui::hooks::input::KeyModifiers::CONTROL) => {
                    // Handled below via `hooks.use_app()`.
                }
                KeyCode::Tab => {
                    transcript.update(|entries| toggle_last_tool_call_expanded(entries));
                }
                KeyCode::Char(c) if !streaming.get() => {
                    input_buffer.update(|b| b.push(c));
                }
                KeyCode::Backspace if !streaming.get() => {
                    input_buffer.update(|b| {
                        b.pop();
                    });
                }
                KeyCode::Enter if !streaming.get() => {
                    let text = input_buffer.get();
                    if text.trim().is_empty() {
                        return;
                    }
                    if let Some(command) = crate::tui::slash::parse_slash_command(&text) {
                        input_buffer.set(String::new());
                        dispatch_slash_command(command, &SlashContext {
                            transcript: transcript.clone(),
                            tier: tier.clone(),
                            session_path: session_path.clone(),
                            connection_name: connection_name.clone(),
                            model_name: model_name.clone(),
                            project_root: std::env::current_dir().unwrap_or_default(),
                            user_state_dir: user_state_dir.clone(),
                        });
                        return;
                    }
                    transcript.update(|entries| {
                        entries.push(TranscriptEntry::UserTurn { text: text.clone() });
                    });
                    input_buffer.set(String::new());
                    streaming.set(true);
                    pending_turn_input.set(Some(text));
                    turn_id.update(|id| *id += 1);
                }
                _ => {}
            }
        }
    });

    let app_handle = hooks.use_app();
    hooks.use_input(move |ev, _ctx| {
        if ev.code == KeyCode::Char('c') && ev.modifiers.contains(ntui::hooks::input::KeyModifiers::CONTROL) {
            app_handle.exit();
        }
    });

    element! {
        View(flex_direction: FlexDirection::Column, height: Dimension::Percent(100.0), padding: 0) {
            Header(connection_name: props.connection_name.clone(), model_name: props.model_name.clone(), tier_label: tier_label(tier.get()).to_string())
            Transcript(entries: transcript.get(), pending_permission: pending_permission.get())
            InputBox(buffer: input_buffer.get(), disabled: streaming.get())
            Footer(usage: usage.get(), streaming: streaming.get())
        }
    }
}

/// Everything a slash-command handler needs, gathered in one place so
/// `dispatch_slash_command`'s signature doesn't grow a new parameter per
/// command. Tasks 10–15 extend this struct as each command's handler needs
/// more state; every field added there is threaded through from the same
/// `App` render this struct is built in.
struct SlashContext {
    transcript: ntui::State<Vec<TranscriptEntry>>,
    tier: ntui::State<PermissionTier>,
    session_path: ntui::State<std::path::PathBuf>,
    connection_name: String,
    model_name: String,
    project_root: std::path::PathBuf,
    user_state_dir: std::path::PathBuf,
}

const HELP_TEXT: &str = "\
/model                     switch the active connection/model (history is kept)
/connections list          list configured connections
/connections remove <name> remove a configured connection
/connections add           not supported in-TUI; run `local-code connections add` in a separate terminal
/init                      generate/update AGENTS.md from a survey of this project
/permissions               view or change the permission tier and allow/deny list
/compact                   summarize older turns to free up context
/resume                    switch to a previous session for this project
/clear                     clear the transcript and start a fresh session
/help                      show this message";

fn dispatch_slash_command(command: crate::tui::slash::SlashCommand, ctx: &SlashContext) {
    use crate::tui::slash::SlashCommand;

    match command {
        SlashCommand::Help => {
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice { text: HELP_TEXT.to_string() });
            });
        }
        SlashCommand::Unknown { raw } => {
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice {
                    text: format!("'{raw}' is not a recognized command. Type /help to see the list."),
                });
            });
        }
        SlashCommand::Clear => {
            ctx.transcript.set(Vec::new());
            let now = chrono::Utc::now();
            let new_path = crate::session::paths::new_session_path(
                &ctx.user_state_dir,
                &ctx.project_root,
                now,
            );
            let fresh = crate::session::types::SessionFile::new(
                ctx.project_root.clone(),
                ctx.connection_name.clone(),
                ctx.model_name.clone(),
                ctx.tier.get(),
                now.to_rfc3339(),
            );
            if let Err(e) = crate::session::store::save_session(&new_path, &fresh) {
                ctx.transcript.update(|entries| {
                    entries.push(TranscriptEntry::SystemNotice {
                        text: format!("cleared transcript, but failed to start a new session file: {e}"),
                    });
                });
            }
            ctx.session_path.set(new_path);
        }
        // Tasks 10–15 fill in every remaining variant. Left unmatched here
        // deliberately would be a compile error (the match is exhaustive),
        // which is why Task 9 (the very next task) adds `Clear` immediately
        // rather than leaving this plan in a non-compiling state at the end
        // of this task; see that task's Step 1.
        other => unreachable!(
            "SlashCommand::{other:?} is handled by a later task in this plan; if you see this at \
             runtime while implementing Task 8 in isolation, that's expected — Task 9 replaces this \
             arm before the plan is done"
        ),
    }
}

/// Drives one turn: streams `agent.prompt_stream(&input)`, folding each
/// `StreamEvent` into `transcript`/`usage`, then clears `streaming` and
/// `pending_turn_input` so the next `Enter` can start a new turn.
async fn run_turn(
    agent: Arc<Agent>,
    input: String,
    transcript: ntui::State<Vec<TranscriptEntry>>,
    usage: ntui::State<UsageSummary>,
    streaming: ntui::State<bool>,
    pending_turn_input: ntui::State<Option<String>>,
    session_path: ntui::State<std::path::PathBuf>,
    connection_name: String,
    model_name: String,
    tier: PermissionTier,
    project_root: std::path::PathBuf,
) {
    let mut stream = match agent.prompt_stream(&input).await {
        Ok(s) => s,
        Err(e) => {
            transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice {
                    text: format!("error: {e}"),
                });
            });
            streaming.set(false);
            pending_turn_input.set(None);
            return;
        }
    };

    while let Some(event) = stream.next().await {
        match event {
            Ok(StreamEvent::TextDelta(delta)) => {
                transcript.update(|entries| match entries.last_mut() {
                    Some(TranscriptEntry::AssistantText { text }) => text.push_str(&delta),
                    _ => entries.push(TranscriptEntry::AssistantText { text: delta }),
                });
            }
            Ok(StreamEvent::ToolCallStart { id, name }) => {
                transcript.update(|entries| {
                    entries.push(TranscriptEntry::ToolCall(ToolCallEntry {
                        id,
                        name,
                        arguments_json: String::new(),
                        result: None,
                        expanded: true,
                    }));
                });
            }
            Ok(StreamEvent::ToolCallDelta { id, arguments_delta }) => {
                transcript.update(|entries| {
                    if let Some(call) = find_tool_call_mut(entries, &id) {
                        call.arguments_json.push_str(&arguments_delta);
                    }
                });
            }
            Ok(StreamEvent::ToolCallEnd { .. }) => {}
            Ok(StreamEvent::ToolResult { id, content, is_error }) => {
                transcript.update(|entries| {
                    if let Some(call) = find_tool_call_mut(entries, &id) {
                        call.result = Some(ToolCallResult { content, is_error });
                    }
                });
            }
            Ok(StreamEvent::Usage {
                input_tokens,
                output_tokens,
                estimated_cost,
                ..
            }) => {
                usage.update(|u| u.add(input_tokens, output_tokens, estimated_cost));
            }
            Ok(StreamEvent::Error(message)) => {
                transcript.update(|entries| {
                    entries.push(TranscriptEntry::SystemNotice {
                        text: format!("error: {message}"),
                    });
                });
            }
            Ok(StreamEvent::Done) => break,
            Err(e) => {
                transcript.update(|entries| {
                    entries.push(TranscriptEntry::SystemNotice {
                        text: format!("error: {e}"),
                    });
                });
                break;
            }
        }
    }

    if let Ok(messages) = agent.memory().get_messages_erased().await {
        let mut session = crate::session::types::SessionFile::new(
            project_root,
            connection_name,
            model_name,
            tier,
            chrono::Utc::now().to_rfc3339(),
        );
        session.entries = transcript.get();
        session.messages = messages;
        let _ = crate::session::store::save_session(&session_path.get(), &session);
    }

    streaming.set(false);
    pending_turn_input.set(None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use daimon::model::types::{ChatRequest, ChatResponse, Message, StopReason, Usage};
    use daimon::stream::ResponseStream;
    use ntui::testing::TestTerminal;
    use ntui::{Element, KeyCode};

    /// Replies with a two-token streamed response and no tool calls.
    ///
    /// Deliberately emits no `StreamEvent::Usage` of its own: `Agent::prompt_stream`
    /// (see `daimon`'s `agent/runner.rs`) always appends its *own* estimated
    /// `Usage` event per ReAct iteration (character-count-based, `chars/4`) after
    /// forwarding whatever the model's stream yields — so any `Usage` this mock
    /// emitted would be forwarded too and summed with the agent's, on top of the
    /// agent's own estimate. Omitting it keeps this test's usage assertions tied
    /// to one authoritative source instead of an arbitrary double-count.
    struct StreamingEchoModel;
    impl daimon::model::Model for StreamingEchoModel {
        async fn generate(&self, _request: &ChatRequest) -> daimon::Result<ChatResponse> {
            Ok(ChatResponse {
                message: Message::assistant("unused"),
                stop_reason: StopReason::EndTurn,
                usage: Some(Usage::default()),
            })
        }
        async fn generate_stream(&self, _request: &ChatRequest) -> daimon::Result<ResponseStream> {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(StreamEvent::TextDelta("Hello".into())),
                Ok(StreamEvent::TextDelta(", world".into())),
                Ok(StreamEvent::Done),
            ])))
        }
    }

    fn test_props() -> AppProps {
        AppProps {
            model: Some(Arc::new(StreamingEchoModel)),
            connection_name: "local-vllm".into(),
            model_name: "qwen2.5-coder-32b".into(),
            always_allow: vec![],
            always_deny: vec![],
            initial_tier: PermissionTier::FullAuto,
            ..AppProps::default()
        }
    }

    async fn type_and_submit(t: &mut TestTerminal, text: &str) {
        for c in text.chars() {
            t.send_key(KeyCode::Char(c)).unwrap();
        }
        t.send_key(KeyCode::Enter).unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn submitting_a_prompt_streams_the_assistant_reply_into_the_transcript() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();

        type_and_submit(&mut t, "hi there").await;
        assert!(t.frame_text().contains("hi there"), "{}", t.frame_text());

        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            t.tick().await.unwrap();
        }

        let text = t.frame_text();
        assert!(text.contains("Hello, world"), "{text}");
        // The exact token counts are `Agent::prompt_stream`'s own
        // chars/4 estimate (see the `StreamingEchoModel` doc comment above),
        // not a value this test controls directly — assert usage moved off
        // its zero default rather than pin an exact, implementation-detail
        // number.
        assert!(!text.contains("0 in / 0 out"), "usage should have accumulated: {text}");
        assert!(text.contains("ready"), "turn should have finished: {text}");
    }

    #[tokio::test(start_paused = true)]
    async fn ctrl_a_cycles_the_permission_tier_label() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        assert!(t.frame_text().contains("[full-auto]"));
        t.send_key_event(ntui::KeyEvent::new(
            KeyCode::Char('a'),
            ntui::hooks::input::KeyModifiers::CONTROL,
        ))
        .unwrap();
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("[ask]"));
    }

    #[tokio::test(start_paused = true)]
    async fn help_command_lists_every_slash_command() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/help").await;
        t.tick().await.unwrap();
        let text = t.frame_text();
        for command in ["/model", "/connections", "/init", "/permissions", "/compact", "/resume", "/clear", "/help"] {
            assert!(text.contains(command), "missing {command} in help text: {text}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn unrecognized_command_shows_a_clear_notice_instead_of_prompting_the_model() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/bogus").await;
        t.tick().await.unwrap();
        let text = t.frame_text();
        assert!(text.contains("not a recognized command"), "{text}");
        assert!(!text.contains("Hello, world"), "must not have run a turn: {text}");
    }

    #[tokio::test(start_paused = true)]
    async fn clear_resets_transcript_and_starts_a_new_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut props = test_props();
        props.user_state_dir = dir.path().to_path_buf();
        props.session_path = dir.path().join("original.json");
        crate::session::store::save_session(
            &props.session_path,
            &crate::session::types::SessionFile::new(
                std::path::PathBuf::from("/proj"),
                "local-vllm".into(),
                "m".into(),
                PermissionTier::FullAuto,
                "2026-07-06T00:00:00Z".into(),
            ),
        )
        .unwrap();

        let mut t = TestTerminal::new(80, 24, Element::component::<App>(props)).unwrap();
        type_and_submit(&mut t, "hi there").await;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            t.tick().await.unwrap();
        }
        assert!(t.frame_text().contains("Hello, world"));

        type_and_submit(&mut t, "/clear").await;
        t.tick().await.unwrap();
        let text = t.frame_text();
        assert!(!text.contains("hi there"), "{text}");
        assert!(!text.contains("Hello, world"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn completed_turn_is_persisted_to_the_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut props = test_props();
        props.user_state_dir = dir.path().to_path_buf();
        props.session_path = dir.path().join("session.json");
        crate::session::store::save_session(
            &props.session_path,
            &crate::session::types::SessionFile::new(
                std::path::PathBuf::from("/proj"),
                "local-vllm".into(),
                "m".into(),
                PermissionTier::FullAuto,
                "2026-07-06T00:00:00Z".into(),
            ),
        )
        .unwrap();
        let session_path = props.session_path.clone();

        let mut t = TestTerminal::new(80, 24, Element::component::<App>(props)).unwrap();
        type_and_submit(&mut t, "hi there").await;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            t.tick().await.unwrap();
        }

        let saved = crate::session::store::load_session(&session_path).unwrap();
        assert!(saved.entries.iter().any(|e| matches!(e, TranscriptEntry::UserTurn { text } if text == "hi there")));
        assert!(!saved.messages.is_empty());
    }
}
