// src/tui/components/transcript.rs

use ntui::props::{FlexDirection, Overflow};
use ntui::style::{BorderStyle, Color};
use ntui::{component, element, Element, KeyCode};

use crate::permissions::types::PermissionRequest;
use crate::tui::components::permission_card::render_permission_card;
use crate::tui::state::TranscriptEntry;

#[derive(Clone, PartialEq, Default)]
pub struct TranscriptProps {
    pub entries: Vec<TranscriptEntry>,
    pub pending_permission: Option<PermissionRequest>,
}

/// The scrollable, full-width transcript pane. Owns its own `Scroll` handle
/// (created via `hooks.use_scroll()`) and a `use_input` that only intercepts
/// Up/Down/PageUp/PageDown, calling `stop_propagation()` for those so they
/// don't also reach `App`'s own handler, while every other key (typed
/// characters, Enter, digits for permission choices) bubbles up untouched.
#[component]
pub fn Transcript(props: &TranscriptProps, hooks: &mut ntui::Hooks) -> Element {
    let scroll = hooks.use_scroll();
    hooks.use_input({
        let scroll = scroll.clone();
        move |ev, ctx| match ev.code {
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
    });

    let mut children: Vec<Element> = props
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| render_entry(entry).with_key(i.to_string()))
        .collect();

    if let Some(request) = &props.pending_permission {
        children.push(render_permission_card(request).with_key("pending-permission"));
    }

    element! {
        View(
            flex_direction: FlexDirection::Column,
            flex_grow: 1.0,
            overflow: Overflow::Scroll,
            scroll: Some(scroll),
            padding: 0
        ) {
            #(children)
        }
    }
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
        TranscriptEntry::PermissionResolved { description, allowed } => {
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
    let header = format!(
        "{} {}",
        if call.expanded { "▾" } else { "▸" },
        call.name
    );
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

    fn entries_fixture() -> Vec<TranscriptEntry> {
        vec![
            TranscriptEntry::UserTurn {
                text: "fix the bug".into(),
            },
            TranscriptEntry::ToolCall(ToolCallEntry {
                id: "1".into(),
                name: "edit_file".into(),
                arguments_json: r#"{"path":"x.rs"}"#.into(),
                result: Some(ToolCallResult {
                    content: "edited x.rs".into(),
                    is_error: false,
                }),
                expanded: true,
            }),
            TranscriptEntry::AssistantText {
                text: "Done, fixed it.".into(),
            },
        ]
    }

    #[tokio::test]
    async fn renders_user_turn_tool_card_and_assistant_text() {
        let props = TranscriptProps {
            entries: entries_fixture(),
            pending_permission: None,
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
        if let TranscriptEntry::ToolCall(call) = &mut entries[1] {
            call.expanded = false;
        }
        let props = TranscriptProps {
            entries,
            pending_permission: None,
        };
        let t = TestTerminal::new(60, 20, Element::component::<Transcript>(props)).unwrap();
        assert!(!t.frame_text().contains("edited x.rs"));
    }

    #[tokio::test]
    async fn renders_pending_permission_card_when_present() {
        let props = TranscriptProps {
            entries: vec![],
            pending_permission: Some(PermissionRequest {
                tool_name: "bash".into(),
                description: "run shell command: rm x".into(),
                command_preview: Some("rm x".into()),
            }),
        };
        let t = TestTerminal::new(60, 10, Element::component::<Transcript>(props)).unwrap();
        assert!(t.frame_text().contains("Permission requested: run shell command: rm x"));
    }

    #[tokio::test]
    async fn up_key_scrolls_without_panicking() {
        let props = TranscriptProps {
            entries: entries_fixture(),
            pending_permission: None,
        };
        let mut t = TestTerminal::new(60, 3, Element::component::<Transcript>(props)).unwrap();
        t.send_key(ntui::KeyCode::Up).unwrap(); // must not panic even with no scroll headroom
    }
}
