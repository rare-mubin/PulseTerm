//! The home screen -- the design's launcher (frame 06, "nothing open").
//!
//! Two halves beside the navigation bar, which stays: it is the window's
//! caption and its buttons, and its state word says IDLE.
//!
//! * **Start**, on the left: the three ways in, each a numbered entry with a
//!   heading, what it needs, and what it would reach -- your last SSH host,
//!   your local shell. The numerals are display-sized because they are the
//!   thing you aim at.
//! * **Resume**, on the right: what space would bring back -- the target, how
//!   many columns and windows, when it was closed, and the strip drawn small.
//!   Then **time**: three graduated dials standing on end, the reading where
//!   they cross the middle.
//!
//! ```text
//!     --  10   --  45   --  40
//!     -        -        -
//!     05    :  45    :  05
//!     -        -        -
//!     --  05   --  40   --  35
//! ```
//!
//! Each dial is a real scale: long graduations every five with their number,
//! short ones between, sliding at the rate of its own unit -- one graduation a
//! second on the right, a minute in the middle, an hour on the left. So the
//! seconds dial visibly runs and the hours dial is all but still. `scale` is
//! handed the live value, fraction and all; handing it `seconds / 60.` makes
//! a dial that moves a graduation a minute, which was the first thing tried.
//!
//! **It animates, so it asks for a frame at a time.** `request_animation_frame`
//! at the end of every render is the whole loop; the moment home closes nothing
//! asks again.
//!
//! The dials are positioned by arithmetic against their panel's laid-out size,
//! measured by a `canvas` into `Workspace::home_dials`. Home repaints every
//! frame, so the measurement is never more than a frame old -- and never
//! visibly so.
//!
//! A free function against the workspace, like `nav_bar`: everything it
//! draws already lives on the workspace or in a global.

use std::sync::OnceLock;
use std::time::SystemTime;

use gpui::{
    canvas, div, prelude::*, px, relative, AnyElement, ClickEvent, Context, FontWeight, Hsla,
    Pixels, SharedString, Window,
};

use super::kit;
use crate::session::SavedTarget;
use crate::store::{Profiles, Settings};
use crate::theme::Theme;
use crate::workspace::Workspace;

/// What the RESUME block reports. Built by `Workspace::resume`, from the live
/// strip when a session is running and from last run's file otherwise.
pub struct Resume {
    pub target: SavedTarget,
    pub columns: usize,
    pub windows: usize,
    /// Each column's share of the viewport, and whether it had the focus --
    /// the strip, to be drawn small.
    pub shares: Vec<(f32, bool)>,
    /// When last run's session was closed; `None` for a live one.
    pub closed: Option<SystemTime>,
    /// A session is running behind this screen.
    pub live: bool,
}

/// A patch of the dial panel the graduations stay out of, because type is
/// drawn over it and text has no background to hide a graduation behind.
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

/// The design's spacing: an 8px rhythm (frame 01's grid), in the multiples the
/// launcher uses. Margins, not measurements -- the type sizes come from `kit`.
const GUTTER: f32 = 48.;
const STEP: f32 = 24.;

#[derive(Clone, Copy)]
struct Ink {
    bg: Hsla,
    raised: Hsla,
    border: Hsla,
    fg: Hsla,
    fg_muted: Hsla,
    fg_subtle: Hsla,
    accent: Hsla,
    micro: Pixels,
    head: Pixels,
    display: Pixels,
    line: Pixels,
    row: Pixels,
}

