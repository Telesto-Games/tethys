//! Tethys's look. The built-in theme follows the Telesto Games brand
//! (telesto.games): a plum night with broadcast-orange signal, monospace type,
//! sharp corners and hairlines. Brand colours: plum `#0B001F`, broadcast orange
//! `#F24B38`, lilac `#C8B8D8`. Hierarchy comes from opacity, not extra hues.
//!
//! A theme is a [`ThemeSpec`]: a backdrop, two text colours, an accent and a
//! font. Everything else (headers, raised surfaces, hovers, hairlines) is
//! derived from those, so a user theme made in Edit → Themes… gets the same
//! structure as the built-in one.

use gpui_kit::component::highlighter::HighlightTheme;
use gpui_kit::component::{Theme, ThemeMode, ThemeTokens};
use gpui_kit::*;
use tethys_adapters::agent_terminal::palette;
use tethys_core::ports::ConfigStore;
use tethys_core::theme::{self, ThemeSpec};

use crate::terminal_view::hsla_from;
use crate::workspace::Services;

/// The theme in use, for the Themes dialog to show which one that is.
pub struct CurrentTheme {
    pub spec: ThemeSpec,
}

impl Global for CurrentTheme {}

/// What terminals draw with: their own palette and font, or the theme's
/// when it asks them to follow it.
#[derive(Clone, Debug, PartialEq)]
pub struct TerminalSkin {
    pub colors: palette::Defaults,
    pub font_family: SharedString,
    pub font_size: f32,
}

impl Global for TerminalSkin {}

impl Default for TerminalSkin {
    fn default() -> Self {
        Self {
            colors: palette::Defaults::default(),
            font_family: ThemeSpec::builtin().font_family.into(),
            font_size: 14.,
        }
    }
}

impl TerminalSkin {
    /// The skin for `spec`: the defaults unless it asks terminals to follow
    /// it. The terminal is a pane, so it sits on the lifted pane colour.
    fn for_spec(spec: &ThemeSpec, font_family: &str, font_size: f32) -> Self {
        if !spec.terminal {
            return Self::default();
        }
        let builtin = ThemeSpec::builtin();
        let to_rgb = |c: Hsla| palette::rgb(packed(c));
        Self {
            colors: palette::Defaults {
                foreground: to_rgb(colour(&spec.foreground, &builtin.foreground)),
                background: to_rgb(pane_colour(spec, &builtin)),
                cursor: to_rgb(colour(&spec.accent, &builtin.accent)),
            },
            font_family: font_family.to_string().into(),
            // Terminals read two pixels smaller than the UI, as the defaults do.
            font_size: (font_size - 2.).max(ThemeSpec::MIN_FONT_SIZE),
        }
    }
}

/// The active tab joins its pane. The built-in theme's tabs match the
/// terminal backdrop exactly; another theme lifts its own background a little.
fn pane_colour(spec: &ThemeSpec, builtin: &ThemeSpec) -> Hsla {
    let background = colour(&spec.background, &builtin.background);
    if background == colour(&builtin.background, &builtin.background) {
        hsla_from(palette::BACKGROUND)
    } else {
        raise(background, 0.03)
    }
}

/// A spec's font size, or the built-in one if it isn't a finite number,
/// clamped to the range the dialog allows.
pub fn font_size_of(size: f32) -> f32 {
    if size.is_finite() {
        size.clamp(ThemeSpec::MIN_FONT_SIZE, ThemeSpec::MAX_FONT_SIZE)
    } else {
        ThemeSpec::builtin().font_size
    }
}

/// A colour as `0xrrggbb`, alpha dropped.
pub fn packed(colour: Hsla) -> u32 {
    let rgba: Rgba = colour.into();
    let channel = |v: f32| (v.clamp(0., 1.) * 255.).round() as u32;
    channel(rgba.r) << 16 | channel(rgba.g) << 8 | channel(rgba.b)
}

/// Applies the saved theme. Needs [`Services`] to be set.
pub fn init(cx: &mut App) {
    let spec = saved(&cx.global::<Services>().config);
    apply(&spec, cx);
}

