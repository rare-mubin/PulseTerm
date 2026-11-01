//! The one navigation bar.
//!
//! There is exactly one, and which edge it sits on is a setting
//! (`Settings::nav_edge`). It replaced three separate pieces of chrome -- a
//! title bar, an icon rail and a status line -- and **nothing may add a
//! second**: anything that wants to say something to the user says it here or
//! in a column header.
//!
//! That is not only tidiness. Three bars took three edges of the window and
//! each one argued for its own height; one bar takes one edge, and the other
//! three go to the terminal.
//!
//! What it carries, in order from the leading end:
//!
//! * the **mark** -- the logo, which is also the way home;
//! * the **actions** -- connections, explorer, new terminal, settings, and
//!   disconnect when there is something to disconnect from;
//! * the **session**: what kind of host, its name, and a dot for the state;
//! * the **read-out**: one segment per column of the strip, as wide as that
//!   column, accent for the focus and pale for the ones scrolled off screen.
//!   It is the only place the strip says how long it is;
//! * a gap that **drags the window**, because with no title bar this bar is
//!   the caption;
//! * the **clock**, horizontal only -- five characters do not fit across a
//!   44px bar and stacking them would be silly;
//! * the **window controls**, always at the far end whichever edge it is on.
//!
//! **Vertical is not horizontal rotated.** gpui cannot rotate a `div` at all
//! (only `svg()` and `img()` take a `Transformation`), so the vertical bar
//! stacks its text a letter at a time -- `stacked` -- and leaves out what
//! cannot survive that. The two arms are written out separately rather than
//! generated from an axis, because every element differs in more than its
//! direction.

use gpui::{
    div, img, prelude::*, px, AnyElement, ClickEvent, Context, Hsla, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Window,
};

use super::icon::{icon, logo, Icon};
use super::window_controls;
use crate::theme::Theme;
use crate::workspace::strip::Segment;
use crate::workspace::{ConnectionState, Workspace};

/// Which edge the bar is on.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    pub fn from_settings(cx: &gpui::App) -> Self {
        match cx.global::<crate::store::Settings>().nav_edge.as_str() {
            "right" => Edge::Right,
            "top" => Edge::Top,
            "bottom" => Edge::Bottom,
            // `Settings::clamp` has already rejected anything else; left is
            // both the default and the fallback.
            _ => Edge::Left,
        }
    }

    pub fn is_vertical(self) -> bool {
        matches!(self, Edge::Left | Edge::Right)
    }
}

/// How thick the bar is: 44px at the default font, and it grows with the font
/// like everything else. A fixed 44 would have been simpler and wrong -- the
/// bar carries type, and type is the one thing the font-size setting moves.
pub fn thickness(theme: &Theme) -> Pixels {
    super::kit::bar(theme)
}

pub fn render(workspace: &Workspace, window: &mut Window, cx: &mut Context<Workspace>) -> AnyElement {
    let edge = Edge::from_settings(cx);
    if edge.is_vertical() {
        vertical(workspace, edge, window, cx)
    } else {
        horizontal(workspace, edge, window, cx)
    }
}

/// What the bar says about the host: a word for the kind of connection, the
/// name of it, and the colour the state dot takes.
fn session(workspace: &Workspace, theme: &Theme) -> (&'static str, String, Hsla) {
    match workspace.connection_state() {
        // The design's word for it (frame 06): nothing is wrong, nothing is
        // running.
        ConnectionState::Disconnected => ("IDLE", String::new(), theme.fg_subtle),
        ConnectionState::Connecting(label) => ("DIALING", label.to_string(), theme.warning),
        // Which kind of host, because half of what the bar sits above -- paths,
        // what a shell can reach -- means something different for each.
        ConnectionState::Connected(host) if host.is_local() => {
            ("LOCAL", host.label.clone(), theme.accent)
        }
        ConnectionState::Connected(host) => ("SSH", host.label.clone(), theme.success),
        ConnectionState::Failed(error) => ("FAILED", error.to_string(), theme.danger),
    }
}

fn clock() -> String {
    chrono::Local::now().format("%H:%M").to_string()
}

