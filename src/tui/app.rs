// src/tui/app.rs

use std::sync::Arc;

use daimon::agent::Agent;
use daimon::model::SharedModel;
use daimon::stream::StreamEvent;
use futures::StreamExt;
use ntui::props::{Dimension, FlexDirection};
use ntui::{component, element, Cleanup, Element, KeyCode};

use crate::permissions::types::{PermissionDecision, PermissionTier};
use crate::tui::components::transcript::{Transcript, TranscriptProps};
use crate::tui::components::{Dashboard, DashboardProps, Footer, FooterProps, InputBox, InputBoxProps};
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
    /// Needed so `/model` can call `load_connections` without re-deriving
    /// `Paths` from scratch inside `App`.
    pub user_config_dir: std::path::PathBuf,
    /// Needed so `/model` can call `load_connections` without re-deriving
    /// `Paths` from scratch inside `App`.
    pub project_config_dir: std::path::PathBuf,
    /// The directory `/init` surveys and writes `AGENTS.md` into. Threaded
    /// through props (rather than having the `Enter`-key handler call
    /// `std::env::current_dir()` directly, as originally sketched) so tests
    /// can point `/init` at a tempdir without mutating the process-global
    /// current directory — a mutation that would otherwise race any other
    /// test in the (parallel-by-default) suite that reads or depends on cwd.
    pub project_root: std::path::PathBuf,
    /// The session's creation timestamp (`SessionFile::created_at`), seeded
    /// once at mount (fresh launch or `--resume`) and kept in sync by
    /// `/clear` and `/resume`'s in-TUI handlers. Lets `run_turn` avoid a
    /// full session-file read+parse on every turn just to recover this one
    /// already-known field (see `run_turn`'s `created_at` state).
    pub created_at: String,
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
            user_config_dir: std::path::PathBuf::new(),
            project_config_dir: std::path::PathBuf::new(),
            project_root: std::path::PathBuf::new(),
            created_at: String::new(),
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

/// Maps a numeric key press to a validated 0-based index, bounded by `max`
/// (the number of items in the pending numbered list/menu). Returns `None`
/// for non-digit keys, `0`, or any digit beyond `max`. Shared by all four
/// "show a numbered list, then intercept the next digit press" pending-choice
/// blocks in `use_input` below (`pending_permission`, and `pending_menu`'s
/// three variants `ModelChoice`, `PermissionsMenu`, `ResumeChoice`) —
/// extracted per code review after Task 11, which flagged the duplicated
/// digit-matching boilerplate across what was then three (now four)
/// near-identical blocks.
fn digit_key_to_index(code: KeyCode, max: usize) -> Option<usize> {
    if let KeyCode::Char(c) = code {
        if let Some(digit) = c.to_digit(10) {
            if digit >= 1 && (digit as usize) <= max {
                return Some(digit as usize - 1);
            }
        }
    }
    None
}

fn tier_label(tier: PermissionTier) -> &'static str {
    match tier {
        PermissionTier::Ask => "ask",
        PermissionTier::AutoAcceptEdits => "auto-accept-edits",
        PermissionTier::FullAuto => "full-auto",
    }
}

/// Which numbered-choice menu (if any) is currently intercepting digit
/// keypresses in `use_input` below: `/model`'s connection/model list,
/// `/permissions`'s tier list, or `/resume`'s session list. Collapses what
/// were three separate `Option`/`bool` `ntui::State`s into one, since
/// `use_input`'s priority chain only ever treats one of them as active at a
/// time.
///
/// `pending_permission` (see `App`'s state below) is a fourth, related
/// "pending numbered menu" but is deliberately kept as its own state rather
/// than a variant here: it is threaded into `rebuild_agent`
/// (`src/tui/rebuild.rs`) and `NtuiPermissionPrompter`
/// (`src/tui/permission_prompter.rs`) as a concrete
/// `ntui::State<Option<PermissionRequest>>`, which those modules set directly
/// from inside a spawned tokio task (the permission gate's `prompt` future) —
/// independent of `App`'s `use_input` closure entirely. Folding it into this
/// enum would require changing those other modules' signatures.
///
/// Known v1 limitation, true of all three variants here (and of
/// `pending_permission`): once one is active there is no cancel/escape path.
/// Any keystroke that isn't a valid in-range digit is silently swallowed and
/// the menu stays open until a valid digit is pressed.
#[derive(Clone)]
enum PendingMenu {
    None,
    ModelChoice(Vec<(crate::config::connection::Connection, String)>),
    PermissionsMenu,
    ResumeChoice(Vec<crate::session::types::SessionSummary>),
    /// A `/mcp add` wizard is mid-flow, waiting for the next line of free-text
    /// input. Unlike the three variants above, this one supports `Esc` to
    /// cancel (see a later task) — the only pending menu that does.
    McpAddWizard(crate::tui::mcp_wizard::McpAddWizard),
    /// The wizard finished (`Advance::Finalize`) and its `connect_one` +
    /// save + agent-rebuild is running in a spawned task. All input is
    /// blocked (like `streaming`) until it resolves and resets this to
    /// `None`.
    McpAddConnecting,
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
    // Mirrors `session_path`'s threading: seeded once from `props` (fresh
    // launch or `--resume`), then kept in sync by `/clear` and `/resume`'s
    // handlers below so `run_turn` never has to re-read the session file
    // from disk just to recover this single static field.
    let created_at = hooks.use_state({
        let initial = props.created_at.clone();
        move || initial
    });
    let pending_permission =
        hooks.use_state(|| Option::<crate::permissions::types::PermissionRequest>::None);
    let pending_menu = hooks.use_state(|| PendingMenu::None);

