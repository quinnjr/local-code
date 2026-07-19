use ntui::props::{FlexDirection, Overflow};
use ntui::style::{BorderStyle, Color};
use ntui::{Element, KeyCode, component, element};

use std::sync::Arc;

use crate::permissions::types::PermissionRequest;
use crate::tui::components::permission_card::render_permission_card;
use crate::tui::state::{TranscriptEntries, TranscriptEntry};

#[derive(Clone, Default)]
pub struct TranscriptProps {
    pub entries: TranscriptEntries,
    pub pending_permission: Option<PermissionRequest>,
    /// The in-flight streamed assistant text for the current turn, rendered
    /// as a trailing pseudo-entry. Kept out of `entries` so every `TextDelta`
    /// re-render clones one small growing `String` instead of the whole
    /// transcript (see `run_turn`'s stream loop) — the entry is folded into
    /// `entries` once the streamed block completes.
    pub streaming_text: String,
    /// Whether this transcript's pane has keyboard focus. The workspace
    /// mounts one `Transcript` per pane (including hidden windows) and ntui
    /// dispatches each key to every handler in the tree, deepest-first, until
    /// one stops propagation — so an unfocused transcript must not consume
    /// scroll keys, or it steals them from the focused pane AND from
    /// `Workspace`'s `C-b <arrow>` chords (leaving the prefix wedged armed).
    pub focused: bool,
    /// Mirror of the workspace's "a `C-b` chord is armed" flag (same handle
    /// `App` checks in `session_may_handle_input`): while armed, even the
    /// focused transcript lets arrow keys bubble up so `C-b <Up>/<Down>`
    /// reach `Workspace` instead of scrolling.
    pub input_gate: Option<ntui::State<bool>>,
}

impl PartialEq for TranscriptProps {
    /// `input_gate` is excluded: handlers read it through the shared
    /// `ntui::State` at event time, so a gate flip needs no re-render here
    /// (mirrors `AppProps`'s treatment of the same handle). Field order
    /// matters: while streaming, `streaming_text` differs on every token, so
    /// comparing it FIRST short-circuits before the O(n) deep walk of
    /// `entries` that would otherwise run per token for an always-false
    /// answer.
    fn eq(&self, other: &Self) -> bool {
        self.streaming_text == other.streaming_text
            && self.focused == other.focused
            && self.pending_permission == other.pending_permission
            && entries_eq(&self.entries, &other.entries)
    }
}

/// Entry-list equality with an `Arc::ptr_eq` fast path: after the Arc
/// migration, an unchanged transcript compares as n pointer checks instead of
/// n deep `String` comparisons (only a genuinely-replaced entry falls back to
/// a value compare).
fn entries_eq(a: &[Arc<TranscriptEntry>], b: &[Arc<TranscriptEntry>]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| Arc::ptr_eq(x, y) || x == y)
}