pub fn render(workspace: &Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> impl IntoElement {
    let ink = {
        let t = cx.global::<Theme>();
        Ink {
            bg: t.bg,
            raised: t.raised,
            border: t.border,
            fg: t.fg,
            fg_muted: t.fg_muted,
            fg_subtle: t.fg_subtle,
            accent: t.accent,
            micro: kit::micro(t),
            head: kit::head(t),
            display: kit::display(t),
            line: t.line_height,
            row: kit::row(t),
        }
    };

    // Focus lives here while home is up, so space and escape reach the `Home`
    // key context. Checked first: focusing unconditionally would steal it back
    // on every frame.
    // Not while the connect form is up: its field has the keyboard, and on
    // the frame it first appears it is not yet part of home's focus tree --
    // checking then took the keyboard straight back out of it.
    if workspace.home_form().is_none() && !workspace.home_focus().contains_focused(window, cx) {
        window.focus(workspace.home_focus());
    }
    // The whole animation loop. Not while the window is in the background: a
    // clock nobody is looking at is not worth a frame every 16 ms, and gpui
    // repaints on activation anyway.
    if window.is_window_active() {
        window.request_animation_frame();
    }

    let resume = workspace.resume(cx);
    let start = start_half(ink, workspace, cx);
    let right = resume_half(workspace, resume, ink, cx);

    div()
        .key_context("Home")
        .track_focus(workspace.home_focus())
        // The one ground on this screen -- on a translucent window, the single
        // layer the desktop shows through (see `Theme::translucent`).
        .bg(ink.bg)
        .size_full()
        .flex()
        .flex_row()
        .overflow_hidden()
        .child(start)
        .child(right)
}

/// The left half: START, and the three ways in.
fn start_half(ink: Ink, workspace: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let ssh_open = workspace.home_ssh_shown();
    let ssh_detail = Profiles::latest(cx).unwrap_or_default();
    let hosts = ssh_list(ink, ssh_open, workspace, cx);
    let local_detail = format!("{} \u{b7} {}", shell_name(cx), user_at_host());

    div()
        .w(relative(0.44))
        .flex_none()
        .h_full()
        .flex()
        .flex_col()
        .px(px(GUTTER))
        .pt(px(GUTTER))
        .border_r_1()
        .border_color(ink.border)
        .child(section_head("start", "01\u{2013}03", ink))
        .child(
            entry(
                "home-ssh",
                1,
                // The expander says the entry opens in place, the way the
                // file tree's folders do.
                if ssh_open > 0.5 {
                    "ssh  \u{2212}"
                } else {
                    "ssh  +"
                },
                "remote host / password or key",
                ssh_detail,
                ink,
            )
            .on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_home_ssh(window, cx)),
            ),
        )
        .child(hosts)
        .child(
            entry(
                "home-local",
                2,
                "this computer",
                "local shell / no connection",
                local_detail,
                ink,
            )
            .on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.connect_local(window, cx)),
            ),
        )
        .child(
            entry(
                "home-settings",
                3,
                "settings",
                "appearance / tiling / keys",
                String::new(),
                ink,
            )
            .on_click(
                cx.listener(|this, _: &ClickEvent, window, cx| this.open_settings(window, cx)),
            ),
        )
        .into_any_element()
}