fn horizontal(
    workspace: &Workspace,
    edge: Edge,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = cx.global::<Theme>();
    let surface = theme.surface;
    let border = theme.border;
    let fg = theme.fg;
    let fg_muted = theme.fg_muted;
    let fg_subtle = theme.fg_subtle;
    let danger = theme.danger;
    let size = thickness(theme);

    let (state, label, dot) = session(workspace, theme);

    let mut bar = div()
        .id("nav-bar")
        .flex()
        .flex_row()
        .items_center()
        .w_full()
        .h(size)
        .flex_none()
        .bg(surface)
        .text_color(fg_muted);
    bar = match edge {
        Edge::Top => bar.border_b_1().border_color(border),
        _ => bar.border_t_1().border_color(border),
    };

    bar.child(mark(size, cx))
        .children(actions(workspace, size, cx))
        .child(separator(false, size, border))
        // The state word stays muted and the host does not: which machine you
        // are typing into is the thing worth reading twice.
        .child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .px_2()
                .flex_none()
                .child(div().size(px(5.)).bg(dot))
                .child(div().text_color(fg_subtle).child(state))
                .child(div().text_color(fg).child(label)),
        )
        .child(readout(workspace, false, cx))
        // Claims every pixel nothing else wanted, and drags the window with
        // it. This is the only caption the window has.
        .child(drag_region("nav-caption").flex_1().min_w(px(16.)).h_full())
        // The clock is the way home, like the logo: home is where the big
        // one is.
        .child(
            div()
                .id("nav-clock")
                .px_2()
                .h_full()
                .flex()
                .items_center()
                .flex_none()
                .text_color(fg_subtle)
                .hover(move |s| s.text_color(fg))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.show_home(window, cx)))
                .child(clock()),
        )
        .child(window_buttons(false, size, fg, fg_subtle, danger, window, cx))
        .into_any_element()
}

fn vertical(
    workspace: &Workspace,
    edge: Edge,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let theme = cx.global::<Theme>();
    let surface = theme.surface;
    let border = theme.border;
    let fg = theme.fg;
    let fg_subtle = theme.fg_subtle;
    let danger = theme.danger;
    let size = thickness(theme);

    let (state, _label, dot) = session(workspace, theme);

    let mut bar = div()
        .id("nav-bar")
        .flex()
        .flex_col()
        .items_center()
        .h_full()
        .w(size)
        .flex_none()
        .bg(surface);
    bar = match edge {
        Edge::Left => bar.border_r_1().border_color(border),
        _ => bar.border_l_1().border_color(border),
    };

    bar.child(mark(size, cx))
        .children(actions(workspace, size, cx))
        .child(separator(true, size, border))
        .child(readout(workspace, true, cx))
        .child(drag_region("nav-caption").flex_1().min_h(px(16.)).w_full())
        // The host name is left out here, not shrunk: a name stacked one
        // letter per line would be taller than the window. The kind of
        // session is four or five letters and fits.
        .child(stacked(state, fg_subtle))
        .child(div().my_2().size(px(5.)).flex_none().bg(dot))
        // The clock, as the design draws it on a side: a face rather than
        // digits, because four stacked digits would be taller than they are
        // worth. The time is its tooltip.
        .child(separator(true, size, border))
        .child(
            div()
                .id("nav-clock")
                .size(size)
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .tooltip(|window, cx| {
                    gpui_component::tooltip::Tooltip::new(format!("{}  \u{b7}  home", clock())).build(window, cx)
                })
                .hover(move |s| s.bg(fg.opacity(0.08)))
                .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.show_home(window, cx)))
                .child(icon(Icon::Clock, px(14.), fg_subtle)),
        )
        .child(separator(true, size, border))
        .child(window_buttons(true, size, fg, fg_subtle, danger, window, cx))
        .into_any_element()
}

/// The logo, and the way home. It is a button rather than decoration because
/// the rail's house icon had to go somewhere and the mark is where a user
/// already clicks to get back to the start.
fn mark(size: Pixels, cx: &mut Context<Workspace>) -> impl IntoElement {
    div()
        .id("nav-mark")
        .size(size)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        // `img`, never `svg`: gpui's `svg()` draws a one-colour alpha mask, so
        // the logo would come out a flat silhouette.
        .child(img(logo()).size(size * 0.42))
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.show_home(window, cx)))
}

