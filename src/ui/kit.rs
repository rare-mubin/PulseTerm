//! The redesign's fixed metrics and the three type sizes, in one place.
//!
//! These are *measurements*, not components. The design fixes a handful of
//! numbers and derives everything else from the type, which is why they are
//! functions over the theme rather than constants: change the font size and
//! the whole UI steps with it, instead of the type growing inside boxes that
//! stay put. The two that genuinely are constants are the two the design
//! states as constants -- a hairline is one pixel, and there is no corner
//! radius and no shadow anywhere, ever.
//!
//! The scale is two sizes and no more: `micro` for labels, and the theme's
//! own `font_size` for everything else -- navigation, rows, controls and the
//! terminal grid alike, because it is one family at one size on one grid.
//! Anything that wants a third size wants a different design, which is why
//! the heading size that was once here is gone: nothing had a heading.

use gpui::{px, Pixels};

use crate::theme::Theme;

/// The navigation bar's thickness, and the one piece of chrome that is sized
/// rather than filled.
pub fn bar(theme: &Theme) -> Pixels {
    theme.line_height + px(26.)
}

/// A column header: the title strip over every window in the strip.
pub fn header(theme: &Theme) -> Pixels {
    theme.line_height + px(8.)
}

/// One row of a list -- the explorer's files, the command bar's results. Taller
/// than the text it holds, because a list is read by scanning down it and the
/// gaps are what the eye counts.
pub fn row(theme: &Theme) -> Pixels {
    theme.line_height + px(10.)
}

/// Label type: section names, key caps, readings, anything upper case. Small
/// enough to sit off the grid without competing with it.
pub fn micro(theme: &Theme) -> Pixels {
    (theme.font_size * 0.72).max(px(8.))
}

/// Letter-spaced text for labels, because **gpui has no letter spacing** and
/// the design's micro type is tracked out at 0.18em. Spelling the spaces into
/// the string is the whole trick.
///
/// Display only. Nothing is ever parsed back out of one of these, and nothing
/// is ever measured from one -- a spaced string is twice the characters, so a
/// layout that fits the text will not fit the spacing.
pub fn spaced(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for (ix, ch) in text.chars().enumerate() {
        if ix > 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// A label as the design sets them: upper case and tracked out.
pub fn label(text: &str) -> String {
    spaced(&text.to_uppercase())
}
