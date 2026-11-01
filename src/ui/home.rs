//! The home screen: three scrolling tapes, and the three ways into the app.
//!
//! It replaces the whole window rather than floating over it -- no title bar,
//! no rail, no status line, no tabs. That is the point of it: the app opens on
//! something calm, and every piece of chrome exists to serve a session that
//! hasn't started yet.
//!
//! The shape is three graduated dials standing on end, over on the left of the
//! window, with the reading where they cross the middle:
//!
//! ```text
//!     --  10   --  45   --  40
//!     -        -        -
//!     -        -        -
//!     05    :  45    :  05
//!     -        -        -
//!     --  05   --  40   --  35
//! ```
//!
//! Each is a real scale rather than a row of dashes: long graduations every
//! five with their number beside them, short ones between, sliding upward at
//! the rate of the unit underneath. The seconds dial visibly runs, the minutes
//! dial creeps, the hours dial is all but still, and the reading never moves.
//!
//! **It animates, so it asks for a frame at a time.** `request_animation_frame`
//! at the end of every render is the whole loop; the moment home closes nothing
//! asks again, and the window goes back to painting only on change.
//!
//! A free function against the workspace, like `nav_bar`:
//! everything it draws already lives on the workspace or in a global, so a view
//! of its own would only add a subscription to keep in sync.

use gpui::{
    div, prelude::*, px, ClickEvent, Context, FontWeight, Hsla, Pixels, SharedString, Window,
};

use crate::session::SavedTarget;
use crate::theme::Theme;
use crate::workspace::Workspace;