fn actions(
    workspace: &Workspace,
    size: Pixels,
    cx: &mut Context<Workspace>,
) -> Vec<AnyElement> {
    let theme = cx.global::<Theme>();
    let fg = theme.fg;
    let fg_muted = theme.fg_muted;
    let raised = theme.raised;
    let accent = theme.accent;
    let danger = theme.danger;
    let glyph = px(15.);

    let connected = matches!(workspace.connection_state(), ConnectionState::Connected(_));
    // Local mode has nothing to hang up: those shells are this machine's own
    // processes and die with their windows.
    let can_disconnect = connected && !workspace.is_local();
    let explorer_open = workspace.explorer_open(cx);

    let _ = fg;
    // Icons, but in the app's own hand: one weight, one size, the muted ink
    // and nothing else -- a pictogram has to say what it does at a glance,
    // which the plain shapes the design first drew did not. Each carries its
    // name and its current shortcut as a tooltip, read from the keymap so a
    // rebinding shows up here too.
    let cell = move |id: &'static str, which: Icon, tint: Hsla, name: &'static str, command: &'static str| {
        div()
            .id(id)
            .size(size)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .hover(move |s| s.bg(raised))
            .tooltip(move |window, cx| {
                let text = match crate::keymap::describe(cx, command) {
                    Some(keys) => format!("{name}  {keys}"),
                    None => name.to_string(),
                };
                gpui_component::tooltip::Tooltip::new(text).build(window, cx)
            })
            .child(icon(which, glyph, tint))
    };
    let mut out: Vec<AnyElement> = vec![
        cell("nav-connections", Icon::Search, fg_muted, "Command bar \u{2014} connect, open, run", "window.command")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.open_connections(window, cx)
            }))
            .into_any_element(),
        cell(
            "nav-explorer",
            Icon::FolderTree,
            if explorer_open { accent } else { fg_muted },
            "File tree",
            "window.files",
        )
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.toggle_explorer(&crate::workspace::ToggleExplorer, window, cx);
        }))
        .into_any_element(),
        cell("nav-new-terminal", Icon::SquareTerminal, fg_muted, "New terminal column", "tiling.column.new")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.open_terminal_column(window, cx)
            }))
            .into_any_element(),
        cell("nav-settings", Icon::Settings2, fg_muted, "Settings", "session.settings")
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.open_settings(window, cx)
            }))
            .into_any_element(),
    ];
    if can_disconnect {
        out.push(
            cell("nav-disconnect", Icon::Unplug, danger, "Disconnect", "session.disconnect")
                .on_click(
                    cx.listener(|this, _: &ClickEvent, window, cx| this.disconnect(window, cx)),
                )
                .into_any_element(),
        );
    }
    out
}

/// A hairline across the bar, at 60% of its thickness so it reads as a divider
/// between groups rather than a second border.
fn separator(vertical: bool, size: Pixels, border: Hsla) -> impl IntoElement {
    let short = size * 0.6;
    let line = div().flex_none().bg(border);
    if vertical {
        line.h(px(1.)).w(short)
    } else {
        line.w(px(1.)).h(short)
    }
}

/// One segment per column, as wide as that column is -- so a strip of four
/// halves and a strip of four thirds do not read the same. Accent is the
/// focus, full-strength is on screen, pale is scrolled off.
///
/// Clicking a segment scrolls to that column, which is the only way to reach
/// one with the mouse without dragging across the strip.
fn readout(workspace: &Workspace, vertical: bool, cx: &mut Context<Workspace>) -> AnyElement {
    let theme = cx.global::<Theme>();
    let fg = theme.fg_muted;
    let subtle = theme.fg_subtle;
    let accent = theme.accent;
    // A half-width column reads as 14px of bar. Long enough to compare two
    // widths at a glance, short enough that a dozen columns still fit.
    const UNIT: f32 = 28.;
    const THICK: f32 = 3.;

    let segments: Vec<AnyElement> = workspace
        .strip_readout()
        .into_iter()
        .enumerate()
        .map(|(ix, (share, segment))| {
            let tint = match segment {
                Segment::Focus => accent,
                Segment::Visible => fg,
                Segment::Offscreen => subtle,
            };
            let long = px((share * UNIT).max(6.));
            // The mark stays a 3px rule; what you click is the cell around
            // it -- the bar's whole thickness across, and half the gap either
            // side along it -- because a 3px target is a target you miss.
            let mark = div().flex_none().bg(tint);
            let mark = if vertical {
                mark.w(px(THICK)).h(long)
            } else {
                mark.h(px(THICK)).w(long)
            };
            let cell = div()
                .id(("nav-segment", ix))
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .group("segment")
                .hover(move |this| this.opacity(0.7))
                .child(mark)
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.focus_column_at(ix, window, cx)
                }));
            if vertical {
                cell.w(px(24.)).py(px(1.5)).into_any_element()
            } else {
                cell.h(px(24.)).px(px(1.5)).into_any_element()
            }
        })
        .collect();

    let row = div()
        .flex()
        .items_center()
        // Shrinks and clips rather than pushing the clock and the window
        // controls off the end: a strip can be arbitrarily long.
        .flex_shrink()
        .overflow_hidden();
    if vertical {
        row.flex_col().py_2().max_h(gpui::relative(0.4))
    } else {
        row.flex_row().px_2().max_w(gpui::relative(0.3))
    }
    .children(segments)
    .into_any_element()
}

