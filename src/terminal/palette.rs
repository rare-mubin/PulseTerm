use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color as TermColor, NamedColor, Rgb};
use gpui::{rgb, Hsla};

use crate::theme::Theme;

/// Resolves an alacritty cell colour against the app theme.
///
/// Three sources, in priority order: a colour the remote program set itself via
/// OSC (`colors[index]`), then our theme's palette, then a computed value for
/// the 256-colour cube. Honouring the OSC overrides first is what lets remote
/// programs that theme themselves (vim colourschemes, `dircolors`) look right.
pub fn resolve(color: TermColor, theme: &Theme, colors: &Colors) -> Hsla {
    match color {
        TermColor::Spec(rgb) => from_rgb(rgb),
        TermColor::Named(named) => {
            if let Some(overridden) = colors[named as usize] {
                return from_rgb(overridden);
            }
            named_color(named, theme)
        }
        TermColor::Indexed(index) => {
            if let Some(overridden) = colors[index as usize] {
                return from_rgb(overridden);
            }
            indexed_color(index, theme)
        }
    }
}

fn named_color(named: NamedColor, theme: &Theme) -> Hsla {
    use NamedColor::*;
    match named {
        Foreground => theme.fg,
        Background => theme.bg,
        Cursor => theme.cursor,

        Black => theme.ansi[0],
        Red => theme.ansi[1],
        Green => theme.ansi[2],
        Yellow => theme.ansi[3],
        Blue => theme.ansi[4],
        Magenta => theme.ansi[5],
        Cyan => theme.ansi[6],
        White => theme.ansi[7],
        BrightBlack => theme.ansi[8],
        BrightRed => theme.ansi[9],
        BrightGreen => theme.ansi[10],
        BrightYellow => theme.ansi[11],
        BrightBlue => theme.ansi[12],
        BrightMagenta => theme.ansi[13],
        BrightCyan => theme.ansi[14],
        BrightWhite => theme.ansi[15],

        // SGR 2 (faint). Rendering it as the normal colour at reduced alpha
        // reads as dim against any background, which a fixed darker palette
        // would not.
        DimBlack => theme.ansi[0].opacity(0.65),
        DimRed => theme.ansi[1].opacity(0.65),
        DimGreen => theme.ansi[2].opacity(0.65),
        DimYellow => theme.ansi[3].opacity(0.65),
        DimBlue => theme.ansi[4].opacity(0.65),
        DimMagenta => theme.ansi[5].opacity(0.65),
        DimCyan => theme.ansi[6].opacity(0.65),
        DimWhite => theme.ansi[7].opacity(0.65),
        DimForeground => theme.fg.opacity(0.65),

        BrightForeground => theme.ansi[15],
    }
}

/// The xterm 256-colour layout: 0-15 are the palette, 16-231 a 6x6x6 RGB cube,
/// 232-255 a 24-step grey ramp.
fn indexed_color(index: u8, theme: &Theme) -> Hsla {
    match index {
        0..=15 => theme.ansi[index as usize],
        16..=231 => {
            let i = index - 16;
            let r = CUBE[(i / 36) as usize];
            let g = CUBE[((i % 36) / 6) as usize];
            let b = CUBE[(i % 6) as usize];
            from_rgb(Rgb { r, g, b })
        }
        232..=255 => {
            let level = 8 + (index - 232) * 10;
            from_rgb(Rgb {
                r: level,
                g: level,
                b: level,
            })
        }
    }
}

const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn from_rgb(c: Rgb) -> Hsla {
    rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32).into()
}
