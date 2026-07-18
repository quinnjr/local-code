// src/tui/workspace/state.rs
//
// Pure state machine for the tmux-style workspace: windows (fullscreen tabs)
// each holding a binary split tree of panes, every pane an agent session.
// Follows the `mcp_wizard.rs` pattern — no side effects, no ntui state, fully
// unit-testable. The `Workspace` component (`mod.rs`) owns one of these in an
// `ntui::State`, feeds every key event through `on_key`, and reacts to the
// returned `KeyAction` (create a session's props, drop them, or exit).

use ntui::hooks::input::{KeyCode, KeyModifiers};

/// Identifies one agent session (== one pane). Allocated monotonically by
/// `WorkspaceState` and never reused, so ntui keys derived from it are stable
/// across splits/closes.
pub type SessionId = u64;

/// How a split lays out its two children. Named by resulting layout rather
/// than tmux's inverted vocabulary: `Row` is tmux `%` (panes side by side),
/// `Column` is tmux `"` (panes stacked).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    Row,
    Column,
}

/// The classic tmux pane model: a binary tree whose leaves are sessions.
/// Splits are always 50/50 in v1 (no resize — see TODO.md).
#[derive(Debug, Clone, PartialEq)]
pub enum PaneNode {
    Leaf(SessionId),
    Split {
        dir: SplitDir,
        children: [Box<PaneNode>; 2],
    },
}

impl PaneNode {
    /// All leaf session ids, in tree (left-to-right) order.
    pub fn leaves(&self) -> Vec<SessionId> {
        match self {
            PaneNode::Leaf(id) => vec![*id],
            PaneNode::Split { children, .. } => {
                let mut out = children[0].leaves();
                out.extend(children[1].leaves());
                out
            }
        }
    }

    /// Path of child indices from `self` down to the leaf holding `target`.
    fn path_to(&self, target: SessionId) -> Option<Vec<usize>> {
        match self {
            PaneNode::Leaf(id) => (*id == target).then(Vec::new),
            PaneNode::Split { children, .. } => {
                for (i, child) in children.iter().enumerate() {
                    if let Some(mut path) = child.path_to(target) {
                        path.insert(0, i);
                        return Some(path);
                    }
                }
                None
            }
        }
    }

    /// Replaces the leaf holding `target` with a split of (`target`, `new_id`)
    /// along `dir`. Returns false (tree unchanged) if `target` isn't present.
    fn split_leaf(&mut self, target: SessionId, dir: SplitDir, new_id: SessionId) -> bool {
        match self {
            PaneNode::Leaf(id) if *id == target => {
                *self = PaneNode::Split {
                    dir,
                    children: [
                        Box::new(PaneNode::Leaf(target)),
                        Box::new(PaneNode::Leaf(new_id)),
                    ],
                };
                true
            }
            PaneNode::Leaf(_) => false,
            PaneNode::Split { children, .. } => children
                .iter_mut()
                .any(|c| c.split_leaf(target, dir, new_id)),
        }
    }

    /// Removes the leaf holding `target`, collapsing its parent split so the
    /// sibling subtree takes the parent's slot. Returns the surviving tree,
    /// or `None` if `target` was the only leaf.
    fn remove_leaf(self, target: SessionId) -> Option<PaneNode> {
        match self {
            PaneNode::Leaf(id) if id == target => None,
            leaf @ PaneNode::Leaf(_) => Some(leaf),
            PaneNode::Split { dir, children } => {
                let [a, b] = children;
                match (a.remove_leaf(target), b.remove_leaf(target)) {
                    (None, Some(survivor)) | (Some(survivor), None) => Some(survivor),
                    (Some(a), Some(b)) => Some(PaneNode::Split {
                        dir,
                        children: [Box::new(a), Box::new(b)],
                    }),
                    // Both `None` is impossible: session ids are unique, so
                    // `target` appears in at most one subtree; `remove_leaf`
                    // on the subtree *not* holding it always returns `Some`.
                    (None, None) => unreachable!("session id {target} present in both subtrees"),
                }
            }
        }
    }
}

/// One tmux-style window: a pane tree plus which pane has keyboard focus.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub root: PaneNode,
    pub focused: SessionId,
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
/// the tree/focus mutation `on_key` already applied internally.
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
}