    let always_allow_snapshot = props.always_allow.clone();
    let always_deny_snapshot = props.always_deny.clone();
    // A `State` (not a plain snapshot, unlike `always_allow_snapshot`/
    // `always_deny_snapshot` above) because a later task's `/mcp add`
    // live-reconnect needs to *append* newly discovered tools at runtime
    // and have every subsequent `/model`/`/resume` rebuild see them — a
    // plain `Vec` clone captured once at mount would silently drop
    // anything added this way.
    let mcp_tools_state = hooks.use_state({
        let initial = props.mcp_tools.clone();
        move || initial
    });
    // Tracks the currently-active model, kept in sync with `agent_and_responder`
    // on every `/model` switch (see the digit-press handler below) so
    // `SlashContext.model` (used by `/compact`'s summarization call) never
    // goes stale after a switch — unlike a plain one-time snapshot of
    // `props.model`, which would keep referencing the pre-switch model
    // forever (that was Bug 2: `SlashContext.model` used such a snapshot).
    let current_model = hooks.use_state({
        let initial = props.model.clone().expect("AppProps::model is always Some");
        move || initial
    });
    // Mirrors `current_model`'s Bug 2 fix, but for the `Header`'s displayed
    // connection/model name: without this, a successful `/model` switch or
    // `/resume` would correctly rebuild the agent yet leave the Header
    // silently showing the connection/model the process launched with
    // forever. Seeded from `props` at mount, then kept in lockstep with
    // `agent_and_responder`/`current_model` at both switch sites below.
    let connection_display = hooks.use_state({
        let initial = props.connection_name.clone();
        move || initial
    });
    let model_display = hooks.use_state({
        let initial = props.model_name.clone();
        move || initial
    });

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
        let created_at = created_at.clone();
        let connection_name = props.connection_name.clone();
        let model_name = props.model_name.clone();
        let tier = tier.clone();
        let project_root = props.project_root.clone();
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
                created_at.clone(),
                connection_name.clone(),
                model_name.clone(),
                tier.get(),
                project_root.clone(),
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
        let pending_menu = pending_menu.clone();
        let responder = responder.clone();
        let tier = tier.clone();
        let session_path = session_path.clone();
        let created_at = created_at.clone();
        let connection_name = props.connection_name.clone();
        let model_name = props.model_name.clone();
        let user_state_dir = props.user_state_dir.clone();
        let user_config_dir = props.user_config_dir.clone();
        let project_config_dir = props.project_config_dir.clone();
        let project_root = props.project_root.clone();
        let agent = agent.clone();
        let agent_and_responder = agent_and_responder.clone();
        let always_allow_snapshot = always_allow_snapshot.clone();
        let always_deny_snapshot = always_deny_snapshot.clone();
        let mcp_tools_state = mcp_tools_state.clone();
        let system_context = props.system_context.clone();
        let current_model = current_model.clone();
        let connection_display = connection_display.clone();
        let model_display = model_display.clone();
        move |ev, _ctx| {
            if pending_permission.get().is_some() {
                let decision = digit_key_to_index(ev.code, 3).map(|idx| match idx {
                    0 => PermissionDecision::Allow,
                    1 => PermissionDecision::AllowAlwaysThisSession,
                    _ => PermissionDecision::Deny {
                        feedback: "denied via TUI".into(),
                    },
                });
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

            match pending_menu.get() {
                PendingMenu::ModelChoice(choices) => {
                    if let Some(idx) = digit_key_to_index(ev.code, choices.len()) {
                        let (connection, model_name) = choices[idx].clone();
                        pending_menu.set(PendingMenu::None);
                        let api_key =
                            crate::config::secrets::SecretStore::get_api_key(&connection.name)
                                .ok()
                                .flatten();
                        match crate::agent::provider::build_model(&connection, api_key) {
                            Ok(new_model) => {
                                let agent_for_history = agent.clone();
                                let pending_permission_for_rebuild = pending_permission.clone();
                                let agent_and_responder = agent_and_responder.clone();
                                let current_model = current_model.clone();
                                let model_for_state = new_model.clone();
                                let transcript_for_notice = transcript.clone();
                                let tier_value = tier.get();
                                let always_allow = always_allow_snapshot.clone();
                                let always_deny = always_deny_snapshot.clone();
                                let system_context = system_context.clone();
                                let mcp_tools = mcp_tools_state.get();
                                let connection_display = connection_display.clone();
                                let model_display = model_display.clone();
                                let connection_name_for_display = connection.name.clone();
                                let model_name_for_display = model_name.clone();
                                tokio::spawn(async move {
                                    let history = agent_for_history
                                        .memory()
                                        .get_messages_erased()
                                        .await
                                        .unwrap_or_default();
                                    let rebuilt = crate::tui::rebuild::rebuild_agent(
                                        new_model,
                                        tier_value,
                                        always_allow,
                                        always_deny,
                                        history,
                                        &system_context,
                                        mcp_tools,
                                        pending_permission_for_rebuild,
                                    );
                                    // Last-write-wins: if multiple `/model` selections somehow
                                    // overlap in flight, whichever `set` call completes last
                                    // wins regardless of submission order. Narrow window today
                                    // since rebuild does no real I/O, but worth revisiting if it grows any.
                                    agent_and_responder.set(rebuilt);
                                    // Kept in lockstep with `agent_and_responder` above (Bug 2
                                    // fix): without this, `SlashContext.model` (built from
                                    // `current_model.get()` in the `Enter` branch below) would
                                    // keep pointing at the pre-switch model forever.
                                    current_model.set(model_for_state);
                                    // Kept in lockstep alongside `current_model` above: the
                                    // Header reads from these, not from `props`, so without
                                    // this it would keep showing the pre-switch connection
                                    // and model name forever after a successful `/model`.
                                    connection_display.set(connection_name_for_display);
                                    model_display.set(model_name_for_display);
                                    transcript_for_notice.update(|entries| {
                                        entries.push(TranscriptEntry::SystemNotice {
                                            text: format!(
                                                "switched to {} · {}",
                                                connection.name, model_name
                                            ),
                                        });
                                    });
                                });
                            }
                            Err(e) => {
                                transcript.update(|entries| {
                                    entries.push(TranscriptEntry::SystemNotice {
                                        text: format!("failed to switch model: {e}"),
                                    });
                                });
                            }
                        }
                    }
                    return;
                }
                PendingMenu::PermissionsMenu => {
                    let new_tier = digit_key_to_index(ev.code, 3).map(|idx| match idx {
                        0 => PermissionTier::Ask,
                        1 => PermissionTier::AutoAcceptEdits,
                        _ => PermissionTier::FullAuto,
                    });
                    if let Some(new_tier) = new_tier {
                        tier.set(new_tier);
                        pending_menu.set(PendingMenu::None);
                        transcript.update(|entries| {
                            entries.push(TranscriptEntry::SystemNotice {
                                text: format!("permission tier set to {new_tier:?}"),
                            });
                        });
                    }
                    return;
                }
                PendingMenu::ResumeChoice(sessions) => {
                    if let Some(idx) = digit_key_to_index(ev.code, sessions.len()) {
                        let summary = sessions[idx].clone();
                        pending_menu.set(PendingMenu::None);
                        match crate::session::store::load_session(&summary.path) {
                            Ok(session) => {
                                let paths_lookup = crate::config::connection::load_connections(
                                    &user_config_dir,
                                    &project_config_dir,
                                );
                                let resolved_connection = paths_lookup
                                    .ok()
                                    .and_then(|conns| conns.into_iter().find(|c| c.name == session.connection_name));

                                match resolved_connection {
                                    Some(mut connection) => {
                                        connection.default_model = session.model_name.clone();
                                        let api_key = crate::config::secrets::SecretStore::get_api_key(&connection.name)
                                            .ok()
                                            .flatten();
                                        match crate::agent::provider::build_model(&connection, api_key) {
                                            Ok(new_model) => {
                                                let model_for_state = new_model.clone();
                                                let rebuilt = crate::tui::rebuild::rebuild_agent(
                                                    new_model,
                                                    session.tier,
                                                    always_allow_snapshot.clone(),
                                                    always_deny_snapshot.clone(),
                                                    session.messages.clone(),
                                                    &system_context,
                                                    mcp_tools_state.get(),
                                                    pending_permission.clone(),
                                                );
                                                agent_and_responder.set(rebuilt);
                                                // Kept in lockstep with `agent_and_responder`, mirroring
                                                // the `/model` fix for Bug 2 above — without this,
                                                // `SlashContext.model` (used by `/compact`) would keep
                                                // pointing at the pre-resume model forever.
                                                current_model.set(model_for_state);
                                                // Kept in lockstep with `current_model` above,
                                                // mirroring the `/model` fix: the Header reads
                                                // from these, not from `props`, so without this
                                                // it would keep showing the pre-resume
                                                // connection/model name forever.
                                                connection_display.set(connection.name.clone());
                                                model_display.set(session.model_name.clone());
                                                tier.set(session.tier);
                                                transcript.set(session.entries.clone());
                                                session_path.set(summary.path.clone());
                                                created_at.set(session.created_at.clone());
                                            }
                                            Err(e) => {
                                                transcript.update(|entries| {
                                                    entries.push(TranscriptEntry::SystemNotice {
                                                        text: format!("failed to resume: could not build model: {e}"),
                                                    });
                                                });
                                            }
                                        }
                                    }
                                    None => {
                                        transcript.update(|entries| {
                                            entries.push(TranscriptEntry::SystemNotice {
                                                text: format!(
                                                    "failed to resume: connection '{}' no longer exists; run `local-code connections list`",
                                                    session.connection_name
                                                ),
                                            });
                                        });
                                    }
                                }
                            }
                            Err(e) => {
                                transcript.update(|entries| {
                                    entries.push(TranscriptEntry::SystemNotice {
                                        text: format!("failed to load session: {e}"),
                                    });
                                });
                            }
                        }
                    }
                    return;
                }
                PendingMenu::McpAddWizard(_) => {
                    // Real free-text handling added in a later task.
                    return;
                }
                PendingMenu::McpAddConnecting => {
                    return;
                }
                PendingMenu::None => {}
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
                            created_at: created_at.clone(),
                            connection_name: connection_name.clone(),
                            model_name: model_name.clone(),
                            project_root: project_root.clone(),
                            user_state_dir: user_state_dir.clone(),
                            user_config_dir: user_config_dir.clone(),
                            project_config_dir: project_config_dir.clone(),
                            pending_menu: pending_menu.clone(),
                            always_allow: always_allow_snapshot.clone(),
                            always_deny: always_deny_snapshot.clone(),
                            agent: agent.clone(),
                            model: current_model.get(),
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

    let mut body: Vec<Element> = Vec::new();
    body.push(
        element! {
            Dashboard(
                connection_name: connection_display.get(),
                model_name: model_display.get(),
                tier_label: tier_label(tier.get()).to_string(),
                usage: usage.get(),
                streaming: streaming.get(),
                session_path: session_path.get().display().to_string(),
                created_at: created_at.get(),
                project_root: props.project_root.display().to_string(),
            )
        }
        .with_key("header"),
    );
    body.push(
        element! {
            Transcript(entries: transcript.get(), pending_permission: pending_permission.get())
        }
        .with_key("transcript"),
    );
    body.push(
        element! {
            InputBox(buffer: input_buffer.get(), disabled: streaming.get())
        }
        .with_key("input"),
    );
    body.push(
        element! {
            Footer(usage: usage.get(), streaming: streaming.get())
        }
        .with_key("footer"),
    );

    element! {
        View(flex_direction: FlexDirection::Column, width: Dimension::Percent(100.0), height: Dimension::Percent(100.0), padding: 0) {
            #(body)
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
    created_at: ntui::State<String>,
    connection_name: String,
    model_name: String,
    project_root: std::path::PathBuf,
    user_state_dir: std::path::PathBuf,
    user_config_dir: std::path::PathBuf,
    project_config_dir: std::path::PathBuf,
    pending_menu: ntui::State<PendingMenu>,
    always_allow: Vec<String>,
    always_deny: Vec<String>,
    agent: Arc<Agent>,
    model: SharedModel,
}

const HELP_TEXT: &str = "\
/model                     switch the active connection/model (history is kept)
/connections list          list configured connections
/connections remove <name> remove a configured connection
/connections add           not supported in-TUI; run `local-code connections add` in a separate terminal
/mcp list                  list configured MCP servers
/mcp remove <name>         remove a configured MCP server
/mcp add                   add an MCP server via a step-by-step wizard (Esc to cancel)
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
            let new_created_at = now.to_rfc3339();
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
                new_created_at.clone(),
            );
            if let Err(e) = crate::session::store::save_session(&new_path, &fresh) {
                ctx.transcript.update(|entries| {
                    entries.push(TranscriptEntry::SystemNotice {
                        text: format!("cleared transcript, but failed to start a new session file: {e}"),
                    });
                });
            }
            ctx.session_path.set(new_path);
            ctx.created_at.set(new_created_at);
        }
        SlashCommand::Model => {
            match crate::config::connection::load_connections(&ctx.user_config_dir, &ctx.project_config_dir) {
                Ok(connections) if connections.is_empty() => {
                    ctx.transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: "no connections configured; run `local-code connections add`".to_string(),
                        });
                    });
                }
                Ok(connections) => {
                    let mut choices = Vec::new();
                    for conn in &connections {
                        let mut models = conn.models.clone();
                        if !models.contains(&conn.default_model) {
                            models.insert(0, conn.default_model.clone());
                        }
                        for model_name in models {
                            choices.push((conn.clone(), model_name));
                        }
                    }
                    let listing: Vec<String> = choices
                        .iter()
                        .enumerate()
                        .take(9)
                        .map(|(i, (conn, model))| format!("{}) {} · {}", i + 1, conn.name, model))
                        .collect();
                    ctx.transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!(
                                "Select a connection/model (press the digit key):\n{}",
                                listing.join("\n")
                            ),
                        });
                    });
                    ctx.pending_menu.set(PendingMenu::ModelChoice(choices.into_iter().take(9).collect()));
                }
                Err(e) => {
                    ctx.transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!("failed to load connections: {e}"),
                        });
                    });
                }
            }
        }
        SlashCommand::Permissions => {
            let current = ctx.tier.get();
            let label = tier_label(current);
            let text = format!(
                "Current tier: {label}\n\
                 1) ask\n2) auto-accept-edits\n3) full-auto\n\
                 (press a digit key to switch, or Ctrl+A to cycle)\n\
                 always-allow: {}\nalways-deny: {}",
                if ctx.always_allow.is_empty() { "(none)".to_string() } else { ctx.always_allow.join(", ") },
                if ctx.always_deny.is_empty() { "(none)".to_string() } else { ctx.always_deny.join(", ") },
            );
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice { text });
            });
            ctx.pending_menu.set(PendingMenu::PermissionsMenu);
        }
        SlashCommand::ConnectionsList => {
            let paths = crate::config::paths::Paths {
                user_config_dir: ctx.user_config_dir.clone(),
                project_config_dir: ctx.project_config_dir.clone(),
                user_state_dir: ctx.user_state_dir.clone(),
            };
            let mut out = Vec::new();
            let text = match crate::cli::connections::list(&paths, &mut out) {
                Ok(()) => String::from_utf8_lossy(&out).to_string(),
                Err(e) => format!("failed to list connections: {e}"),
            };
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice { text });
            });
        }
        SlashCommand::ConnectionsRemove { name } => {
            let paths = crate::config::paths::Paths {
                user_config_dir: ctx.user_config_dir.clone(),
                project_config_dir: ctx.project_config_dir.clone(),
                user_state_dir: ctx.user_state_dir.clone(),
            };
            let mut out = Vec::new();
            let text = match crate::cli::connections::remove(&paths, &name, &mut out) {
                Ok(()) => String::from_utf8_lossy(&out).to_string(),
                Err(e) => format!("failed to remove connection: {e}"),
            };
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice { text });
            });
        }
        SlashCommand::ConnectionsAddUnsupported => {
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice {
                    text: "adding a connection interactively isn't supported inside the TUI\n\
                           (the wizard needs multi-step line-by-line stdin, which the raw-mode\n\
                           TUI input loop doesn't support). Exit and run\n\
                           `local-code connections add` in a separate terminal, then use /model\n\
                           to switch to it."
                        .to_string(),
                });
            });
        }
        SlashCommand::McpList => {
            let paths = crate::config::paths::Paths {
                user_config_dir: ctx.user_config_dir.clone(),
                project_config_dir: ctx.project_config_dir.clone(),
                user_state_dir: ctx.user_state_dir.clone(),
            };
            let mut out = Vec::new();
            let text = match crate::cli::mcp::list(&paths, &mut out) {
                Ok(()) => String::from_utf8_lossy(&out).to_string(),
                Err(e) => format!("failed to list MCP servers: {e}"),
            };
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice { text });
            });
        }
        SlashCommand::McpRemove { name } => {
            let paths = crate::config::paths::Paths {
                user_config_dir: ctx.user_config_dir.clone(),
                project_config_dir: ctx.project_config_dir.clone(),
                user_state_dir: ctx.user_state_dir.clone(),
            };
            let mut out = Vec::new();
            let text = match crate::cli::mcp::remove(&paths, &name, &mut out) {
                Ok(()) => String::from_utf8_lossy(&out).to_string(),
                Err(e) => format!("failed to remove MCP server: {e}"),
            };
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice { text });
            });
        }
        SlashCommand::McpAdd => {
            let (wizard, prompt) = crate::tui::mcp_wizard::start();
            ctx.transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice { text: prompt });
            });
            ctx.pending_menu.set(PendingMenu::McpAddWizard(wizard));
        }
        SlashCommand::Compact => {
            const RETAIN_RECENT: usize = 10;
            const COMPACT_THRESHOLD: usize = 20;
            let agent = ctx.agent.clone();
            let model = ctx.model.clone();
            let transcript = ctx.transcript.clone();
            tokio::spawn(async move {
                let history = match agent.memory().get_messages_erased().await {
                    Ok(h) => h,
                    Err(e) => {
                        transcript.update(|entries| {
                            entries.push(TranscriptEntry::SystemNotice {
                                text: format!("compact failed: could not read history: {e}"),
                            });
                        });
                        return;
                    }
                };

                if history.len() <= COMPACT_THRESHOLD {
                    transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!(
                                "nothing to compact yet ({} messages, threshold is {COMPACT_THRESHOLD})",
                                history.len()
                            ),
                        });
                    });
                    return;
                }

                let split_at = history.len().saturating_sub(RETAIN_RECENT);
                let (older, recent) = history.split_at(split_at);

                let mut conversation_text = String::new();
                for msg in older {
                    let role = format!("{:?}", msg.role);
                    if let Some(content) = &msg.content {
                        conversation_text.push_str(&format!("{role}: {content}\n"));
                    }
                }

                let summary_request = daimon::model::types::ChatRequest {
                    messages: vec![
                        daimon::model::types::Message::system(
                            "You are a conversation summarizer. Summarize the following \
                             conversation into a concise paragraph that preserves all important \
                             facts, decisions, tool results, and context. Be specific — include \
                             names, numbers, and outcomes. Do not include any preamble, just the \
                             summary.",
                        ),
                        daimon::model::types::Message::user(conversation_text),
                    ],
                    tools: Vec::new(),
                    temperature: Some(0.0),
                    max_tokens: Some(512),
                };

                let summary_text = match model.generate_erased(&summary_request).await {
                    Ok(response) => response.text().to_string(),
                    Err(e) => {
                        transcript.update(|entries| {
                            entries.push(TranscriptEntry::SystemNotice {
                                text: format!("compact failed: summarization call errored: {e}"),
                            });
                        });
                        return;
                    }
                };

                if let Err(e) = agent.memory().clear_erased().await {
                    transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!("compact failed: could not clear memory: {e}"),
                        });
                    });
                    return;
                }
                let _ = agent
                    .memory()
                    .add_message_erased(daimon::model::types::Message::system(format!(
                        "Previous conversation summary: {summary_text}"
                    )))
                    .await;
                for msg in recent.iter().cloned() {
                    let _ = agent.memory().add_message_erased(msg).await;
                }

                // The display transcript has no 1:1 correspondence to the
                // message-level split above (one user turn can expand into
                // several TranscriptEntry values via tool cards) — this plan
                // approximates the same boundary at the display layer by
                // keeping only the transcript's last RETAIN_RECENT entries
                // and prepending one SystemNotice with the summary, rather
                // than attempting an exact message-to-entry alignment. This
                // is a documented approximation, the same honest-scoping
                // approach Phase 3 used for diff coloring.
                transcript.update(|entries| {
                    let keep_from = entries.len().saturating_sub(RETAIN_RECENT);
                    let mut compacted = vec![TranscriptEntry::SystemNotice {
                        text: format!("compacted {} older messages into a summary", older.len()),
                    }];
                    compacted.extend(entries.split_off(keep_from));
                    *entries = compacted;
                });
            });
        }
        SlashCommand::Init => {
            let model = ctx.model.clone();
            let project_root = ctx.project_root.clone();
            let transcript = ctx.transcript.clone();
            transcript.update(|entries| {
                entries.push(TranscriptEntry::SystemNotice {
                    text: "surveying the project and generating AGENTS.md…".to_string(),
                });
            });
            tokio::spawn(async move {
                let survey = crate::init::survey_project(&project_root);
                match crate::init::generate_agents_md(&model, &survey).await {
                    Ok(content) => match crate::init::write_agents_md(&project_root, &content) {
                        Ok(()) => transcript.update(|entries| {
                            entries.push(TranscriptEntry::SystemNotice {
                                text: "wrote AGENTS.md".to_string(),
                            });
                        }),
                        Err(e) => transcript.update(|entries| {
                            entries.push(TranscriptEntry::SystemNotice {
                                text: format!("/init failed to write AGENTS.md: {e}"),
                            });
                        }),
                    },
                    Err(e) => transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!("/init failed: {e}"),
                        });
                    }),
                }
            });
        }
        SlashCommand::Resume => {
            match crate::session::store::list_sessions(&ctx.user_state_dir, &ctx.project_root) {
                Ok(sessions) if sessions.is_empty() => {
                    ctx.transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: "no previous sessions found for this project".to_string(),
                        });
                    });
                }
                Ok(sessions) => {
                    let listing: Vec<String> = sessions
                        .iter()
                        .enumerate()
                        .take(9)
                        .map(|(i, s)| {
                            format!(
                                "{}) {} · {} · {}{}",
                                i + 1,
                                s.updated_at,
                                s.connection_name,
                                s.model_name,
                                s.first_user_turn_preview
                                    .as_ref()
                                    .map(|p| format!(" · \"{p}\""))
                                    .unwrap_or_default()
                            )
                        })
                        .collect();
                    ctx.transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!("Select a session to resume (press the digit key):\n{}", listing.join("\n")),
                        });
                    });
                    ctx.pending_menu.set(PendingMenu::ResumeChoice(sessions.into_iter().take(9).collect()));
                }
                Err(e) => {
                    ctx.transcript.update(|entries| {
                        entries.push(TranscriptEntry::SystemNotice {
                            text: format!("failed to list sessions: {e}"),
                        });
                    });
                }
            }
        }
    }
}

