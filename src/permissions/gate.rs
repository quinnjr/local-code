use std::collections::HashSet;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::permissions::settings::PermissionSettings;
use crate::permissions::types::{
    PermissionDecision, PermissionPrompter, PermissionRequest, PermissionTier, ToolKind,
    classify_tool,
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
    /// Set while an injected peer turn is running. When true, only read-only
    /// tools are permitted (see [`PermissionGate::check`]). A plain
    /// `std::sync::Mutex` (not `tokio::sync::Mutex`) because it is toggled
    /// synchronously around a turn and is never held across an `await`.
    peer_restricted: std::sync::Mutex<bool>,
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
            peer_restricted: std::sync::Mutex::new(false),
        }
    }

    pub async fn set_tier(&self, tier: PermissionTier) {
        *self.tier.lock().await = tier;
    }

    pub async fn tier(&self) -> PermissionTier {
        *self.tier.lock().await
    }

    /// Marks whether the gate is currently enforcing the peer-initiated-turn
    /// restriction (read-only tools only). Set before an injected peer turn and
    /// cleared when it finishes.
    pub fn set_peer_restricted(&self, restricted: bool) {
        *self
            .peer_restricted
            .lock()
            .expect("peer_restricted mutex poisoned") = restricted;
    }

    /// Decides whether `tool_name` may execute with `arguments`. Read-only tools
    /// always return `Allowed`. Bash commands are checked against the always-deny
    /// list first (a hard boundary regardless of tier) and then the always-allow
    /// list (skips prompting regardless of tier). Otherwise the decision follows
    /// the current tier, prompting via [`PermissionPrompter`] when required.
    pub async fn check(&self, tool_name: &str, arguments: &serde_json::Value) -> CheckOutcome {
        let kind = classify_tool(tool_name);

        // Peer-initiated turns are read-only. This runs before the tier match so
        // the restriction holds even under `FullAuto`, where a peer turn would
        // otherwise be able to run bash/write/send tools with no prompting.
        if *self
            .peer_restricted
            .lock()
            .expect("peer_restricted mutex poisoned")
        {
            return match kind {
                ToolKind::ReadOnly => CheckOutcome::Allowed,
                _ => {
                    CheckOutcome::Denied("peer-initiated turns may only use read-only tools".into())
                }
            };
        }

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
        // A `send_message` with no `to` is a reply to whichever peer message is
        // currently being handled. Replies are deliberately allow-once: caching
        // one reply approval under `send_message:<reply>` would silently approve
        // every future reply for the rest of the session (and there is no single
        // target to scope `AllowAlwaysThisSession` to), so the session allow-set
        // is neither consulted nor updated for them — every reply prompts.
        let is_reply = is_reply(tool_name, arguments);

        let key = session_key(tool_name, arguments);
        if !is_reply && self.session_allow.lock().await.contains(&key) {
            return CheckOutcome::Allowed;
        }

        let request = PermissionRequest {
            description: describe_call(tool_name, arguments),
        };

        match self.prompter.prompt(&request).await {
            PermissionDecision::Allow => CheckOutcome::Allowed,
            PermissionDecision::AllowAlwaysThisSession => {
                if !is_reply {
                    self.session_allow.lock().await.insert(key);
                }
                CheckOutcome::Allowed
            }
            PermissionDecision::Deny { feedback } => CheckOutcome::Denied(feedback),
        }
    }
}

/// Whether a `send_message` call is a reply — i.e. it carries no explicit `to`
/// target. Kept as a single helper so `ask`'s "never cache replies" rule and
/// `session_key`'s `<reply>` scope can't diverge; in particular `"to": null`
/// counts as a reply in both.
fn is_reply(tool_name: &str, arguments: &serde_json::Value) -> bool {
    tool_name == "send_message" && arguments.get("to").and_then(|v| v.as_str()).is_none()
}

/// Builds the key used to cache a "don't ask again this session" approval so that
/// approving one specific call does not silently cover unrelated, potentially more
/// dangerous calls to the same tool. For `bash`, the key includes the exact command
/// string (approving `cargo test` must not also cover `rm -rf /`). For calls with a
/// `path` argument (`write_file`/`edit_file`), the key includes the path (approving
/// an edit to `foo.rs` must not also cover writing to `/etc/passwd`). Falls back to
/// just `tool_name` when neither field is present.
fn session_key(tool_name: &str, arguments: &serde_json::Value) -> String {
    // Messaging is scoped to its target: approving "send to bob" must not
    // whitelist sending to every peer for the rest of the session.
    if tool_name == "send_message" {
        return if is_reply(tool_name, arguments) {
            "send_message:<reply>".to_string()
        } else {
            let to = arguments
                .get("to")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            format!("send_message:{to}")
        };
    }
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
            arguments
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
        ),
        "write_file" => format!(
            "write file: {}",
            arguments.get("path").and_then(|v| v.as_str()).unwrap_or("")
        ),
        "edit_file" => format!(
            "edit file: {}",
            arguments.get("path").and_then(|v| v.as_str()).unwrap_or("")
        ),
        "send_message" => {
            let preview = argument_preview(arguments, "text", 80);
            match arguments.get("to").and_then(|v| v.as_str()) {
                Some(target) => format!("send a message to peer {target}: {preview}"),
                None => format!("reply to a peer: {preview}"),
            }
        }
        other => format!("call tool '{other}'"),
    }
}

