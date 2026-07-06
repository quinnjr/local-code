// src/tui/components/header.rs

use ntui::props::{FlexDirection, JustifyContent};
use ntui::style::Color;
use ntui::{component, element};

#[derive(Clone, PartialEq, Default)]
pub struct HeaderProps {
    pub connection_name: String,
    pub model_name: String,
    pub tier_label: String,
}

/// Always-visible top bar: connection, model, and permission mode. Pure/stateless
/// — a plain data-in, tree-out component, per the spec's "always visible" header.
#[component]
pub fn Header(props: &HeaderProps, _hooks: &mut ntui::Hooks) -> ntui::Element {
    element! {
        View(flex_direction: FlexDirection::Row, justify_content: JustifyContent::SpaceBetween, padding: 0) {
            Text(content: format!("local-code · {}", props.connection_name), color: Color::Cyan)
            Text(content: props.model_name.clone(), color: Color::White)
            Text(content: format!("[{}]", props.tier_label), color: Color::Yellow)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntui::testing::TestTerminal;
    use ntui::Element;

    #[tokio::test]
    async fn renders_connection_model_and_tier() {
        let props = HeaderProps {
            connection_name: "local-vllm".into(),
            model_name: "qwen2.5-coder-32b".into(),
            tier_label: "ask".into(),
        };
        let t = TestTerminal::new(60, 1, Element::component::<Header>(props)).unwrap();
        let text = t.frame_text();
        assert!(text.contains("local-vllm"));
        assert!(text.contains("qwen2.5-coder-32b"));
        assert!(text.contains("[ask]"));
    }
}