/// SSH's list, opened under its entry: the saved hosts, newest first, then a
/// row for a new one. Its height slides with `open` (0 to 1) and clips what
/// does not fit yet, so the entries below are pushed down rather than drawn
/// over. Picking a row slides a connect form open under it (`form`): a saved
/// host asks for its password or passphrase, a new connection for the address
/// first. Each saved host has an x that forgets it.
fn ssh_list(ink: Ink, open: f32, workspace: &Workspace, cx: &mut Context<Workspace>) -> AnyElement {
    let mut hosts: Vec<(u64, String, u64)> = cx
        .global::<Profiles>()
        .items
        .iter()
        .map(|p| (p.id, p.label(), p.last_used))
        .collect();
    hosts.sort_by(|a, b| b.2.cmp(&a.2));
    hosts.truncate(8);

    let font = cx.global::<Theme>().font_size;
    let form = workspace
        .home_form()
        .map(|(at, input, label, status)| (at, input.clone(), label, status));
    let form_open = workspace.home_form_shown();
    // Label, then the field under it at a comfortable height, then the
    // status line: the form's full height, which its slide runs to.
    let field_h = ink.row + px(8.);
    let form_h = ink.line + px(6.) + field_h + px(6.) + ink.line + px(20.);
    let picked = form.as_ref().map(|f| f.0);

    let row = |id: gpui::ElementId, label: String, chosen: bool| {
        let hover = ink.raised;
        div()
            .id(id)
            .relative()
            .h(ink.row)
            .flex_none()
            .flex()
            .items_center()
            .pl(px(STEP))
            .pr(px(8.))
            .gap(px(12.))
            .border_b_1()
            .border_color(ink.border)
            .text_color(if chosen { ink.fg } else { ink.fg_muted })
            .when(chosen, |r| r.bg(ink.raised))
            .hover(move |s| s.bg(hover).text_color(ink.fg))
            // Just the host: the form says what it needs once a row is
            // picked, so the row gives every pixel to the name.
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(2.))
                    .bg(ink.accent)
                    .when(!chosen, |d| d.invisible()),
            )
    };

    // The form under the chosen row: what it asks, the field, and a line for
    // the error or the connect's progress. Its own key context, so Space
    // types and Escape folds it (main.rs).
    let form_block = |cx: &mut Context<Workspace>| -> AnyElement {
        let Some((_, input, label, status)) = form.clone() else {
            return div().into_any_element();
        };
        div()
            .key_context("HomeForm")
            .on_action(
                cx.listener(|this, _: &crate::workspace::CloseHomeForm, window, cx| {
                    this.home_form_close(window, cx)
                }),
            )
            .flex_none()
            .h(form_h * form_open)
            .overflow_hidden()
            .border_b_1()
            .border_color(ink.border)
            .child(
                div()
                    .pl(px(STEP))
                    .pr(px(8.))
                    .pt(px(10.))
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .child(
                        div()
                            .h(ink.line)
                            .flex()
                            .items_end()
                            .text_size(ink.micro)
                            .text_color(ink.fg_subtle)
                            .child(kit::label(label)),
                    )
                    .child(
                        div().w_full().child(
                            gpui_component::input::Input::new(&input)
                                .text_size(font)
                                .h(field_h)
                                .w_full(),
                        ),
                    )
                    .child(
                        div()
                            .h(ink.line)
                            .truncate()
                            .text_size(ink.micro)
                            .text_color(match &status {
                                Some((_, true)) => ink.accent,
                                _ => ink.fg_subtle,
                            })
                            .child(match status {
                                Some((text, _)) => text,
                                None => "enter connects \u{b7} esc folds".to_string(),
                            }),
                    ),
            )
            .into_any_element()
    };

    let rows_h = ink.row * (hosts.len() + 1) as f32;
    let extra = if form.is_some() {
        form_h * form_open
    } else {
        px(0.)
    };
    let mut list = div()
        .flex_none()
        .h((rows_h + extra) * open)
        .overflow_hidden()
        .flex()
        .flex_col();
    for (id, label, _) in hosts {
        let chosen = picked == Some(Some(id));
        let subtle = ink.fg_subtle;
        let fg = ink.fg;
        list = list.child(
            row(("home-ssh-host", id as usize).into(), label, chosen)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.home_pick(Some(id), window, cx)
                }))
                .child(
                    // Forget this host. Its own click target, which stops
                    // there: the row under it would open the form.
                    div()
                        .id(("home-ssh-forget", id as usize))
                        .flex_none()
                        .w(px(22.))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_color(subtle)
                        .hover(move |s| s.text_color(fg))
                        .tooltip(|window, cx| {
                            gpui_component::tooltip::Tooltip::new("Forget this host")
                                .build(window, cx)
                        })
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            cx.stop_propagation();
                            this.forget_profile(id, window, cx);
                        }))
                        .child("\u{d7}"),
                ),
        );
        if chosen {
            list = list.child(form_block(cx));
        }
    }
    let new_chosen = picked == Some(None);
    list = list.child(
        row(
            "home-ssh-new".into(),
            "new connection\u{2026}".into(),
            new_chosen,
        )
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.home_pick(None, window, cx))),
    );
    if new_chosen {
        list = list.child(form_block(cx));
    }
    list.into_any_element()
}

