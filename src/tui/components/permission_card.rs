// src/tui/components/permission_card.rs

use ntui::Element;
use ntui::props::FlexDirection;
use ntui::style::{BorderStyle, Color};

use crate::permissions::types::PermissionRequest;

/// Renders a pending permission request as an inline card with numbered
/// choices, matching the spec's "Yes / Yes don't ask again this session / No"
/// options. A plain function (not a `#[component]`) since it holds no state of
/// its own — `Transcript` calls it inline wherever the pending request should
/// appear (immediately after the in-progress tool call that triggered it).
pub fn render_permission_card(request: &PermissionRequest) -> Element {
    ntui::element! {
        View(flex_direction: FlexDirection::Column, border_style: BorderStyle::Round, border_color: Color::Yellow, padding: 1) {
            Text(content: format!("Permission requested: {}", request.description), color: Color::Yellow)
            Text(content: "1) Yes", color: Color::White)
            Text(content: "2) Yes, don't ask again this session", color: Color::White)
            Text(content: "3) No (provide feedback)", color: Color::White)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntui::testing::TestTerminal;

    #[tokio::test]
    async fn renders_description_and_three_numbered_choices() {
        let request = PermissionRequest {
            tool_name: "bash".into(),
            description: "run shell command: rm file.txt".into(),
            command_preview: Some("rm file.txt".into()),
        };
        let t = TestTerminal::new(60, 6, render_permission_card(&request)).unwrap();
        let text = t.frame_text();
        assert!(text.contains("run shell command: rm file.txt"));
        assert!(text.contains("1) Yes"));
        assert!(text.contains("don't ask again this session"));
        assert!(text.contains("3) No"));
    }
}
