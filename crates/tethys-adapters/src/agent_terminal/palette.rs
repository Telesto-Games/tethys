//! Default terminal colours, used when the program hasn't overridden them.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

const fn rgb(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

/// ANSI 0-15 (a dark theme based on Zed's One Dark).
const BASE: [Rgb; 16] = [
    rgb(0x282c34),
    rgb(0xe06c75),
    rgb(0x98c379),
    rgb(0xe5c07b),
    rgb(0x61afef),
    rgb(0xc678dd),
    rgb(0x56b6c2),
    rgb(0xabb2bf),
    rgb(0x5c6370),
    rgb(0xea858b),
    rgb(0xaad581),
    rgb(0xffd885),
    rgb(0x85c1ff),
    rgb(0xd398eb),
    rgb(0x6ed5de),
    rgb(0xfafafa),
];

pub const FOREGROUND: Rgb = rgb(0xdcdfe4);
pub const BACKGROUND: Rgb = rgb(0x1e2127);
pub const CURSOR: Rgb = rgb(0x74ade8);

/// The default colour for a palette index (0..269, see `NamedColor`).
pub fn default_color(index: usize) -> Rgb {
    match index {
        0..16 => BASE[index],
        16..232 => {
            let i = index - 16;
            let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            Rgb {
                r: level(i / 36),
                g: level((i / 6) % 6),
                b: level(i % 6),
            }
        }
        232..256 => {
            let v = (8 + (index - 232) * 10) as u8;
            Rgb { r: v, g: v, b: v }
        }
        i if i == NamedColor::Foreground as usize => FOREGROUND,
        i if i == NamedColor::Background as usize => BACKGROUND,
        i if i == NamedColor::Cursor as usize => CURSOR,
        i if i == NamedColor::BrightForeground as usize => BASE[15],
        i if i == NamedColor::DimForeground as usize => dim(FOREGROUND),
        // DimBlack..=DimWhite
        i if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&i) => {
            dim(BASE[i - NamedColor::DimBlack as usize])
        }
        _ => FOREGROUND,
    }
}

fn dim(c: Rgb) -> Rgb {
    let f = |v: u8| (v as u16 * 2 / 3) as u8;
    Rgb {
        r: f(c.r),
        g: f(c.g),
        b: f(c.b),
    }
}

/// Resolves a cell colour against the terminal's overrides, then the defaults.
pub fn resolve(color: Color, overrides: &Colors) -> Rgb {
    match color {
        Color::Spec(rgb) => rgb,
        Color::Named(named) => overrides[named].unwrap_or_else(|| default_color(named as usize)),
        Color::Indexed(i) => overrides[i as usize].unwrap_or_else(|| default_color(i as usize)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_and_grays() {
        assert_eq!(default_color(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(
            default_color(231),
            Rgb {
                r: 255,
                g: 255,
                b: 255
            }
        );
        assert_eq!(default_color(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(
            default_color(255),
            Rgb {
                r: 238,
                g: 238,
                b: 238
            }
        );
    }

    #[test]
    fn named_specials() {
        assert_eq!(default_color(NamedColor::Background as usize), BACKGROUND);
        assert_eq!(default_color(NamedColor::DimRed as usize), dim(BASE[1]));
    }
}
