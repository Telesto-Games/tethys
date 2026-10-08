//! Tethys's look: a dark slate palette with a blue accent, where each pane
//! reads as a bordered card on a darker backdrop.

use gpui_kit::component::{Theme, ThemeMode, ThemeTokens};
use gpui_kit::*;
use tethys_adapters::agent_terminal::palette;

use crate::terminal_view::hsla_from;

/// Window backdrop, behind the panes and tab bars.
const BACKDROP: u32 = 0x0e1014;
/// Raised surfaces: buttons, hover states.
const SURFACE: u32 = 0x1a1e25;
const SURFACE_HOVER: u32 = 0x242932;
const BORDER: u32 = 0x2c323c;
/// Menu bar and header: lighter than the backdrop so the chrome stands apart.
const HEADER: u32 = 0x1b1f27;
const FOREGROUND: u32 = 0xe6e9ef;
const MUTED: u32 = 0x8a93a3;
const ACCENT: u32 = 0x4f8cff;
const ACCENT_HOVER: u32 = 0x6b9fff;

pub fn apply(cx: &mut App) {
    Theme::change(ThemeMode::Dark, None, cx);
    let theme = Theme::global_mut(cx);
    let c = &mut theme.colors;
    let pane = hsla_from(palette::BACKGROUND);

    c.background = rgb(BACKDROP).into();
    c.foreground = rgb(FOREGROUND).into();
    c.muted = rgb(SURFACE).into();
    c.muted_foreground = rgb(MUTED).into();
    c.border = rgb(BORDER).into();
    c.title_bar = rgb(HEADER).into();
    c.title_bar_border = rgb(BORDER).into();
    c.popover = rgb(SURFACE).into();
    c.popover_foreground = rgb(FOREGROUND).into();

    c.primary = rgb(ACCENT).into();
    c.primary_hover = rgb(ACCENT_HOVER).into();
    c.primary_active = rgb(ACCENT).into();
    c.primary_foreground = rgb(0xffffff).into();
    c.ring = rgb(ACCENT).into();
    c.accent = rgb(SURFACE_HOVER).into();
    c.accent_foreground = rgb(FOREGROUND).into();

    c.secondary = rgb(SURFACE).into();
    c.secondary_hover = rgb(SURFACE_HOVER).into();
    c.secondary_active = rgb(SURFACE_HOVER).into();
    c.secondary_foreground = rgb(FOREGROUND).into();

    c.button = rgb(SURFACE).into();
    c.button_hover = rgb(SURFACE_HOVER).into();
    c.button_active = rgb(SURFACE_HOVER).into();
    c.button_foreground = rgb(FOREGROUND).into();
    c.button_primary = rgb(ACCENT).into();
    c.button_primary_hover = rgb(ACCENT_HOVER).into();
    c.button_primary_active = rgb(ACCENT).into();
    c.button_primary_foreground = rgb(0xffffff).into();
    c.button_secondary = rgb(SURFACE).into();
    c.button_secondary_hover = rgb(SURFACE_HOVER).into();
    c.button_secondary_active = rgb(SURFACE_HOVER).into();
    c.button_secondary_foreground = rgb(FOREGROUND).into();

    // Tabs: inactive tabs sit on the backdrop, the active one joins its pane.
    c.tab_bar = rgb(BACKDROP).into();
    c.tab = rgb(BACKDROP).into();
    c.tab_foreground = rgb(MUTED).into();
    c.tab_active = pane;
    c.tab_active_foreground = rgb(FOREGROUND).into();
    c.drag_border = rgb(ACCENT).into();
    c.drop_target = hsla_from_hex(ACCENT).opacity(0.18);

    theme.radius = px(6.);
    theme.radius_lg = px(10.);
    // The dock and tabs read these derived tokens, not the colors directly.
    theme.tokens = ThemeTokens::from(theme.colors);
}

fn hsla_from_hex(hex: u32) -> Hsla {
    rgb(hex).into()
}