/// The right half: RESUME, TIME and the footer.
fn resume_half(
    workspace: &Workspace,
    resume: Option<Resume>,
    ink: Ink,
    _cx: &mut Context<Workspace>,
) -> AnyElement {
    let now = chrono::Local::now();
    let date = format!(
        "{} \u{b7} {}",
        now.format("%d %b %Y").to_string().to_uppercase(),
        now.format("%A").to_string().to_uppercase()
    );
    let measured = workspace.home_dials();
    let bounds = measured.get();

    div()
        .flex_1()
        .min_w_0()
        .h_full()
        .flex()
        .flex_col()
        .px(px(GUTTER + 8.))
        .pt(px(GUTTER))
        .child(section_head(
            "resume",
            if resume.as_ref().is_some_and(|r| r.live) {
                "running"
            } else {
                "last session"
            },
            ink,
        ))
        .child(resume_block(resume, ink))
        .child(div().mt(px(STEP * 2.)).child(section_head("time", &date, ink)))
        .child(
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(
                    canvas(move |b, _, _| measured.set(b), |_, _, _, _| {})
                        .absolute()
                        .size_full(),
                )
                .child(dials(
                    f32::from(bounds.size.width),
                    f32::from(bounds.size.height),
                    now,
                    ink,
                )),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .justify_between()
                .gap_x_4()
                .gap_y_1()
                .py(px(12.))
                .border_t_1()
                .border_color(ink.border)
                .text_size(ink.micro)
                .text_color(ink.fg_subtle)
                // Two lines rather than one crowded one when the panel is
                // narrow: tracked type cannot be squeezed, only wrapped.
                .child(div().child(kit::label(&format!("pulseterm \u{b7} {}", env!("CARGO_PKG_VERSION")))))
                .child(div().child(kit::label("ctrl+shift+k \u{2014} command bar"))),
        )
        .into_any_element()
}

/// What space (or escape) would do, and what it would bring back.
fn resume_block(resume: Option<Resume>, ink: Ink) -> AnyElement {
    let Some(resume) = resume else {
        return div()
            .pt(px(STEP))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(ink.head * 1.5)
                    .font_weight(FontWeight::BOLD)
                    .text_color(ink.fg)
                    .child("nothing saved"),
            )
            .child(
                div()
                    .text_color(ink.fg_muted)
                    .child("Pick a way in on the left; the layout you leave is kept for next time."),
            )
            .into_any_element();
    };

    let title = match &resume.target {
        SavedTarget::Local => format!("{} \u{2014} local", user_at_host()),
        target => describe(target),
    };
    let mut detail = format!(
        "{} column{} \u{b7} {} window{}",
        resume.columns,
        if resume.columns == 1 { "" } else { "s" },
        resume.windows,
        if resume.windows == 1 { "" } else { "s" },
    );
    if let Some(closed) = resume.closed {
        let closed: chrono::DateTime<chrono::Local> = closed.into();
        detail.push_str(&format!(" \u{b7} closed {}", closed.format("%H:%M")));
    }
    let (key, action) = if resume.live {
        ("esc", "return")
    } else {
        ("space", "continue")
    };

    // The strip drawn small: one bar per column, its length its share.
    let total: f32 = resume.shares.iter().map(|(share, _)| share).sum::<f32>().max(1.);
    let strip = div().flex().gap(px(3.)).w(px(280.)).children(
        resume.shares.iter().enumerate().map(|(ix, (share, focus))| {
            div()
                .h(px(3.))
                .w(relative(share / total))
                .bg(if *focus {
                    ink.accent
                } else if ix % 2 == 0 {
                    ink.fg_muted
                } else {
                    ink.fg_subtle
                })
        }),
    );

    div()
        .pt(px(STEP))
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .text_size(ink.head * 1.5)
                .font_weight(FontWeight::BOLD)
                .text_color(ink.fg)
                .child(title),
        )
        .child(div().text_color(ink.fg_muted).child(detail))
        .child(strip)
        .child(
            div()
                .mt(px(8.))
                .flex()
                .items_center()
                .gap(px(10.))
                .child(keycap(key, ink))
                .child(
                    div()
                        .text_size(ink.micro)
                        .text_color(ink.fg)
                        .child(kit::label(action)),
                ),
        )
        .into_any_element()
}

