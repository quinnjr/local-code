use ntui::props::{FlexDirection, JustifyContent};
use ntui::{component, element};

use crate::tui::components::status_indicator::{StatusIndicator, StatusIndicatorProps};
use crate::tui::state::UsageSummary;

#[derive(Clone, PartialEq, Default)]
pub struct FooterProps {
    pub usage: UsageSummary,
    pub streaming: bool,
}

/// Bottom status line: quick hints (`/model`, ctrl+a auto-accept, ctrl+c
/// exit) plus token usage. The hint list is always-visible UI, per the
/// spec's footer note.
#[component]
pub fn Footer(props: &FooterProps, hooks: &mut ntui::Hooks) -> ntui::Element {
    let theme = hooks.use_theme();
    let tokens = format!(
        "{} in / {} out",
        props.usage.input_tokens, props.usage.output_tokens
    );
    element! {
        View(flex_direction: FlexDirection::Row, justify_content: JustifyContent::SpaceBetween, padding: 0) {
            Text(content: "/model · ctrl+a auto-accept · ctrl+c exit", color: theme.muted)
            StatusIndicator(streaming: props.streaming)
            Text(content: tokens, color: theme.muted)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntui::Element;
    use ntui::testing::TestTerminal;

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
        let t = TestTerminal::new(80, 1, Element::component::<Footer>(props)).unwrap();
        let text = t.frame_text();
        assert!(text.contains("/model"));
        assert!(text.contains("● ready"));
        assert!(text.contains("120 in / 45 out"));
    }

    #[tokio::test]
    async fn shows_spinner_while_streaming() {
        let props = FooterProps {
            usage: UsageSummary::default(),
            streaming: true,
        };
        let t = TestTerminal::new(80, 1, Element::component::<Footer>(props)).unwrap();
        let text = t.frame_text();
        assert!(text.contains("generating…"));
        assert!(!text.contains("● ready"));
    }
}
