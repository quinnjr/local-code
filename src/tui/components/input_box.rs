// src/tui/components/input_box.rs

use ntui::props::FlexDirection;
use ntui::style::{BorderStyle, Color};
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
/// current buffer text down each render.
#[component]
pub fn InputBox(props: &InputBoxProps, _hooks: &mut ntui::Hooks) -> ntui::Element {
    let prompt_color = if props.disabled {
        Color::DarkGrey
    } else {
        Color::White
    };
    let cursor = if props.disabled { "" } else { "▏" };
    element! {
        View(flex_direction: FlexDirection::Row, border_style: BorderStyle::Round, border_color: Color::DarkGrey, padding: 0) {
            Text(content: "> ", color: Color::Cyan)
            Text(content: format!("{}{}", props.buffer, cursor), color: prompt_color)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntui::testing::TestTerminal;
    use ntui::Element;

    #[tokio::test]
    async fn renders_current_buffer_with_cursor_when_enabled() {
        let props = InputBoxProps {
            buffer: "fix the bug".into(),
            disabled: false,
        };
        let t = TestTerminal::new(40, 3, Element::component::<InputBox>(props)).unwrap();
        assert!(t.frame_text().contains("fix the bug"));
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
