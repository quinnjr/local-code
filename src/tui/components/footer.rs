// src/tui/components/footer.rs

use ntui::props::{FlexDirection, JustifyContent};
use ntui::style::Color;
use ntui::{component, element};

use crate::tui::state::UsageSummary;

#[derive(Clone, PartialEq, Default)]
pub struct FooterProps {
    pub usage: UsageSummary,
    pub streaming: bool,
}

/// Bottom status line: quick hints plus token usage, per the spec. `/model` and
/// auto-accept-toggle hints are listed even though the keys aren't wired up
/// until Phase 4 — the footer is allowed to advertise the hint text now since
/// the spec calls the footer's hint list out explicitly as always-visible UI,
/// not as a slash-command implementation.
#[component]
pub fn Footer(props: &FooterProps, _hooks: &mut ntui::Hooks) -> ntui::Element {
    let status = if props.streaming { "generating…" } else { "ready" };
    let tokens = format!(
        "{} in / {} out",
        props.usage.input_tokens, props.usage.output_tokens
    );
    element! {
        View(flex_direction: FlexDirection::Row, justify_content: JustifyContent::SpaceBetween, padding: 0) {
            Text(content: "/model  ctrl+a auto-accept  ctrl+c exit", color: Color::DarkGrey)
            Text(content: status.to_string(), color: Color::DarkGrey)
            Text(content: tokens, color: Color::DarkGrey)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntui::testing::TestTerminal;
    use ntui::Element;

    #[tokio::test]
    async fn renders_hints_status_and_token_usage() {
        let props = FooterProps {
            usage: UsageSummary {
                input_tokens: 120,
                output_tokens: 45,
                estimated_cost: 0.0,
            },
            streaming: false,
        };
        let t = TestTerminal::new(60, 1, Element::component::<Footer>(props)).unwrap();
        let text = t.frame_text();
        assert!(text.contains("/model"));
        assert!(text.contains("ready"));
        assert!(text.contains("120 in / 45 out"));
    }

    #[tokio::test]
    async fn shows_generating_status_while_streaming() {
        let props = FooterProps {
            usage: UsageSummary::default(),
            streaming: true,
        };
        let t = TestTerminal::new(60, 1, Element::component::<Footer>(props)).unwrap();
        assert!(t.frame_text().contains("generating…"));
    }
}
