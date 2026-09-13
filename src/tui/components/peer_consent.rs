use ntui::Element;
use ntui::props::FlexDirection;
use ntui::style::Weight;
use ntui::widgets::Theme;

use crate::peers::message::{PeerMessage, sanitize_peer_text};
use crate::tui::theme::{ChipBackground, ON_WARN, WARN, chip};

/// One inbound peer message awaiting the user's per-sender approval. `project_root`
/// is the sender's project path as recorded in the registry (best-effort), shown
/// so the user can tell who is asking.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingPeerRequest {
    pub message: PeerMessage,
    pub project_root: String,
}

/// Longest message preview rendered on the consent card; peer text is
/// untrusted and can be arbitrarily long, so the card is bounded.
const MAX_PREVIEW_CHARS: usize = 400;

/// Sanitizes and truncates a peer message for the consent card, appending `…`
/// when it was cut. Extracted so the bound is unit-testable without a terminal
/// (`ntui`'s `Text` clips rather than wraps, hiding a trailing ellipsis).
fn truncate_preview(text: &str) -> String {
    let sanitized = sanitize_peer_text(text);
    let mut preview: String = sanitized.chars().take(MAX_PREVIEW_CHARS).collect();
    if sanitized.chars().count() > MAX_PREVIEW_CHARS {
        preview.push('…');
    }
    preview
}

/// Renders an unapproved peer message as an inline card with a numbered
/// approve/dismiss choice, mirroring `render_permission_card` (a plain function
/// so `Transcript` can call it inline with the theme it resolved).
pub fn render_peer_consent_card(request: &PendingPeerRequest, theme: &Theme) -> Element {
    let choice = |n: &str, label: &str| {
        let n = n.to_string();
        let label = label.to_string();
        ntui::element! {
            View(flex_direction: FlexDirection::Row, gap: 1) {
                Text(content: n, color: theme.accent, weight: Weight::Bold)
                Text(content: label, color: theme.foreground)
            }
        }
    };

    let preview = truncate_preview(&request.message.text);
    // `from` and `project_root` both come from another process (the registry
    // written by the sender for the latter), so sanitize them before display.
    let from = sanitize_peer_text(&request.message.from);
    let project = if request.project_root.is_empty() {
        String::new()
    } else {
        format!(" ({})", sanitize_peer_text(&request.project_root))
    };

    ntui::element! {
        View(flex_direction: FlexDirection::Column, border_style: theme.border_style, border_color: WARN, padding: 1) {
            View(flex_direction: FlexDirection::Row, gap: 1) {
                #(vec![chip("PEER MESSAGE", ChipBackground::Flat(WARN), ON_WARN)])
                Text(content: format!("{from}{project} wants to send you a message"), color: WARN)
            }
            Text(content: preview, color: theme.foreground)
            #(vec![
                choice("1)", "Approve this sender for the session (their messages will interrupt you)"),
                choice("2)", "Dismiss this message"),
            ])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::local_code_theme;
    use ntui::testing::TestTerminal;

    fn request() -> PendingPeerRequest {
        PendingPeerRequest {
            message: PeerMessage::new(
                "bob-1234abcd".into(),
                "me-5678efgh".into(),
                "can you rebase my branch?".into(),
                6,
                true,
                "2026-09-13T00:00:00Z".into(),
            ),
            project_root: "/home/u/other".into(),
        }
    }

    #[tokio::test]
    async fn renders_sender_project_preview_and_two_choices() {
        let t = TestTerminal::new(
            90,
            9,
            render_peer_consent_card(&request(), &local_code_theme()),
        )
        .unwrap();
        let text = t.frame_text();
        assert!(text.contains("PEER MESSAGE"), "{text}");
        assert!(text.contains("bob-1234abcd"), "{text}");
        assert!(text.contains("/home/u/other"), "{text}");
        assert!(text.contains("rebase my branch"), "{text}");
        assert!(text.contains("1) Approve"), "{text}");
        assert!(text.contains("2) Dismiss"), "{text}");
    }

    #[tokio::test]
    async fn preview_drops_escape_sequences() {
        let mut r = request();
        r.message.text = "hi\x1b[2Jthere".into();
        let t =
            TestTerminal::new(90, 9, render_peer_consent_card(&r, &local_code_theme())).unwrap();
        assert!(!t.frame_text().contains('\x1b'));
    }

    #[test]
    fn preview_is_truncated_to_the_char_cap() {
        let preview = truncate_preview(&"Z".repeat(MAX_PREVIEW_CHARS + 50));
        assert!(preview.ends_with('…'), "{preview}");
        assert_eq!(preview.matches('Z').count(), MAX_PREVIEW_CHARS);

        // At or under the cap: no ellipsis, full text preserved.
        let short = truncate_preview(&"Z".repeat(MAX_PREVIEW_CHARS));
        assert!(!short.contains('…'));
        assert_eq!(short.matches('Z').count(), MAX_PREVIEW_CHARS);
    }
}
