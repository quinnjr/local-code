use ntui::props::FlexDirection;
use ntui::{component, element};

#[derive(Clone, PartialEq, Default)]
pub struct InputBoxProps {
    pub buffer: String,
    /// Disabled (dimmed prompt, no cursor) while a turn is streaming — the
    /// spec's transcript still shows the box, just not accepting new input.
    pub disabled: bool,
}

/// Bottom, full-width input box. Purely presentational: `App` owns the actual
/// buffer `State` and all key handling (backspace/char/enter), and passes the
/// current buffer text down each render. The border doubles as a focus cue —
/// accent while accepting input, muted while a turn is streaming.
#[component]
pub fn InputBox(props: &InputBoxProps, hooks: &mut ntui::Hooks) -> ntui::Element {
    let theme = hooks.use_theme();
    let (border_color, prompt_color, text_color) = if props.disabled {
        (theme.border, theme.muted, theme.muted)
    } else {
        (theme.accent, theme.accent, theme.foreground)
    };
    let cursor = if props.disabled { "" } else { "▏" };
    element! {
        View(flex_direction: FlexDirection::Row, border_style: theme.border_style, border_color: border_color, padding: 0) {
            Text(content: "❯ ", color: prompt_color)
            Text(content: format!("{}{}", props.buffer, cursor), color: text_color)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntui::Element;
    use ntui::testing::TestTerminal;

    #[tokio::test]
    async fn renders_current_buffer_with_cursor_when_enabled() {
        let props = InputBoxProps {
            buffer: "fix the bug".into(),
            disabled: false,
        };
        let t = TestTerminal::new(40, 3, Element::component::<InputBox>(props)).unwrap();
        let text = t.frame_text();
        assert!(text.contains("fix the bug"));
        assert!(text.contains('❯'));
    }

    #[tokio::test]
    async fn omits_cursor_when_disabled() {
        let props = InputBoxProps {
            buffer: String::new(),
            disabled: true,
        };
        let t = TestTerminal::new(40, 3, Element::component::<InputBox>(props)).unwrap();
        assert!(!t.frame_text().contains('▏'));
    }
}
