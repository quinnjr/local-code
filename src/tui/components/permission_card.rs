use ntui::Element;
use ntui::props::FlexDirection;
use ntui::style::Weight;
use ntui::widgets::Theme;

use crate::permissions::types::PermissionRequest;
use crate::tui::theme::{ChipBackground, ON_WARN, WARN, chip};

/// Renders a pending permission request as an inline card with numbered
/// choices, matching the spec's "Yes / Yes don't ask again this session / No"
/// options. A plain function (not a `#[component]`) since it holds no state of
/// its own — `Transcript` calls it inline wherever the pending request should
/// appear (immediately after the in-progress tool call that triggered it),
/// passing down the theme it resolved via `use_theme()`.
pub fn render_permission_card(request: &PermissionRequest, theme: &Theme) -> Element {
    let choice = |n: &str, label: &str| {
        let n = n.to_string();
        let label = label.to_string();
        ntui::element! {
            View(flex_direction: FlexDirection::Row, gap: 1) {
                Text(content: n, color: theme.accent, weight: Weight::Bold)
                Text(content: label, color: theme.foreground)
            }
        }
    };
    ntui::element! {
        View(flex_direction: FlexDirection::Column, border_style: theme.border_style, border_color: WARN, padding: 1) {
            View(flex_direction: FlexDirection::Row, gap: 1) {
                #(vec![chip("PERMISSION", ChipBackground::Flat(WARN), ON_WARN)])
                Text(content: format!("Permission requested: {}", request.description), color: WARN)
            }
            #(vec![
                choice("1)", "Yes"),
                choice("2)", "Yes, don't ask again this session"),
                choice("3)", "No (provide feedback)"),
            ])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::local_code_theme;
    use ntui::testing::TestTerminal;

    #[tokio::test]
    async fn renders_description_and_three_numbered_choices() {
        let request = PermissionRequest {
            description: "run shell command: rm file.txt".into(),
        };
        let t = TestTerminal::new(70, 7, render_permission_card(&request, &local_code_theme()))
            .unwrap();
        let text = t.frame_text();
        assert!(text.contains("PERMISSION"));
        assert!(text.contains("run shell command: rm file.txt"));
        assert!(text.contains("1) Yes"));
        assert!(text.contains("don't ask again this session"));
        assert!(text.contains("3) No"));
    }
}