impl WorkspaceState {
    /// A workspace with one window holding one pane: session id 0 (the
    /// session `run_tui` seeds from CLI flags / `--resume`).
    pub fn new() -> (Self, SessionId) {
        let first = 0;
        (
            WorkspaceState {
                windows: vec![Window {
                    root: PaneNode::Leaf(first),
                    focused: first,
                }],
                active: 0,
                prefix_pending: false,
                next_session: first + 1,
            },
            first,
        )
    }

    fn alloc_session(&mut self) -> SessionId {
        let id = self.next_session;
        self.next_session += 1;
        id
    }

    fn active_window_mut(&mut self) -> &mut Window {
        &mut self.windows[self.active]
    }

    pub fn active_window(&self) -> &Window {
        &self.windows[self.active]
    }

    /// The session that currently has keyboard focus (focused pane of the
    /// active window).
    pub fn focused_session(&self) -> SessionId {
        self.active_window().focused
    }

    /// Every live session id across all windows, in window order then tree
    /// order — the component renders (mounts) exactly this set.
    pub fn all_sessions(&self) -> Vec<SessionId> {
        self.windows.iter().flat_map(|w| w.root.leaves()).collect()
    }

    fn new_window(&mut self) -> SessionId {
        let id = self.alloc_session();
        self.windows.push(Window {
            root: PaneNode::Leaf(id),
            focused: id,
        });
        self.active = self.windows.len() - 1;
        id
    }

    fn split(&mut self, dir: SplitDir) -> SessionId {
        let id = self.alloc_session();
        let window = &mut self.windows[self.active];
        let focused = window.focused;
        window.root.split_leaf(focused, dir, id);
        window.focused = id;
        id
    }

    fn close_focused(&mut self) -> KeyAction {
        let window = self.active_window_mut();
        let closed = window.focused;
        match std::mem::replace(&mut window.root, PaneNode::Leaf(closed)).remove_leaf(closed) {
            Some(survivor) => {
                // Focus the leaf nearest (in tree order) to where the closed
                // pane was — tmux focuses the sibling; the first leaf of the
                // surviving sibling subtree is exactly that.
                let path = survivor.leaves();
                window.root = survivor;
                window.focused = path[0];
                KeyAction::SessionClosed(closed)
            }
            None => {
                self.windows.remove(self.active);
                if self.windows.is_empty() {
                    return KeyAction::ExitApp(closed);
                }
                if self.active >= self.windows.len() {
                    self.active = self.windows.len() - 1;
                }
                KeyAction::SessionClosed(closed)
            }
        }
    }

    fn focus_next(&mut self) {
        let window = self.active_window_mut();
        let leaves = window.root.leaves();
        let pos = leaves.iter().position(|&l| l == window.focused).unwrap_or(0);
        window.focused = leaves[(pos + 1) % leaves.len()];
    }