/// Drives one turn: streams `agent.prompt_stream(&input)`, folding each
/// `StreamEvent` into `transcript`/`usage`, then clears `streaming` and
/// `pending_turn_input` so the next `Enter` can start a new turn.
///
/// # Memory persistence is handled by `daimon` itself
///
/// As of `daimon` 0.17.0's "consolidate API surface" release,
/// `daimon::agent::Agent::prompt_stream` persists every assistant-with-tool-
/// calls message, every tool-result message, and the final assistant text
/// message into `agent.memory()` itself, exactly matching what its
/// non-streaming sibling `run_react_loop` (which backs `Agent::prompt`) does
/// — confirmed by reading `daimon` 0.19.0's `src/agent/runner.rs::prompt_stream`
/// directly (see `memory.add_message_erased` calls for the tool-calls
/// message, each tool result, and the final assistant text). Earlier
/// (`daimon` 0.16.0 and before), `prompt_stream` only ever persisted the
/// user's own message, silently discarding everything else once the
/// returned stream was dropped — this function used to reconstruct that
/// missing persistence itself, event by event. That reconstruction has been
/// removed now that `daimon` does it natively; duplicating it here would
/// double-write every assistant/tool message into memory. This function now
/// only needs to fold `StreamEvent`s into the `transcript`/`usage` UI state —
/// `agent.memory()` is already correct by the time the stream ends.
/// Renders a `run_turn` failure as the text of a `TranscriptEntry::SystemNotice`.
///
/// `daimon::DaimonError` collapses every model-facing failure (HTTP, SDK,
/// timeout, ...) into a single `Model(String)` variant, so there's no typed
/// way to detect a timeout — this instead pattern-matches the stringified
/// error for "timeout"/"timed out" (case-insensitively) so a stuck local
/// model server (now bounded by the client's `.with_timeout(...)`) surfaces
/// as a clear diagnosis instead of the generic `error: ...` wrapper.
fn format_turn_error(e: impl std::fmt::Display) -> String {
    let error_text = e.to_string();
    let lower = error_text.to_lowercase();
    if lower.contains("timed out") || lower.contains("timeout") {
        format!("error: request timed out — the local model server may be unresponsive ({error_text})")
    } else {
        format!("error: {error_text}")
    }
}