/// Micro type down the bar, a letter per line. The design calls this rotated
/// text; gpui has no rotation for a `div`, and stacking reads at this size.
fn stacked(text: &str, color: Hsla) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .items_center()
        .flex_none()
        .text_color(color)
        .children(
            text.chars()
                .map(|c| div().line_height(gpui::relative(1.)).child(c.to_string())),
        )
}

/// Minimise, maximise/restore, close -- at the far end of the bar whichever
/// edge it is on.
///
/// Close goes through the workspace rather than straight to `remove_window`:
/// with a session live or a file unsaved it asks first, the same as the OS
/// close does.
fn window_buttons(
    vertical: bool,
    size: Pixels,
    fg: Hsla,
    fg_subtle: Hsla,
    danger: Hsla,
    window: &Window,
    cx: &mut Context<Workspace>,
) -> impl IntoElement {
    let maximized = window.is_maximized();
    // A horizontal bar can afford the usual wide caption buttons; a vertical
    // one is only as wide as it is thick, so the cells go square.
    let w = if vertical { size } else { px(44.) };
    let h = if vertical { size * 0.62 } else { size };

    let button = move |id: &'static str, which: Icon, close: bool| {
        div()
            .id(id)
            .w(w)
            .h(h)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .text_color(fg_subtle)
            .hover(move |s| {
                if close {
                    s.bg(danger).text_color(gpui::white())
                } else {
                    s.bg(fg.opacity(0.12)).text_color(fg)
                }
            })
            .child(icon(which, px(12.), fg_subtle))
    };

    div()
        .flex()
        .flex_none()
        .items_center()
        .when(vertical, |this| this.flex_col())
        .child(
            button("win-min", Icon::Minus, false)
                .on_click(|_: &ClickEvent, window, _| window.minimize_window()),
        )
        .child(
            // Two overlapping squares once maximised, the way Windows signals
            // "restore". gpui's `zoom_window` only ever maximises, so the
            // toggle has to be ours -- see `window_controls::toggle_maximize`.
            button(
                "win-max",
                if maximized { Icon::Copy } else { Icon::Square },
                false,
            )
            .on_click(|_: &ClickEvent, window, _| window_controls::toggle_maximize(window)),
        )
        .child(
            button("win-close", Icon::X, true).on_click(cx.listener(
                |this, _: &ClickEvent, window, cx| this.close_window(window, cx),
            )),
        )
}

/// A stretch of bar that behaves like a real caption: drag to move,
/// double-click to maximise or restore.
pub(crate) fn drag_region(id: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        // Mouse-down, not click: a click only resolves on release, by which
        // point the OS move loop needed to have started. Everything after that
        // -- the drag itself, snapping, Aero Shake -- belongs to Windows.
        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, window, _| {
            window_controls::caption_press(window);
        })
        // The rest is Linux's: there the press only arms, the first drag
        // starts the move, and a double-click is seen as one -- see
        // `window_controls`. On Windows these are no-ops.
        .on_mouse_move(|event: &MouseMoveEvent, window, _| {
            window_controls::caption_drag(window, event);
        })
        .on_mouse_up(MouseButton::Left, |_: &MouseUpEvent, _, _| {
            window_controls::caption_release();
        })
        .on_click(|event: &ClickEvent, window, _| {
            window_controls::caption_click(window, event.click_count());
        })
        .on_mouse_down(MouseButton::Right, |event: &MouseDownEvent, window, _| {
            window_controls::caption_menu(window, event.position);
        })
}

