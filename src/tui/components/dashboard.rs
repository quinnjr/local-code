use ntui::props::{AlignItems, Dimension, FlexDirection, JustifyContent, TextWrap};
use ntui::style::{Color, Weight};
use ntui::widgets::{
    Divider, DividerProps, GradientText, GradientTextProps, Table, TableProps, Theme,
};
use ntui::{component, element};

use crate::permissions::types::PermissionTier;
use crate::tui::components::status_indicator::{StatusIndicator, StatusIndicatorProps};
use crate::tui::state::UsageSummary;
use crate::tui::theme::{BRAND_FROM, BRAND_TO, ChipBackground, ON_WARN, WARN, chip};

#[derive(Clone, PartialEq, Default)]
pub struct DashboardProps {
    pub connection_name: String,
    pub model_name: String,
    /// Display text for the tier chip. Derived from `tier` by the caller
    /// (`tier_label`); kept separate so the label can change without
    /// affecting the severity styling, which keys off the enum.
    pub tier_label: String,
    /// Drives the chip's severity styling. Never derive severity from
    /// `tier_label` — a label rename must not be able to silently drop the
    /// danger cue.
    pub tier: PermissionTier,
    pub usage: UsageSummary,
    pub streaming: bool,
    pub session_path: String,
    pub created_at: String,
    pub project_root: String,
}

/// The tier chip's severity styling, keyed off the enum: the brand gradient
/// for the safe default, a warning chip for auto-accept-edits (file
/// mutations auto-approved), flat danger for full-auto (nothing prompts).
/// Returns `(background, text color)`.
fn tier_chip_style(tier: PermissionTier, theme: &Theme) -> (ChipBackground, Color) {
    match tier {
        PermissionTier::Ask => (
            ChipBackground::Gradient(BRAND_FROM, BRAND_TO),
            theme.surface,
        ),
        PermissionTier::AutoAcceptEdits => (ChipBackground::Flat(WARN), ON_WARN),
        PermissionTier::FullAuto => (ChipBackground::Flat(theme.danger), theme.surface),
    }
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
    let (chip_bg, chip_fg) = tier_chip_style(props.tier, &theme);
    let tier_chip = chip(&props.tier_label, chip_bg, chip_fg);
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
                #(vec![tier_chip])
            }
            View(flex_direction: FlexDirection::Row, justify_content: JustifyContent::SpaceBetween) {
                StatusIndicator(streaming: props.streaming)
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
            tier: PermissionTier::Ask,
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

    // The chip's color branches are invisible to `frame_text()` (it drops
    // styling), so the severity mapping is unit-tested through the pure
    // `tier_chip_style` seam instead.
    #[test]
    fn ask_tier_wears_the_brand_gradient() {
        let theme = crate::tui::theme::local_code_theme();
        assert_eq!(
            tier_chip_style(PermissionTier::Ask, &theme),
            (
                ChipBackground::Gradient(BRAND_FROM, BRAND_TO),
                theme.surface
            )
        );
    }

    #[test]
    fn auto_accept_edits_tier_wears_the_warning_chip() {
        let theme = crate::tui::theme::local_code_theme();
        assert_eq!(
            tier_chip_style(PermissionTier::AutoAcceptEdits, &theme),
            (ChipBackground::Flat(WARN), ON_WARN)
        );
    }

    #[test]
    fn full_auto_tier_wears_the_danger_chip() {
        let theme = crate::tui::theme::local_code_theme();
        assert_eq!(
            tier_chip_style(PermissionTier::FullAuto, &theme),
            (ChipBackground::Flat(theme.danger), theme.surface)
        );
    }
}
