// src/tui/permission_prompter.rs

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use ntui::State;
use tokio::sync::oneshot;

use crate::permissions::types::{PermissionDecision, PermissionPrompter, PermissionRequest};

/// Renders a [`PermissionRequest`] as an inline card in the transcript instead
/// of over stdio. Implements the exact same [`PermissionPrompter`] trait
/// Phase 2's `StdioPrompter` implements, so [`crate::permissions::gate::PermissionGate`]
/// needs no changes at all to work with either.
///
/// `pending` is an `ntui::State` handle shared with the `App` component: setting
/// it (via `State::set`) marks `App`'s fiber dirty, so the next render shows the
/// permission card. The `responder` channel is how the UI (a keypress handler
/// reading `1`/`2`/`3`) sends the user's decision back to the `.await` inside
/// [`PermissionPrompter::prompt`].
pub struct NtuiPermissionPrompter {
    pending: State<Option<PermissionRequest>>,
    responder: Arc<Mutex<Option<oneshot::Sender<PermissionDecision>>>>,
}

impl NtuiPermissionPrompter {
    pub fn new(pending: State<Option<PermissionRequest>>) -> Self {
        Self {
            pending,
            responder: Arc::new(Mutex::new(None)),
        }
    }

    /// A clone of the responder slot, for the input handler that reads the
    /// user's numbered choice to call `respond` through.
    pub fn responder_handle(&self) -> Arc<Mutex<Option<oneshot::Sender<PermissionDecision>>>> {
        self.responder.clone()
    }

    /// Sends `decision` to whichever `prompt()` call is currently pending, if
    /// any. A no-op if nothing is pending (e.g. a stray keypress after the
    /// prompt already resolved) — returns `false` in that case.
    pub fn respond(
        responder: &Arc<Mutex<Option<oneshot::Sender<PermissionDecision>>>>,
        decision: PermissionDecision,
    ) -> bool {
        let sender = responder.lock().unwrap_or_else(|p| p.into_inner()).take();
        match sender {
            Some(tx) => tx.send(decision).is_ok(),
            None => false,
        }
    }
}

impl PermissionPrompter for NtuiPermissionPrompter {
    fn prompt<'a>(
        &'a self,
        request: &'a PermissionRequest,
    ) -> Pin<Box<dyn Future<Output = PermissionDecision> + Send + 'a>> {
        Box::pin(async move {
            let (tx, rx) = oneshot::channel();
            *self.responder.lock().unwrap_or_else(|p| p.into_inner()) = Some(tx);
            self.pending.set(Some(request.clone()));
            let decision = rx.await.unwrap_or(PermissionDecision::Deny {
                feedback: "permission prompt was dismissed".into(),
            });
            self.pending.set(None);
            decision
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntui::testing::TestTerminal;
    use ntui::{Element, component, element};

    // `PermissionGate::check` is driven through a spawned task so this test
    // observes the prompter's *async* boundary the same way the real TUI
    // will: `pending` becomes visible before the gate's decision resolves,
    // and `resolved` only updates once a decision is sent back through the
    // responder (which this test never does — it only asserts the pending
    // half, matching this task's scope; end-to-end responder wiring through
    // an input handler is Task 7's job).
    #[component]
    fn Harness(hooks: &mut ntui::Hooks) -> Element {
        use crate::permissions::gate::{CheckOutcome, PermissionGate};
        use crate::permissions::settings::PermissionSettings;
        use crate::permissions::types::PermissionTier;

        let pending = hooks.use_state(|| Option::<PermissionRequest>::None);
        let resolved = hooks.use_state(|| Option::<PermissionDecision>::None);

        hooks.use_effect((), {
            let pending = pending.clone();
            let resolved = resolved.clone();
            move || {
                let prompter = NtuiPermissionPrompter::new(pending);
                tokio::spawn(async move {
                    let gate = PermissionGate::new(
                        PermissionTier::Ask,
                        PermissionSettings::default(),
                        Arc::new(prompter),
                    );
                    let outcome = gate
                        .check("bash", &serde_json::json!({"command": "rm x"}))
                        .await;
                    let decision = match outcome {
                        CheckOutcome::Allowed => PermissionDecision::Allow,
                        CheckOutcome::Denied(feedback) => PermissionDecision::Deny { feedback },
                    };
                    resolved.set(Some(decision));
                });
            }
        });

        let pending_text = match pending.get() {
            Some(req) => format!("PENDING: {}", req.description),
            None => "NONE".to_string(),
        };
        let resolved_text = match resolved.get() {
            Some(PermissionDecision::Allow) => "ALLOW".to_string(),
            Some(PermissionDecision::AllowAlwaysThisSession) => "ALLOW_ALWAYS".to_string(),
            Some(PermissionDecision::Deny { feedback }) => format!("DENY: {feedback}"),
            None => "UNRESOLVED".to_string(),
        };

        element! {
            View {
                Text(content: format!("{pending_text} | {resolved_text}"))
            }
        }
    }

    #[tokio::test]
    async fn prompt_marks_pending_state_visible_to_the_ui() {
        let mut t = TestTerminal::new(60, 1, Element::component::<Harness>(())).unwrap();
        t.tick().await.unwrap();
        assert!(
            t.frame_text().contains("PENDING:"),
            "expected a pending permission request to be visible: {}",
            t.frame_text()
        );
    }
}
