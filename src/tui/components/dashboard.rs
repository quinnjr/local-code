use ntui::props::{Dimension, FlexDirection, JustifyContent, TextWrap};
use ntui::style::{BorderStyle, Color};
use ntui::{component, element};

use crate::tui::state::UsageSummary;

#[derive(Clone, PartialEq, Default)]
pub struct DashboardProps {
    pub connection_name: String,
    pub model_name: String,
    pub tier_label: String,
    pub usage: UsageSummary,
    pub streaming: bool,
    pub session_path: String,
    pub created_at: String,
    pub project_root: String,
}

/// Always-visible top dashboard: a bordered, table-like frame whose top border
/// carries the connection/model/tier title, one row of live session stats
/// (tokens, status), and one row of session metadata (path, created-at,
/// project root). Pure/stateless — a plain data-in, tree-out component,
/// replacing the old single-line Header.
#[component]
pub fn Dashboard(props: &DashboardProps, _hooks: &mut ntui::Hooks) -> ntui::Element {
    let status = if props.streaming {
        "generating…"
    } else {
        "ready"
    };
    let tokens = format!(
        "{} in / {} out",
        props.usage.input_tokens, props.usage.output_tokens
    );
    let model_label = if props.model_name.trim().is_empty() {
        "no model set".to_string()
    } else {
        props.model_name.clone()
    };
    let title = format!(
        "local-code v{} · {} · {} [{}]",
        env!("CARGO_PKG_VERSION"),
        props.connection_name,
        model_label,
        props.tier_label
    );

    element! {
        View(
            flex_direction: FlexDirection::Column,
            width: Dimension::Percent(100.0),
            border_style: BorderStyle::Round,
            border_color: Color::DarkGrey,
            border_title: Some(title),
            border_title_color: Color::Cyan,
            padding: 1,
        ) {
            View(flex_direction: FlexDirection::Row, justify_content: JustifyContent::SpaceBetween) {
                Text(content: status.to_string(), color: Color::DarkGrey)
                Text(content: tokens, color: Color::DarkGrey)
            }
            View(flex_direction: FlexDirection::Column) {
                Text(content: format!("project: {}", props.project_root), color: Color::DarkGrey, wrap: TextWrap::Truncate)
                Text(content: format!("session: {}", props.session_path), color: Color::DarkGrey, wrap: TextWrap::Truncate)
                Text(content: format!("started: {}", props.created_at), color: Color::DarkGrey, wrap: TextWrap::Truncate)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntui::Element;
    use ntui::testing::TestTerminal;

    fn props() -> DashboardProps {
        DashboardProps {
            connection_name: "local-vllm".into(),
            model_name: "qwen2.5-coder-32b".into(),
            tier_label: "ask".into(),
            usage: UsageSummary {
                input_tokens: 120,
                output_tokens: 45,
                estimated_cost: 0.0123,
            },
            streaming: false,
            session_path: "/tmp/session.json".into(),
            created_at: "2026-07-07T00:00:00Z".into(),
            project_root: "/home/joseph/Projects/local-code".into(),
        }
    }

    #[tokio::test]
    async fn renders_connection_model_and_tier() {
        let t = TestTerminal::new(100, 6, Element::component::<Dashboard>(props())).unwrap();
        let text = t.frame_text();
        assert!(text.contains(concat!("local-code v", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains("local-vllm"));
        assert!(text.contains("qwen2.5-coder-32b"));
        assert!(text.contains("[ask]"));
    }

    #[tokio::test]
    async fn renders_session_stats_and_metadata() {
        let t = TestTerminal::new(100, 6, Element::component::<Dashboard>(props())).unwrap();
        let text = t.frame_text();
        assert!(text.contains("ready"));
        assert!(text.contains("120 in / 45 out"));
        assert!(text.contains("/tmp/session.json"));
        assert!(text.contains("/home/joseph/Projects/local-code"));
    }

    #[tokio::test]
    async fn shows_generating_status_while_streaming() {
        let mut p = props();
        p.streaming = true;
        let t = TestTerminal::new(100, 6, Element::component::<Dashboard>(p)).unwrap();
        assert!(t.frame_text().contains("generating…"));
    }

    #[tokio::test]
    async fn shows_placeholder_when_model_name_is_unset() {
        let mut p = props();
        p.model_name = String::new();
        let t = TestTerminal::new(100, 6, Element::component::<Dashboard>(p)).unwrap();
        assert!(t.frame_text().contains("no model set"));
    }
}
