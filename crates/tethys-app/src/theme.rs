//! Tethys's look, after the Telesto Games brand (telesto.games): a plum night
//! with broadcast-orange signal, monospace type, sharp corners and hairlines.
//!
//! Brand colours: plum `#0B001F`, broadcast orange `#F24B38`, lilac `#C8B8D8`.
//! Hierarchy comes from opacity, not extra hues.

use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::{Theme, ThemeMode, ThemeTokens};
use gpui_kit::*;
use tethys_adapters::agent_terminal::palette;

use crate::terminal_view::hsla_from;

/// Brand plum: the window backdrop.
pub const PLUM: u32 = 0x0b001f;
/// Brand broadcast orange: the one accent.
pub const ORANGE: u32 = 0xf24b38;
/// Brand lilac, used by telesto.games for quiet text.
pub const LILAC: u32 = 0xc8b8d8;

/// Menu bar and header: a lifted plum so the chrome reads apart from the panes.
const HEADER: u32 = 0x160a33;
/// Raised surfaces: buttons, popovers.
const SURFACE: u32 = 0x1f1240;
const SURFACE_HOVER: u32 = 0x2a1a52;
const FOREGROUND: u32 = 0xece4f5;
const ORANGE_HOVER: u32 = 0xff6a57;

/// UI typeface: Cascadia Mono (SIL OFL, ships with Windows 11), the same face
/// as the terminals. The brand face, PP Fraktion Mono, is commercial and can't
/// ship in an MIT product. Cascadia's capitals are centred in its line box, so
/// button labels sit level.
pub const UI_FONT: &str = "Cascadia Mono";

pub fn apply(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    let theme = Theme::global_mut(cx);
    let c = &mut theme.colors;
    let pane = hsla_from(palette::BACKGROUND);
    let orange: Hsla = rgb(ORANGE).into();
    // Telesto draws structure with orange hairlines at low opacity.
    let hairline = orange.opacity(0.22);

    c.background = rgb(PLUM).into();
    c.foreground = rgb(FOREGROUND).into();
    c.muted = rgb(SURFACE).into();
    c.muted_foreground = Hsla::from(rgb(LILAC)).opacity(0.62);
    c.border = hairline;
    c.title_bar = rgb(HEADER).into();
    c.title_bar_border = orange.opacity(0.35);
    c.popover = rgb(SURFACE).into();
    c.popover_foreground = rgb(FOREGROUND).into();

    c.primary = orange;
    c.primary_hover = rgb(ORANGE_HOVER).into();
    c.primary_active = orange;
    // Orange buttons carry plum text, like the brand mark on its ground.
    c.primary_foreground = rgb(PLUM).into();
    c.ring = orange;
    c.accent = rgb(SURFACE_HOVER).into();
    c.accent_foreground = rgb(FOREGROUND).into();
    c.link = orange;

    c.secondary = rgb(SURFACE).into();
    c.secondary_hover = rgb(SURFACE_HOVER).into();
    c.secondary_active = rgb(SURFACE_HOVER).into();
    c.secondary_foreground = rgb(FOREGROUND).into();

    c.button = rgb(SURFACE).into();
    c.button_hover = rgb(SURFACE_HOVER).into();
    c.button_active = rgb(SURFACE_HOVER).into();
    c.button_foreground = rgb(FOREGROUND).into();
    c.button_primary = orange;
    c.button_primary_hover = rgb(ORANGE_HOVER).into();
    c.button_primary_active = orange;
    c.button_primary_foreground = rgb(PLUM).into();
    c.button_secondary = rgb(SURFACE).into();
    c.button_secondary_hover = rgb(SURFACE_HOVER).into();
    c.button_secondary_active = rgb(SURFACE_HOVER).into();
    c.button_secondary_foreground = rgb(FOREGROUND).into();

    // Tabs: inactive tabs sit on the backdrop, the active one joins its pane.
    c.tab_bar = rgb(PLUM).into();
    c.tab = rgb(PLUM).into();
    c.tab_foreground = Hsla::from(rgb(LILAC)).opacity(0.55);
    c.tab_active = pane;
    c.tab_active_foreground = orange;
    c.drag_border = orange;
    c.drop_target = orange.opacity(0.16);
    c.selection = orange.opacity(0.28);
    c.caret = orange;
    c.scrollbar_thumb = Hsla::from(rgb(LILAC)).opacity(0.18);
    c.scrollbar_thumb_hover = orange.opacity(0.5);

    theme.font_family = UI_FONT.into();
    theme.highlight_theme = HighlightTheme::default_dark();
    // Telesto is all hairlines and hard edges; keep corners nearly square.
    theme.radius = px(2.);
    theme.radius_lg = px(3.);
    // The dock and tabs read these derived tokens, not the colors directly.
    theme.tokens = ThemeTokens::from(theme.colors);
}

/// An uppercase label with wide tracking, imitating Telesto's letter-spacing
/// (GPUI has none): `"Recent projects"` → `"R E C E N T   P R O J E C T S"`.
pub fn tracked(label: &str) -> String {
    label
        .split_whitespace()
        .map(|word| {
            word.to_uppercase()
                .chars()
                .map(String::from)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("   ")
}

#[cfg(test)]
mod tests {
    use super::tracked;

    #[test]
    fn tracks_labels() {
        assert_eq!(tracked("Recent projects"), "R E C E N T   P R O J E C T S");
        assert_eq!(tracked("files"), "F I L E S");
    }
}
