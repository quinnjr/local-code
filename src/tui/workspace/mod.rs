// The tmux-style root component: mounts every window's sessions at once
// (inactive windows are collapsed to zero height, never unmounted, so their
// agents keep streaming and their transcript state survives), routes `C-b`
// prefix chords via the pure `WorkspaceState` machine, and draws the tab bar.

// Crate-internal: the state machine and status line are implementation
// details of `Workspace`; only `Workspace`/`WorkspaceProps` are public
// surface (re-exported from `tui`). Narrowed after the branch's API review
// flagged the fully-pub mutable state-machine fields as inviting external
// coupling to internal layout state.
pub(crate) mod state;
pub(crate) mod tab_bar;

use std::collections::HashMap;

use ntui::props::{Dimension, FlexDirection, Overflow};
use ntui::style::BorderStyle;
use ntui::{Element, KeyCode, component, element};

use crate::session::store::create_fresh_session;
use crate::tui::app::{App, AppProps};
use crate::tui::theme::local_code_theme;
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
/// file on disk (via the same `create_fresh_session` recipe `run_tui` uses
/// at startup), empty transcript/history, everything else inherited.
fn new_session_props(template: &AppProps) -> Result<AppProps, String> {
    let (path, created_at) = create_fresh_session(
        &template.user_state_dir,
        &template.project_root,
        &template.connection_name,
        &template.model_name,
        template.initial_tier,
        chrono::Utc::now(),
    )
    .map_err(|e| e.to_string())?;
    Ok(AppProps {
        initial_entries: Vec::new(),
        initial_messages: Vec::new(),
        session_path: path,
        created_at,
        ..template.clone()
    })
}

