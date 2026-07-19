// Pure state machine for the tmux-style workspace: windows (fullscreen tabs)
// each holding a row or column of panes, every pane an agent session. Follows
// the `mcp_wizard.rs` pattern — no side effects, no ntui state, fully
// unit-testable. The `Workspace` component (`mod.rs`) owns one of these in an
// `ntui::State`, feeds every key event through `on_key`, and reacts to the
// returned `KeyAction` (create a session's props, drop them, or exit).
//
// Panes are deliberately a flat `Vec` per window rather than tmux's nested
// binary split tree: ntui's reconciler matches keys among *siblings only*, so
// nesting would reparent a session's component on every split — unmounting it
// and losing its live transcript/agent state. A flat list keeps every pane a
// sibling under one stable window wrapper forever. The cost is that a window
// has a single split axis (its first split picks row vs column; later splits
// extend along the same axis) — recorded as a v1 limitation in TODO.md.

use ntui::hooks::input::{KeyCode, KeyModifiers};

/// Identifies one agent session (== one pane). Allocated monotonically by
/// `WorkspaceState` and never reused, so ntui keys derived from it are stable
/// across splits/closes.
pub type SessionId = u64;

/// The axis a window's panes are laid out along. Named by resulting layout
/// rather than tmux's inverted vocabulary: `Row` is tmux `%` (panes side by
/// side), `Column` is tmux `"` (panes stacked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    Row,
    Column,
}

/// One tmux-style window: equal-sized panes along one axis, plus which pane
/// has keyboard focus. `dir` is `None` until the first split (a single pane
/// has no axis) and never reverts — closing back down to one pane keeps the
/// axis for the next split, which is invisible in practice.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    /// Stable identity for ntui keys (windows have no other identity — their
    /// index shifts when an earlier window closes, and their first pane can
    /// be closed out from under them). Allocated monotonically, never reused.
    pub id: u64,
    pub dir: Option<SplitDir>,
    pub panes: Vec<SessionId>,
    /// Index into `panes`.
    pub focused: usize,
}

impl Window {
    pub fn focused_session(&self) -> SessionId {
        self.panes[self.focused]
    }
}

/// Where `C-b <arrow>` should move pane focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// What the `Workspace` component must do in response to a key event, beyond
/// the window/pane mutation `on_key` already applied internally.
#[derive(Debug, Clone, PartialEq)]
pub enum KeyAction {
    /// Not the workspace's key — let it fall through to the focused session.
    Pass,
    /// Handled internally (window switch, focus move, prefix armed/canceled);
    /// nothing further for the component to do beyond re-rendering.
    Consumed,
    /// A split or new window created this session; the component must build
    /// its `AppProps` (new session file etc.) before the next render.
    SessionCreated(SessionId),
    /// A pane was closed; the component should drop the session's props so
    /// the `App` unmounts (aborting any in-flight turn via its `Cleanup`).
    SessionClosed(SessionId),
    /// The last pane of the last window was closed — exit the app. The
    /// session id is the pane that was closed.
    ExitApp(SessionId),
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceState {
    pub windows: Vec<Window>,
    /// Index into `windows` of the fullscreen-visible window.
    pub active: usize,
    /// `Ctrl+B` was pressed and the next key is a workspace command. While
    /// set, sessions must ignore input (the component mirrors this into the
    /// `input_gate` state their guards check).
    pub prefix_pending: bool,
    next_session: SessionId,
    next_window: u64,
}

impl WorkspaceState {
    /// A workspace with one window holding one pane: session id 0 (the
    /// session `run_tui` seeds from CLI flags / `--resume`).
    pub fn new() -> (Self, SessionId) {
        let first = 0;
        (
            WorkspaceState {
                windows: vec![Window {
                    id: 0,
                    dir: None,
                    panes: vec![first],
                    focused: 0,
                }],
                active: 0,
                prefix_pending: false,
                next_session: first + 1,
                next_window: 1,
            },
            first,
        )
    }

    fn alloc_session(&mut self) -> SessionId {
        let id = self.next_session;
        self.next_session += 1;
        id
    }

    pub fn active_window(&self) -> &Window {
        &self.windows[self.active]
    }

    /// The session that currently has keyboard focus (focused pane of the
    /// active window).
    pub fn focused_session(&self) -> SessionId {
        self.active_window().focused_session()
    }

    /// Every live session id across all windows, in window order then pane
    /// order. Test-only observability helper (the component iterates windows
    /// directly when rendering); `#[cfg(test)]` keeps it honest now that this
    /// module is crate-private and dead-code analysis applies.
    #[cfg(test)]
    pub fn all_sessions(&self) -> Vec<SessionId> {
        self.windows.iter().flat_map(|w| w.panes.clone()).collect()
    }

