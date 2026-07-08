// src/permissions/gate.rs

use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::permissions::settings::PermissionSettings;
use crate::permissions::types::{
    classify_tool, PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier,
    ToolKind,
};

/// Result of [`PermissionGate::check`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    Allowed,
    /// Denied, with the reason/feedback to relay back to the model as the tool result.
    Denied(String),
}

/// The permission decision engine. Holds the current tier, the project/user
/// allow/deny list, per-session "don't ask again" state, and a pluggable
/// [`PermissionPrompter`]. Reused verbatim by the TUI phase (only the prompter
/// implementation changes).
pub struct PermissionGate {
    tier: Mutex<PermissionTier>,
    settings: PermissionSettings,
    session_allow: Mutex<HashSet<String>>,
    prompter: Arc<dyn PermissionPrompter>,
}

impl PermissionGate {
    pub fn new(
        tier: PermissionTier,
        settings: PermissionSettings,
        prompter: Arc<dyn PermissionPrompter>,
    ) -> Self {
        Self {
            tier: Mutex::new(tier),
            settings,
            session_allow: Mutex::new(HashSet::new()),
            prompter,
        }
    }

    pub async fn set_tier(&self, tier: PermissionTier) {
        *self.tier.lock().await = tier;
    }

    pub async fn tier(&self) -> PermissionTier {
        *self.tier.lock().await
    }

    /// Decides whether `tool_name` may execute with `arguments`. Read-only tools
    /// always return `Allowed`. Bash commands are checked against the always-deny
    /// list first (a hard boundary regardless of tier) and then the always-allow
    /// list (skips prompting regardless of tier). Otherwise the decision follows
    /// the current tier, prompting via [`PermissionPrompter`] when required.
    pub async fn check(&self, tool_name: &str, arguments: &serde_json::Value) -> CheckOutcome {
        let kind = classify_tool(tool_name);

        if kind == ToolKind::ReadOnly {
            return CheckOutcome::Allowed;
        }

        if kind == ToolKind::Bash
            && let Some(command) = arguments.get("command").and_then(|v| v.as_str())
        {
            // NOTE(security, v1 limitation): this is substring matching over the raw
            // command string, not a tokenized/parsed shell command. It is a best-effort
            // safety net, not a hard security boundary — it can be bypassed by an
            // adversarial or merely unlucky command string (e.g. extra whitespace,
            // reordered flags, or splitting `rm -rf` into `rm -r -f`). A more robust
            // (tokenized) matcher is a candidate for a future pass.
            if self
                .settings
                .always_deny
                .iter()
                .any(|rule| command.contains(rule.as_str()))
            {
                return CheckOutcome::Denied(format!(
                    "command matches an always-deny rule and was blocked: {command}"
                ));
            }
            if self
                .settings
                .always_allow
                .iter()
                .any(|rule| command.contains(rule.as_str()))
            {
                return CheckOutcome::Allowed;
            }
        }

        let tier = self.tier().await;
        match (tier, kind) {
            (PermissionTier::FullAuto, _) => CheckOutcome::Allowed,
            (PermissionTier::AutoAcceptEdits, ToolKind::Edit) => CheckOutcome::Allowed,
            _ => self.ask(tool_name, arguments).await,
        }
    }

    async fn ask(&self, tool_name: &str, arguments: &serde_json::Value) -> CheckOutcome {
        let key = session_key(tool_name, arguments);
        if self.session_allow.lock().await.contains(&key) {
            return CheckOutcome::Allowed;
        }

        let command_preview = arguments
            .get("command")
            .and_then(|v| v.as_str())
            .map(String::from);
        let description = describe_call(tool_name, arguments);
        let request = PermissionRequest {
            tool_name: tool_name.to_string(),
            description,
            command_preview,
        };

        match self.prompter.prompt(&request).await {
            PermissionDecision::Allow => CheckOutcome::Allowed,
            PermissionDecision::AllowAlwaysThisSession => {
                self.session_allow.lock().await.insert(key);
                CheckOutcome::Allowed
            }
            PermissionDecision::Deny { feedback } => CheckOutcome::Denied(feedback),
        }
    }
}

/// Builds the key used to cache a "don't ask again this session" approval so that
/// approving one specific call does not silently cover unrelated, potentially more
/// dangerous calls to the same tool. For `bash`, the key includes the exact command
/// string (approving `cargo test` must not also cover `rm -rf /`). For calls with a
/// `path` argument (`write_file`/`edit_file`), the key includes the path (approving
/// an edit to `foo.rs` must not also cover writing to `/etc/passwd`). Falls back to
/// just `tool_name` when neither field is present.
fn session_key(tool_name: &str, arguments: &serde_json::Value) -> String {
    if let Some(command) = arguments.get("command").and_then(|v| v.as_str()) {
        return format!("bash:{command}");
    }
    if let Some(path) = arguments.get("path").and_then(|v| v.as_str()) {
        return format!("{tool_name}:{path}");
    }
    tool_name.to_string()
}

