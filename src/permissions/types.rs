use std::future::Future;
use std::pin::Pin;

/// How aggressively the agent may act without asking the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionTier {
    /// Every write/edit/bash call prompts (default).
    #[default]
    Ask,
    /// File writes/edits auto-approved; bash still prompts.
    AutoAcceptEdits,
    /// Nothing prompts.
    FullAuto,
}

/// Coarse classification of a tool call for permission purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    /// Never mutates state, never prompts (`read_file`, `grep`, `glob`).
    ReadOnly,
    /// Mutates the filesystem (`write_file`, `edit_file`).
    Edit,
    /// Executes an arbitrary shell command (`bash`).
    Bash,
}

/// Classifies a tool by name for permission enforcement.
///
/// Unknown tool names (e.g. future MCP-provided tools) intentionally classify as
/// [`ToolKind::Edit`] rather than [`ToolKind::ReadOnly`] — the safe default is to
/// prompt for anything we don't explicitly know is read-only.
pub fn classify_tool(name: &str) -> ToolKind {
    match name {
        "read_file" | "grep" | "glob" => ToolKind::ReadOnly,
        "write_file" | "edit_file" => ToolKind::Edit,
        "bash" => ToolKind::Bash,
        _ => ToolKind::Edit,
    }
}

/// A human-readable description of a pending tool call, shown to the user by
/// whatever [`PermissionPrompter`] is in use.
#[derive(Debug, Clone, PartialEq)]
pub struct PermissionRequest {
    pub description: String,
}

/// What the user decided in response to a [`PermissionRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    AllowAlwaysThisSession,
    Deny { feedback: String },
}

/// Renders a [`PermissionRequest`] to the user and returns their decision.
///
/// Kept separate from [`crate::permissions::gate::PermissionGate`]'s decision logic
/// so the TUI phase can supply an `ntui`-rendering implementation without touching
/// the gate at all. Uses a boxed future (rather than an `impl Future` return, which
/// would not be object-safe) so implementations can be stored behind `Arc<dyn
/// PermissionPrompter>`.
pub trait PermissionPrompter: Send + Sync {
    fn prompt<'a>(
        &'a self,
        request: &'a PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_tools_classify_as_read_only() {
        assert_eq!(classify_tool("read_file"), ToolKind::ReadOnly);
        assert_eq!(classify_tool("grep"), ToolKind::ReadOnly);
        assert_eq!(classify_tool("glob"), ToolKind::ReadOnly);
    }

    #[test]
    fn write_tools_classify_as_edit() {
        assert_eq!(classify_tool("write_file"), ToolKind::Edit);
        assert_eq!(classify_tool("edit_file"), ToolKind::Edit);
    }

    #[test]
    fn bash_classifies_as_bash() {
        assert_eq!(classify_tool("bash"), ToolKind::Bash);
    }

    #[test]
    fn unknown_tool_defaults_to_edit() {
        assert_eq!(classify_tool("some_future_mcp_tool"), ToolKind::Edit);
    }

    #[test]
    fn namespaced_mcp_tool_names_default_to_edit_not_read_only() {
        // Mirrors the `{server_name}__{tool_name}` shape produced by
        // `local_code::mcp::tool::NamespacedMcpTool::new` — asserting here
        // (rather than only in `src/mcp/tool.rs`) keeps the permission
        // default visible from the permissions module itself, since that is
        // what a future edit to `classify_tool` is most likely to touch.
        assert_eq!(classify_tool("filesystem__write_file"), ToolKind::Edit);
        assert_eq!(classify_tool("filesystem__read_file"), ToolKind::Edit);
        assert_eq!(
            classify_tool("some_remote_server__delete_everything"),
            ToolKind::Edit
        );
    }

    #[test]
    fn permission_tier_round_trips_through_json() {
        let tier = PermissionTier::AutoAcceptEdits;
        let json = serde_json::to_string(&tier).unwrap();
        assert_eq!(json, "\"auto-accept-edits\"");
        let back: PermissionTier = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tier);
    }
}
