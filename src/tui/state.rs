// src/tui/state.rs

/// One entry in the transcript, in display order. Cloned into `ntui::State` on
/// every update, so kept cheap and flat (no `Rc`/`Arc` needed — clones are just
/// string/vec copies of already-small turn data).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TranscriptEntry {
    /// A user-submitted prompt, rendered in a bordered box.
    UserTurn { text: String },
    /// Assistant plain-text output, appended to token-by-token while streaming.
    AssistantText { text: String },
    /// A tool call in progress or completed, rendered as an inline card.
    ToolCall(ToolCallEntry),
    /// A permission decision that has already been resolved (so the transcript
    /// keeps a record after the inline prompt is dismissed).
    PermissionResolved { description: String, allowed: bool },
    /// A non-fatal system message (errors, and — until Phase 4 implements real
    /// dispatch — the "slash commands aren't implemented yet" notice).
    ///
    /// `text` is NOT auto-wrapped by the Transcript component's current layout
    /// (available width isn't definite when `Text` is measured), so any text
    /// expected to run past ~70-80 chars on a line must include explicit `\n`
    /// breaks or it will be clipped mid-word at paint time.
    SystemNotice { text: String },
}

/// A tool call's lifecycle, tracked as one mutable entry updated in place as
/// `StreamEvent`s arrive (`ToolCallStart` creates it, `ToolCallDelta` appends to
/// `arguments_json`, `ToolResult` fills in `result`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallEntry {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
    pub result: Option<ToolCallResult>,
    /// Whether the card renders its arguments/result body. Toggled by the
    /// Transcript component's Tab handler (Task 6); defaults to expanded so a
    /// freshly-arriving card is immediately readable.
    pub expanded: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCallResult {
    pub content: String,
    pub is_error: bool,
}

/// Running token/cost totals shown in the footer. Cost stays `0.0` for
/// local-only connections (no cost model wired up in this phase) but the field
/// is populated so a later non-local connection lights it up with no shape
/// change, per the spec's footer note.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct UsageSummary {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub estimated_cost: f64,
}

impl UsageSummary {
    pub fn add(&mut self, input_tokens: u32, output_tokens: u32, estimated_cost: f64) {
        self.input_tokens += input_tokens;
        self.output_tokens += output_tokens;
        self.estimated_cost += estimated_cost;
    }
}

/// Finds the most recently appended `ToolCall` entry with matching `id`, for
/// in-place updates as further `StreamEvent`s for the same call arrive.
pub fn find_tool_call_mut<'a>(
    entries: &'a mut [TranscriptEntry],
    id: &str,
) -> Option<&'a mut ToolCallEntry> {
    entries.iter_mut().rev().find_map(|e| match e {
        TranscriptEntry::ToolCall(call) if call.id == id => Some(call),
        _ => None,
    })
}

/// Toggles `expanded` on the most recently appended `ToolCall` entry, if any.
/// Used by the Transcript component's Tab key handler.
pub fn toggle_last_tool_call_expanded(entries: &mut [TranscriptEntry]) {
    if let Some(TranscriptEntry::ToolCall(call)) = entries
        .iter_mut()
        .rev()
        .find(|e| matches!(e, TranscriptEntry::ToolCall(_)))
    {
        call.expanded = !call.expanded;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_call(id: &str) -> TranscriptEntry {
        TranscriptEntry::ToolCall(ToolCallEntry {
            id: id.to_string(),
            name: "bash".into(),
            arguments_json: String::new(),
            result: None,
            expanded: true,
        })
    }

    #[test]
    fn find_tool_call_mut_locates_by_id_from_the_end() {
        let mut entries = vec![sample_call("a"), sample_call("b")];
        let found = find_tool_call_mut(&mut entries, "b").expect("should find call b");
        found.arguments_json = "{}".into();
        let TranscriptEntry::ToolCall(call) = &entries[1] else {
            panic!("expected ToolCall")
        };
        assert_eq!(call.id, "b"); // unchanged id/name
        assert_eq!(call.name, "bash");
        assert_eq!(call.arguments_json, "{}");
    }

    #[test]
    fn find_tool_call_mut_returns_none_for_unknown_id() {
        let mut entries = vec![sample_call("a")];
        assert!(find_tool_call_mut(&mut entries, "missing").is_none());
    }

    #[test]
    fn toggle_last_tool_call_expanded_flips_only_the_most_recent_call() {
        let mut entries = vec![sample_call("a"), sample_call("b")];
        toggle_last_tool_call_expanded(&mut entries);
        let TranscriptEntry::ToolCall(a) = &entries[0] else {
            panic!()
        };
        let TranscriptEntry::ToolCall(b) = &entries[1] else {
            panic!()
        };
        assert!(a.expanded, "earlier call untouched");
        assert!(!b.expanded, "most recent call toggled off");
    }

    #[test]
    fn toggle_last_tool_call_expanded_is_a_no_op_when_no_tool_calls_exist() {
        let mut entries = vec![TranscriptEntry::UserTurn {
            text: "hi".into(),
        }];
        toggle_last_tool_call_expanded(&mut entries); // must not panic
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn usage_summary_add_accumulates() {
        let mut usage = UsageSummary::default();
        usage.add(10, 20, 0.001);
        usage.add(5, 5, 0.0005);
        assert_eq!(usage.input_tokens, 15);
        assert_eq!(usage.output_tokens, 25);
        assert!((usage.estimated_cost - 0.0015).abs() < 1e-9);
    }

    #[test]
    fn transcript_entry_round_trips_through_json() {
        let entries = vec![
            TranscriptEntry::UserTurn { text: "fix the bug".into() },
            TranscriptEntry::ToolCall(ToolCallEntry {
                id: "1".into(),
                name: "edit_file".into(),
                arguments_json: "{}".into(),
                result: Some(ToolCallResult { content: "edited x.rs".into(), is_error: false }),
                expanded: true,
            }),
            TranscriptEntry::AssistantText { text: "done".into() },
            TranscriptEntry::PermissionResolved { description: "run rm".into(), allowed: false },
            TranscriptEntry::SystemNotice { text: "note".into() },
        ];
        let json = serde_json::to_string(&entries).unwrap();
        let back: Vec<TranscriptEntry> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entries);
    }

    #[test]
    fn usage_summary_round_trips_through_json() {
        let usage = UsageSummary { input_tokens: 10, output_tokens: 5, estimated_cost: 0.01 };
        let json = serde_json::to_string(&usage).unwrap();
        let back: UsageSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back, usage);
    }
}
