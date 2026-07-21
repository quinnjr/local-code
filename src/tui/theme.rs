use ntui::style::{BorderStyle, Color};
use ntui::widgets::Theme;

/// The two endpoints of the local-code brand gradient (cyan → violet), used
/// by `GradientText` titles and gradient chip backgrounds so every branded
/// surface interpolates between the same pair.
pub const BRAND_FROM: Color = Color::Rgb(34, 211, 238);
pub const BRAND_TO: Color = Color::Rgb(167, 139, 250);

/// Violet identity color for tool-call cards in the transcript (the brand
/// gradient's far endpoint, used flat).
pub const TOOL_ACCENT: Color = BRAND_TO;

/// The app-wide `ntui::widgets::Theme`, provided once via `ContextProvider`
/// at the `Workspace` root so every widget (and `hooks.use_theme()` call)
/// below it resolves the same palette. Components rendered from plain
/// functions without `Hooks` access (e.g. `transcript::render_entry`) reach
/// the same tokens through this function directly — it's `const`-shaped
/// (pure, no I/O), so both paths always agree.
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
