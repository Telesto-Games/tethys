//! Colour themes: a few colours and a typeface that the app turns into the
//! whole UI palette. Kept small on purpose: a theme is a backdrop, two text
//! colours, an accent and a font.

use serde::{Deserialize, Serialize};

/// One theme, as saved in `state.toml`. Colours are `#rrggbb`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThemeSpec {
    pub name: String,
    /// The window backdrop.
    #[serde(default = "defaults::background")]
    pub background: String,
    /// Ordinary text.
    #[serde(default = "defaults::foreground")]
    pub foreground: String,
    /// Quiet text: help, inactive tabs.
    #[serde(default = "defaults::muted_foreground")]
    pub muted_foreground: String,
    /// The one accent: primary buttons, links, hairlines, the caret.
    #[serde(default = "defaults::accent")]
    pub accent: String,
    /// The UI typeface.
    #[serde(default = "defaults::font_family")]
    pub font_family: String,
    /// The base font size in pixels; everything else scales from it.
    #[serde(default = "defaults::font_size")]
    pub font_size: f32,
    /// Whether terminals take the theme's background, text and accent colours
    /// and its font, instead of their own palette.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,
}

mod defaults {
    pub fn background() -> String {
        "#0b001f".into()
    }
    pub fn foreground() -> String {
        "#ece4f5".into()
    }
    pub fn muted_foreground() -> String {
        "#c8b8d8".into()
    }
    pub fn accent() -> String {
        "#f24b38".into()
    }
    pub fn font_family() -> String {
        "Cascadia Mono".into()
    }
    pub fn font_size() -> f32 {
        16.
    }
}

impl ThemeSpec {
    /// The name of the built-in theme, which can't be edited or deleted.
    pub const BUILTIN_NAME: &'static str = "Telesto";
    pub const MIN_FONT_SIZE: f32 = 8.;
    pub const MAX_FONT_SIZE: f32 = 40.;

    /// Tethys's own look, after the Telesto Games brand: plum, broadcast
    /// orange and lilac, in Cascadia Mono.
    pub fn builtin() -> Self {
        Self {
            name: Self::BUILTIN_NAME.into(),
            background: defaults::background(),
            foreground: defaults::foreground(),
            muted_foreground: defaults::muted_foreground(),
            accent: defaults::accent(),
            font_family: defaults::font_family(),
            font_size: defaults::font_size(),
            terminal: false,
        }
    }

    /// Whether this is the built-in theme (by name).
    pub fn is_builtin(&self) -> bool {
        self.name == Self::BUILTIN_NAME
    }

    /// A name for a copy of this theme that none of `taken` uses:
    /// "Telesto copy", then "Telesto copy 2", and so on.
    pub fn copy_name(&self, taken: &[String]) -> String {
        let base = format!("{} copy", self.name);
        if !taken.contains(&base) {
            return base;
        }
        (2..)
            .map(|n| format!("{base} {n}"))
            .find(|candidate| !taken.contains(candidate))
            .expect("some number is free")
    }
}

impl Default for ThemeSpec {
    fn default() -> Self {
        Self::builtin()
    }
}

/// Parses `#rrggbb` (or `rrggbb`) into `0xrrggbb`.
pub fn parse_hex(text: &str) -> Option<u32> {
    let digits = text.trim().trim_start_matches('#');
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

/// Formats `0xrrggbb` as `#rrggbb`.
pub fn hex(rgb: u32) -> String {
    format!("#{:06x}", rgb & 0xffffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips() {
        assert_eq!(parse_hex("#0B001F"), Some(0x0b001f));
        assert_eq!(parse_hex("f24b38"), Some(0xf24b38));
        assert_eq!(parse_hex(" #c8b8d8 "), Some(0xc8b8d8));
        assert_eq!(parse_hex("#fff"), None);
        assert_eq!(parse_hex("#gg0000"), None);
        assert_eq!(hex(0x0b001f), "#0b001f");
    }

    #[test]
    fn copy_names_avoid_taken_ones() {
        let t = ThemeSpec::builtin();
        assert_eq!(t.copy_name(&[]), "Telesto copy");
        let taken = ["Telesto copy".to_string(), "Telesto copy 2".to_string()];
        assert_eq!(t.copy_name(&taken), "Telesto copy 3");
    }

    #[test]
    fn missing_fields_fall_back_to_the_builtin() {
        let spec: ThemeSpec =
            serde_json::from_str(r##"{"name": "Mine", "accent": "#00ff00"}"##).unwrap();
        assert_eq!(spec.accent, "#00ff00");
        assert_eq!(spec.background, ThemeSpec::builtin().background);
        assert_eq!(spec.font_size, 16.);
        assert!(!spec.terminal);
    }
}
