// src/agent/gated_tool.rs

use std::sync::Arc;

use daimon::tool::{Tool, ToolOutput};

use crate::permissions::gate::{CheckOutcome, PermissionGate};

/// Wraps any `daimon::tool::Tool` so its own `execute` consults a
/// [`PermissionGate`] before doing real work. Both `daimon::agent::Agent::prompt`
/// and `Agent::prompt_stream` call a tool's `execute`/`execute_erased` to run it,
/// so embedding the check here (rather than in a `daimon::middleware::Middleware`,
/// which `prompt_stream` never invokes — confirmed by reading
/// `daimon-0.16.0/src/agent/runner.rs`) makes permission enforcement work
/// identically no matter which of the two the caller uses. This is the single
/// enforcement mechanism for the whole project: headless mode (this phase's
/// `build_agent`/`register_all_tools`), the TUI (a later phase's `build_streaming_agent`),
/// and MCP tools (a later phase's `NamespacedMcpTool`, wrapped exactly like a built-in)
/// all wrap every tool in `GatedTool` before registering it.
pub struct GatedTool<T> {
    inner: T,
    gate: Arc<PermissionGate>,
}

impl<T: Tool> GatedTool<T> {
    pub fn new(inner: T, gate: Arc<PermissionGate>) -> Self {
        Self { inner, gate }
    }
}

impl<T: Tool> Tool for GatedTool<T> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.inner.parameters_schema()
    }

    async fn execute(&self, input: &serde_json::Value) -> daimon::Result<ToolOutput> {
        match self.gate.check(self.inner.name(), input).await {
            CheckOutcome::Allowed => self.inner.execute(input).await,
            CheckOutcome::Denied(reason) => Ok(ToolOutput::error(reason)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::tools::{Bash, ReadFile, WriteFile};
    use crate::permissions::settings::PermissionSettings;
    use crate::permissions::types::{
        PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier,
    };
    use std::future::Future;
    use std::pin::Pin;

    struct StubPrompter {
        decision: PermissionDecision,
    }
    impl PermissionPrompter for StubPrompter {
        fn prompt<'a>(
            &'a self,
            _request: &'a PermissionRequest,
        ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
            let decision = self.decision.clone();
            Box::pin(async move { decision })
        }
    }

    fn gate_with(tier: PermissionTier, decision: PermissionDecision) -> Arc<PermissionGate> {
        Arc::new(PermissionGate::new(
            tier,
            PermissionSettings::default(),
            Arc::new(StubPrompter { decision }),
        ))
    }

    #[tokio::test]
    async fn read_only_gated_tool_never_prompts_and_executes() {
        let gate = gate_with(
            PermissionTier::Ask,
            PermissionDecision::Deny {
                feedback: "should never be reached".into(),
            },
        );
        let tool = GatedTool::new(ReadFile, gate);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("x.txt");
        std::fs::write(&file, "hello").unwrap();
        let output = tool
            .execute(&serde_json::json!({"path": file.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert_eq!(output.content, "hello");
    }

    #[tokio::test]
    async fn denied_edit_tool_never_touches_the_filesystem() {
        let gate = gate_with(
            PermissionTier::Ask,
            PermissionDecision::Deny {
                feedback: "no thanks".into(),
            },
        );
        let tool = GatedTool::new(WriteFile, gate);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("out.txt");
        let output = tool
            .execute(&serde_json::json!({
                "path": file.to_str().unwrap(),
                "content": "should not be written"
            }))
            .await
            .unwrap();
        assert!(output.is_error);
        assert_eq!(output.content, "no thanks");
        assert!(!file.exists());
    }

    #[tokio::test]
    async fn allowed_bash_tool_executes() {
        let gate = gate_with(PermissionTier::FullAuto, PermissionDecision::Allow);
        let tool = GatedTool::new(Bash, gate);
        let output = tool
            .execute(&serde_json::json!({"command": "echo gated_ok"}))
            .await
            .unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("gated_ok"));
    }

    #[tokio::test]
    async fn denied_mcp_shaped_tool_call_never_reaches_the_wrapped_tool() {
        // Proves MCP tool calls are gated through the exact same mechanism as
        // built-ins, not a separate (and therefore possibly-bypassable) path.
        // A namespaced-style name (`fixture__dangerous`) falls through
        // `classify_tool`'s `_ => ToolKind::Edit` default (locked down
        // separately in `src/permissions/types.rs`), so the `Ask`-tier gate
        // below prompts for it — denying that prompt must mean the wrapped
        // tool's `execute` body never runs at all.
        struct PanicsIfCalled;
        impl Tool for PanicsIfCalled {
            fn name(&self) -> &str {
                "fixture__dangerous"
            }
            fn description(&self) -> &str {
                "would do something irreversible if actually called"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(&self, _input: &serde_json::Value) -> daimon::Result<ToolOutput> {
                panic!("must never be reached when the gate denies the call")
            }
        }

        let gate = gate_with(
            PermissionTier::Ask,
            PermissionDecision::Deny {
                feedback: "no".into(),
            },
        );
        let tool = GatedTool::new(PanicsIfCalled, gate);
        let output = tool.execute(&serde_json::json!({})).await.unwrap();
        assert!(output.is_error);
        assert_eq!(output.content, "no");
    }
}