    fn new_window(&mut self) -> SessionId {
        let id = self.alloc_session();
        let window_id = self.next_window;
        self.next_window += 1;
        self.windows.push(Window {
            id: window_id,
            dir: None,
            panes: vec![id],
            focused: 0,
        });
        self.active = self.windows.len() - 1;
        id
    }

    /// Splits the focused pane: the new pane is inserted right after it and
    /// takes focus. The window's first split fixes its axis; on an already
    /// split window the requested direction is ignored in favor of the
    /// window's axis (we don't nest mixed-direction splits — see module doc).
    fn split(&mut self, requested: SplitDir) -> SessionId {
        let id = self.alloc_session();
        let window = &mut self.windows[self.active];
        if window.dir.is_none() {
            window.dir = Some(requested);
        }
        window.panes.insert(window.focused + 1, id);
        window.focused += 1;
        id
    }

    fn close_focused(&mut self) -> KeyAction {
        let window = &mut self.windows[self.active];
        let closed = window.panes.remove(window.focused);
        if window.panes.is_empty() {
            self.windows.remove(self.active);
            if self.windows.is_empty() {
                return KeyAction::ExitApp(closed);
            }
            if self.active >= self.windows.len() {
                self.active = self.windows.len() - 1;
            }
        } else if window.focused >= window.panes.len() {
            window.focused = window.panes.len() - 1;
        }
        KeyAction::SessionClosed(closed)
    }

    /// `C-b o`: cycle pane focus within the active window.
    fn focus_next(&mut self) {
        let window = &mut self.windows[self.active];
        window.focused = (window.focused + 1) % window.panes.len();
    }

    /// `C-b <arrow>`: move pane focus along the window's axis. Arrows across
    /// the axis (e.g. Up in a side-by-side window) are no-ops, like tmux with
    /// no pane in that direction. No wraparound, also like tmux.
    fn focus_dir(&mut self, direction: Direction) {
        let window = &mut self.windows[self.active];
        let Some(dir) = window.dir else { return };
        let delta: isize = match (dir, direction) {
            (SplitDir::Row, Direction::Left) | (SplitDir::Column, Direction::Up) => -1,
            (SplitDir::Row, Direction::Right) | (SplitDir::Column, Direction::Down) => 1,
            _ => 0,
        };
        let target = window.focused as isize + delta;
        if delta != 0 && target >= 0 && (target as usize) < window.panes.len() {
            window.focused = target as usize;
        }
    }