/// The three dials, laid out in a panel `w` by `h`.
fn dials(w: f32, h: f32, now: chrono::DateTime<chrono::Local>, ink: Ink) -> AnyElement {
    // The first frame has no measurement yet; the next one draws for real.
    if w <= 0. || h <= 0. {
        return div().into_any_element();
    }
    let hour = now.format("%H").to_string();
    let minute = now.format("%M").to_string();
    let second = now.format("%S").to_string();
    // Sub-second, or the dials would step instead of slide.
    let within = now.timestamp_subsec_millis() as f32 / 1000.;
    let seconds: f32 = second.parse().unwrap_or(0.) + within;
    let minutes: f32 = minute.parse().unwrap_or(0.) + seconds / 60.;
    let hours: f32 = hour.parse().unwrap_or(0.) + minutes / 60.;

    // Each dial is one centre line: its graduations, its reading and its
    // unit label all sit on it, so the three read as one instrument each.
    // The scale's numbers sit just right of the graduations, which is what a
    // column needs room for beyond the reading -- 52px either side of centre.
    const HALF: f32 = 52.;
    // Sized by both axes: height so a short window does not grow digits into
    // the labels, width so three columns and two gaps fit in 92% of the panel.
    // Two monospace digits are about 1.2 of the type size; 1.4 leaves air.
    let across = (w * 0.92 / (1.4 * 3. + 0.5 * 2.)).max(16.);
    let digit = (h * 0.16).min(across).clamp(16., 96.);
    let line = f32::from(ink.line);
    let middle = h * 0.5;
    let column = (digit * 1.4).max(HALF * 2.);
    let gap = ((w * 0.92 - column * 3.) / 2.).clamp(8., digit * 0.8);
    let block = column * 3. + gap * 2.;
    let origin = ((w - block) / 2.).max(0.);
    let centre = move |ix: f32| origin + column / 2. + ix * (column + gap);
    let scale_x = centre;
    let reading_x = centre;
    let pitch = (h * 0.072).max(18.);

    let reading_band = Clear {
        x: origin,
        y: middle - digit * 0.75,
        w,
        h: digit * 1.5,
    };
    let date_band = Clear {
        x: 0.,
        y: h - line * 2.2,
        w,
        h: line * 2.2,
    };
    let clear = [reading_band, date_band];

    let reading = move |ix: f32, value: String| {
        div()
            .absolute()
            .left(px(reading_x(ix) - digit * 0.7))
            .top(px(middle - digit * 0.65))
            .w(px(digit * 1.4))
            .text_center()
            .text_size(px(digit))
            .font_weight(FontWeight::BOLD)
            .text_color(ink.fg)
            .child(value)
    };
    let colon = move |x: f32| {
        div()
            .absolute()
            .left(px(x - digit * 0.2))
            .top(px(middle - digit * 0.5))
            .w(px(digit * 0.4))
            .text_center()
            .text_size(px(digit * 0.7))
            .text_color(ink.accent)
            .child(":")
    };
    let unit = move |ix: f32, text: &str| {
        div()
            .absolute()
            .left(px(reading_x(ix) - 60.))
            .bottom(px(line * 0.4))
            .w(px(120.))
            .text_center()
            .text_size(ink.micro)
            .text_color(ink.fg_subtle)
            .child(kit::label(text))
    };

    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .child(scale(scale_x(0.), hours, 24, h, middle, pitch, clear, ink.fg_subtle))
        .child(scale(scale_x(1.), minutes, 60, h, middle, pitch, clear, ink.fg_subtle))
        .child(scale(scale_x(2.), seconds, 60, h, middle, pitch, clear, ink.fg_subtle))
        .child(reading(0., hour))
        .child(reading(1., minute))
        .child(reading(2., second))
        // Midway between neighbouring centres.
        .child(colon((centre(0.) + centre(1.)) / 2.))
        .child(colon((centre(1.) + centre(2.)) / 2.))
        .child(unit(0., "hours"))
        .child(unit(1., "minutes"))
        .child(unit(2., "seconds"))
        .into_any_element()
}

/// One dial, standing on end: a graduation per unit down the panel, long and
/// numbered every fifth, sliding upward as the value runs.
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
                if y < 0. || y > h || clear.iter().any(|c| c.covers(x, y)) {
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
                // Faded by distance from the reading, but only so far -- a
                // hairline at a fifth opacity is a smudge, not a graduation.
                let fade = 1. - ((y - middle).abs() / (h * 0.55)).clamp(0., 0.6);

                let graduation = div()
                    .absolute()
                    // Long and short alike centred on the dial's line.
                    .left(px(x - width / 2.))
                    .top(px(y))
                    .w(px(width))
                    .h(px(if major { 2. } else { 1. }))
                    .bg(ink.opacity(fade))
                    .into_any_element();

                // Numbered on the outside of the long graduations, as a scale
                // is; checked against the keep-outs at its own position.
                let label = (major && !clear.iter().any(|c| c.covers(x + 30., y))).then(|| {
                    div()
                        .absolute()
                        .left(px(x + 26.))
                        .top(px(y - 8.))
                        .text_color(ink.opacity(fade * 0.85))
                        .child(format!("{reading:02}"))
                        .into_any_element()
                });

                [Some(graduation), label].into_iter().flatten()
            }),
    )
}