fn describe_call(tool_name: &str, arguments: &serde_json::Value) -> String {
    match tool_name {
        "bash" => format!(
            "run shell command: {}",
            arguments.get("command").and_then(|v| v.as_str()).unwrap_or("")
        ),
        "write_file" => format!(
            "write file: {}",
            arguments.get("path").and_then(|v| v.as_str()).unwrap_or("")
        ),
        "edit_file" => format!(
            "edit file: {}",
            arguments.get("path").and_then(|v| v.as_str()).unwrap_or("")
        ),
        other => format!("call tool '{other}'"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn gate_with(tier: PermissionTier, decision: PermissionDecision) -> PermissionGate {
        PermissionGate::new(
            tier,
            PermissionSettings::default(),
            Arc::new(StubPrompter { decision }),
        )
    }

    #[tokio::test]
    async fn read_only_tools_never_prompt_even_in_ask_tier() {
        let gate = gate_with(
            PermissionTier::Ask,
            PermissionDecision::Deny {
                feedback: "should never be reached".into(),
            },
        );
        let outcome = gate.check("read_file", &serde_json::json!({"path": "x"})).await;
        assert_eq!(outcome, CheckOutcome::Allowed);
    }

    #[tokio::test]
    async fn full_auto_allows_bash_without_prompting() {
        let gate = gate_with(
            PermissionTier::FullAuto,
            PermissionDecision::Deny {
                feedback: "should never be reached".into(),
            },
        );
        let outcome = gate.check("bash", &serde_json::json!({"command": "ls"})).await;
        assert_eq!(outcome, CheckOutcome::Allowed);
    }

    #[tokio::test]
    async fn auto_accept_edits_allows_edit_but_still_prompts_bash() {
        let gate = gate_with(PermissionTier::AutoAcceptEdits, PermissionDecision::Allow);
        let edit_outcome = gate
            .check("write_file", &serde_json::json!({"path": "x", "content": "y"}))
            .await;
        assert_eq!(edit_outcome, CheckOutcome::Allowed);

        let gate_denying_bash = gate_with(
            PermissionTier::AutoAcceptEdits,
            PermissionDecision::Deny {
                feedback: "no".into(),
            },
        );
        let bash_outcome = gate_denying_bash
            .check("bash", &serde_json::json!({"command": "ls"}))
            .await;
        assert_eq!(bash_outcome, CheckOutcome::Denied("no".into()));
    }

    #[tokio::test]
    async fn ask_tier_denies_with_feedback() {
        let gate = gate_with(
            PermissionTier::Ask,
            PermissionDecision::Deny {
                feedback: "use a different approach".into(),
            },
        );
        let outcome = gate
            .check("edit_file", &serde_json::json!({"path": "x", "find": "a", "replace": "b"}))
            .await;
        assert_eq!(
            outcome,
            CheckOutcome::Denied("use a different approach".into())
        );
    }

    #[tokio::test]
    async fn allow_always_this_session_skips_future_prompts_for_the_same_command_only() {
        // A prompter that always answers AllowAlwaysThisSession, used to record the
        // approval for the first ("cargo test") command.
        let gate = gate_with(PermissionTier::Ask, PermissionDecision::AllowAlwaysThisSession);
        let first = gate
            .check("bash", &serde_json::json!({"command": "cargo test"}))
            .await;
        assert_eq!(first, CheckOutcome::Allowed);
        assert!(gate.session_allow.lock().await.contains("bash:cargo test"));

        // A *different* bash command must NOT be silently allowed by the cache
        // entry recorded for "cargo test" — it must still go through `ask` and
        // receive the prompter's actual (denying) decision.
        let gate_denying = PermissionGate::new(
            PermissionTier::Ask,
            PermissionSettings::default(),
            Arc::new(StubPrompter {
                decision: PermissionDecision::Deny {
                    feedback: "no".into(),
                },
            }),
        );
        gate_denying
            .session_allow
            .lock()
            .await
            .insert("bash:cargo test".to_string());

        // Same command as cached: still allowed from cache, prompter not consulted.
        let same_command = gate_denying
            .check("bash", &serde_json::json!({"command": "cargo test"}))
            .await;
        assert_eq!(same_command, CheckOutcome::Allowed);

        // Different, more dangerous command: must NOT leak the cached approval;
        // must go through the (denying) prompter instead.
        let different_command = gate_denying
            .check("bash", &serde_json::json!({"command": "rm -rf /tmp/x"}))
            .await;
        assert_eq!(different_command, CheckOutcome::Denied("no".into()));
    }

    #[tokio::test]
    async fn always_deny_list_blocks_regardless_of_tier() {
        let mut settings = PermissionSettings::default();
        settings.always_deny.push("rm -rf".into());
        let gate = PermissionGate::new(
            PermissionTier::FullAuto,
            settings,
            Arc::new(StubPrompter {
                decision: PermissionDecision::Allow,
            }),
        );
        let outcome = gate
            .check("bash", &serde_json::json!({"command": "rm -rf /tmp/x"}))
            .await;
        assert!(matches!(outcome, CheckOutcome::Denied(_)));
    }

    #[tokio::test]
    async fn always_allow_list_skips_prompt_in_ask_tier() {
        let mut settings = PermissionSettings::default();
        settings.always_allow.push("cargo test".into());
        let gate = PermissionGate::new(
            PermissionTier::Ask,
            settings,
            Arc::new(StubPrompter {
                decision: PermissionDecision::Deny {
                    feedback: "should never be reached".into(),
                },
            }),
        );
        let outcome = gate
            .check("bash", &serde_json::json!({"command": "cargo test --lib"}))
            .await;
        assert_eq!(outcome, CheckOutcome::Allowed);
    }
}