    /// Feeds one key event through the tmux prefix state machine, applying
    /// any window/pane mutation internally and telling the component what
    /// else (if anything) it must do.
    pub fn on_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> KeyAction {
        if !self.prefix_pending {
            if code == KeyCode::Char('b') && modifiers.contains(KeyModifiers::CONTROL) {
                self.prefix_pending = true;
                return KeyAction::Consumed;
            }
            return KeyAction::Pass;
        }
        self.prefix_pending = false;
        match code {
            KeyCode::Char('c') => KeyAction::SessionCreated(self.new_window()),
            KeyCode::Char('n') => {
                self.active = (self.active + 1) % self.windows.len();
                KeyAction::Consumed
            }
            KeyCode::Char('p') => {
                self.active = (self.active + self.windows.len() - 1) % self.windows.len();
                KeyAction::Consumed
            }
            KeyCode::Char(c @ '0'..='9') => {
                let idx = c as usize - '0' as usize;
                if idx < self.windows.len() {
                    self.active = idx;
                }
                KeyAction::Consumed
            }
            KeyCode::Char('%') => KeyAction::SessionCreated(self.split(SplitDir::Row)),
            KeyCode::Char('"') => KeyAction::SessionCreated(self.split(SplitDir::Column)),
            KeyCode::Char('o') => {
                self.focus_next();
                KeyAction::Consumed
            }
            KeyCode::Char('x') => self.close_focused(),
            KeyCode::Left => {
                self.focus_dir(Direction::Left);
                KeyAction::Consumed
            }
            KeyCode::Right => {
                self.focus_dir(Direction::Right);
                KeyAction::Consumed
            }
            KeyCode::Up => {
                self.focus_dir(Direction::Up);
                KeyAction::Consumed
            }
            KeyCode::Down => {
                self.focus_dir(Direction::Down);
                KeyAction::Consumed
            }
            // Esc — and any unbound key, matching tmux — cancels the prefix.
            _ => KeyAction::Consumed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefixed(state: &mut WorkspaceState, code: KeyCode) -> KeyAction {
        assert_eq!(
            state.on_key(KeyCode::Char('b'), KeyModifiers::CONTROL),
            KeyAction::Consumed,
            "C-b should arm the prefix"
        );
        assert!(state.prefix_pending);
        state.on_key(code, KeyModifiers::NONE)
    }

    #[test]
    fn starts_with_one_window_one_pane() {
        let (state, first) = WorkspaceState::new();
        assert_eq!(first, 0);
        assert_eq!(state.windows.len(), 1);
        assert_eq!(state.focused_session(), 0);
        assert_eq!(state.all_sessions(), vec![0]);
        assert_eq!(state.active_window().dir, None);
    }

    #[test]
    fn plain_keys_pass_through_without_prefix() {
        let (mut state, _) = WorkspaceState::new();
        assert_eq!(
            state.on_key(KeyCode::Char('c'), KeyModifiers::NONE),
            KeyAction::Pass
        );
        assert_eq!(
            state.on_key(KeyCode::Char('x'), KeyModifiers::CONTROL),
            KeyAction::Pass
        );
        assert!(!state.prefix_pending);
    }

    #[test]
    fn unknown_key_cancels_prefix_without_acting() {
        let (mut state, _) = WorkspaceState::new();
        assert_eq!(
            prefixed(&mut state, KeyCode::Char('z')),
            KeyAction::Consumed
        );
        assert!(!state.prefix_pending);
        assert_eq!(state.windows.len(), 1);
    }

    #[test]
    fn esc_cancels_prefix() {
        let (mut state, _) = WorkspaceState::new();
        assert_eq!(prefixed(&mut state, KeyCode::Esc), KeyAction::Consumed);
        assert!(!state.prefix_pending);
    }

    #[test]
    fn c_creates_and_activates_new_window() {
        let (mut state, _) = WorkspaceState::new();
        let action = prefixed(&mut state, KeyCode::Char('c'));
        assert_eq!(action, KeyAction::SessionCreated(1));
        assert_eq!(state.windows.len(), 2);
        assert_eq!(state.active, 1);
        assert_eq!(state.focused_session(), 1);
        assert_eq!(state.all_sessions(), vec![0, 1]);
    }

    #[test]
    fn n_and_p_cycle_windows_with_wraparound() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('c'));
        prefixed(&mut state, KeyCode::Char('c'));
        assert_eq!(state.active, 2);
        prefixed(&mut state, KeyCode::Char('n'));
        assert_eq!(state.active, 0, "n wraps from last to first");
        prefixed(&mut state, KeyCode::Char('p'));
        assert_eq!(state.active, 2, "p wraps from first to last");
        prefixed(&mut state, KeyCode::Char('p'));
        assert_eq!(state.active, 1);
    }