/// Shown when `C-b x` targets a pane with a turn in flight. The prose names
/// the confirming chord, so the constant lives beside the handler that
/// implements it — if the kill binding is ever remapped, both change here
/// (and the test asserting this string fails until they agree).
const KILL_CONFIRM_NOTICE: &str = "pane is mid-turn — press C-b x again to kill it";

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
    let permission_flags = hooks.use_state(HashMap::<u64, bool>::new);
    // `C-b x` on a streaming pane arms this instead of killing; a second
    // `C-b x` on the same pane confirms. Cleared by any other completed chord.
    let pending_kill = hooks.use_state(|| Option::<state::SessionId>::None);

    let app_handle = hooks.use_app();
    hooks.use_input({
        let workspace = workspace.clone();
        let input_gate = input_gate.clone();
        let sessions = sessions.clone();
        let notice = notice.clone();
        let streaming_flags = streaming_flags.clone();
        let permission_flags = permission_flags.clone();
        let template = props.template.clone();
        let app_handle = app_handle.clone();
        let pending_kill = pending_kill.clone();
        move |ev, ctx| {
            let ctrl = ev
                .modifiers
                .contains(ntui::hooks::input::KeyModifiers::CONTROL);
            if ev.code == KeyCode::Char('c') && ctrl {
                app_handle.exit();
                return;
            }
            // One clone per keystroke: `workspace.get()` is already owned.
            // The rollback path below re-fetches instead of keeping a second
            // eager snapshot that the common `Pass` case would waste.
            let mut state = workspace.get();

            // `C-b x` on a pane that is mid-turn needs a second confirmation
            // chord: aborting the stream discards the turn even though tool
            // side effects it already ran (edits, commands) are on disk.
            if state.prefix_pending && ev.code == KeyCode::Char('x') {
                let target = state.focused_session();
                let busy = streaming_flags.get().get(&target).copied().unwrap_or(false);
                if busy && pending_kill.get() != Some(target) {
                    pending_kill.set(Some(target));
                    notice.set(Some(KILL_CONFIRM_NOTICE.to_string()));
                    state.prefix_pending = false;
                    input_gate.set(false);
                    workspace.set(state);
                    ctx.stop_propagation();
                    return;
                }
            }

            // Was this event the arming `C-b` itself (as opposed to a command
            // key)? A pending notice — e.g. a session-creation failure or the
            // kill-confirm prompt — must survive the arming press of the NEXT
            // chord, or the user can never read it before retrying.
            let arming = !state.prefix_pending && ev.code == KeyCode::Char('b') && ctrl;

            let action = state.on_key(ev.code, ev.modifiers);
            if action == KeyAction::Pass {
                return;
            }
            // A pending notice (session-creation failure, kill-confirm
            // prompt) is cleared only when a chord COMPLETES a command:
            // neither the arming `C-b` (the notice must survive into the
            // user's retry) nor a cancel (`Esc` — an accidental arm must not
            // eat the message) may clear it.
            if !arming && action != KeyAction::PrefixCanceled {
                notice.set(None);
                pending_kill.set(None);
            }
            match action {
                KeyAction::Pass => unreachable!("handled above"),
                KeyAction::Consumed | KeyAction::PrefixCanceled => {}
                KeyAction::SessionCreated(id) => match new_session_props(&template) {
                    Ok(new_props) => sessions.update(|list| list.push((id, new_props))),
                    Err(err) => {
                        // Roll the pane/window back out — the workspace must
                        // never render a session it has no props for. The
                        // pre-key state is still what `workspace` holds (the
                        // write-back below hasn't happened yet), so re-fetch
                        // instead of keeping an always-paid snapshot clone.
                        notice.set(Some(format!("couldn't create session: {err}")));
                        state = workspace.get();
                        state.prefix_pending = false;
                    }
                },
                KeyAction::SessionClosed(id) => {
                    sessions.update(|list| list.retain(|(sid, _)| *sid != id));
                    streaming_flags.update(|map| {
                        map.remove(&id);
                    });
                    permission_flags.update(|map| {
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
    let waiting = permission_flags.get();
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
            app_props.permission_flags = Some(permission_flags.clone());
            let border_style = if split {
                BorderStyle::Single
            } else {
                BorderStyle::None
            };
            let border_color = if split && app_props.focused {
                local_code_theme().accent
            } else {
                local_code_theme().border
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
            awaiting_permission: window
                .panes
                .iter()
                .any(|sid| waiting.get(sid).copied().unwrap_or(false)),
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

    // The one place the app-wide theme is provided: every pane's `App` (and
    // every widget below it) resolves `use_theme()` against this context.
    element! {
        ContextProvider(value: local_code_theme()) {
            View(flex_direction: FlexDirection::Column, width: Dimension::Percent(100.0), height: Dimension::Percent(100.0), padding: 0) {
                #(children)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use daimon::model::SharedModel;
    use daimon::stream::StreamEvent;
    use ntui::testing::TestTerminal;
    use ntui::{Element, KeyCode};

    use crate::permissions::types::PermissionTier;
    use crate::tui::test_support::{ChannelModel, StreamingEchoModel};

    /// Workspace props whose template writes session files into `dir` (new
    /// tabs/panes create one on the spot, so it must be a real tempdir).
    fn props_in(dir: &std::path::Path) -> WorkspaceProps {
        props_with_model(dir, Arc::new(StreamingEchoModel))
    }

    fn props_with_model(dir: &std::path::Path, model: SharedModel) -> WorkspaceProps {
        WorkspaceProps {
            template: AppProps {
                model: Some(model),
                connection_name: "local-vllm".into(),
                model_name: "qwen2.5-coder-32b".into(),
                initial_tier: PermissionTier::FullAuto,
                user_state_dir: dir.to_path_buf(),
                project_root: dir.to_path_buf(),
                ..AppProps::default()
            },
        }
    }

    fn prefix(t: &mut TestTerminal) {
        t.send_key_event(ntui::KeyEvent::new(
            KeyCode::Char('b'),
            ntui::hooks::input::KeyModifiers::CONTROL,
        ))
        .unwrap();
    }

    async fn chord(t: &mut TestTerminal, code: KeyCode) {
        prefix(t);
        t.tick().await.unwrap();
        t.send_key(code).unwrap();
        t.tick().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn starts_with_one_window_marked_active_in_the_tab_bar() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = TestTerminal::new(
            90,
            24,
            Element::component::<Workspace>(props_in(dir.path())),
        )
        .unwrap();
        t.tick().await.unwrap();
        let text = t.frame_text();
        assert!(text.contains("local-code"), "{text}");
        assert!(text.contains("0:agent*"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn prefix_shows_badge_and_is_not_typed_into_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = TestTerminal::new(
            90,
            24,
            Element::component::<Workspace>(props_in(dir.path())),
        )
        .unwrap();
        prefix(&mut t);
        t.tick().await.unwrap();
        let text = t.frame_text();
        assert!(text.contains("C-b …"), "{text}");
        assert!(
            !text.contains("❯ b"),
            "prefix key must not be typed: {text}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn c_b_c_opens_a_second_window_and_switches_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = TestTerminal::new(
            90,
            24,
            Element::component::<Workspace>(props_in(dir.path())),
        )
        .unwrap();
        chord(&mut t, KeyCode::Char('c')).await;
        let text = t.frame_text();
        assert!(text.contains("0:agent "), "{text}");
        assert!(text.contains("1:agent*"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn typing_lands_only_in_the_focused_window_and_survives_switching() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = TestTerminal::new(
            90,
            24,
            Element::component::<Workspace>(props_in(dir.path())),
        )
        .unwrap();
        for c in "first".chars() {
            t.send_key(KeyCode::Char(c)).unwrap();
        }
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("first"));

        chord(&mut t, KeyCode::Char('c')).await; // window 1, empty input
        let text = t.frame_text();
        assert!(
            !text.contains("first"),
            "window 0 is hidden, its buffer must not paint: {text}"
        );

        for c in "second".chars() {
            t.send_key(KeyCode::Char(c)).unwrap();
        }
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("second"));

        chord(&mut t, KeyCode::Char('p')).await; // back to window 0
        let text = t.frame_text();
        assert!(
            text.contains("first"),
            "window 0's input buffer must survive being hidden: {text}"
        );
        assert!(!text.contains("second"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn digit_selects_a_window_directly() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = TestTerminal::new(
            90,
            24,
            Element::component::<Workspace>(props_in(dir.path())),
        )
        .unwrap();
        chord(&mut t, KeyCode::Char('c')).await;
        chord(&mut t, KeyCode::Char('0')).await;
        assert!(t.frame_text().contains("0:agent*"), "{}", t.frame_text());
    }

    #[tokio::test(start_paused = true)]
    async fn percent_splits_into_two_panes_with_independent_input() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = TestTerminal::new(
            120,
            30,
            Element::component::<Workspace>(props_in(dir.path())),
        )
        .unwrap();
        chord(&mut t, KeyCode::Char('%')).await;
        assert!(
            t.frame_text().contains("0:agent[2]*"),
            "tab bar shows the pane count: {}",
            t.frame_text()
        );
        // New (right) pane has focus: type into it.
        for c in "right".chars() {
            t.send_key(KeyCode::Char(c)).unwrap();
        }
        t.tick().await.unwrap();
        assert!(t.frame_text().contains("right"));
        // Move focus left and type there — both stay visible (both panes are
        // on screen), each in its own pane.
        chord(&mut t, KeyCode::Left).await;
        for c in "left".chars() {
            t.send_key(KeyCode::Char(c)).unwrap();
        }
        t.tick().await.unwrap();
        let text = t.frame_text();
        assert!(text.contains("left"), "{text}");
        assert!(text.contains("right"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn x_closes_the_focused_pane_and_returns_to_a_single_pane() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = TestTerminal::new(
            120,
            30,
            Element::component::<Workspace>(props_in(dir.path())),
        )
        .unwrap();
        chord(&mut t, KeyCode::Char('%')).await;
        chord(&mut t, KeyCode::Char('x')).await;
        let text = t.frame_text();
        assert!(text.contains("0:agent*"), "{text}");
        assert!(!text.contains("0:agent[2]*"), "{text}");
    }

    #[tokio::test(start_paused = true)]
    async fn new_pane_gets_its_own_session_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = TestTerminal::new(
            120,
            30,
            Element::component::<Workspace>(props_in(dir.path())),
        )
        .unwrap();
        chord(&mut t, KeyCode::Char('c')).await;
        // The template's own session file is created by `run_tui` before the
        // workspace mounts (not by this test), so exactly the new tab's file
        // exists under the state dir's sessions tree.
        let sessions_root = dir.path().join("sessions");
        let count = walk_files(&sessions_root);
        assert_eq!(count, 1, "one session file for the new tab");
    }

    fn walk_files(root: &std::path::Path) -> usize {
        let Ok(entries) = std::fs::read_dir(root) else {
            return 0;
        };
        entries
            .flatten()
            .map(|e| {
                let p = e.path();
                if p.is_dir() { walk_files(&p) } else { 1 }
            })
            .sum()
    }

    /// Regression test for the multi-mount input-dispatch bug: `Transcript`s
    /// used to consume Up/Down unconditionally (deepest-first dispatch), so a
    /// `C-b <Up>` never reached `Workspace` — pane focus didn't move and,
    /// worse, the prefix stayed wedged armed, turning the next innocent
    /// keystroke into a chord.
    #[tokio::test(start_paused = true)]
    async fn c_b_arrow_is_consumed_by_the_workspace_not_a_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = TestTerminal::new(
            120,
            30,
            Element::component::<Workspace>(props_in(dir.path())),
        )
        .unwrap();
        chord(&mut t, KeyCode::Char('"')).await; // column split, focus bottom
        chord(&mut t, KeyCode::Up).await; // move focus to the top pane
        let text = t.frame_text();
        assert!(
            !text.contains("C-b …"),
            "prefix must not stay wedged after C-b <Up>: {text}"
        );
        for c in "still typing".chars() {
            t.send_key(KeyCode::Char(c)).unwrap();
        }
        t.tick().await.unwrap();
        assert!(
            t.frame_text().contains("still typing"),
            "keys after the chord must be plain input again: {}",
            t.frame_text()
        );
    }

    /// The flagship workspace claim: a window switched away from keeps its
    /// turn streaming (the tab bar marks it `✻`), and the completed reply is
    /// there when the user switches back.
    #[tokio::test(start_paused = true)]
    async fn hidden_window_keeps_streaming_and_shows_the_busy_marker() {
        let dir = tempfile::tempdir().unwrap();
        let (model, events) = ChannelModel::new();
        let mut t = TestTerminal::new(
            120,
            30,
            Element::component::<Workspace>(props_with_model(dir.path(), Arc::new(model))),
        )
        .unwrap();

        for c in "go".chars() {
            t.send_key(KeyCode::Char(c)).unwrap();
        }
        t.send_key(KeyCode::Enter).unwrap();
        t.tick().await.unwrap(); // turn effect spawns; stream opens
        t.tick().await.unwrap();

        chord(&mut t, KeyCode::Char('c')).await; // window 1 takes over the screen

        events
            .send(Ok(StreamEvent::TextDelta("hidden reply".into())))
            .unwrap();
        t.tick().await.unwrap();
        let text = t.frame_text();
        assert!(
            !text.contains("hidden reply"),
            "window 0 is collapsed; its stream must not paint: {text}"
        );
        assert!(
            text.contains("0:agent✻"),
            "tab bar must mark the hidden window busy: {text}"
        );

        events.send(Ok(StreamEvent::Done)).unwrap();
        // The end-of-turn session save runs on the blocking pool (a real OS
        // thread), which the paused test clock can't fast-forward — tick
        // until the turn lands, bounded so a regression still fails loudly.
        let mut turn_finished = false;
        for _ in 0..100 {
            t.tick().await.unwrap();
            if !t.frame_text().contains('✻') {
                turn_finished = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(
            turn_finished,
            "busy marker clears once the turn finishes: {}",
            t.frame_text()
        );

        chord(&mut t, KeyCode::Char('p')).await; // back to window 0
        let text = t.frame_text();
        assert!(
            text.contains("hidden reply"),
            "the reply streamed while hidden must be in the transcript: {text}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn session_creation_failure_rolls_back_and_shows_a_notice() {
        let dir = tempfile::tempdir().unwrap();
        // `user_state_dir` is a regular FILE, so `create_fresh_session`'s
        // `create_dir_all(<file>/sessions/…)` fails for every new pane.
        let state_file = dir.path().join("not-a-dir");
        std::fs::write(&state_file, "x").unwrap();
        let mut props = props_in(dir.path());
        props.template.user_state_dir = state_file;

        let mut t = TestTerminal::new(120, 30, Element::component::<Workspace>(props)).unwrap();
        chord(&mut t, KeyCode::Char('c')).await;
        let text = t.frame_text();
        assert!(
            text.contains("couldn't create session"),
            "the failure must be surfaced: {text}"
        );
        assert!(
            text.contains("0:agent*") && !text.contains("1:agent"),
            "the failed window must be rolled back: {text}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn c_b_x_on_a_busy_pane_warns_then_a_second_x_kills() {
        let dir = tempfile::tempdir().unwrap();
        let (model, events) = ChannelModel::new();
        let mut t = TestTerminal::new(
            120,
            30,
            Element::component::<Workspace>(props_with_model(dir.path(), Arc::new(model))),
        )
        .unwrap();
        // Open a second window so killing the busy pane doesn't exit the app.
        chord(&mut t, KeyCode::Char('c')).await;
        chord(&mut t, KeyCode::Char('p')).await; // back to window 0
        for c in "go".chars() {
            t.send_key(KeyCode::Char(c)).unwrap();
        }
        t.send_key(KeyCode::Enter).unwrap();
        t.tick().await.unwrap();
        t.tick().await.unwrap(); // turn is now streaming (channel held open)

        chord(&mut t, KeyCode::Char('x')).await;
        let text = t.frame_text();
        assert!(
            text.contains(KILL_CONFIRM_NOTICE),
            "first C-b x on a busy pane must warn, not kill: {text}"
        );
        assert!(
            text.contains("0:agent"),
            "the busy pane must still be alive: {text}"
        );

        chord(&mut t, KeyCode::Char('x')).await;
        let text = t.frame_text();
        assert!(
            !text.contains("1:agent"),
            "the confirming second C-b x must kill the pane (one window left): {text}"
        );
        drop(events);
    }

    #[tokio::test(start_paused = true)]
    async fn another_chord_clears_the_pending_kill_confirmation() {
        let dir = tempfile::tempdir().unwrap();
        let (model, events) = ChannelModel::new();
        let mut t = TestTerminal::new(
            120,
            30,
            Element::component::<Workspace>(props_with_model(dir.path(), Arc::new(model))),
        )
        .unwrap();
        chord(&mut t, KeyCode::Char('c')).await;
        chord(&mut t, KeyCode::Char('p')).await;
        for c in "go".chars() {
            t.send_key(KeyCode::Char(c)).unwrap();
        }
        t.send_key(KeyCode::Enter).unwrap();
        t.tick().await.unwrap();
        t.tick().await.unwrap();

        chord(&mut t, KeyCode::Char('x')).await; // arm the kill confirm
        chord(&mut t, KeyCode::Char('n')).await; // different chord: clears it
        chord(&mut t, KeyCode::Char('p')).await; // back to the busy window
        chord(&mut t, KeyCode::Char('x')).await; // must WARN again, not kill
        let text = t.frame_text();
        assert!(
            text.contains(KILL_CONFIRM_NOTICE),
            "pending kill must reset after an unrelated chord: {text}"
        );
        assert!(text.contains("1:agent"), "no pane was killed: {text}");
        drop(events);
    }

    #[tokio::test(start_paused = true)]
    async fn a_notice_survives_the_arming_press_of_the_next_chord() {
        let dir = tempfile::tempdir().unwrap();
        let state_file = dir.path().join("not-a-dir");
        std::fs::write(&state_file, "x").unwrap();
        let mut props = props_in(dir.path());
        props.template.user_state_dir = state_file;

        let mut t = TestTerminal::new(120, 30, Element::component::<Workspace>(props)).unwrap();
        chord(&mut t, KeyCode::Char('c')).await; // fails -> notice
        assert!(t.frame_text().contains("couldn't create session"));

        // An accidental arm + cancel must not eat the message: while armed
        // the tab bar shows the `C-b …` badge instead, but after Esc the
        // notice is back on screen.
        prefix(&mut t);
        t.tick().await.unwrap();
        assert!(
            t.frame_text().contains("C-b …"),
            "armed badge shows while mid-chord: {}",
            t.frame_text()
        );
        t.send_key(KeyCode::Esc).unwrap();
        t.tick().await.unwrap();
        assert!(
            t.frame_text().contains("couldn't create session"),
            "an armed-then-canceled chord must not clear the notice: {}",
            t.frame_text()
        );

        // A chord that completes a real command clears it.
        chord(&mut t, KeyCode::Char('n')).await;
        assert!(
            !t.frame_text().contains("couldn't create session"),
            "a completed chord clears the notice: {}",
            t.frame_text()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn ctrl_c_exits_the_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = TestTerminal::new(
            90,
            24,
            Element::component::<Workspace>(props_in(dir.path())),
        )
        .unwrap();
        t.send_key_event(ntui::KeyEvent::new(
            KeyCode::Char('c'),
            ntui::hooks::input::KeyModifiers::CONTROL,
        ))
        .unwrap();
        t.tick().await.unwrap();
        assert!(t.exited(), "Ctrl+C must exit now that Workspace owns it");
    }

    #[tokio::test(start_paused = true)]
    async fn c_b_x_on_the_last_pane_exits_the_app() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = TestTerminal::new(
            90,
            24,
            Element::component::<Workspace>(props_in(dir.path())),
        )
        .unwrap();
        chord(&mut t, KeyCode::Char('x')).await;
        assert!(t.exited(), "closing the only pane of the only window exits");
    }
}