/// The theme the config says is in use: the built-in one unless a user theme
/// of the active name exists.
pub fn saved(config: &impl ConfigStore) -> ThemeSpec {
    let active = config.active_theme().ok().flatten();
    let themes = config.themes().unwrap_or_default();
    active
        .and_then(|name| themes.into_iter().find(|t| t.name == name))
        .unwrap_or_else(ThemeSpec::builtin)
}

/// A spec colour, or `fallback` (a built-in theme colour) if it doesn't parse.
pub fn colour(text: &str, fallback: &str) -> Hsla {
    let rgb = theme::parse_hex(text)
        .or_else(|| theme::parse_hex(fallback))
        .expect("the built-in colours parse");
    gpui_kit::rgb(rgb).into()
}

/// `c` moved away from its background by `by` in lightness: lighter on a
/// dark theme, darker on a light one. Raised surfaces lose some saturation,
/// as the brand's lifted plums do.
fn raise(c: Hsla, by: f32) -> Hsla {
    let l = if c.l > 0.5 { c.l - by } else { c.l + by };
    Hsla {
        l: l.clamp(0., 1.),
        s: c.s * 0.6,
        ..c
    }
}

/// Applies `spec` to every window and remembers it as [`CurrentTheme`].
pub fn apply(spec: &ThemeSpec, cx: &mut App) {
    let builtin = ThemeSpec::builtin();
    let background = colour(&spec.background, &builtin.background);
    let foreground = colour(&spec.foreground, &builtin.foreground);
    let quiet = colour(&spec.muted_foreground, &builtin.muted_foreground);
    let accent = colour(&spec.accent, &builtin.accent);
    let light = background.l > 0.5;
    // Chrome and raised surfaces step away from the backdrop in three lifts.
    let header = raise(background, 0.06);
    let surface = raise(background, 0.10);
    let surface_hover = raise(background, 0.15);
    let accent_hover = Hsla {
        l: (accent.l + 0.08).min(1.),
        ..accent
    };
    let pane = pane_colour(spec, &builtin);
    // Structure is drawn with accent hairlines at low opacity.
    let hairline = accent.opacity(0.22);
    let font_size = font_size_of(spec.font_size);
    let font_family = if spec.font_family.trim().is_empty() {
        builtin.font_family.clone()
    } else {
        spec.font_family.trim().to_string()
    };

    // Start from gpui-component's registered theme for the mode, so anything
    // not set below has a sensible value, then write every colour we own.
    let mode = if light {
        ThemeMode::Light
    } else {
        ThemeMode::Dark
    };
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| {
        let c = &mut theme.colors;
        c.background = background;
        c.foreground = foreground;
        c.muted = surface;
        c.muted_foreground = quiet.opacity(0.62);
        c.border = hairline;
        c.title_bar = header;
        c.title_bar_border = accent.opacity(0.35);
        c.popover = surface;
        c.popover_foreground = foreground;

        c.primary = accent;
        c.primary_hover = accent_hover;
        c.primary_active = accent;
        // Accent buttons carry backdrop-coloured text, like the brand mark on its ground.
        c.primary_foreground = background;
        c.ring = accent;
        c.accent = surface_hover;
        c.accent_foreground = foreground;
        c.link = accent;

        c.secondary = surface;
        c.secondary_hover = surface_hover;
        c.secondary_active = surface_hover;
        c.secondary_foreground = foreground;

        c.button = surface;
        c.button_hover = surface_hover;
        c.button_active = surface_hover;
        c.button_foreground = foreground;
        c.button_primary = accent;
        c.button_primary_hover = accent_hover;
        c.button_primary_active = accent;
        c.button_primary_foreground = background;
        c.button_secondary = surface;
        c.button_secondary_hover = surface_hover;
        c.button_secondary_active = surface_hover;
        c.button_secondary_foreground = foreground;

        // Tabs: inactive tabs sit on the backdrop, the active one joins its pane.
        c.tab_bar = background;
        c.tab = background;
        c.tab_foreground = quiet.opacity(0.55);
        c.tab_active = pane;
        c.tab_active_foreground = accent;
        c.drag_border = accent;
        c.drop_target = accent.opacity(0.16);
        c.selection = accent.opacity(0.28);
        c.caret = accent;
        c.scrollbar_thumb = quiet.opacity(0.18);
        c.scrollbar_thumb_hover = accent.opacity(0.5);

        theme.font_family = font_family.clone().into();
        theme.font_size = px(font_size);
        theme.highlight_theme = if light {
            HighlightTheme::default_light()
        } else {
            HighlightTheme::default_dark()
        };
        // All hairlines and hard edges; keep corners nearly square.
        theme.radius = px(2.);
        theme.radius_lg = px(3.);
        // The dock and tabs read these derived tokens, not the colors directly.
        theme.tokens = ThemeTokens::from(theme.colors);
    });
    let skin = TerminalSkin::for_spec(spec, &font_family, font_size);
    // Programs asking for their colours (OSC 10/11/12) are answered from here.
    palette::set_defaults(skin.colors);
    cx.set_global(skin);
    cx.set_global(CurrentTheme { spec: spec.clone() });
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
    use gpui_kit::Hsla;
    use tethys_core::theme::ThemeSpec;

    use super::{TerminalSkin, colour, font_size_of, packed, raise, tracked};
    use tethys_adapters::agent_terminal::palette;

    #[test]
    fn tracks_labels() {
        assert_eq!(tracked("Recent projects"), "R E C E N T   P R O J E C T S");
        assert_eq!(tracked("files"), "F I L E S");
    }

    #[test]
    fn raising_steps_away_from_the_backdrop() {
        let dark = Hsla {
            h: 0.7,
            s: 1.,
            l: 0.06,
            a: 1.,
        };
        assert!(raise(dark, 0.1).l > dark.l);
        let light = Hsla { l: 0.95, ..dark };
        assert!(raise(light, 0.1).l < light.l);
        assert_eq!(raise(Hsla { l: 0., ..dark }, 0.1).l, 0.1);
    }

    #[test]
    fn terminals_follow_a_theme_only_when_asked() {
        let mut spec = ThemeSpec {
            background: "#ffffff".into(),
            foreground: "#101010".into(),
            accent: "#00ff00".into(),
            ..ThemeSpec::builtin()
        };
        assert_eq!(
            TerminalSkin::for_spec(&spec, "Consolas", 18.),
            TerminalSkin::default()
        );
        spec.terminal = true;
        let skin = TerminalSkin::for_spec(&spec, "Consolas", 18.);
        assert_eq!(skin.colors.foreground, palette::rgb(0x101010));
        assert_eq!(skin.colors.cursor, palette::rgb(0x00ff00));
        // A light background lifts downwards: the pane is a touch darker.
        assert!(skin.colors.background.r < 255);
        assert_eq!(skin.font_family, "Consolas");
        assert_eq!(skin.font_size, 16.);
    }

    #[test]
    fn font_sizes_must_be_finite_and_in_range() {
        assert_eq!(font_size_of(f32::NAN), 16.);
        assert_eq!(font_size_of(f32::INFINITY), 16.);
        assert_eq!(font_size_of(2.), ThemeSpec::MIN_FONT_SIZE);
        assert_eq!(font_size_of(14.), 14.);
    }

    #[test]
    fn packing_round_trips() {
        for rgb in [0x0b001f, 0xf24b38, 0xffffff, 0x000000, 0xc8b8d8] {
            assert_eq!(packed(gpui_kit::rgb(rgb).into()), rgb);
        }
    }

    #[test]
    fn bad_colours_fall_back() {
        let builtin = ThemeSpec::builtin();
        let accent: Hsla = gpui_kit::rgb(0xf24b38).into();
        assert_eq!(colour("not a colour", &builtin.accent), accent);
        assert_eq!(colour("#F24B38", &builtin.accent), accent);
    }
}
