use ntui::props::{FlexDirection, GradientDirection, JustifyContent, TextWrap};
use ntui::style::Weight;
use ntui::widgets::{GradientText, GradientTextProps};
use ntui::{component, element};

use crate::tui::theme::{BRAND_FROM, BRAND_TO};

/// What the tab bar shows for one window.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct TabInfo {
    /// Position in the window list — also the digit that selects it (`C-b <n>`).
    pub index: usize,
    /// Number of panes, shown as a suffix (`[2]`) when the window is split.
    pub panes: usize,
    /// Any pane in the window has a turn streaming.
    pub streaming: bool,
    /// Any pane in the window has a permission prompt waiting for a
    /// decision. Takes display priority over `streaming`: a blocked turn
    /// only advances once the user focuses that window and answers, so
    /// showing the generic busy marker would misrepresent a stuck turn as
    /// progress.
    pub awaiting_permission: bool,
}

#[derive(Clone, PartialEq, Default)]
pub struct TabBarProps {
    pub tabs: Vec<TabInfo>,
    pub active: usize,
    /// `C-b` is armed; the bar shows a `C-b` badge exactly while the next
    /// keystroke will be taken as a workspace command.
    pub prefix_pending: bool,
    /// Transient workspace-level error (e.g. a new tab's session file could
    /// not be created) — shown here because it belongs to no session.
    pub notice: Option<String>,
}

/// One-line tmux-style status bar listing windows: `0:agent* 1:agent✻ 2:agent!`
/// — `*` marks the active window, `✻` a window with a streaming session, `!`
/// a window whose session is blocked waiting for a permission decision. The
/// active window's label sits on a brand-gradient chip; blocked windows show
/// in the danger color, streaming ones in the accent.
#[component]
pub fn TabBar(props: &TabBarProps, hooks: &mut ntui::Hooks) -> ntui::Element {
    let theme = hooks.use_theme();
    let tab_els: Vec<ntui::Element> = props
        .tabs
        .iter()
        .map(|tab| {
            let marker = if tab.index == props.active { "*" } else { "" };
            let busy = if tab.awaiting_permission {
                "!"
            } else if tab.streaming {
                "✻"
            } else {
                ""
            };
            let panes = if tab.panes > 1 {
                format!("[{}]", tab.panes)
            } else {
                String::new()
            };
            let label = format!("{}:agent{panes}{marker}{busy}", tab.index);
            if tab.index == props.active {
                element! {
                    View(background_gradient: Some((BRAND_FROM, BRAND_TO, GradientDirection::Horizontal))) {
                        // Truncate keeps the chip's padding spaces, which the
                        // default Wrap measurement would collapse away.
                        Text(content: format!(" {label} "), color: theme.surface, weight: Weight::Bold, wrap: TextWrap::Truncate)
                    }
                }
            } else {
                let color = if tab.awaiting_permission {
                    theme.danger
                } else if tab.streaming {
                    theme.accent
                } else {
                    theme.muted
                };
                element! { Text(content: label, color: color) }
            }
        })
        .collect();
    let right = if props.prefix_pending {
        "C-b …".to_string()
    } else if let Some(notice) = &props.notice {
        notice.clone()
    } else {
        "C-b c/n/p/%/\"/x".to_string()
    };
    let right_color = if props.prefix_pending {
        theme.accent
    } else if props.notice.is_some() {
        theme.danger
    } else {
        theme.muted
    };
    element! {
        View(flex_direction: FlexDirection::Row, justify_content: JustifyContent::SpaceBetween, padding: 0) {
            View(flex_direction: FlexDirection::Row, gap: 1) {
                GradientText(content: "local-code".to_string(), from: Some(BRAND_FROM), to: Some(BRAND_TO), weight: Weight::Bold)
                #(tab_els)
            }
            Text(content: right, color: right_color)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntui::Element;
    use ntui::testing::TestTerminal;

    fn tabs(n: usize) -> Vec<TabInfo> {
        (0..n)
            .map(|index| TabInfo {
                index,
                panes: 1,
                streaming: false,
                awaiting_permission: false,
            })
            .collect()
    }

    #[tokio::test]
    async fn marks_the_active_window_with_a_star() {
        let props = TabBarProps {
            tabs: tabs(3),
            active: 1,
            prefix_pending: false,
            notice: None,
        };
        let t = TestTerminal::new(80, 1, Element::component::<TabBar>(props)).unwrap();
        let text = t.frame_text();
        assert!(text.contains("0:agent "), "{text}");
        assert!(text.contains("1:agent*"), "{text}");
        assert!(text.contains("2:agent"), "{text}");
    }

    #[tokio::test]
    async fn shows_streaming_marker_and_pane_count() {
        let mut all = tabs(2);
        all[0].streaming = true;
        all[1].panes = 3;
        let props = TabBarProps {
            tabs: all,
            active: 0,
            prefix_pending: false,
            notice: None,
        };
        let t = TestTerminal::new(80, 1, Element::component::<TabBar>(props)).unwrap();
        let text = t.frame_text();
        assert!(text.contains("0:agent*✻"), "{text}");
        assert!(text.contains("1:agent[3]"), "{text}");
    }

    #[tokio::test]
    async fn awaiting_permission_marker_outranks_streaming() {
        let mut all = tabs(2);
        all[1].streaming = true;
        all[1].awaiting_permission = true;
        let props = TabBarProps {
            tabs: all,
            active: 0,
            prefix_pending: false,
            notice: None,
        };
        let t = TestTerminal::new(80, 1, Element::component::<TabBar>(props)).unwrap();
        let text = t.frame_text();
        assert!(text.contains("1:agent!"), "{text}");
        assert!(!text.contains("1:agent✻"), "{text}");
    }

    #[tokio::test]
    async fn shows_prefix_badge_while_armed() {
        let props = TabBarProps {
            tabs: tabs(1),
            active: 0,
            prefix_pending: true,
            notice: None,
        };
        let t = TestTerminal::new(80, 1, Element::component::<TabBar>(props)).unwrap();
        assert!(t.frame_text().contains("C-b …"));
    }

    #[tokio::test]
    async fn shows_notice_when_present_and_not_armed() {
        let props = TabBarProps {
            tabs: tabs(1),
            active: 0,
            prefix_pending: false,
            notice: Some("couldn't create session: disk full".into()),
        };
        let t = TestTerminal::new(80, 1, Element::component::<TabBar>(props)).unwrap();
        assert!(t.frame_text().contains("disk full"));
    }
}