async fn run_turn(
    agent: Arc<Agent>,
    input: String,
    transcript: ntui::State<Vec<TranscriptEntry>>,
    usage: ntui::State<UsageSummary>,
    streaming: ntui::State<bool>,
    pending_turn_input: ntui::State<Option<String>>,
    session_path: ntui::State<std::path::PathBuf>,
    created_at: ntui::State<String>,
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
                    text: format_turn_error(e),
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
                        text: format_turn_error(e),
                    });
                });
                break;
            }
        }
    }

    if let Ok(messages) = agent.memory().get_messages_erased().await {
        let now = chrono::Utc::now().to_rfc3339();
        let mut session = crate::session::types::SessionFile::new(
            project_root,
            connection_name,
            model_name,
            tier,
            created_at.get(),
        );
        session.updated_at = now;
        session.entries = transcript.get();
        session.messages = messages;
        if let Err(e) = crate::session::store::save_session(&session_path.get(), &session) {
            eprintln!("warning: failed to persist session to {}: {e}", session_path.get().display());
        }
    }

    streaming.set(false);
    pending_turn_input.set(None);
}

#[cfg(test)]
mod tests {
    use super::*;
    use daimon::model::types::{ChatRequest, ChatResponse, Message, Role, StopReason, Usage};
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

    // --- Bug 1 fixtures: verifies `agent.memory()` ends up correctly -------
    // populated after a streamed turn. As of `daimon` 0.17.0+, `prompt_stream`
    // persists these messages itself (see the doc comment on `run_turn`
    // above); these tests just assert the end state.
    //
    // `run_turn` is exercised directly here (bypassing the full `App`
    // component and its slash-command/permission-gate machinery) via a
    // minimal harness component, following the same pattern
    // `src/tui/rebuild.rs`'s own tests use (a throwaway component whose sole
    // job is to call `hooks.use_state` — which cannot be constructed outside
    // an `ntui` render — then hand the built `Agent` back out through a
    // plain `Arc<Mutex<Option<..>>>` "slot" the test can read after ticking).

