use ntui::widgets::{Spinner, SpinnerProps};
use ntui::{component, element};

#[derive(Clone, PartialEq, Default)]
pub struct StatusIndicatorProps {
    pub streaming: bool,
}

/// The shared turn-status readout — an animated spinner while a turn is
/// streaming, a green ready dot otherwise. One component so the dashboard
/// and footer can never drift on the glyphs, label, or color.
#[component]
pub fn StatusIndicator(props: &StatusIndicatorProps, hooks: &mut ntui::Hooks) -> ntui::Element {
    let theme = hooks.use_theme();
    if props.streaming {
        element! { Spinner(label: "generating…".to_string()) }
    } else {
        element! { Text(content: "● ready".to_string(), color: theme.success) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntui::Element;
    use ntui::testing::TestTerminal;

    #[tokio::test]
    async fn shows_ready_dot_when_idle() {
        let t = TestTerminal::new(
            20,
            1,
            Element::component::<StatusIndicator>(StatusIndicatorProps { streaming: false }),
        )
        .unwrap();
        assert!(t.frame_text().contains("● ready"));
    }

    #[tokio::test]
    async fn shows_spinner_while_streaming() {
        let t = TestTerminal::new(
            20,
            1,
            Element::component::<StatusIndicator>(StatusIndicatorProps { streaming: true }),
        )
        .unwrap();
        let text = t.frame_text();
        assert!(text.contains("generating…"));
        assert!(!text.contains("● ready"));
    }
}
