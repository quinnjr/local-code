// src/tui/components/dashboard.rs

use ntui::props::{FlexDirection, JustifyContent};
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

/// Always-visible top dashboard: a bordered, table-like frame with one row of
/// connection/model/tier, one row of live session stats (tokens, status), and
/// one row of session metadata (path, created-at, project root). Pure/stateless
/// — a plain data-in, tree-out component, replacing the old single-line Header.
#[component]
pub fn Dashboard(props: &DashboardProps, _hooks: &mut ntui::Hooks) -> ntui::Element {
    let status = if props.streaming { "generating…" } else { "ready" };
    let tokens = format!("{} in / {} out", props.usage.input_tokens, props.usage.output_tokens);

    element! {
        View(
            flex_direction: FlexDirection::Column,
            border_style: BorderStyle::Round,
            border_color: Color::DarkGrey,
            padding: 1,
        ) {
            View(flex_direction: FlexDirection::Row, justify_content: JustifyContent::SpaceBetween) {
                Text(content: format!("local-code · {}", props.connection_name), color: Color::Cyan)
                Text(content: props.model_name.clone(), color: Color::White)
                Text(content: format!("[{}]", props.tier_label), color: Color::Yellow)
            }
            View(flex_direction: FlexDirection::Row, justify_content: JustifyContent::SpaceBetween) {
                Text(content: status.to_string(), color: Color::DarkGrey)
                Text(content: tokens, color: Color::DarkGrey)
                Text(content: format!("${:.4}", props.usage.estimated_cost), color: Color::DarkGrey)
            }
            View(flex_direction: FlexDirection::Row, justify_content: JustifyContent::SpaceBetween) {
                Text(content: format!("project: {}", props.project_root), color: Color::DarkGrey)
                Text(content: format!("session: {}", props.session_path), color: Color::DarkGrey)
                Text(content: format!("started: {}", props.created_at), color: Color::DarkGrey)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntui::testing::TestTerminal;
    use ntui::Element;

    fn props() -> DashboardProps {
        DashboardProps {
            connection_name: "local-vllm".into(),
            model_name: "qwen2.5-coder-32b".into(),
            tier_label: "ask".into(),
            usage: UsageSummary { input_tokens: 120, output_tokens: 45, estimated_cost: 0.0123 },
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
}