/// The scrollable, full-width transcript pane. Owns its own `Scroll` handle
/// (created via `hooks.use_scroll()`) and a `use_input` that only intercepts
/// Up/Down/PageUp/PageDown — and only while its pane is focused and no
/// workspace prefix chord is armed — calling `stop_propagation()` for those
/// so they don't also reach other handlers, while every other key (typed
/// characters, Enter, digits for permission choices) bubbles up untouched.
#[component]
pub fn Transcript(props: &TranscriptProps, hooks: &mut ntui::Hooks) -> Element {
    let scroll = hooks.use_scroll();
    hooks.use_input({
        let scroll = scroll.clone();
        let focused = props.focused;
        let input_gate = props.input_gate.clone();
        move |ev, ctx| {
            if !focused || input_gate.as_ref().is_some_and(|gate| gate.get()) {
                return;
            }
            match ev.code {
                KeyCode::Up => {
                    scroll.scroll_by(-1);
                    ctx.stop_propagation();
                }
                KeyCode::Down => {
                    scroll.scroll_by(1);
                    ctx.stop_propagation();
                }
                KeyCode::PageUp => {
                    scroll.scroll_by(-10);
                    ctx.stop_propagation();
                }
                KeyCode::PageDown => {
                    scroll.scroll_by(10);
                    ctx.stop_propagation();
                }
                _ => {}
            }
        }
    });

    // Each entry is rendered via its own `TranscriptEntryView` component
    // (rather than inlining `render_entry`'s output directly) so ntui's
    // fiber reconciler can skip re-running the render body for entries
    // whose content hasn't changed since the last render (`update_fiber`'s
    // `props_eq` check in `ntui`'s reconciler short-circuits before
    // recursing when a `Component` fiber's props compare equal). Since
    // `run_turn` only ever mutates the *last* entry while streaming, this
    // means every earlier (unchanged) entry's rebuild is skipped on every
    // streamed token, rather than being rebuilt from scratch on each
    // `Transcript` render as before.
    let mut children: Vec<Element> = props
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            Element::component::<TranscriptEntryView>(EntryProps {
                entry: entry.clone(),
            })
            .with_key(i.to_string())
        })
        .collect();

    if !props.streaming_text.is_empty() {
        children.push(
            render_entry(&TranscriptEntry::AssistantText {
                text: props.streaming_text.clone(),
            })
            .with_key("streaming-tail"),
        );
    }

    if let Some(request) = &props.pending_permission {
        children.push(render_permission_card(request).with_key("pending-permission"));
    }

    element! {
        View(
            flex_direction: FlexDirection::Column,
            flex_grow: 1.0_f32,
            overflow: Overflow::Scroll,
            scroll: Some(scroll),
            padding: 0
        ) {
            #(children)
        }
    }
}

/// Props for `TranscriptEntryView`, wrapping a single entry so it can be
/// mounted as its own component fiber (see the doc comment at `Transcript`'s
/// `children` construction above for why). `Default` is only ever exercised
/// to satisfy `Component::Props`'s bound (this component is always mounted
/// with a real entry via `element!`/`Element::component`, never defaulted).
#[derive(Clone)]
struct EntryProps {
    entry: Arc<TranscriptEntry>,
}

impl PartialEq for EntryProps {
    /// `Arc::ptr_eq` fast path first: while streaming, every entry except the
    /// last is the same allocation render-to-render.
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.entry, &other.entry) || self.entry == other.entry
    }
}

impl Default for EntryProps {
    fn default() -> Self {
        EntryProps {
            entry: Arc::new(TranscriptEntry::SystemNotice {
                text: String::new(),
            }),
        }
    }
}

#[component]
fn TranscriptEntryView(props: &EntryProps, _hooks: &mut ntui::Hooks) -> Element {
    render_entry(&props.entry)
}

fn render_entry(entry: &TranscriptEntry) -> Element {
    match entry {
        TranscriptEntry::UserTurn { text } => element! {
            View(border_style: BorderStyle::Single, border_color: Color::Blue, padding: 1) {
                Text(content: text.clone(), color: Color::White)
            }
        },
        TranscriptEntry::AssistantText { text } => element! {
            View(padding: 1) {
                Text(content: text.clone(), color: Color::Reset)
            }
        },
        TranscriptEntry::ToolCall(call) => render_tool_card(call),
        TranscriptEntry::PermissionResolved {
            description,
            allowed,
        } => {
            let (label, color) = if *allowed {
                ("allowed", Color::Green)
            } else {
                ("denied", Color::Red)
            };
            element! {
                View(padding: 1) {
                    Text(content: format!("[{label}] {description}"), color: color)
                }
            }
        }
        TranscriptEntry::SystemNotice { text } => element! {
            View(padding: 1) {
                Text(content: text.clone(), color: Color::DarkGrey)
            }
        },
    }
}

fn render_tool_card(call: &crate::tui::state::ToolCallEntry) -> Element {
    let header = format!("{} {}", if call.expanded { "▾" } else { "▸" }, call.name);
    if !call.expanded {
        return element! {
            View(border_style: BorderStyle::Single, border_color: Color::DarkGrey, padding: 0) {
                Text(content: header, color: Color::Magenta)
            }
        };
    }

    let mut body: Vec<Element> = vec![element! {
        Text(content: format!("args: {}", call.arguments_json), color: Color::DarkGrey)
    }];

    if let Some(result) = &call.result {
        for line in diff_lines(&call.name, &result.content) {
            body.push(line);
        }
        if result.is_error {
            body.push(element! { Text(content: "(tool reported an error)", color: Color::Red) });
        }
    } else {
        body.push(element! { Text(content: "(running…)", color: Color::DarkGrey) });
    }

    element! {
        View(flex_direction: FlexDirection::Column, border_style: BorderStyle::Single, border_color: Color::Magenta, padding: 1) {
            Text(content: header, color: Color::Magenta)
            #(body)
        }
    }
}