    #[test]
    fn digits_select_windows_and_ignore_out_of_range() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('c'));
        prefixed(&mut state, KeyCode::Char('0'));
        assert_eq!(state.active, 0);
        prefixed(&mut state, KeyCode::Char('7'));
        assert_eq!(state.active, 0, "out-of-range digit is a no-op");
        prefixed(&mut state, KeyCode::Char('1'));
        assert_eq!(state.active, 1);
    }

    #[test]
    fn percent_splits_row_and_focuses_new_pane() {
        let (mut state, _) = WorkspaceState::new();
        let action = prefixed(&mut state, KeyCode::Char('%'));
        assert_eq!(action, KeyAction::SessionCreated(1));
        assert_eq!(state.focused_session(), 1);
        let window = state.active_window();
        assert_eq!(window.dir, Some(SplitDir::Row));
        assert_eq!(window.panes, vec![0, 1]);
    }

    #[test]
    fn quote_splits_column() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('"'));
        assert_eq!(state.active_window().dir, Some(SplitDir::Column));
    }

    #[test]
    fn split_inserts_after_focused_pane() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%')); // [0, 1], focus 1
        prefixed(&mut state, KeyCode::Left); // focus 0
        prefixed(&mut state, KeyCode::Char('%')); // insert after 0
        let window = state.active_window();
        assert_eq!(window.panes, vec![0, 2, 1]);
        assert_eq!(state.focused_session(), 2);
    }

    #[test]
    fn second_split_keeps_the_windows_axis() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%')); // axis: Row
        prefixed(&mut state, KeyCode::Char('"')); // requested Column, ignored
        let window = state.active_window();
        assert_eq!(window.dir, Some(SplitDir::Row), "axis fixed by first split");
        assert_eq!(window.panes, vec![0, 1, 2]);
    }

    #[test]
    fn o_cycles_pane_focus_with_wraparound() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%'));
        prefixed(&mut state, KeyCode::Char('%')); // [0, 1, 2], focus idx 2
        assert_eq!(state.focused_session(), 2);
        prefixed(&mut state, KeyCode::Char('o'));
        assert_eq!(state.focused_session(), 0, "o wraps to first pane");
        prefixed(&mut state, KeyCode::Char('o'));
        assert_eq!(state.focused_session(), 1);
    }

    #[test]
    fn arrow_focus_moves_along_a_row_window_without_wrap() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%')); // [0, 1], focus 1
        prefixed(&mut state, KeyCode::Left);
        assert_eq!(state.focused_session(), 0);
        prefixed(&mut state, KeyCode::Left);
        assert_eq!(state.focused_session(), 0, "no pane to the left: no-op");
        prefixed(&mut state, KeyCode::Right);
        assert_eq!(state.focused_session(), 1);
        prefixed(&mut state, KeyCode::Right);
        assert_eq!(state.focused_session(), 1, "no pane to the right: no-op");
    }

    #[test]
    fn arrow_focus_ignores_the_cross_axis() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%')); // Row window
        prefixed(&mut state, KeyCode::Up);
        assert_eq!(state.focused_session(), 1, "Up in a Row window is a no-op");
        prefixed(&mut state, KeyCode::Down);
        assert_eq!(state.focused_session(), 1);
    }

    #[test]
    fn arrows_work_up_down_in_a_column_window() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('"')); // [0 / 1], focus 1
        prefixed(&mut state, KeyCode::Up);
        assert_eq!(state.focused_session(), 0);
        prefixed(&mut state, KeyCode::Down);
        assert_eq!(state.focused_session(), 1);
    }

    #[test]
    fn x_closes_pane_and_focuses_the_next_one() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%')); // [0, 1], focus 1
        let action = prefixed(&mut state, KeyCode::Char('x'));
        assert_eq!(action, KeyAction::SessionClosed(1));
        assert_eq!(state.active_window().panes, vec![0]);
        assert_eq!(state.focused_session(), 0);
    }

    #[test]
    fn x_in_the_middle_keeps_focus_index_on_the_successor() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%'));
        prefixed(&mut state, KeyCode::Char('%')); // [0, 1, 2], focus idx 2
        prefixed(&mut state, KeyCode::Left); // focus idx 1 (session 1)
        let action = prefixed(&mut state, KeyCode::Char('x'));
        assert_eq!(action, KeyAction::SessionClosed(1));
        assert_eq!(state.active_window().panes, vec![0, 2]);
        assert_eq!(state.focused_session(), 2, "successor pane takes focus");
    }

    #[test]
    fn x_on_last_pane_of_a_window_removes_the_window() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('c')); // window 1, session 1
        let action = prefixed(&mut state, KeyCode::Char('x'));
        assert_eq!(action, KeyAction::SessionClosed(1));
        assert_eq!(state.windows.len(), 1);
        assert_eq!(state.active, 0);
        assert_eq!(state.focused_session(), 0);
    }

    #[test]
    fn x_on_the_only_pane_exits_the_app() {
        let (mut state, _) = WorkspaceState::new();
        let action = prefixed(&mut state, KeyCode::Char('x'));
        assert_eq!(action, KeyAction::ExitApp(0));
    }

    #[test]
    fn closing_the_last_window_clamps_active_index() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('c')); // window 1
        prefixed(&mut state, KeyCode::Char('c')); // window 2, active = 2
        prefixed(&mut state, KeyCode::Char('x')); // close window 2
        assert_eq!(state.windows.len(), 2);
        assert_eq!(state.active, 1, "active clamps to the last window");
    }

    #[test]
    fn session_ids_are_never_reused_after_close() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%')); // session 1
        prefixed(&mut state, KeyCode::Char('x')); // close 1
        let action = prefixed(&mut state, KeyCode::Char('%'));
        assert_eq!(action, KeyAction::SessionCreated(2), "id 1 is not recycled");
    }

    #[test]
    fn arrow_on_an_unsplit_window_is_a_noop() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Left); // dir is still None
        assert_eq!(state.focused_session(), 0);
        assert!(!state.prefix_pending, "the chord is still consumed");
    }

    #[test]
    fn prefix_ignores_modifiers_on_the_command_key() {
        // Like tmux: after `C-b`, the command key's modifiers are not
        // inspected — `C-b Ctrl+x` closes the pane exactly like `C-b x`.
        // Pinned so a future modifier-sensitive rebind is a deliberate
        // decision, not an accident.
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%'));
        assert!(!state.prefix_pending);
        state.on_key(KeyCode::Char('b'), KeyModifiers::CONTROL);
        let action = state.on_key(KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(action, KeyAction::SessionClosed(1));
    }

    #[test]
    fn prefix_key_itself_is_consumed_not_passed() {
        let (mut state, _) = WorkspaceState::new();
        assert_eq!(
            state.on_key(KeyCode::Char('b'), KeyModifiers::CONTROL),
            KeyAction::Consumed
        );
        // A second C-b while armed is just an unbound key: cancels.
        assert_eq!(
            state.on_key(KeyCode::Char('b'), KeyModifiers::CONTROL),
            KeyAction::Consumed
        );
        assert!(!state.prefix_pending);
    }
}
