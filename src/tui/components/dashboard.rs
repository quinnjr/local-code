use ntui::props::{
    AlignItems, Dimension, FlexDirection, GradientDirection, JustifyContent, TextWrap,
};
use ntui::style::Weight;
use ntui::widgets::{
    Divider, DividerProps, GradientText, GradientTextProps, Spinner, SpinnerProps, Table,
    TableProps,
};
use ntui::{component, element};

use crate::tui::state::UsageSummary;
use crate::tui::theme::{BRAND_FROM, BRAND_TO};

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

/// Always-visible top dashboard: a bordered frame with a gradient brand
/// title, connection/model line, a permission-tier chip, a live status row
/// (spinner while generating), and the session metadata as an aligned
/// two-column table. Pure/stateless — a plain data-in, tree-out component.
#[component]
pub fn Dashboard(props: &DashboardProps, hooks: &mut ntui::Hooks) -> ntui::Element {
    let theme = hooks.use_theme();
    let model_label = if props.model_name.trim().is_empty() {
        "no model set".to_string()
    } else {
        props.model_name.clone()
    };
    let brand = format!("◆ local-code v{}", env!("CARGO_PKG_VERSION"));
    let endpoint = format!("{} · {}", props.connection_name, model_label);
    let tokens = format!(
        "{} in / {} out",
        props.usage.input_tokens, props.usage.output_tokens
    );
    // The tier chip's background gradient doubles as a severity cue: the
    // brand gradient for the default ask tier, flat danger for full-auto.
    let chip_gradient = if props.tier_label == "full-auto" {
        (theme.danger, theme.danger, GradientDirection::Horizontal)
    } else {
        (BRAND_FROM, BRAND_TO, GradientDirection::Horizontal)
    };
    let status: ntui::Element = if props.streaming {
        element! { Spinner(label: "generating…".to_string()) }
    } else {
        element! { Text(content: "● ready".to_string(), color: theme.success) }
    };
    let metadata = vec![
        vec!["project".to_string(), props.project_root.clone()],
        vec!["session".to_string(), props.session_path.clone()],
        vec!["started".to_string(), props.created_at.clone()],
    ];

    element! {
        View(
            flex_direction: FlexDirection::Column,
            width: Dimension::Percent(100.0),
            border_style: theme.border_style,
            border_color: theme.border,
            padding: 1,
        ) {
            View(flex_direction: FlexDirection::Row, justify_content: JustifyContent::SpaceBetween, align_items: AlignItems::Center) {
                View(flex_direction: FlexDirection::Row, gap: 2) {
                    GradientText(content: brand, from: Some(BRAND_FROM), to: Some(BRAND_TO), weight: Weight::Bold)
                    Text(content: endpoint, color: theme.foreground, wrap: TextWrap::Truncate)
                }
                View(background_gradient: Some(chip_gradient)) {
                    // Truncate, not the default Wrap: wrapping's measurement
                    // collapses the chip's deliberate padding spaces.
                    Text(content: format!(" {} ", props.tier_label), color: theme.surface, weight: Weight::Bold, wrap: TextWrap::Truncate)
                }
            }
            View(flex_direction: FlexDirection::Row, justify_content: JustifyContent::SpaceBetween) {
                #(vec![status])
                Text(content: tokens, color: theme.muted)
            }
            Divider(label: "session".to_string())
            Table(rows: metadata)
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
        let t = TestTerminal::new(100, 10, Element::component::<Dashboard>(props())).unwrap();
        let text = t.frame_text();
        assert!(text.contains(concat!("local-code v", env!("CARGO_PKG_VERSION"))));
        assert!(text.contains("local-vllm"));
        assert!(text.contains("qwen2.5-coder-32b"));
        assert!(text.contains(" ask "));
    }

    #[tokio::test]
    async fn renders_session_stats_and_metadata() {
        let t = TestTerminal::new(100, 10, Element::component::<Dashboard>(props())).unwrap();
        let text = t.frame_text();
        assert!(text.contains("● ready"));
        assert!(text.contains("120 in / 45 out"));
        assert!(text.contains("/tmp/session.json"));
        assert!(text.contains("/home/joseph/Projects/local-code"));
    }

    #[tokio::test]
    async fn shows_spinner_instead_of_ready_while_streaming() {
        let mut p = props();
        p.streaming = true;
        let t = TestTerminal::new(100, 10, Element::component::<Dashboard>(p)).unwrap();
        let text = t.frame_text();
        assert!(text.contains("generating…"));
        assert!(!text.contains("● ready"));
    }

    #[tokio::test]
    async fn shows_placeholder_when_model_name_is_unset() {
        let mut p = props();
        p.model_name = String::new();
        let t = TestTerminal::new(100, 10, Element::component::<Dashboard>(p)).unwrap();
        assert!(t.frame_text().contains("no model set"));
    }
}