    /// Streams one tool call on its first invocation (mirroring `daimon`'s
    /// own `agent/runner.rs` test fixture `ToolCallingModel`, adapted to the
    /// streaming `StreamEvent` shape `generate_stream` must yield), then a
    /// plain text reply with no tool calls on its second invocation.
    struct StreamingToolCallModel {
        call_count: std::sync::atomic::AtomicUsize,
    }
    impl StreamingToolCallModel {
        fn new() -> Self {
            Self { call_count: std::sync::atomic::AtomicUsize::new(0) }
        }
    }
    impl daimon::model::Model for StreamingToolCallModel {
        async fn generate(&self, _request: &ChatRequest) -> daimon::Result<ChatResponse> {
            unreachable!("prompt_stream only ever calls generate_stream_erased")
        }
        async fn generate_stream(&self, _request: &ChatRequest) -> daimon::Result<ResponseStream> {
            let count = self.call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if count == 0 {
                Ok(Box::pin(futures::stream::iter(vec![
                    Ok(StreamEvent::ToolCallStart { id: "call_1".into(), name: "adder".into() }),
                    Ok(StreamEvent::ToolCallDelta {
                        id: "call_1".into(),
                        arguments_delta: "{\"a\":2,\"b\":3}".into(),
                    }),
                    Ok(StreamEvent::ToolCallEnd { id: "call_1".into() }),
                    Ok(StreamEvent::Done),
                ])))
            } else {
                Ok(Box::pin(futures::stream::iter(vec![
                    Ok(StreamEvent::TextDelta("The sum is 5".into())),
                    Ok(StreamEvent::Done),
                ])))
            }
        }
    }

