// src/tui/workspace/mod.rs
//
// The tmux-style root component: mounts every window's sessions at once
// (inactive windows are collapsed to zero height, never unmounted, so their
// agents keep streaming and their transcript state survives), routes `C-b`
// prefix chords via the pure `WorkspaceState` machine, and draws the tab bar.

pub mod state;
pub mod tab_bar;

use std::collections::HashMap;

use ntui::props::{Dimension, FlexDirection, Overflow};
use ntui::style::{BorderStyle, Color};
use ntui::{Element, KeyCode, component, element};

use crate::session::paths::new_session_path;
use crate::session::store::save_session;
use crate::session::types::SessionFile;
use crate::tui::app::{App, AppProps};
use state::{KeyAction, SessionId, SplitDir, WorkspaceState};
use tab_bar::{TabBar, TabBarProps, TabInfo};

#[derive(Clone, Default)]
pub struct WorkspaceProps {
    /// Props of the initial session (id 0), exactly as `run_tui` would have
    /// mounted a lone `App` — including `--resume` seeding. Also the template
    /// new tabs/panes are stamped from (fresh transcript, fresh session file,
    /// everything else inherited).
    pub template: AppProps,
}

impl PartialEq for WorkspaceProps {
    /// `Workspace` is mounted exactly once at the TUI root, so its props
    /// never change between renders (same rationale as the pre-workspace
    /// `AppProps` impl had when `App` was the root).
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

/// Stamps a brand-new session's `AppProps` from the template: fresh session
/// file on disk (same shape as `run_tui` creates at startup), empty
/// transcript/history, everything else inherited.
fn new_session_props(template: &AppProps) -> Result<AppProps, String> {
    let now = chrono::Utc::now();
    let path = new_session_path(&template.user_state_dir, &template.project_root, now);
    let created_at = now.to_rfc3339();
    let session = SessionFile::new(
        template.project_root.clone(),
        template.connection_name.clone(),
        template.model_name.clone(),
        template.initial_tier,
        created_at.clone(),
    );
    save_session(&path, &session).map_err(|e| e.to_string())?;
    Ok(AppProps {
        initial_entries: Vec::new(),
        initial_messages: Vec::new(),
        session_path: path,
        created_at,
        ..template.clone()
    })
}

#[component]
pub fn Workspace(props: &WorkspaceProps, hooks: &mut Hooks) -> Element {
    let workspace = hooks.use_state(|| WorkspaceState::new().0);
    let input_gate = hooks.use_state(|| false);
    let sessions = hooks.use_state({
        let template = props.template.clone();
        move || vec![(0 as SessionId, template)]
    });
    let notice = hooks.use_state(|| Option::<String>::None);
    let streaming_flags = hooks.use_state(HashMap::<u64, bool>::new);

    let app_handle = hooks.use_app();
    hooks.use_input({
        let workspace = workspace.clone();
        let input_gate = input_gate.clone();
        let sessions = sessions.clone();
        let notice = notice.clone();
        let streaming_flags = streaming_flags.clone();
        let template = props.template.clone();
        let app_handle = app_handle.clone();
        move |ev, ctx| {
            if ev.code == KeyCode::Char('c')
                && ev
                    .modifiers
                    .contains(ntui::hooks::input::KeyModifiers::CONTROL)
            {
                app_handle.exit();
                return;
            }
            let snapshot = workspace.get();
            let mut state = snapshot.clone();
            let action = state.on_key(ev.code, ev.modifiers);
            if action == KeyAction::Pass {
                return;
            }
            notice.set(None);
            match action {
                KeyAction::Pass => unreachable!("handled above"),
                KeyAction::Consumed => {}
                KeyAction::SessionCreated(id) => match new_session_props(&template) {
                    Ok(new_props) => sessions.update(|list| list.push((id, new_props))),
                    Err(err) => {
                        // Roll the pane/window back out — the workspace must
                        // never render a session it has no props for.
                        notice.set(Some(format!("couldn't create session: {err}")));
                        state = snapshot;
                        state.prefix_pending = false;
                    }
                },
                KeyAction::SessionClosed(id) => {
                    sessions.update(|list| list.retain(|(sid, _)| *sid != id));
                    streaming_flags.update(|map| {
                        map.remove(&id);
                    });
                }
                KeyAction::ExitApp(_) => {
                    app_handle.exit();
                    return;
                }
            }
            input_gate.set(state.prefix_pending);
            workspace.set(state);
            ctx.stop_propagation();
        }
    });

    let state = workspace.get();
    let session_props = sessions.get();
    let flags = streaming_flags.get();
    let focused_session = state.focused_session();

    let mut children: Vec<Element> = Vec::new();
    for (window_index, window) in state.windows.iter().enumerate() {
        let is_active = window_index == state.active;
        let split = window.panes.len() > 1;
        let mut pane_els: Vec<Element> = Vec::new();
        for &sid in &window.panes {
            let Some((_, stored)) = session_props.iter().find(|(id, _)| *id == sid) else {
                // A created-but-failed session can never appear here (the
                // handler rolls the state back), so this is purely defensive.
                continue;
            };
            let mut app_props = stored.clone();
            app_props.focused = is_active && sid == focused_session;
            app_props.input_gate = Some(input_gate.clone());
            app_props.session_tag = sid;
            app_props.streaming_flags = Some(streaming_flags.clone());
            let border_style = if split {
                BorderStyle::Single
            } else {
                BorderStyle::None
            };
            let border_color = if split && app_props.focused {
                Color::Cyan
            } else {
                Color::DarkGrey
            };
            pane_els.push(
                element! {
                    View(flex_grow: 1.0, border_style: border_style, border_color: border_color) {
                        #(vec![Element::component::<App>(app_props)])
                    }
                }
                .with_key(format!("s-{sid}")),
            );
        }
        let direction = match window.dir {
            Some(SplitDir::Row) => FlexDirection::Row,
            Some(SplitDir::Column) | None => FlexDirection::Column,
        };
        let window_el = if is_active {
            element! {
                View(flex_grow: 1.0, flex_direction: direction, width: Dimension::Percent(100.0)) {
                    #(pane_els)
                }
            }
        } else {
            // Collapsed, clipped, but still mounted: the window's sessions
            // keep their state and any in-flight streams while hidden.
            element! {
                View(height: Dimension::Cells(0), flex_direction: direction, width: Dimension::Percent(100.0), overflow: Overflow::Clip) {
                    #(pane_els)
                }
            }
        };
        children.push(window_el.with_key(format!("w-{}", window.id)));
    }

    let tabs = state
        .windows
        .iter()
        .enumerate()
        .map(|(index, window)| TabInfo {
            index,
            panes: window.panes.len(),
            streaming: window
                .panes
                .iter()
                .any(|sid| flags.get(sid).copied().unwrap_or(false)),
        })
        .collect::<Vec<_>>();
    children.push(
        element! {
            TabBar(
                tabs: tabs,
                active: state.active,
                prefix_pending: state.prefix_pending,
                notice: notice.get(),
            )
        }
        .with_key("tab-bar"),
    );

    element! {
        View(flex_direction: FlexDirection::Column, width: Dimension::Percent(100.0), height: Dimension::Percent(100.0), padding: 0) {
            #(children)
        }
    }
}