/// A section's head: its name on the left, a reading on the right, a hairline
/// under both -- START / 01-03, RESUME / LAST SESSION, TIME / the date.
fn section_head(name: &str, reading: &str, ink: Ink) -> AnyElement {
    div()
        .flex()
        .justify_between()
        .items_center()
        .pb(px(14.))
        .border_b_1()
        .border_color(ink.border)
        .text_size(ink.micro)
        .child(div().text_color(ink.fg).child(kit::label(name)))
        .child(div().text_color(ink.fg_subtle).child(kit::label(reading)))
        .into_any_element()
}

/// One way in: heading, what it needs, what it would reach, and its numeral.
fn entry(
    id: &'static str,
    number: usize,
    heading: &str,
    needs: &str,
    detail: String,
    ink: Ink,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .relative()
        .flex()
        .flex_col()
        .gap(px(6.))
        .py(px(STEP + 4.))
        .min_h(ink.display * 2.4)
        .border_b_1()
        .border_color(ink.border)
        .hover(move |this| this.bg(ink.raised.opacity(0.5)))
        .child(
            div()
                .text_size(ink.head)
                .font_weight(FontWeight::BOLD)
                .text_color(ink.fg)
                .child(heading.to_uppercase()),
        )
        // The words keep clear of the numeral and give way on one line
        // rather than wrapping into it: in a narrow window the tracked
        // subtitle is the first thing to run out of room.
        // Padding outside, truncation inside: gpui clips at the padding's
        // outer edge, so a truncating box with its own padding still lets
        // the text run under the numeral.
        .child(
            div().pr(ink.display * 1.4).child(
                div()
                    .truncate()
                    .text_size(ink.micro)
                    .text_color(ink.fg_subtle)
                    .child(kit::label(needs)),
            ),
        )
        .when(!detail.is_empty(), |this| {
            this.child(
                div().mt(px(4.)).pr(ink.display * 1.4).child(
                    div().truncate().text_color(ink.fg_muted).child(detail),
                ),
            )
        })
        .child(
            div()
                .absolute()
                .right_0()
                .bottom(px(STEP * 0.6))
                .text_size(ink.display)
                .font_weight(FontWeight::BOLD)
                .text_color(ink.fg)
                .child(format!("{number:02}")),
        )
}

fn keycap(text: &str, ink: Ink) -> AnyElement {
    div()
        .px(px(10.))
        .py(px(3.))
        .border_1()
        .border_color(ink.fg_subtle)
        .text_size(ink.micro)
        .text_color(ink.fg_muted)
        .child(kit::label(text))
        .into_any_element()
}

/// The local shell as the entry names it: the configured one's file name, or
/// the session's own -- no probing, because this is on the first frame and the
/// startup budget allows no scan.
fn shell_name(cx: &Context<Workspace>) -> String {
    let configured = cx.global::<Settings>().local_shell.clone();
    let path = if configured.is_empty() {
        std::env::var("SHELL")
            .or_else(|_| std::env::var("ComSpec"))
            .unwrap_or_default()
    } else {
        configured
    };
    path.rsplit(['/', '\\'])
        .next()
        .map(|name| name.trim_end_matches(".exe").to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "shell".to_string())
}

/// `user@host`, worked out once: environment variables and one system call,
/// never a file, for the same startup reason as `shell_name`.
fn user_at_host() -> SharedString {
    static ONCE: OnceLock<SharedString> = OnceLock::new();
    ONCE.get_or_init(|| {
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "user".into());
        format!("{user}@{}", host_name()).into()
    })
    .clone()
}

#[cfg(target_os = "linux")]
fn host_name() -> String {
    let mut buffer = [0u8; 256];
    let rc = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) };
    if rc != 0 {
        return "localhost".into();
    }
    let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
    String::from_utf8_lossy(&buffer[..end]).into_owned()
}

#[cfg(not(target_os = "linux"))]
fn host_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "localhost".into())
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
