use ntui::props::{GradientDirection, TextWrap};
use ntui::style::{BorderStyle, Color, Weight};
use ntui::widgets::Theme;
use ntui::{Element, element};

// Token split: `Theme` (below) carries the per-widget tokens ntui's widgets
// resolve via `use_theme()`; the free consts here are brand identity —
// gradient endpoints and fixed semantic colors that can't live in a
// flat-color `Theme` field. Both are part of the one app palette.

/// The two endpoints of the local-code brand gradient (cyan → violet), used
/// by `GradientText` titles and gradient chip backgrounds so every branded
/// surface interpolates between the same pair.
pub const BRAND_FROM: Color = Color::Rgb(34, 211, 238);
pub const BRAND_TO: Color = Color::Rgb(167, 139, 250);

/// Violet identity color for tool-call cards in the transcript (the brand
/// gradient's far endpoint, used flat).
pub const TOOL_ACCENT: Color = BRAND_TO;

/// Attention/warning surfaces (permission prompts, elevated-but-not-maximal
/// permission tiers) and the foreground that stays readable on them.
pub const WARN: Color = Color::Yellow;
pub const ON_WARN: Color = Color::Black;

/// The app-wide `ntui::widgets::Theme`, provided once via `ContextProvider`
/// at the `Workspace` root so every widget (and `hooks.use_theme()` call)
/// below it resolves the same palette. Plain render functions receive it as
/// a `&Theme` parameter threaded down from their nearest `#[component]`
/// caller's `use_theme()`, so there is a single resolution path at runtime.
pub fn local_code_theme() -> Theme {
    Theme {
        accent: BRAND_FROM,
        surface: Color::Rgb(24, 24, 32),
        border: Color::DarkGrey,
        muted: Color::Rgb(140, 140, 150),
        foreground: Color::White,
        danger: Color::Rgb(248, 113, 113),
        success: Color::Rgb(52, 211, 153),
        border_style: BorderStyle::Round,
    }
}

/// Background of a [`chip`]: a flat color or a horizontal gradient.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ChipBackground {
    Flat(Color),
    Gradient(Color, Color),
}

/// A one-line label chip — bold text on a colored or gradient background,
/// padded with one space either side.
///
/// `Truncate`, not the default `Wrap`: `wrap_text`'s measurement collapses
/// the chip's deliberate padding spaces. Hand-rolled rather than ntui's
/// `Badge` because `Badge` pads all four sides (three rows tall) and these
/// chips must fit single-line rows like the tab bar.
pub fn chip(label: &str, bg: ChipBackground, fg: Color) -> Element {
    let (background, background_gradient) = match bg {
        ChipBackground::Flat(color) => (color, None),
        ChipBackground::Gradient(from, to) => (
            Color::Reset,
            Some((from, to, GradientDirection::Horizontal)),
        ),
    };
    element! {
        View(background: background, background_gradient: background_gradient) {
            Text(content: format!(" {label} "), color: fg, weight: Weight::Bold, wrap: TextWrap::Truncate)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ntui::testing::TestTerminal;

    #[test]
    fn accent_matches_the_brand_gradient_start() {
        assert_eq!(local_code_theme().accent, BRAND_FROM);
    }

    #[test]
    fn palette_tokens_are_distinct_enough_to_carry_meaning() {
        let t = local_code_theme();
        assert_ne!(t.accent, t.danger);
        assert_ne!(t.danger, t.success);
        assert_ne!(t.muted, t.foreground);
        assert_ne!(WARN, t.danger);
    }

    #[tokio::test]
    async fn chip_keeps_its_padding_spaces() {
        let el = chip("ask", ChipBackground::Flat(WARN), ON_WARN);
        let t = TestTerminal::new(10, 1, el).unwrap();
        assert!(t.frame_text().contains(" ask "));
    }
}