    /// Moves pane focus directionally: walk up from the focused leaf to the
    /// nearest ancestor split along the movement axis where the focused leaf
    /// sits on the near side, cross into the sibling subtree, then descend
    /// picking the child nearest the crossing edge. v1 ignores cross-axis
    /// position (good enough for 50/50 trees).
    fn focus_dir(&mut self, direction: Direction) {
        let window = self.active_window_mut();
        let Some(path) = window.root.path_to(window.focused) else {
            return;
        };
        let (axis, forward) = match direction {
            Direction::Left => (SplitDir::Row, false),
            Direction::Right => (SplitDir::Row, true),
            Direction::Up => (SplitDir::Column, false),
            Direction::Down => (SplitDir::Column, true),
        };
        // Find the deepest ancestor split we can cross at.
        let mut node = &window.root;
        let mut ancestors: Vec<(&PaneNode, usize)> = Vec::new();
        for &step in &path {
            ancestors.push((node, step));
            let PaneNode::Split { children, .. } = node else {
                return;
            };
            node = &children[step];
        }
        for (ancestor, step) in ancestors.into_iter().rev() {
            let PaneNode::Split { dir, children } = ancestor else {
                continue;
            };
            let crossable = *dir == axis && ((forward && step == 0) || (!forward && step == 1));
            if crossable {
                let target_child = if forward { &children[1] } else { &children[0] };
                let mut cursor: &PaneNode = target_child;
                loop {
                    match cursor {
                        PaneNode::Leaf(id) => {
                            window.focused = *id;
                            return;
                        }
                        PaneNode::Split { dir, children } => {
                            // Along the movement axis, enter from the near
                            // edge; across it, default to the first child.
                            cursor = if *dir == axis && !forward {
                                &children[1]
                            } else {
                                &children[0]
                            };
                        }
                    }
                }
            }
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
        assert_eq!(prefixed(&mut state, KeyCode::Char('z')), KeyAction::Consumed);
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
        assert_eq!(
            state.active_window().root,
            PaneNode::Split {
                dir: SplitDir::Row,
                children: [
                    Box::new(PaneNode::Leaf(0)),
                    Box::new(PaneNode::Leaf(1)),
                ],
            }
        );
    }

    #[test]
    fn quote_splits_column() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('"'));
        let PaneNode::Split { dir, .. } = &state.active_window().root else {
            panic!("expected split");
        };
        assert_eq!(*dir, SplitDir::Column);
    }

    #[test]
    fn nested_split_splits_only_the_focused_leaf() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%')); // [0 | 1], focus 1
        prefixed(&mut state, KeyCode::Char('"')); // 1 becomes [1 / 2]
        assert_eq!(state.all_sessions(), vec![0, 1, 2]);
        assert_eq!(
            state.active_window().root,
            PaneNode::Split {
                dir: SplitDir::Row,
                children: [
                    Box::new(PaneNode::Leaf(0)),
                    Box::new(PaneNode::Split {
                        dir: SplitDir::Column,
                        children: [
                            Box::new(PaneNode::Leaf(1)),
                            Box::new(PaneNode::Leaf(2)),
                        ],
                    }),
                ],
            }
        );
    }

    #[test]
    fn o_cycles_pane_focus_in_tree_order() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%'));
        prefixed(&mut state, KeyCode::Char('"'));
        assert_eq!(state.focused_session(), 2);
        prefixed(&mut state, KeyCode::Char('o'));
        assert_eq!(state.focused_session(), 0, "o wraps to first leaf");
        prefixed(&mut state, KeyCode::Char('o'));
        assert_eq!(state.focused_session(), 1);
    }

    #[test]
    fn arrow_focus_moves_across_a_row_split() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%')); // [0 | 1], focus 1
        prefixed(&mut state, KeyCode::Left);
        assert_eq!(state.focused_session(), 0);
        prefixed(&mut state, KeyCode::Right);
        assert_eq!(state.focused_session(), 1);
        prefixed(&mut state, KeyCode::Right);
        assert_eq!(state.focused_session(), 1, "no pane to the right: no-op");
    }

    #[test]
    fn arrow_focus_ignores_wrong_axis() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%')); // row split
        prefixed(&mut state, KeyCode::Up);
        assert_eq!(state.focused_session(), 1, "no column split: up is a no-op");
    }

    #[test]
    fn arrow_focus_descends_to_nearest_edge_in_nested_tree() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%')); // [0 | 1], focus 1
        prefixed(&mut state, KeyCode::Char('"')); // [0 | [1 / 2]], focus 2
        prefixed(&mut state, KeyCode::Left); // cross into left subtree
        assert_eq!(state.focused_session(), 0);
        prefixed(&mut state, KeyCode::Right); // back: nearest edge is leaf 1
        assert_eq!(state.focused_session(), 1);
        prefixed(&mut state, KeyCode::Down);
        assert_eq!(state.focused_session(), 2);
        prefixed(&mut state, KeyCode::Up);
        assert_eq!(state.focused_session(), 1);
    }

    #[test]
    fn x_closes_pane_and_collapses_split_to_sibling() {
        let (mut state, _) = WorkspaceState::new();
        prefixed(&mut state, KeyCode::Char('%')); // [0 | 1], focus 1
        let action = prefixed(&mut state, KeyCode::Char('x'));
        assert_eq!(action, KeyAction::SessionClosed(1));
        assert_eq!(state.active_window().root, PaneNode::Leaf(0));
        assert_eq!(state.focused_session(), 0);
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
    fn closing_a_middle_window_keeps_active_index_valid() {
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