/// First `max` characters of a string tool argument, suffixed with an ellipsis
/// when the value was truncated. A missing or non-string argument renders as an
/// empty string rather than panicking or leaking a raw JSON blob into the prompt.
fn argument_preview(arguments: &serde_json::Value, key: &str, max: usize) -> String {
    let text = arguments.get(key).and_then(|v| v.as_str()).unwrap_or("");
    if text.chars().count() > max {
        let mut out: String = text.chars().take(max).collect();
        out.push('…');
        out
    } else {
        text.to_string()
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

    #[test]
    fn session_key_scopes_send_message_to_its_target() {
        assert_eq!(
            session_key(
                "send_message",
                &serde_json::json!({"to": "bob-1", "text": "x"})
            ),
            "send_message:bob-1"
        );
        assert_ne!(
            session_key("send_message", &serde_json::json!({"to": "bob-1"})),
            session_key("send_message", &serde_json::json!({"to": "carol-1"}))
        );
        // No `to` (a reply) is a distinct scope from any explicit target.
        assert_eq!(
            session_key("send_message", &serde_json::json!({"text": "x"})),
            "send_message:<reply>"
        );
    }

    #[test]
    fn explicit_null_to_counts_as_a_reply_in_both_predicate_and_key() {
        // `"to": null` must be treated as a reply by the caching predicate and
        // scoped to `<reply>` by the session key, matching how the runtime/tool
        // treat a missing `to`. Before G4-2 the predicate saw `Some(Null)` and
        // the key saw `<reply>`, so the two disagreed.
        assert!(is_reply("send_message", &serde_json::json!({"to": null})));
        assert!(is_reply("send_message", &serde_json::json!({"text": "x"})));
        assert!(!is_reply(
            "send_message",
            &serde_json::json!({"to": "bob-1"})
        ));
        assert!(!is_reply("bash", &serde_json::json!({"to": null})));

        assert_eq!(
            session_key("send_message", &serde_json::json!({"to": null})),
            "send_message:<reply>"
        );
        assert_eq!(
            session_key("send_message", &serde_json::json!({"to": "bob-1"})),
            "send_message:bob-1"
        );
    }

    #[test]
    fn describe_call_shows_the_send_message_target_and_body_preview() {
        let described = describe_call(
            "send_message",
            &serde_json::json!({"to": "bob-1", "text": "hello"}),
        );
        assert!(described.contains("bob-1"), "{described}");
        assert!(described.contains("hello"), "{described}");

        let reply = describe_call("send_message", &serde_json::json!({"text": "a reply"}));
        assert!(reply.contains("reply to a peer"), "{reply}");
        assert!(reply.contains("a reply"), "{reply}");

        // Missing `text` must not panic and must not dump raw JSON.
        let missing = describe_call("send_message", &serde_json::json!({"to": "carol-1"}));
        assert!(missing.contains("carol-1"), "{missing}");
    }

    #[test]
    fn describe_call_truncates_a_long_send_message_body() {
        let long = "x".repeat(200);
        let described = describe_call("send_message", &serde_json::json!({"text": long}));
        assert!(described.contains('…'), "{described}");
        // 80 preview chars plus the label, comfortably under the source length.
        assert!(described.chars().count() < 120, "{described}");
    }

    #[tokio::test]
    async fn read_only_tools_never_prompt_even_in_ask_tier() {
        let gate = gate_with(
            PermissionTier::Ask,
            PermissionDecision::Deny {
                feedback: "should never be reached".into(),
            },
        );
        let outcome = gate
            .check("read_file", &serde_json::json!({"path": "x"}))
            .await;
        assert_eq!(outcome, CheckOutcome::Allowed);
    }

    #[tokio::test]
    async fn reply_approvals_are_never_cached_for_the_session() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingPrompter {
            calls: AtomicUsize,
            decision: PermissionDecision,
        }
        impl PermissionPrompter for CountingPrompter {
            fn prompt<'a>(
                &'a self,
                _request: &'a PermissionRequest,
            ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let decision = self.decision.clone();
                Box::pin(async move { decision })
            }
        }

        let prompter = Arc::new(CountingPrompter {
            calls: AtomicUsize::new(0),
            decision: PermissionDecision::AllowAlwaysThisSession,
        });
        let gate = PermissionGate::new(
            PermissionTier::Ask,
            PermissionSettings::default(),
            prompter.clone(),
        );

        // First reply: approved (AllowAlwaysThisSession), but replies are
        // allow-once, so nothing may be cached.
        let first = gate
            .check("send_message", &serde_json::json!({"text": "one"}))
            .await;
        assert_eq!(first, CheckOutcome::Allowed);
        assert!(
            gate.session_allow.lock().await.is_empty(),
            "a reply approval must not enter the session allow-set"
        );

        // A second reply must prompt again, not ride the first approval.
        let second = gate
            .check("send_message", &serde_json::json!({"text": "two"}))
            .await;
        assert_eq!(second, CheckOutcome::Allowed);
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            2,
            "every reply must be prompted, even after an earlier AllowAlwaysThisSession"
        );
    }

    #[tokio::test]
    async fn full_auto_allows_bash_without_prompting() {
        let gate = gate_with(
            PermissionTier::FullAuto,
            PermissionDecision::Deny {
                feedback: "should never be reached".into(),
            },
        );
        let outcome = gate
            .check("bash", &serde_json::json!({"command": "ls"}))
            .await;
        assert_eq!(outcome, CheckOutcome::Allowed);
    }

    #[tokio::test]
    async fn peer_restricted_gate_allows_read_only_and_denies_writes_even_in_full_auto() {
        // FullAuto so it is the restriction, not the tier, that denies.
        let gate = gate_with(PermissionTier::FullAuto, PermissionDecision::Allow);
        gate.set_peer_restricted(true);

        let read = gate
            .check("read_file", &serde_json::json!({"path": "x"}))
            .await;
        assert_eq!(read, CheckOutcome::Allowed);
        let list = gate.check("list_peers", &serde_json::json!({})).await;
        assert_eq!(list, CheckOutcome::Allowed);

        for (tool, args) in [
            ("bash", serde_json::json!({"command": "ls"})),
            (
                "write_file",
                serde_json::json!({"path": "x", "content": "y"}),
            ),
            (
                "send_message",
                serde_json::json!({"to": "bob-1", "text": "hi"}),
            ),
        ] {
            let outcome = gate.check(tool, &args).await;
            assert_eq!(
                outcome,
                CheckOutcome::Denied("peer-initiated turns may only use read-only tools".into()),
                "{tool} must be denied while peer-restricted, even at FullAuto"
            );
        }
    }

    #[tokio::test]
    async fn clearing_peer_restriction_restores_the_prior_tier_behavior() {
        let gate = gate_with(PermissionTier::FullAuto, PermissionDecision::Allow);
        gate.set_peer_restricted(true);
        assert_eq!(
            gate.check("bash", &serde_json::json!({"command": "ls"}))
                .await,
            CheckOutcome::Denied("peer-initiated turns may only use read-only tools".into())
        );

        gate.set_peer_restricted(false);
        assert_eq!(
            gate.check("bash", &serde_json::json!({"command": "ls"}))
                .await,
            CheckOutcome::Allowed,
            "clearing the restriction must restore the prior tier behavior"
        );
    }

    #[tokio::test]
    async fn auto_accept_edits_allows_edit_but_still_prompts_bash() {
        let gate = gate_with(PermissionTier::AutoAcceptEdits, PermissionDecision::Allow);
        let edit_outcome = gate
            .check(
                "write_file",
                &serde_json::json!({"path": "x", "content": "y"}),
            )
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
            .check(
                "edit_file",
                &serde_json::json!({"path": "x", "find": "a", "replace": "b"}),
            )
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
        let gate = gate_with(
            PermissionTier::Ask,
            PermissionDecision::AllowAlwaysThisSession,
        );
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

    #[tokio::test]
    async fn a_session_approval_for_one_target_does_not_cover_another_target() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct CountingPrompter {
            calls: AtomicUsize,
            decision: PermissionDecision,
        }
        impl PermissionPrompter for CountingPrompter {
            fn prompt<'a>(
                &'a self,
                _request: &'a PermissionRequest,
            ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                let decision = self.decision.clone();
                Box::pin(async move { decision })
            }
        }

        let prompter = Arc::new(CountingPrompter {
            calls: AtomicUsize::new(0),
            decision: PermissionDecision::AllowAlwaysThisSession,
        });
        let gate = PermissionGate::new(
            PermissionTier::Ask,
            PermissionSettings::default(),
            prompter.clone(),
        );

        // First send to bob-1 prompts and is cached under that target.
        let first = gate
            .check(
                "send_message",
                &serde_json::json!({"to": "bob-1", "text": "one"}),
            )
            .await;
        assert_eq!(first, CheckOutcome::Allowed);
        assert_eq!(prompter.calls.load(Ordering::SeqCst), 1);

        // A second send to the same target rides the cached approval.
        let second = gate
            .check(
                "send_message",
                &serde_json::json!({"to": "bob-1", "text": "two"}),
            )
            .await;
        assert_eq!(second, CheckOutcome::Allowed);
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            1,
            "a repeated send to an approved target must not prompt again"
        );

        // A different target is a different scope and must prompt.
        let third = gate
            .check(
                "send_message",
                &serde_json::json!({"to": "carol-1", "text": "three"}),
            )
            .await;
        assert_eq!(third, CheckOutcome::Allowed);
        assert_eq!(
            prompter.calls.load(Ordering::SeqCst),
            2,
            "approving bob-1 must not silently approve carol-1"
        );
    }
}