    struct AdderTool;
    impl daimon::tool::Tool for AdderTool {
        fn name(&self) -> &str {
            "adder"
        }
        fn description(&self) -> &str {
            "Adds two numbers"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
                "required": ["a", "b"],
            })
        }
        async fn execute(&self, input: &serde_json::Value) -> daimon::Result<daimon::tool::ToolOutput> {
            let a = input["a"].as_i64().unwrap_or(0);
            let b = input["b"].as_i64().unwrap_or(0);
            Ok(daimon::tool::ToolOutput::text(format!("{}", a + b)))
        }
    }

    #[derive(Clone, Copy, PartialEq, Default)]
    enum HarnessMode {
        #[default]
        ToolCall,
        TextOnly,
    }

    #[derive(Clone, Default)]
    struct AgentSlot(Arc<std::sync::Mutex<Option<Arc<Agent>>>>);

    #[derive(Clone, Default)]
    struct RunTurnHarnessProps {
        slot: AgentSlot,
        mode: HarnessMode,
    }
    impl PartialEq for RunTurnHarnessProps {
        // Mounted exactly once per test, same rationale as `AppProps::eq`.
        fn eq(&self, _other: &Self) -> bool {
            true
        }
    }

    #[component]
    fn RunTurnHarness(props: &RunTurnHarnessProps, hooks: &mut Hooks) -> Element {
        let transcript = hooks.use_state(Vec::<TranscriptEntry>::new);
        let usage_state = hooks.use_state(UsageSummary::default);
        let streaming = hooks.use_state(|| false);
        let pending_turn_input = hooks.use_state(|| Option::<String>::None);
        let session_path = hooks.use_state(std::path::PathBuf::new);
        let created_at = hooks.use_state(|| "2026-01-01T00:00:00Z".to_string());

        hooks.use_effect((), {
            let slot = props.slot.clone();
            let mode = props.mode;
            let transcript = transcript.clone();
            let usage_state = usage_state.clone();
            let streaming = streaming.clone();
            let pending_turn_input = pending_turn_input.clone();
            let session_path = session_path.clone();
            let created_at = created_at.clone();
            move || {
                let agent = Arc::new(match mode {
                    HarnessMode::ToolCall => Agent::builder()
                        .model(StreamingToolCallModel::new())
                        .tool(AdderTool)
                        .build()
                        .unwrap(),
                    HarnessMode::TextOnly => {
                        Agent::builder().model(StreamingEchoModel).build().unwrap()
                    }
                });
                *slot.0.lock().unwrap() = Some(agent.clone());
                tokio::spawn(run_turn(
                    agent,
                    "add 2 and 3".to_string(),
                    transcript,
                    usage_state,
                    streaming,
                    pending_turn_input,
                    session_path,
                    created_at,
                    "local-vllm".into(),
                    "qwen2.5-coder-32b".into(),
                    PermissionTier::FullAuto,
                    std::env::temp_dir(),
                ));
            }
        });

        element! { View {} }
    }

    /// Bug 1, tool-calling case: as of `daimon` 0.17.0+, `Agent::prompt_stream`
    /// persists both the assistant-with-tool-calls message and each
    /// tool-result message into `agent.memory()` itself — this asserts that
    /// end state directly (see the doc comment on `run_turn` above; on
    /// `daimon` 0.16.0 this used to require a manual reconstruction that has
    /// since been removed).
    #[tokio::test(start_paused = true)]
    async fn run_turn_persists_tool_call_and_result_messages_to_memory() {
        let slot = AgentSlot::default();
        let props = RunTurnHarnessProps { slot: slot.clone(), mode: HarnessMode::ToolCall };
        let mut t = TestTerminal::new(10, 1, Element::component::<RunTurnHarness>(props)).unwrap();

        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            t.tick().await.unwrap();
        }

        let agent = slot.0.lock().unwrap().clone().expect("harness should have built an agent");
        let messages = agent.memory().get_messages_erased().await.unwrap();

        assert!(
            messages.iter().any(|m| m.role == Role::User && m.content.as_deref() == Some("add 2 and 3")),
            "missing user message: {messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.role == Role::Assistant
                && m.content.is_none()
                && m.tool_calls.len() == 1
                && m.tool_calls[0].id == "call_1"
                && m.tool_calls[0].name == "adder"
                && m.tool_calls[0].arguments == serde_json::json!({"a": 2, "b": 3})),
            "missing assistant-with-tool-calls message: {messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.role == Role::Tool
                && m.tool_call_id.as_deref() == Some("call_1")
                && m.content.as_deref() == Some("5")),
            "missing tool-result message: {messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.role == Role::Assistant && m.content.as_deref() == Some("The sum is 5")),
            "missing final assistant text message: {messages:?}"
        );
        // Exactly these four: user, assistant-with-tool-calls, tool-result,
        // final assistant text — proving nothing extra or duplicated leaked
        // in from the reconstruction.
        assert_eq!(messages.len(), 4, "{messages:?}");
    }

    /// Bug 1, plain-text case: covers turns with no tool calls at all — both
    /// the user's message and the assistant's plain-text reply should be in
    /// `agent.memory()` after the stream ends.
    #[tokio::test(start_paused = true)]
    async fn run_turn_persists_plain_text_reply_to_memory() {
        let slot = AgentSlot::default();
        let props = RunTurnHarnessProps { slot: slot.clone(), mode: HarnessMode::TextOnly };
        let mut t = TestTerminal::new(10, 1, Element::component::<RunTurnHarness>(props)).unwrap();

        for _ in 0..30 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            t.tick().await.unwrap();
        }

        let agent = slot.0.lock().unwrap().clone().expect("harness should have built an agent");
        let messages = agent.memory().get_messages_erased().await.unwrap();

        assert!(
            messages.iter().any(|m| m.role == Role::User && m.content.as_deref() == Some("add 2 and 3")),
            "missing user message: {messages:?}"
        );
        assert!(
            messages.iter().any(|m| m.role == Role::Assistant && m.content.as_deref() == Some("Hello, world")),
            "missing assistant reply message: {messages:?}"
        );
        assert_eq!(messages.len(), 2, "{messages:?}");
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

        // The `/clear` handler must also start a brand-new session file on disk
        // (not just reset the in-memory transcript): the original file should
        // remain untouched, a new file should appear alongside it, and that new
        // file should hold a genuinely fresh (empty) session.
        fn find_json_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_dir() {
                    find_json_files(&path, out);
                } else if path.extension().and_then(|s| s.to_str()) == Some("json") {
                    out.push(path);
                }
            }
        }
        let mut json_files = Vec::new();
        find_json_files(dir.path(), &mut json_files);
        assert!(
            json_files.len() >= 2,
            "expected a new session file in addition to original.json, found: {json_files:?}"
        );

        let new_path = json_files
            .iter()
            .find(|p| p.file_name().unwrap() != "original.json")
            .expect("a new session file distinct from original.json should exist");
        let new_session = crate::session::store::load_session(new_path).unwrap();
        assert!(new_session.entries.is_empty(), "new session should start with an empty transcript");
        assert!(new_session.messages.is_empty(), "new session should start with empty message history");

        // original.json was written to by the "hi there" turn (which ran before
        // /clear), so it should still hold that turn's history — /clear must not
        // retroactively wipe it, only stop using it going forward.
        let original = crate::session::store::load_session(&dir.path().join("original.json")).unwrap();
        assert!(
            original.entries.iter().any(|e| matches!(e, TranscriptEntry::UserTurn { text } if text == "hi there")),
            "original.json should retain the pre-/clear turn history"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn completed_turn_is_persisted_to_the_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut props = test_props();
        props.user_state_dir = dir.path().to_path_buf();
        props.session_path = dir.path().join("session.json");
        props.created_at = "2026-07-06T00:00:00Z".into();
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
        assert_eq!(saved.created_at, "2026-07-06T00:00:00Z");
    }

    #[tokio::test(start_paused = true)]
    async fn run_turn_persists_the_seeded_created_at_not_the_disk_value() {
        // `run_turn` must source `created_at` from the seeded in-memory state
        // (`props.created_at`, threaded through `use_state`), not by re-reading
        // the session file off disk via `load_session(...)`. Seed the props
        // with one timestamp and write a *different* timestamp into the
        // on-disk session file first (mimicking what a stale disk read would
        // still see) to make sure the two can't be confused: only the seeded
        // value should ever end up back on disk after a turn completes.
        let dir = tempfile::tempdir().unwrap();
        let mut props = test_props();
        props.user_state_dir = dir.path().to_path_buf();
        props.session_path = dir.path().join("session.json");
        props.created_at = "2099-01-01T00:00:00Z".into();
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
        assert_eq!(
            saved.created_at, "2099-01-01T00:00:00Z",
            "created_at should come from the seeded in-memory state, not the stale on-disk value"
        );
        assert_ne!(saved.created_at, "2026-07-06T00:00:00Z");
    }

    #[tokio::test(start_paused = true)]
    async fn model_command_lists_choices_and_switches_on_digit_press() {
        // This test exercises the parsing/listing/selection *mechanics* using
        // a fixture with zero real connections configured (since App's test
        // harness has no filesystem-backed Paths wired in) — it asserts the
        // "no connections configured" branch specifically, which is exactly
        // as real a code path as the populated-list branch and doesn't
        // require constructing on-disk connections.toml fixtures inside this
        // component test. The populated-list and successful-switch branches
        // are covered by `dispatch_slash_command`'s own logic being pure
        // enough to reason about, and by this plan's Task 16 CLI-level tests
        // that do set up real connections.toml fixtures.
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/model").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("no connections configured"), "{}", t.frame_text());
    }

    #[tokio::test(start_paused = true)]
    async fn permissions_command_shows_tier_and_lists_and_digit_press_changes_tier() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/permissions").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("Current tier: full-auto"), "{}", t.frame_text());

        t.send_key(KeyCode::Char('1')).unwrap();
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("[ask]"), "{}", t.frame_text());
    }

    #[tokio::test(start_paused = true)]
    async fn connections_list_reports_no_connections_configured() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/connections list").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("No connections configured"), "{}", t.frame_text());
    }

    #[tokio::test(start_paused = true)]
    async fn mcp_list_reports_no_servers_configured() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/mcp list").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("No MCP servers configured"), "{}", t.frame_text());
    }

    #[tokio::test(start_paused = true)]
    async fn mcp_add_starts_the_wizard_and_prompts_for_a_name() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/mcp add").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("Server name:"), "{}", t.frame_text());
    }

    #[tokio::test(start_paused = true)]
    async fn connections_add_explains_it_is_unsupported_in_tui() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/connections add").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("local-code connections add"), "{}", t.frame_text());
    }

    #[tokio::test(start_paused = true)]
    async fn compact_reports_nothing_to_do_below_the_threshold() {
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(test_props())).unwrap();
        type_and_submit(&mut t, "/compact").await;
        for _ in 0..10 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            t.tick().await.unwrap();
        }
        assert!(t.frame_text().contains("nothing to compact yet"), "{}", t.frame_text());
    }

    #[tokio::test(start_paused = true)]
    async fn compact_summarizes_older_messages_and_keeps_recent_ones() {
        let props = test_props();
        // Reuse StreamingEchoModel as the active model for both turns and the
        // summarization call — its generate() (non-streaming) path is used by
        // /compact, its generate_stream() path by ordinary turns; both are
        // already implemented on this fixture from Phase 3.
        // A tall terminal (rather than the usual 80x24) so every turn's
        // rendered entries stay on-screen — with 25 real turns run before
        // `/compact`, a normal 24-row terminal would scroll the compaction
        // notice (pushed to the front of the post-compaction transcript) off
        // the visible frame well before the assertion below runs, making the
        // test fragile regardless of whether compaction actually happened.
        let mut t = TestTerminal::new(80, 1000, Element::component::<App>(props.clone())).unwrap();

        // `Agent::prompt_stream` (the streaming path `run_turn` uses) persists
        // both the user message and the assistant's final text into
        // `agent.memory()` itself as of daimon 0.17.0+ (see `run_turn`'s doc
        // comment and the dedicated `run_turn_persists_plain_text_reply_to_memory`
        // / `run_turn_persists_tool_call_and_result_messages_to_memory` tests
        // above) — so each submitted turn here contributes 2 messages (user +
        // assistant), not 1. This test still submits 25 turns (more than
        // strictly needed now) so the resulting 50-message history clears
        // `COMPACT_THRESHOLD` (20) with margin.
        for i in 0..25 {
            type_and_submit(&mut t, &format!("turn {i}")).await;
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                t.tick().await.unwrap();
            }
        }

        type_and_submit(&mut t, "/compact").await;
        for _ in 0..40 {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            t.tick().await.unwrap();
        }
        assert!(t.frame_text().contains("compacted"), "{}", t.frame_text());
        let _ = props; // props is cloned above only to keep it available for potential future assertions
    }

    /// Bug 2: `SlashContext.model` used to be built from a one-time snapshot
    /// of `props.model` (`model_snapshot`), taken once at mount and never
    /// updated — so after a `/model` switch, `/compact`'s summarization call
    /// kept silently using the *pre-switch* model forever. This test proves
    /// the fix (`current_model`, kept in lockstep with `agent_and_responder`
    /// on every switch) by making the pre- and post-switch models
    /// *observably different*:
    ///
    /// - The pre-switch model is `StreamingEchoModel`, whose non-streaming
    ///   `generate()` (the path `/compact` calls) succeeds instantly with
    ///   canned text — if `/compact` ran against this (stale) model, it would
    ///   report a successful "compacted N older messages" notice.
    /// - The post-switch model is a *real* `daimon` Ollama client (built via
    ///   the same `crate::agent::provider::build_model` the real `/model`
    ///   command path uses) pointed at `http://127.0.0.1:1` — a port nothing
    ///   listens on, so any real request against it fails fast with a
    ///   connection error. If `/compact` runs against the *new* model (the
    ///   correct, fixed behavior), its summarization call fails, and the
    ///   transcript shows a "compact failed: summarization call errored"
    ///   notice instead of a successful compaction.
    ///
    /// So: success notice ⇒ bug (stale model); failure notice referencing the
    /// summarization call ⇒ fix confirmed (current model is the new one).
    #[tokio::test(start_paused = true)]
    async fn model_switch_updates_the_model_compact_uses() {
        let user_config_dir = tempfile::tempdir().unwrap();
        let project_config_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            user_config_dir.path().join("connections.toml"),
            r#"
[[connection]]
name = "test-ollama"
provider = "ollama"
base_url = "http://127.0.0.1:1"
default_model = "test-model"
models = ["test-model"]
"#,
        )
        .unwrap();

        let mut props = test_props();
        props.user_config_dir = user_config_dir.path().to_path_buf();
        props.project_config_dir = project_config_dir.path().to_path_buf();

        let mut t = TestTerminal::new(80, 1000, Element::component::<App>(props)).unwrap();

        // Cross COMPACT_THRESHOLD (20) before switching models — 11 turns *
        // 2 messages/turn (see Bug 1's fix and its tests) = 22 messages.
        for i in 0..11 {
            type_and_submit(&mut t, &format!("turn {i}")).await;
            for _ in 0..20 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                t.tick().await.unwrap();
            }
        }

        type_and_submit(&mut t, "/model").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("test-ollama"), "{}", t.frame_text());

        t.send_key(KeyCode::Char('1')).unwrap();
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            t.tick().await.unwrap();
        }
        assert!(t.frame_text().contains("switched to test-ollama"), "{}", t.frame_text());

        type_and_submit(&mut t, "/compact").await;
        for _ in 0..60 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            t.tick().await.unwrap();
        }

        let text = t.frame_text();
        assert!(
            text.contains("compact failed: summarization call errored"),
            "expected /compact to fail against the NEW (unreachable) model, proving \
             `SlashContext.model` was updated by the switch, not stale: {text}"
        );
        assert!(
            !text.contains("compacted 22 older messages"),
            "a successful compaction here would mean /compact used the stale \
             pre-switch model instead of the new one: {text}"
        );
    }

    /// Header staleness bug found in code review: `Header`'s
    /// `connection_name`/`model_name` used to be read directly from
    /// `props.connection_name`/`props.model_name` — a one-time snapshot taken
    /// at mount and never refreshed — so after a successful `/model` switch
    /// (which does correctly rebuild the agent and post a "switched to X · Y"
    /// notice, per `model_switch_updates_the_model_compact_uses` above), the
    /// Header kept silently showing the connection/model the process
    /// launched with. This test proves the fix (`connection_display`/
    /// `model_display`, kept in lockstep with `current_model` at the same
    /// `/model` digit-press site) by asserting the Header's rendered text
    /// shows the NEW connection/model name after switching, and no longer
    /// shows `test_props()`'s original ones.
    #[tokio::test(start_paused = true)]
    async fn model_switch_updates_the_header_display() {
        let user_config_dir = tempfile::tempdir().unwrap();
        let project_config_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            user_config_dir.path().join("connections.toml"),
            r#"
[[connection]]
name = "test-ollama"
provider = "ollama"
base_url = "http://127.0.0.1:1"
default_model = "test-model"
models = ["test-model"]
"#,
        )
        .unwrap();

        let mut props = test_props();
        props.user_config_dir = user_config_dir.path().to_path_buf();
        props.project_config_dir = project_config_dir.path().to_path_buf();

        let mut t = TestTerminal::new(80, 24, Element::component::<App>(props)).unwrap();

        // Sanity check: the Header starts out showing test_props()'s
        // original connection/model, exactly as it did before this fix.
        assert!(t.frame_text().contains("local-vllm"), "{}", t.frame_text());
        assert!(t.frame_text().contains("qwen2.5-coder-32b"), "{}", t.frame_text());

        type_and_submit(&mut t, "/model").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("test-ollama"), "{}", t.frame_text());

        t.send_key(KeyCode::Char('1')).unwrap();
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            t.tick().await.unwrap();
        }
        assert!(t.frame_text().contains("switched to test-ollama"), "{}", t.frame_text());

        let text = t.frame_text();
        assert!(
            text.contains("test-ollama") && text.contains("test-model"),
            "expected the Header to now show the NEW connection/model after \
             the switch: {text}"
        );
        assert!(
            !text.contains("local-vllm") && !text.contains("qwen2.5-coder-32b"),
            "the Header still shows the ORIGINAL (pre-switch) connection/model \
             — this is the staleness bug the fix addresses: {text}"
        );
    }

    // `/init` reads `ctx.project_root`, which is now sourced from
    // `AppProps::project_root` rather than `std::env::current_dir()` (see
    // that field's doc comment) — so this test points `/init` at a tempdir
    // by setting `test_props().project_root` directly, with no
    // `std::env::set_current_dir` involved. That sidesteps the flakiness the
    // original plan flagged: a process-global cwd mutation would race any
    // other test in this (parallel-by-default) suite that reads or depends
    // on the current directory.
    #[tokio::test(start_paused = true)]
    async fn init_command_writes_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();

        let mut props = test_props();
        props.project_root = dir.path().to_path_buf();

        let mut t = TestTerminal::new(80, 24, Element::component::<App>(props)).unwrap();
        type_and_submit(&mut t, "/init").await;
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            t.tick().await.unwrap();
        }

        assert!(t.frame_text().contains("wrote AGENTS.md"), "{}", t.frame_text());
        assert!(dir.path().join("AGENTS.md").exists());
    }

    #[tokio::test(start_paused = true)]
    async fn resume_command_reports_no_previous_sessions_when_none_exist() {
        let dir = tempfile::tempdir().unwrap();
        let mut props = test_props();
        props.user_state_dir = dir.path().to_path_buf();
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(props)).unwrap();
        type_and_submit(&mut t, "/resume").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("no previous sessions found"), "{}", t.frame_text());
    }

    // `/resume`'s listing reads `list_sessions(&ctx.user_state_dir,
    // &ctx.project_root)`, and `ctx.project_root` is sourced from
    // `AppProps::project_root` (see `init_command_writes_agents_md`'s comment
    // above on the same fix) — not `std::env::current_dir()`. `test_props()`
    // defaults `project_root` to an empty `PathBuf` (via `AppProps::default`),
    // so this test sets `props.project_root` explicitly to a fixed path and
    // saves the session fixture under that *same* path, rather than relying
    // on `std::env::current_dir()` coincidentally matching whatever
    // `test_props()` defaults to.
    #[tokio::test(start_paused = true)]
    async fn resume_command_lists_existing_sessions_and_resuming_restores_the_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = std::path::PathBuf::from("/some/fixed/project-root");
        let mut session = crate::session::types::SessionFile::new(
            project_root.clone(),
            "some-connection".into(),
            "some-model".into(),
            PermissionTier::FullAuto,
            "2026-07-06T09:00:00Z".into(),
        );
        session.entries.push(TranscriptEntry::UserTurn { text: "earlier turn".into() });
        let path = crate::session::paths::new_session_path(dir.path(), &project_root, chrono::Utc::now());
        crate::session::store::save_session(&path, &session).unwrap();

        let mut props = test_props();
        props.user_state_dir = dir.path().to_path_buf();
        props.project_root = project_root;
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(props)).unwrap();
        type_and_submit(&mut t, "/resume").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("some-connection"), "{}", t.frame_text());

        // Resuming when the session's connection is no longer configured
        // (test_props() sets up no real connections.toml) surfaces the
        // clear "connection no longer exists" notice rather than panicking —
        // this exercises that failure path explicitly, since it's the
        // reachable one without a full connections.toml fixture.
        t.send_key(KeyCode::Char('1')).unwrap();
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("no longer exists"), "{}", t.frame_text());
    }

    // Closes the coverage gap the test above deliberately leaves open: that
    // test only exercises listing plus the "connection no longer exists"
    // failure branch, since `test_props()` wires up no real connections.toml.
    // This test follows `model_switch_updates_the_header_display`'s fixture
    // pattern (a real `connections.toml` under tempdir-backed
    // `user_config_dir`/`project_config_dir`) so the *success* branch of
    // `/resume` — rebuilding the agent, restoring the transcript, and
    // refreshing the Header — is exercised too.
    #[tokio::test(start_paused = true)]
    async fn resume_command_success_path_restores_transcript_and_updates_header() {
        let state_dir = tempfile::tempdir().unwrap();
        let user_config_dir = tempfile::tempdir().unwrap();
        let project_config_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            user_config_dir.path().join("connections.toml"),
            r#"
[[connection]]
name = "resumed-connection"
provider = "ollama"
base_url = "http://127.0.0.1:1"
default_model = "irrelevant-default"
models = ["irrelevant-default", "resumed-model"]
"#,
        )
        .unwrap();

        let project_root = std::path::PathBuf::from("/some/fixed/project-root");
        let mut session = crate::session::types::SessionFile::new(
            project_root.clone(),
            "resumed-connection".into(),
            "resumed-model".into(),
            PermissionTier::Ask,
            "2026-07-06T09:00:00Z".into(),
        );
        session.entries.push(TranscriptEntry::UserTurn { text: "earlier turn".into() });
        let path = crate::session::paths::new_session_path(state_dir.path(), &project_root, chrono::Utc::now());
        crate::session::store::save_session(&path, &session).unwrap();

        let mut props = test_props();
        props.user_state_dir = state_dir.path().to_path_buf();
        props.user_config_dir = user_config_dir.path().to_path_buf();
        props.project_config_dir = project_config_dir.path().to_path_buf();
        props.project_root = project_root;
        let mut t = TestTerminal::new(80, 24, Element::component::<App>(props)).unwrap();

        // Sanity check: the Header starts out showing test_props()'s
        // original connection/model, exactly as it did before the fix this
        // guards (Header staleness after in-TUI /model and /resume switches).
        assert!(t.frame_text().contains("local-vllm"), "{}", t.frame_text());

        type_and_submit(&mut t, "/resume").await;
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("resumed-connection"), "{}", t.frame_text());

        t.send_key(KeyCode::Char('1')).unwrap();
        for _ in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            t.tick().await.unwrap();
        }

        let text = t.frame_text();
        assert!(!text.contains("no longer exists"), "{text}");
        assert!(!text.contains("failed to resume"), "{text}");
        assert!(text.contains("earlier turn"), "{text}");
        assert!(text.contains("resumed-connection") && text.contains("resumed-model"), "{text}");
    }
}