/// A patch of window the dials stay out of, because type is drawn over it and
/// text has no background to hide a graduation behind.
#[derive(Clone, Copy, Default)]
struct Clear {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Clear {
    fn covers(&self, x: f32, y: f32) -> bool {
        const MARGIN: f32 = 6.;
        x > self.x - MARGIN
            && x < self.x + self.w + MARGIN
            && y > self.y - MARGIN
            && y < self.y + self.h + MARGIN
    }
}

pub fn render(
    workspace: &Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let (bg, fg, fg_subtle, accent, danger, font_size, line_height) = {
        let t = cx.global::<Theme>();
        (
            t.bg,
            t.fg,
            t.fg_subtle,
            t.accent,
            t.danger,
            t.font_size,
            t.line_height,
        )
    };

    let now = chrono::Local::now();
    let hour = now.format("%H").to_string();
    let minute = now.format("%M").to_string();
    let second = now.format("%S").to_string();
    let day = now.format("%d %b %Y").to_string().to_uppercase();
    let weekday = now.format("%A").to_string().to_uppercase();

    // Sub-second, or the tapes would step sixty times a minute instead of
    // moving. Each column slides by its own unit's share of the one below it.
    let within_second = now.timestamp_subsec_millis() as f32 / 1000.;
    let seconds: f32 = second.parse().unwrap_or(0.) + within_second;
    let minutes: f32 = minute.parse().unwrap_or(0.) + seconds / 60.;
    let hours: f32 = hour.parse().unwrap_or(0.) + minutes / 60.;

    // Laid out against the real window, so the columns keep their proportions
    // rather than their pixel sizes.
    let viewport = window.viewport_size();
    let (w, h) = (f32::from(viewport.width), f32::from(viewport.height));
    let caption_height = line_height + px(4.);

    // Focus lives here while home is up, so space and escape reach the `Home`
    // key context. Checked first: focusing unconditionally would steal it back
    // on every frame.
    if !workspace.home_focus().is_focused(window) {
        window.focus(workspace.home_focus());
    }

    // Monospace makes width arithmetic a multiplication, which is the only
    // reason any of this can be decided before laying anything out.
    let cell_w = f32::from(font_size) * 0.62;
    // The ways in are a column at every size (see `way_in`), so this is only
    // about how much room the words themselves have: below it the tracking
    // comes off and the type steps down.
    let compact = w < 900.;
    // Sized by *both* axes. Height alone made the type outgrow a short, narrow
    // window -- the date ran off the left edge before anything else broke.
    let digit_size = px((h * 0.11).min(w * 0.085).clamp(28., 120.));
    let digit_px = f32::from(digit_size);
    // Two monospace digits, near enough: the dials are positioned from this
    // rather than measured, and their graduations line up with the numbers.
    let cell = digit_px * 1.3;
    let gap = digit_px * 0.85;
    let span = cell * 3. + gap * 2.;
    // Over on the left, not centred: the reading and the dials are one block,
    // and the ways in have the other side of the window to themselves.
    let left = w * 0.08;
    let column_at = move |ix: f32| left + cell * (ix + 0.5) + gap * ix;
    let middle = h * 0.5;
    // Fine enough that a dial reads as a scale rather than as a few dashes:
    // thirty-odd graduations are on screen at once.
    let pitch = (h * 0.032).max(26.);

    // Two patches of window the dials stay out of: the reading with the date
    // under it, and the block of ways in down in the corner. The second only
    // matters once the window is narrow enough for the two to share a column,
    // but it is measured the same way either way.
    let reading_clear = Clear {
        x: 0.,
        y: middle - digit_px * 0.9,
        w: left + span + digit_px * 4.,
        h: digit_px * 2.4,
    };
    // Measured from the type rather than guessed at: a constant that was right
    // at one font size put a dial straight through the text at another. The
    // widest line is the host name under "continue", not one of the actions,
    // since it is set larger and tracked.
    let (block_w, block_h) = if compact {
        (cell_w * 22., f32::from(line_height) * 10.)
    } else {
        (cell_w * 1.4 * 44., f32::from(line_height) * 13.)
    };
    let block_clear = Clear {
        x: w - w * 0.06 - block_w,
        y: h - h * 0.1 - block_h,
        w: block_w + w * 0.06,
        h: block_h + h * 0.1,
    };
    let clear = [reading_clear, block_clear];

    // The date, and whether it fits under the reading. The separator and its
    // two gaps are the +3.
    let (date_day, date_weekday) = if compact {
        (
            SharedString::from(day.clone()),
            SharedString::from(weekday.clone()),
        )
    } else {
        (spaced(&day), spaced(&weekday))
    };
    let date_fits =
        (date_day.chars().count() + date_weekday.chars().count() + 3) as f32 * cell_w < span;

    let dial = move |ix: f32, value: f32, modulus: i32| {
        scale(
            column_at(ix),
            value,
            modulus,
            h,
            middle,
            pitch,
            clear,
            fg_subtle,
        )
    };
    let reading = move |ix: f32, value: String| {
        div()
            .absolute()
            .left(px(column_at(ix) - cell / 2.))
            .top(px(middle - digit_px * 0.72))
            .w(px(cell))
            .text_center()
            .text_size(digit_size)
            .font_weight(FontWeight::BLACK)
            .text_color(fg)
            .child(value)
    };
    let colon = move |x: f32| {
        div()
            .absolute()
            .left(px(x - digit_px * 0.2))
            .top(px(middle - digit_px * 0.6))
            .w(px(digit_px * 0.4))
            .text_center()
            .text_size(digit_size * 0.7)
            .text_color(accent)
            .child(":")
    };

    // Another frame, please: this is the whole animation loop. Nothing else on
    // this screen changes by itself, and nothing asks once home is gone.
    //
    // Not while the window is in the background, though: a clock nobody is
    // looking at is not worth a frame every sixteen milliseconds, and gpui
    // repaints on activation anyway, so the tapes are already where they belong
    // by the time the window is back in front.
    if window.is_window_active() {
        window.request_animation_frame();
    }

    div()
        .key_context("Home")
        .track_focus(workspace.home_focus())
        // The one ground on this screen. On a translucent window nothing else
        // paints a background, so this is the single layer the desktop shows
        // through -- see `Theme::translucent`.
        .bg(bg)
        .size_full()
        .relative()
        .overflow_hidden()
        // The value itself, fraction and all: a dial slides by exactly one
        // graduation per unit -- one a second on the right, one a minute in the
        // middle, one an hour on the left -- and numbers its own scale from it.
        .child(dial(0., hours, 24))
        .child(dial(1., minutes, 60))
        .child(dial(2., seconds, 60))
        .child(reading(0., hour))
        .child(reading(1., minute))
        .child(reading(2., second))
        .child(colon(left + cell + gap * 0.5))
        .child(colon(left + cell * 2. + gap * 1.5))
        // Centred under the reading, in a box exactly as wide as it -- unless
        // the line is wider than the reading, in which case centring would
        // hang it off both ends and the left end is the one that falls off the
        // screen. Then it starts where the reading does instead. Monospace is
        // what makes that decidable without laying anything out first.
        .child(
            div()
                .absolute()
                .left(px(left))
                .top(px(middle + digit_px * 0.85))
                .w(px(span))
                .flex()
                .items_center()
                .when(date_fits, |this| this.justify_center())
                .gap(px(10.))
                .text_color(fg_subtle)
                .child(date_day)
                .child(div().child("·"))
                .child(date_weekday),
        )
        .child(super::nav_bar::home_chrome(
            caption_height,
            fg,
            fg_subtle,
            danger,
            window,
            cx,
        ))
        .child(way_in(
            workspace, w, h, compact, font_size, fg, fg_subtle, cx,
        ))
}

/// One dial, standing on end: a graduation per unit down the whole window,
/// long and numbered every fifth, sliding upward as the value runs.
///
/// `value` is the live reading, fraction and all. Its whole part decides what
/// each graduation is called and its fraction is how far the scale has slid, so
/// the mark passing the middle is always the one the reading replaces.
#[allow(clippy::too_many_arguments)]
fn scale(
    x: f32,
    value: f32,
    modulus: i32,
    h: f32,
    middle: f32,
    pitch: f32,
    clear: [Clear; 2],
    ink: Hsla,
) -> impl IntoElement {
    // One past the edge at each end, so a graduation is never seen arriving.
    let reach = (h / pitch / 2.).ceil() as i32 + 1;
    let whole = value.floor() as i32;
    let slide = value - value.floor();

    div().absolute().inset_0().children(
        (-reach..=reach)
            .filter_map(move |step| {
                let y = middle + (step as f32 - slide) * pitch;
                if y < -pitch || y > h + pitch || clear.iter().any(|c| c.covers(x, y)) {
                    return None;
                }
                // `rem_euclid`, not `%`: the scale runs backwards past zero too,
                // and there is no minute minus one.
                let reading = (whole + step).rem_euclid(modulus);
                Some((y, reading))
            })
            .flat_map(move |(y, reading)| {
                let major = reading % 5 == 0;
                let width = if major { 34. } else { 14. };
                // Faded by distance from the reading: a dial thins out toward
                // the top and bottom instead of stopping dead. Only so far,
                // though -- a hairline at a fifth opacity is a smudge.
                let fade = 1. - ((y - middle).abs() / (h * 0.55)).clamp(0., 0.6);

                let graduation = div()
                    .absolute()
                    .left(px(x - width / 2.))
                    .top(px(y))
                    .w(px(width))
                    .h(px(if major { 2.5 } else { 1.5 }))
                    .bg(ink.opacity(fade))
                    .into_any_element();

                // Numbered on the outside of the long graduations, the way a
                // scale is: the short ones between are just subdivision. The
                // number is checked against the keep-outs at its own position,
                // not the graduation's -- it sits a good way to the right.
                let label = (major && !clear.iter().any(|c| c.covers(x + 30., y))).then(|| {
                    div()
                        .absolute()
                        .left(px(x + 26.))
                        .top(px(y - 9.))
                        .text_color(ink.opacity(fade * 0.85))
                        .child(format!("{reading:02}"))
                        .into_any_element()
                });

                [Some(graduation), label].into_iter().flatten()
            }),
    )
}

/// The top strip: the mark, and the window buttons.
///
/// The chrome is hidden, not abolished. A window that cannot be moved,
/// minimised or closed is not minimal, it is stuck -- so the caption's two jobs
/// survive here as unpainted space that drags and three buttons in the corner.
/// The bottom right: the three ways in, and what one key will do.
///
/// They sit together down here because this corner is where the eye ends up,
/// and because they are one decision -- the lock screen this is shaped after
/// puts its password field in the same place.
#[allow(clippy::too_many_arguments)]
fn way_in(
    workspace: &Workspace,
    w: f32,
    h: f32,
    compact: bool,
    font_size: Pixels,
    fg: Hsla,
    fg_subtle: Hsla,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let (caption, target, key) = match (workspace.is_connected(), workspace.saved_target()) {
        // Reached from the rail while a session is running: home is a screen
        // you step onto and back off.
        (true, _) => ("RETURN TO", "YOUR SESSION".to_string(), "ESC"),
        (false, Some(target)) => ("CONTINUE", describe(&target).to_uppercase(), "SPACE"),
        (false, None) => ("NOTHING SAVED", "PICK A HOST".to_string(), ""),
    };

    // Tracking is the first thing to go when the window narrows: it is what
    // makes this row half again as wide as the words in it, and the row is what
    // runs into the clock.
    let action = move |id: &'static str, label: &'static str| {
        div()
            .id(id)
            .px(px(if compact { 0. } else { 10. }))
            .py(px(2.))
            .text_size(font_size * if compact { 1. } else { 1.15 })
            .text_color(fg_subtle)
            .hover(move |s| s.text_color(fg))
            .child(if compact {
                SharedString::from(label)
            } else {
                spaced(label)
            })
    };

    div()
        .absolute()
        .right(px(w * 0.06))
        .bottom(px(h * 0.1))
        .flex()
        .flex_col()
        .items_end()
        .gap(px(10.))
        // One under another at every size. Abreast they were a line across the
        // window that grew with the font and had to be measured against the
        // clock; stacked they are a corner, and the corner is where the eye
        // already is.
        .child(
            div()
                .flex()
                .flex_col()
                .items_end()
                .gap(px(2.))
                .pb(px(12.))
                .child(
                    action("home-ssh", "CONNECT SSH").on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| this.open_connections(window, cx),
                    )),
                )
                .child(
                    action("home-local", "THIS COMPUTER").on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| this.connect_local(window, cx),
                    )),
                )
                .child(
                    action("home-settings", "SETTINGS").on_click(cx.listener(
                        |this, _: &ClickEvent, window, cx| this.open_settings(window, cx),
                    )),
                ),
        )
        .child(div().text_color(fg_subtle).child(if compact {
            SharedString::from(caption)
        } else {
            spaced(caption)
        }))
        .child(
            div()
                .text_size(font_size * if compact { 1.1 } else { 1.4 })
                .font_weight(FontWeight::BOLD)
                .text_color(fg)
                .child(if compact {
                    SharedString::from(target.clone())
                } else {
                    spaced(&target)
                }),
        )
        .when(!key.is_empty(), |this| {
            this.child(
                div()
                    .mt(px(4.))
                    .px(px(8.))
                    .py(px(2.))
                    .border_1()
                    .border_color(fg_subtle)
                    .text_color(fg_subtle)
                    .child(spaced(key)),
            )
        })
}

/// Wide letter spacing, the hard way: gpui has no tracking, so the string
/// carries it. Display text only -- nothing is ever parsed back out of one.
fn spaced(text: &str) -> SharedString {
    let mut out = String::with_capacity(text.len() * 2);
    for (ix, ch) in text.chars().enumerate() {
        if ix > 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    out.into()
}

fn describe(target: &SavedTarget) -> String {
    match target {
        SavedTarget::Local => "this computer".to_string(),
        SavedTarget::Ssh {
            host,
            port,
            username,
        } if *port == 22 => format!("{username}@{host}"),
        SavedTarget::Ssh {
            host,
            port,
            username,
        } => format!("{username}@{host}:{port}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracking_is_spelled_into_the_string() {
        assert_eq!(spaced("MONDAY"), SharedString::from("M O N D A Y"));
        assert_eq!(spaced(""), SharedString::from(""));
    }
}