/// Renders `edit_file`/`write_file` tool results with +/- diff coloring per the
/// spec; every other tool's result is shown as plain text. `edit_file`'s
/// `ToolOutput` content (from `local_code::agent::tools::edit_file`) is either
/// `"edited {path}"` (success, no diff body to show beyond that line) or an
/// error string — the built-in tool doesn't currently return a unified diff,
/// so "diff coloring" here colors whole result lines green (success) or red
/// (error) rather than per-hunk +/-; this is the full extent of diff
/// information the Phase 2 tool contract exposes today.
fn diff_lines(tool_name: &str, content: &str) -> Vec<Element> {
    let is_mutation = matches!(tool_name, "write_file" | "edit_file" | "bash");
    content
        .lines()
        .map(|line| {
            let (prefix, color) = if is_mutation {
                ("+ ", Color::Green)
            } else {
                ("  ", Color::Reset)
            };
            element! { Text(content: format!("{prefix}{line}"), color: color) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::state::{ToolCallEntry, ToolCallResult};
    use ntui::testing::TestTerminal;

    fn entries_fixture() -> TranscriptEntries {
        vec![
            Arc::new(TranscriptEntry::UserTurn {
                text: "fix the bug".into(),
            }),
            Arc::new(TranscriptEntry::ToolCall(ToolCallEntry {
                id: "1".into(),
                name: "edit_file".into(),
                arguments_json: r#"{"path":"x.rs"}"#.into(),
                result: Some(ToolCallResult {
                    content: "edited x.rs".into(),
                    is_error: false,
                }),
                expanded: true,
            })),
            Arc::new(TranscriptEntry::AssistantText {
                text: "Done, fixed it.".into(),
            }),
        ]
    }

    #[tokio::test]
    async fn renders_user_turn_tool_card_and_assistant_text() {
        let props = TranscriptProps {
            entries: entries_fixture(),
            ..Default::default()
        };
        let t = TestTerminal::new(60, 20, Element::component::<Transcript>(props)).unwrap();
        let text = t.frame_text();
        assert!(text.contains("fix the bug"));
        assert!(text.contains("edit_file"));
        assert!(text.contains("edited x.rs"));
        assert!(text.contains("Done, fixed it."));
    }

    #[tokio::test]
    async fn collapsed_tool_card_hides_its_body() {
        let mut entries = entries_fixture();
        if let TranscriptEntry::ToolCall(call) = Arc::make_mut(&mut entries[1]) {
            call.expanded = false;
        }
        let props = TranscriptProps {
            entries,
            ..Default::default()
        };
        let t = TestTerminal::new(60, 20, Element::component::<Transcript>(props)).unwrap();
        assert!(!t.frame_text().contains("edited x.rs"));
    }

    #[tokio::test]
    async fn renders_pending_permission_card_when_present() {
        let props = TranscriptProps {
            entries: vec![],
            focused: true,
            pending_permission: Some(PermissionRequest {
                tool_name: "bash".into(),
                description: "run shell command: rm x".into(),
                command_preview: Some("rm x".into()),
            }),
            ..Default::default()
        };
        let t = TestTerminal::new(60, 10, Element::component::<Transcript>(props)).unwrap();
        assert!(
            t.frame_text()
                .contains("Permission requested: run shell command: rm x")
        );
    }

    #[tokio::test]
    async fn up_key_scrolls_without_panicking() {
        let props = TranscriptProps {
            entries: entries_fixture(),
            focused: true,
            ..Default::default()
        };
        let mut t = TestTerminal::new(60, 3, Element::component::<Transcript>(props)).unwrap();
        t.send_key(ntui::KeyCode::Up).unwrap(); // must not panic even with no scroll headroom
    }
}
