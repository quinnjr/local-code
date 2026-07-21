use std::sync::Arc;

/// One entry in the transcript, in display order. Cloned into `ntui::State` on
/// every update. Most fields are small (turn text, tool names/args), so plain
/// `String` clones are fine there — but `ToolCallResult::content` can hold a
/// full file read or large `bash`/`grep` output, so it's `Arc<str>` instead of
/// `String`: cloning a `TranscriptEntry` (which happens on every render, since
/// `ntui::State::get()` clones its value) then only bumps a refcount for that
/// field instead of copying potentially many KB of text.
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
    /// A non-fatal system message surfaced in the transcript (e.g. a streamed
    /// error or slash-command output).
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
    pub content: Arc<str>,
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

/// The transcript's in-memory storage: entries behind `Arc` so the per-render
/// clone (`ntui::State::get()` clones the whole Vec, once per keystroke and
/// per streamed token) bumps refcounts instead of deep-copying every entry's
/// `String`s. Mutation goes through `Arc::make_mut` (clone-on-write of the
/// single entry being updated). The session-file format is unchanged —
/// `SessionFile::entries` stays `Vec<TranscriptEntry>`, converted at the
/// save/load boundary.
pub type TranscriptEntries = Vec<Arc<TranscriptEntry>>;

/// `entries.push_entry(TranscriptEntry::…)` — keeps the many transcript push
/// sites free of `Arc::new` noise.
pub trait PushEntry {
    fn push_entry(&mut self, entry: TranscriptEntry);
}

impl PushEntry for TranscriptEntries {
    fn push_entry(&mut self, entry: TranscriptEntry) {
        self.push(Arc::new(entry));
    }
}

/// Finds the most recently appended `ToolCall` entry with matching `id`, for
/// in-place updates as further `StreamEvent`s for the same call arrive.
/// Locates the entry immutably first so `Arc::make_mut`'s clone-on-write hits
/// only the one entry being updated, never the entries scanned past.
pub fn find_tool_call_mut<'a>(
    entries: &'a mut [Arc<TranscriptEntry>],
    id: &str,
) -> Option<&'a mut ToolCallEntry> {
    let idx = entries
        .iter()
        .rposition(|e| matches!(&**e, TranscriptEntry::ToolCall(call) if call.id == id))?;
    match Arc::make_mut(&mut entries[idx]) {
        TranscriptEntry::ToolCall(call) => Some(call),
        _ => None,
    }
}

/// Toggles `expanded` on the most recently appended `ToolCall` entry, if any.
/// Used by the Transcript component's Tab key handler.
pub fn toggle_last_tool_call_expanded(entries: &mut [Arc<TranscriptEntry>]) {
    let last_call = entries
        .iter()
        .rposition(|e| matches!(&**e, TranscriptEntry::ToolCall(_)));
    if let Some(idx) = last_call
        && let TranscriptEntry::ToolCall(call) = Arc::make_mut(&mut entries[idx])
    {
        call.expanded = !call.expanded;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_call(id: &str) -> Arc<TranscriptEntry> {
        Arc::new(TranscriptEntry::ToolCall(ToolCallEntry {
            id: id.to_string(),
            name: "bash".into(),
            arguments_json: String::new(),
            result: None,
            expanded: true,
        }))
    }

    #[test]
    fn find_tool_call_mut_locates_by_id_from_the_end() {
        let mut entries = vec![sample_call("a"), sample_call("b")];
        let found = find_tool_call_mut(&mut entries, "b").expect("should find call b");
        found.arguments_json = "{}".into();
        let TranscriptEntry::ToolCall(call) = &*entries[1] else {
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

    /// The load-bearing Arc invariant: while streaming, a previous render
    /// holds clones of every entry (refcount ≥ 2), so a mutation must
    /// clone-on-write — landing in the live Vec while leaving the snapshot
    /// the in-flight frame rendered untouched.
    #[test]
    fn find_tool_call_mut_under_a_shared_arc_writes_the_vec_not_the_snapshot() {
        let mut entries = vec![sample_call("a"), sample_call("b")];
        let snapshot = entries.clone(); // what a prior render would hold

        find_tool_call_mut(&mut entries, "b")
            .expect("call b exists")
            .arguments_json = "{\"x\":1}".into();

        let TranscriptEntry::ToolCall(live) = &*entries[1] else {
            panic!()
        };
        let TranscriptEntry::ToolCall(snap) = &*snapshot[1] else {
            panic!()
        };
        assert_eq!(
            live.arguments_json, "{\"x\":1}",
            "mutation lands in the vec"
        );
        assert_eq!(snap.arguments_json, "", "the shared snapshot is untouched");
        assert!(
            !Arc::ptr_eq(&entries[1], &snapshot[1]),
            "the mutated entry was cloned-on-write"
        );
        assert!(
            Arc::ptr_eq(&entries[0], &snapshot[0]),
            "unmutated siblings still share their allocation"
        );
    }

    #[test]
    fn toggle_under_a_shared_arc_writes_the_vec_not_the_snapshot() {
        let mut entries = vec![sample_call("a")];
        let snapshot = entries.clone();
        toggle_last_tool_call_expanded(&mut entries);
        let TranscriptEntry::ToolCall(live) = &*entries[0] else {
            panic!()
        };
        let TranscriptEntry::ToolCall(snap) = &*snapshot[0] else {
            panic!()
        };
        assert!(!live.expanded);
        assert!(snap.expanded, "snapshot keeps the pre-toggle state");
    }

    #[test]
    fn toggle_last_tool_call_expanded_flips_only_the_most_recent_call() {
        let mut entries = vec![sample_call("a"), sample_call("b")];
        toggle_last_tool_call_expanded(&mut entries);
        let TranscriptEntry::ToolCall(a) = &*entries[0] else {
            panic!()
        };
        let TranscriptEntry::ToolCall(b) = &*entries[1] else {
            panic!()
        };
        assert!(a.expanded, "earlier call untouched");
        assert!(!b.expanded, "most recent call toggled off");
    }

    #[test]
    fn toggle_last_tool_call_expanded_is_a_no_op_when_no_tool_calls_exist() {
        let mut entries = vec![Arc::new(TranscriptEntry::UserTurn { text: "hi".into() })];
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
            TranscriptEntry::UserTurn {
                text: "fix the bug".into(),
            },
            TranscriptEntry::ToolCall(ToolCallEntry {
                id: "1".into(),
                name: "edit_file".into(),
                arguments_json: "{}".into(),
                result: Some(ToolCallResult {
                    content: "edited x.rs".into(),
                    is_error: false,
                }),
                expanded: true,
            }),
            TranscriptEntry::AssistantText {
                text: "done".into(),
            },
            TranscriptEntry::PermissionResolved {
                description: "run rm".into(),
                allowed: false,
            },
            TranscriptEntry::SystemNotice {
                text: "note".into(),
            },
        ];
        let json = serde_json::to_string(&entries).unwrap();
        let back: Vec<TranscriptEntry> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entries);
    }

    #[test]
    fn usage_summary_round_trips_through_json() {
        let usage = UsageSummary {
            input_tokens: 10,
            output_tokens: 5,
            estimated_cost: 0.01,
        };
        let json = serde_json::to_string(&usage).unwrap();
        let back: UsageSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(back, usage);
    }
}
